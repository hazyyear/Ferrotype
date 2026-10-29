//! Colour calibration from a photo of a target: an X-Rite ColorChecker 24 (reference values built in) or an
//! IT8.7 chart with the reference file that came with it. Builds a 3×3 matrix for the camera or scanner that
//! corrects its colours and leaves white where it was, so white balance and exposure stay the user's.
use image::Rgb32FImage;
use serde::{Deserialize, Serialize};

pub type M3 = [[f32; 3]; 3];

/// A camera or scanner profile: applied to every pixel of the source before anything else (and before a
/// negative is inverted, so a scanner profile corrects the scan itself).
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct ColorProfile {
    pub name: String,
    pub m: M3,
}

impl Default for ColorProfile {
    fn default() -> Self {
        ColorProfile { name: String::new(), m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] }
    }
}

pub const EXT: &str = "ferroprofile";

pub fn dir() -> Option<std::path::PathBuf> {
    crate::keys::dirs_config().map(|d| d.join("profiles"))
}

/// Your saved profiles, by name.
pub fn saved() -> Vec<ColorProfile> {
    let mut v: Vec<ColorProfile> = dir().and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten().filter_map(|e| {
        let p = e.path();
        (p.extension()? == EXT).then_some(())?;
        serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()
    }).collect();
    v.sort_by_key(|a| a.name.to_lowercase());
    v
}

impl ColorProfile {
    /// Keep it in the profiles folder, named after the profile (replaces one of the same name).
    pub fn install(&self) -> Result<std::path::PathBuf, String> {
        let dir = dir().ok_or("no config folder")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let safe: String = self.name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
        let path = dir.join(format!("{}.{EXT}", if safe.trim().is_empty() { "profile" } else { safe.trim() }));
        crate::write_atomic(&path, serde_json::to_string_pretty(self).map_err(|e| e.to_string())?.as_bytes()).map_err(|e| e.to_string())?;
        Ok(path)
    }

    pub fn apply(&self, img: &mut Rgb32FImage) {
        use rayon::prelude::*;
        let m = self.m;
        crate::develop::px3_mut(img).par_iter_mut().for_each(|c| {
            let v = *c;
            *c = [0, 1, 2].map(|i| (m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2]).max(0.0));
        });
    }
}

/// A chart: patches on a grid, row by row, with their reference colours (CIE L*a*b*, D50). A missing patch is None.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub name: String,
    pub cols: usize,
    pub rows: usize,
    pub lab: Vec<Option<[f32; 3]>>,
}

/// X-Rite ColorChecker Classic, 24 patches, reference values for charts made after November 2014.
pub fn colorchecker() -> Target {
    const LAB: [[f32; 3]; 24] = [
        [37.54, 14.37, 14.92], [64.66, 19.27, 17.5], [49.32, -3.82, -22.54], [43.46, -12.74, 22.72], [54.94, 9.61, -24.79], [70.48, -32.26, -0.37],
        [62.73, 35.83, 56.5], [39.43, 10.75, -45.17], [50.57, 48.64, 16.67], [30.1, 22.54, -20.87], [71.77, -24.13, 58.19], [71.51, 18.24, 67.37],
        [28.37, 15.42, -49.8], [54.38, -39.72, 32.27], [42.43, 51.05, 28.62], [81.8, 2.67, 80.41], [50.63, 51.28, -14.12], [49.57, -29.71, -28.32],
        [95.19, -1.03, 2.93], [81.29, -0.57, 0.44], [66.89, -0.75, -0.06], [50.76, -0.13, 0.14], [35.63, -0.46, -0.48], [20.64, 0.07, -0.46],
    ];
    Target { name: "ColorChecker 24".into(), cols: 6, rows: 4, lab: LAB.iter().map(|v| Some(*v)).collect() }
}

/// An IT8.7/1 or IT8.7/2 reference file (CGATS text): patches named A1…L22 (letters are rows), with LAB_L/A/B or
/// XYZ_X/Y/Z columns. The grey strip (GS0…) sits outside the grid and is left out.
pub fn parse_cgats(text: &str, name: &str) -> Result<Target, String> {
    let mut fields: Vec<String> = vec![];
    let mut data: Vec<String> = vec![];
    let (mut in_fmt, mut in_data) = (false, false);
    for line in text.lines() {
        let t = line.trim();
        match t {
            "BEGIN_DATA_FORMAT" => in_fmt = true,
            "END_DATA_FORMAT" => in_fmt = false,
            "BEGIN_DATA" => in_data = true,
            "END_DATA" => in_data = false,
            _ if in_fmt => fields.extend(t.split_whitespace().map(String::from)),
            _ if in_data => data.extend(t.split_whitespace().map(|s| s.trim_matches('"').to_string())),
            _ => {}
        }
    }
    let col = |n: &str| fields.iter().position(|f| f.eq_ignore_ascii_case(n));
    let id = col("SAMPLE_ID").or_else(|| col("SAMPLE_NAME")).ok_or("no SAMPLE_ID column")?;
    let (lab, xyz) = ([col("LAB_L"), col("LAB_A"), col("LAB_B")], [col("XYZ_X"), col("XYZ_Y"), col("XYZ_Z")]);
    if fields.is_empty() || data.len() % fields.len() != 0 {
        return Err("not a CGATS reference file".into());
    }
    let mut patches = vec![];
    for rec in data.chunks(fields.len()) {
        let sid = rec[id].to_ascii_uppercase();
        let mut ch = sid.chars();
        let Some(row) = ch.next().filter(|c| c.is_ascii_uppercase()) else { continue };
        let Ok(c) = ch.as_str().parse::<usize>() else { continue };
        if c == 0 {
            continue;
        }
        let num = |i: Option<usize>| i.and_then(|i| rec[i].replace(',', ".").parse::<f32>().ok());
        let v = if let [Some(l), Some(a), Some(b)] = lab.map(num) {
            [l, a, b]
        } else if let [Some(x), Some(y), Some(z)] = xyz.map(num) {
            xyz_to_lab([x / 100.0, y / 100.0, z / 100.0], D50)
        } else {
            return Err("no LAB_L/A/B or XYZ_X/Y/Z columns".into());
        };
        patches.push(((row as u8 - b'A') as usize, c - 1, v));
    }
    let rows = patches.iter().map(|p| p.0 + 1).max().ok_or("no patches like A1…L22")?;
    let cols = patches.iter().map(|p| p.1 + 1).max().unwrap_or(0);
    if rows < 2 || cols < 2 || patches.len() < 12 {
        return Err("too few patches".into());
    }
    let mut lab = vec![None; rows * cols];
    for (r, c, v) in patches {
        lab[r * cols + c] = Some(v);
    }
    Ok(Target { name: name.into(), cols, rows, lab })
}

// ---------- colour maths ----------

const D50: [f32; 3] = [0.9642, 1.0, 0.8251];
const D65: [f32; 3] = [0.95047, 1.0, 1.08883];

fn mul(m: &M3, c: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
}

fn mat_mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

pub fn invert(m: &M3) -> Option<M3> {
    let d = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if d.abs() < 1e-12 || !d.is_finite() {
        return None;
    }
    let c = |a: usize, b: usize, x: usize, y: usize| m[a][b] * m[x][y] - m[a][y] * m[x][b];
    Some([
        [c(1, 1, 2, 2) / d, -c(0, 1, 2, 2) / d, c(0, 1, 1, 2) / d],
        [-c(1, 0, 2, 2) / d, c(0, 0, 2, 2) / d, -c(0, 0, 1, 2) / d],
        [c(1, 0, 2, 1) / d, -c(0, 0, 2, 1) / d, c(0, 0, 1, 1) / d],
    ])
}

fn lab_to_xyz(l: [f32; 3], w: [f32; 3]) -> [f32; 3] {
    let fy = (l[0] + 16.0) / 116.0;
    let f = [fy + l[1] / 500.0, fy, fy - l[2] / 200.0];
    let inv = |t: f32| if t > 6.0 / 29.0 { t * t * t } else { 3.0 * (6.0f32 / 29.0).powi(2) * (t - 4.0 / 29.0) };
    [0, 1, 2].map(|i| inv(f[i]) * w[i])
}

fn xyz_to_lab(x: [f32; 3], w: [f32; 3]) -> [f32; 3] {
    let f = |t: f32| if t > (6.0f32 / 29.0).powi(3) { t.cbrt() } else { t / (3.0 * (6.0f32 / 29.0).powi(2)) + 4.0 / 29.0 };
    let [fx, fy, fz] = [0, 1, 2].map(|i| f(x[i] / w[i]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

const XYZ_TO_RGB: M3 = [[3.2404542, -1.5371385, -0.4985314], [-0.9692660, 1.8760108, 0.0415560], [0.0556434, -0.2040259, 1.0572252]];
const RGB_TO_XYZ: M3 = [[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]];
/// Bradford adaptation D50 → D65.
const D50_TO_D65: M3 = [[0.9555766, -0.0230393, 0.0631636], [-0.0282895, 1.0099416, 0.0210077], [0.0122982, -0.0204830, 1.3299098]];

/// A D50 L*a*b* reference as linear sRGB (D65, white = 1).
pub fn lab_to_rgb(l: [f32; 3]) -> [f32; 3] {
    mul(&XYZ_TO_RGB, mul(&D50_TO_D65, lab_to_xyz(l, D50)))
}

fn rgb_to_lab(c: [f32; 3]) -> [f32; 3] {
    xyz_to_lab(mul(&RGB_TO_XYZ, c), D65)
}

fn delta_e(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (la, lb) = (rgb_to_lab(a), rgb_to_lab(b));
    ((la[0] - lb[0]).powi(2) + (la[1] - lb[1]).powi(2) + (la[2] - lb[2]).powi(2)).sqrt()
}

// ---------- placing and sampling the chart ----------

/// Where the centre of grid point (u, v) ∈ [0, 1]² lands, for corners `q` = centres of the first patch, the last
/// of the first row, the very last patch and the first of the last row (a perspective map, square → quad).
pub fn quad_point(q: &[[f32; 2]; 4], u: f32, v: f32) -> [f32; 2] {
    let [p0, p1, p2, p3] = *q;
    let (dx1, dx2, dx3) = (p1[0] - p2[0], p3[0] - p2[0], p0[0] - p1[0] + p2[0] - p3[0]);
    let (dy1, dy2, dy3) = (p1[1] - p2[1], p3[1] - p2[1], p0[1] - p1[1] + p2[1] - p3[1]);
    let det = dx1 * dy2 - dx2 * dy1;
    let (g, h) = if det.abs() < 1e-12 { (0.0, 0.0) } else { ((dx3 * dy2 - dx2 * dy3) / det, (dx1 * dy3 - dx3 * dy1) / det) };
    let (a, b, c) = (p1[0] - p0[0] + g * p1[0], p3[0] - p0[0] + h * p3[0], p0[0]);
    let (d, e, f) = (p1[1] - p0[1] + g * p1[1], p3[1] - p0[1] + h * p3[1], p0[1]);
    let z = g * u + h * v + 1.0;
    [(a * u + b * v + c) / z, (d * u + e * v + f) / z]
}

/// Centre of patch (row, col) for corners `q`.
pub fn patch_center(t: &Target, q: &[[f32; 2]; 4], row: usize, col: usize) -> [f32; 2] {
    quad_point(q, col as f32 / (t.cols - 1).max(1) as f32, row as f32 / (t.rows - 1).max(1) as f32)
}

/// Half the side of the square sampled in each patch, in the same units as `q` (about a third of the spacing).
pub fn sample_half(t: &Target, q: &[[f32; 2]; 4], aspect: f32) -> f32 {
    let d = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) * aspect * aspect + (a[1] - b[1]).powi(2)).sqrt();
    let sx = d(q[0], q[1]).min(d(q[3], q[2])) / (t.cols - 1).max(1) as f32;
    let sy = d(q[0], q[3]).min(d(q[1], q[2])) / (t.rows - 1).max(1) as f32;
    sx.min(sy) * 0.3
}

/// Mean linear colour of each patch in `img` (the uncropped frame); corners `q` are frame fractions.
pub fn sample(img: &Rgb32FImage, t: &Target, q: &[[f32; 2]; 4]) -> Vec<Option<[f32; 3]>> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    // Half-size in pixels: measure the spacing in pixels (heights are fractions of h, widths of w).
    let half = sample_half(t, &q.map(|p| [p[0] * w / h, p[1]]), 1.0) * h;
    let r = half.max(1.0) as i64;
    (0..t.rows * t.cols).map(|i| {
        let c = patch_center(t, q, i / t.cols, i % t.cols);
        let (cx, cy) = ((c[0] * w) as i64, (c[1] * h) as i64);
        let mut sum = [0.0f64; 3];
        let mut n = 0;
        for y in (cy - r).max(0)..=(cy + r).min(h as i64 - 1) {
            for x in (cx - r).max(0)..=(cx + r).min(w as i64 - 1) {
                let p = img.get_pixel(x as u32, y as u32).0;
                for k in 0..3 {
                    sum[k] += p[k] as f64;
                }
                n += 1;
            }
        }
        (n > 0).then(|| sum.map(|s| (s / n as f64) as f32))
    }).collect()
}

/// The result of fitting: the profile's matrix and the average colour error (CIE ΔE 1976) before and after.
#[derive(Clone, Debug)]
pub struct Fit {
    pub m: M3,
    pub de_before: f32,
    pub de_after: f32,
    pub de_worst: f32,
    pub used: usize,
}

/// Fit a matrix from the patches' measured colours. Clipped and missing patches are left out. The matrix keeps
/// the camera's white (as shot) where it was, so a photo of the chart still needs its white balance set.
pub fn fit(t: &Target, measured: &[Option<[f32; 3]>]) -> Result<Fit, String> {
    let pairs: Vec<([f32; 3], [f32; 3])> = measured.iter().zip(&t.lab).filter_map(|(m, r)| {
        let (m, r) = ((*m)?, (*r)?);
        let ok = m.iter().all(|v| v.is_finite() && *v < 0.98) && m.iter().sum::<f32>() > 1e-4;
        ok.then(|| (m, lab_to_rgb(r)))
    }).collect();
    if pairs.len() < 9 {
        return Err(format!("only {} usable patches (some may be clipped: lower the exposure)", pairs.len()));
    }
    // Weighted least squares, one row at a time: errors count relative to each patch's brightness.
    let mut a = [[0.0f64; 3]; 3];
    let mut b = [[0.0f64; 3]; 3]; // b[row][k]
    for (x, y) in &pairs {
        let yl = crate::develop::luma(*y).max(0.0) as f64;
        let w = 1.0 / (yl + 0.02).powi(2);
        for i in 0..3 {
            for j in 0..3 {
                a[i][j] += w * x[i] as f64 * x[j] as f64;
            }
            for row in 0..3 {
                b[row][i] += w * x[i] as f64 * y[row] as f64;
            }
        }
    }
    let ai = invert(&a.map(|r| r.map(|v| v as f32))).ok_or("the patches are too alike to fit (is the chart in place?)")?;
    let full: M3 = [0, 1, 2].map(|row| [0, 1, 2].map(|j| (0..3).map(|k| ai[j][k] * b[row][k] as f32).sum()));
    // The camera colour that comes out white, and the matrix rescaled per channel so it maps that to itself.
    let white = mul(&invert(&full).ok_or("the fit is degenerate")?, [1.0; 3]);
    if white.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err("the fit doesn't keep white positive (is the chart placed on the right patches?)".into());
    }
    let m = mat_mul(&[[white[0], 0.0, 0.0], [0.0, white[1], 0.0], [0.0, 0.0, white[2]]], &full);
    let n = pairs.len() as f32;
    let before: Vec<f32> = pairs.iter().map(|(x, y)| delta_e([0, 1, 2].map(|k| x[k] / white[k]), *y)).collect();
    let after: Vec<f32> = pairs.iter().map(|(x, y)| delta_e(mul(&full, *x), *y)).collect();
    Ok(Fit {
        m,
        de_before: before.iter().sum::<f32>() / n,
        de_after: after.iter().sum::<f32>() / n,
        de_worst: after.iter().cloned().fold(0.0, f32::max),
        used: pairs.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_corners_land_on_the_corners() {
        let q = [[0.1, 0.2], [0.8, 0.15], [0.9, 0.7], [0.05, 0.75]];
        for (i, (u, v)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].into_iter().enumerate() {
            let p = quad_point(&q, u, v);
            assert!((p[0] - q[i][0]).abs() < 1e-5 && (p[1] - q[i][1]).abs() < 1e-5, "{i} {p:?}");
        }
        // A parallelogram is affine: the centre is the average.
        let r = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let c = quad_point(&r, 0.5, 0.5);
        assert!((c[0] - 0.5).abs() < 1e-6 && (c[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn reference_white_is_near_white() {
        let w = lab_to_rgb([95.19, -1.03, 2.93]);
        assert!(w.iter().all(|v| (*v - 0.87).abs() < 0.04), "{w:?}");
        let g = lab_to_rgb([50.76, -0.13, 0.14]);
        assert!((g[0] - g[2]).abs() < 0.01 && (g[1] - 0.19).abs() < 0.02, "{g:?}");
    }

    /// A chart photographed by a "camera" with a colour cast and crosstalk: the fit undoes the crosstalk, keeps
    /// the cast (white balance is separate), and the error drops.
    #[test]
    fn fit_recovers_a_camera_matrix() {
        let t = colorchecker();
        let cam: M3 = [[0.8, 0.15, 0.05], [0.1, 0.75, 0.15], [0.02, 0.18, 0.8]];
        let cast = [0.9, 1.0, 0.6];
        let measured: Vec<Option<[f32; 3]>> = t.lab.iter().map(|l| {
            let c = mul(&cam, lab_to_rgb(l.unwrap()));
            Some([0, 1, 2].map(|k| c[k] * cast[k] * 0.9))
        }).collect();
        let f = fit(&t, &measured).unwrap();
        assert!(f.de_after < 0.5 && f.de_before > 3.0, "{f:?}");
        // White as shot stays as shot.
        let white_shot = [0, 1, 2].map(|k| cast[k]);
        let out = mul(&f.m, white_shot);
        assert!((0..3).all(|k| (out[k] - white_shot[k]).abs() < 1e-3), "{out:?}");
    }

    #[test]
    fn samples_patches_where_the_corners_say() {
        let t = colorchecker();
        // A 6×4 chart drawn as flat patches 20 px apart, first centre at (15, 15).
        let img = Rgb32FImage::from_fn(140, 100, |x, y| {
            let (c, r) = (((x as i32 - 5) / 20).clamp(0, 5) as usize, ((y as i32 - 5) / 20).clamp(0, 3) as usize);
            image::Rgb(lab_to_rgb(t.lab[r * 6 + c].unwrap()).map(|v| v.clamp(0.0, 1.0)))
        });
        let q = [[15.0 / 140.0, 15.0 / 100.0], [115.0 / 140.0, 15.0 / 100.0], [115.0 / 140.0, 75.0 / 100.0], [15.0 / 140.0, 75.0 / 100.0]];
        let s = sample(&img, &t, &q);
        for (i, v) in s.iter().enumerate() {
            let want = lab_to_rgb(t.lab[i].unwrap()).map(|v| v.clamp(0.0, 1.0));
            assert!((0..3).all(|k| (v.unwrap()[k] - want[k]).abs() < 1e-4), "patch {i}");
        }
    }

    #[test]
    fn reads_an_it8_reference_file() {
        let mut txt = String::from("IT8.7/2\nNUMBER_OF_FIELDS 4\nBEGIN_DATA_FORMAT\nSAMPLE_ID LAB_L LAB_A LAB_B\nEND_DATA_FORMAT\nBEGIN_DATA\n");
        for r in 0..12u8 {
            for c in 1..=22 {
                txt += &format!("{}{} {}.5 {} -{}\n", (b'A' + r) as char, c, 20 + r, c, r);
            }
        }
        txt += "GS0 90.1 0 0\nEND_DATA\n";
        let t = parse_cgats(&txt, "R1").unwrap();
        assert_eq!((t.rows, t.cols), (12, 22));
        assert_eq!(t.lab[22 + 2], Some([21.5, 3.0, -1.0])); // B3
    }
}

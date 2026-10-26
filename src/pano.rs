//! Panorama merge: match corner features between overlapping photos, work out how the camera turned between them
//! (a rotation-only model, refined over all overlaps at once), level the horizon, then project every photo onto one
//! cylinder, sphere or plane with exposure matched and seams feathered. Works on linear images.
use crate::develop::blur;
use image::Rgb32FImage;
use rayon::prelude::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Projection {
    Auto,
    Cylindrical,
    Spherical,
    Perspective,
}

impl Projection {
    pub const ALL: [(Projection, &'static str); 4] =
        [(Projection::Auto, "Auto"), (Projection::Cylindrical, "Cylindrical"), (Projection::Spherical, "Spherical"), (Projection::Perspective, "Perspective")];

    pub fn parse(s: &str) -> Option<Projection> {
        Self::ALL.iter().find(|(_, n)| n.eq_ignore_ascii_case(s) || n[..3].eq_ignore_ascii_case(s)).map(|p| p.0)
    }
}

type M3 = [[f64; 3]; 3];
type V3 = [f64; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

fn mv(a: &M3, v: V3) -> V3 {
    [0, 1, 2].map(|i| a[i][0] * v[0] + a[i][1] * v[1] + a[i][2] * v[2])
}

fn tr(a: &M3) -> M3 {
    [0, 1, 2].map(|i| [a[0][i], a[1][i], a[2][i]])
}

fn det(a: &M3) -> f64 {
    a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0]) + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
}

fn inv(a: &M3) -> Option<M3> {
    let d = det(a);
    if d.abs() < 1e-12 {
        return None;
    }
    let c = |r0: usize, r1: usize, c0: usize, c1: usize| a[r0][c0] * a[r1][c1] - a[r0][c1] * a[r1][c0];
    Some([
        [c(1, 2, 1, 2) / d, -c(0, 2, 1, 2) / d, c(0, 1, 1, 2) / d],
        [-c(1, 2, 0, 2) / d, c(0, 2, 0, 2) / d, -c(0, 1, 0, 2) / d],
        [c(1, 2, 0, 1) / d, -c(0, 2, 0, 1) / d, c(0, 1, 0, 1) / d],
    ])
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn norm(v: V3) -> V3 {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    v.map(|x| x / l)
}

/// Nearest rotation to `m` (polar decomposition by averaging with the inverse transpose).
fn orthonormalize(mut m: M3) -> M3 {
    let d = det(&m);
    if d.abs() > 1e-12 {
        let s = d.abs().cbrt() * d.signum();
        m = m.map(|r| r.map(|x| x / s));
    }
    for _ in 0..20 {
        let Some(it) = inv(&m).map(|i| tr(&i)) else { break };
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = 0.5 * (m[i][j] + it[i][j]);
            }
        }
    }
    m
}

/// Rotation from an axis-angle vector.
fn rodrigues(r: V3) -> M3 {
    let t = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
    if t < 1e-12 {
        return [[1.0, -r[2], r[1]], [r[2], 1.0, -r[0]], [-r[1], r[0], 1.0]];
    }
    let [x, y, z] = r.map(|v| v / t);
    let (s, c) = t.sin_cos();
    let v = 1.0 - c;
    [
        [c + x * x * v, x * y * v - z * s, x * z * v + y * s],
        [y * x * v + z * s, c + y * y * v, y * z * v - x * s],
        [z * x * v - y * s, z * y * v + x * s, c + z * z * v],
    ]
}

/// Eigenvalues and eigenvectors (as columns) of a small symmetric matrix, by Jacobi rotations.
fn jacobi(mut a: Vec<Vec<f64>>) -> (Vec<f64>, Vec<Vec<f64>>) {
    let n = a.len();
    let mut v: Vec<Vec<f64>> = (0..n).map(|i| (0..n).map(|j| if i == j { 1.0 } else { 0.0 }).collect()).collect();
    for _ in 0..100 {
        let off: f64 = (0..n).flat_map(|i| (0..n).filter(move |j| *j != i).map(move |j| (i, j))).map(|(i, j)| a[i][j] * a[i][j]).sum();
        if off < 1e-22 {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                if a[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (akp, akq) = (a[k][p], a[k][q]);
                    a[k][p] = c * akp - s * akq;
                    a[k][q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let (apk, aqk) = (a[p][k], a[q][k]);
                    a[p][k] = c * apk - s * aqk;
                    a[q][k] = s * apk + c * aqk;
                }
                for row in v.iter_mut() {
                    let (vkp, vkq) = (row[p], row[q]);
                    row[p] = c * vkp - s * vkq;
                    row[q] = s * vkp + c * vkq;
                }
            }
        }
    }
    ((0..n).map(|i| a[i][i]).collect(), v)
}

/// Solve a small dense system (Gaussian elimination with partial pivoting).
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for c in 0..n {
        let p = (c..n).max_by(|x, y| a[*x][c].abs().total_cmp(&a[*y][c].abs()))?;
        if a[p][c].abs() < 1e-15 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..n {
            let f = a[r][c] / a[c][c];
            for k in c..n {
                a[r][k] -= f * a[c][k];
            }
            b[r] -= f * b[c];
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        x[r] = (b[r] - (r + 1..n).map(|k| a[r][k] * x[k]).sum::<f64>()) / a[r][r];
    }
    Some(x)
}

// ---------- features ----------

const SMALL: u32 = 1000;

struct Small {
    g: Vec<f32>,
    w: usize,
    h: usize,
    /// small / full size
    s: f64,
}

fn small(img: &Rgb32FImage) -> Small {
    let sm = crate::load::fit(img, SMALL);
    let (w, h) = (sm.width() as usize, sm.height() as usize);
    let g = sm.chunks(3).map(|c| crate::develop::luma([c[0], c[1], c[2]]).max(0.0).sqrt()).collect();
    Small { g, w, h, s: w as f64 / img.width() as f64 }
}

struct Feat {
    /// Position relative to the image centre, small-image pixels.
    x: f64,
    y: f64,
    d: [f32; 64],
}

fn sample(g: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    let (x, y) = (x.clamp(0.0, (w - 1) as f32), y.clamp(0.0, (h - 1) as f32));
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    (g[y0 * w + x0] * (1.0 - fx) + g[y0 * w + x1] * fx) * (1.0 - fy) + (g[y1 * w + x0] * (1.0 - fx) + g[y1 * w + x1] * fx) * fy
}

/// Harris corners spread over the frame, each with a normalised 8×8 patch (MOPS-style) as its descriptor.
fn features(s: &Small) -> Vec<Feat> {
    let (w, h) = (s.w, s.h);
    let g = blur(&s.g, w, h, 1.0);
    let (mut xx, mut yy, mut xy) = (vec![0f32; w * h], vec![0f32; w * h], vec![0f32; w * h]);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            let (dx, dy) = ((g[i + 1] - g[i - 1]) * 0.5, (g[i + w] - g[i - w]) * 0.5);
            (xx[i], yy[i], xy[i]) = (dx * dx, dy * dy, dx * dy);
        }
    }
    let (xx, yy, xy) = (blur(&xx, w, h, 1.5), blur(&yy, w, h, 1.5), blur(&xy, w, h, 1.5));
    let resp: Vec<f32> = (0..w * h).map(|i| xx[i] * yy[i] - xy[i] * xy[i] - 0.04 * (xx[i] + yy[i]).powi(2)).collect();
    // Relative threshold: flat or soft photos still give corners, noise in the sky doesn't.
    let thresh = resp.iter().cloned().fold(0.0f32, f32::max) * 1e-3;
    let border = 22;
    let cell = 40;
    let mut cand: Vec<(f32, usize, usize)> = vec![];
    for cy in (border..h.saturating_sub(border)).step_by(cell) {
        for cx in (border..w.saturating_sub(border)).step_by(cell) {
            let mut local = vec![];
            for y in cy..(cy + cell).min(h - border) {
                for x in cx..(cx + cell).min(w - border) {
                    let r = resp[y * w + x];
                    if r > thresh && (-1i64..=1).all(|oy| (-1i64..=1).all(|ox| (ox == 0 && oy == 0) || resp[((y as i64 + oy) as usize) * w + (x as i64 + ox) as usize] < r)) {
                        local.push((r, x, y));
                    }
                }
            }
            local.sort_by(|a, b| b.0.total_cmp(&a.0));
            cand.extend(local.into_iter().take(6));
        }
    }
    cand.sort_by(|a, b| b.0.total_cmp(&a.0));
    cand.truncate(2500);
    let gd = blur(&s.g, w, h, 2.5);
    cand.into_iter().map(|(_, x, y)| {
        let mut d = [0f32; 64];
        for j in 0..8 {
            for i in 0..8 {
                d[j * 8 + i] = sample(&gd, w, h, x as f32 + (i as f32 - 3.5) * 5.0, y as f32 + (j as f32 - 3.5) * 5.0);
            }
        }
        let mean = d.iter().sum::<f32>() / 64.0;
        let sd = (d.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / 64.0).sqrt().max(1e-6);
        d.iter_mut().for_each(|v| *v = (*v - mean) / sd);
        Feat { x: x as f64 - w as f64 / 2.0, y: y as f64 - h as f64 / 2.0, d }
    }).collect()
}

/// Nearest-neighbour matches that pass Lowe's ratio test.
fn match_feats(a: &[Feat], b: &[Feat]) -> Vec<(usize, usize)> {
    if b.len() < 2 {
        return vec![];
    }
    a.par_iter().enumerate().filter_map(|(i, fa)| {
        let (mut d1, mut d2, mut best) = (f32::MAX, f32::MAX, 0);
        for (j, fb) in b.iter().enumerate() {
            let d: f32 = fa.d.iter().zip(&fb.d).map(|(x, y)| (x - y) * (x - y)).sum();
            if d < d1 {
                (d2, d1, best) = (d1, d, j);
            } else if d < d2 {
                d2 = d;
            }
        }
        (d1 < 0.5 * d2).then_some((i, best))
    }).collect()
}

/// Homography taking `p.1` to `p.0`, normalised DLT on 4+ correspondences.
fn homography(p: &[([f64; 2], [f64; 2])]) -> Option<M3> {
    let norm_of = |pts: Vec<[f64; 2]>| {
        let n = pts.len() as f64;
        let (cx, cy) = (pts.iter().map(|q| q[0]).sum::<f64>() / n, pts.iter().map(|q| q[1]).sum::<f64>() / n);
        let d = pts.iter().map(|q| ((q[0] - cx).powi(2) + (q[1] - cy).powi(2)).sqrt()).sum::<f64>() / n;
        let s = std::f64::consts::SQRT_2 / d.max(1e-9);
        [[s, 0.0, -s * cx], [0.0, s, -s * cy], [0.0, 0.0, 1.0]]
    };
    let ta = norm_of(p.iter().map(|q| q.0).collect());
    let tb = norm_of(p.iter().map(|q| q.1).collect());
    let mut ata = vec![vec![0.0; 9]; 9];
    for (a, b) in p {
        let [x, y, _] = mv(&tb, [b[0], b[1], 1.0]);
        let [u, v, _] = mv(&ta, [a[0], a[1], 1.0]);
        for row in [[-x, -y, -1.0, 0.0, 0.0, 0.0, u * x, u * y, u], [0.0, 0.0, 0.0, -x, -y, -1.0, v * x, v * y, v]] {
            for i in 0..9 {
                for j in 0..9 {
                    ata[i][j] += row[i] * row[j];
                }
            }
        }
    }
    let (vals, vecs) = jacobi(ata);
    let k = (0..9).min_by(|a, b| vals[*a].total_cmp(&vals[*b]))?;
    let h: M3 = [0, 1, 2].map(|r| [0, 1, 2].map(|c| vecs[r * 3 + c][k]));
    let hn = mul(&mul(&inv(&ta)?, &h), &tb);
    (hn[2][2].abs() > 1e-12).then(|| hn.map(|r| r.map(|v| v / hn[2][2])))
}

fn project(h: &M3, p: [f64; 2]) -> Option<[f64; 2]> {
    let q = mv(h, [p[0], p[1], 1.0]);
    (q[2].abs() > 1e-12).then(|| [q[0] / q[2], q[1] / q[2]])
}

/// RANSAC homography between two images' matches; returns it with its inlier pairs.
fn ransac(fa: &[Feat], fb: &[Feat], m: &[(usize, usize)]) -> Option<(M3, Vec<(usize, usize)>)> {
    if m.len() < 12 {
        return None;
    }
    let pt = |&(i, j): &(usize, usize)| ([fa[i].x, fa[i].y], [fb[j].x, fb[j].y]);
    let mut seed = 0x9E3779B97F4A7C15u64 ^ m.len() as u64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as usize
    };
    let inliers = |h: &M3| m.iter().filter(|q| {
        let (a, b) = pt(q);
        project(h, b).is_some_and(|p| (p[0] - a[0]).powi(2) + (p[1] - a[1]).powi(2) < 9.0)
    }).copied().collect::<Vec<_>>();
    let mut best: Option<(M3, Vec<(usize, usize)>)> = None;
    for _ in 0..1500 {
        let mut idx = [0usize; 4];
        for k in 0..4 {
            idx[k] = rnd() % m.len();
        }
        if (0..4).any(|a| (a + 1..4).any(|b| idx[a] == idx[b])) {
            continue;
        }
        let Some(h) = homography(&idx.map(|k| pt(&m[k]))) else { continue };
        let inl = inliers(&h);
        if best.as_ref().is_none_or(|b| inl.len() > b.1.len()) {
            best = Some((h, inl));
        }
    }
    let (_, inl) = best?;
    let h = homography(&inl.iter().map(pt).collect::<Vec<_>>())?;
    let inl = inliers(&h);
    // Too few agreeing matches for the number found means a chance alignment (OpenCV's rule).
    (inl.len() >= 15 && inl.len() as f64 > 8.0 + 0.3 * m.len() as f64).then_some((h, inl))
}

/// Focal length (pixels) implied by a rotation-only homography, for each of the two images (Szeliski's formulas).
fn focal_from_h(h: &M3) -> Option<f64> {
    let h = [h[0][0], h[0][1], h[0][2], h[1][0], h[1][1], h[1][2], h[2][0], h[2][1], h[2][2]];
    let pick = |d1: f64, d2: f64, mut v1: f64, mut v2: f64| {
        if v1 < v2 {
            std::mem::swap(&mut v1, &mut v2);
        }
        if v1 > 0.0 && v2 > 0.0 { Some((if d1.abs() > d2.abs() { v1 } else { v2 }).sqrt()) } else if v1 > 0.0 { Some(v1.sqrt()) } else { None }
    };
    let (d1, d2) = (h[6] * h[7], (h[7] - h[6]) * (h[7] + h[6]));
    let f1 = pick(d1, d2, -(h[0] * h[1] + h[3] * h[4]) / d1, (h[0] * h[0] + h[3] * h[3] - h[1] * h[1] - h[4] * h[4]) / d2);
    let (d1, d2) = (h[0] * h[3] + h[1] * h[4], h[0] * h[0] + h[1] * h[1] - h[3] * h[3] - h[4] * h[4]);
    let f0 = pick(d1, d2, -h[2] * h[5] / d1, (h[5] * h[5] - h[2] * h[2]) / d2);
    match (f0, f1) {
        (Some(a), Some(b)) if a.is_finite() && b.is_finite() => Some((a * b).sqrt()),
        _ => None,
    }
}

// ---------- alignment ----------

struct Pair {
    i: usize,
    j: usize,
    h: M3,
    /// Inlier points (centred, small-image pixels): in i, in j.
    pts: Vec<([f64; 2], [f64; 2])>,
}

/// Camera rotations (world → camera) and the focal length in small-image pixels of image 0's scale.
fn align(sm: &[Small], focal_hint: Option<f64>, progress: &dyn Fn(&str)) -> Result<(Vec<M3>, f64), String> {
    let n = sm.len();
    progress("Finding features");
    let feats: Vec<Vec<Feat>> = sm.par_iter().map(features).collect();
    progress("Matching photos");
    let pairs: Vec<(usize, usize)> = (0..n).flat_map(|i| (i + 1..n).map(move |j| (i, j))).collect();
    let mut found: Vec<Pair> = pairs.par_iter().filter_map(|&(i, j)| {
        let m = match_feats(&feats[i], &feats[j]);
        let (h, inl) = ransac(&feats[i], &feats[j], &m)?;
        let pts = inl.iter().map(|&(a, b)| ([feats[i][a].x, feats[i][a].y], [feats[j][b].x, feats[j][b].y])).collect();
        Some(Pair { i, j, h, pts })
    }).collect();
    found.sort_by_key(|p| std::cmp::Reverse(p.pts.len()));
    // Images at different sizes (portrait and landscape mixed) share one focal length in full-size pixels.
    let scale: Vec<f64> = sm.iter().map(|s| s.s / sm[0].s).collect();
    let mut f = focal_hint.unwrap_or(0.0);
    if f <= 0.0 {
        let mut est: Vec<f64> = found.iter().filter_map(|p| focal_from_h(&p.h)).filter(|f| *f > 50.0 && *f < 20000.0).collect();
        est.sort_by(|a, b| a.total_cmp(b));
        f = est.get(est.len() / 2).copied().unwrap_or(sm[0].w.max(sm[0].h) as f64 * 1.2);
    }
    // Spanning tree with the strongest links, from the best-connected photo.
    let total = |k: usize| found.iter().filter(|p| p.i == k || p.j == k).map(|p| p.pts.len()).sum::<usize>();
    let root = (0..n).max_by_key(|k| total(*k)).unwrap_or(0);
    let k_of = |i: usize, f: f64| { let fi = f * scale[i]; [[fi, 0.0, 0.0], [0.0, fi, 0.0], [0.0, 0.0, 1.0]] };
    let mut rot: Vec<Option<M3>> = vec![None; n];
    rot[root] = Some([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    loop {
        let next = found.iter().find(|p| rot[p.i].is_some() != rot[p.j].is_some());
        let Some(p) = next else { break };
        // H maps j to i: R_i R_jᵀ ∝ K_i⁻¹ H K_j.
        let m = orthonormalize(mul(&mul(&inv(&k_of(p.i, f)).unwrap(), &p.h), &k_of(p.j, f)));
        if let Some(ri) = rot[p.i] {
            rot[p.j] = Some(mul(&tr(&m), &ri));
        } else if let Some(rj) = rot[p.j] {
            rot[p.i] = Some(mul(&m, &rj));
        }
    }
    let missing: Vec<usize> = (0..n).filter(|k| rot[*k].is_none()).collect();
    if !missing.is_empty() {
        return Err(format!("no overlap found for photo {} (photos need about a third overlapping)", missing.iter().map(|k| (k + 1).to_string()).collect::<Vec<_>>().join(", ")));
    }
    let mut rot: Vec<M3> = rot.into_iter().flatten().collect();

    // Bundle adjustment: refine every rotation and the focal length so all overlaps agree at once.
    progress("Refining alignment");
    // Matches that disagree with the chained rotations are chance alignments (repeating patterns, sky): leave them out.
    let err0 = |i: usize, j: usize, a: [f64; 2], b: [f64; 2]| {
        let c = mv(&rot[i], mv(&tr(&rot[j]), [b[0], b[1], f * scale[j]]));
        let fi = f * scale[i];
        if c[2] <= 1e-9 { f64::MAX } else { (fi * c[0] / c[2] - a[0]).hypot(fi * c[1] / c[2] - a[1]) }
    };
    let obs: Vec<(usize, usize, [f64; 2], [f64; 2])> = found.iter().flat_map(|p| {
        let good: Vec<_> = p.pts.iter().filter(|q| err0(p.i, p.j, q.0, q.1) < 5.0).collect();
        let step = (good.len() / 60).max(1);
        let keep = if good.len() >= 8 { good } else { vec![] };
        keep.into_iter().step_by(step).map(move |q| (p.i, p.j, q.0, q.1)).collect::<Vec<_>>()
    }).collect();
    let np = 3 * n + 1;
    // Reprojection error both ways, in pixels: each match's point carried from one photo into the other.
    let resid = |x: &[f64]| -> Vec<f64> {
        let f = f * x[3 * n].exp();
        let r: Vec<M3> = (0..n).map(|k| mul(&rodrigues([x[3 * k], x[3 * k + 1], x[3 * k + 2]]), &rot[k])).collect();
        let carry = |from: usize, to: usize, p: [f64; 2]| {
            let d = mv(&tr(&r[from]), [p[0], p[1], f * scale[from]]);
            let c = mv(&r[to], d);
            let ft = f * scale[to];
            if c[2] > 1e-9 { [ft * c[0] / c[2], ft * c[1] / c[2]] } else { [1e6, 1e6] }
        };
        obs.iter().flat_map(|&(i, j, a, b)| {
            let (pa, pb) = (carry(j, i, b), carry(i, j, a));
            [pa[0] - a[0], pa[1] - a[1], pb[0] - b[0], pb[1] - b[1]]
        }).collect()
    };
    let mut x = vec![0.0; np];
    let mut lambda = 1e-3;
    let mut cost: f64 = resid(&x).iter().map(|v| v * v).sum();

    for _ in 0..30 {
        let r0 = resid(&x);
        // Numeric Jacobian; the root photo's rotation is held fixed.
        let cols: Vec<Option<Vec<f64>>> = (0..np).into_par_iter().map(|c| {
            if c / 3 == root && c < 3 * n {
                return None;
            }
            let mut xp = x.clone();
            let eps = 1e-6;
            xp[c] += eps;
            Some(resid(&xp).iter().zip(&r0).map(|(a, b)| (a - b) / eps).collect())
        }).collect();
        let mut jtj = vec![vec![0.0; np]; np];
        let mut jtr = vec![0.0; np];
        for a in 0..np {
            let Some(ca) = &cols[a] else { jtj[a][a] = 1.0; continue };
            jtr[a] = -ca.iter().zip(&r0).map(|(p, q)| p * q).sum::<f64>();
            for b in a..np {
                if let Some(cb) = &cols[b] {
                    let v: f64 = ca.iter().zip(cb).map(|(p, q)| p * q).sum();
                    jtj[a][b] = v;
                    jtj[b][a] = v;
                }
            }
        }
        let mut improved = false;
        for _ in 0..8 {
            let mut a = jtj.clone();
            (0..np).for_each(|k| a[k][k] *= 1.0 + lambda);
            let Some(dx) = solve(a, jtr.clone()) else { lambda *= 10.0; continue };
            let xn: Vec<f64> = x.iter().zip(&dx).map(|(a, b)| a + b).collect();
            let c: f64 = resid(&xn).iter().map(|v| v * v).sum();
            if c < cost {
                (x, cost, improved) = (xn, c, true);
                lambda = (lambda / 10.0).max(1e-9);
                break;
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    let f = f * x[3 * n].exp();
    rot = (0..n).map(|k| mul(&rodrigues([x[3 * k], x[3 * k + 1], x[3 * k + 2]]), &rot[k])).collect();

    // Level the horizon: the camera's left-right axes all lie in the horizontal plane (OpenCV's wave correction).
    let mut moment = vec![vec![0.0; 3]; 3];
    for r in &rot {
        for a in 0..3 {
            for b in 0..3 {
                moment[a][b] += r[0][a] * r[0][b];
            }
        }
    }
    let (vals, vecs) = jacobi(moment);
    let mut order = [0, 1, 2];
    order.sort_by(|a, b| vals[*a].total_cmp(&vals[*b]));
    // With all cameras in one column there is no horizontal spread to learn the horizon from.
    if n > 1 && vals[order[1]] > 4.0 * vals[order[0]].max(1e-9) {
        let mut up: V3 = [0, 1, 2].map(|c| vecs[c][order[0]]);
        let fwd = rot.iter().fold([0.0; 3], |a, r| [a[0] + r[2][0], a[1] + r[2][1], a[2] + r[2][2]]);
        let mut right = norm(cross(up, fwd));
        if rot.iter().map(|r| right[0] * r[0][0] + right[1] * r[0][1] + right[2] * r[0][2]).sum::<f64>() < 0.0 {
            right = right.map(|v| -v);
            up = up.map(|v| -v);
        }
        let back = cross(right, up);
        let w: M3 = [right, up, back];
        rot = rot.iter().map(|r| mul(r, &tr(&w))).collect();
    }
    Ok((rot, f))
}

// ---------- projection and blending ----------

fn to_uv(p: Projection, d: V3) -> Option<(f64, f64)> {
    match p {
        Projection::Perspective => (d[2] > 1e-6).then(|| (d[0] / d[2], d[1] / d[2])),
        Projection::Cylindrical => {
            let r = d[0].hypot(d[2]);
            (r > 1e-9).then(|| (d[0].atan2(d[2]), d[1] / r))
        }
        _ => {
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            Some((d[0].atan2(d[2]), (d[1] / l).asin()))
        }
    }
}

fn from_uv(p: Projection, u: f64, v: f64) -> V3 {
    match p {
        Projection::Perspective => [u, v, 1.0],
        Projection::Cylindrical => [u.sin(), v, u.cos()],
        _ => [u.sin() * v.cos(), v.sin(), u.cos() * v.cos()],
    }
}

/// Largest axis-aligned rectangle of valid pixels (row-histogram method). Returns x0, y0, x1, y1.
fn largest_rect(mask: &[bool], w: usize, h: usize) -> (usize, usize, usize, usize) {
    let mut heights = vec![0usize; w];
    let mut best = (0, (0, 0, w, h));
    for y in 0..h {
        for x in 0..w {
            heights[x] = if mask[y * w + x] { heights[x] + 1 } else { 0 };
        }
        let mut stack: Vec<usize> = vec![];
        for x in 0..=w {
            let cur = if x < w { heights[x] } else { 0 };
            while let Some(&top) = stack.last() {
                if heights[top] <= cur {
                    break;
                }
                stack.pop();
                let left = stack.last().map_or(0, |l| l + 1);
                let area = heights[top] * (x - left);
                if area > best.0 {
                    best = (area, (left, y + 1 - heights[top], x, y + 1));
                }
            }
            stack.push(x);
        }
    }
    best.1
}

/// Merge overlapping photos (linear light). `focal_hint` is the focal length in full-size pixels of the first photo,
/// from EXIF, when known. Returns the panorama and a crop [left, top, right, bottom] that trims the empty edges.
pub fn stitch(imgs: &[Rgb32FImage], focal_hint: Option<f64>, proj: Projection, max_mp: f64, progress: &dyn Fn(&str)) -> Result<(Rgb32FImage, [f32; 4]), String> {
    if imgs.len() < 2 {
        return Err("pick at least two photos".into());
    }
    let sm: Vec<Small> = imgs.par_iter().map(small).collect();
    let (rot, f_small) = align(&sm, focal_hint.map(|f| f * sm[0].s), progress)?;
    // Full-size focal length per photo.
    let fl: Vec<f64> = (0..imgs.len()).map(|k| f_small / sm[0].s * (imgs[k].width() as f64 / imgs[0].width() as f64)).collect();
    let rays = |k: usize, x: f64, y: f64| {
        let (cx, cy) = (imgs[k].width() as f64 / 2.0, imgs[k].height() as f64 / 2.0);
        mv(&tr(&rot[k]), [x - cx, y - cy, fl[k]])
    };
    let border = |k: usize| {
        let (w, h) = (imgs[k].width() as f64, imgs[k].height() as f64);
        (0..=40).flat_map(move |i| {
            let t = i as f64 / 40.0;
            [(t * w, 0.0), (t * w, h), (0.0, t * h), (w, t * h)]
        })
    };
    let bounds = |p: Projection| -> Option<Vec<[f64; 4]>> {
        (0..imgs.len()).map(|k| {
            let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for (x, y) in border(k) {
                let (u, v) = to_uv(p, rays(k, x, y))?;
                b = [b[0].min(u), b[1].min(v), b[2].max(u), b[3].max(v)];
            }
            Some(b)
        }).collect()
    };
    let span = |bb: &[[f64; 4]]| {
        let all = bb.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
        all
    };
    let proj = match proj {
        Projection::Auto => {
            let s = span(&bounds(Projection::Spherical).ok_or("alignment failed")?);
            if s[2] - s[0] < 1.6 && s[3] - s[1] < 1.6 {
                Projection::Perspective
            } else if s[3] - s[1] > 1.4 {
                Projection::Spherical
            } else {
                Projection::Cylindrical
            }
        }
        p => p,
    };
    let bb = bounds(proj).filter(|b| proj != Projection::Perspective || span(b)[2] - span(b)[0] < 4.0)
        .ok_or("too wide for a perspective projection: try Cylindrical or Spherical")?;
    let all = span(&bb);
    let mut scale = fl[0];
    let mp = (all[2] - all[0]) * (all[3] - all[1]) * scale * scale / 1e6;
    if mp > max_mp {
        scale *= (max_mp / mp).sqrt();
    }
    let (ow, oh) = (((all[2] - all[0]) * scale).ceil() as usize, ((all[3] - all[1]) * scale).ceil() as usize);
    let to_img = |k: usize, d: V3| -> Option<(f32, f32)> {
        let c = mv(&rot[k], d);
        if c[2] <= 1e-9 {
            return None;
        }
        let (x, y) = (fl[k] * c[0] / c[2] + imgs[k].width() as f64 / 2.0, fl[k] * c[1] / c[2] + imgs[k].height() as f64 / 2.0);
        (x >= 0.0 && y >= 0.0 && x <= (imgs[k].width() - 1) as f64 && y <= (imgs[k].height() - 1) as f64).then_some((x as f32, y as f32))
    };
    let feather = |k: usize, x: f32, y: f32| {
        let (w, h) = (imgs[k].width() as f32, imgs[k].height() as f32);
        let (a, b) = ((x.min(w - 1.0 - x) / (w / 2.0)).clamp(0.0, 1.0), (y.min(h - 1.0 - y) / (h / 2.0)).clamp(0.0, 1.0));
        (a * b).powi(4) + 1e-12
    };

    // Exposure matching from a small render: mean brightness of each photo where it overlaps each other one.
    progress("Matching exposure");
    let n = imgs.len();
    let ls = (1200.0 / ow as f64).min(1.0);
    let (lw, lh) = (((ow as f64) * ls) as usize, ((oh as f64) * ls) as usize);
    let mut stat = vec![vec![(0.0f64, 0.0f64); n]; n]; // (count, sum of i's luma) where i and j overlap
    let mut mask = vec![false; lw * lh];
    for y in 0..lh {
        for x in 0..lw {
            let (u, v) = (all[0] + (x as f64 + 0.5) / ls / scale, all[1] + (y as f64 + 0.5) / ls / scale);
            let d = from_uv(proj, u, v);
            let hits: Vec<(usize, f64)> = (0..n).filter(|k| u >= bb[*k][0] && u <= bb[*k][2] && v >= bb[*k][1] && v <= bb[*k][3])
                .filter_map(|k| to_img(k, d).map(|(px, py)| {
                    let c = imgs[k].get_pixel(px as u32, py as u32).0;
                    (k, crate::develop::luma(c) as f64)
                })).collect();
            mask[y * lw + x] = !hits.is_empty();
            for &(i, li) in &hits {
                for &(j, _) in &hits {
                    if i != j {
                        stat[i][j].0 += 1.0;
                        stat[i][j].1 += li;
                    }
                }
            }
        }
    }
    let (alpha, beta) = (0.01, 100.0);
    let mut a = vec![vec![0.0; n]; n];
    let mut b = vec![0.0; n];
    for i in 0..n {
        for j in 0..n {
            let nij = stat[i][j].0;
            if i == j || nij == 0.0 {
                continue;
            }
            let (iij, iji) = (stat[i][j].1 / nij * 255.0, stat[j][i].1 / stat[j][i].0.max(1.0) * 255.0);
            b[i] += beta * nij;
            a[i][i] += beta * nij + 2.0 * alpha * iij * iij * nij;
            a[i][j] -= 2.0 * alpha * iij * iji * nij;
        }
        if a[i][i] == 0.0 {
            a[i][i] = 1.0;
            b[i] = 1.0;
        }
    }
    let gains: Vec<f32> = solve(a, b).unwrap_or(vec![1.0; n]).iter().map(|g| g.clamp(0.25, 4.0) as f32).collect();

    progress("Blending");
    let mut out = Rgb32FImage::new(ow as u32, oh as u32);
    out.par_chunks_mut(ow * 3).enumerate().for_each(|(y, row)| {
        let v = all[1] + (y as f64 + 0.5) / scale;
        let live: Vec<usize> = (0..n).filter(|k| v >= bb[*k][1] && v <= bb[*k][3]).collect();
        for x in 0..ow {
            let u = all[0] + (x as f64 + 0.5) / scale;
            let d = from_uv(proj, u, v);
            let (mut acc, mut wsum) = ([0f32; 3], 0f32);
            for &k in &live {
                if u < bb[k][0] || u > bb[k][2] {
                    continue;
                }
                if let Some((px, py)) = to_img(k, d) {
                    let wgt = feather(k, px, py);
                    let c = crate::develop::bilinear(&imgs[k], px, py);
                    (0..3).for_each(|ch| acc[ch] += c[ch] * gains[k] * wgt);
                    wsum += wgt;
                }
            }
            if wsum > 0.0 {
                row[x * 3..x * 3 + 3].copy_from_slice(&acc.map(|c| c / wsum));
            }
        }
    });
    let (x0, y0, x1, y1) = largest_rect(&mask, lw, lh);
    let crop = [x0 as f32 / lw as f32, y0 as f32 / lh as f32, 1.0 - x1 as f32 / lw as f32, 1.0 - y1 as f32 / lh as f32].map(|c| (c + 0.002).min(0.45));
    Ok((out, crop))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic scene seen by a camera turning left to right: the panorama finds the turn and puts it back together.
    #[test]
    fn three_turned_views_merge_into_one() {
        // Value noise over the sphere: irregular texture with corners everywhere, like a real scene.
        let hash = |x: i64, y: i64| {
            let mut n = (x as u64).wrapping_mul(0x9E3779B97F4A7C15) ^ (y as u64).wrapping_mul(0xC2B2AE3D27D4EB4F);
            n ^= n >> 29;
            n = n.wrapping_mul(0xBF58476D1CE4E5B9);
            ((n >> 40) & 0xffff) as f64 / 65535.0
        };
        let noise = |u: f64, v: f64| {
            let (xi, yi) = (u.floor() as i64, v.floor() as i64);
            let (fx, fy) = (u - xi as f64, v - yi as f64);
            let a = hash(xi, yi) * (1.0 - fx) + hash(xi + 1, yi) * fx;
            let b = hash(xi, yi + 1) * (1.0 - fx) + hash(xi + 1, yi + 1) * fx;
            a * (1.0 - fy) + b * fy
        };
        let scene = |d: V3| {
            let (u, v) = (d[0].atan2(d[2]), (d[1] / (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()).asin());
            let a = noise(u * 40.0, v * 40.0) * 0.6 + noise(u * 90.0 + 7.0, v * 90.0) * 0.4;
            [0.03 + 0.5 * a as f32, 0.03 + 0.4 * a as f32, 0.05 + 0.3 * a as f32]
        };
        let (w, h, f) = (640u32, 427u32, 600.0);
        let views: Vec<Rgb32FImage> = [-0.45f64, 0.0, 0.45].iter().map(|yaw| {
            let r = rodrigues([0.0, *yaw, 0.0]);
            Rgb32FImage::from_fn(w, h, |x, y| {
                let d = mv(&r, [x as f64 - w as f64 / 2.0, y as f64 - h as f64 / 2.0, f]);
                image::Rgb(scene(d))
            })
        }).collect();
        let (pano, crop) = stitch(&views, None, Projection::Cylindrical, 50.0, &|_| {}).unwrap();
        // Three views 26° apart with a ~56° field of view each: about 108° across.
        let want = f * (0.9 + 2.0 * (w as f64 / 2.0 / f).atan());
        assert!(((pano.width() as f64) - want).abs() < want * 0.08, "{} vs {want}", pano.width());
        assert!(crop.iter().all(|c| *c < 0.3), "{crop:?}");
        // The middle of the panorama is the middle photo's centre.
        let (cx, cy) = (pano.width() / 2, pano.height() / 2);
        let (a, b) = (pano.get_pixel(cx, cy).0, views[1].get_pixel(w / 2, h / 2).0);
        assert!((a[0] - b[0]).abs() < 0.08, "{a:?} {b:?}");
    }

    #[test]
    fn largest_rectangle_in_a_mask() {
        let (w, h) = (10, 6);
        let mask: Vec<bool> = (0..w * h).map(|i| { let (x, y) = (i % w, i / w); x >= 2 && y >= 1 && !(x == 9 && y == 5) }).collect();
        assert_eq!(largest_rect(&mask, w, h), (2, 1, 9, 6));
    }
}

/// Homography taking full-size pixel positions in `b` to `a` (same scene, roughly the same framing, as in an
/// exposure bracket). `gain_b` brightens `b` to match `a` before looking for features. None when the two can't
/// be matched (e.g. a featureless scene; a tripod bracket then needs no alignment anyway).
pub fn align_pair(a: &Rgb32FImage, b: &Rgb32FImage, gain_b: f32) -> Option<[[f64; 3]; 3]> {
    let mut b2 = b.clone();
    b2.iter_mut().for_each(|v| *v = (*v * gain_b).min(1.0));
    let mut a2 = a.clone();
    a2.iter_mut().for_each(|v| *v = v.min(1.0));
    let (sa, sb) = (small(&a2), small(&b2));
    let (fa, fb) = (features(&sa), features(&sb));
    let (h, _) = ransac(&fa, &fb, &match_feats(&fa, &fb))?;
    // Small-image coordinates are centred: full = (small + size / 2) / s.
    let to_small = |s: &Small| [[s.s, 0.0, -(s.w as f64) / 2.0], [0.0, s.s, -(s.h as f64) / 2.0], [0.0, 0.0, 1.0]];
    let full = mul(&mul(&inv(&to_small(&sa))?, &h), &to_small(&sb));
    (full[2][2].abs() > 1e-12).then(|| full.map(|r| r.map(|v| v / full[2][2])))
}

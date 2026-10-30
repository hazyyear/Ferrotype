//! RAW development with our own demosaicing and colour, in place of rawler's PPG (Bayer) and bilinear (X-Trans).
//!
//! - Bayer: RCD (Ratio Corrected Demosaicing, Luis Sanz Rodríguez), as in RawTherapee and darktable. Sharper
//!   than PPG with fewer zippers and colour moiré.
//! - Fuji X-Trans: green from direction-weighted neighbours, then red and blue as colour differences spread
//!   with weights that follow the green detail (edge-aware), and a final green refinement. Much finer than the
//!   bilinear rawler uses.
//! - Colour: the camera matrix is applied without clipping negative channels, so colours outside sRGB survive
//!   for wide-gamut exports (see `colour`). Highlights above white keep rawler's hue-preserving blend.
//!
//! Anything else (4-colour sensors, monochrome, linear DNGs, Fuji SuperCCD) goes back to rawler.

use image::Rgb32FImage;
use rawler::rawimage::{RawImage, RawPhotometricInterpretation};
use rayon::prelude::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sensor {
    Bayer,
    XTrans,
}

/// Develop a RAW to linear sRGB-primaries RGB (extended: negatives allowed), or None if we don't handle it.
pub fn develop(raw: &RawImage) -> Option<Rgb32FImage> {
    if raw.cpp != 1 || raw.fuji_rotation_width.is_some() {
        return None;
    }
    let RawPhotometricInterpretation::Cfa(config) = &raw.photometric else { return None };
    if !config.cfa.is_rgb() || config.cfa.unique_colors() != 3 {
        return None;
    }
    let sensor = match config.sensor {
        rawler::imgop::sensor::SensorType::Bayer if config.cfa.width == 2 && config.cfa.height == 2 => Sensor::Bayer,
        rawler::imgop::sensor::SensorType::Xtrans => Sensor::XTrans,
        _ => return None,
    };
    let mut r = raw.clone();
    r.apply_scaling().ok()?;
    let data = r.data.as_f32();
    let (w, h) = (r.width, r.height);
    let cfa = &config.cfa;
    let colour_at = |y: usize, x: usize| cfa.color_at(y, x) as u8;
    let pattern: Vec<u8> = (0..h * w).map(|i| colour_at(i / w, i % w)).collect();
    let planes = match sensor {
        Sensor::Bayer => rcd(&data, &pattern, w, h),
        Sensor::XTrans => xtrans(&data, &pattern, w, h),
    };
    let rgb = to_srgb(raw, &planes, w * h)?;
    // Active area (the sensor's usable part), then the camera's default crop inside it.
    let mut img = Rgb32FImage::from_raw(w as u32, h as u32, rgb)?;
    let active = raw.active_area.unwrap_or(rawler::imgop::Rect::new(rawler::imgop::Point::new(0, 0), rawler::imgop::Dim2::new(w, h)));
    let mut crop = active;
    if let Some(c) = raw.crop_area {
        crop = c;
    }
    let (cx, cy) = (crop.p.x.min(w.saturating_sub(1)), crop.p.y.min(h.saturating_sub(1)));
    let (cw, ch) = (crop.d.w.min(w - cx).max(1), crop.d.h.min(h - cy).max(1));
    if (cx, cy, cw, ch) != (0, 0, w, h) {
        img = image::imageops::crop_imm(&img, cx as u32, cy as u32, cw as u32, ch as u32).to_image();
    }
    Some(img)
}

/// White balance (the camera's as-shot), then camera RGB → linear sRGB with the camera's D65 matrix, as rawler
/// does, but keeping negative channels (colours sRGB can't show).
fn to_srgb(raw: &RawImage, planes: &[Vec<f32>; 3], n: usize) -> Option<Vec<f32>> {
    use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse, transform_1d};
    use rawler::imgop::xyz::{Illuminant, SRGB_TO_XYZ_D65};
    let (illu, m) = raw.color_matrix_find_first([Illuminant::D65, Illuminant::A, Illuminant::B, Illuminant::C, Illuminant::D50, Illuminant::D55, Illuminant::D75, Illuminant::Daylight, Illuminant::Flash])?;
    if m.len() != 9 {
        return None;
    }
    let mut xyz2cam: [[f32; 3]; 3] = transform_1d::<3, 3>(&m)?;
    if illu != Illuminant::D65 {
        xyz2cam = rawler::imgop::chromatic_adaption::adapt_bradford(&illu, &Illuminant::D65, &xyz2cam);
    }
    let rgb2cam = normalize(multiply(&xyz2cam, &SRGB_TO_XYZ_D65));
    let cam2rgb = pseudo_inverse(rgb2cam);
    let wb = if raw.wb_coeffs[0].is_nan() { [1.0; 4] } else { raw.wb_coeffs };
    let mut out = vec![0f32; n * 3];
    out.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
        let c = [planes[0][i] * wb[0], planes[1][i] * wb[1], planes[2][i] * wb[2]];
        let mut s = [0, 1, 2].map(|k| cam2rgb[k][0] * c[0] + cam2rgb[k][1] * c[1] + cam2rgb[k][2] * c[2]);
        let max = s[0].max(s[1]).max(s[2]);
        if max > 1.0 {
            // Past white: rawler's blend of the colour's direction and its brightness, so clipped highlights
            // turn white smoothly instead of magenta.
            s = rawler::imgop::raw::clip_euclidean_norm_avg(&s);
        } else if s[0] + s[1] + s[2] < 0.0 {
            s = [0.0; 3];
        }
        px.copy_from_slice(&s);
    });
    Some(out)
}

/// Every pixel's colour from its own and its neighbours' samples, simply (fills the borders the real
/// algorithms leave out, and starts them off).
fn bilinear(cfa: &[f32], pattern: &[u8], w: usize, h: usize, radius: usize) -> [Vec<f32>; 3] {
    bilinear_edges(cfa, pattern, w, h, radius, None)
}

/// `bilinear`, or with `edge` set only within that many pixels of the border (inside, only the pixel's own
/// sample is filled in: the caller computes the rest).
fn bilinear_edges(cfa: &[f32], pattern: &[u8], w: usize, h: usize, radius: usize, edge: Option<usize>) -> [Vec<f32>; 3] {
    let mut out = [vec![0f32; w * h], vec![0f32; w * h], vec![0f32; w * h]];
    let rows: Vec<Vec<[f32; 3]>> = (0..h).into_par_iter().map(|y| {
        (0..w).map(|x| {
            let i = y * w + x;
            if let Some(e) = edge
                && y >= e && x >= e && y + e < h && x + e < w
            {
                let mut v = [0f32; 3];
                v[pattern[i] as usize] = cfa[i];
                return v;
            }
            let mut sum = [0f32; 3];
            let mut cnt = [0f32; 3];
            let r = radius as isize;
            for dy in -r..=r {
                for dx in -r..=r {
                    let (yy, xx) = (y as isize + dy, x as isize + dx);
                    if yy < 0 || xx < 0 || yy >= h as isize || xx >= w as isize {
                        continue;
                    }
                    let j = yy as usize * w + xx as usize;
                    let c = pattern[j] as usize;
                    let wt = if dx == 0 && dy == 0 { 4.0 } else { 1.0 / (dx * dx + dy * dy) as f32 };
                    sum[c] += cfa[j] * wt;
                    cnt[c] += wt;
                }
            }
            let mut v = [0, 1, 2].map(|c| if cnt[c] > 0.0 { sum[c] / cnt[c] } else { 0.0 });
            v[pattern[i] as usize] = cfa[i];
            v
        }).collect()
    }).collect();
    for (y, row) in rows.into_iter().enumerate() {
        for (x, v) in row.into_iter().enumerate() {
            for c in 0..3 {
                out[c][y * w + x] = v[c];
            }
        }
    }
    out
}

/// RCD demosaicing for a 2×2 Bayer pattern.
pub fn rcd(cfa: &[f32], pattern: &[u8], w: usize, h: usize) -> [Vec<f32>; 3] {
    if w < 16 || h < 16 {
        return bilinear(cfa, pattern, w, h, 1);
    }
    // RCD fills everything 4 px in from the border; the border itself gets the simple fill.
    let mut rgb = bilinear_edges(cfa, pattern, w, h, 1, Some(4));
    const EPS: f32 = 1e-5;
    const EPSSQ: f32 = 1e-10;
    let (w1, w2, w3, w4) = (w as isize, 2 * w as isize, 3 * w as isize, 4 * w as isize);
    let at = |i: usize, d: isize| cfa[(i as isize + d) as usize];
    let sq = |x: f32| x * x;
    let intp = |a: f32, b: f32, c: f32| a * (b - c) + c;
    let green = |i: usize| pattern[i] == 1;

    // Step 1: vertical / horizontal direction from high-pass colour differences.
    let mut hpf_v = vec![0f32; w * h];
    let mut hpf_h = vec![0f32; w * h];
    hpf_v.par_chunks_mut(w).zip(hpf_h.par_chunks_mut(w)).enumerate().for_each(|(y, (v, hh))| {
        if y < 3 || y >= h - 3 {
            return;
        }
        for x in 3..w - 3 {
            let i = y * w + x;
            let c = cfa[i];
            v[x] = sq((at(i, -w3) - at(i, -w1) - at(i, w1) + at(i, w3)) - 3.0 * (at(i, -w2) + at(i, w2)) + 6.0 * c);
            hh[x] = sq((at(i, -3) - at(i, -1) - at(i, 1) + at(i, 3)) - 3.0 * (at(i, -2) + at(i, 2)) + 6.0 * c);
        }
    });
    let mut vh = vec![0.5f32; w * h];
    vh.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y < 4 || y >= h - 4 {
            return;
        }
        for x in 4..w - 4 {
            let i = y * w + x;
            let v = (hpf_v[i - w] + hpf_v[i] + hpf_v[i + w]).max(EPSSQ);
            let hz = (hpf_h[i - 1] + hpf_h[i] + hpf_h[i + 1]).max(EPSSQ);
            row[x] = v / (v + hz);
        }
    });
    drop((hpf_v, hpf_h));

    // Step 2: low-pass filter at red and blue sites.
    let mut lpf = vec![0f32; w * h];
    lpf.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y < 2 || y >= h - 2 {
            return;
        }
        for x in 2..w - 2 {
            let i = y * w + x;
            if green(i) {
                continue;
            }
            row[x] = cfa[i] + 0.5 * (cfa[i - w] + cfa[i + w] + cfa[i - 1] + cfa[i + 1]) + 0.25 * (cfa[i - w - 1] + cfa[i - w + 1] + cfa[i + w - 1] + cfa[i + w + 1]);
        }
    });

    // Step 3: green at red and blue sites (written in place: it reads only the mosaic and the maps above).
    rgb[1].par_chunks_mut(w).enumerate().for_each(|(y, grow)| {
        if y < 4 || y >= h - 4 {
            return;
        }
        for x in 4..w - 4 {
            let i = y * w + x;
            if green(i) {
                continue;
            }
            let central = vh[i];
            let neigh = 0.25 * (vh[i - w - 1] + vh[i - w + 1] + vh[i + w - 1] + vh[i + w + 1]);
            let disc = if (0.5 - central).abs() < (0.5 - neigh).abs() { neigh } else { central };
            let c = cfa[i];
            let n_g = EPS + (at(i, -w1) - at(i, w1)).abs() + (c - at(i, -w2)).abs() + (at(i, -w1) - at(i, -w3)).abs() + (at(i, -w2) - at(i, -w4)).abs();
            let s_g = EPS + (at(i, -w1) - at(i, w1)).abs() + (c - at(i, w2)).abs() + (at(i, w1) - at(i, w3)).abs() + (at(i, w2) - at(i, w4)).abs();
            let w_g = EPS + (at(i, -1) - at(i, 1)).abs() + (c - at(i, -2)).abs() + (at(i, -1) - at(i, -3)).abs() + (at(i, -2) - at(i, -4)).abs();
            let e_g = EPS + (at(i, -1) - at(i, 1)).abs() + (c - at(i, 2)).abs() + (at(i, 1) - at(i, 3)).abs() + (at(i, 2) - at(i, 4)).abs();
            let l = lpf[i];
            let n_e = at(i, -w1) * (l + l) / (EPS + l + lpf[i - 2 * w]);
            let s_e = at(i, w1) * (l + l) / (EPS + l + lpf[i + 2 * w]);
            let w_e = at(i, -1) * (l + l) / (EPS + l + lpf[i - 2]);
            let e_e = at(i, 1) * (l + l) / (EPS + l + lpf[i + 2]);
            let v_est = (s_g * n_e + n_g * s_e) / (n_g + s_g);
            let h_est = (w_g * e_e + e_g * w_e) / (e_g + w_g);
            grow[x] = intp(disc, h_est, v_est);
        }
    });
    drop(lpf);

    // Step 4.1: diagonal direction at red and blue sites.
    let mut p_hpf = vec![0f32; w * h];
    let mut q_hpf = vec![0f32; w * h];
    p_hpf.par_chunks_mut(w).zip(q_hpf.par_chunks_mut(w)).enumerate().for_each(|(y, (p, q))| {
        if y < 3 || y >= h - 3 {
            return;
        }
        for x in 3..w - 3 {
            let i = y * w + x;
            let c = cfa[i];
            p[x] = sq((at(i, -w3 - 3) - at(i, -w1 - 1) - at(i, w1 + 1) + at(i, w3 + 3)) - 3.0 * (at(i, -w2 - 2) + at(i, w2 + 2)) + 6.0 * c);
            q[x] = sq((at(i, -w3 + 3) - at(i, -w1 + 1) - at(i, w1 - 1) + at(i, w3 - 3)) - 3.0 * (at(i, -w2 + 2) + at(i, w2 - 2)) + 6.0 * c);
        }
    });
    let mut pq = vec![0.5f32; w * h];
    pq.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        if y < 4 || y >= h - 4 {
            return;
        }
        for x in 4..w - 4 {
            let i = y * w + x;
            let p = (p_hpf[i - w - 1] + p_hpf[i] + p_hpf[i + w + 1]).max(EPSSQ);
            let q = (q_hpf[i - w + 1] + q_hpf[i] + q_hpf[i + w - 1]).max(EPSSQ);
            row[x] = p / (p + q);
        }
    });
    drop((p_hpf, q_hpf));

    // Step 4.2: the missing one of red and blue at blue and red sites (diagonal colour differences). Reads the
    // other colour's own samples on the diagonals, so it works from copies and writes in place.
    let (r_src, b_src) = (rgb[0].clone(), rgb[2].clone());
    {
        let [rp, gp, bp] = &mut rgb;
        let g = &*gp;
        rp.par_chunks_mut(w).zip(bp.par_chunks_mut(w)).enumerate().for_each(|(y, (rrow, brow))| {
            if y < 4 || y >= h - 4 {
                return;
            }
            for x in 4..w - 4 {
                let i = y * w + x;
                if green(i) {
                    continue;
                }
                let c = 2 - pattern[i] as usize;
                let rc = if c == 0 { &r_src } else { &b_src };
                let central = pq[i];
                let neigh = 0.25 * (pq[i - w - 1] + pq[i - w + 1] + pq[i + w - 1] + pq[i + w + 1]);
                let disc = if (0.5 - central).abs() < (0.5 - neigh).abs() { neigh } else { central };
                let idx = |d: isize| (i as isize + d) as usize;
                let nw_g = EPS + (rc[idx(-w1 - 1)] - rc[idx(w1 + 1)]).abs() + (rc[idx(-w1 - 1)] - rc[idx(-w3 - 3)]).abs() + (g[i] - g[idx(-w2 - 2)]).abs();
                let ne_g = EPS + (rc[idx(-w1 + 1)] - rc[idx(w1 - 1)]).abs() + (rc[idx(-w1 + 1)] - rc[idx(-w3 + 3)]).abs() + (g[i] - g[idx(-w2 + 2)]).abs();
                let sw_g = EPS + (rc[idx(-w1 + 1)] - rc[idx(w1 - 1)]).abs() + (rc[idx(w1 - 1)] - rc[idx(w3 - 3)]).abs() + (g[i] - g[idx(w2 - 2)]).abs();
                let se_g = EPS + (rc[idx(-w1 - 1)] - rc[idx(w1 + 1)]).abs() + (rc[idx(w1 + 1)] - rc[idx(w3 + 3)]).abs() + (g[i] - g[idx(w2 + 2)]).abs();
                let nw = rc[idx(-w1 - 1)] - g[idx(-w1 - 1)];
                let ne = rc[idx(-w1 + 1)] - g[idx(-w1 + 1)];
                let sw = rc[idx(w1 - 1)] - g[idx(w1 - 1)];
                let se = rc[idx(w1 + 1)] - g[idx(w1 + 1)];
                let p_est = (nw_g * se + se_g * nw) / (nw_g + se_g);
                let q_est = (ne_g * sw + sw_g * ne) / (ne_g + sw_g);
                let v = g[i] + intp(disc, q_est, p_est);
                if c == 0 { rrow[x] = v } else { brow[x] = v }
            }
        });
    }
    drop((r_src, b_src, pq));

    // Step 4.3: red and blue at green sites, from their now complete red and blue neighbours.
    let (r_src, b_src) = (rgb[0].clone(), rgb[2].clone());
    {
        let [rp, gp, bp] = &mut rgb;
        let g = &*gp;
        rp.par_chunks_mut(w).zip(bp.par_chunks_mut(w)).enumerate().for_each(|(y, (rrow, brow))| {
            if y < 4 || y >= h - 4 {
                return;
            }
            for x in 4..w - 4 {
                let i = y * w + x;
                if !green(i) {
                    continue;
                }
                let central = vh[i];
                let neigh = 0.25 * (vh[i - w - 1] + vh[i - w + 1] + vh[i + w - 1] + vh[i + w + 1]);
                let disc = if (0.5 - central).abs() < (0.5 - neigh).abs() { neigh } else { central };
                let g1 = g[i];
                let idx = |d: isize| (i as isize + d) as usize;
                for (c, rc) in [(0usize, &r_src), (2, &b_src)] {
                    let n_g = EPS + (g1 - g[idx(-w2)]).abs() + (rc[idx(-w1)] - rc[idx(w1)]).abs() + (rc[idx(-w1)] - rc[idx(-w3)]).abs();
                    let s_g = EPS + (g1 - g[idx(w2)]).abs() + (rc[idx(w1)] - rc[idx(-w1)]).abs() + (rc[idx(w1)] - rc[idx(w3)]).abs();
                    let w_g = EPS + (g1 - g[idx(-2)]).abs() + (rc[idx(-1)] - rc[idx(1)]).abs() + (rc[idx(-1)] - rc[idx(-3)]).abs();
                    let e_g = EPS + (g1 - g[idx(2)]).abs() + (rc[idx(1)] - rc[idx(-1)]).abs() + (rc[idx(1)] - rc[idx(3)]).abs();
                    let n_e = rc[idx(-w1)] - g[idx(-w1)];
                    let s_e = rc[idx(w1)] - g[idx(w1)];
                    let w_e = rc[idx(-1)] - g[idx(-1)];
                    let e_e = rc[idx(1)] - g[idx(1)];
                    let v_est = (n_g * s_e + s_g * n_e) / (n_g + s_g);
                    let h_est = (e_g * w_e + w_g * e_e) / (e_g + w_g);
                    let v = g1 + intp(disc, h_est, v_est);
                    if c == 0 { rrow[x] = v } else { brow[x] = v }
                }
            }
        });
    }
    rgb
}

/// X-Trans (6×6 pattern) demosaicing: directional green, edge-aware colour differences, green refinement.
pub fn xtrans(cfa: &[f32], pattern: &[u8], w: usize, h: usize) -> [Vec<f32>; 3] {
    let mut rgb = bilinear(cfa, pattern, w, h, 2);
    if w < 16 || h < 16 {
        return rgb;
    }
    const EPS: f32 = 1e-5;
    let (wi, hi) = (w as isize, h as isize);
    let inside = |y: isize, x: isize| y >= 0 && x >= 0 && y < hi && x < wi;

    // Green at red and blue sites, as this pixel's own colour minus the colour difference (own colour − green)
    // at its green neighbours (cardinal and diagonal). Each neighbour is weighted by how flat the picture is
    // in its direction, so the estimate follows edges instead of averaging across them.
    let dirs: [(isize, isize); 8] = [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (-1, 1), (1, -1), (1, 1)];
    let g0: Vec<f32> = (0..h * w).into_par_iter().map(|i| {
        let ch = pattern[i] as usize;
        if ch == 1 {
            return cfa[i];
        }
        let (y, x) = ((i / w) as isize, (i % w) as isize);
        let c = cfa[i];
        let (mut sum, mut wsum) = (0f32, 0f32);
        for (dy, dx) in dirs {
            let (yy, xx) = (y + dy, x + dx);
            if !inside(yy, xx) {
                continue;
            }
            let n = yy as usize * w + xx as usize;
            if pattern[n] != 1 {
                continue;
            }
            let d = rgb[ch][n] - cfa[n];
            let mut grad = EPS + (rgb[ch][n] - c).abs();
            let (y2, x2) = (y + 2 * dy, x + 2 * dx);
            if inside(y2, x2) && pattern[y2 as usize * w + x2 as usize] == 1 {
                grad += (cfa[y2 as usize * w + x2 as usize] - cfa[n]).abs();
            }
            let (yb, xb) = (y - dy, x - dx);
            if inside(yb, xb) && pattern[yb as usize * w + xb as usize] == 1 {
                grad += 0.5 * (cfa[yb as usize * w + xb as usize] - cfa[n]).abs();
            }
            let diag = if dy != 0 && dx != 0 { 0.5 } else { 1.0 };
            let wt = diag / (grad * grad);
            sum += d * wt;
            wsum += wt;
        }
        if wsum > 0.0 { c - sum / wsum } else { rgb[1][i] }
    }).collect();

    // Colour differences at red and blue sites, spread to every pixel with weights that drop where green
    // changes (so colour doesn't bleed across edges), within a 5×5 window.
    let spread = |g: &[f32], chan: u8| -> Vec<f32> {
        let diff: Vec<f32> = (0..w * h).map(|i| if pattern[i] == chan { cfa[i] - g[i] } else { 0.0 }).collect();
        (0..h * w).into_par_iter().map(|i| {
            if pattern[i] == chan {
                return diff[i];
            }
            let (y, x) = ((i / w) as isize, (i % w) as isize);
            let gi = g[i];
            let (mut s, mut ws) = (0f32, 0f32);
            for dy in -2isize..=2 {
                for dx in -2isize..=2 {
                    let (yy, xx) = (y + dy, x + dx);
                    if !inside(yy, xx) {
                        continue;
                    }
                    let j = yy as usize * w + xx as usize;
                    if pattern[j] != chan {
                        continue;
                    }
                    let dg = g[j] - gi;
                    let sigma = 0.02 + 0.1 * gi.abs();
                    let wt = (-(dg * dg) / (sigma * sigma)).exp() / (1.0 + (dy * dy + dx * dx) as f32);
                    s += diff[j] * wt;
                    ws += wt;
                }
            }
            if ws > 1e-12 { s / ws } else { 0.0 }
        }).collect()
    };
    let dr = spread(&g0, 0);
    let db = spread(&g0, 2);

    // Refine green at red and blue sites: its own sample minus the colour difference its neighbours suggest,
    // averaged with the directional estimate (less zipper on fine colour edges).
    let g1: Vec<f32> = (0..h * w).into_par_iter().map(|i| {
        match pattern[i] {
            0 => 0.5 * g0[i] + 0.5 * (cfa[i] - neighbours_mean(&dr, pattern, w, h, i, 0)),
            2 => 0.5 * g0[i] + 0.5 * (cfa[i] - neighbours_mean(&db, pattern, w, h, i, 2)),
            _ => cfa[i],
        }
    }).collect();
    let dr = spread(&g1, 0);
    let db = spread(&g1, 2);
    for i in 0..w * h {
        rgb[1][i] = g1[i];
        rgb[0][i] = if pattern[i] == 0 { cfa[i] } else { g1[i] + dr[i] };
        rgb[2][i] = if pattern[i] == 2 { cfa[i] } else { g1[i] + db[i] };
    }
    rgb
}

/// Mean of `v` over same-colour sites around `i` (excluding `i`), within 2 px.
fn neighbours_mean(v: &[f32], pattern: &[u8], w: usize, h: usize, i: usize, chan: u8) -> f32 {
    let (y, x) = ((i / w) as isize, (i % w) as isize);
    let (mut s, mut n) = (0f32, 0f32);
    for dy in -2isize..=2 {
        for dx in -2isize..=2 {
            let (yy, xx) = (y + dy, x + dx);
            if (dy == 0 && dx == 0) || yy < 0 || xx < 0 || yy >= h as isize || xx >= w as isize {
                continue;
            }
            let j = yy as usize * w + xx as usize;
            if pattern[j] == chan {
                let wt = 1.0 / (dy * dy + dx * dx) as f32;
                s += v[j] * wt;
                n += wt;
            }
        }
    }
    if n > 0.0 { s / n } else { v[i] }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XTRANS: [&str; 6] = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];

    fn mosaic(img: &dyn Fn(usize, usize) -> [f32; 3], w: usize, h: usize, colour: &dyn Fn(usize, usize) -> u8) -> (Vec<f32>, Vec<u8>) {
        let pattern: Vec<u8> = (0..w * h).map(|i| colour(i / w, i % w)).collect();
        let cfa = (0..w * h).map(|i| img(i / w, i % w)[pattern[i] as usize]).collect();
        (cfa, pattern)
    }

    fn bayer(y: usize, x: usize) -> u8 {
        // RGGB
        match (y % 2, x % 2) { (0, 0) => 0, (1, 1) => 2, _ => 1 }
    }

    fn xtrans_at(y: usize, x: usize) -> u8 {
        match XTRANS[y % 6].as_bytes()[x % 6] { b'R' => 0, b'G' => 1, _ => 2 }
    }

    /// Mean error against the truth, away from the borders.
    fn error(planes: &[Vec<f32>; 3], truth: &dyn Fn(usize, usize) -> [f32; 3], w: usize, h: usize) -> f32 {
        let (mut e, mut n) = (0f32, 0f32);
        for y in 6..h - 6 {
            for x in 6..w - 6 {
                let t = truth(y, x);
                for c in 0..3 {
                    e += (planes[c][y * w + x] - t[c]).abs();
                    n += 1.0;
                }
            }
        }
        e / n
    }

    /// Fine diagonal stripes over a slow colour change: detail that bilinear blurs and zippers. Channels move
    /// together, as they do in real photos (light and shade change all three).
    fn scene(y: usize, x: usize) -> [f32; 3] {
        let t = ((x as f32 * 0.9 + y as f32 * 0.35).sin() * 0.5 + 0.5) * 0.6 + 0.2;
        let hue = 0.5 + 0.4 * (x as f32 * 0.03).sin() * (y as f32 * 0.04).cos();
        [t * (0.6 + 0.6 * hue), t, t * (1.2 - 0.6 * hue)]
    }

    #[test]
    fn flat_colour_comes_back_exactly() {
        let (w, h) = (40, 32);
        let flat = |_: usize, _: usize| [0.4, 0.5, 0.2];
        let (cfa, pat) = mosaic(&flat, w, h, &bayer);
        assert!(error(&rcd(&cfa, &pat, w, h), &flat, w, h) < 1e-3);
        let (cfa, pat) = mosaic(&flat, w, h, &xtrans_at);
        assert!(error(&xtrans(&cfa, &pat, w, h), &flat, w, h) < 1e-3);
    }

    #[test]
    fn rcd_beats_bilinear_on_detail() {
        let (w, h) = (96, 80);
        let (cfa, pat) = mosaic(&scene, w, h, &bayer);
        let b = error(&bilinear(&cfa, &pat, w, h, 1), &scene, w, h);
        let r = error(&rcd(&cfa, &pat, w, h), &scene, w, h);
        assert!(r < b * 0.4, "rcd {r} bilinear {b}");
    }

    #[test]
    fn xtrans_beats_bilinear_on_detail() {
        let (w, h) = (96, 84);
        let (cfa, pat) = mosaic(&scene, w, h, &xtrans_at);
        let b = error(&bilinear(&cfa, &pat, w, h, 2), &scene, w, h);
        let x = error(&xtrans(&cfa, &pat, w, h), &scene, w, h);
        assert!(x < b * 0.6, "xtrans {x} bilinear {b}");
    }
}

#[cfg(test)]
mod real {
    /// `RAW_FILE=x.NEF CMP_DIR=dir cargo test --release compare_with_rawler -- --ignored --nocapture`: timing
    /// and 400 px crops from the centre, ours and rawler's, for looking at.
    #[test]
    #[ignore]
    fn compare_with_rawler() {
        use rawler::{decoders::RawDecodeParams, imgop::develop::{ProcessingStep, RawDevelop}, rawsource::RawSource};
        let path = std::env::var("RAW_FILE").expect("RAW_FILE");
        let out = std::path::PathBuf::from(std::env::var("CMP_DIR").unwrap_or(".".into()));
        let src = RawSource::new(std::path::Path::new(&path)).unwrap();
        let raw = rawler::get_decoder(&src).unwrap().raw_image(&src, &RawDecodeParams::default(), false).unwrap();
        let t = std::time::Instant::now();
        let ours = super::develop(&raw).expect("handled");
        let t_ours = t.elapsed();
        let t = std::time::Instant::now();
        let steps: Vec<_> = RawDevelop::default().steps.into_iter().filter(|s| *s != ProcessingStep::SRgb).collect();
        let theirs = RawDevelop::new_with(&steps).develop_intermediate(&raw).unwrap().to_dynamic_image().unwrap().to_rgb32f();
        let t_theirs = t.elapsed();
        eprintln!("ours {:?} {}x{}  rawler {:?} {}x{}", t_ours, ours.width(), ours.height(), t_theirs, theirs.width(), theirs.height());
        let neg = ours.iter().filter(|v| **v < 0.0).count();
        eprintln!("negative channel values kept: {neg}");
        for (name, img) in [("ours", &ours), ("rawler", &theirs)] {
            let (w, h) = img.dimensions();
            let c = image::imageops::crop_imm(img, w / 2 - 200, h / 2 - 200, 400, 400).to_image();
            let enc: Vec<u8> = c.iter().map(|v| (crate::load::linear_to_srgb(*v * 2.0).max(0.0) * 255.0) as u8).collect();
            image::RgbImage::from_raw(400, 400, enc).unwrap().save(out.join(format!("{name}.png"))).unwrap();
        }
    }
}

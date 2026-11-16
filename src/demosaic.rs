//! RAW development with our own demosaicing and colour, in place of rawler's PPG (Bayer) and bilinear (X-Trans).
//!
//! - Bayer: RCD (Ratio Corrected Demosaicing, Luis Sanz Rodríguez), as in RawTherapee and darktable. Sharper
//!   than PPG with fewer zippers and colour moiré.
//! - Fuji X-Trans: green from direction-weighted neighbours, then red and blue as colour differences spread
//!   with weights that follow the green detail (edge-aware), and a final green refinement. Much finer than the
//!   bilinear rawler uses.
//! - Colour: the camera matrix is applied without clipping negative channels, so colours outside sRGB survive
//!   for wide-gamut exports (see `colour`).
//! - Highlights: clipped photosites are rebuilt before demosaicing from the colours around them ("inpaint
//!   opposed", darktable's default method), and values above white are kept, so lowering Exposure or Highlights
//!   brings back detail. The blend that turns clipped highlights white happens at the display end (`develop`).
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
    let mut data = match std::mem::replace(&mut r.data, rawler::rawimage::RawImageData::Float(vec![])) {
        rawler::rawimage::RawImageData::Float(v) => v,
        other => other.as_f32().into_owned(),
    };
    let (w, h) = (r.width, r.height);
    let cfa = &config.cfa;
    // The pattern repeats every cfa.width × cfa.height pixels: build one tile and repeat it row by row.
    let (tw, th) = (cfa.width.max(1), cfa.height.max(1));
    let tile: Vec<u8> = (0..th * tw).map(|k| cfa.color_at(k / tw, k % tw) as u8).collect();
    let mut pattern = vec![0u8; w * h];
    pattern.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let t = &tile[(y % th) * tw..][..tw];
        row.iter_mut().enumerate().for_each(|(x, v)| *v = t[x % tw]);
    });
    let wb = if raw.wb_coeffs[0].is_nan() || raw.wb_coeffs[1] <= 0.0 { [1.0; 3] } else { [0, 1, 2].map(|c| raw.wb_coeffs[c] / raw.wb_coeffs[1]) };
    reconstruct_highlights(&mut data, &pattern, w, h, wb);
    let planes = match sensor {
        Sensor::Bayer => rcd(&data, &pattern, w, h),
        Sensor::XTrans => xtrans(&data, &pattern, w, h),
    };
    // Active area (the sensor's usable part), then the camera's default crop inside it: converted straight into
    // the cropped picture, so the full frame is never copied once more.
    let crop = raw.crop_area.or(raw.active_area).unwrap_or(rawler::imgop::Rect::new(rawler::imgop::Point::new(0, 0), rawler::imgop::Dim2::new(w, h)));
    let (cx, cy) = (crop.p.x.min(w.saturating_sub(1)), crop.p.y.min(h.saturating_sub(1)));
    let (cw, ch) = (crop.d.w.min(w - cx).max(1), crop.d.h.min(h - cy).max(1));
    let rgb = to_srgb(raw, &planes, w, (cx, cy, cw, ch))?;
    Rgb32FImage::from_raw(cw as u32, ch as u32, rgb)
}

/// White balance (the camera's as-shot), then camera RGB → linear sRGB with the camera's D65 matrix, as rawler
/// does, but keeping negative channels (colours sRGB can't show).
fn to_srgb(raw: &RawImage, planes: &[Vec<f32>; 3], w: usize, (cx, cy, cw, ch): (usize, usize, usize, usize)) -> Option<Vec<f32>> {
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
    let mut out = vec![0f32; cw * ch * 3];
    out.par_chunks_mut(cw * 3).enumerate().for_each(|(y, row)| for (x, px) in row.chunks_exact_mut(3).enumerate() {
        let i = (cy + y) * w + cx + x;
        let c = [planes[0][i] * wb[0], planes[1][i] * wb[1], planes[2][i] * wb[2]];
        let mut s = [0, 1, 2].map(|k| cam2rgb[k][0] * c[0] + cam2rgb[k][1] * c[1] + cam2rgb[k][2] * c[2]);
        // Values past white are kept (highlight headroom): `develop` blends them to white at the display end.
        if s[0] + s[1] + s[2] < 0.0 {
            s = [0.0; 3];
        }
        px.copy_from_slice(&s);
    });
    Some(out)
}

/// A photosite at or above this (after black and white level scaling) has clipped.
const CLIP: f32 = 0.987;

/// Rebuild clipped photosites in place ("inpaint opposed", darktable's default highlight reconstruction).
///
/// A clipped photosite only says "at least this bright". Its likely value comes from the other two colours
/// around it (white balanced, averaged as cube roots so one bright neighbour doesn't dominate), times a colour
/// correction per channel measured on the unclipped photosites at the edge of the clipped areas. darktable adds
/// that correction; here it is a ratio, so it scales with brightness across a sky that brightens into the clip. This runs on
/// the raw data, before demosaicing and before any colour profile, as darktable does: there's no "white" yet at
/// this stage, so nothing is pushed towards white here. Returns how many photosites were rebuilt.
///
/// It can guess wrong where a clipped area touches something of a different colour (a lamp against a red wall):
/// the edge colour is averaged over the whole photo.
pub fn reconstruct_highlights(data: &mut [f32], pattern: &[u8], w: usize, h: usize, wb: [f32; 3]) -> usize {
    if w < 3 || h < 3 || data.len() != w * h || pattern.len() != w * h {
        return 0;
    }
    let clipped: Vec<bool> = data.par_iter().map(|&v| v >= CLIP).collect();
    if !clipped.par_iter().any(|&c| c) {
        return 0;
    }
    let wb = wb.map(|v| if v.is_finite() && v > 0.0 { v } else { 1.0 });
    let val = |d: &[f32], i: usize| d[i] * wb[pattern[i] as usize];
    // The other two colours around photosite i, white balanced: the cube of the mean of the cube roots.
    let opposed = |d: &[f32], i: usize| -> f32 {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        let me = pattern[i] as usize;
        let (mut acc, mut n) = ([0f32; 3], [0u32; 3]);
        for dy in -1isize..=1 {
            for dx in -1isize..=1 {
                let (xx, yy) = ((x + dx).clamp(0, w as isize - 1) as usize, (y + dy).clamp(0, h as isize - 1) as usize);
                let j = yy * w + xx;
                let k = pattern[j] as usize;
                if k != me && k < 3 {
                    acc[k] += val(d, j).max(0.0).cbrt();
                    n[k] += 1;
                }
            }
        }
        let (mut sum, mut m) = (0f32, 0f32);
        for k in 0..3 {
            if k != me && n[k] > 0 {
                sum += acc[k] / n[k] as f32;
                m += 1.0;
            }
        }
        if m > 0.0 { (sum / m).powi(3) } else { val(d, i) }
    };
    // The edge of the clipped areas: unclipped photosites within 3 of a clipped one (separable dilation).
    const R: usize = 3;
    let mut rows = vec![false; w * h];
    rows.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &clipped[y * w..][..w];
        for x in 0..w {
            row[x] = src[x.saturating_sub(R)..(x + R + 1).min(w)].iter().any(|&c| c);
        }
    });
    let near: Vec<bool> = (0..w * h).into_par_iter().map(|i| {
        let (x, y) = (i % w, i / w);
        (y.saturating_sub(R)..(y + R + 1).min(h)).any(|yy| rows[yy * w + x])
    }).collect();
    drop(rows);
    // Colour correction per channel: how each colour compares with its opposed colours along that edge.
    let (sum, cnt) = (0..w * h).into_par_iter().filter(|&i| near[i] && !clipped[i] && data[i] > 0.2).fold(|| ([0f64; 3], [0u64; 3]), |(mut s, mut n), i| {
        let k = pattern[i] as usize;
        let o = opposed(data, i);
        if k < 3 && o > 0.02 {
            s[k] += (val(data, i) / o).ln() as f64;
            n[k] += 1;
        }
        (s, n)
    }).reduce(|| ([0f64; 3], [0u64; 3]), |a, b| ([0, 1, 2].map(|k| a.0[k] + b.0[k]), [0, 1, 2].map(|k| a.1[k] + b.1[k])));
    // Geometric mean of the ratios, kept to a sane range (one odd edge can't paint a highlight bright green).
    let chroma = [0, 1, 2].map(|k| if cnt[k] > 0 { ((sum[k] / cnt[k] as f64).exp() as f32).clamp(0.25, 4.0) } else { 1.0 });
    // Where every photosite around is clipped too (the sun, a lamp) there is no colour to rebuild from: the
    // channel with the most white-balance gain would end up highest and tint it. Those stay neutral, at the
    // highest white-balanced clip level.
    let white = wb[0].max(wb[1]).max(wb[2]);
    let all_clipped = |i: usize| {
        let (x, y) = ((i % w) as isize, (i / w) as isize);
        (-1isize..=1).all(|dy| (-1isize..=1).all(|dx| {
            let (xx, yy) = ((x + dx).clamp(0, w as isize - 1) as usize, (y + dy).clamp(0, h as isize - 1) as usize);
            clipped[yy * w + xx]
        }))
    };
    let fixed: Vec<(usize, f32)> = (0..w * h).into_par_iter().filter(|&i| clipped[i]).map(|i| {
        let k = (pattern[i] as usize).min(2);
        let v = if all_clipped(i) { white.max(val(data, i)) } else { val(data, i).max(opposed(data, i) * chroma[k]) };
        (i, v / wb[k])
    }).collect();
    for &(i, v) in &fixed {
        data[i] = v;
    }
    fixed.len()
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
    let [rp, gp, bp] = &mut out;
    rp.par_chunks_mut(w).zip(gp.par_chunks_mut(w)).zip(bp.par_chunks_mut(w)).enumerate().for_each(|(y, ((rr, gg), bb))| {
        let rows = [rr, gg, bb];
        for x in 0..w {
            let i = y * w + x;
            if let Some(e) = edge
                && y >= e && x >= e && y + e < h && x + e < w
            {
                rows[pattern[i] as usize][x] = cfa[i];
                continue;
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
            for c in 0..3 {
                rows[c][x] = if c == pattern[i] as usize { cfa[i] } else if cnt[c] > 0.0 { sum[c] / cnt[c] } else { 0.0 };
            }
        }
    });
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
            if green(i) {
                continue; // only read at red and blue sites (and their diagonal neighbours, also red or blue)
            }
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
    // other colour's own raw samples on the diagonals, so it writes in place.
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
                // Every place read below is a site of colour `c` itself, so its value is the raw sample.
                let rc = cfa;
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
    drop(pq);

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

    /// `BENCH_FILE=x.NEF cargo test --release bench_raw_develop -- --ignored --nocapture`: decode time and how
    /// much of the photo sits past white (rebuilt highlights).
    #[test]
    #[ignore]
    fn bench_raw_develop() {
        let Some(f) = std::env::var_os("BENCH_FILE") else { return };
        let t = std::time::Instant::now();
        let img = crate::load::load(std::path::Path::new(&f)).unwrap();
        let over = img.chunks(3).filter(|c| c[0].max(c[1]).max(c[2]) > 1.0).count();
        let max = img.iter().cloned().fold(0.0, f32::max);
        eprintln!("load {:.0} ms, {:.3}% past white, max {max:.2}", t.elapsed().as_secs_f64() * 1e3, over as f64 / (img.len() / 3) as f64 * 100.0);
    }

    #[test]
    fn rebuilds_clipped_green_from_red_and_blue() {
        // A bright, slightly warm sky: true green 1.4 clips at 1.0; red and blue stay below white.
        let (w, h) = (40, 40);
        let truth = |y: usize, x: usize| if (12..28).contains(&x) && (12..28).contains(&y) { [0.9, 1.4, 0.7] } else { [0.45, 0.7, 0.35] };
        let (mut cfa, pat) = mosaic(&truth, w, h, &bayer);
        cfa.iter_mut().for_each(|v| *v = v.min(1.0));
        let n = reconstruct_highlights(&mut cfa, &pat, w, h, [1.0; 3]);
        assert!(n > 0);
        let i = 20 * w + 21; // a green photosite in the clipped area
        assert_eq!(pat[i], 1);
        assert!(cfa[i] > 1.2 && cfa[i] < 1.6, "green rebuilt near its true 1.4, got {}", cfa[i]);
        // Nothing unclipped changes.
        assert!((cfa[2 * w + 2] - truth(2, 2)[pat[2 * w + 2] as usize]).abs() < 1e-6);
        // Nothing to do on an unclipped photo.
        let (mut low, _) = mosaic(&|_, _| [0.4, 0.5, 0.3], w, h, &bayer);
        assert_eq!(reconstruct_highlights(&mut low, &pat, w, h, [1.0; 3]), 0);
    }

    #[test]
    fn fully_blown_white_stays_neutral() {
        // White light three times over the clip in the middle, grey around; the camera records scene / white balance.
        let wb = [2.0f32, 1.0, 1.5];
        let (w, h) = (40, 40);
        let (mut cfa, pat) = mosaic(&|y, x| if (12..28).contains(&x) && (12..28).contains(&y) { [3.0; 3] } else { [0.3; 3] }, w, h, &bayer);
        cfa.iter_mut().zip(&pat).for_each(|(v, &k)| *v = (*v / wb[k as usize]).min(1.0));
        reconstruct_highlights(&mut cfa, &pat, w, h, wb);
        let at = |x: usize, y: usize| cfa[y * w + x] * wb[pat[y * w + x] as usize];
        let (r, g, b) = (at(20, 20), at(21, 20), at(21, 21));
        assert!((r - g).abs() < 0.01 && (g - b).abs() < 0.01, "neutral, got {r} {g} {b}");
    }

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

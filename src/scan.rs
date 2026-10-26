//! Film scanning helpers: find the frame inside the holder, split a strip into frames, find dust.
//! All work on the uncropped frame (after quarter turns and straightening) and answer in frame fractions.
use crate::develop::{self, Params, Spot, luma};
use image::Rgb32FImage;

/// The photo's frame without its crop: what crops and spots are measured against.
pub fn frame(src: &Rgb32FImage, p: &Params) -> Rgb32FImage {
    develop::geometry(src, &Params { crop: [0.0; 4], ..p.clone() })
}

/// Per pixel: is this picture (not the dark holder, not bare film base)?
fn content_mask(img: &Rgb32FImage, neg: bool) -> Vec<bool> {
    let base = crate::negative::auto_base(img);
    let bl = luma(base);
    img.chunks(3).map(|c| {
        let l = luma([c[0], c[1], c[2]]);
        let holder = l < 0.3 * bl;
        // Bare film (rebate, gaps between frames) is close to the base colour; only negatives have one.
        let bare = neg && (0..3).all(|k| (c[k] / base[k] - 1.0).abs() < 0.08);
        !holder && !bare
    }).collect()
}

/// Longest run of indices where `on` holds.
fn longest_run(on: impl Iterator<Item = bool>) -> Option<(usize, usize)> {
    let (mut best, mut start) = (None::<(usize, usize)>, None);
    for (i, v) in on.chain(std::iter::once(false)).enumerate() {
        match (v, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if best.is_none_or(|(a, b)| i - s > b - a) {
                    best = Some((s, i));
                }
                start = None;
            }
            _ => {}
        }
    }
    best
}

/// Crop [left, top, right, bottom] that trims the holder and film rebate around a single frame.
pub fn auto_crop(frame: &Rgb32FImage, neg: bool) -> Option<[f32; 4]> {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let m = content_mask(frame, neg);
    let rows = longest_run((0..h).map(|y| m[y * w..(y + 1) * w].iter().filter(|v| **v).count() as f32 > 0.25 * w as f32))?;
    let cols = longest_run((0..w).map(|x| (rows.0..rows.1).filter(|y| m[y * w + x]).count() as f32 > 0.25 * (rows.1 - rows.0) as f32))?;
    // A little inset so no sliver of edge survives.
    let (ix, iy) = (0.006 * w as f32, 0.006 * h as f32);
    let c = [
        (cols.0 as f32 + ix) / w as f32,
        (rows.0 as f32 + iy) / h as f32,
        (w as f32 - cols.1 as f32 + ix) / w as f32,
        (h as f32 - rows.1 as f32 + iy) / h as f32,
    ];
    (c[0] + c[2] < 0.9 && c[1] + c[3] < 0.9).then_some(c.map(|v| v.max(0.0)))
}

/// Crops for each frame of a strip scan, in order along its long side. Gaps are runs of bare film or holder.
pub fn split_strip(frame: &Rgb32FImage, neg: bool) -> Vec<[f32; 4]> {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let m = content_mask(frame, neg);
    let horizontal = w >= h;
    let (long, short) = if horizontal { (w, h) } else { (h, w) };
    let at = |i: usize, j: usize| if horizontal { m[j * w + i] } else { m[i * w + j] };
    let filled: Vec<bool> = (0..long).map(|i| (0..short).filter(|j| at(i, *j)).count() as f32 > 0.2 * short as f32).collect();
    let mut frames = vec![];
    let mut i = 0;
    while i < long {
        if !filled[i] {
            i += 1;
            continue;
        }
        let s = i;
        while i < long && filled[i] {
            i += 1;
        }
        // Anything much narrower than a frame is a light leak or the strip's edge, not a frame.
        if (i - s) as f32 > 0.4 * short as f32 {
            frames.push((s, i));
        }
    }
    frames.iter().map(|&(a, b)| {
        // The frame's extent across the strip.
        let across = longest_run((0..short).map(|j| (a..b).filter(|i| at(*i, j)).count() as f32 > 0.25 * (b - a) as f32)).unwrap_or((0, short));
        let inset = 0.006 * long as f32;
        let (l0, l1) = ((a as f32 + inset) / long as f32, (long as f32 - b as f32 + inset) / long as f32);
        let (s0, s1) = ((across.0 as f32 + inset) / short as f32, (short as f32 - across.1 as f32 + inset) / short as f32);
        if horizontal { [l0, s0, l1, s1] } else { [s0, l0, s1, l1] }
    }).collect()
}

/// Dust and specks: small blobs much darker than their surroundings on the scan (dust blocks light, so it is dark
/// on a negative scan and on a sensor). Only looks inside `crop`. Returns spots ready to heal, biggest contrast first.
pub fn find_dust(frame: &Rgb32FImage, crop: [f32; 4], max: usize) -> Vec<Spot> {
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let l: Vec<f32> = frame.chunks(3).map(|c| luma([c[0], c[1], c[2]]).max(0.0).sqrt()).collect();
    let bg = develop::blur(&l, w, h, 4.0);
    // Typical local variation, so busy texture isn't mistaken for dust.
    let dev: Vec<f32> = l.iter().zip(&bg).map(|(a, b)| (a - b).abs()).collect();
    let noise = develop::blur(&dev, w, h, 8.0);
    let (x0, y0) = ((crop[0] * w as f32) as usize, (crop[1] * h as f32) as usize);
    let (x1, y1) = (w - (crop[2] * w as f32) as usize, h - (crop[3] * h as f32) as usize);
    let dark = |i: usize| bg[i] - l[i] > (4.0 * noise[i]).max(0.04);
    let mut seen = vec![false; w * h];
    let mut found = vec![];
    for y in y0.max(1)..y1.min(h - 1) {
        for x in x0.max(1)..x1.min(w - 1) {
            let i = y * w + x;
            if seen[i] || !dark(i) {
                continue;
            }
            // Flood the blob.
            let (mut stack, mut px) = (vec![i], vec![]);
            seen[i] = true;
            while let Some(j) = stack.pop() {
                px.push(j);
                if px.len() > 400 {
                    break;
                }
                let (jx, jy) = (j % w, j / w);
                for (nx, ny) in [(jx.wrapping_sub(1), jy), (jx + 1, jy), (jx, jy.wrapping_sub(1)), (jx, jy + 1)] {
                    if nx < w && ny < h && !seen[ny * w + nx] && dark(ny * w + nx) {
                        seen[ny * w + nx] = true;
                        stack.push(ny * w + nx);
                    }
                }
            }
            let n = px.len();
            if !(2..=400).contains(&n) {
                continue;
            }
            let (sx, sy) = px.iter().fold((0.0, 0.0), |a, j| (a.0 + (j % w) as f32, a.1 + (j / w) as f32));
            let (cx, cy) = (sx / n as f32, sy / n as f32);
            // Round-ish only: long thin lines are usually edges in the picture, not dust.
            let spread = px.iter().map(|j| ((j % w) as f32 - cx).powi(2) + ((j / w) as f32 - cy).powi(2)).fold(0.0f32, f32::max).sqrt();
            if spread > 2.5 * (n as f32 / std::f32::consts::PI).sqrt() + 1.5 {
                continue;
            }
            let contrast: f32 = px.iter().map(|j| bg[*j] - l[*j]).sum::<f32>() / n as f32;
            found.push((contrast, cx, cy, spread));
        }
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    let long = w.max(h) as f32;
    found.into_iter().take(max).map(|(_, x, y, spread)| {
        let r = spread * 1.4 + 2.5;
        let (sx, sy) = develop::find_source(frame, x, y, r);
        Spot { x: x / w as f32, y: y / h as f32, r: r / long, sx: sx / w as f32, sy: sy / h as f32, ..Default::default() }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Grey picture on an orange "film base" strip inside a black holder, frames split by base gaps.
    fn strip(frames: usize) -> Rgb32FImage {
        let (fw, gap, border) = (100u32, 12u32, 20u32);
        let w = border * 2 + frames as u32 * fw + (frames as u32 - 1) * gap;
        let h = 110;
        Rgb32FImage::from_fn(w, h, |x, y| {
            let base = image::Rgb([0.8, 0.5, 0.3]);
            if x < border || x >= w - border || !(10..100).contains(&y) {
                return image::Rgb([0.01, 0.01, 0.01]); // holder
            }
            let i = x - border;
            if i % (fw + gap) >= fw || !(20..90).contains(&y) {
                return base; // gap or rebate
            }
            let v = 0.2 + 0.2 * ((x + y) % 7) as f32 / 7.0;
            image::Rgb([v, v * 0.8, v * 0.6])
        })
    }

    #[test]
    fn auto_crop_trims_holder_and_rebate() {
        let img = strip(1);
        let c = auto_crop(&img, true).unwrap();
        let (w, h) = (img.width() as f32, img.height() as f32);
        assert!((c[0] * w - 20.0).abs() < 3.0 && (c[1] * h - 20.0).abs() < 3.0, "{c:?}");
        assert!((w - c[2] * w - 120.0).abs() < 3.0 && (h - c[3] * h - 90.0).abs() < 3.0, "{c:?}");
    }

    #[test]
    fn strip_splits_into_its_frames() {
        let img = strip(4);
        let f = split_strip(&img, true);
        assert_eq!(f.len(), 4);
        let w = img.width() as f32;
        assert!((f[1][0] * w - (20.0 + 112.0)).abs() < 3.0, "{:?}", f[1]);
    }

    #[test]
    fn dust_specks_are_found_but_not_texture() {
        let mut img = Rgb32FImage::from_fn(300, 200, |x, _| {
            let v = 0.4 + 0.1 * (x as f32 / 30.0).sin();
            image::Rgb([v, v, v])
        });
        for (cx, cy) in [(50, 50), (200, 120), (120, 160)] {
            for y in cy - 2..=cy + 2 {
                for x in cx - 2..=cx + 2 {
                    img.put_pixel(x, y, image::Rgb([0.05, 0.05, 0.05]));
                }
            }
        }
        let d = find_dust(&img, [0.0; 4], 50);
        assert_eq!(d.len(), 3, "{d:?}");
        assert!(d.iter().any(|s| (s.x * 300.0 - 50.0).abs() < 1.5 && (s.y * 200.0 - 50.0).abs() < 1.5));
    }
}

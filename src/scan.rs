//! Film scanning helpers: find the frame inside the holder, split a strip into frames, find dust.
//! All work on the uncropped frame (after quarter turns and straightening) and answer in frame fractions.
use crate::develop::{self, Params, Spot, luma};
use image::Rgb32FImage;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

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

// ---------- infrared dust and scratch removal ----------

/// The infrared channel of a scan: film dyes let infrared through, dust, hair and scratches don't, so it shows
/// the defects alone. Values 0..1, `w`×`h`, in the scan's own orientation.
pub struct Ir {
    pub w: u32,
    pub h: u32,
    pub data: Vec<f32>,
}

/// The infrared channel of an RGBI TIFF (VueScan, SilverFast: four samples per pixel, 8 or 16 bits), if the scan
/// has one. Cached per file (and its modification time), so moving the slider doesn't decode the scan again.
pub fn ir_channel(path: &Path) -> Option<Arc<Ir>> {
    let real = crate::load::copy_of(path).unwrap_or_else(|| path.to_path_buf());
    let ext = real.extension()?.to_str()?.to_ascii_lowercase();
    if ext != "tif" && ext != "tiff" {
        return None;
    }
    let mtime = std::fs::metadata(&real).and_then(|m| m.modified()).ok();
    type Cache = Mutex<Option<(PathBuf, Option<std::time::SystemTime>, Option<Arc<Ir>>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((p, t, ir)) = &*cache.lock().ok()?
        && *p == real
        && *t == mtime
    {
        return ir.clone();
    }
    let ir = read_ir(&real).map(Arc::new);
    if let Ok(mut c) = cache.lock() {
        *c = Some((real, mtime, ir.clone()));
    }
    ir
}

fn read_ir(path: &Path) -> Option<Ir> {
    use tiff::decoder::{Decoder, DecodingResult};
    let f = std::fs::File::open(path).ok()?;
    let mut dec = Decoder::new(std::io::BufReader::new(f)).ok()?;
    let (w, h) = dec.dimensions().ok()?;
    let samples = match dec.colortype().ok()? {
        tiff::ColorType::RGBA(_) => 4,
        tiff::ColorType::Multiband { num_samples, .. } if num_samples >= 4 => num_samples as usize,
        _ => return None,
    };
    let data: Vec<f32> = match dec.read_image().ok()? {
        DecodingResult::U8(v) => v.chunks_exact(samples).map(|c| c[3] as f32 / 255.0).collect(),
        DecodingResult::U16(v) => v.chunks_exact(samples).map(|c| c[3] as f32 / 65535.0).collect(),
        DecodingResult::F32(v) => v.chunks_exact(samples).map(|c| c[3]).collect(),
        _ => return None,
    };
    if data.len() != (w * h) as usize {
        return None;
    }
    // A real alpha channel (a PNG-style cut-out saved as TIFF) is mostly fully opaque: not infrared.
    let opaque = data.iter().step_by(97).filter(|&&v| v >= 0.999).count() as f32 / (data.len() / 97).max(1) as f32;
    if opaque > 0.9 {
        return None;
    }
    Some(Ir { w, h, data })
}

/// How sure we are that each pixel is a defect, 0..1, from the infrared channel. `strength` 0..100 sets how
/// faint a defect still counts. The cyan dye leaks a little into infrared on some scanners, so the part of the
/// infrared that follows the red channel is taken out first.
pub fn ir_defects(ir: &Ir, red: Option<&[f32]>, strength: f32) -> Vec<f32> {
    let (w, h) = (ir.w as usize, ir.h as usize);
    let mut v = ir.data.clone();
    if let Some(red) = red.filter(|r| r.len() == v.len()) {
        // Least-squares slope of infrared on red over the clear film, removed around the mean.
        let n = v.len() as f64;
        let (mr, mi) = (red.iter().map(|&x| x as f64).sum::<f64>() / n, v.iter().map(|&x| x as f64).sum::<f64>() / n);
        let (mut sxy, mut sxx) = (0f64, 0f64);
        for (r, i) in red.iter().zip(&v).step_by(7) {
            sxy += (*r as f64 - mr) * (*i as f64 - mi);
            sxx += (*r as f64 - mr).powi(2);
        }
        let k = if sxx > 0.0 { (sxy / sxx).clamp(0.0, 1.0) as f32 } else { 0.0 };
        for (i, r) in v.iter_mut().zip(red) {
            *i -= k * (r - mr as f32);
        }
    }
    // Clear film around a defect: a wide blur of the infrared, with the defect itself mostly left out by
    // taking the brighter of the local level and a level a little wider.
    let near = develop::blur(&v, w, h, 6.0);
    let far = develop::blur(&v, w, h, 18.0);
    let t = 0.35 + (0.05 - 0.35) * (strength / 100.0).clamp(0.0, 1.0); // how deep a dip counts as a defect
    let raw: Vec<f32> = v.iter().zip(near.iter().zip(&far)).map(|(&x, (&a, &b))| {
        let r = a.max(b).max(1e-4);
        let dip = 1.0 - x / r;
        develop::smooth(t * 0.5, t, dip)
    }).collect();
    // Grow by two pixels: the soft edge of a speck is darker in the picture before it shows in infrared.
    let mut grown = raw.clone();
    for y in 0..h {
        for x in 0..w {
            let mut m = raw[y * w + x];
            if m >= 1.0 {
                continue;
            }
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                    if xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h && dx * dx + dy * dy <= 5 {
                        m = m.max(raw[yy as usize * w + xx as usize] * (1.0 - 0.15 * (dx * dx + dy * dy) as f32 / 5.0));
                    }
                }
            }
            grown[y * w + x] = m;
        }
    }
    grown
}

/// Fill the defects in `img` from the clean film around them: normalised blurs of the clean pixels at growing
/// sizes, each pixel taking the finest one that has enough clean film nearby.
pub fn ir_clean(img: &Rgb32FImage, ir: &Ir, strength: f32) -> Rgb32FImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    // A resized source: sample the infrared at this size.
    let resized;
    let ir = if (ir.w as usize, ir.h as usize) == (w, h) {
        ir
    } else {
        resized = Ir { w: w as u32, h: h as u32, data: (0..w * h).map(|i| {
            let (x, y) = ((i % w) as f32 * ir.w as f32 / w as f32, (i / w) as f32 * ir.h as f32 / h as f32);
            ir.data[(y as usize).min(ir.h as usize - 1) * ir.w as usize + (x as usize).min(ir.w as usize - 1)]
        }).collect() };
        &resized
    };
    let red: Vec<f32> = img.chunks(3).map(|c| c[0]).collect();
    fill(img, &ir_defects(ir, Some(&red), strength))
}

/// Replace `img` where `mask` is 1 (blending where it's in between) with colour from the unmasked surroundings.
pub fn fill(img: &Rgb32FImage, mask: &[f32]) -> Rgb32FImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if mask.iter().all(|&m| m <= 0.0) {
        return img.clone();
    }
    let keep: Vec<f32> = mask.iter().map(|m| 1.0 - m).collect();
    let mut out = img.clone();
    let mut done = vec![0f32; w * h]; // how much of each masked pixel is already filled
    for sigma in [1.5f32, 3.0, 6.0, 12.0, 24.0, 48.0] {
        let wsum = develop::blur(&keep, w, h, sigma);
        let chans: Vec<Vec<f32>> = (0..3).map(|c| {
            let v: Vec<f32> = img.chunks(3).zip(&keep).map(|(p, k)| p[c] * k).collect();
            develop::blur(&v, w, h, sigma)
        }).collect();
        let mut left = false;
        for i in 0..w * h {
            if mask[i] <= 0.0 || done[i] >= 1.0 {
                continue;
            }
            // Enough clean film within this radius: take it, for the share not yet filled.
            let take = ((wsum[i] - 0.15) / 0.35).clamp(0.0, 1.0) * (1.0 - done[i]);
            if take > 0.0 {
                let o = &mut out.as_mut()[i * 3..i * 3 + 3];
                for c in 0..3 {
                    let fillv = chans[c][i] / wsum[i].max(1e-6);
                    // Blend between what has been filled so far and this scale's fill.
                    let so_far = if done[i] > 0.0 { o[c] } else { 0.0 };
                    o[c] = so_far + fillv * take;
                }
                done[i] += take;
            }
            if done[i] < 0.999 {
                left = true;
            }
        }
        if !left {
            break;
        }
    }
    // Mix: masked share from the fill, the rest from the original.
    for i in 0..w * h {
        if mask[i] <= 0.0 {
            continue;
        }
        let m = mask[i];
        let d = done[i].max(1e-6);
        let p = img.as_raw();
        let o = &mut out.as_mut()[i * 3..i * 3 + 3];
        for c in 0..3 {
            let f = o[c] / d;
            o[c] = p[i * 3 + c] * (1.0 - m) + f * m;
        }
    }
    out
}

/// The source for rendering `path` with `p`: infrared cleaning applied when asked for and the scan has infrared.
pub fn prepare_ir(img: Rgb32FImage, path: &Path, p: &Params) -> Rgb32FImage {
    if p.ir_clean <= 0.0 {
        return img;
    }
    match ir_channel(path) {
        Some(ir) => ir_clean(&img, &ir, p.ir_clean),
        None => img,
    }
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
    fn infrared_finds_and_fills_dust() {
        // A smooth gradient picture with two dark specks; the infrared is clear film with the same specks.
        let (w, h) = (160u32, 120u32);
        let speck = |x: u32, y: u32| [(40u32, 40u32), (110, 80)].iter().any(|&(cx, cy)| (x as i32 - cx as i32).pow(2) + (y as i32 - cy as i32).pow(2) <= 9);
        let img = Rgb32FImage::from_fn(w, h, |x, y| {
            let v = 0.3 + 0.3 * x as f32 / w as f32;
            if speck(x, y) { image::Rgb([0.02, 0.02, 0.02]) } else { image::Rgb([v, v * 0.9, v * 0.8]) }
        });
        let ir = Ir { w, h, data: (0..w * h).map(|i| if speck(i % w, i / w) { 0.1 } else { 0.85 + 0.02 * ((i % 13) as f32 / 13.0) }).collect() };
        let mask = ir_defects(&ir, None, 50.0);
        assert!(mask[(40 * w + 40) as usize] > 0.9 && mask[(80 * w + 110) as usize] > 0.9);
        let n = mask.iter().filter(|&&m| m > 0.5).count();
        assert!(n < 250 && mask[(20 * w + 80) as usize] == 0.0, "only the specks: {n}");
        let out = ir_clean(&img, &ir, 50.0);
        let want = 0.3 + 0.3 * 40.0 / w as f32;
        assert!((out.get_pixel(40, 40).0[0] - want).abs() < 0.03, "{:?}", out.get_pixel(40, 40));
        assert_eq!(out.get_pixel(10, 10), img.get_pixel(10, 10), "clean film untouched");
        // Off: nothing changes.
        assert_eq!(prepare_ir(img.clone(), Path::new("/nope.tif"), &Params::default()), img);
    }

    #[test]
    fn rgbi_tiff_gives_its_infrared() {
        let dir = std::env::temp_dir().join(format!("ferro-ir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scan.tif");
        let (w, h) = (16u32, 8u32);
        let px: Vec<u16> = (0..w * h).flat_map(|i| [30000u16, 20000, 10000, if i == 5 { 5000 } else { 60000 }]).collect();
        {
            let f = std::fs::File::create(&path).unwrap();
            let mut enc = tiff::encoder::TiffEncoder::new(f).unwrap();
            enc.write_image::<tiff::encoder::colortype::RGBA16>(w, h, &px).unwrap();
        }
        let ir = ir_channel(&path).unwrap();
        assert_eq!((ir.w, ir.h), (w, h));
        assert!((ir.data[5] - 5000.0 / 65535.0).abs() < 1e-4 && ir.data[6] > 0.9);
        // A plain RGB scan has none.
        image::RgbImage::new(4, 4).save(dir.join("plain.tif")).unwrap();
        assert!(ir_channel(&dir.join("plain.tif")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
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

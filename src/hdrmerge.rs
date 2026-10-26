//! Merge an exposure bracket into one HDR photo (linear, 32-bit float), and find brackets in a folder.
use crate::lens::Shot;
use image::Rgb32FImage;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// How much a pixel value can be trusted: none when clipped, little when deep in the noise.
fn weight(v: [f32; 3]) -> f32 {
    let m = v[0].max(v[1]).max(v[2]);
    let lo = (m / 0.06).clamp(0.0, 1.0);
    let hi = 1.0 - ((m - 0.8) / 0.15).clamp(0.0, 1.0);
    lo * hi
}

fn median(mut v: Vec<f32>) -> Option<f32> {
    if v.len() < 50 {
        return None;
    }
    let k = v.len() / 2;
    v.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    Some(v[k])
}

/// Brightness ratio `a / b` from pixels well exposed in both (sampled).
fn ratio(a: &Rgb32FImage, b: &Rgb32FImage) -> Option<f32> {
    let r: Vec<f32> = a.par_chunks(3).zip(b.par_chunks(3)).step_by(7).filter_map(|(x, y)| {
        let (lx, ly) = (crate::develop::luma([x[0], x[1], x[2]]), crate::develop::luma([y[0], y[1], y[2]]));
        (weight([x[0], x[1], x[2]]) > 0.5 && weight([y[0], y[1], y[2]]) > 0.5 && ly > 1e-4).then(|| lx / ly)
    }).collect();
    median(r)
}

/// Merge `imgs` (linear, same size) into one image at the exposure of the middle one. `shots` gives each one's
/// capture settings, used for the order and as a fallback for the exposure ratios.
pub fn merge(imgs: &[Rgb32FImage], shots: &[Shot], progress: &dyn Fn(&str)) -> Result<Rgb32FImage, String> {
    if imgs.len() < 2 {
        return Err("choose at least two photos of the same scene at different exposures".into());
    }
    let (w, h) = imgs[0].dimensions();
    if imgs.iter().any(|i| i.dimensions() != (w, h)) {
        return Err("the photos are not all the same size".into());
    }
    // Order by brightness; the middle one is the reference everything is matched to.
    let mean = |i: &Rgb32FImage| i.par_chunks(3).step_by(13).map(|c| crate::develop::luma([c[0], c[1], c[2]])).sum::<f32>();
    let mut order: Vec<usize> = (0..imgs.len()).collect();
    let means: Vec<f32> = imgs.iter().map(mean).collect();
    order.sort_by(|a, b| means[*a].total_cmp(&means[*b]));
    let r = order[order.len() / 2];
    progress("Matching exposures");
    // Gain that brings each photo to the reference's exposure: from EXIF first, then measured.
    let exif_gain = |i: usize| Some(2f32.powf(shots.get(i)?.ev100()? - shots.get(r)?.ev100()?));
    let rough: Vec<f32> = (0..imgs.len()).map(|i| exif_gain(i).unwrap_or(means[r] / means[i].max(1e-6))).collect();
    progress("Aligning");
    let warped: Vec<Rgb32FImage> = (0..imgs.len()).into_par_iter().map(|i| {
        if i == r {
            return imgs[i].clone();
        }
        match crate::pano::align_pair(&imgs[r], &imgs[i], rough[i]) {
            // Only a small shift or turn is expected; anything bigger is a false match.
            Some(hm) if (hm[0][0] - 1.0).abs() < 0.05 && (hm[1][1] - 1.0).abs() < 0.05 && hm[0][2].abs() < w as f64 * 0.1 && hm[1][2].abs() < h as f64 * 0.1 => warp(&imgs[i], &hm),
            _ => imgs[i].clone(),
        }
    }).collect();
    let gain: Vec<f32> = (0..imgs.len()).map(|i| if i == r { 1.0 } else { ratio(&warped[r], &warped[i]).unwrap_or(rough[i]) }).collect();
    progress("Merging");
    let mut out = Rgb32FImage::new(w, h);
    let (darkest, brightest) = (order[0], order[order.len() - 1]);
    out.par_chunks_mut(3).enumerate().for_each(|(p, o)| {
        let px = |i: usize| { let s = &warped[i].as_raw()[p * 3..p * 3 + 3]; [s[0], s[1], s[2]] };
        let refv = px(r).map(|v| v * gain[r]);
        let wr = weight(px(r));
        let (mut acc, mut sw) = ([0.0f32; 3], 0.0f32);
        for i in 0..warped.len() {
            let v = px(i);
            let mut wi = weight(v);
            let n = v.map(|x| x * gain[i]);
            // Moving things (people, branches): when the reference is reliable here and this photo disagrees, skip it.
            if i != r && wr > 0.3 {
                let (a, b) = (crate::develop::luma(n), crate::develop::luma(refv));
                if (a - b).abs() > 0.3 * b + 0.01 {
                    wi = 0.0;
                }
            }
            // Longer exposures have less noise: trust them more.
            wi *= 1.0 / gain[i].max(1e-3);
            (0..3).for_each(|k| acc[k] += n[k] * wi);
            sw += wi;
        }
        let v = if sw > 1e-6 {
            acc.map(|a| a / sw)
        } else if crate::develop::luma(px(r)) > 0.5 {
            px(darkest).map(|x| x * gain[darkest]) // clipped everywhere: the darkest frame has the most
        } else {
            px(brightest).map(|x| x * gain[brightest])
        };
        o.copy_from_slice(&v);
    });
    Ok(out)
}

/// `img` resampled so that output pixel p shows img at H⁻¹·p (H maps img into the reference).
fn warp(img: &Rgb32FImage, hm: &[[f64; 3]; 3]) -> Rgb32FImage {
    let m = hm;
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let inv = [
        [(m[1][1] * m[2][2] - m[1][2] * m[2][1]) / det, (m[0][2] * m[2][1] - m[0][1] * m[2][2]) / det, (m[0][1] * m[1][2] - m[0][2] * m[1][1]) / det],
        [(m[1][2] * m[2][0] - m[1][0] * m[2][2]) / det, (m[0][0] * m[2][2] - m[0][2] * m[2][0]) / det, (m[0][2] * m[1][0] - m[0][0] * m[1][2]) / det],
        [(m[1][0] * m[2][1] - m[1][1] * m[2][0]) / det, (m[0][1] * m[2][0] - m[0][0] * m[2][1]) / det, (m[0][0] * m[1][1] - m[0][1] * m[1][0]) / det],
    ];
    let (w, h) = img.dimensions();
    let mut out = Rgb32FImage::new(w, h);
    out.par_chunks_mut(w as usize * 3).enumerate().for_each(|(y, row)| {
        for (x, px) in row.chunks_mut(3).enumerate() {
            let (xf, yf) = (x as f64, y as f64);
            let z = inv[2][0] * xf + inv[2][1] * yf + inv[2][2];
            let (sx, sy) = ((inv[0][0] * xf + inv[0][1] * yf + inv[0][2]) / z, (inv[1][0] * xf + inv[1][1] * yf + inv[1][2]) / z);
            let v = crate::develop::bilinear(img, (sx as f32).clamp(0.0, (w - 1) as f32), (sy as f32).clamp(0.0, (h - 1) as f32));
            px.copy_from_slice(&v);
        }
    });
    out
}

/// Exposure brackets among `files`: runs of photos taken within a few seconds of each other at different
/// exposures (by capture time and exposure settings), in shooting order.
pub fn brackets(files: &[PathBuf]) -> Vec<Vec<PathBuf>> {
    let shots: Vec<(PathBuf, Shot)> = files.par_iter().filter(|f| crate::load::copy_of(f).is_none()).filter_map(|f| Some((f.clone(), crate::lens::shot(f)?))).collect();
    group(shots)
}

fn group(shots: Vec<(PathBuf, Shot)>) -> Vec<Vec<PathBuf>> {
    let mut shots: Vec<(PathBuf, Shot)> = shots.into_iter().filter(|(_, s)| s.seconds().is_some()).collect();
    shots.sort_by_key(|(f, s)| (s.seconds(), f.clone()));
    // Without full settings, exposure compensation still tells brackets apart.
    let exposure = |s: &Shot| s.ev100().unwrap_or(-s.bias);
    let mut out: Vec<Vec<PathBuf>> = vec![];
    let mut run: Vec<(PathBuf, Shot)> = vec![];
    let flush = |run: &mut Vec<(PathBuf, Shot)>, out: &mut Vec<Vec<PathBuf>>| {
        let mut evs: Vec<f32> = run.iter().map(|(_, s)| exposure(s)).collect();
        evs.sort_by(|a, b| a.total_cmp(b));
        evs.dedup_by(|a, b| (*a - *b).abs() < 0.3);
        if run.len() >= 2 && evs.len() == run.len() {
            out.push(run.iter().map(|(f, _)| f.clone()).collect());
        }
        run.clear();
    };
    for (f, s) in shots {
        let continues = run.last().is_some_and(|(_, prev)| {
            let gap = s.seconds().unwrap_or(0) - prev.seconds().unwrap_or(0);
            gap <= 3 && prev.model == s.model && (exposure(prev) - exposure(&s)).abs() >= 0.3 && !run.iter().any(|(_, q)| (exposure(q) - exposure(&s)).abs() < 0.3)
        });
        if !continues {
            flush(&mut run, &mut out);
        }
        run.push((f, s));
    }
    flush(&mut run, &mut out);
    out
}

/// Merge the photos in `files` and save `<first>-hdr.exr` next to them (linear 32-bit float, opened like any photo).
pub fn merge_files(files: &[PathBuf], progress: &dyn Fn(&str)) -> Result<PathBuf, String> {
    progress("Loading photos");
    let imgs: Vec<Rgb32FImage> = files.par_iter().map(|f| crate::load::load(f)).collect::<Result<_, _>>()?;
    let shots: Vec<Shot> = files.iter().map(|f| crate::lens::shot(f).unwrap_or_default()).collect();
    let hdr = merge(&imgs, &shots, progress)?;
    progress("Saving");
    let dir = files[0].parent().unwrap_or(Path::new("."));
    let out = crate::claim_output(dir, &format!("{}-hdr", crate::export_stem(&files[0])), "exr")?;
    image::DynamicImage::ImageRgb32F(hdr).save(&out).map_err(|e| {
        let _ = std::fs::remove_file(&out);
        e.to_string()
    })?;
    // The lens profile and camera details of the first photo carry over, so lens corrections still work.
    if let Some(p) = crate::read_params(&files[0]).filter(|p| !p.lens.is_empty()) {
        let q = crate::develop::Params { lens: p.lens, lens_focal: p.lens_focal, lens_f: p.lens_f, lens_crop: p.lens_crop, ..Default::default() };
        let _ = crate::write_params(&out, &q);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brackets_are_grouped_by_time_and_exposure() {
        let s = |t: &str, shutter: f32| Shot { model: "X".into(), fnum: 8.0, iso: 100.0, shutter, taken: format!("2026:10:26 12:00:{t}"), ..Default::default() };
        let shots = vec![
            ("a".into(), s("00", 1.0 / 250.0)), ("b".into(), s("01", 1.0 / 60.0)), ("c".into(), s("01", 1.0 / 15.0)), // a bracket
            ("d".into(), s("20", 1.0 / 60.0)), ("e".into(), s("21", 1.0 / 60.0)), // a burst: same exposure
            ("f".into(), s("40", 1.0 / 60.0)), ("g".into(), s("41", 1.0 / 250.0)), ("h".into(), s("50", 1.0 / 15.0)), // pair, then too late
        ];
        let g = group(shots);
        let names: Vec<Vec<String>> = g.iter().map(|v| v.iter().map(|p| p.display().to_string()).collect()).collect();
        assert_eq!(names, vec![vec!["a", "b", "c"], vec!["f", "g"]]);
    }

    #[test]
    fn bracket_merge_recovers_highlights_and_shadows() {
        // A scene with a range far beyond one exposure: 0.002 to 2.6 (linear), shot at -2, 0 and +2 EV.
        let (w, h) = (240u32, 160u32);
        let scene = |x: u32, y: u32| {
            let t = x as f32 / (w - 1) as f32;
            let v = 0.002 * 1000f32.powf(t) * (1.0 + 0.3 * ((y as f32 * 0.7).sin() * (x as f32 * 0.3).cos()));
            [v, v * 0.8, v * 0.6]
        };
        let shot = |ev: f32| Rgb32FImage::from_fn(w, h, |x, y| image::Rgb(scene(x, y).map(|v| (v * 2f32.powf(ev)).min(1.0))));
        let imgs = vec![shot(-2.0), shot(0.0), shot(2.0)];
        let out = merge(&imgs, &[Shot::default(), Shot::default(), Shot::default()], &|_| {}).unwrap();
        for x in [5u32, 60, 120, 200, 235] {
            let (got, want) = (out.get_pixel(x, 80)[0], scene(x, 80)[0]);
            assert!((got / want - 1.0).abs() < 0.08, "x {x}: {got} vs {want}");
        }
    }
}

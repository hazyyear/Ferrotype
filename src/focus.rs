//! Focus stacking: merge photos of one scene focused at different distances into one photo that is sharp
//! throughout. Each frame is lined up with the middle one (focus breathing changes the scale a little), then
//! every pixel is taken mostly from the frames that are sharpest there.
//!
//! Frames are loaded one at a time and added to a running sum, so memory stays at about four full-size images
//! however many frames there are.
use image::Rgb32FImage;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// Local sharpness of a linear image: the size of the Laplacian of perceptual brightness, spread out a little so
/// the choice between frames changes smoothly instead of pixel by pixel.
fn sharpness(img: &Rgb32FImage) -> Vec<f32> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let l: Vec<f32> = img.as_raw().par_chunks(3).map(|c| crate::develop::luma([c[0], c[1], c[2]]).max(0.0).sqrt()).collect();
    let mut lap = vec![0.0f32; w * h];
    lap.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (ya, yb) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for (x, o) in row.iter_mut().enumerate() {
            let (xa, xb) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let c = l[y * w + x];
            *o = (4.0 * c - l[y * w + xa] - l[y * w + xb] - l[ya * w + x] - l[yb * w + x]).abs();
        }
    });
    let sigma = (w.max(h) as f32 / 500.0).max(2.0);
    crate::develop::blur(&lap, w, h, sigma)
}

/// How strongly the sharpest frame wins. Higher is crisper but shows more seams where frames disagree.
const POWER: f32 = 4.0;

/// Accumulates frames: the weighted colour sum and the weight sum.
struct Stack {
    acc: Vec<f32>,
    sw: Vec<f32>,
}

impl Stack {
    fn new(w: u32, h: u32) -> Self {
        Stack { acc: vec![0.0; (w * h * 3) as usize], sw: vec![0.0; (w * h) as usize] }
    }

    fn add(&mut self, img: &Rgb32FImage) {
        let s = sharpness(img);
        // Normalise so the power doesn't underflow on dark or soft frames.
        let top = s.par_iter().cloned().reduce(|| 0.0, f32::max).max(1e-9);
        self.acc.par_chunks_mut(3).zip(self.sw.par_iter_mut()).zip(img.as_raw().par_chunks(3)).zip(s.par_iter()).for_each(|(((a, sw), px), s)| {
            let wgt = (s / top + 1e-4).powf(POWER);
            (0..3).for_each(|k| a[k] += px[k] * wgt);
            *sw += wgt;
        });
    }

    fn finish(self, w: u32, h: u32) -> Rgb32FImage {
        let mut out = self.acc;
        out.par_chunks_mut(3).zip(self.sw.par_iter()).for_each(|(px, sw)| px.iter_mut().for_each(|v| *v /= sw.max(1e-12)));
        Rgb32FImage::from_raw(w, h, out).expect("buffer size matches")
    }
}

/// The frame lined up with `reference`, if it needs moving. Focus breathing scales the frame by a few per cent;
/// anything far beyond that is a false match and the frame is used as shot.
fn aligned(reference: &Rgb32FImage, img: Rgb32FImage) -> Rgb32FImage {
    let (w, h) = img.dimensions();
    match crate::pano::align_pair(reference, &img, 1.0) {
        Some(hm) if (hm[0][0] - 1.0).abs() < 0.15 && (hm[1][1] - 1.0).abs() < 0.15 && hm[0][1].abs() < 0.05 && hm[0][2].abs() < w as f64 * 0.1 && hm[1][2].abs() < h as f64 * 0.1 => crate::hdrmerge::warp(&img, &hm),
        _ => img,
    }
}

/// Stack frames already in memory (all the same size). The file version below streams frames instead.
#[cfg(test)]
pub fn stack(imgs: Vec<Rgb32FImage>, progress: &dyn Fn(&str)) -> Result<Rgb32FImage, String> {
    let n = imgs.len();
    if n < 2 {
        return Err("choose at least two photos of the same scene focused at different distances".into());
    }
    let (w, h) = imgs[0].dimensions();
    if imgs.iter().any(|i| i.dimensions() != (w, h)) {
        return Err("the photos are not all the same size".into());
    }
    let r = n / 2;
    let reference = imgs[r].clone();
    let mut st = Stack::new(w, h);
    for (i, img) in imgs.into_iter().enumerate() {
        progress(&format!("Frame {} of {n}", i + 1));
        st.add(&if i == r { img } else { aligned(&reference, img) });
    }
    Ok(st.finish(w, h))
}

/// Stack `files` (in focus order) and save `<first>-stack.exr` next to them: linear 32-bit float, opened like any
/// photo. Frames are loaded one by one to keep memory down.
pub fn stack_files(files: &[PathBuf], progress: &dyn Fn(&str)) -> Result<PathBuf, String> {
    let n = files.len();
    if n < 2 {
        return Err("choose at least two photos of the same scene focused at different distances".into());
    }
    let r = n / 2;
    progress("Loading the middle frame");
    let reference = crate::load::load(&files[r])?;
    let (w, h) = reference.dimensions();
    let mut st = Stack::new(w, h);
    for (i, f) in files.iter().enumerate() {
        progress(&format!("Frame {} of {n}", i + 1));
        if i == r {
            st.add(&reference);
            continue;
        }
        let img = crate::load::load(f)?;
        if img.dimensions() != (w, h) {
            return Err(format!("{} is not the same size as the others", f.file_name().unwrap_or_default().to_string_lossy()));
        }
        st.add(&aligned(&reference, img));
    }
    drop(reference);
    let out_img = st.finish(w, h);
    progress("Saving");
    let dir = files[0].parent().unwrap_or(Path::new("."));
    let out = crate::claim_output(dir, &format!("{}-stack", crate::export_stem(&files[0])), "exr")?;
    image::DynamicImage::ImageRgb32F(out_img).save(&out).map_err(|e| {
        let _ = std::fs::remove_file(&out);
        e.to_string()
    })?;
    // Keep the lens profile and camera details of the first frame, so lens corrections still work.
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
    fn stack_keeps_the_sharp_half_of_each_frame() {
        let (w, h) = (200u32, 120u32);
        // Fine checkerboard: sharp detail everywhere.
        let scene = Rgb32FImage::from_fn(w, h, |x, y| { let v = if (x / 3 + y / 3) % 2 == 0 { 0.8 } else { 0.1 }; image::Rgb([v, v, v]) });
        let soft = |img: &Rgb32FImage| {
            let mut c = [0usize, 1, 2].map(|k| img.as_raw().iter().skip(k).step_by(3).cloned().collect::<Vec<f32>>());
            c = c.map(|ch| crate::develop::blur(&ch, w as usize, h as usize, 3.0));
            Rgb32FImage::from_fn(w, h, |x, y| { let i = (y * w + x) as usize; image::Rgb([c[0][i], c[1][i], c[2][i]]) })
        };
        let blurred = soft(&scene);
        // Frame A is in focus on the left, frame B on the right.
        let a = Rgb32FImage::from_fn(w, h, |x, y| *if x < w / 2 { scene.get_pixel(x, y) } else { blurred.get_pixel(x, y) });
        let b = Rgb32FImage::from_fn(w, h, |x, y| *if x >= w / 2 { scene.get_pixel(x, y) } else { blurred.get_pixel(x, y) });
        let out = stack(vec![a, b], &|_| {}).unwrap();
        let err = |x0: u32, x1: u32, img: &Rgb32FImage| {
            let mut e = 0.0;
            for y in 10..h - 10 {
                for x in x0..x1 {
                    e += (img.get_pixel(x, y)[0] - scene.get_pixel(x, y)[0]).abs();
                }
            }
            e / ((h - 20) * (x1 - x0)) as f32
        };
        for (x0, x1) in [(10, 80), (120, 190)] {
            let (got, soft_err) = (err(x0, x1, &out), err(x0, x1, &blurred));
            assert!(got < soft_err * 0.25, "columns {x0}..{x1}: stacked error {got} vs blurred {soft_err}");
        }
    }
}

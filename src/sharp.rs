//! A sharpness score for culling: how crisp the sharpest part of a photo is, from 0 (soft all over) to 100.
//! Measured on a 1024 px copy so photos of one burst, from one camera, compare fairly.

use image::Rgb32FImage;
use std::path::{Path, PathBuf};

/// The size every photo is brought to before measuring.
pub const SIZE: u32 = 1024;

/// Score a linear picture. Takes the strongest 1% of edges (Laplacian of gamma-encoded luma): one sharp
/// subject on a blurred background still scores high, a frame missed everywhere scores low.
pub fn score(img: &Rgb32FImage) -> u8 {
    let small = if img.width().max(img.height()) > SIZE { crate::load::fit(img, SIZE) } else { img.clone() };
    let (w, h) = (small.width() as usize, small.height() as usize);
    if w < 3 || h < 3 {
        return 0;
    }
    // Square root is close enough to a display gamma here, and cheap.
    let l: Vec<f32> = small.chunks(3).map(|c| (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).max(0.0).sqrt()).collect();
    let mut e = Vec::with_capacity(w * h);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = y * w + x;
            e.push((4.0 * l[i] - l[i - 1] - l[i + 1] - l[i - w] - l[i + w]).abs());
        }
    }
    let k = e.len() * 99 / 100;
    let (_, p99, _) = e.select_nth_unstable_by(k, |a, b| a.total_cmp(b));
    let p99 = *p99;
    let top: Vec<f32> = e.into_iter().filter(|v| *v >= p99).collect();
    // Against the picture's own tonal range, so a dark night shot and a bright beach scene score alike.
    let mut ls: Vec<f32> = l.iter().step_by(3).copied().collect();
    ls.sort_by(|a, b| a.total_cmp(b));
    let spread = (ls[ls.len() * 995 / 1000] - ls[ls.len() * 5 / 1000]).max(0.1);
    let v = top.iter().sum::<f32>() / top.len().max(1) as f32 / spread;
    (100.0 * (1.0 - (-v / 0.11).exp())).round().clamp(0.0, 100.0) as u8
}

/// Score a file from its fast preview (the JPEG a RAW carries), or None when it can't be read.
pub fn score_file(f: &Path) -> Option<u8> {
    crate::load::load_quick(f, SIZE).ok().map(|img| score(&img))
}

/// Score many files in parallel, reporting each as it finishes.
pub fn score_files(files: &[PathBuf], each: &(dyn Fn(PathBuf, Option<u8>) + Sync)) {
    use rayon::prelude::*;
    // A few at a time: a folder of big TIFF scans decodes each whole, and all cores at once would eat memory.
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build();
    match pool {
        Ok(pool) => pool.install(|| files.par_iter().for_each(|f| each(f.clone(), score_file(f)))),
        Err(_) => files.iter().for_each(|f| each(f.clone(), score_file(f))),
    }
}

/// In each burst, the sharpest frame (ties go to the earlier one). Frames with no score are left out.
pub fn sharpest<'a>(group: &'a [PathBuf], scores: &std::collections::HashMap<PathBuf, u8>) -> Option<&'a PathBuf> {
    group.iter().filter(|f| scores.contains_key(*f)).fold(None, |best: Option<&PathBuf>, f| match best {
        Some(b) if scores[b] >= scores[f] => Some(b),
        _ => Some(f),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fine detail: a checker of small squares with a few lines, in linear light.
    fn detailed() -> Rgb32FImage {
        Rgb32FImage::from_fn(800, 600, |x, y| {
            let v = if (x / 16 + y / 16) % 2 == 0 { 0.7 } else { 0.05 };
            image::Rgb([v, v, v])
        })
    }

    fn blur(img: &Rgb32FImage, r: i64) -> Rgb32FImage {
        let (w, h) = (img.width() as i64, img.height() as i64);
        Rgb32FImage::from_fn(w as u32, h as u32, |x, y| {
            let mut acc = [0f32; 3];
            let mut n = 0.0;
            for dy in -r..=r {
                for dx in -r..=r {
                    let (xx, yy) = ((x as i64 + dx).clamp(0, w - 1), (y as i64 + dy).clamp(0, h - 1));
                    let p = img.get_pixel(xx as u32, yy as u32).0;
                    (0..3).for_each(|k| acc[k] += p[k]);
                    n += 1.0;
                }
            }
            image::Rgb(acc.map(|a| a / n))
        })
    }

    #[test]
    fn sharp_beats_blurred() {
        let sharp = detailed();
        let soft = blur(&sharp, 8);
        let (a, b) = (score(&sharp), score(&soft));
        assert!(a > b + 20, "sharp {a} vs blurred {b}");
        assert!(a >= 70, "a crisp chart scores high: {a}");
        let flat = Rgb32FImage::from_pixel(400, 300, image::Rgb([0.3, 0.3, 0.3]));
        assert_eq!(score(&flat), 0);
    }

    #[test]
    fn picks_the_sharpest_of_a_group() {
        let g: Vec<PathBuf> = ["a", "b", "c", "d"].iter().map(PathBuf::from).collect();
        let s: std::collections::HashMap<PathBuf, u8> = [("a", 40), ("b", 72), ("c", 72)].iter().map(|(k, v)| (PathBuf::from(k), *v)).collect();
        assert_eq!(sharpest(&g, &s), Some(&PathBuf::from("b")));
        assert_eq!(sharpest(&g[3..], &s), None);
    }
}

#[cfg(test)]
mod real {
    /// `SHARP_FILE=x.NEF cargo test --release sharp_on_a_real_file -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn sharp_on_a_real_file() {
        let f = std::env::var("SHARP_FILE").expect("SHARP_FILE");
        let img = crate::load::load_quick(std::path::Path::new(&f), super::SIZE).unwrap();
        for r in [1, 2, 4] {
            println!("blur {r}: {}", super::score(&image::imageops::blur(&img, r as f32)));
        }
        println!("original: {}", super::score(&img));
        if let Ok(g) = std::env::var("SHARP_FILE2") {
            let img = crate::load::load_quick(std::path::Path::new(&g), super::SIZE).unwrap();
            println!("second: {} ({}x{})", super::score(&img), img.width(), img.height());
            println!("second blur 1: {}", super::score(&image::imageops::blur(&img, 1.0)));
        }
    }
}

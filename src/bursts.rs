//! Burst and duplicate stacks for the Library: runs of photos that are near copies of each other (a burst, or
//! the same frame saved twice) shown as one thumbnail with a count, so culling a folder goes faster.
//!
//! Two neighbouring photos (in shooting order) belong together when their small "difference hashes" are almost
//! the same, or when they were taken within two seconds and look alike. Nothing is written to disk.
use image::Rgb32FImage;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// What grouping needs to know about one photo.
#[derive(Clone, Copy, Debug, Default)]
pub struct Info {
    /// Capture time in seconds (any fixed origin), if the file says.
    pub taken: Option<i64>,
    /// 64-bit difference hash of the picture: each bit says whether a pixel of a 9×8 shrink is brighter than its
    /// right-hand neighbour.
    pub hash: u64,
}

/// The difference hash of a picture (any tone curve: only the order of neighbouring brightnesses counts).
pub fn dhash(img: &Rgb32FImage) -> u64 {
    let (w, h) = (img.width().max(1) as usize, img.height().max(1) as usize);
    // Average the 9×8 cells: robust to noise and to small changes in size.
    let mut cells = [[0.0f32; 9]; 8];
    let mut counts = [[0u32; 9]; 8];
    for (i, c) in img.as_raw().chunks(3).enumerate() {
        let (x, y) = (i % w, i / w);
        let (cx, cy) = ((x * 9 / w).min(8), (y * 8 / h).min(7));
        cells[cy][cx] += crate::develop::luma([c[0], c[1], c[2]]);
        counts[cy][cx] += 1;
    }
    let mut bits = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            let a = cells[y][x] / counts[y][x].max(1) as f32;
            let b = cells[y][x + 1] / counts[y][x + 1].max(1) as f32;
            bits = (bits << 1) | (a > b) as u64;
        }
    }
    bits
}

/// Gather `Info` for one photo: reads its EXIF and a small decode (the camera's embedded preview for RAW).
pub fn info(f: &Path) -> Option<Info> {
    let real = crate::load::copy_of(f).unwrap_or_else(|| f.to_path_buf());
    let img = crate::load::load_quick(&real, 96).ok()?;
    Some(Info { taken: crate::lens::shot(&real).and_then(|s| s.seconds()), hash: dhash(&img) })
}

/// Differing bits at or under which two photos count as the same picture whenever they were taken.
const SAME: u32 = 5;
/// Differing bits at or under which two photos taken within `BURST_GAP` seconds are one burst.
const ALIKE: u32 = 16;
const BURST_GAP: i64 = 2;

fn belongs(a: &Info, b: &Info) -> bool {
    let d = (a.hash ^ b.hash).count_ones();
    let close = matches!((a.taken, b.taken), (Some(x), Some(y)) if (x - y).abs() <= BURST_GAP);
    d <= SAME || (close && d <= ALIKE)
}

/// Groups of two or more neighbouring photos, in the order given (the Library's shooting order). Virtual copies
/// are left out: they are already shown next to their original.
pub fn group(items: &[(PathBuf, Info)]) -> Vec<Vec<PathBuf>> {
    let mut out: Vec<Vec<PathBuf>> = vec![];
    let mut run: Vec<&(PathBuf, Info)> = vec![];
    for it in items {
        if !run.last().is_some_and(|prev| belongs(&prev.1, &it.1)) {
            if run.len() >= 2 {
                out.push(run.iter().map(|r| r.0.clone()).collect());
            }
            run.clear();
        }
        run.push(it);
    }
    if run.len() >= 2 {
        out.push(run.iter().map(|r| r.0.clone()).collect());
    }
    out
}

/// Find the stacks among `files` (already in the Library's order). Slow-ish: run it off the UI thread.
pub fn find(files: &[PathBuf]) -> Vec<Vec<PathBuf>> {
    let items: Vec<(PathBuf, Info)> = files.par_iter().filter(|f| crate::load::copy_of(f).is_none()).filter_map(|f| Some((f.clone(), info(f)?))).collect();
    group(&items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(shift: f32, seed: f32) -> Rgb32FImage {
        Rgb32FImage::from_fn(180, 120, |x, y| {
            let v = 0.5 + 0.4 * ((x as f32 + shift) * 0.05 * seed).sin() * (y as f32 * 0.04 * seed).cos();
            image::Rgb([v, v * 0.9, v * 0.8])
        })
    }

    #[test]
    fn near_copies_hash_alike_and_different_scenes_do_not() {
        let a = dhash(&scene(0.0, 1.0));
        let b = dhash(&scene(2.0, 1.0)); // the same scene, a touch of movement
        let c = dhash(&scene(0.0, 2.7)); // another picture
        assert!((a ^ b).count_ones() <= ALIKE, "{}", (a ^ b).count_ones());
        assert!((a ^ c).count_ones() > ALIKE, "{}", (a ^ c).count_ones());
    }

    #[test]
    fn bursts_group_by_look_and_time() {
        let i = |t: i64, h: u64| Info { taken: Some(t), hash: h };
        let items: Vec<(PathBuf, Info)> = vec![
            ("a".into(), i(0, 0)), ("b".into(), i(1, 0xFF)), ("c".into(), i(2, 0xFF)), // a burst: 8 bits apart, 1 s
            ("d".into(), i(10, u64::MAX)), // a different picture
            ("e".into(), i(100, 0xF0F0)), ("f".into(), i(900, 0xF0F1)), // the same frame twice, long apart
            ("g".into(), i(901, 0xFFFF_0000_FFFF)), // alike in time only
        ];
        let g = group(&items);
        let names: Vec<Vec<String>> = g.iter().map(|v| v.iter().map(|p| p.display().to_string()).collect()).collect();
        assert_eq!(names, vec![vec!["a", "b", "c"], vec!["e", "f"]]);
    }
}

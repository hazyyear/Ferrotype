//! Video-style scopes for the rendered preview: a luma waveform, an RGB parade and a vectorscope.
//! Built on the render thread from the same display-encoded picture the histogram uses, as small images
//! the panel shows as they are.

use eframe::egui::{Color32, ColorImage};
use image::Rgb32FImage;
use rayon::prelude::*;

/// Waveform size: one column per slice of the photo's width, one row per level.
pub const WAVE_W: usize = 256;
pub const WAVE_H: usize = 128;
/// Each channel of the parade is this wide; the three sit side by side.
pub const PARADE_W: usize = 128;
/// The vectorscope is square.
pub const VEC_N: usize = 160;

/// The three scopes as ready-to-show images (premultiplied over transparent).
#[derive(Clone)]
pub struct Scopes {
    pub wave: ColorImage,
    pub parade: ColorImage,
    pub vector: ColorImage,
}

impl Default for Scopes {
    fn default() -> Self {
        Self {
            wave: ColorImage::filled([WAVE_W, WAVE_H], Color32::TRANSPARENT),
            parade: ColorImage::filled([PARADE_W * 3, WAVE_H], Color32::TRANSPARENT),
            vector: ColorImage::filled([VEC_N, VEC_N], Color32::TRANSPARENT),
        }
    }
}

/// Rec. 709 luma of display-encoded values, as a video waveform shows it.
pub fn luma709(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Rec. 709 colour difference (Cb, Cr) of display-encoded values, each in -0.5..0.5.
pub fn cbcr(c: [f32; 3]) -> (f32, f32) {
    let y = luma709(c);
    ((c[2] - y) / 1.8556, (c[0] - y) / 1.5748)
}

/// Where a colour lands on the vectorscope, in its pixel grid (x right = blue, y up = red).
pub fn vec_pos(c: [f32; 3]) -> (f32, f32) {
    let (cb, cr) = cbcr(c);
    let n = VEC_N as f32;
    // ±0.5 fills the square with a small margin.
    ((cb * 0.92 + 0.5) * n, (0.5 - cr * 0.92) * n)
}

/// Counts to brightness: a gentle curve so both a busy and a sparse trace read.
fn shade(counts: &[u32], samples: usize, cells: usize, col: [f32; 3]) -> Vec<Color32> {
    let avg = (samples as f32 / cells as f32).max(1e-3);
    counts.iter().map(|&n| {
        if n == 0 {
            return Color32::TRANSPARENT;
        }
        let a = (1.0 - (-(n as f32) / avg * 0.9).exp()).clamp(0.12, 1.0);
        let [r, g, b] = col.map(|v| (v * a * 255.0) as u8);
        Color32::from_rgba_premultiplied(r, g, b, (a * 255.0) as u8)
    }).collect()
}

/// Build all three scopes from a display-encoded (0..1) picture. Every other pixel in each direction is
/// enough for a 256-column trace and keeps this to a few milliseconds.
pub fn build(img: &Rgb32FImage) -> Scopes {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w == 0 || h == 0 {
        return Scopes::default();
    }
    let step = if w * h > 400_000 { 2 } else { 1 };
    let rows: Vec<usize> = (0..h).step_by(step).collect();
    let raw = img.as_raw();
    let lvl = |v: f32| ((1.0 - v.clamp(0.0, 1.0)) * (WAVE_H - 1) as f32).round() as usize;
    // Per-thread tallies, merged after.
    let (wave, parade, vector, samples) = rows.par_iter().fold(
        || (vec![0u32; WAVE_W * WAVE_H], vec![0u32; PARADE_W * 3 * WAVE_H], vec![0u32; VEC_N * VEC_N], 0usize),
        |(mut wv, mut pr, mut vc, mut n), &y| {
            for x in (0..w).step_by(step) {
                let i = (y * w + x) * 3;
                let c = [raw[i], raw[i + 1], raw[i + 2]];
                let wx = x * WAVE_W / w;
                wv[lvl(luma709(c)) * WAVE_W + wx] += 1;
                let px = x * PARADE_W / w;
                for (k, v) in c.iter().enumerate() {
                    pr[lvl(*v) * PARADE_W * 3 + k * PARADE_W + px] += 1;
                }
                let (vx, vy) = vec_pos(c);
                let (vx, vy) = (vx as isize, vy as isize);
                if (0..VEC_N as isize).contains(&vx) && (0..VEC_N as isize).contains(&vy) {
                    vc[vy as usize * VEC_N + vx as usize] += 1;
                }
                n += 1;
            }
            (wv, pr, vc, n)
        },
    ).reduce(
        || (vec![0u32; WAVE_W * WAVE_H], vec![0u32; PARADE_W * 3 * WAVE_H], vec![0u32; VEC_N * VEC_N], 0usize),
        |mut a, b| {
            a.0.iter_mut().zip(&b.0).for_each(|(x, y)| *x += y);
            a.1.iter_mut().zip(&b.1).for_each(|(x, y)| *x += y);
            a.2.iter_mut().zip(&b.2).for_each(|(x, y)| *x += y);
            (a.0, a.1, a.2, a.3 + b.3)
        },
    );
    let bone = [0.925, 0.91, 0.882];
    let wave_px = shade(&wave, samples, WAVE_W * WAVE_H / 6, bone);
    // Each parade column holds a third of the samples per channel width; tint per channel.
    let tints = [[0.95, 0.42, 0.38], [0.45, 0.85, 0.52], [0.45, 0.62, 1.0]];
    let mut parade_px = Vec::with_capacity(parade.len());
    for row in parade.chunks(PARADE_W * 3) {
        for k in 0..3 {
            parade_px.extend(shade(&row[k * PARADE_W..(k + 1) * PARADE_W], samples, PARADE_W * WAVE_H / 6, tints[k]));
        }
    }
    // The vectorscope is mostly empty space: scale against the occupied cells, not the whole square.
    let used = vector.iter().filter(|n| **n > 0).count().max(1);
    let vector_px = shade(&vector, samples, used, bone);
    Scopes {
        wave: ColorImage::new([WAVE_W, WAVE_H], wave_px),
        parade: ColorImage::new([PARADE_W * 3, WAVE_H], parade_px),
        vector: ColorImage::new([VEC_N, VEC_N], vector_px),
    }
}

/// The 75% colour bars a vectorscope's targets mark: red, yellow, green, cyan, blue, magenta.
pub const TARGETS: [(&str, [f32; 3]); 6] = [
    ("R", [0.75, 0.0, 0.0]),
    ("Yl", [0.75, 0.75, 0.0]),
    ("G", [0.0, 0.75, 0.0]),
    ("Cy", [0.0, 0.75, 0.75]),
    ("B", [0.0, 0.0, 0.75]),
    ("Mg", [0.75, 0.0, 0.75]),
];

/// Direction of the skin-tone line (the I axis, 123° from +Cb), as a unit step in the scope's pixel grid.
pub fn skin_dir() -> (f32, f32) {
    let a = 123f32.to_radians();
    (a.cos(), -a.sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(img: &ColorImage, x: usize, y: usize) -> bool {
        img.pixels[y * img.size[0] + x].a() > 0
    }

    #[test]
    fn grey_ramp_makes_a_diagonal_waveform() {
        let img = Rgb32FImage::from_fn(512, 64, |x, _| {
            let v = x as f32 / 511.0;
            image::Rgb([v, v, v])
        });
        let s = build(&img);
        // Left columns are near the bottom (black), right columns near the top (white).
        let top_of = |col: usize| (0..WAVE_H).find(|y| lit(&s.wave, col, *y)).unwrap();
        assert!(top_of(2) > WAVE_H - 6, "dark end low: {}", top_of(2));
        assert!(top_of(WAVE_W - 3) < 6, "bright end high: {}", top_of(WAVE_W - 3));
        assert!(top_of(WAVE_W / 2).abs_diff(WAVE_H / 2) < 6);
        // Grey has no colour: everything sits in the middle of the vectorscope.
        let c = VEC_N / 2;
        assert!(lit(&s.vector, c, c));
        let off = s.vector.pixels.iter().enumerate().filter(|(i, p)| p.a() > 0 && ((i % VEC_N).abs_diff(c) > 2 || (i / VEC_N).abs_diff(c) > 2)).count();
        assert_eq!(off, 0, "grey stays at the centre");
    }

    #[test]
    fn primaries_land_on_their_targets() {
        for (_, t) in TARGETS {
            let img = Rgb32FImage::from_pixel(32, 32, image::Rgb(t));
            let s = build(&img);
            let (x, y) = vec_pos(t);
            assert!(lit(&s.vector, x as usize, y as usize), "{t:?} at {x},{y}");
        }
        // Red is up and to the left, blue is to the right, as on any vectorscope.
        let (rx, ry) = vec_pos([0.75, 0.0, 0.0]);
        let (bx, _) = vec_pos([0.0, 0.0, 0.75]);
        let c = VEC_N as f32 / 2.0;
        assert!(ry < c && rx < c && bx > c);
    }

    #[test]
    fn parade_splits_channels() {
        // Pure red: the red third is high, green and blue sit at the bottom.
        let img = Rgb32FImage::from_pixel(64, 16, image::Rgb([1.0, 0.0, 0.0]));
        let s = build(&img);
        assert!(lit(&s.parade, PARADE_W / 2, 0));
        assert!(lit(&s.parade, PARADE_W + PARADE_W / 2, WAVE_H - 1));
        assert!(!lit(&s.parade, PARADE_W + PARADE_W / 2, 0));
    }
}

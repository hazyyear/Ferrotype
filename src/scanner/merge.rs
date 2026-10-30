#![cfg_attr(windows, allow(dead_code))]
//! Multi-pass scanning: the same frame scanned several times and averaged. Sensor noise falls by the square root
//! of the number of passes (4 passes halve it), which mostly shows in the dense parts of a negative (the
//! highlights). A flatbed's carriage never stops in quite the same place twice, so each pass is first shifted
//! into line with the first.

/// One pass: width, height, samples per pixel and the samples (0..1, interleaved).
pub type Pass = (u32, u32, usize, Vec<f32>);

/// The passes averaged, each moved by whole pixels to line up with the first. Passes of another size or layout
/// than the first are left out.
pub fn average(passes: &[Pass]) -> Option<Pass> {
    let (w, h, s, first) = passes.first()?.clone();
    let mut sum: Vec<f32> = first.clone();
    let mut n = vec![1u16; (w * h) as usize];
    for (pw, ph, ps, data) in &passes[1..] {
        if (*pw, *ph, *ps) != (w, h, s) {
            continue;
        }
        let (dx, dy) = shift_of(&first, data, w as usize, h as usize, s);
        for y in 0..h as i64 {
            let sy = y + dy as i64;
            if sy < 0 || sy >= h as i64 {
                continue;
            }
            for x in 0..w as i64 {
                let sx = x + dx as i64;
                if sx < 0 || sx >= w as i64 {
                    continue;
                }
                let (o, i) = ((y * w as i64 + x) as usize, (sy * w as i64 + sx) as usize);
                for k in 0..s {
                    sum[o * s + k] += data[i * s + k];
                }
                n[o] += 1;
            }
        }
    }
    for (px, &c) in sum.chunks_exact_mut(s).zip(&n) {
        px.iter_mut().for_each(|v| *v /= c as f32);
    }
    Some((w, h, s, sum))
}

/// How far `b` sits from `a`, in whole pixels (a pixel of `a` at x is at x + dx in `b`), within ±8 pixels: the
/// shift with the smallest difference over a patch from the middle of the frame.
pub fn shift_of(a: &[f32], b: &[f32], w: usize, h: usize, s: usize) -> (i32, i32) {
    const R: i32 = 8;
    let luma = |d: &[f32], x: usize, y: usize| -> f32 {
        let p = &d[(y * w + x) * s..];
        if s >= 3 { 0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2] } else { p[0] }
    };
    let pw = (w / 2).min(400);
    let ph = (h / 2).min(400);
    if pw < 4 * R as usize || ph < 4 * R as usize {
        return (0, 0);
    }
    let (x0, y0) = ((w - pw) / 2, (h - ph) / 2);
    let step = (pw.max(ph) / 120).max(1);
    let mut best = (f32::MAX, (0, 0));
    for dy in -R..=R {
        for dx in -R..=R {
            let mut err = 0f32;
            for y in (y0..y0 + ph).step_by(step) {
                for x in (x0..x0 + pw).step_by(step) {
                    let (bx, by) = ((x as i32 + dx) as usize, (y as i32 + dy) as usize);
                    err += (luma(a, x, y) - luma(b, bx, by)).abs();
                }
            }
            // Ties go to the smaller move.
            let err = err * (1.0 + 1e-4 * (dx.abs() + dy.abs()) as f32);
            if err < best.0 {
                best = (err, (dx, dy));
            }
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A textured test scene, one sample per pixel, with noise from a simple generator.
    fn scene(w: usize, h: usize, ox: i32, oy: i32, seed: u32) -> Vec<f32> {
        let mut r = seed;
        (0..w * h).map(|i| {
            let (x, y) = ((i % w) as i32 - ox, (i / w) as i32 - oy);
            r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((r >> 8) as f32 / (1 << 24) as f32 - 0.5) * 0.1;
            0.5 + 0.3 * ((x as f32 / 7.0).sin() * (y as f32 / 11.0).cos()) + noise
        }).collect()
    }

    #[test]
    fn passes_are_lined_up_and_noise_falls() {
        let (w, h) = (200, 160);
        // The second pass sits 3 px right and 2 px down.
        let a = scene(w, h, 0, 0, 1);
        let b = scene(w, h, 3, 2, 2);
        assert_eq!(shift_of(&a, &b, w, h, 1), (3, 2));
        // Four passes, all in line: the noise around the true picture drops by about half.
        let truth: Vec<f32> = (0..w * h).map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            0.5 + 0.3 * ((x / 7.0).sin() * (y / 11.0).cos())
        }).collect();
        let passes: Vec<Pass> = (1..=4).map(|k| (w as u32, h as u32, 1, scene(w, h, 0, 0, k))).collect();
        let rms = |d: &[f32]| (d.iter().zip(&truth).map(|(a, b)| (a - b).powi(2)).sum::<f32>() / d.len() as f32).sqrt();
        let one = rms(&passes[0].3);
        let four = rms(&average(&passes).unwrap().3);
        assert!(four < one * 0.6, "{one} → {four}");
    }
}

//! Multi-pass scanning: the same frame scanned several times and averaged. Sensor noise falls by the square root
//! of the number of passes (4 passes halve it), which mostly shows in the dense parts of a negative (the
//! highlights). A flatbed's carriage never stops in quite the same place twice, so each pass is first shifted
//! into line with the ones before.
//!
//! Passes are added one at a time, so memory stays at one running sum plus the pass being added, however many
//! passes there are: a film strip at 2400 ppi is over 100 million pixels.

/// The running sum of the passes added so far.
pub struct Sum {
    pub w: u32,
    pub h: u32,
    pub samples: usize,
    sum: Vec<f32>,
    /// Passes counted at each pixel (the edges of a shifted pass add nothing).
    n: Vec<u8>,
}

impl Sum {
    /// Start from the first pass (16-bit samples, interleaved).
    pub fn new(w: u32, h: u32, samples: usize, first: &[u16]) -> Sum {
        Sum { w, h, samples, sum: first.iter().map(|&v| v as f32).collect(), n: vec![1; w as usize * h as usize] }
    }

    /// Add a pass, moved by whole pixels to line up with the sum. A pass of another size or layout is left out:
    /// returns whether it was added.
    pub fn add(&mut self, w: u32, h: u32, samples: usize, data: &[u16]) -> bool {
        if (w, h, samples) != (self.w, self.h, self.samples) || data.len() != self.sum.len() {
            return false;
        }
        let (w, h, s) = (w as i64, h as i64, samples);
        let (dx, dy) = shift_of(&self.sum, &self.n, data, w as usize, h as usize, s);
        for y in 0..h {
            let sy = y + dy as i64;
            if sy < 0 || sy >= h {
                continue;
            }
            for x in 0..w {
                let sx = x + dx as i64;
                if sx < 0 || sx >= w {
                    continue;
                }
                let (o, i) = ((y * w + x) as usize, (sy * w + sx) as usize);
                for k in 0..s {
                    self.sum[o * s + k] += data[i * s + k] as f32;
                }
                self.n[o] = self.n[o].saturating_add(1);
            }
        }
        true
    }

    /// The average, as 16-bit samples.
    pub fn average(&self) -> Vec<u16> {
        self.sum.chunks_exact(self.samples).zip(&self.n).flat_map(|(px, &c)| px.iter().map(move |v| (v / c as f32).round().clamp(0.0, 65535.0) as u16)).collect()
    }
}

/// How far `b` sits from the running sum `a` (`n` passes per pixel), in whole pixels (a pixel of `a` at x is at
/// x + dx in `b`), within ±8 pixels: the shift with the smallest difference over a patch from the middle.
fn shift_of(a: &[f32], n: &[u8], b: &[u16], w: usize, h: usize, s: usize) -> (i32, i32) {
    const R: i32 = 8;
    let luma = |p: [f32; 3]| if s >= 3 { 0.25 * p[0] + 0.5 * p[1] + 0.25 * p[2] } else { p[0] };
    let la = |x: usize, y: usize| {
        let i = y * w + x;
        let c = n[i].max(1) as f32;
        luma([a[i * s] / c, a[i * s + 1.min(s - 1)] / c, a[i * s + 2.min(s - 1)] / c])
    };
    let lb = |x: usize, y: usize| {
        let i = (y * w + x) * s;
        luma([b[i] as f32, b[i + 1.min(s - 1)] as f32, b[i + 2.min(s - 1)] as f32])
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
                    err += (la(x, y) - lb((x as i32 + dx) as usize, (y as i32 + dy) as usize)).abs();
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
    fn scene(w: usize, h: usize, ox: i32, oy: i32, seed: u32) -> Vec<u16> {
        let mut r = seed;
        (0..w * h).map(|i| {
            let (x, y) = ((i % w) as i32 - ox, (i / w) as i32 - oy);
            r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((r >> 8) as f32 / (1 << 24) as f32 - 0.5) * 0.1;
            ((0.5 + 0.3 * ((x as f32 / 7.0).sin() * (y as f32 / 11.0).cos()) + noise) * 65535.0) as u16
        }).collect()
    }

    #[test]
    fn passes_are_lined_up_and_noise_falls() {
        let (w, h) = (200, 160);
        // The second pass sits 3 px right and 2 px down.
        let a = scene(w, h, 0, 0, 1);
        let b = scene(w, h, 3, 2, 2);
        let first = Sum::new(w as u32, h as u32, 1, &a);
        assert_eq!(shift_of(&first.sum, &first.n, &b, w, h, 1), (3, 2));
        // Four passes, all in line: the noise around the true picture drops by about half.
        let truth: Vec<f32> = (0..w * h).map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            (0.5 + 0.3 * ((x / 7.0).sin() * (y / 11.0).cos())) * 65535.0
        }).collect();
        let rms = |d: &[u16]| (d.iter().zip(&truth).map(|(&a, b)| (a as f32 - b).powi(2)).sum::<f32>() / d.len() as f32).sqrt();
        let mut sum = Sum::new(w as u32, h as u32, 1, &scene(w, h, 0, 0, 1));
        for k in 2..=4 {
            assert!(sum.add(w as u32, h as u32, 1, &scene(w, h, 0, 0, k)));
        }
        assert!(!sum.add(w as u32 - 1, h as u32, 1, &scene(w - 1, h, 0, 0, 9)), "another size is left out");
        let one = rms(&scene(w, h, 0, 0, 1));
        let four = rms(&sum.average());
        assert!(four < one * 0.6, "{one} → {four}");
    }
}

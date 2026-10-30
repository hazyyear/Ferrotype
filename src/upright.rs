//! Upright: level the horizon and straighten converging lines, found from the photo's own straight edges.
//!
//! No machine learning: short straight edge pieces ("edgelets") are found with a structure tensor on a small copy
//! of the photo, kept only when their neighbours along the same line agree (so grass and foliage don't vote), and
//! then Rotate, Vertical and Horizontal are searched for the values that make the near-vertical pieces vertical and
//! the near-horizontal ones horizontal, through the same `Mapper` the render uses.

use crate::develop::{Mapper, Params};
use image::Rgb32FImage;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    /// Level plus a gentle vertical fix, only as far as the photo clearly asks for it.
    Auto,
    /// Rotation only.
    Level,
    /// Rotation and converging verticals.
    Vertical,
    /// Rotation, verticals and horizontals.
    Full,
}

impl Mode {
    pub const ALL: [(Mode, &'static str); 4] = [(Mode::Auto, "Auto"), (Mode::Level, "Level"), (Mode::Vertical, "Vertical"), (Mode::Full, "Full")];

    pub fn parse(s: &str) -> Option<Mode> {
        Mode::ALL.iter().find(|(_, n)| n.eq_ignore_ascii_case(s)).map(|m| m.0)
    }
}

/// A short straight piece of edge: position (source pixels), unit direction, and how much it counts.
#[derive(Clone, Copy, Debug)]
pub struct Edgelet {
    pub x: f32,
    pub y: f32,
    pub dx: f32,
    pub dy: f32,
    pub w: f32,
}

const BLOCK: usize = 8;

/// Straight edge pieces in `img` (already quarter-turned), in its own pixel coordinates.
pub fn edgelets(img: &Rgb32FImage) -> (Vec<Edgelet>, u32, u32) {
    let (w0, h0) = img.dimensions();
    // Work on a copy about 640 px on the long side: enough for lines, fast, and noise averages out.
    let step = ((w0.max(h0) as f32 / 640.0).ceil() as u32).max(1);
    let (w, h) = ((w0 / step).max(1) as usize, (h0 / step).max(1) as usize);
    let mut lum = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for j in 0..step {
                for i in 0..step {
                    let p = img.get_pixel((x as u32 * step + i).min(w0 - 1), (y as u32 * step + j).min(h0 - 1)).0;
                    s += 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                }
            }
            // Square root: edges in the shadows count about as much as edges in the highlights.
            lum[y * w + x] = (s / (step * step) as f32).max(0.0).sqrt();
        }
    }
    let (bw, bh) = (w / BLOCK, h / BLOCK);
    if bw < 3 || bh < 3 {
        return (vec![], w0, h0);
    }
    // Sobel gradient per pixel.
    let mut grad = vec![(0f32, 0f32); w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let l = |dx: isize, dy: isize| lum[(y as isize + dy) as usize * w + (x as isize + dx) as usize];
            let gx = (l(1, -1) + 2.0 * l(1, 0) + l(1, 1)) - (l(-1, -1) + 2.0 * l(-1, 0) + l(-1, 1));
            let gy = (l(-1, 1) + 2.0 * l(0, 1) + l(1, 1)) - (l(-1, -1) + 2.0 * l(0, -1) + l(1, -1));
            grad[y * w + x] = (gx, gy);
        }
    }
    // Per block: structure tensor of the gradient → edge angle, coherence and strength.
    let mut blocks = vec![(0f32, 0f32, 0f32, 0f32, 0f32); bw * bh]; // (angle of the edge, coherence, energy, cx, cy)
    for by in 0..bh {
        for bx in 0..bw {
            let (mut jxx, mut jxy, mut jyy, mut sw, mut cx, mut cy) = (0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
            for y in by * BLOCK..(by + 1) * BLOCK {
                for x in bx * BLOCK..(bx + 1) * BLOCK {
                    let (gx, gy) = grad[y * w + x];
                    let m = gx * gx + gy * gy;
                    jxx += gx * gx;
                    jxy += gx * gy;
                    jyy += gy * gy;
                    // Centre of the block's edge: where the gradient is strongest.
                    sw += m;
                    cx += m * x as f32;
                    cy += m * y as f32;
                }
            }
            let tr = jxx + jyy;
            if tr <= 0.0 {
                continue;
            }
            let coh = ((jxx - jyy).powi(2) + 4.0 * jxy * jxy).sqrt() / tr;
            // Gradient direction, then the edge runs at right angles to it.
            let grad = 0.5 * (2.0 * jxy).atan2(jxx - jyy);
            let edge = grad + std::f32::consts::FRAC_PI_2;
            blocks[by * bw + bx] = (edge, coh, tr, cx / sw, cy / sw);
        }
    }
    // Ignore faint blocks: below a share of the strongest (texture-free skies and walls carry no line).
    let mut energies: Vec<f32> = blocks.iter().map(|b| b.2).filter(|&e| e > 0.0).collect();
    if energies.is_empty() {
        return (vec![], w0, h0);
    }
    energies.sort_by(|a, b| a.total_cmp(b));
    let floor = energies[energies.len() * 6 / 10].max(energies[energies.len() - 1] * 0.01);
    let good = |b: &(f32, f32, f32, f32, f32)| b.1 > 0.75 && b.2 >= floor;
    let same = |a: f32, b: f32| {
        let mut d = (a - b).rem_euclid(std::f32::consts::PI);
        if d > std::f32::consts::FRAC_PI_2 {
            d = std::f32::consts::PI - d;
        }
        d < 4f32.to_radians()
    };
    let mut out = vec![];
    let s = step as f32;
    for by in 0..bh {
        for bx in 0..bw {
            let b = blocks[by * bw + bx];
            if !good(&b) {
                continue;
            }
            // Part of a line: follow the edge a few blocks either way through blocks with the same direction. A
            // piece needs company on both sides (grass and leaves rarely line up like that), and the line through
            // the centres it collects gives a far more precise angle than one 8 px block can.
            let (ux, uy) = (b.0.cos(), b.0.sin());
            let mut chain = vec![(bx, by)];
            let mut sides = 0;
            for dir in [-1.0f32, 1.0] {
                let (mut cx, mut cy) = (bx as f32 + 0.5, by as f32 + 0.5);
                let mut found = 0;
                'walk: for _ in 0..5 {
                    for reach in [1.0f32, 1.5] {
                        let nx = (cx + dir * ux * reach).floor();
                        let ny = (cy + dir * uy * reach).floor();
                        if nx < 0.0 || ny < 0.0 || nx >= bw as f32 || ny >= bh as f32 || (nx == cx.floor() && ny == cy.floor()) {
                            continue;
                        }
                        let n = blocks[ny as usize * bw + nx as usize];
                        if good(&n) && same(n.0, b.0) {
                            chain.push((nx as usize, ny as usize));
                            (cx, cy) = (nx + 0.5, ny + 0.5);
                            found += 1;
                            continue 'walk;
                        }
                    }
                    break;
                }
                if found > 0 {
                    sides += 1;
                }
            }
            if sides < 2 {
                continue;
            }
            // Fit the line to the edge pixels themselves (gradient across the line, strong enough), weighted by
            // strength: sub-pixel precise, unlike block centres.
            let (nxv, nyv) = (-uy, ux);
            // Light-to-dark and dark-to-light edges are fitted apart: a thin bar has one of each a few pixels apart,
            // and fitting both as one line tilts it wherever the two are covered unevenly.
            let mut acc = [(0f32, 0f32, 0f32, 0f32, 0f32, 0f32); 2]; // w, x, y, xx, xy, yy
            let mut peak = 0f32;
            for &(cx, cy) in &chain {
                for y in cy * BLOCK..(cy + 1) * BLOCK {
                    for x in cx * BLOCK..(cx + 1) * BLOCK {
                        let (gx, gy) = grad[y * w + x];
                        peak = peak.max(gx * gx + gy * gy);
                    }
                }
            }
            for &(cx, cy) in &chain {
                for y in cy * BLOCK..(cy + 1) * BLOCK {
                    for x in cx * BLOCK..(cx + 1) * BLOCK {
                        let (gx, gy) = grad[y * w + x];
                        let m = gx * gx + gy * gy;
                        let along = gx * nxv + gy * nyv;
                        if m < peak * 0.2 || along.abs() < 0.97 * m.sqrt() {
                            continue;
                        }
                        let a = &mut acc[(along > 0.0) as usize];
                        let (xf, yf) = (x as f32, y as f32);
                        *a = (a.0 + m, a.1 + m * xf, a.2 + m * yf, a.3 + m * xf * xf, a.4 + m * xf * yf, a.5 + m * yf * yf);
                    }
                }
            }
            let fit = |a: (f32, f32, f32, f32, f32, f32)| {
                let (mx, my) = (a.1 / a.0, a.2 / a.0);
                let (sxx, sxy, syy) = (a.3 / a.0 - mx * mx, a.4 / a.0 - mx * my, a.5 / a.0 - my * my);
                0.5 * (2.0 * sxy).atan2(sxx - syy)
            };
            // The polarity with more edge, i.e. the longer, better covered line.
            let a = if acc[0].0 >= acc[1].0 { acc[0] } else { acc[1] };
            if a.0 <= 0.0 {
                continue;
            }
            let mut ang = fit(a);
            // Second pass: the blocks clip a line that drifts across a block edge, which pulls the angle towards
            // the pixel grid. Refit on every pixel of the same polarity within 2 px of the first line, over the
            // stretch the chain covers.
            let pol = if acc[0].0 >= acc[1].0 { -1.0f32 } else { 1.0 };
            let (mx, my) = (a.1 / a.0, a.2 / a.0);
            for _ in 0..2 {
                let (lx, ly) = (ang.cos(), ang.sin());
                let (px, py) = (-ly, lx);
                let half = chain.iter().map(|&(cx, cy)| {
                    let (dx, dy) = ((cx as f32 + 0.5) * BLOCK as f32 - mx, (cy as f32 + 0.5) * BLOCK as f32 - my);
                    (dx * lx + dy * ly).abs()
                }).fold(0f32, f32::max) + BLOCK as f32 / 2.0;
                let mut b2 = (0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
                let (x0, x1) = (((mx - half).floor() as isize).max(1), ((mx + half).ceil() as isize).min(w as isize - 2));
                let (y0, y1) = (((my - half).floor() as isize).max(1), ((my + half).ceil() as isize).min(h as isize - 2));
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let (xf, yf) = (x as f32, y as f32);
                        let (dx, dy) = (xf - mx, yf - my);
                        if (dx * px + dy * py).abs() > 2.0 || (dx * lx + dy * ly).abs() > half {
                            continue;
                        }
                        let (gx, gy) = grad[y as usize * w + x as usize];
                        let m = gx * gx + gy * gy;
                        let along = (gx * nxv + gy * nyv) * pol;
                        if m < peak * 0.1 || along < 0.9 * m.sqrt() {
                            continue;
                        }
                        b2 = (b2.0 + m, b2.1 + m * xf, b2.2 + m * yf, b2.3 + m * xf * xf, b2.4 + m * xf * yf, b2.5 + m * yf * yf);
                    }
                }
                if b2.0 > 0.0 {
                    ang = fit(b2);
                }
            }
            let (ux, uy) = if same(ang, b.0) { (ang.cos(), ang.sin()) } else { (ux, uy) };
            out.push(Edgelet { x: (b.3 + 0.5) * s, y: (b.4 + 0.5) * s, dx: ux, dy: uy, w: b.1 * b.1 * b.2.sqrt() });
        }
    }
    (out, w0, h0)
}

/// Robust penalty: like a squared error for small misses, flat for wild ones (so stray edges don't pull).
fn rho(e: f32, sigma: f32) -> f32 {
    let e2 = e * e;
    e2 / (e2 + sigma * sigma)
}

struct Sets {
    vert: Vec<Edgelet>,
    hor: Vec<Edgelet>,
}

fn split(edges: &[Edgelet]) -> Sets {
    let (mut vert, mut hor) = (vec![], vec![]);
    for e in edges {
        let a = e.dy.atan2(e.dx).to_degrees().rem_euclid(180.0); // 0 = horizontal, 90 = vertical
        if (a - 90.0).abs() < 25.0 {
            vert.push(*e);
        } else if !(25.0..=155.0).contains(&a) {
            hor.push(*e);
        }
    }
    Sets { vert, hor }
}

/// How far (degrees) each edge piece is from upright after the transform `p`, for pieces of the given set.
fn misses<'a>(m: &'a Mapper, set: &'a [Edgelet], vertical: bool) -> impl Iterator<Item = (f32, f32)> + 'a {
    set.iter().map(move |e| {
        let l = 6.0;
        let a = m.unmap(e.x - e.dx * l, e.y - e.dy * l, (e.x - e.dx * l, e.y - e.dy * l));
        let b = m.unmap(e.x + e.dx * l, e.y + e.dy * l, (e.x + e.dx * l, e.y + e.dy * l));
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let dev = if vertical { dx.atan2(dy) } else { dy.atan2(dx) }.to_degrees();
        // Fold so a line pointing the "other way" counts the same.
        let dev = if dev > 90.0 { dev - 180.0 } else if dev < -90.0 { dev + 180.0 } else { dev };
        (dev, e.w)
    })
}

/// The Rotate, Vertical and Horizontal values that make the photo upright. `p` supplies everything else that bends
/// the picture (lens profile, manual distortion, scale, flip); its own straightening is ignored.
/// `img` is the source before geometry (any size), not yet quarter-turned.
pub fn solve(img: &Rgb32FImage, p: &Params, mode: Mode) -> (f32, f32, f32) {
    let turned = match p.rot90 % 4 {
        0 => None,
        1 => Some(image::imageops::rotate90(img)),
        2 => Some(image::imageops::rotate180(img)),
        _ => Some(image::imageops::rotate270(img)),
    };
    let (edges, w, h) = edgelets(turned.as_ref().unwrap_or(img));
    solve_edges(&edges, w, h, p, mode)
}

pub fn solve_edges(edges: &[Edgelet], w: u32, h: u32, p: &Params, mode: Mode) -> (f32, f32, f32) {
    let sets = split(edges);
    let total: f32 = edges.iter().map(|e| e.w).sum::<f32>().max(1e-6);
    let vw: f32 = sets.vert.iter().map(|e| e.w).sum();
    let hw: f32 = sets.hor.iter().map(|e| e.w).sum();
    if vw + hw < total * 0.05 || sets.vert.len() + sets.hor.len() < 4 {
        return (0.0, 0.0, 0.0);
    }
    let (use_v, use_h) = (mode != Mode::Level || vw > 0.0, true);
    let (free_v, free_h) = match mode {
        Mode::Level => (false, false),
        Mode::Vertical | Mode::Auto => (true, false),
        Mode::Full => (true, true),
    };
    // With the verticals free, horizontals mostly decide the level; with them fixed, both sets vote on it.
    let hor_weight = if free_v && !free_h { 0.5 } else { 1.0 };
    let reg = if mode == Mode::Auto { 0.6 } else { 0.15 };
    let cost = |rot: f32, v: f32, hz: f32, sigma: f32| {
        let mut q = p.clone();
        (q.t_rot, q.t_vert, q.t_hor) = (rot, v, hz);
        let m = Mapper::new(&q, w, h);
        let mut c = 0.0;
        if use_v {
            c += misses(&m, &sets.vert, true).map(|(d, wt)| wt * rho(d, sigma)).sum::<f32>() / total;
        }
        if use_h {
            c += hor_weight * misses(&m, &sets.hor, false).map(|(d, wt)| wt * rho(d, sigma)).sum::<f32>() / total;
        }
        // A little pull towards no correction, stronger in Auto, so a photo without clear lines stays as it was.
        c + reg * 1e-4 * (rot * rot * 4.0 + v * v * 0.05 + hz * hz * 0.05)
    };
    // Coarse rotation scan with a wide penalty, then coordinate descent with a narrow one.
    let (mut rot, mut v, mut hz) = (0.0f32, 0.0f32, 0.0f32);
    let mut best = f32::MAX;
    let mut r = -15.0;
    while r <= 15.0 {
        let c = cost(r, 0.0, 0.0, 6.0);
        if c < best {
            (best, rot) = (c, r);
        }
        r += 0.5;
    }
    for sigma in [4.0, 1.5] {
        best = cost(rot, v, hz, sigma);
        for stepsz in [16.0f32, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25] {
            let mut moved = true;
            let mut rounds = 0;
            while moved && rounds < 20 {
                moved = false;
                rounds += 1;
                for k in 0..3 {
                    if (k == 1 && !free_v) || (k == 2 && !free_h) {
                        continue;
                    }
                    // Rotation moves in degrees, the perspective sliders in slider units (a bigger range).
                    let d = if k == 0 { stepsz * 0.1 } else { stepsz };
                    for sgn in [-1.0f32, 1.0] {
                        let (mut a, mut b, mut c) = (rot, v, hz);
                        match k {
                            0 => a = (a + sgn * d).clamp(-45.0, 45.0),
                            1 => b = (b + sgn * d).clamp(-100.0, 100.0),
                            _ => c = (c + sgn * d).clamp(-100.0, 100.0),
                        }
                        let cc = cost(a, b, c, sigma);
                        if cc < best - 1e-7 {
                            (best, rot, v, hz) = (cc, a, b, c);
                            moved = true;
                        }
                    }
                }
            }
        }
    }
    if mode == Mode::Auto {
        // Auto keeps a correction only if it clearly helps, and never goes all the way to extreme keystoning.
        let before = cost(0.0, 0.0, 0.0, 1.5);
        if best > before * 0.9 {
            return (0.0, 0.0, 0.0);
        }
        v = v.clamp(-60.0, 60.0);
    }
    let round = |x: f32, s: f32| (x / s).round() * s;
    (round(rot, 0.05), round(v, 0.5), round(hz, 0.5))
}

/// `p` with Upright applied (Rotate, Vertical and Horizontal set; everything else kept).
pub fn apply(img: &Rgb32FImage, p: &Params, mode: Mode) -> Params {
    let (r, v, h) = solve(img, p, mode);
    let mut q = p.clone();
    (q.t_rot, q.t_vert, q.t_hor) = (r, v, h);
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture of a "building": dark vertical and horizontal bars on a light ground, drawn through a known tilt.
    pub fn scene(w: u32, h: u32, p: &Params) -> Rgb32FImage {
        // Render the scene as the camera would see it: for each source pixel, find where it lands in the upright
        // frame (`unmap` of the transform that *corrects* it), and paint a grid there.
        let m = Mapper::new(p, w, h);
        let mut img = Rgb32FImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // 4×4 samples per pixel, so edges are smooth as in a real photo (a hard staircase hides small tilts).
                let mut v = 0.0;
                for j in 0..4 {
                    for i in 0..4 {
                        let (sx, sy) = (x as f32 + (i as f32 + 0.5) / 4.0, y as f32 + (j as f32 + 0.5) / 4.0);
                        let (fx, fy) = m.unmap(sx, sy, (sx, sy));
                        let on = |c: f32, period: f32| (c / period).rem_euclid(1.0) < 0.12;
                        v += if on(fx, 60.0) || on(fy, 80.0) { 0.05 } else { 0.7 } / 16.0;
                    }
                }
                img.put_pixel(x, y, image::Rgb([v, v, v]));
            }
        }
        img
    }

    #[test]
    fn level_finds_a_tilt() {
        let truth = Params { t_rot: 3.0, ..Params::default() };
        let img = scene(600, 400, &truth);
        let (r, v, h) = solve(&img, &Params::default(), Mode::Level);
        assert!((r - 3.0).abs() < 0.15, "rotate {r}");
        assert_eq!((v, h), (0.0, 0.0));
    }

    #[test]
    fn vertical_fixes_converging_lines() {
        let truth = Params { t_rot: -1.5, t_vert: 30.0, ..Params::default() };
        let img = scene(600, 800, &truth);
        let (r, v, _) = solve(&img, &Params::default(), Mode::Vertical);
        assert!((r + 1.5).abs() < 0.3, "rotate {r}");
        assert!((v - 30.0).abs() < 3.0, "vertical {v}");
    }

    #[test]
    fn full_fixes_both_directions() {
        let truth = Params { t_vert: -20.0, t_hor: 15.0, ..Params::default() };
        let img = scene(700, 500, &truth);
        let (r, v, h) = solve(&img, &Params::default(), Mode::Full);
        assert!(r.abs() < 0.4 && (v + 20.0).abs() < 4.0 && (h - 15.0).abs() < 4.0, "{r} {v} {h}");
    }

    #[test]
    fn follows_quarter_turns() {
        // The same tilted scene, stored on its side: Level must still find the tilt once turned back.
        let truth = Params { t_rot: 2.0, ..Params::default() };
        let img = image::imageops::rotate270(&scene(600, 400, &truth));
        let p = Params { rot90: 1, ..Params::default() };
        let (r, ..) = solve(&img, &p, Mode::Level);
        assert!((r - 2.0).abs() < 0.15, "rotate {r}");
    }

    #[test]
    fn auto_leaves_lineless_photos_alone() {
        // Smooth noise: no straight lines at all.
        let mut img = Rgb32FImage::new(400, 300);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = 0.4 + 0.3 * ((x as f32 * 0.05).sin() * (y as f32 * 0.07).cos());
            *px = image::Rgb([v, v, v]);
        }
        assert_eq!(solve(&img, &Params::default(), Mode::Auto), (0.0, 0.0, 0.0));
    }

    #[test]
    fn parse_modes() {
        assert_eq!(Mode::parse("full"), Some(Mode::Full));
        assert_eq!(Mode::parse("nope"), None);
    }
}

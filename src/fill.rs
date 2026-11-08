//! Content-aware fill for the Heal tool: rebuild an area from texture found around it, with PatchMatch
//! (Barnes et al. 2009) run coarse to fine. Only a box around the area is touched, so it stays fast enough
//! for the live preview.
use crate::develop::px3;
use image::Rgb32FImage;

/// Half the patch size: 7×7 patches.
const HALF: i32 = 3;

/// A small deterministic random source (xorshift), so a fill is the same on every render.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    fn below(&mut self, n: i32) -> i32 {
        (self.next() % n.max(1) as u32) as i32
    }
}

/// One level of the pyramid: the box's pixels and which of them are to be filled.
struct Level {
    w: i32,
    h: i32,
    px: Vec<[f32; 3]>,
    hole: Vec<bool>,
}

impl Level {
    fn at(&self, x: i32, y: i32) -> [f32; 3] {
        self.px[(y.clamp(0, self.h - 1) * self.w + x.clamp(0, self.w - 1)) as usize]
    }

    fn is_hole(&self, x: i32, y: i32) -> bool {
        self.hole[(y.clamp(0, self.h - 1) * self.w + x.clamp(0, self.w - 1)) as usize]
    }

    /// Half size: averages of 2×2 blocks; a block is a hole if any of it is.
    fn down(&self) -> Level {
        let (w, h) = ((self.w + 1) / 2, (self.h + 1) / 2);
        let mut px = vec![[0.0; 3]; (w * h) as usize];
        let mut hole = vec![false; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0; 3];
                let mut any = false;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let c = self.at(2 * x + dx, 2 * y + dy);
                    (0..3).for_each(|k| acc[k] += c[k] / 4.0);
                    any |= self.is_hole(2 * x + dx, 2 * y + dy);
                }
                px[(y * w + x) as usize] = acc;
                hole[(y * w + x) as usize] = any;
            }
        }
        Level { w, h, px, hole }
    }
}

/// Patch distance between the patch at `a` (in the hole) and the one at `b` (a source), over all pixels.
/// Gives up early once it is worse than `best`.
fn dist(l: &Level, a: (i32, i32), b: (i32, i32), best: f32) -> f32 {
    let mut d = 0.0;
    for dy in -HALF..=HALF {
        for dx in -HALF..=HALF {
            let (p, q) = (l.at(a.0 + dx, a.1 + dy), l.at(b.0 + dx, b.1 + dy));
            d += (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2);
        }
        if d > best {
            return d;
        }
    }
    d
}

/// A source patch centre must be inside the box and away from the hole, so it holds only real picture.
fn valid(l: &Level, ok: &[bool], x: i32, y: i32) -> bool {
    x >= HALF && y >= HALF && x < l.w - HALF && y < l.h - HALF && ok[(y * l.w + x) as usize]
}

/// Where patches may come from: centres whose whole patch misses the hole.
fn sources(l: &Level) -> Vec<bool> {
    let mut ok = vec![true; (l.w * l.h) as usize];
    for y in 0..l.h {
        for x in 0..l.w {
            if l.hole[(y * l.w + x) as usize] {
                for dy in -HALF..=HALF {
                    for dx in -HALF..=HALF {
                        let (xx, yy) = (x + dx, y + dy);
                        if xx >= 0 && yy >= 0 && xx < l.w && yy < l.h {
                            ok[(yy * l.w + xx) as usize] = false;
                        }
                    }
                }
            }
        }
    }
    ok
}

/// Run PatchMatch on one level: improve the match `nnf` of every hole pixel, then rebuild the hole from the
/// matched patches (each pixel averages what the patches covering it say).
fn patchmatch(l: &mut Level, nnf: &mut [(i32, i32)], rng: &mut Rng, iters: usize) {
    let ok = sources(l);
    if !ok.iter().any(|v| *v) {
        return;
    }
    let holes: Vec<(i32, i32)> = (0..l.h).flat_map(|y| (0..l.w).map(move |x| (x, y))).filter(|&(x, y)| l.hole[(y * l.w + x) as usize]).collect();
    // Any match that isn't a valid source (new pixels after upscaling) starts from a random one.
    for &(x, y) in &holes {
        let i = (y * l.w + x) as usize;
        let (sx, sy) = nnf[i];
        if !valid(l, &ok, sx, sy) {
            for _ in 0..200 {
                let c = (rng.below(l.w), rng.below(l.h));
                if valid(l, &ok, c.0, c.1) {
                    nnf[i] = c;
                    break;
                }
            }
        }
    }
    let mut cost: Vec<f32> = vec![f32::MAX; (l.w * l.h) as usize];
    for it in 0..iters {
        for &(x, y) in &holes {
            let i = (y * l.w + x) as usize;
            cost[i] = dist(l, (x, y), nnf[i], f32::MAX);
        }
        // Alternate the scan direction, so good matches spread both ways.
        let order: Box<dyn Iterator<Item = &(i32, i32)>> = if it % 2 == 0 { Box::new(holes.iter()) } else { Box::new(holes.iter().rev()) };
        let step = if it % 2 == 0 { 1 } else { -1 };
        for &(x, y) in order {
            let i = (y * l.w + x) as usize;
            // Propagation: the neighbour's match, shifted by one.
            for (nx, ny) in [(x - step, y), (x, y - step)] {
                if nx < 0 || ny < 0 || nx >= l.w || ny >= l.h || !l.hole[(ny * l.w + nx) as usize] {
                    continue;
                }
                let m = nnf[(ny * l.w + nx) as usize];
                let c = (m.0 + (x - nx), m.1 + (y - ny));
                if valid(l, &ok, c.0, c.1) {
                    let d = dist(l, (x, y), c, cost[i]);
                    if d < cost[i] {
                        (cost[i], nnf[i]) = (d, c);
                    }
                }
            }
            // Random search in a window that halves each try.
            let mut rad = l.w.max(l.h);
            while rad >= 1 {
                let c = (nnf[i].0 + rng.below(2 * rad + 1) - rad, nnf[i].1 + rng.below(2 * rad + 1) - rad);
                if valid(l, &ok, c.0, c.1) {
                    let d = dist(l, (x, y), c, cost[i]);
                    if d < cost[i] {
                        (cost[i], nnf[i]) = (d, c);
                    }
                }
                rad /= 2;
            }
        }
        // Vote: every hole pixel becomes the average of what overlapping matched patches put there.
        let mut acc = vec![[0.0f32; 4]; (l.w * l.h) as usize];
        for &(x, y) in &holes {
            let (sx, sy) = nnf[(y * l.w + x) as usize];
            for dy in -HALF..=HALF {
                for dx in -HALF..=HALF {
                    let (tx, ty) = (x + dx, y + dy);
                    if tx < 0 || ty < 0 || tx >= l.w || ty >= l.h || !l.hole[(ty * l.w + tx) as usize] {
                        continue;
                    }
                    let c = l.at(sx + dx, sy + dy);
                    let a = &mut acc[(ty * l.w + tx) as usize];
                    (0..3).for_each(|k| a[k] += c[k]);
                    a[3] += 1.0;
                }
            }
        }
        for &(x, y) in &holes {
            let i = (y * l.w + x) as usize;
            if acc[i][3] > 0.0 {
                l.px[i] = [0, 1, 2].map(|k| acc[i][k] / acc[i][3]);
            }
        }
    }
}

/// Fill the area within `r` pixels of the stroke `pts` (pixel positions in `img`) with texture from around it.
/// The edge is feathered into the original over the outer part of the radius.
pub fn fill(img: &mut Rgb32FImage, pts: &[[f32; 2]], r: f32, seed: u32) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for q in pts {
        lo = [lo[0].min(q[0]), lo[1].min(q[1])];
        hi = [hi[0].max(q[0]), hi[1].max(q[1])];
    }
    // The box: the area plus room around it to find texture in.
    let m = r * 3.0 + 8.0;
    let (bx0, by0) = (((lo[0] - m).floor() as i32).max(0), ((lo[1] - m).floor() as i32).max(0));
    let (bx1, by1) = (((hi[0] + m).ceil() as i32).min(iw), ((hi[1] + m).ceil() as i32).min(ih));
    let (w, h) = (bx1 - bx0, by1 - by0);
    if w < 4 * HALF || h < 4 * HALF {
        return;
    }
    // Every render runs the spots again (a slider moved after the heal, say): a fill of the same picture
    // area comes back from a small cache instead of running PatchMatch again (tens of ms per fill).
    let row = |y: i32| (((y * iw + bx0) * 3) as usize, (w * 3) as usize);
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        (iw, ih, bx0, by0, bx1, by1, seed, r.to_bits()).hash(&mut hs);
        pts.iter().for_each(|q| (q[0].to_bits(), q[1].to_bits()).hash(&mut hs));
        let raw: &[f32] = img;
        for y in by0..by1 {
            let (o, n) = row(y);
            raw[o..o + n].iter().for_each(|v| v.to_bits().hash(&mut hs));
        }
        hs.finish()
    };
    if let Some(done) = cache(key, None) {
        let raw: &mut [f32] = img;
        for (k, y) in (by0..by1).enumerate() {
            let (o, n) = row(y);
            raw[o..o + n].copy_from_slice(&done[k * n..(k + 1) * n]);
        }
        return;
    }
    let src = px3(img.as_raw());
    let near = |x: f32, y: f32| pts.iter().zip(pts.iter().skip(1).chain(std::iter::once(&pts[pts.len() - 1]))).map(|(a, b)| {
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let l2 = ex * ex + ey * ey;
        let t = if l2 > 0.0 { (((x - a[0]) * ex + (y - a[1]) * ey) / l2).clamp(0.0, 1.0) } else { 0.0 };
        ((x - a[0] - ex * t).powi(2) + (y - a[1] - ey * t).powi(2)).sqrt()
    }).fold(f32::MAX, f32::min);
    let mut px = Vec::with_capacity((w * h) as usize);
    let mut hole = Vec::with_capacity((w * h) as usize);
    let mut edge = Vec::with_capacity((w * h) as usize);
    for y in by0..by1 {
        for x in bx0..bx1 {
            px.push(src[(y * iw + x) as usize]);
            let d = near(x as f32, y as f32);
            // Fill a little past the radius, so the feather blends into filled texture, not the old spot.
            hole.push(d < r * 1.1);
            edge.push(1.0 - crate::develop::smooth(0.7 * r, 1.1 * r, d));
        }
    }
    let mut levels = vec![Level { w, h, px, hole }];
    while levels.last().is_some_and(|l| l.w.min(l.h) > 8 * HALF && l.hole.iter().filter(|v| **v).count() > 64) {
        let d = levels.last().unwrap().down();
        levels.push(d);
    }
    // Coarsest first: start the hole as the average of the picture around it.
    let mut rng = Rng(seed | 1);
    let top = levels.len() - 1;
    {
        let l = &mut levels[top];
        let (mut acc, mut n) = ([0.0f32; 3], 0.0f32);
        for (i, c) in l.px.iter().enumerate() {
            if !l.hole[i] {
                (0..3).for_each(|k| acc[k] += c[k]);
                n += 1.0;
            }
        }
        let mean = acc.map(|v| v / n.max(1.0));
        for i in 0..l.px.len() {
            if l.hole[i] {
                l.px[i] = mean;
            }
        }
    }
    let mut nnf = vec![(-1, -1); (levels[top].w * levels[top].h) as usize];
    for k in (0..=top).rev() {
        if k < top {
            // Up a level: take the hole's pixels and matches from the level above, doubled.
            let (fine, coarse) = levels.split_at_mut(k + 1);
            let (c, f) = (&coarse[0], &mut fine[k]);
            let mut up = vec![(-1, -1); (f.w * f.h) as usize];
            for y in 0..f.h {
                for x in 0..f.w {
                    let i = (y * f.w + x) as usize;
                    if f.hole[i] {
                        f.px[i] = c.at(x / 2, y / 2);
                        let m = nnf[((y / 2).min(c.h - 1) * c.w + (x / 2).min(c.w - 1)) as usize];
                        up[i] = (m.0 * 2 + x % 2, m.1 * 2 + y % 2);
                    }
                }
            }
            nnf = up;
        }
        patchmatch(&mut levels[k], &mut nnf, &mut rng, if k == 0 { 3 } else { 5 });
    }
    let l = &levels[0];
    let buf: &mut [f32] = img;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let a = edge[i];
            if a > 0.0 {
                let o = (((y + by0) * iw + x + bx0) * 3) as usize;
                for k in 0..3 {
                    buf[o + k] += (l.px[i][k] - buf[o + k]) * a;
                }
            }
        }
    }
    let raw: &[f32] = img;
    let done: Vec<f32> = (by0..by1).flat_map(|y| { let (o, n) = row(y); raw[o..o + n].to_vec() }).collect();
    cache(key, Some(done));
}

/// Recent fills by the hash of what went in: look one up (`put` None) or keep one. Holds about 32 MB at most.
fn cache(key: u64, put: Option<Vec<f32>>) -> Option<std::sync::Arc<Vec<f32>>> {
    use std::sync::{Arc, Mutex};
    static RECENT: Mutex<Vec<(u64, Arc<Vec<f32>>)>> = Mutex::new(Vec::new());
    let mut c = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    match put {
        None => c.iter().find(|e| e.0 == key).map(|e| e.1.clone()),
        Some(v) => {
            c.retain(|e| e.0 != key);
            c.insert(0, (key, Arc::new(v)));
            let mut total = 0;
            c.retain(|e| { total += e.1.len(); total <= 8_000_000 });
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spot on vertical stripes is filled with stripes that line up, not a grey blur.
    #[test]
    fn fills_with_matching_texture() {
        let (w, h) = (96u32, 96u32);
        let stripe = |x: u32| if (x / 4) % 2 == 0 { 0.8 } else { 0.1 };
        let mut img = Rgb32FImage::from_fn(w, h, |x, y| {
            let v = if (x as i32 - 48).pow(2) + (y as i32 - 48).pow(2) < 100 { 0.45 } else { stripe(x) };
            image::Rgb([v, v, v])
        });
        fill(&mut img, &[[48.0, 48.0]], 9.0, 7);
        let mut err = 0.0;
        for y in 44..53 {
            for x in 44..53 {
                err += (img.get_pixel(x, y).0[0] - stripe(x)).abs();
            }
        }
        // Grey left in place would be off by ~0.35 everywhere; matched stripes are close.
        assert!(err / 81.0 < 0.15, "mean error {}", err / 81.0);
    }

    #[test]
    fn the_same_fill_again_comes_from_the_cache() {
        let make = || Rgb32FImage::from_fn(200, 160, |x, y| image::Rgb([((x * 7 + y * 3) % 40) as f32 / 40.0, (y % 9) as f32 / 9.0, 0.3]));
        let (mut a, mut b) = (make(), make());
        fill(&mut a, &[[100.0, 60.0], [100.0, 110.0]], 8.0, 3);
        let t = std::time::Instant::now();
        fill(&mut b, &[[100.0, 60.0], [100.0, 110.0]], 8.0, 3);
        assert_eq!(a.as_raw(), b.as_raw(), "same picture, same fill");
        // A different picture underneath is filled afresh, not taken from the cache.
        let mut c = make();
        c.put_pixel(100, 30, image::Rgb([1.0, 0.0, 0.0]));
        fill(&mut c, &[[100.0, 60.0], [100.0, 110.0]], 8.0, 3);
        assert_eq!(c.get_pixel(100, 30).0, [1.0, 0.0, 0.0]);
        assert!(t.elapsed().as_secs() < 5);
    }
}

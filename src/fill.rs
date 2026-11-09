//! Content-aware fill for the Heal tool: rebuild an area from texture found around it.
//!
//! PatchMatch (Barnes et al. 2009) runs coarse to fine, with the parts of Image Melding (Darabi et al. 2012)
//! that matter for photos:
//! - patches are compared on brightness gradients as well as colour, and may be mirrored;
//! - every patch's vote is weighted by how well it matched (Newson et al. 2014), so one poor match doesn't smear;
//! - the result is blended in through its gradients (a Poisson solve), so it takes on the brightness at its edge
//!   instead of leaving a lighter or darker patch on a smooth sky.
//!
//! The hole starts "onion peel" style, grown in from its edge. Only a box around the area is touched, so it stays
//! fast enough for the live preview.
use crate::develop::px3;
use image::Rgb32FImage;

/// Half the patch size: 7×7 patches.
const HALF: i32 = 3;
/// How much brightness gradients count against colour when comparing patches.
const GRAD: f32 = 0.5;

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

/// A match: the centre of the source patch, and whether it is mirrored left to right.
#[derive(Clone, Copy)]
struct M {
    x: i32,
    y: i32,
    flip: bool,
}

impl M {
    const NONE: M = M { x: -1, y: -1, flip: false };

    /// Which way x runs in the source: 1, or -1 when mirrored.
    fn sx(&self) -> i32 {
        if self.flip { -1 } else { 1 }
    }
}

/// One level of the pyramid: the box's pixels, which of them are to be filled, and their brightness gradients.
struct Level {
    w: i32,
    h: i32,
    px: Vec<[f32; 3]>,
    hole: Vec<bool>,
    g: Vec<[f32; 2]>,
}

impl Level {
    fn new(w: i32, h: i32, px: Vec<[f32; 3]>, hole: Vec<bool>) -> Level {
        let mut l = Level { w, h, px, hole, g: vec![] };
        l.grads();
        l
    }

    fn idx(&self, x: i32, y: i32) -> usize {
        (y.clamp(0, self.h - 1) * self.w + x.clamp(0, self.w - 1)) as usize
    }

    fn at(&self, x: i32, y: i32) -> [f32; 3] {
        self.px[self.idx(x, y)]
    }

    fn is_hole(&self, x: i32, y: i32) -> bool {
        self.hole[self.idx(x, y)]
    }

    /// Brightness gradients (central differences), scaled for the patch distance. Run again when the hole changes.
    fn grads(&mut self) {
        let lum = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let mut g = vec![[0.0; 2]; self.px.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                g[(y * self.w + x) as usize] = [
                    (lum(self.at(x + 1, y)) - lum(self.at(x - 1, y))) * 0.5 * GRAD,
                    (lum(self.at(x, y + 1)) - lum(self.at(x, y - 1))) * 0.5 * GRAD,
                ];
            }
        }
        self.g = g;
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
        Level::new(w, h, px, hole)
    }
}

/// Patch distance between the patch at `a` (in the hole) and the source patch `b`: colour plus gradients.
/// Gives up early once it is worse than `best`.
fn dist(l: &Level, a: (i32, i32), b: M, best: f32) -> f32 {
    let s = b.sx();
    let mut d = 0.0;
    for dy in -HALF..=HALF {
        for dx in -HALF..=HALF {
            let (i, j) = (l.idx(a.0 + dx, a.1 + dy), l.idx(b.x + s * dx, b.y + dy));
            let (p, q, gp, gq) = (l.px[i], l.px[j], l.g[i], l.g[j]);
            d += (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)
                + (gp[0] - s as f32 * gq[0]).powi(2) + (gp[1] - gq[1]).powi(2);
        }
        if d > best {
            return d;
        }
    }
    d
}

/// A source patch centre must be inside the box and away from the hole, so it holds only real picture.
/// Mirroring keeps a patch in the same square, so this holds for both ways round.
fn valid(l: &Level, ok: &[bool], m: M) -> bool {
    m.x >= HALF && m.y >= HALF && m.x < l.w - HALF && m.y < l.h - HALF && ok[(m.y * l.w + m.x) as usize]
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

/// What the matched patches say each pixel should be: a weighted sum and the total weight, for the hole and for
/// the ring of real pixels the hole's patches reach over. A match counts less the worse it is.
/// With `best`, each pixel instead takes its value from the one best-matched patch over it (Newson et al.
/// 2014): averaging many patches blurs grain and fine texture, so the finest level's last pass copies.
fn vote(l: &Level, holes: &[(i32, i32)], nnf: &[M], cost: &[f32], best: bool) -> Vec<[f32; 4]> {
    // Scale the weights by a typical match cost (the 75th percentile), so they work at any brightness.
    let mut cs: Vec<f32> = holes.iter().map(|&(x, y)| cost[(y * l.w + x) as usize]).filter(|c| c.is_finite()).collect();
    let typical = if cs.is_empty() {
        1.0
    } else {
        let k = (cs.len() * 3 / 4).min(cs.len() - 1);
        *cs.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
    };
    let scale = 1.0 / (2.0 * typical + 1e-9);
    let mut acc = vec![[0.0f32; 4]; (l.w * l.h) as usize];
    if best {
        let mut low = vec![f32::INFINITY; (l.w * l.h) as usize];
        for &(x, y) in holes {
            let i = (y * l.w + x) as usize;
            let (m, s) = (nnf[i], nnf[i].sx());
            for dy in -HALF..=HALF {
                for dx in -HALF..=HALF {
                    let (tx, ty) = (x + dx, y + dy);
                    if tx < 0 || ty < 0 || tx >= l.w || ty >= l.h {
                        continue;
                    }
                    let t = (ty * l.w + tx) as usize;
                    if cost[i] < low[t] {
                        low[t] = cost[i];
                        let c = l.at(m.x + s * dx, m.y + dy);
                        acc[t] = [c[0], c[1], c[2], 1.0];
                    }
                }
            }
        }
        return acc;
    }
    for &(x, y) in holes {
        let i = (y * l.w + x) as usize;
        let m = nnf[i];
        let wgt = if cost[i].is_finite() { (-cost[i] * scale).exp().max(1e-6) } else { 1e-6 };
        let s = m.sx();
        for dy in -HALF..=HALF {
            for dx in -HALF..=HALF {
                let (tx, ty) = (x + dx, y + dy);
                if tx < 0 || ty < 0 || tx >= l.w || ty >= l.h {
                    continue;
                }
                let c = l.at(m.x + s * dx, m.y + dy);
                let a = &mut acc[(ty * l.w + tx) as usize];
                (0..3).for_each(|k| a[k] += c[k] * wgt);
                a[3] += wgt;
            }
        }
    }
    acc
}

/// Run PatchMatch on one level: improve the match `nnf` of every hole pixel, then rebuild the hole from the
/// matched patches. Returns the last vote and the hole pixels, for the blend.
fn patchmatch(l: &mut Level, nnf: &mut [M], rng: &mut Rng, iters: usize, finest: bool) -> Option<(Vec<[f32; 4]>, Vec<(i32, i32)>)> {
    let ok = sources(l);
    if !ok.iter().any(|v| *v) {
        return None;
    }
    let holes: Vec<(i32, i32)> = (0..l.h).flat_map(|y| (0..l.w).map(move |x| (x, y))).filter(|&(x, y)| l.hole[(y * l.w + x) as usize]).collect();
    // Any match that isn't a valid source (new pixels after upscaling) starts from a random one.
    for &(x, y) in &holes {
        let i = (y * l.w + x) as usize;
        if !valid(l, &ok, nnf[i]) {
            for _ in 0..200 {
                let c = M { x: rng.below(l.w), y: rng.below(l.h), flip: false };
                if valid(l, &ok, c) {
                    nnf[i] = c;
                    break;
                }
            }
        }
    }
    let mut cost: Vec<f32> = vec![f32::MAX; (l.w * l.h) as usize];
    let mut last = None;
    for it in 0..iters {
        for &(x, y) in &holes {
            let i = (y * l.w + x) as usize;
            cost[i] = if valid(l, &ok, nnf[i]) { dist(l, (x, y), nnf[i], f32::MAX) } else { f32::MAX };
        }
        // Alternate the scan direction, so good matches spread both ways.
        let order: Box<dyn Iterator<Item = &(i32, i32)>> = if it % 2 == 0 { Box::new(holes.iter()) } else { Box::new(holes.iter().rev()) };
        let step = if it % 2 == 0 { 1 } else { -1 };
        for &(x, y) in order {
            let i = (y * l.w + x) as usize;
            let try_it = |c: M, cost: &mut [f32], nnf: &mut [M]| {
                if valid(l, &ok, c) {
                    let d = dist(l, (x, y), c, cost[i]);
                    if d < cost[i] {
                        (cost[i], nnf[i]) = (d, c);
                    }
                }
            };
            // Propagation: the neighbour's match, shifted by one (the other way in a mirrored source).
            for (nx, ny) in [(x - step, y), (x, y - step)] {
                if nx < 0 || ny < 0 || nx >= l.w || ny >= l.h || !l.hole[(ny * l.w + nx) as usize] {
                    continue;
                }
                let m = nnf[(ny * l.w + nx) as usize];
                try_it(M { x: m.x + m.sx() * (x - nx), y: m.y + (y - ny), flip: m.flip }, &mut cost, nnf);
            }
            // Random search in a window that halves each try; now and then try the mirror image.
            let mut rad = l.w.max(l.h);
            while rad >= 1 {
                let cur = nnf[i];
                let flip = if rng.below(4) == 0 { !cur.flip } else { cur.flip };
                let c = M { x: cur.x + rng.below(2 * rad + 1) - rad, y: cur.y + rng.below(2 * rad + 1) - rad, flip };
                try_it(c, &mut cost, nnf);
                rad /= 2;
            }
        }
        let acc = vote(l, &holes, nnf, &cost, finest && it + 1 == iters);
        for &(x, y) in &holes {
            let i = (y * l.w + x) as usize;
            if acc[i][3] > 0.0 {
                l.px[i] = [0, 1, 2].map(|k| acc[i][k] / acc[i][3]);
            }
        }
        l.grads();
        last = Some(acc);
    }
    last.map(|a| (a, holes))
}

/// Fill the unknown values (`known` false) from the known ones, a ring at a time from the outside in: each new
/// value is the average of its known neighbours. Leaves values with no known pixel anywhere as they are.
fn onion(v: &mut [[f32; 3]], known: &mut [bool], w: i32, h: i32) {
    loop {
        let mut next = vec![];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if known[i] {
                    continue;
                }
                let (mut acc, mut n) = ([0.0f32; 3], 0.0f32);
                for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    let (xx, yy) = (x + dx, y + dy);
                    if xx >= 0 && yy >= 0 && xx < w && yy < h && known[(yy * w + xx) as usize] {
                        let c = v[(yy * w + xx) as usize];
                        (0..3).for_each(|k| acc[k] += c[k]);
                        n += 1.0;
                    }
                }
                if n > 0.0 {
                    next.push((i, acc.map(|a| a / n)));
                }
            }
        }
        if next.is_empty() {
            return;
        }
        for (i, c) in next {
            v[i] = c;
            known[i] = true;
        }
    }
}

/// Blend the fill in through its gradients: on the ring of real pixels round the hole, the difference between the
/// picture and what the fill says it should be there is spread smoothly over the hole (a Laplace solve) and added,
/// so the fill keeps its texture but meets the picture's brightness and colour all the way round.
fn blend(l: &mut Level, acc: &[[f32; 4]], holes: &[(i32, i32)], r: f32) {
    let n = (l.w * l.h) as usize;
    let mut c = vec![[0.0f32; 3]; n];
    let mut known = vec![false; n];
    for i in 0..n {
        if !l.hole[i] {
            known[i] = true;
            if acc[i][3] > 0.0 {
                c[i] = [0, 1, 2].map(|k| l.px[i][k] - acc[i][k] / acc[i][3]);
            }
        }
    }
    // Leave out ring pixels that disagree far more than the rest (the end of a stroke stopping on a lamp post or a
    // wire): spread into the hole, they would leave a smudge of that object's colour. A typical ring difference
    // is a tone shift; an outlier is a different thing.
    let size = |v: [f32; 3]| v[0].abs() + v[1].abs() + v[2].abs();
    let mut ring: Vec<f32> = (0..n).filter(|&i| !l.hole[i] && acc[i][3] > 0.0).map(|i| size(c[i])).collect();
    if !ring.is_empty() {
        let k = ring.len() / 2;
        let median = *ring.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1;
        let limit = 3.0 * median + 0.01;
        for i in 0..n {
            if !l.hole[i] && acc[i][3] > 0.0 && size(c[i]) > limit {
                (c[i], known[i]) = ([0.0; 3], false);
            }
        }
    }
    // The difference wanted is a tone shift, not grain: one pixel's difference is mostly grain or noise, and
    // spread into the hole it would leave blotches. So average it along the ring over a few pixels first.
    let ring: Vec<bool> = (0..n).map(|i| !l.hole[i] && acc[i][3] > 0.0).collect();
    let rad = ((r * 0.25) as i32).max(4);
    let (w, h) = (l.w as usize, l.h as usize);
    // Summed tables of the kept differences and of how many were kept.
    let mut sum = vec![[0.0f64; 4]; (w + 1) * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let v = if ring[i] && known[i] { [c[i][0] as f64, c[i][1] as f64, c[i][2] as f64, 1.0] } else { [0.0; 4] };
            let (a, b, d) = (sum[y * (w + 1) + x + 1], sum[(y + 1) * (w + 1) + x], sum[y * (w + 1) + x]);
            sum[(y + 1) * (w + 1) + x + 1] = [0, 1, 2, 3].map(|k| v[k] + a[k] + b[k] - d[k]);
        }
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !ring[i] {
                continue;
            }
            let (x0, y0) = ((x as i32 - rad).max(0) as usize, (y as i32 - rad).max(0) as usize);
            let (x1, y1) = ((x as i32 + rad + 1).min(l.w) as usize, (y as i32 + rad + 1).min(l.h) as usize);
            let at = |xx: usize, yy: usize| sum[yy * (w + 1) + xx];
            let t = [0, 1, 2, 3].map(|k| at(x1, y1)[k] - at(x0, y1)[k] - at(x1, y0)[k] + at(x0, y0)[k]);
            if t[3] > 0.5 {
                (c[i], known[i]) = ([0, 1, 2].map(|k| (t[k] / t[3]) as f32), true);
            }
        }
    }
    // Real pixels the fill never reached say nothing; only the ring next to the hole sets the edge.
    onion(&mut c, &mut known, l.w, l.h);
    // Smooth it to the harmonic solution with over-relaxed Gauss–Seidel; the onion start is already close.
    let iters = (r as usize).clamp(30, 400);
    for _ in 0..iters {
        for &(x, y) in holes {
            let i = (y * l.w + x) as usize;
            let nb = [l.idx(x - 1, y), l.idx(x + 1, y), l.idx(x, y - 1), l.idx(x, y + 1)];
            for k in 0..3 {
                let avg = nb.iter().map(|&j| c[j][k]).sum::<f32>() / 4.0;
                c[i][k] += 1.8 * (avg - c[i][k]);
            }
        }
    }
    for &(x, y) in holes {
        let i = (y * l.w + x) as usize;
        l.px[i] = [0, 1, 2].map(|k| (l.px[i][k] + c[i][k]).max(0.0));
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
    let m = r * 4.0 + 8.0;
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
    let mut levels = vec![Level::new(w, h, px, hole)];
    while levels.last().is_some_and(|l| l.w.min(l.h) > 8 * HALF && l.hole.iter().filter(|v| **v).count() > 64) {
        let d = levels.last().unwrap().down();
        levels.push(d);
    }
    // Coarsest first: grow the hole in from its edge, so it starts with the colours around each part of it.
    let mut rng = Rng(seed | 1);
    let top = levels.len() - 1;
    {
        let l = &mut levels[top];
        let mut known: Vec<bool> = l.hole.iter().map(|h| !h).collect();
        onion(&mut l.px, &mut known, l.w, l.h);
        l.grads();
    }
    let mut nnf = vec![M::NONE; (levels[top].w * levels[top].h) as usize];
    for k in (0..=top).rev() {
        if k < top {
            // Up a level: take the hole's pixels and matches from the level above, doubled.
            let (fine, coarse) = levels.split_at_mut(k + 1);
            let (c, f) = (&coarse[0], &mut fine[k]);
            let mut up = vec![M::NONE; (f.w * f.h) as usize];
            for y in 0..f.h {
                for x in 0..f.w {
                    let i = (y * f.w + x) as usize;
                    if f.hole[i] {
                        f.px[i] = c.at(x / 2, y / 2);
                        let m = nnf[((y / 2).min(c.h - 1) * c.w + (x / 2).min(c.w - 1)) as usize];
                        up[i] = M { x: m.x * 2 + (x % 2) * m.sx(), y: m.y * 2 + y % 2, flip: m.flip };
                    }
                }
            }
            f.grads();
            nnf = up;
        }
        let out = patchmatch(&mut levels[k], &mut nnf, &mut rng, if k == 0 { 3 } else { 5 }, k == 0);
        if k == 0 {
            if let Some((acc, holes)) = out {
                blend(&mut levels[0], &acc, &holes, r);
            }
        }
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

    /// A spot on a smooth sky ramp (with fine grain, as on film) comes back at the ramp's brightness, with no
    /// lighter or darker band where texture was copied from higher or lower in the sky.
    #[test]
    fn fills_a_gradient_without_a_band() {
        let (w, h) = (160u32, 160u32);
        let sky = |x: u32, y: u32| 0.2 + 0.6 * y as f32 / h as f32 + 0.01 * (((x * 7 + y * 13) % 5) as f32 - 2.0);
        let mut img = Rgb32FImage::from_fn(w, h, |x, y| {
            let v = if (x as i32 - 80).pow(2) + (y as i32 - 80).pow(2) < 13 * 13 { 0.05 } else { sky(x, y) };
            image::Rgb([v, v * 0.9, v * 1.1])
        });
        fill(&mut img, &[[80.0, 80.0]], 20.0, 11);
        // Mean error per row across the spot: a band shows as a row offset, grain averages out.
        let mut worst: f32 = 0.0;
        for y in 68..93 {
            let row: f32 = (68..93).map(|x| img.get_pixel(x, y).0[0] - sky(x, y)).sum::<f32>() / 25.0;
            worst = worst.max(row.abs());
        }
        assert!(worst < 0.012, "worst row offset {worst}");
    }

    /// On film grain the fill keeps the brightness and most of the grain: copying the best patch at the end
    /// instead of averaging keeps grain, and the edge correction only carries the tone, not single grains.
    #[test]
    fn keeps_grain_and_brightness() {
        let grain = |x: u32, y: u32, s: u32| {
            let mut h = x.wrapping_mul(374_761_393) ^ y.wrapping_mul(668_265_263) ^ s.wrapping_mul(2_246_822_519);
            h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
            h ^= h >> 16;
            (0..4).map(|k| ((h >> (8 * k)) & 0xff) as f32 / 255.0 - 0.5).sum::<f32>()
        };
        let stats = |img: &Rgb32FImage, a: u32, b: u32| {
            let v: Vec<f32> = (a..b).flat_map(|y| (a..b).map(move |x| (x, y))).map(|(x, y)| img.get_pixel(x, y).0[1]).collect();
            let m = v.iter().sum::<f32>() / v.len() as f32;
            (m, (v.iter().map(|q| (q - m).powi(2)).sum::<f32>() / v.len() as f32).sqrt())
        };
        let (mut off, mut kept) = (0.0, 0.0);
        for seed in 1..=6u32 {
            let mut img = Rgb32FImage::from_fn(200, 200, |x, y| {
                let v = 0.18 * (1.0 + 0.6 * grain(x, y, seed));
                image::Rgb([v, v, v])
            });
            let (m0, s0) = stats(&img, 70, 130);
            for y in 85..115 {
                for x in 85..115 {
                    img.put_pixel(x, y, image::Rgb([0.9, 0.9, 0.9]));
                }
            }
            fill(&mut img, &[[100.0, 100.0]], 24.0, seed);
            let (m1, s1) = stats(&img, 88, 112);
            (off, kept) = (off + (m1 / m0 - 1.0).abs() / 6.0, kept + s1 / s0 / 6.0);
        }
        // Before: 4% off on average (9% at worst) and 61% of the grain.
        assert!(off < 0.025, "brightness off by {off}");
        assert!(kept > 0.75, "grain kept {kept}");
    }

    /// A different variant gives a different fill of the same spot, and the same variant the same one.
    #[test]
    fn another_seed_gives_another_fill() {
        // Irregular texture: on an exactly repeating pattern every seed finds the same perfect matches.
        let n = |x: u32, y: u32| ((x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)).wrapping_mul(2_246_822_519) >> 24) as f32 / 255.0;
        let make = || Rgb32FImage::from_fn(120, 120, |x, y| image::Rgb([n(x / 2, y / 2), n(y, x) * 0.5, 0.4]));
        let (mut a, mut b) = (make(), make());
        fill(&mut a, &[[60.0, 60.0]], 10.0, 1);
        fill(&mut b, &[[60.0, 60.0]], 10.0, 1 ^ 0x9E37_79B9);
        assert_ne!(a.as_raw(), b.as_raw());
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

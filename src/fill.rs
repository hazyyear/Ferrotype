//! Content-aware fill for the Heal tool: rebuild an area from texture found around it.
//!
//! PatchMatch (Barnes et al. 2009) runs coarse to fine, with the parts of Image Melding (Darabi et al. 2012)
//! and later work that matter for photos:
//! - patches are compared on brightness gradients as well as colour, and may be mirrored;
//! - every patch's vote is weighted by how well it matched (Newson et al. 2014), and the last pass copies each
//!   pixel from its best patch instead of averaging, so grain survives; where two copied regions meet, the seam
//!   is softened with the average (spatial patch blending, Daisy et al. 2013);
//! - a source used over and over costs more (spatial uniformity, Kaspar et al. 2015), so big fills don't
//!   repeat one patch of grass or leaves;
//! - the result is blended in through its gradients (a Poisson solve), so it takes on the tone at its edge;
//! - on a wall or facade seen at an angle, the fill runs on a straightened copy of the plane (after Huang et
//!   al. 2014, one plane only), so windows and bricks keep their size across it.
//!
//! The hole starts "onion peel" style, grown in from its edge. Areas marked "don't sample" are never copied.
//! Only a box around the area is touched, and the search runs in bands across all cores.
use crate::develop::px3;
use image::Rgb32FImage;
use rayon::prelude::*;

/// Half the patch size: 7×7 patches.
const HALF: i32 = 3;
/// How much brightness gradients count against colour when comparing patches.
const GRAD: f32 = 0.5;
/// How much a source used more than its share costs, relative to a typical match.
const UNIFORM: f32 = 0.6;
/// The range of scales (relative to the hole centre) of the wall a perspective-aware fill copies from.
const USABLE: (f64, f64) = (0.85, 3.0);

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

/// One level of the pyramid: the box's pixels, which are to be filled, which may not be copied from, and the
/// brightness gradients.
struct Level {
    w: i32,
    h: i32,
    px: Vec<[f32; 3]>,
    hole: Vec<bool>,
    avoid: Vec<bool>,
    g: Vec<[f32; 2]>,
}

impl Level {
    fn new(w: i32, h: i32, px: Vec<[f32; 3]>, hole: Vec<bool>, avoid: Vec<bool>) -> Level {
        let mut l = Level { w, h, px, hole, avoid, g: vec![] };
        l.grads();
        l
    }

    fn idx(&self, x: i32, y: i32) -> usize {
        (y.clamp(0, self.h - 1) * self.w + x.clamp(0, self.w - 1)) as usize
    }

    fn at(&self, x: i32, y: i32) -> [f32; 3] {
        self.px[self.idx(x, y)]
    }

    /// Brightness gradients (central differences), scaled for the patch distance. Run again when the hole changes.
    fn grads(&mut self) {
        let lum = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let l = &*self;
        let g: Vec<[f32; 2]> = (0..l.h).into_par_iter().flat_map_iter(|y| (0..l.w).map(move |x| [
            (lum(l.at(x + 1, y)) - lum(l.at(x - 1, y))) * 0.5 * GRAD,
            (lum(l.at(x, y + 1)) - lum(l.at(x, y - 1))) * 0.5 * GRAD,
        ])).collect();
        self.g = g;
    }

    /// Half size: averages of 2×2 blocks; a block is a hole (or not to be copied) if any of it is.
    fn down(&self) -> Level {
        let (w, h) = ((self.w + 1) / 2, (self.h + 1) / 2);
        let n = (w * h) as usize;
        let (mut px, mut hole, mut avoid) = (vec![[0.0; 3]; n], vec![false; n], vec![false; n]);
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0; 3];
                let i = (y * w + x) as usize;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let j = self.idx(2 * x + dx, 2 * y + dy);
                    (0..3).for_each(|k| acc[k] += self.px[j][k] / 4.0);
                    hole[i] |= self.hole[j];
                    avoid[i] |= self.avoid[j];
                }
                px[i] = acc;
            }
        }
        Level::new(w, h, px, hole, avoid)
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

/// A source patch centre must be inside the box and its whole patch real picture that may be copied.
/// Mirroring keeps a patch in the same square, so this holds for both ways round.
fn valid(l: &Level, ok: &[bool], m: M) -> bool {
    m.x >= HALF && m.y >= HALF && m.x < l.w - HALF && m.y < l.h - HALF && ok[(m.y * l.w + m.x) as usize]
}

/// Where patches may come from: centres whose whole patch misses the hole and the "don't sample" areas.
fn sources(l: &Level) -> Vec<bool> {
    let mut ok = vec![true; (l.w * l.h) as usize];
    for y in 0..l.h {
        for x in 0..l.w {
            let i = (y * l.w + x) as usize;
            if l.hole[i] || l.avoid[i] {
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

/// A typical match cost: the 75th percentile over the hole.
fn typical(cost: &[f32], hole: &[bool]) -> f32 {
    let mut cs: Vec<f32> = cost.iter().zip(hole).filter(|(c, h)| **h && c.is_finite() && **c < f32::MAX).map(|(c, _)| *c).collect();
    if cs.is_empty() {
        return 1.0;
    }
    let k = (cs.len() * 3 / 4).min(cs.len() - 1);
    *cs.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
}

/// What the matched patches say each pixel should be, for the hole and for the ring of real pixels the hole's
/// patches reach over: the weighted average (colour, total weight; a match counts less the worse it is), and
/// the value from the single best patch over the pixel with where it came from (source x, y and mirroring).
struct Votes {
    mean: Vec<[f32; 4]>,
    best: Vec<[f32; 3]>,
    from: Vec<Option<(i32, i32, bool)>>,
}

fn vote(l: &Level, nnf: &[M], cost: &[f32]) -> Votes {
    let scale = 1.0 / (2.0 * typical(cost, &l.hole) + 1e-9);
    // Only pixels within a patch of the hole get votes: the hole's column span over each band of rows.
    let span: Vec<(i32, i32)> = (0..l.h).map(|y| {
        let row = &l.hole[(y * l.w) as usize..((y + 1) * l.w) as usize];
        match (row.iter().position(|v| *v), row.iter().rposition(|v| *v)) {
            (Some(a), Some(b)) => (a as i32, b as i32),
            _ => (i32::MAX, i32::MIN),
        }
    }).collect();
    let rows: Vec<Vec<([f32; 4], [f32; 3], Option<(i32, i32, bool)>)>> = (0..l.h).into_par_iter().map(|ty| {
        let (mut a, mut b) = (i32::MAX, i32::MIN);
        for y in (ty - HALF).max(0)..=(ty + HALF).min(l.h - 1) {
            (a, b) = (a.min(span[y as usize].0), b.max(span[y as usize].1));
        }
        (0..l.w).map(|tx| {
            if tx < a - HALF || tx > b + HALF {
                return ([0.0; 4], [0.0; 3], None);
            }
            let (mut mean, mut best, mut from, mut low) = ([0.0f32; 4], [0.0f32; 3], None, f32::INFINITY);
            for dy in -HALF..=HALF {
                for dx in -HALF..=HALF {
                    // The hole patch centred at q covers this pixel at offset (dx, dy).
                    let (qx, qy) = (tx - dx, ty - dy);
                    if qx < 0 || qy < 0 || qx >= l.w || qy >= l.h {
                        continue;
                    }
                    let q = (qy * l.w + qx) as usize;
                    if !l.hole[q] || nnf[q].x < 0 {
                        continue;
                    }
                    let m = nnf[q];
                    let (sx, sy) = (m.x + m.sx() * dx, m.y + dy);
                    let c = l.at(sx, sy);
                    let wgt = if cost[q] < f32::MAX { (-cost[q] * scale).exp().max(1e-6) } else { 1e-6 };
                    (0..3).for_each(|k| mean[k] += c[k] * wgt);
                    mean[3] += wgt;
                    if cost[q] < low {
                        (low, best, from) = (cost[q], c, Some((sx, sy, m.flip)));
                    }
                }
            }
            (mean, best, from)
        }).collect()
    }).collect();
    let mut v = Votes { mean: Vec::with_capacity(l.px.len()), best: Vec::with_capacity(l.px.len()), from: Vec::with_capacity(l.px.len()) };
    for (m, b, f) in rows.into_iter().flatten() {
        v.mean.push(m);
        v.best.push(b);
        v.from.push(f);
    }
    v
}

/// Copy each hole pixel from its best patch, and soften seams: where neighbouring pixels come from places in the
/// source that don't join up, the weighted average takes over, fading out over three pixels.
fn copy_with_seams(l: &mut Level, holes: &[(i32, i32)], v: &Votes) {
    let (w, h) = (l.w, l.h);
    let n = (w * h) as usize;
    // How much neighbouring pixels differ in the real picture around the hole (grain, fine texture): a change
    // of source is only a seam when the step it makes is clearly bigger than that.
    let step = |a: [f32; 3], b: [f32; 3]| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs();
    let mut usual: Vec<f32> = (0..n).filter(|&i| !l.hole[i] && v.mean[i][3] > 0.0 && (i as i32 % w) + 1 < w).map(|i| step(l.px[i], l.px[i + 1])).collect();
    let usual = if usual.is_empty() {
        0.0
    } else {
        let k = (usual.len() * 3 / 4).min(usual.len() - 1);
        *usual.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
    };
    let mut seam = vec![false; n];
    for &(x, y) in holes {
        let i = (y * w + x) as usize;
        let Some((sx, sy, f)) = v.from[i] else { continue };
        let s = if f { -1 } else { 1 };
        for (ex, ey) in [(1, 0), (0, 1)] {
            let (nx, ny) = (x + ex, y + ey);
            if nx >= w || ny >= h || !l.hole[(ny * w + nx) as usize] {
                continue;
            }
            let j = (ny * w + nx) as usize;
            if let Some((tx, ty, g)) = v.from[j]
                && (g != f || (tx - (sx + s * ex)).abs() > 1 || (ty - (sy + ey)).abs() > 1)
                && step(v.best[i], v.best[j]) > step(l.at(sx, sy), l.at(sx + s * ex, sy + ey)) + 2.0 * usual
            {
                (seam[i], seam[j]) = (true, true);
            }
        }
    }
    // Distance to the nearest seam, up to 3 px (two chamfer passes).
    let mut d = vec![9.0f32; n];
    for i in 0..n {
        if seam[i] {
            d[i] = 0.0;
        }
    }
    for pass in 0..2 {
        let ys: Box<dyn Iterator<Item = i32>> = if pass == 0 { Box::new(0..h) } else { Box::new((0..h).rev()) };
        for y in ys {
            let xs: Box<dyn Iterator<Item = i32>> = if pass == 0 { Box::new(0..w) } else { Box::new((0..w).rev()) };
            for x in xs {
                let i = (y * w + x) as usize;
                let st = if pass == 0 { -1 } else { 1 };
                for (dx, dy, c) in [(st, 0, 1.0), (0, st, 1.0), (st, st, 1.414), (-st, st, 1.414)] {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx >= 0 && ny >= 0 && nx < w && ny < h {
                        d[i] = d[i].min(d[(ny * w + nx) as usize] + c);
                    }
                }
            }
        }
    }
    for &(x, y) in holes {
        let i = (y * w + x) as usize;
        if v.from[i].is_none() {
            if v.mean[i][3] > 0.0 {
                l.px[i] = [0, 1, 2].map(|k| v.mean[i][k] / v.mean[i][3]);
            }
            continue;
        }
        let a = if d[i] < 3.5 && v.mean[i][3] > 0.0 { (-d[i] * d[i] / (2.0 * 1.2 * 1.2)).exp() } else { 0.0 };
        l.px[i] = [0, 1, 2].map(|k| {
            let m = if v.mean[i][3] > 0.0 { v.mean[i][k] / v.mean[i][3] } else { v.best[i][k] };
            v.best[i][k] + (m - v.best[i][k]) * a
        });
    }
}

/// Run PatchMatch on one level: improve the match `nnf` of every hole pixel, then rebuild the hole from the
/// matched patches. The search runs in bands of rows on all cores: a band sees its own new matches at once and
/// the other bands' as they were at the start of the pass. Returns the last votes and the hole pixels.
fn patchmatch(l: &mut Level, nnf: &mut [M], seed: u32, iters: usize, finest: bool) -> Option<(Votes, Vec<(i32, i32)>)> {
    let ok = sources(l);
    let n_ok = ok.iter().filter(|v| **v).count();
    if n_ok == 0 {
        return None;
    }
    let (w, h) = (l.w, l.h);
    let holes: Vec<(i32, i32)> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| l.hole[(y * w + x) as usize]).collect();
    // Any match that isn't a valid source (new pixels after upscaling) starts from a random one.
    let mut rng = Rng(seed.wrapping_mul(0x2545_F491) | 1);
    for &(x, y) in &holes {
        let i = (y * w + x) as usize;
        if !valid(l, &ok, nnf[i]) {
            for _ in 0..200 {
                let c = M { x: rng.below(w), y: rng.below(h), flip: false };
                if valid(l, &ok, c) {
                    nnf[i] = c;
                    break;
                }
            }
        }
    }
    // Bands depend only on the size, never on the number of cores, so a fill is the same on every machine.
    let band = (h / 64).max(8);
    let mut cost: Vec<f32> = vec![f32::MAX; (w * h) as usize];
    let mut last = None;
    for it in 0..iters {
        let snap: Vec<M> = nnf.to_vec();
        // How many hole pixels copy from each source pixel. Copying a region once uses each source once; more
        // than that is the same texture appearing twice.
        let used = if finest && it > 0 && UNIFORM > 0.0 {
            let mut count = vec![0u16; (w * h) as usize];
            for &(x, y) in &holes {
                let m = snap[(y * w + x) as usize];
                if m.x >= 0 {
                    let c = &mut count[(m.y * w + m.x) as usize];
                    *c = c.saturating_add(1);
                }
            }
            Some((count, typical(&cost, &l.hole)))
        } else {
            None
        };
        let lr = &*l;
        // A source already used by other hole pixels costs a share of a typical match per extra use.
        let penalty = |m: M, own: M| -> f32 {
            let Some((count, typ)) = &used else { return 0.0 };
            let mut c = count[(m.y * w + m.x) as usize] as f32;
            if own.x == m.x && own.y == m.y {
                c -= 1.0;
            }
            UNIFORM * typ * c.max(0.0)
        };
        let dir = if it % 2 == 0 { 1 } else { -1 };
        nnf.par_chunks_mut((w * band) as usize).zip(cost.par_chunks_mut((w * band) as usize)).enumerate().for_each(|(b, (nn, cc))| {
            let y0 = b as i32 * band;
            let rows = (nn.len() as i32) / w;
            let mut rng = Rng((seed ^ (it as u32).wrapping_mul(0x9E37_79B9) ^ (b as u32).wrapping_mul(0x85EB_CA6B)) | 1);
            let local = |x: i32, y: i32| ((y - y0) * w + x) as usize;
            // Cost of the current matches (with the reuse penalty).
            for y in y0..y0 + rows {
                for x in 0..w {
                    let i = local(x, y);
                    if lr.hole[(y * w + x) as usize] {
                        cc[i] = if valid(lr, &ok, nn[i]) { let p = penalty(nn[i], snap[(y * w + x) as usize]); dist(lr, (x, y), nn[i], f32::MAX) + p } else { f32::MAX };
                    }
                }
            }
            let ys: Vec<i32> = if dir > 0 { (y0..y0 + rows).collect() } else { (y0..y0 + rows).rev().collect() };
            for y in ys {
                let xs: Vec<i32> = if dir > 0 { (0..w).collect() } else { (0..w).rev().collect() };
                for x in xs {
                    if !lr.hole[(y * w + x) as usize] {
                        continue;
                    }
                    let i = local(x, y);
                    let own = snap[(y * w + x) as usize];
                    let try_it = |c: M, nn: &mut [M], cc: &mut [f32]| {
                        if valid(lr, &ok, c) {
                            let p = penalty(c, own);
                            if p < cc[i] {
                                let d = dist(lr, (x, y), c, cc[i] - p) + p;
                                if d < cc[i] {
                                    (cc[i], nn[i]) = (d, c);
                                }
                            }
                        }
                    };
                    // Propagation: the neighbour's match, shifted by one (the other way in a mirrored source).
                    for (nx, ny) in [(x - dir, y), (x, y - dir)] {
                        if nx < 0 || ny < 0 || nx >= w || ny >= h || !lr.hole[(ny * w + nx) as usize] {
                            continue;
                        }
                        let m = if ny >= y0 && ny < y0 + rows { nn[local(nx, ny)] } else { snap[(ny * w + nx) as usize] };
                        try_it(M { x: m.x + m.sx() * (x - nx), y: m.y + (y - ny), flip: m.flip }, nn, cc);
                    }
                    // Random search in a window that halves each try; now and then try the mirror image.
                    let mut rad = w.max(h);
                    while rad >= 1 {
                        let cur = nn[i];
                        let flip = if rng.below(4) == 0 { !cur.flip } else { cur.flip };
                        let c = M { x: cur.x + rng.below(2 * rad + 1) - rad, y: cur.y + rng.below(2 * rad + 1) - rad, flip };
                        try_it(c, nn, cc);
                        rad /= 2;
                    }
                }
            }
        });
        let v = vote(l, nnf, &cost);
        if finest && it + 1 == iters {
            copy_with_seams(l, &holes, &v);
        } else {
            for &(x, y) in &holes {
                let i = (y * w + x) as usize;
                if v.mean[i][3] > 0.0 {
                    l.px[i] = [0, 1, 2].map(|k| v.mean[i][k] / v.mean[i][3]);
                }
            }
        }
        l.grads();
        last = Some(v);
    }
    last.map(|v| (v, holes))
}
/// Fill the unknown values (`known` false) from the known ones, a ring at a time from the outside in: each new
/// value is the average of its known neighbours. Leaves values with no known pixel anywhere as they are.
/// Works ring by ring from a list of the unknown pixels next to known ones, so it costs the hole's size, not the
/// box's size times the number of rings.
fn onion(v: &mut [[f32; 3]], known: &mut [bool], w: i32, h: i32) {
    const N8: [(i32, i32); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];
    let unknown_next_to_known = |i: usize, known: &[bool]| {
        let (x, y) = (i as i32 % w, i as i32 / w);
        !known[i] && N8.iter().any(|&(dx, dy)| {
            let (xx, yy) = (x + dx, y + dy);
            xx >= 0 && yy >= 0 && xx < w && yy < h && known[(yy * w + xx) as usize]
        })
    };
    let mut front: Vec<usize> = (0..known.len()).filter(|&i| unknown_next_to_known(i, known)).collect();
    let mut queued = vec![false; known.len()];
    while !front.is_empty() {
        let next: Vec<(usize, [f32; 3])> = front.iter().map(|&i| {
            let (x, y) = (i as i32 % w, i as i32 / w);
            let (mut acc, mut n) = ([0.0f32; 3], 0.0f32);
            for (dx, dy) in N8 {
                let (xx, yy) = (x + dx, y + dy);
                if xx >= 0 && yy >= 0 && xx < w && yy < h && known[(yy * w + xx) as usize] {
                    let c = v[(yy * w + xx) as usize];
                    (0..3).for_each(|k| acc[k] += c[k]);
                    n += 1.0;
                }
            }
            (i, acc.map(|a| a / n.max(1.0)))
        }).collect();
        for &(i, c) in &next {
            v[i] = c;
            known[i] = true;
        }
        let mut f = vec![];
        for &(i, _) in &next {
            let (x, y) = (i as i32 % w, i as i32 / w);
            for (dx, dy) in N8 {
                let (xx, yy) = (x + dx, y + dy);
                if xx >= 0 && yy >= 0 && xx < w && yy < h {
                    let j = (yy * w + xx) as usize;
                    if !known[j] && !queued[j] {
                        queued[j] = true;
                        f.push(j);
                    }
                }
            }
        }
        front = f;
    }
}

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
    // An avoided area next to the hole is something to keep out (a person beside the sign): its colour must not
    // bleed in either, so it says nothing about the edge.
    for i in 0..n {
        if l.avoid[i] {
            (c[i], known[i]) = ([0.0; 3], false);
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
    let iters = ((r * 0.5) as usize).clamp(20, 200);
    // Red-black order: all "red" pixels (x + y even) only read "black" ones and the other way round, so each
    // half-step runs on all cores.
    let (w, h) = (l.w, l.h);
    let by_colour: [Vec<(i32, i32)>; 2] = [0, 1].map(|p| holes.iter().copied().filter(|&(x, y)| (x + y) % 2 == p).collect());
    for _ in 0..iters {
        for part in &by_colour {
            let cr = &c;
            let upd: Vec<[f32; 3]> = part.par_iter().map(|&(x, y)| {
                let i = (y * w + x) as usize;
                let nb = [l.idx(x - 1, y), l.idx(x + 1, y), l.idx(x, y - 1), l.idx(x, y + 1)];
                [0, 1, 2].map(|k| {
                    let avg = nb.iter().map(|&j| cr[j][k]).sum::<f32>() / 4.0;
                    cr[i][k] + 1.8 * (avg - cr[i][k])
                })
            }).collect();
            for (&(x, y), u) in part.iter().zip(upd) {
                c[(y * w + x) as usize] = u;
            }
        }
    }
    let _ = h;
    for &(x, y) in holes {
        let i = (y * l.w + x) as usize;
        l.px[i] = [0, 1, 2].map(|k| (l.px[i][k] + c[i][k]).max(0.0));
    }
}

/// Fill a box's hole: the coarse-to-fine PatchMatch, then the blend. Returns the box's pixels with the hole filled.
fn fill_core(px: Vec<[f32; 3]>, hole: Vec<bool>, avoid: Vec<bool>, w: i32, h: i32, r: f32, seed: u32) -> Vec<[f32; 3]> {
    let mut levels = vec![Level::new(w, h, px, hole, avoid)];
    while levels.last().is_some_and(|l| l.w.min(l.h) > 8 * HALF && l.hole.iter().filter(|v| **v).count() > 64) {
        let d = levels.last().unwrap().down();
        levels.push(d);
    }
    // Coarsest first: grow the hole in from its edge, so it starts with the colours around each part of it.
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
        let level_seed = seed ^ (k as u32).wrapping_mul(0xC2B2_AE35);
        let out = patchmatch(&mut levels[k], &mut nnf, level_seed, if k == 0 { 3 } else { 5 }, k == 0);
        if k == 0
            && let Some((v, holes)) = out
        {
            blend(&mut levels[0], &v.mean, &holes, r);
        }
    }
    levels.swap_remove(0).px
}

/// A wall or facade seen at an angle: the line its vanishing points lie on (a, b, c with a·x + b·y + c = 0, in
/// box pixels), scaled so that a·x + b·y + c is 1 at the hole's centre. Found from the straight edges around the
/// hole: two vanishing points, each backed by a good share of them. None for a flat-on view, too few lines, or
/// a plane so steep that straightening would stretch the box too far.
fn plane(px: &[[f32; 3]], hole: &[bool], w: i32, h: i32, centre: (f32, f32)) -> Option<[f64; 3]> {
    if w.min(h) < 96 {
        return None;
    }
    let edges = pieces(px, hole, w, h);
    if edges.len() < 16 {
        return None;
    }
    let line = |e: &Piece| {
        let (p, q) = ([e.x as f64, e.y as f64, 1.0], [(e.x + e.dx) as f64, (e.y + e.dy) as f64, 1.0]);
        cross(p, q)
    };
    // Does an edge point at the vanishing point v? Within 2 degrees.
    let agrees = |e: &Piece, v: [f64; 3]| {
        let (tx, ty) = (v[0] - e.x as f64 * v[2], v[1] - e.y as f64 * v[2]);
        let n = (tx * tx + ty * ty).sqrt();
        n > 1e-9 && ((tx * e.dy as f64 - ty * e.dx as f64) / n).abs() < 2f64.to_radians().sin()
    };
    let total: f32 = edges.iter().map(|e| e.w).sum();
    let mut left: Vec<&Piece> = edges.iter().collect();
    let mut vps = vec![];
    let mut rng = Rng(0x1234_5677);
    for _ in 0..2 {
        let mut best: Option<([f64; 3], f32)> = None;
        for _ in 0..300 {
            let (a, b) = (left[rng.below(left.len() as i32) as usize], left[rng.below(left.len() as i32) as usize]);
            let v = cross(line(a), line(b));
            if v.iter().all(|c| c.abs() < 1e-12) {
                continue;
            }
            let s: f32 = left.iter().filter(|e| agrees(e, v)).map(|e| e.w).sum();
            if best.is_none_or(|b| s > b.1) {
                best = Some((v, s));
            }
        }
        let (v, s) = best?;
        if s < 0.2 * total || left.iter().filter(|e| agrees(e, v)).count() < 8 {
            return None;
        }
        left.retain(|e| !agrees(e, v));
        vps.push(v);
        if left.len() < 8 {
            return None;
        }
    }
    let l = cross(vps[0], vps[1]);
    let at = |x: f64, y: f64| l[0] * x + l[1] * y + l[2];
    let c0 = at(centre.0 as f64, centre.1 as f64);
    if c0.abs() < 1e-12 {
        return None;
    }
    let l = [l[0] / c0, l[1] / c0, l[2] / c0];
    // Scale over the hole (1 at its centre): it must stay well away from the horizon.
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for y in 0..h {
        for x in 0..w {
            if hole[(y * w + x) as usize] {
                let v = l[0] * x as f64 + l[1] * y as f64 + l[2];
                (lo, hi) = (lo.min(v), hi.max(v));
            }
        }
    }
    // And over the usable part of the box (see `fill_on_plane`): worth it only when sizes really change there.
    let corners = [(0.0, 0.0), (w as f64, 0.0), (0.0, h as f64), (w as f64, h as f64)].map(|(x, y)| (l[0] * x + l[1] * y + l[2]).clamp(USABLE.0, USABLE.1));
    let (clo, chi) = corners.iter().fold((f64::MAX, f64::MIN), |a, &v| (a.0.min(v), a.1.max(v)));
    (lo > 0.5 && hi < 2.0 && chi / clo > 1.15).then_some(l)
}

/// A short straight piece of edge: where it is, which way it runs, and how strong it is.
struct Piece {
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    w: f32,
}

/// Straight edge pieces in the real picture of the box: 8×8 blocks whose gradients all point one way (high
/// coherence of the structure tensor), away from the hole. Short edges count too, like the sides of windows.
fn pieces(px: &[[f32; 3]], hole: &[bool], w: i32, h: i32) -> Vec<Piece> {
    const B: i32 = 8;
    // Square root of brightness: edges in the shadows count about as much as in the highlights.
    let lum: Vec<f32> = px.iter().map(|c| (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).max(0.0).sqrt()).collect();
    let at = |x: i32, y: i32| lum[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize];
    let mut out = vec![];
    for by in 0..h / B {
        for bx in 0..w / B {
            let (mut jxx, mut jxy, mut jyy, mut sw, mut cx, mut cy, mut near_hole) = (0f32, 0f32, 0f32, 0f32, 0f32, 0f32, false);
            for y in by * B..(by + 1) * B {
                for x in bx * B..(bx + 1) * B {
                    near_hole |= hole[(y * w + x) as usize];
                    let gx = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1));
                    let gy = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1)) - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1));
                    let m = gx * gx + gy * gy;
                    (jxx, jxy, jyy, sw, cx, cy) = (jxx + gx * gx, jxy + gx * gy, jyy + gy * gy, sw + m, cx + m * x as f32, cy + m * y as f32);
                }
            }
            let tr = jxx + jyy;
            if near_hole || tr <= 1e-6 {
                continue;
            }
            let coh = ((jxx - jyy).powi(2) + 4.0 * jxy * jxy).sqrt() / tr;
            if coh < 0.85 {
                continue;
            }
            let edge = 0.5 * (2.0 * jxy).atan2(jxx - jyy) + std::f32::consts::FRAC_PI_2;
            out.push(Piece { x: cx / sw, y: cy / sw, dx: edge.cos(), dy: edge.sin(), w: tr.sqrt() });
        }
    }
    // Faint blocks (sky, smooth wall) carry no line: keep the stronger ones.
    if let Some(top) = out.iter().map(|p| p.w).reduce(f32::max) {
        out.retain(|p| p.w > 0.1 * top);
    }
    out
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Bilinear sample of a w×h buffer at (x, y), clamped at the edges.
fn sample(px: &[[f32; 3]], w: i32, h: i32, x: f32, y: f32) -> [f32; 3] {
    let (x, y) = (x.clamp(0.0, (w - 1) as f32), y.clamp(0.0, (h - 1) as f32));
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let p = |xx: i32, yy: i32| px[(yy * w + xx) as usize];
    let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    [0, 1, 2].map(|k| (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy)
}

/// Fill on a straightened copy of the plane `l` (see `plane`): a box pixel p sits at p / s(p) there, with
/// s(p) = l·(p, 1), so things far away on the wall come out the same size as things near. Returns the box with
/// the hole filled, or None when the straightened copy would be too large.
fn fill_on_plane(px: &[[f32; 3]], hole: &[bool], avoid: &[bool], w: i32, h: i32, r: f32, seed: u32, l: [f64; 3], centre: (f64, f64)) -> Option<Vec<[f32; 3]>> {
    let s = |x: f64, y: f64| l[0] * x + l[1] * y + l[2];
    // p / s(p) makes the wall's lines parallel again (affine rectification) but squashes it unevenly; an affine
    // map then undoes that at the hole centre c, so the copy keeps the photo's resolution there:
    // q = J⁻¹·(p/s(p) − c) + c, with J = I − c·(l0, l1) the slope of p/s(p) at c (where s = 1).
    let (cx, cy) = centre;
    let j = [1.0 - cx * l[0], -cx * l[1], -cy * l[0], 1.0 - cy * l[1]];
    let det = j[0] * j[3] - j[1] * j[2];
    if det.abs() < 1e-9 {
        return None;
    }
    let ji = [j[3] / det, -j[1] / det, -j[2] / det, j[0] / det];
    let fwd = |x: f64, y: f64| {
        let (ax, ay) = (x / s(x, y) - cx, y / s(x, y) - cy);
        (ji[0] * ax + ji[1] * ay + cx, ji[2] * ax + ji[3] * ay + cy)
    };
    // Only the part of the box no further off than the hole (and not hugely nearer) is straightened: a farther
    // part comes out enlarged, so blurred, and copying from it would blur the fill.
    let usable = |x: f64, y: f64| (USABLE.0..=USABLE.1).contains(&s(x, y));
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for y in (0..=h).step_by(4) {
        for x in (0..=w).step_by(4) {
            let (x, y) = (x.min(w - 1) as f64, y.min(h - 1) as f64);
            if usable(x, y) {
                let (qx, qy) = fwd(x, y);
                (x0, y0, x1, y1) = (x0.min(qx), y0.min(qy), x1.max(qx), y1.max(qy));
            }
        }
    }
    if x0 > x1 {
        return None;
    }
    let (rw, rh) = ((x1 - x0).ceil() as i32 + 1, (y1 - y0).ceil() as i32 + 1);
    if rw < 4 * HALF || rh < 4 * HALF || (rw as i64 * rh as i64) > 4 * (w as i64 * h as i64) {
        return None;
    }
    // Back from the straightened copy: undo the affine map, then p = l2·a / (1 − l0·ax − l1·ay) (from p = a·s(p)).
    let back = |qx: f64, qy: f64| {
        let (dx, dy) = (qx - cx, qy - cy);
        let (ax, ay) = (j[0] * dx + j[1] * dy + cx, j[2] * dx + j[3] * dy + cy);
        let d = 1.0 - l[0] * ax - l[1] * ay;
        (l[2] * ax / d, l[2] * ay / d)
    };
    let n = (rw * rh) as usize;
    let (mut rpx, mut rhole, mut ravoid) = (vec![[0.0f32; 3]; n], vec![false; n], vec![false; n]);
    for y in 0..rh {
        for x in 0..rw {
            let i = (y * rw + x) as usize;
            let (px_, py_) = back(x as f64 + x0, y as f64 + y0);
            if !px_.is_finite() || !py_.is_finite() || px_ < 0.0 || py_ < 0.0 || px_ > (w - 1) as f64 || py_ > (h - 1) as f64 || !usable(px_, py_) {
                // Outside the box: nothing to copy from here.
                ravoid[i] = true;
                continue;
            }
            rpx[i] = sample(px, w, h, px_ as f32, py_ as f32);
            // A pixel blended from any hole pixel carries the old spot's colour: it is part of the hole.
            let (fx, fy) = (px_.floor() as i32, py_.floor() as i32);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let j = ((fy + dy).min(h - 1) * w + (fx + dx).min(w - 1)) as usize;
                rhole[i] |= hole[j];
                ravoid[i] |= avoid[j];
            }
            ravoid[i] &= !rhole[i];
        }
    }
    let done = fill_core(rpx, rhole, ravoid, rw, rh, r, seed);
    let mut out = px.to_vec();
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if hole[i] {
                let (qx, qy) = fwd(x as f64, y as f64);
                out[i] = sample(&done, rw, rh, (qx - x0) as f32, (qy - y0) as f32);
            }
        }
    }
    Some(out)
}

/// Distance from (x, y) to a stroke (a polyline of points; one point is a round spot).
fn near(pts: &[[f32; 2]], x: f32, y: f32) -> f32 {
    pts.iter().zip(pts.iter().skip(1).chain(std::iter::once(&pts[pts.len() - 1]))).map(|(a, b)| {
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let l2 = ex * ex + ey * ey;
        let t = if l2 > 0.0 { (((x - a[0]) * ex + (y - a[1]) * ey) / l2).clamp(0.0, 1.0) } else { 0.0 };
        ((x - a[0] - ex * t).powi(2) + (y - a[1] - ey * t).powi(2)).sqrt()
    }).fold(f32::MAX, f32::min)
}

/// Fill the area within `r` pixels of the stroke `pts` (pixel positions in `img`) with texture from around it.
/// The edge is feathered into the original over the outer part of the radius.
#[cfg(test)]
pub fn fill(img: &mut Rgb32FImage, pts: &[[f32; 2]], r: f32, seed: u32) {
    fill_avoiding(img, pts, r, seed, &[]);
}

/// `fill`, never copying from the areas in `avoid` (strokes with their radius, in pixels of `img`).
pub fn fill_avoiding(img: &mut Rgb32FImage, pts: &[[f32; 2]], r: f32, seed: u32, avoid: &[(Vec<[f32; 2]>, f32)]) {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    if pts.is_empty() {
        return;
    }
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
    // area comes back from a small cache instead of running PatchMatch again.
    let row = |y: i32| (((y * iw + bx0) * 3) as usize, (w * 3) as usize);
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        (iw, ih, bx0, by0, bx1, by1, seed, r.to_bits()).hash(&mut hs);
        pts.iter().for_each(|q| (q[0].to_bits(), q[1].to_bits()).hash(&mut hs));
        for (a, ar) in avoid {
            ar.to_bits().hash(&mut hs);
            a.iter().for_each(|q| (q[0].to_bits(), q[1].to_bits()).hash(&mut hs));
        }
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
    let n = (w * h) as usize;
    let (mut px, mut hole, mut edge, mut no) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    let (mut cx, mut cy, mut cn) = (0.0f32, 0.0f32, 0.0f32);
    for y in by0..by1 {
        for x in bx0..bx1 {
            px.push(src[(y * iw + x) as usize]);
            let d = near(pts, x as f32, y as f32);
            // Fill a little past the radius, so the feather blends into filled texture, not the old spot.
            let inside = d < r * 1.1;
            if inside {
                (cx, cy, cn) = (cx + (x - bx0) as f32, cy + (y - by0) as f32, cn + 1.0);
            }
            hole.push(inside);
            edge.push(1.0 - crate::develop::smooth(0.7 * r, 1.1 * r, d));
            no.push(!inside && avoid.iter().any(|(a, ar)| !a.is_empty() && near(a, x as f32, y as f32) < *ar));
        }
    }
    if cn == 0.0 {
        return;
    }
    let centre = (cx / cn, cy / cn);
    let done = plane(&px, &hole, w, h, centre)
        .and_then(|l| fill_on_plane(&px, &hole, &no, w, h, r, seed, l, (centre.0 as f64, centre.1 as f64)))
        .unwrap_or_else(|| fill_core(px, hole, no, w, h, r, seed));
    let buf: &mut [f32] = img;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let a = edge[i];
            if a > 0.0 {
                let o = (((y + by0) * iw + x + bx0) * 3) as usize;
                for k in 0..3 {
                    buf[o + k] += (done[i][k] - buf[o + k]) * a;
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

    #[test]
    fn never_copies_from_an_avoided_area() {
        // Grey texture with a red band starting in the middle of the hole (a person beside a sign): marked as
        // avoid, none of the red is copied into the fill or bleeds in from its edge.
        let n = |x: u32, y: u32| ((x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)).wrapping_mul(2_246_822_519) >> 24) as f32 / 255.0;
        let mut img = Rgb32FImage::from_fn(200, 160, |x, y| {
            let v = 0.4 + 0.2 * n(x / 2, y / 2);
            image::Rgb(if (112..150).contains(&x) { [0.9, v * 0.3, v * 0.3] } else { [v, v, v] })
        });
        let avoid = vec![(vec![[131.0, 0.0], [131.0, 160.0]], 20.0)];
        fill_avoiding(&mut img, &[[112.0, 80.0]], 12.0, 1, &avoid);
        // Inside the feathered edge (0.7 of the radius), which fades back to the photo.
        for y in 72..89 {
            for x in 104..121 {
                if (x as f32 - 112.0).hypot(y as f32 - 80.0) > 8.0 {
                    continue;
                }
                let c = img.get_pixel(x, y).0;
                assert!(c[0] - c[1] < 0.15, "red copied in at {x},{y}: {c:?}");
            }
        }
    }

    #[test]
    fn fills_a_wall_seen_at_an_angle_in_perspective() {
        // A wall of windows seen at an angle (each pixel averages 4×4 samples of the scene, like a camera):
        // the windows get smaller to the left. Filled on the straightened wall, the windows behind the hole
        // come back the right size and in the right place, far closer than a flat fill gets.
        let (w, h) = (480i32, 360i32);
        let pat = |u: f32, v: f32| {
            let (fu, fv) = ((u / 28.0).rem_euclid(1.0), (v / 28.0).rem_euclid(1.0));
            if fu > 0.25 && fu < 0.75 && fv > 0.2 && fv < 0.8 { 0.12 } else { 0.6 }
        };
        let scene = |x: f32, y: f32| {
            let s = 1.0 + 0.0035 * (x - 240.0);
            pat((x - 240.0) / s + 240.0, (y - 180.0) / s)
        };
        let pixel = |x: f32, y: f32| (0..16).map(|k| scene(x - 0.375 + (k % 4) as f32 * 0.25, y - 0.375 + (k / 4) as f32 * 0.25)).sum::<f32>() / 16.0;
        let (mut px, mut hole, mut truth) = (vec![], vec![], vec![]);
        for y in 0..h {
            for x in 0..w {
                let v = pixel(x as f32, y as f32);
                let inside = (x as f32 - 250.0).hypot(y as f32 - 175.0) < 40.0;
                truth.push(v);
                hole.push(inside);
                px.push(if inside { [0.9, 0.2, 0.2] } else { [v, v, v] });
            }
        }
        let err = |out: &[[f32; 3]]| {
            let (e, n) = (0..out.len()).filter(|&i| hole[i]).fold((0.0, 0.0), |(e, n), i| (e + (out[i][0] - truth[i]).abs(), n + 1.0));
            e / n
        };
        let no = vec![false; px.len()];
        let l = plane(&px, &hole, w, h, (250.0, 175.0)).expect("finds the wall's plane");
        let flat = err(&fill_core(px.clone(), hole.clone(), no.clone(), w, h, 36.0, 3));
        let straight = err(&fill_on_plane(&px, &hole, &no, w, h, 36.0, 3, l, (250.0, 175.0)).expect("straightens it"));
        assert!(straight < 0.6 * flat, "straightened {straight:.3} against flat {flat:.3}");
        // A flat-on view of the same wall has no vanishing line: the plain fill is used.
        let flat_on: Vec<[f32; 3]> = (0..w * h).map(|i| { let v = pat((i % w) as f32, (i / w) as f32); [v, v, v] }).collect();
        assert!(plane(&flat_on, &hole, w, h, (250.0, 175.0)).is_none());
    }
}

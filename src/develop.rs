//! Develop settings and the render pipeline. Works on linear sRGB-primaries f32, outputs sRGB-encoded f32 in 0..1.
use crate::load::linear_to_srgb;
#[cfg(test)]
use crate::load::srgb_to_linear;
use crate::negative::{self, NegParams};
use image::Rgb32FImage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    // Basic
    pub temp: f32, // kelvin, 5500 = as shot
    pub tint: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub vibrance: f32,
    pub saturation: f32,
    // Tone curve (parametric regions)
    pub c_hi: f32,
    pub c_li: f32,
    pub c_da: f32,
    pub c_sh: f32,
    // Color mixer: [hue, sat, lum] for red, orange, yellow, green, aqua, blue, purple, magenta
    pub hsl: [[f32; 3]; 8],
    // Color grading: [hue 0..360, sat, lum] for shadows, midtones, highlights, global
    pub grade: [[f32; 3]; 4],
    pub g_blend: f32,
    pub g_bal: f32,
    // Detail
    pub sh_amt: f32,
    pub sh_rad: f32,
    pub sh_det: f32,
    pub sh_mask: f32,
    pub nr_lum: f32,
    pub nr_det: f32,
    pub nr_col: f32,
    // Lens corrections (manual)
    pub o_dist: f32,
    pub o_vig: f32,
    pub o_fringe: f32,
    // Transform + crop
    pub t_vert: f32,
    pub t_hor: f32,
    pub t_rot: f32,
    pub t_scale: f32,
    pub flip_h: bool,
    pub crop: [f32; 4], // fraction trimmed from left, top, right, bottom
    // Effects
    pub vig: f32,
    pub vig_mid: f32,
    pub vig_round: f32,
    pub vig_fea: f32,
    pub vig_hi: f32,
    pub gr_amt: f32,
    pub gr_size: f32,
    pub gr_rough: f32,
    /// How much grain is left in the highlights, 0–100 (film prints have clean highlights).
    pub gr_hi: f32,
    // Halation: red glow around bright highlights, as on film without an anti-halation layer
    pub hal_amt: f32,
    pub hal_size: f32,
    pub hal_thr: f32,
    /// Point curves (x, y in 0..1) for RGB together, then red, green, blue. Empty = straight line.
    pub curves: [Vec<[f32; 2]>; 4],
    /// The RGB point curve changes brightness only, leaving colours as they are (Luminosity mode).
    pub curve_lum: bool,
    /// A .cube colour look applied after the tone and colour work, and how strongly (0..100).
    pub lut: String,
    pub lut_amt: f32,
    // Library: 0..5 stars; flag 1 = pick, -1 = reject. Per photo: never copied by paste or sync.
    pub rating: u8,
    pub flag: i8,
    // Calibration: shadows tint, then [hue, sat] for red, green, blue primaries
    pub cal_sh: f32,
    pub cal: [[f32; 2]; 3],
    pub neg: NegParams,
    pub border: Border,
    /// Healed spots (dust, sensor spots). Per photo: never copied by paste or sync.
    pub spots: Vec<Spot>,
    /// Old-style graduated filters and brush masks: read from older edits and turned into `masks` (see `migrate`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub grads: Vec<GradFilter>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub brushes: Vec<BrushMask>,
    /// Local adjustments: each mask is a shape (or several combined) with its own settings.
    pub masks: Vec<Mask>,
    /// Quarter turns clockwise, applied before everything else in geometry.
    pub rot90: u8,
    /// Colour label: 0 none, 1 red, 2 yellow, 3 green, 4 blue, 5 purple (Lightroom's set).
    pub label: u8,
    /// AI denoise strength, 0–100 (needs the optional model).
    pub ai_nr: f32,
    /// Named versions of these settings to come back to.
    pub snapshots: Vec<Snapshot>,
    /// Lens profile from the lensfun database ("" = off), with the shot's focal length, f-number and camera crop factor.
    pub lens: String,
    pub lens_focal: f32,
    pub lens_f: f32,
    pub lens_crop: f32,
    /// How much of the profile's distortion and vignetting correction to apply, 0–200.
    pub lens_dist: f32,
    pub lens_vig: f32,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Snapshot {
    pub name: String,
    pub params: Params,
}

/// A painted mask: soft round dabs [x, y, radius, +1 paint / -1 erase] in frame fractions, like spots.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct BrushMask {
    pub dabs: Vec<[f32; 4]>,
    pub exposure: f32,
    pub temp: f32,
    pub tint: f32,
    pub saturation: f32,
}

/// Positions are fractions of the uncropped frame (after rotation and perspective), so cropping later doesn't move them.
/// When the geometry changes they are moved along with the photo (`remap_marks`).
#[derive(Clone, PartialEq, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
    /// Radius as a fraction of the frame's long side.
    pub r: f32,
    /// Where the replacement texture is taken from (the source for the point x, y).
    pub sx: f32,
    pub sy: f32,
    /// A painted stroke: more points after (x, y). Empty = a round spot.
    pub path: Vec<[f32; 2]>,
    /// Clone copies the source as it is; heal (the default) matches it to the colour and brightness around it.
    pub clone: bool,
    /// Turn the source by this many degrees.
    pub angle: f32,
}

impl Spot {
    /// Every point of the spot or stroke, starting with (x, y).
    pub fn points(&self) -> impl Iterator<Item = [f32; 2]> + '_ {
        std::iter::once([self.x, self.y]).chain(self.path.iter().copied())
    }
}

/// One shape of a mask. Positions are frame fractions (like spots); sizes are fractions of the frame's long side.
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub enum Shape {
    /// Full effect on the `a` side of the line, fading to none at `b`.
    Linear { a: [f32; 2], b: [f32; 2] },
    /// An ellipse: full effect inside, fading out over the last `feather` % of its radius.
    Radial { c: [f32; 2], rx: f32, ry: f32, angle: f32, feather: f32 },
    /// Painted: soft round dabs [x, y, radius, +1 paint / -1 erase].
    Brush { dabs: Vec<[f32; 4]> },
    /// Brightness range (0–1 of the edited photo), with soft edges.
    Luminance { lo: f32, hi: f32, soft: f32 },
    /// Colours close to `rgb` (display values, picked from the photo); `range` 0–100.
    Color { rgb: [f32; 3], range: f32 },
    /// The whole photo (to subtract other shapes from).
    All,
}

impl Shape {
    pub fn name(&self) -> &'static str {
        match self {
            Shape::Linear { .. } => "Linear gradient",
            Shape::Radial { .. } => "Radial gradient",
            Shape::Brush { .. } => "Brush",
            Shape::Luminance { .. } => "Luminance range",
            Shape::Color { .. } => "Colour range",
            Shape::All => "Whole photo",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub enum MaskOp {
    #[default]
    Add,
    Subtract,
    Intersect,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct MaskPart {
    pub shape: Shape,
    pub op: MaskOp,
    #[serde(default)]
    pub invert: bool,
}

/// A local adjustment: shapes combined (add, subtract, intersect), softened and faded, with its own settings.
/// `adj` holds the settings as changes from the defaults (Exposure +1 here means one stop brighter than the
/// rest of the photo); see `Params::with_mask`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mask {
    pub name: String,
    pub parts: Vec<MaskPart>,
    /// 0–100.
    pub opacity: f32,
    /// Extra softening of the finished mask, 0–100.
    pub feather: f32,
    pub invert: bool,
    pub hidden: bool,
    #[serde(with = "sparse")]
    pub adj: Params,
}

impl Default for Mask {
    fn default() -> Self {
        Mask { name: String::new(), parts: vec![], opacity: 100.0, feather: 0.0, invert: false, hidden: false, adj: Params::default() }
    }
}

/// A mask's settings are saved as just what differs from the defaults.
mod sparse {
    use super::Params;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(p: &Params, s: S) -> Result<S::Ok, S::Error> {
        let (Ok(serde_json::Value::Object(mut m)), Ok(serde_json::Value::Object(d))) = (serde_json::to_value(p), serde_json::to_value(Params::default())) else {
            return p.serialize(s);
        };
        m.retain(|k, v| d.get(k) != Some(v));
        m.serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Params, D::Error> {
        Params::deserialize(d)
    }
}

/// Old-style graduated filter: full effect on the `a` side of the line, fading to none at `b`.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct GradFilter {
    pub a: [f32; 2],
    pub b: [f32; 2],
    pub exposure: f32,
    pub temp: f32,
    pub tint: f32,
    pub saturation: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            temp: 5500.0, tint: 0.0, exposure: 0.0, contrast: 0.0, highlights: 0.0, shadows: 0.0, whites: 0.0, blacks: 0.0,
            texture: 0.0, clarity: 0.0, dehaze: 0.0, vibrance: 0.0, saturation: 0.0,
            c_hi: 0.0, c_li: 0.0, c_da: 0.0, c_sh: 0.0,
            hsl: [[0.0; 3]; 8],
            grade: [[0.0; 3]; 4], g_blend: 50.0, g_bal: 0.0,
            sh_amt: 40.0, sh_rad: 1.0, sh_det: 25.0, sh_mask: 0.0, nr_lum: 0.0, nr_det: 50.0, nr_col: 25.0,
            o_dist: 0.0, o_vig: 0.0, o_fringe: 0.0,
            t_vert: 0.0, t_hor: 0.0, t_rot: 0.0, t_scale: 100.0, flip_h: false, crop: [0.0; 4],
            vig: 0.0, vig_mid: 50.0, vig_round: 0.0, vig_fea: 50.0, vig_hi: 0.0,
            gr_amt: 0.0, gr_size: 25.0, gr_rough: 50.0, gr_hi: 25.0, hal_amt: 0.0, hal_size: 40.0, hal_thr: 50.0,
            curves: Default::default(), curve_lum: false, lut: String::new(), lut_amt: 100.0, rating: 0, flag: 0,
            cal_sh: 0.0, cal: [[0.0; 2]; 3],
            neg: NegParams::default(),
            border: Border::default(),
            spots: vec![],
            grads: vec![],
            brushes: vec![],
            masks: vec![],
            rot90: 0, label: 0, ai_nr: 0.0, snapshots: vec![],
            lens: String::new(), lens_focal: 0.0, lens_f: 0.0, lens_crop: 1.0, lens_dist: 100.0, lens_vig: 100.0,
        }
    }
}

impl Params {
    /// Copy the look but keep this photo's geometry and negative conversion (what paste, sync and presets should do).
    /// "Apply to roll" is the one place that copies the conversion.
    pub fn with_look_of(&self, other: &Params) -> Params {
        let mut p = other.clone();
        (p.t_vert, p.t_hor, p.t_rot, p.t_scale, p.flip_h, p.crop) = (self.t_vert, self.t_hor, self.t_rot, self.t_scale, self.flip_h, self.crop);
        p.neg = self.neg.clone();
        p.spots = self.spots.clone();
        (p.rating, p.flag, p.label, p.rot90) = (self.rating, self.flag, self.label, self.rot90);
        p.snapshots = self.snapshots.clone();
        // The lens profile belongs to the photo, like its geometry.
        (p.lens, p.lens_focal, p.lens_f, p.lens_crop, p.lens_dist, p.lens_vig) =
            (self.lens.clone(), self.lens_focal, self.lens_f, self.lens_crop, self.lens_dist, self.lens_vig);
        p
    }

    /// Part-way from `from` to `to` (0 = from, 1 = to, up to 2 to exaggerate): every number is mixed, including
    /// the settings inside masks; switches, names and lists of different lengths change at the half-way point.
    /// Used by the preset Amount slider.
    pub fn blend(from: &Params, to: &Params, t: f32) -> Params {
        use serde_json::Value;
        fn mix(a: &Value, b: &Value, t: f64) -> Value {
            match (a, b) {
                (Value::Number(x), Value::Number(y)) => {
                    let (x0, y0) = (x.as_f64().unwrap_or(0.0), y.as_f64().unwrap_or(0.0));
                    let v = x0 + (y0 - x0) * t;
                    if x.is_f64() || y.is_f64() {
                        serde_json::json!(v)
                    } else if x.is_u64() && y.is_u64() {
                        serde_json::json!(v.round().max(0.0) as u64)
                    } else {
                        serde_json::json!(v.round() as i64)
                    }
                }
                (Value::Array(x), Value::Array(y)) if x.len() == y.len() => Value::Array(x.iter().zip(y).map(|(u, v)| mix(u, v, t)).collect()),
                (Value::Object(x), Value::Object(y)) => Value::Object(y.iter().map(|(k, v)| (k.clone(), x.get(k).map_or(v.clone(), |u| mix(u, v, t)))).collect()),
                _ => if t >= 0.5 { b.clone() } else { a.clone() },
            }
        }
        let (Ok(a), Ok(b)) = (serde_json::to_value(from), serde_json::to_value(to)) else { return to.clone() };
        let mut p: Params = serde_json::from_value(mix(&a, &b, t as f64)).unwrap_or_else(|_| to.clone());
        // Ratings and the like are never part of a look.
        (p.rating, p.flag, p.label) = (from.rating, from.flag, from.label);
        p
    }

    /// Turn old graduated filters and brush masks into masks (same look, now with every setting available).
    pub fn migrate(&mut self) {
        let old = |exposure: f32, temp: f32, tint: f32, saturation: f32| Params { exposure, temp: 5500.0 + temp * 30.0, tint, saturation, ..Default::default() };
        for (i, g) in std::mem::take(&mut self.grads).into_iter().enumerate() {
            self.masks.push(Mask { name: format!("Gradient {}", i + 1), parts: vec![MaskPart { shape: Shape::Linear { a: g.a, b: g.b }, op: MaskOp::Add, invert: false }], adj: old(g.exposure, g.temp, g.tint, g.saturation), ..Default::default() });
        }
        for (i, b) in std::mem::take(&mut self.brushes).into_iter().enumerate() {
            self.masks.push(Mask { name: format!("Brush {}", i + 1), parts: vec![MaskPart { shape: Shape::Brush { dabs: b.dabs }, op: MaskOp::Add, invert: false }], adj: old(b.exposure, b.temp, b.tint, b.saturation), ..Default::default() });
        }
    }

    /// These settings with a mask's changes added: what the photo looks like where the mask is fully on.
    pub fn with_mask(&self, a: &Params) -> Params {
        let d = Params::default();
        let mut q = Params { masks: vec![], ..self.clone() };
        macro_rules! add {
            ($($f:ident),*) => { $( q.$f += a.$f - d.$f; )* };
        }
        add!(temp, tint, exposure, contrast, highlights, shadows, whites, blacks, texture, clarity, dehaze, vibrance, saturation, c_hi, c_li, c_da, c_sh, sh_amt, nr_lum, nr_col, o_fringe);
        (q.sh_amt, q.nr_lum, q.nr_col, q.o_fringe) = (q.sh_amt.max(0.0), q.nr_lum.max(0.0), q.nr_col.max(0.0), q.o_fringe.max(0.0));
        for (qb, ab) in q.hsl.iter_mut().zip(&a.hsl) {
            for k in 0..3 {
                qb[k] += ab[k];
            }
        }
        // A mask's curve applies after the photo's own.
        for c in 0..4 {
            if a.curves[c].len() >= 2 {
                let (g, m) = (curve_table(&q.curves[c]), curve_table(&a.curves[c]));
                q.curves[c] = (0..=32).map(|i| { let x = i as f32 / 32.0; [x, table(&m, table(&g, x))] }).collect();
            }
        }
        q
    }

    /// The settings that move pixels around (what spots, filters and brush strokes are positioned against).
    fn geometry_key(&self) -> (f32, f32, f32, f32, bool, f32, &str, f32, f32, f32) {
        (self.t_vert, self.t_hor, self.t_rot, self.t_scale, self.flip_h, self.o_dist, self.lens.as_str(), self.lens_focal, self.lens_crop, self.lens_dist)
    }

    /// True when the photo has edits, ignoring its rating, flag, label and snapshots.
    pub fn is_edited(&self) -> bool {
        Params { rating: 0, flag: 0, label: 0, snapshots: vec![], ..self.clone() } != Params::default()
    }

    /// Turn a quarter clockwise (or back), carrying the crop, spots, filters and brush strokes along.
    pub fn rotate(&mut self, cw: bool) {
        // The flip is applied after the quarter turns, so on a flipped photo turning the source one way turns
        // what you see the other way. `cw` is what you see; the marks below are in that (frame) space.
        let src_cw = cw != self.flip_h;
        self.rot90 = (self.rot90 % 4 + if src_cw { 1 } else { 3 }) % 4;
        let f = |x: f32, y: f32| if cw { (1.0 - y, x) } else { (y, 1.0 - x) };
        let [l, t, r, b] = self.crop;
        self.crop = if cw { [b, l, t, r] } else { [t, r, b, l] };
        for s in &mut self.spots {
            (s.x, s.y) = f(s.x, s.y);
            (s.sx, s.sy) = f(s.sx, s.sy);
            for q in &mut s.path {
                (q[0], q[1]) = f(q[0], q[1]);
            }
        }
        for g in &mut self.grads {
            (g.a[0], g.a[1]) = f(g.a[0], g.a[1]);
            (g.b[0], g.b[1]) = f(g.b[0], g.b[1]);
        }
        for d in self.brushes.iter_mut().flat_map(|b| b.dabs.iter_mut()) {
            (d[0], d[1]) = f(d[0], d[1]);
        }
        for part in self.masks.iter_mut().flat_map(|m| m.parts.iter_mut()) {
            match &mut part.shape {
                Shape::Linear { a, b } => {
                    (a[0], a[1]) = f(a[0], a[1]);
                    (b[0], b[1]) = f(b[0], b[1]);
                }
                Shape::Radial { c, angle, .. } => {
                    (c[0], c[1]) = f(c[0], c[1]);
                    *angle += if cw { 90.0 } else { -90.0 };
                }
                Shape::Brush { dabs } => dabs.iter_mut().for_each(|d| (d[0], d[1]) = f(d[0], d[1])),
                _ => {}
            }
        }
    }
}

/// Plain frame for posting: black or white, width as % of the long side, optionally padded out to a fixed canvas aspect.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Border {
    pub width: f32,
    pub white: bool,
    /// Canvas width/height, e.g. 1.0 for square, 0.8 for 4:5. 0 = follow the photo.
    pub aspect: f32,
    /// Text set in a wider bottom band, like a print caption.
    pub caption: String,
}

/// Canvas size and where the photo sits on it: (canvas w, canvas h, photo x, photo y, caption px size).
pub fn border_layout(w: u32, h: u32, b: &Border) -> (u32, u32, u32, u32, f32) {
    let long = w.max(h) as f32;
    let pad = (b.width / 100.0 * long).round() as u32;
    let text_px = if b.caption.trim().is_empty() { 0.0 } else { (long * 0.022).max(10.0) };
    let band = (text_px * 2.6).round() as u32; // caption band added under the photo
    let (mut cw, mut ch) = (w + 2 * pad, h + 2 * pad + band);
    if b.aspect > 0.0 {
        if (cw as f32 / ch as f32) < b.aspect {
            cw = (ch as f32 * b.aspect).round() as u32;
        } else {
            ch = (cw as f32 / b.aspect).round() as u32;
        }
    }
    (cw, ch, (cw - w) / 2, (ch - h - band) / 2, text_px)
}

pub fn add_border(img: Rgb32FImage, b: &Border) -> Rgb32FImage {
    if b.width <= 0.0 && b.aspect <= 0.0 && b.caption.trim().is_empty() {
        return img;
    }
    let (w, h) = img.dimensions();
    let (cw, ch, ox, oy, text_px) = border_layout(w, h, b);
    let fill = if b.white { 1.0 } else { 0.0 };
    let mut out = Rgb32FImage::from_pixel(cw, ch, image::Rgb([fill; 3]));
    image::imageops::replace(&mut out, &img, ox as i64, oy as i64);
    if text_px > 0.0 {
        // Centred in the space between the photo and the bottom edge.
        let cy = (oy + h) as f32 + (ch - oy - h) as f32 / 2.0;
        let ink = if b.white { 0.12 } else { 0.88 };
        draw_text(&mut out, b.caption.trim(), cw as f32 / 2.0, cy, text_px, ink);
    }
    out
}

/// Anti-aliased single-line text, centred on (cx, cy), in the bundled IBM Plex Sans.
pub fn draw_text(img: &mut Rgb32FImage, text: &str, cx: f32, cy: f32, px: f32, ink: f32) {
    use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
    let Ok(font) = FontRef::try_from_slice(include_bytes!("../assets/fonts/IBMPlexSans-400.ttf")) else { return };
    let sf = font.as_scaled(PxScale::from(px));
    let mut x = 0.0;
    let mut prev = None;
    let mut glyphs = vec![];
    for c in text.chars() {
        let id = font.glyph_id(c);
        if let Some(p) = prev {
            x += sf.kern(p, id);
        }
        glyphs.push(id.with_scale_and_position(px, point(x, sf.ascent())));
        x += sf.h_advance(id);
        prev = Some(id);
    }
    let (x0, y0) = (cx - x / 2.0, cy - (sf.ascent() - sf.descent()) / 2.0);
    let (w, h) = img.dimensions();
    for g in glyphs {
        if let Some(o) = font.outline_glyph(g) {
            let bb = o.px_bounds();
            o.draw(|gx, gy, cov| {
                let (px_, py_) = ((x0 + bb.min.x) as i64 + gx as i64, (y0 + bb.min.y) as i64 + gy as i64);
                if px_ >= 0 && py_ >= 0 && (px_ as u32) < w && (py_ as u32) < h {
                    let p = img.get_pixel_mut(px_ as u32, py_ as u32);
                    p.0 = p.0.map(|v| v + (ink - v) * cov.min(1.0));
                }
            });
        }
    }
}

// ---------- spot healing ----------

/// Crop edges, clamped so at least 5% of the frame is left on each axis.
pub fn crop_of(p: &Params) -> [f32; 4] {
    let [l, t, r, b] = p.crop.map(|v| v.clamp(0.0, 0.95));
    let (r, b) = (r.min(0.95 - l), b.min(0.95 - t));
    [l, t, r, b]
}

/// Uncropped frame size and where the cropped image starts in it, for an output of `w`×`h`.
pub fn frame_of(p: &Params, w: usize, h: usize) -> (f32, f32, f32, f32) {
    let [cl, ct, cr, cb] = crop_of(p);
    let (fw, fh) = (w as f32 / (1.0 - cl - cr), h as f32 / (1.0 - ct - cb));
    (fw, fh, cl * fw, ct * fh)
}

fn bilinear_clamped(img: &Rgb32FImage, x: f32, y: f32) -> [f32; 3] {
    let (w, h) = img.dimensions();
    bilinear(img, x.clamp(0.0, (w - 1) as f32), y.clamp(0.0, (h - 1) as f32))
}

/// Pick a source for a spot at (x, y) with radius r (output pixels): of 8 nearby candidates,
/// the one whose surroundings look most like the spot's surroundings.
pub fn find_source(img: &Rgb32FImage, x: f32, y: f32, r: f32) -> (f32, f32) {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let ring = |cx: f32, cy: f32| (0..24).map(|i| {
        let (s, c) = (i as f32 / 24.0 * std::f32::consts::TAU).sin_cos();
        bilinear_clamped(img, cx + c * r * 1.3, cy + s * r * 1.3)
    }).collect::<Vec<_>>();
    let target = ring(x, y);
    (0..8).map(|i| {
        let (s, c) = (i as f32 / 8.0 * std::f32::consts::TAU).sin_cos();
        (x + c * r * 2.4, y + s * r * 2.4)
    }).filter(|(cx, cy)| *cx > r && *cy > r && *cx < w - r && *cy < h - r)
        .map(|(cx, cy)| {
            let d: f32 = ring(cx, cy).iter().zip(&target).map(|(a, b)| (0..3).map(|k| (a[k] - b[k]).abs()).sum::<f32>()).sum();
            (d, (cx, cy))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, p)| p)
        .unwrap_or((x + r * 2.4, y))
}

/// Distance from (x, y) to a stroke through `pts`, and the stroke point nearest to it.
fn stroke_dist(pts: &[[f32; 2]], x: f32, y: f32) -> (f32, usize) {
    let (mut d, mut near) = (f32::MAX, 0);
    for k in 0..pts.len() {
        let a = pts[k];
        let b = pts[(k + 1).min(pts.len() - 1)];
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let l2 = ex * ex + ey * ey;
        let t = if l2 > 0.0 { (((x - a[0]) * ex + (y - a[1]) * ey) / l2).clamp(0.0, 1.0) } else { 0.0 };
        let dd = ((x - a[0] - ex * t).powi(2) + (y - a[1] - ey * t).powi(2)).sqrt();
        if dd < d {
            (d, near) = (dd, if t > 0.5 { (k + 1).min(pts.len() - 1) } else { k });
        }
    }
    (d, near)
}

/// Replace each spot or stroke with texture from its source (turned by its angle), feathered in. Healing also
/// matches the source to the colour around each part of the stroke; cloning copies it unchanged.
/// `full` is the whole output's size and `off` where this image sits in it (they differ only for 1:1 zoom).
fn heal(img: &mut Rgb32FImage, p: &Params, full: (usize, usize), off: (f32, f32)) {
    if p.spots.is_empty() {
        return;
    }
    let (w, h) = img.dimensions();
    let (fw, fh, ox, oy) = frame_of(p, full.0, full.1);
    let (ox, oy) = (ox + off.0, oy + off.1);
    let src = img.clone();
    for s in &p.spots {
        let r = (s.r * fw.max(fh)).max(1.5);
        let pts: Vec<[f32; 2]> = s.points().map(|q| [q[0] * fw - ox, q[1] * fh - oy]).collect();
        let (tx, ty, sx, sy) = (pts[0][0], pts[0][1], s.sx * fw - ox, s.sy * fh - oy);
        let (sin, cos) = s.angle.to_radians().sin_cos();
        let source = |x: f32, y: f32| {
            let (dx, dy) = (x - tx, y - ty);
            (sx + dx * cos - dy * sin, sy + dx * sin + dy * cos)
        };
        // Colour correction per stroke point: surroundings of the target minus those of its source, from ring
        // samples outside the stroke (along a scratch, the ring would otherwise see the scratch itself).
        let fix: Vec<[f32; 3]> = pts.iter().map(|q| {
            if s.clone {
                return [0.0; 3];
            }
            let (mut acc, mut n) = ([0.0f32; 3], 0.0);
            for i in 0..32 {
                let (sn, cs) = (i as f32 / 32.0 * std::f32::consts::TAU).sin_cos();
                let (x, y) = (q[0] + cs * r * 1.15, q[1] + sn * r * 1.15);
                if stroke_dist(&pts, x, y).0 < r {
                    continue;
                }
                let (qx, qy) = source(x, y);
                let (t, o) = (bilinear_clamped(&src, x, y), bilinear_clamped(&src, qx, qy));
                (0..3).for_each(|k| acc[k] += t[k] - o[k]);
                n += 1.0;
            }
            if n > 0.0 { acc.map(|v| v / n) } else { [0.0; 3] }
        }).collect();
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for q in &pts {
            lo = [lo[0].min(q[0]), lo[1].min(q[1])];
            hi = [hi[0].max(q[0]), hi[1].max(q[1])];
        }
        let (x0, x1) = ((lo[0] - r).floor().max(0.0) as u32, ((hi[0] + r).ceil().max(0.0) as u32).min(w));
        let (y0, y1) = ((lo[1] - r).floor().max(0.0) as u32, ((hi[1] + r).ceil().max(0.0) as u32).min(h));
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        let stride = w as usize * 3;
        let buf: &mut [f32] = img;
        buf[y0 as usize * stride..y1 as usize * stride].par_chunks_mut(stride).enumerate().for_each(|(j, row)| {
            let yf = (y0 as usize + j) as f32;
            for (x, px) in px3_mut(row).iter_mut().enumerate().take(x1 as usize).skip(x0 as usize) {
                let xf = x as f32;
                let (d, near) = stroke_dist(&pts, xf, yf);
                let a = 1.0 - smooth(0.55 * r, r, d);
                if a <= 0.0 {
                    continue;
                }
                let (qx, qy) = source(xf, yf);
                let v = bilinear_clamped(&src, qx, qy);
                *px = [0, 1, 2].map(|k| px[k] + ((v[k] + fix[near][k]).max(0.0) - px[k]) * a);
            }
        });
    }
}

pub const HUE_CENTERS: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 275.0, 310.0];

/// An RGB buffer as pixels. Fixed-size pixels let the compiler drop bounds checks and vectorise; `chunks(3)`
/// slices made every 3-float copy a call into memmove.
pub fn px3(s: &[f32]) -> &[[f32; 3]] {
    s.as_chunks::<3>().0
}

pub fn px3_mut(s: &mut [f32]) -> &mut [[f32; 3]] {
    s.as_chunks_mut::<3>().0
}

pub fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear sRGB of a blackbody at `k` kelvin (Y = 1), from the Kim et al. Planckian-locus fit (1667–25000 K).
/// Smooth and accurate, unlike the common piecewise RGB curve fits.
pub fn kelvin_rgb(k: f32) -> [f32; 3] {
    let t = k.clamp(1667.0, 25000.0) as f64;
    let x = if t <= 4000.0 { -0.2661239e9 / t.powi(3) - 0.2343589e6 / t.powi(2) + 0.8776956e3 / t + 0.179910 } else { -3.0258469e9 / t.powi(3) + 2.1070379e6 / t.powi(2) + 0.2226347e3 / t + 0.240390 };
    let y = if t <= 2222.0 { -1.1063814 * x.powi(3) - 1.34811020 * x * x + 2.18555832 * x - 0.20219683 } else if t <= 4000.0 { -0.9549476 * x.powi(3) - 1.37418593 * x * x + 2.09137015 * x - 0.16748867 } else { 3.0817580 * x.powi(3) - 5.87338670 * x * x + 3.75112997 * x - 0.37001483 };
    let (xx, yy, zz) = (x / y, 1.0, (1.0 - x - y) / y);
    [3.2406 * xx - 1.5372 * yy - 0.4986 * zz, -0.9689 * xx + 1.8758 * yy + 0.0415 * zz, 0.0557 * xx - 0.2040 * yy + 1.0570 * zz].map(|v| (v as f32).max(0.005))
}

/// White-balance channel gains for a Temp/Tint setting, normalised to keep luminance (5500 K / 0 = as shot).
pub fn wb_gains(temp: f32, tint: f32) -> [f32; 3] {
    let (r, c) = (kelvin_rgb(5500.0), kelvin_rgb(temp));
    let mut g = [0, 1, 2].map(|i| r[i] / c[i]);
    g[1] *= 1.0 - tint / 400.0;
    let l = luma(g);
    g.map(|v| v / l)
}

/// White balance (temp, tint) that makes this linear colour neutral grey: the eyedropper.
pub fn solve_wb(c: [f32; 3]) -> (f32, f32) {
    let err = |t: f32, ti: f32| {
        let g = wb_gains(t, ti);
        let v = [0, 1, 2].map(|k| (c[k] * g[k]).max(1e-6).ln());
        let m = (v[0] + v[1] + v[2]) / 3.0;
        v.iter().map(|x| (x - m).powi(2)).sum::<f32>()
    };
    let mut best = (f32::MAX, 5500.0, 0.0);
    for i in 0..=200 {
        let t = 2000.0 * (6.0f32).powf(i as f32 / 200.0); // 2000–12000 K, finer at the warm end
        for j in -75..=75 {
            let e = err(t, j as f32 * 2.0);
            if e < best.0 {
                best = (e, t, j as f32 * 2.0);
            }
        }
    }
    ((best.1 / 10.0).round() * 10.0, best.2)
}

/// `x` raised to `p` for x ≥ 0, through fast log2/exp2 (relative error about 1e-6). Hot per-pixel loops
/// call powf several times a pixel; this is a good deal cheaper and well below what 16-bit output can show.
#[inline]
pub fn fpow(x: f32, p: f32) -> f32 {
    if x <= 0.0 {
        return if p == 0.0 { 1.0 } else { 0.0 };
    }
    if !x.is_finite() {
        return x.powf(p);
    }
    exp2_fast(p * log2_fast(x))
}

#[inline]
pub(crate) fn log2_fast(x: f32) -> f32 {
    // x = m · 2^e with m in [√½, √2), then log2(m) from the atanh series in t = (m − 1)/(m + 1), |t| < 0.172.
    let bits = x.to_bits();
    let mut e = ((bits >> 23) & 0xff) as i32 - 127;
    let mut m = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
    if e == -127 {
        return x.log2(); // subnormal
    }
    if m > std::f32::consts::SQRT_2 {
        m *= 0.5;
        e += 1;
    }
    let t = (m - 1.0) / (m + 1.0);
    let t2 = t * t;
    e as f32 + t * (2.885_39 + t2 * (0.961_797_7 + t2 * (0.577_078 + t2 * 0.412_198)))
}

#[inline]
fn exp2_fast(y: f32) -> f32 {
    let y = y.clamp(-126.0, 127.0);
    let i = floor_fast(y);
    let f = y - i;
    // 2^f on [0, 1): degree-6 polynomial (Taylor of e^(f·ln2), error < 2e-7).
    let p = 1.0 + f * (std::f32::consts::LN_2 + f * (0.240_226_5 + f * (0.055_504_1 + f * (0.009_618_13 + f * (0.001_333_36 + f * 0.000_154_04)))));
    p * f32::from_bits(((i as i32 + 127) as u32) << 23)
}

pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = if (0.0..360.0).contains(&h) { h } else { h.rem_euclid(360.0) } / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h - 2.0 * floor_fast(h * 0.5)) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0), 1 => (x, c, 0.0), 2 => (0.0, c, x), 3 => (0.0, x, c), 4 => (x, 0.0, c), _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

pub fn rgb_to_hsv(c: [f32; 3]) -> (f32, f32, f32) {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = mx - mn;
    let h = if d <= 1e-6 { 0.0 } else if mx == c[0] { let q = (c[1] - c[2]) / d; 60.0 * if q < 0.0 { q + 6.0 } else { q } } else if mx == c[1] { 60.0 * ((c[2] - c[0]) / d + 2.0) } else { 60.0 * ((c[0] - c[1]) / d + 4.0) };
    (h, if mx <= 1e-6 { 0.0 } else { d / mx }, mx)
}

/// Weight of `hue` in color-mixer band `i` (triangular between neighbouring centers). The reference for
/// `band_pair` and the GPU shader.
#[cfg(test)]
fn band_weight(hue: f32, i: usize) -> f32 {
    let c = HUE_CENTERS[i];
    // hue and the centres are in 0..360, so one wrap brings the difference into -180..180.
    let mut d = hue - c;
    if d > 180.0 {
        d -= 360.0;
    } else if d < -180.0 {
        d += 360.0;
    }
    let span = if d < 0.0 { BAND_SPAN[i].0 } else { BAND_SPAN[i].1 };
    (1.0 - d.abs() / span).max(0.0)
}

/// The two mixer bands a hue lies between, and how far it is towards the second: the same weights as
/// `band_weight` over all 8 bands (only two are ever non-zero), at a fraction of the work per pixel.
fn band_pair(hue: f32) -> (usize, usize, f32) {
    let a = HUE_CENTERS.iter().rposition(|c| *c <= hue).unwrap_or(0); // hue ≥ 0 = the first centre
    let t = ((hue - HUE_CENTERS[a]) / BAND_SPAN[a].1).clamp(0.0, 1.0);
    (a, (a + 1) % 8, t)
}

/// Each band's distance to the centre below and above it. Worked out once: `rem_euclid` is a libm call, and the
/// mixer asks for 8 bands a pixel.
const BAND_SPAN: [(f32, f32); 8] = {
    let mut s = [(0.0, 0.0); 8];
    let mut i = 0;
    while i < 8 {
        let (c, prev, next) = (HUE_CENTERS[i], HUE_CENTERS[(i + 7) % 8], HUE_CENTERS[(i + 1) % 8]);
        s[i] = (if c > prev { c - prev } else { c - prev + 360.0 }, if next > c { next - c } else { next - c + 360.0 });
        i += 1;
    }
    s
};

/// Rotate a colour around the grey axis by `deg` degrees (Rodrigues).
pub fn hue_rotate(c: [f32; 3], deg: f32) -> [f32; 3] {
    let (s, co) = deg.to_radians().sin_cos();
    let k = 1.0 / 3f32.sqrt();
    let dot = (c[0] + c[1] + c[2]) * k;
    let cross = [k * (c[2] - c[1]), k * (c[0] - c[2]), k * (c[1] - c[0])];
    [0, 1, 2].map(|i| c[i] * co + cross[i] * s + k * dot * (1.0 - co))
}

type M3 = [[f32; 3]; 3];

fn mul(m: &M3, c: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
}

/// Calibration: each primary's column is hue-rotated and saturation-scaled.
fn calibration_matrix(p: &Params) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for j in 0..3 {
        let mut e = [0.0; 3];
        e[j] = 1.0;
        let e = hue_rotate(e, p.cal[j][0] * 0.3);
        let y = luma(e);
        let e = e.map(|v| y + (v - y) * (1.0 + p.cal[j][1] / 100.0));
        for i in 0..3 {
            m[i][j] = e[i];
        }
    }
    m
}

// ---------- blur helpers (3x box ≈ gaussian, O(n) in radius) ----------

/// One horizontal box pass of radius `r` over a row, edges clamped (running sum, O(1) per pixel).
fn box_row(s: &[f32], o: &mut [f32], r: usize) {
    let w = s.len();
    let inv = 1.0 / (2 * r + 1) as f32;
    let mut acc: f32 = (0..=2 * r).map(|i| s[i.saturating_sub(r).min(w - 1)]).sum();
    for x in 0..w {
        o[x] = acc * inv;
        acc += s[(x + r + 1).min(w - 1)] - s[x.saturating_sub(r)];
    }
}

/// One vertical box pass of radius `r`, edges clamped. Works on whole rows, so it reads memory in order and
/// vectorises. Each band of rows keeps a running sum of the rows in its window: one row in and one out per
/// output row, instead of adding up all 2r + 1 again.
fn box_cols(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    const BAND: usize = 64;
    let inv = 1.0 / (2 * r + 1) as f32;
    let row = |y: usize| &src[y.min(h - 1) * w..][..w];
    dst.par_chunks_mut(w * BAND).enumerate().for_each(|(band, out)| {
        let y0 = band * BAND;
        let mut acc = vec![0f32; w];
        for k in 0..=2 * r {
            acc.iter_mut().zip(row((y0 + k).saturating_sub(r))).for_each(|(a, b)| *a += *b);
        }
        for (j, o) in out.chunks_mut(w).enumerate() {
            let y = y0 + j;
            o.iter_mut().zip(&acc).for_each(|(o, a)| *o = *a * inv);
            let (gone, next) = (row(y.saturating_sub(r)), row(y + r + 1));
            acc.iter_mut().zip(next.iter().zip(gone)).for_each(|(a, (n, g))| *a += *n - *g);
        }
    });
}

pub fn blur(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    // Wide blurs (clarity, dehaze, halation) are smooth: blur a smaller copy and scale it back up.
    let f = (sigma / 4.0) as usize;
    if f >= 2 && w > 2 * f && h > 2 * f {
        let (sw, sh) = (w.div_ceil(f), h.div_ceil(f));
        let mut small = vec![0f32; sw * sh];
        small.par_chunks_mut(sw).enumerate().for_each(|(sy, row)| {
            for (sx, v) in row.iter_mut().enumerate() {
                let (xa, ya) = (sx * f, sy * f);
                let (xb, yb) = ((xa + f).min(w), (ya + f).min(h));
                let mut acc = 0.0;
                for y in ya..yb {
                    acc += src[y * w + xa..y * w + xb].iter().sum::<f32>();
                }
                *v = acc / ((xb - xa) * (yb - ya)) as f32;
            }
        });
        let b = blur_full(&small, sw, sh, ((sigma / f as f32).powi(2) - 1.0 / 12.0).sqrt()); // the averaging already blurred a little
        let mut out = vec![0f32; w * h];
        out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let fy = ((y as f32 + 0.5) / f as f32 - 0.5).clamp(0.0, (sh - 1) as f32);
            let (y0, ty) = (fy as usize, fy.fract());
            let y1 = (y0 + 1).min(sh - 1);
            for (x, v) in row.iter_mut().enumerate() {
                let fx = ((x as f32 + 0.5) / f as f32 - 0.5).clamp(0.0, (sw - 1) as f32);
                let (x0, tx) = (fx as usize, fx.fract());
                let x1 = (x0 + 1).min(sw - 1);
                let top = b[y0 * sw + x0] + (b[y0 * sw + x1] - b[y0 * sw + x0]) * tx;
                let bot = b[y1 * sw + x0] + (b[y1 * sw + x1] - b[y1 * sw + x0]) * tx;
                *v = top + (bot - top) * ty;
            }
        });
        return out;
    }
    blur_full(src, w, h, sigma)
}

fn blur_full(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let r = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round() as usize;
    if r == 0 || w == 0 || h == 0 {
        return src.to_vec();
    }
    // Three box passes each way ≈ a gaussian.
    let mut a = vec![0f32; w * h];
    a.par_chunks_mut(w).zip(src.par_chunks(w)).for_each_init(|| (vec![0f32; w], vec![0f32; w]), |(t1, t2), (o, s)| {
        box_row(s, t1, r);
        box_row(t1, t2, r);
        box_row(t2, o, r);
    });
    let mut b = vec![0f32; w * h];
    box_cols(&a, &mut b, w, h, r);
    box_cols(&b, &mut a, w, h, r);
    box_cols(&a, &mut b, w, h, r);
    b
}

// ---------- geometry ----------

pub fn bilinear(img: &Rgb32FImage, x: f32, y: f32) -> [f32; 3] {
    let (w, h) = img.dimensions();
    if x < 0.0 || y < 0.0 || x > (w - 1) as f32 || y > (h - 1) as f32 {
        return [0.0; 3];
    }
    let (x0, y0) = (x as u32, y as u32);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let p = |x, y| img.get_pixel(x, y).0;
    let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    [0, 1, 2].map(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
}

/// A part of the output to render, for 1:1 zoom: x, y, width, height in output pixels.
pub type Window = [u32; 4];

/// Output size (after crop) for a source of this size.
pub fn output_size(w: u32, h: u32, p: &Params) -> (u32, u32) {
    let (w, h) = oriented(w, h, p);
    let [cl, ct, cr, cb] = crop_of(p);
    (((w as f32 * (1.0 - cl - cr)) as u32).max(1), ((h as f32 * (1.0 - ct - cb)) as u32).max(1))
}

/// Source size after the quarter turns.
pub fn oriented(w: u32, h: u32, p: &Params) -> (u32, u32) {
    if p.rot90 % 2 == 1 { (h, w) } else { (w, h) }
}

/// Crop, straighten, perspective, scale, flip and lens distortion in one inverse-mapped resample.
pub fn geometry(src: &Rgb32FImage, p: &Params) -> Rgb32FImage {
    geometry_window(src, p, None)
}

/// Inverse map from the uncropped output frame to the (quarter-turned) source: straighten, perspective, scale,
/// flip, manual distortion and the lens profile, all in pixels.
pub struct Mapper {
    hw: f32,
    hh: f32,
    diag2: f32,
    sin: f32,
    cos: f32,
    scale: f32,
    kv: f32,
    kh: f32,
    flip: bool,
    k: f32,
    lens: Option<LensMap>,
}

struct LensMap {
    dist: Option<crate::lens::Dist>,
    tca: Option<[f32; 6]>,
    vig: Option<[f32; 3]>,
    /// Pixel radius → the profile's units.
    hugin: f32,
    vscale: f32,
    da: f32,
    va: f32,
    /// Zoom so a pincushion correction doesn't pull empty space in at the edges.
    auto: f32,
}

impl LensMap {
    /// Corrected radius → where to sample, as a factor on the position.
    fn dist_factor(&self, r: f32) -> f32 {
        match self.dist {
            Some(d) if r > 0.0 => {
                let ru = r * self.hugin;
                1.0 + self.da * (d.apply(ru) / ru - 1.0)
            }
            _ => 1.0,
        }
    }
}

impl Mapper {
    /// For a source (already quarter-turned) of `w`×`h`.
    pub fn new(p: &Params, w: u32, h: u32) -> Self {
        let (wf, hf) = (w as f32, h as f32);
        let (hw, hh) = (wf / 2.0, hf / 2.0);
        let (sin, cos) = (-p.t_rot.to_radians()).sin_cos();
        // Straightening zooms in just enough to hide the rotated corners (Lightroom's "constrain crop").
        let (ac, as_) = (cos.abs(), sin.abs());
        let cover = ((wf * ac + hf * as_) / wf).max((wf * as_ + hf * ac) / hf);
        let diag2 = hw * hw + hh * hh;
        let lens = (!p.lens.is_empty()).then(|| crate::lens::profile(&p.lens, p.lens_focal, p.lens_f, p.lens_crop)).flatten().map(|pr| {
            let half = diag2.sqrt();
            let mut l = LensMap { dist: pr.dist, tca: pr.tca, vig: pr.vig, hugin: pr.hugin / half, vscale: pr.vig_scale / half, da: p.lens_dist / 100.0, va: p.lens_vig / 100.0, auto: 1.0 };
            // Largest zoom (≤ 1) for which every edge radius still lands inside the source.
            let fits = |s: f32| (0..=16).all(|i| {
                let r = hw.min(hh) + (half - hw.min(hh)) * i as f32 / 16.0;
                r * s * l.dist_factor(r * s) <= r * 1.0005
            });
            if !fits(1.0) {
                let (mut lo, mut hi) = (0.5f32, 1.0f32);
                for _ in 0..24 {
                    let m = (lo + hi) / 2.0;
                    if fits(m) { lo = m } else { hi = m }
                }
                l.auto = lo;
            }
            l
        });
        Mapper { hw, hh, diag2, sin, cos, scale: 100.0 / p.t_scale.max(1.0) / cover, kv: p.t_vert / 100.0 * 0.4, kh: p.t_hor / 100.0 * 0.4, flip: p.flip_h, k: -p.o_dist / 100.0 * 0.25, lens }
    }

    /// Source position (for green) of frame pixel (fx, fy), the radial scale for red and blue, and the vignetting gain.
    pub fn map(&self, fx: f32, fy: f32) -> (f32, f32, f32, f32, f32) {
        let (mut x, mut y) = ((fx - self.hw) * self.scale, (fy - self.hh) * self.scale);
        (x, y) = (x * self.cos - y * self.sin, x * self.sin + y * self.cos);
        // keystone: vertical slider widens the top or bottom, horizontal the left or right side
        x *= 1.0 + self.kv * y / self.hh;
        y *= 1.0 + self.kh * x / self.hw;
        if self.flip {
            x = -x;
        }
        let f = 1.0 + self.k * (x * x + y * y) / self.diag2;
        (x, y) = (x * f, y * f);
        let (mut sr, mut sb, mut gain) = (1.0, 1.0, 1.0);
        if let Some(l) = &self.lens {
            (x, y) = (x * l.auto, y * l.auto);
            let m = l.dist_factor((x * x + y * y).sqrt());
            (x, y) = (x * m, y * m);
            let r = (x * x + y * y).sqrt();
            if let Some(t) = l.tca {
                let rh = r * l.hugin;
                (sr, sb) = ((t[2] * rh + t[1]) * rh + t[0], (t[5] * rh + t[4]) * rh + t[3]);
            }
            if let Some(k) = l.vig {
                let r2 = (r * l.vscale).powi(2);
                let c = 1.0 + r2 * (k[0] + r2 * (k[1] + r2 * k[2]));
                gain = 1.0 / (1.0 + l.va * (c - 1.0)).max(0.05);
            }
        }
        (x + self.hw, y + self.hh, sr, sb, gain)
    }

    pub fn is_identity(p: &Params) -> bool {
        p.t_vert == 0.0 && p.t_hor == 0.0 && p.t_rot == 0.0 && p.t_scale == 100.0 && !p.flip_h && p.o_dist == 0.0 && p.lens.is_empty()
    }

    /// The frame pixel showing source pixel (sx, sy): the inverse of `map`, by Newton's method.
    fn unmap(&self, sx: f32, sy: f32, guess: (f32, f32)) -> (f32, f32) {
        let (mut fx, mut fy) = guess;
        for _ in 0..30 {
            let (gx, gy, ..) = self.map(fx, fy);
            let (ex, ey) = (gx - sx, gy - sy);
            if ex.abs() + ey.abs() < 0.01 {
                break;
            }
            let (ax, ay, ..) = self.map(fx + 1.0, fy);
            let (bx, by, ..) = self.map(fx, fy + 1.0);
            let (j00, j01, j10, j11) = (ax - gx, bx - gx, ay - gy, by - gy);
            let det = j00 * j11 - j01 * j10;
            if det.abs() < 1e-9 {
                break;
            }
            fx -= (j11 * ex - j01 * ey) / det;
            fy -= (-j10 * ex + j00 * ey) / det;
        }
        (fx, fy)
    }
}

/// After a geometry change from `old` to `new`, move heal spots, graduated filters and brush strokes so they stay
/// on the same part of the photo. `w`×`h` is any size with the photo's shape (before quarter turns).
pub fn remap_marks(old: &Params, new: &mut Params, w: u32, h: u32) {
    if old.geometry_key() == new.geometry_key() || old.rot90 != new.rot90 || (new.spots.is_empty() && new.grads.is_empty() && new.brushes.is_empty() && new.masks.is_empty()) {
        return;
    }
    let (w, h) = oriented(w, h, new);
    let (a, b) = (Mapper::new(old, w, h), Mapper::new(new, w, h));
    let (wf, hf) = (w as f32, h as f32);
    let mv = |x: &mut f32, y: &mut f32| {
        let (sx, sy, ..) = a.map(*x * wf, *y * hf);
        let (fx, fy) = b.unmap(sx, sy, (*x * wf, *y * hf));
        (*x, *y) = (fx / wf, fy / hf);
    };
    // How many source pixels one frame pixel covers here, to keep sizes on the photo the same.
    let mag = |m: &Mapper, x: f32, y: f32| {
        let (p, q) = (m.map(x * wf, y * hf), m.map(x * wf + 1.0, y * hf));
        ((q.0 - p.0).powi(2) + (q.1 - p.1).powi(2)).sqrt().max(1e-3)
    };
    for s in &mut new.spots {
        let before = mag(&a, s.x, s.y);
        mv(&mut s.x, &mut s.y);
        mv(&mut s.sx, &mut s.sy);
        for q in &mut s.path {
            let [x, y] = q;
            mv(x, y);
        }
        s.r *= before / mag(&b, s.x, s.y);
    }
    for g in &mut new.grads {
        for pt in [&mut g.a, &mut g.b] {
            let [x, y] = pt;
            mv(x, y);
        }
    }
    let dab = |d: &mut [f32; 4]| {
        let before = mag(&a, d[0], d[1]);
        let (mut x, mut y) = (d[0], d[1]);
        mv(&mut x, &mut y);
        (d[0], d[1]) = (x, y);
        d[2] *= before / mag(&b, x, y);
    };
    new.brushes.iter_mut().flat_map(|b| b.dabs.iter_mut()).for_each(dab);
    for part in new.masks.iter_mut().flat_map(|m| m.parts.iter_mut()) {
        match &mut part.shape {
            Shape::Linear { a: p0, b: p1 } => {
                for pt in [p0, p1] {
                    let [x, y] = pt;
                    mv(x, y);
                }
            }
            Shape::Radial { c, rx, ry, .. } => {
                let before = mag(&a, c[0], c[1]);
                let [x, y] = c;
                mv(x, y);
                let k = before / mag(&b, c[0], c[1]);
                (*rx, *ry) = (*rx * k, *ry * k);
            }
            Shape::Brush { dabs } => dabs.iter_mut().for_each(dab),
            _ => {}
        }
    }
}

fn geometry_window(src: &Rgb32FImage, p: &Params, win: Option<Window>) -> Rgb32FImage {
    let (fw, fh) = output_size(src.width(), src.height(), p);
    let turned;
    let src = match p.rot90 % 4 {
        0 => src,
        n => {
            turned = match n { 1 => image::imageops::rotate90(src), 2 => image::imageops::rotate180(src), _ => image::imageops::rotate270(src) };
            &turned
        }
    };
    let (w, h) = src.dimensions();
    let [cl, ct, ..] = crop_of(p);
    let [x0, y0, ow, oh] = win.unwrap_or([0, 0, fw, fh]);
    let (ow, oh) = (ow.min(fw.saturating_sub(x0)).max(1), oh.min(fh.saturating_sub(y0)).max(1));
    if Mapper::is_identity(p) {
        return image::imageops::crop_imm(src, (w as f32 * cl) as u32 + x0, (h as f32 * ct) as u32 + y0, ow, oh).to_image();
    }
    let m = Mapper::new(p, w, h);
    let (hw, hh) = (w as f32 / 2.0, h as f32 / 2.0);
    let mut out = Rgb32FImage::new(ow, oh);
    out.par_chunks_mut(ow as usize * 3).enumerate().for_each(|(oy, row)| {
        for ox in 0..ow as usize {
            let (x, y, sr, sb, gain) = m.map((ox as u32 + x0) as f32 + w as f32 * cl, (oy as u32 + y0) as f32 + h as f32 * ct);
            let mut v = bilinear(src, x, y);
            // Lateral chromatic aberration: red and blue are sampled a little further in or out.
            if sr != 1.0 || sb != 1.0 {
                v[0] = bilinear(src, hw + (x - hw) * sr, hh + (y - hh) * sr)[0];
                v[2] = bilinear(src, hw + (x - hw) * sb, hh + (y - hh) * sb)[2];
            }
            row[ox * 3..ox * 3 + 3].copy_from_slice(&v.map(|c| c * gain));
        }
    });
    out
}

// ---------- the pipeline ----------

fn hash(x: i32, y: i32) -> f32 {
    let mut n = (x as u32).wrapping_mul(374761393) ^ (y as u32).wrapping_mul(668265263);
    n = (n ^ (n >> 13)).wrapping_mul(1274126177);
    ((n ^ (n >> 16)) & 0xffff) as f32 / 65535.0
}

/// `floor` for values well inside i32 range. Plain x86-64 has no rounding instruction, so `f32::floor` is a libm
/// call; the grain and fast-math loops call it several times a pixel.
#[inline]
fn floor_fast(x: f32) -> f32 {
    let t = x as i32 as f32;
    if t > x { t - 1.0 } else { t }
}

fn value_noise(x: f32, y: f32) -> f32 {
    let (xi, yi) = (floor_fast(x) as i32, floor_fast(y) as i32);
    let (fx, fy) = (smooth(0.0, 1.0, x - xi as f32), smooth(0.0, 1.0, y - yi as f32));
    let a = hash(xi, yi) * (1.0 - fx) + hash(xi + 1, yi) * fx;
    let b = hash(xi, yi + 1) * (1.0 - fx) + hash(xi + 1, yi + 1) * fx;
    a * (1.0 - fy) + b * fy
}

fn bump(x: f32, center: f32, width: f32) -> f32 {
    let t = (x - center) / width;
    (1.0 - t * t).max(0.0).powi(2)
}

/// Parametric tone curve (Highlights, Lights, Darks, Shadows regions) on perceptual luminance.
pub fn tone_curve(l: f32, p: &Params) -> f32 {
    l + 0.15 * (p.c_sh / 100.0 * bump(l, 0.12, 0.2) + p.c_da / 100.0 * bump(l, 0.35, 0.22) + p.c_li / 100.0 * bump(l, 0.62, 0.22) + p.c_hi / 100.0 * bump(l, 0.88, 0.2))
}

/// Global tone on perceptual luminance (0..1-ish).
fn tone(mut l: f32, p: &Params) -> f32 {
    let bk = -p.blacks / 100.0 * 0.08;
    let wh = 1.0 - p.whites / 100.0 * 0.2;
    l = (l - bk) / (wh - bk);
    l += p.highlights / 100.0 * 0.22 * smooth(0.45, 1.0, l) * (1.0 - smooth(1.0, 1.6, l) * 0.5);
    l += p.shadows / 100.0 * 0.22 * (1.0 - smooth(0.0, 0.5, l)) * smooth(-0.05, 0.08, l);
    let k = p.contrast / 100.0;
    let lc = l.clamp(0.0, 1.0);
    l += if k > 0.0 { k * (smooth(0.0, 1.0, lc) - lc) } else { k * (lc - 0.46) * 0.6 };
    tone_curve(l, p)
}

/// Render `src` (linear) to sRGB-encoded output. `scale` = this image's width / full-resolution width,
/// so pixel radii (sharpening, grain, blurs) match between preview and export.
pub fn render(src: &Rgb32FImage, p: &Params, scale: f32) -> Rgb32FImage {
    render_window(src, p, scale, None)
}

/// Render only `win` of the output (1:1 zoom). Everything that depends on position (vignettes, filters,
/// grain, blur sizes) uses the whole output's coordinates, so the window matches the same part of a full render.
/// A negative needs `p.neg.base` and `levels` set, since a window alone can't be analysed.
pub fn render_window(src: &Rgb32FImage, p: &Params, scale: f32, win: Option<Window>) -> Rgb32FImage {
    // Old-style filters and brushes (settings from an older version) render as the masks they become.
    let migrated;
    let p = if p.grads.is_empty() && p.brushes.is_empty() {
        p
    } else {
        let mut q = p.clone();
        q.migrate();
        migrated = q;
        &migrated
    };
    let (full_w, full_h) = output_size(src.width(), src.height(), p);
    let (x0, y0) = win.map_or((0, 0), |v| (v[0] as usize, v[1] as usize));
    let mut base = geometry_window(src, p, win);
    heal(&mut base, p, (full_w as usize, full_h as usize), (x0 as f32, y0 as f32)); // before inversion: dust on a scan is healed on the negative
    if p.neg.enabled {
        negative::convert(&mut base, &p.neg);
    }
    let masks: Vec<&Mask> = p.masks.iter().filter(|m| !m.hidden && !m.parts.is_empty() && m.opacity > 0.0).collect();
    let mut img = finish(if masks.is_empty() { std::mem::take(&mut base) } else { base.clone() }, p, scale, (full_w, full_h), (x0, y0));
    // Each mask: the photo rendered with the mask's settings, blended in where the mask is on.
    if !masks.is_empty() {
        let global = img.clone();
        for m in masks {
            let wts = mask_weights(m, p, &global, (full_w, full_h), (x0, y0));
            if wts.iter().all(|v| *v <= 0.0) {
                continue;
            }
            let alt = finish(base.clone(), &p.with_mask(&m.adj), scale, (full_w, full_h), (x0, y0));
            px3_mut(&mut img).par_iter_mut().zip(px3(&alt).par_iter()).zip(wts.par_iter()).for_each(|((o, a), w)| {
                for k in 0..3 {
                    o[k] += (a[k] - o[k]) * w;
                }
            });
        }
    }
    if win.is_some() { img } else { add_border(img, &p.border) }
}

/// How strongly mask `m` applies at each pixel of a render (0–1), given that render's global result `shown`
/// (display values; luminance and colour ranges are read from it). `full` is the whole output's size and
/// `off` where this render sits in it.
pub fn mask_weights(m: &Mask, p: &Params, shown: &Rgb32FImage, full: (u32, u32), off: (usize, usize)) -> Vec<f32> {
    let (w, h) = (shown.width() as usize, shown.height() as usize);
    let (fw, fh, ox, oy) = frame_of(p, full.0 as usize, full.1 as usize);
    let (x0, y0) = (off.0 as f32, off.1 as f32);
    let long = fw.max(fh);
    let to_px = |f: [f32; 2]| [f[0] * fw - ox - x0, f[1] * fh - oy - y0];
    let mut acc: Vec<f32> = vec![0.0; w * h];
    for (k, part) in m.parts.iter().enumerate() {
        let mut v: Vec<f32> = vec![0.0; w * h];
        match &part.shape {
            Shape::Linear { a, b } => {
                let (a, b) = (to_px(*a), to_px(*b));
                let d = [b[0] - a[0], b[1] - a[1]];
                let l2 = (d[0] * d[0] + d[1] * d[1]).max(1.0);
                v.par_chunks_mut(w).enumerate().for_each(|(y, row)| for (x, o) in row.iter_mut().enumerate() {
                    let t = ((x as f32 - a[0]) * d[0] + (y as f32 - a[1]) * d[1]) / l2;
                    *o = 1.0 - smooth(0.0, 1.0, t);
                });
            }
            Shape::Radial { c, rx, ry, angle, feather } => {
                let c = to_px(*c);
                let (rx, ry) = ((rx * long).max(1.0), (ry * long).max(1.0));
                let (sn, cs) = angle.to_radians().sin_cos();
                let inner = 1.0 - (feather / 100.0).clamp(0.01, 1.0);
                v.par_chunks_mut(w).enumerate().for_each(|(y, row)| for (x, o) in row.iter_mut().enumerate() {
                    let (dx, dy) = (x as f32 - c[0], y as f32 - c[1]);
                    let (u, q) = (dx * cs + dy * sn, -dx * sn + dy * cs);
                    let d = ((u / rx).powi(2) + (q / ry).powi(2)).sqrt();
                    *o = 1.0 - smooth(inner, 1.0, d);
                });
            }
            Shape::Brush { dabs } => {
                let dabs: Vec<_> = dabs.iter().map(|d| { let c = to_px([d[0], d[1]]); (c[0], c[1], (d[2] * long).max(1.0), d[3] > 0.0) }).collect();
                v.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
                    let yf = y as f32;
                    for &(tx, ty, r, add) in &dabs {
                        let dy2 = (yf - ty).powi(2);
                        if dy2 >= r * r {
                            continue;
                        }
                        let (xa, xb) = ((tx - r).floor().max(0.0) as usize, ((tx + r).ceil().max(0.0) as usize).min(w));
                        for (x, o) in row.iter_mut().enumerate().take(xb).skip(xa) {
                            let a = 1.0 - smooth(0.4 * r, r, ((x as f32 - tx).powi(2) + dy2).sqrt());
                            *o = if add { o.max(a) } else { *o * (1.0 - a) };
                        }
                    }
                });
            }
            Shape::Luminance { lo, hi, soft } => {
                let s = soft.max(0.001);
                v.par_iter_mut().zip(px3(shown).par_iter()).for_each(|(o, c)| {
                    let l = luma(*c);
                    *o = smooth(lo - s, *lo, l) * (1.0 - smooth(*hi, hi + s, l));
                });
            }
            Shape::Color { rgb, range } => {
                // Distance in a simple opponent space: mostly hue and colourfulness, a little brightness.
                let opp = |c: [f32; 3]| [c[0] - c[1], (c[0] + c[1]) * 0.5 - c[2], luma(c) * 0.5];
                let t = opp(*rgb);
                let r = 0.02 + range / 100.0 * 0.35;
                v.par_iter_mut().zip(px3(shown).par_iter()).for_each(|(o, c)| {
                    let q = opp(*c);
                    let d = ((q[0] - t[0]).powi(2) + (q[1] - t[1]).powi(2) + (q[2] - t[2]).powi(2)).sqrt();
                    *o = 1.0 - smooth(r * 0.5, r, d);
                });
            }
            Shape::All => v.iter_mut().for_each(|o| *o = 1.0),
        }
        if part.invert {
            v.iter_mut().for_each(|o| *o = 1.0 - *o);
        }
        match (k, part.op) {
            (0, MaskOp::Add) | (0, MaskOp::Intersect) => acc = v,
            (0, MaskOp::Subtract) => {}
            (_, MaskOp::Add) => acc.par_iter_mut().zip(&v).for_each(|(a, b)| *a = a.max(*b)),
            (_, MaskOp::Subtract) => acc.par_iter_mut().zip(&v).for_each(|(a, b)| *a *= 1.0 - b),
            (_, MaskOp::Intersect) => acc.par_iter_mut().zip(&v).for_each(|(a, b)| *a *= b),
        }
    }
    if m.feather > 0.0 {
        acc = blur(&acc, w, h, m.feather / 100.0 * long * 0.04);
    }
    let k = (m.opacity / 100.0).clamp(0.0, 1.0);
    acc.par_iter_mut().for_each(|a| *a = if m.invert { 1.0 - *a } else { *a }.clamp(0.0, 1.0) * k);
    acc
}

/// Everything after geometry and negative conversion, for one set of settings: tone, colour, effects, detail.
fn finish(mut img: Rgb32FImage, p: &Params, scale: f32, full: (u32, u32), off: (usize, usize)) -> Rgb32FImage {
    let (full_w, full_h) = full;
    let (x0, y0) = off;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let (big_w, big_h) = (full_w as f32, full_h as f32);

    // Linear stage: white balance, calibration, exposure, manual lens vignetting.
    let ev = 2f32.powf(p.exposure);
    let gains = wb_gains(p.temp, p.tint).map(|g| g * ev);
    let cal = calibration_matrix(p);
    // Positions below are in whole-output pixels: (gx, gy) = this pixel's place in the full render.
    let (cx, cy) = (big_w / 2.0, big_h / 2.0);
    let r2max = cx * cx + cy * cy;
    let cal_on = p.cal.iter().flatten().any(|v| *v != 0.0);
    img.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for (x, px) in px3_mut(row).iter_mut().enumerate() {
            let (gx, gy) = ((x + x0) as f32, (y + y0) as f32);
            let mut c = [px[0] * gains[0], px[1] * gains[1], px[2] * gains[2]];
            if cal_on {
                c = mul(&cal, c);
            }
            if p.o_vig != 0.0 {
                let r2 = ((gx - cx).powi(2) + (gy - cy).powi(2)) / r2max;
                c = c.map(|v| v * (1.0 + p.o_vig / 100.0 * r2));
            }
            if p.cal_sh != 0.0 {
                let s = (1.0 - smooth(0.0, 0.18, luma(c))) * p.cal_sh / 100.0 * 0.02;
                c[1] += s;
            }
            *px = c.map(|v| v.max(0.0));
        }
    });

    // Halation: highlights above the threshold, blurred wide and added back as a red-orange glow.
    if p.hal_amt > 0.0 {
        let t = 0.15 + p.hal_thr / 100.0 * 0.85;
        let hi: Vec<f32> = px3(&img).par_iter().map(|c| (luma(*c) - t).max(0.0)).collect();
        let sigma = big_w.max(big_h) * (0.002 + p.hal_size / 100.0 * 0.02);
        let (near, far) = (blur(&hi, w, h, sigma * 0.4), blur(&hi, w, h, sigma));
        let k = p.hal_amt / 100.0 * 3.0;
        px3_mut(&mut img).par_iter_mut().enumerate().for_each(|(i, px)| {
            let g = (near[i] * 0.4 + far[i] * 0.6) * k;
            px[0] += g;
            px[1] += g * 0.22;
            px[2] += g * 0.06;
        });
    }

    // Local-contrast inputs (only computed when used).
    let lp: Vec<f32> = if p.clarity != 0.0 || p.texture != 0.0 { px3(&img).par_iter().map(|c| fpow(luma(*c), 1.0 / 2.2)).collect() } else { vec![] };
    let big = (p.clarity != 0.0).then(|| blur(&lp, w, h, big_w.max(big_h) / 60.0));
    let small = (p.texture != 0.0).then(|| blur(&lp, w, h, 4.0 * scale.max(0.25)));
    let haze = (p.dehaze != 0.0).then(|| {
        let dark: Vec<f32> = px3(&img).par_iter().map(|c| c[0].min(c[1]).min(c[2])).collect();
        blur(&dark, w, h, big_w.max(big_h) / 80.0)
    });
    let curves = curve_tables(p);
    let lut = (!p.lut.is_empty() && p.lut_amt > 0.0).then(|| load_cube(&p.lut)).flatten();

    let g_exp = 2.0 - p.g_blend / 100.0 * 1.5;
    let mid = 0.5 - p.g_bal / 250.0;
    let grade_cols: Vec<[f32; 3]> = p.grade.iter().map(|g| {
        let c = hsv_to_rgb(g[0], 1.0, 1.0);
        let m = luma(c);
        c.map(|v| (v - m) * g[1] / 100.0 * 0.25)
    }).collect();
    let tone_on = big.is_some() || small.is_some() || [p.blacks, p.whites, p.highlights, p.shadows, p.contrast, p.c_sh, p.c_da, p.c_li, p.c_hi].iter().any(|v| *v != 0.0);
    let sat_on = p.saturation != 0.0 || p.vibrance != 0.0 || p.dehaze != 0.0;
    let mixer_on = p.hsl.iter().flatten().any(|v| *v != 0.0);
    let grade_on = p.grade.iter().any(|g| g[1] != 0.0 || g[2] != 0.0);
    let vr_round = p.vig_round / 100.0;
    let grain_px = (1.0 + p.gr_size / 25.0) * scale.max(0.05);

    // The same stage on the GPU when there is one (LUTs stay on the CPU).
    let on_gpu = lut.is_none() && w * h >= crate::gpu::MIN_PIXELS && {
        let mut prm = vec![0f32; 128 + 4 * 256];
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        prm[2] = x0 as f32;
        prm[4..8].copy_from_slice(&[cx, cy, big_w, big_h]);
        prm[8..33].copy_from_slice(&[
            flag(haze.is_some()), p.dehaze, flag(tone_on), flag(big.is_some()), p.clarity, flag(small.is_some()), p.texture,
            flag(sat_on), p.saturation, p.vibrance, flag(curves.is_some()), flag(p.curve_lum), flag(mixer_on), flag(grade_on), mid, g_exp,
            p.vig, vr_round, p.vig_mid, p.vig_fea, p.vig_hi, p.gr_amt, grain_px, p.gr_rough, p.gr_hi,
        ]);
        prm[33..42].copy_from_slice(&[p.blacks, p.whites, p.highlights, p.shadows, p.contrast, p.c_sh, p.c_da, p.c_li, p.c_hi]);
        for b in 0..8 {
            prm[48 + b * 3..51 + b * 3].copy_from_slice(&p.hsl[b]);
        }
        for z in 0..4 {
            prm[72 + z * 3..75 + z * 3].copy_from_slice(&grade_cols[z]);
            prm[84 + z] = p.grade[z][2];
        }
        if let Some(t) = &curves {
            for k in 0..4 {
                prm[128 + k * 256..128 + (k + 1) * 256].copy_from_slice(&t[k]);
            }
        }
        let lpo = (big.is_some() || small.is_some()).then_some(lp.as_slice());
        crate::gpu::tone_stage(img.as_mut(), w, y0 as f32, [lpo, big.as_deref(), small.as_deref(), haze.as_deref()], prm)
    };
    if !on_gpu {
    img.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for (x, px) in px3_mut(row).iter_mut().enumerate() {
            let i = y * w + x;
            let (gx, gy) = ((x + x0) as f32, (y + y0) as f32);
            let mut c = *px;
            if let Some(hz) = &haze {
                let s = (p.dehaze / 100.0 * 0.9).clamp(-0.9, 0.9);
                c = if s > 0.0 { c.map(|v| ((v - s * hz[i]) / (1.0 - s * hz[i])).max(0.0)) } else { c.map(|v| v + (-s) * 0.25 * (0.6 - v).max(0.0)) };
            }
            // Tone on luminance, colour ratios preserved (skipped when it would change nothing).
            if tone_on {
            let y0 = luma(c);
            let mut l = fpow(y0.max(0.0), 1.0 / 2.2);
            let midw = bump(l, 0.5, 0.6);
            if let Some(b) = &big {
                l += p.clarity / 100.0 * 0.6 * (lp[i] - b[i]) * midw;
            }
            if let Some(b) = &small {
                l += p.texture / 100.0 * 1.2 * (lp[i] - b[i]);
            }
            l = tone(l, p);
            let y1 = fpow(l.max(0.0), 2.2);
            c = if y0 > 1e-6 { c.map(|v| v * y1 / y0) } else { [y1; 3] };
            }
            // Saturation / vibrance.
            if sat_on {
                let yl = luma(c);
                let (_, sat0, _) = rgb_to_hsv(c);
                let k = 1.0 + p.saturation / 100.0 + p.vibrance / 100.0 * (1.0 - sat0).powi(2) + p.dehaze / 400.0;
                c = c.map(|v| (yl + (v - yl) * k).max(0.0));
            }
            // Display encode, then perceptual colour work.
            let mut e = c.map(linear_to_srgb);
            if let Some(t) = &curves {
                if p.curve_lum {
                    let l = luma(e);
                    let k = if l > 1e-5 { table(&t[0], l) / l } else { 1.0 };
                    e = [0, 1, 2].map(|j| table(&t[j + 1], e[j] * k));
                } else {
                    e = [0, 1, 2].map(|k| table(&t[k + 1], table(&t[0], e[k])));
                }
            }
            if mixer_on {
                let (hh, s, v) = rgb_to_hsv(e);
                let (a, b, t) = band_pair(hh);
                let [dh, ds, dl] = [0, 1, 2].map(|k| p.hsl[a][k] * (1.0 - t) + p.hsl[b][k] * t);
                e = hsv_to_rgb(hh + dh * 0.3, (s * (1.0 + ds / 100.0)).clamp(0.0, 1.0), (v * (1.0 + dl / 100.0 * 0.4 * s)).max(0.0));
            }
            if grade_on {
                let le = luma(e).clamp(0.0, 1.0);
                let ws = fpow((1.0 - le / mid).clamp(0.0, 1.0), g_exp);
                let wh = fpow(((le - mid) / (1.0 - mid)).clamp(0.0, 1.0), g_exp);
                let wm = (1.0 - ws - wh).max(0.0);
                for (z, wz) in [ws, wm, wh, 1.0].into_iter().enumerate() {
                    let lum = p.grade[z][2] / 100.0 * 0.2;
                    e = [0, 1, 2].map(|j| e[j] + wz * (grade_cols[z][j] + lum));
                }
            }
            if let Some(l) = &lut {
                let v = l.apply(e);
                e = [0, 1, 2].map(|k| e[k] + (v[k] - e[k]) * p.lut_amt / 100.0);
            }
            // Post-crop vignette.
            if p.vig != 0.0 {
                let (nx, ny) = ((gx - cx) / cx, (gy - cy) / cy);
                // roundness: -100 rounded rectangle, 0 ellipse fitting the frame, +100 circle
                let t = vr_round.max(0.0);
                let (ax, ay) = (1.0 + t * (big_w / big_h - 1.0).max(0.0), 1.0 + t * (big_h / big_w - 1.0).max(0.0));
                let (ex, ey) = (nx * ax / ax.max(ay), ny * ay / ax.max(ay));
                let pe = 2.0 - vr_round.min(0.0) * 4.0;
                let r = if pe == 2.0 { (ex * ex + ey * ey).sqrt() * std::f32::consts::FRAC_1_SQRT_2 } else { fpow(fpow(ex.abs(), pe) + fpow(ey.abs(), pe), 1.0 / pe) / 2f32.powf(1.0 / pe) };
                let start = p.vig_mid / 100.0 * 0.9;
                let m = smooth(start, start + 0.05 + p.vig_fea / 100.0 * 0.6, r);
                let a = p.vig / 100.0;
                let protect = 1.0 - p.vig_hi / 100.0 * smooth(0.6, 1.0, luma(e));
                e = if a < 0.0 { e.map(|v| v * (1.0 + a * m * protect)) } else { e.map(|v| v + (1.0 - v) * a * m) };
            }
            if p.gr_amt > 0.0 {
                let (gx, gy) = (gx / grain_px, gy / grain_px);
                let rough = p.gr_rough / 100.0;
                let n = value_noise(gx, gy) * (1.0 - rough * 0.5) + value_noise(gx * 2.3 + 17.0, gy * 2.3 + 5.0) * rough * 0.5 - 0.5;
                let le = luma(e);
                let amt = p.gr_amt / 100.0 * 0.22 * (0.3 + bump(le, 0.5, 0.7)) * (1.0 - (1.0 - p.gr_hi / 100.0) * smooth(0.55, 0.95, le));
                e = e.map(|v| v + n * amt);
            }
            *px = e;
        }
    });
    }

    detail(&mut img, p, scale);
    img.iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
    img
}

// ---------- point curves ----------

/// 256-entry table through the points, with a monotone cubic (Fritsch–Carlson): smooth, and never overshoots
/// between points, so a curve can't fold back on itself.
pub fn curve_table(pts: &[[f32; 2]]) -> [f32; 256] {
    let mut p: Vec<[f32; 2]> = pts.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    p.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-4);
    if p.len() < 2 {
        return std::array::from_fn(|i| i as f32 / 255.0);
    }
    let n = p.len();
    let d: Vec<f32> = (0..n - 1).map(|i| (p[i + 1][1] - p[i][1]) / (p[i + 1][0] - p[i][0])).collect();
    let mut m: Vec<f32> = (0..n).map(|i| if i == 0 { d[0] } else if i == n - 1 { d[n - 2] } else if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 }).collect();
    for i in 0..n - 1 {
        if d[i] == 0.0 {
            (m[i], m[i + 1]) = (0.0, 0.0);
        } else {
            let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                (m[i], m[i + 1]) = (t * a * d[i], t * b * d[i]);
            }
        }
    }
    std::array::from_fn(|k| {
        let x = k as f32 / 255.0;
        if x <= p[0][0] {
            return p[0][1].clamp(0.0, 1.0);
        }
        if x >= p[n - 1][0] {
            return p[n - 1][1].clamp(0.0, 1.0);
        }
        let i = (0..n - 1).find(|&i| x <= p[i + 1][0]).unwrap_or(n - 2);
        let hgt = p[i + 1][0] - p[i][0];
        let t = (x - p[i][0]) / hgt;
        let (t2, t3) = (t * t, t * t * t);
        let y = (2.0 * t3 - 3.0 * t2 + 1.0) * p[i][1] + (t3 - 2.0 * t2 + t) * hgt * m[i] + (-2.0 * t3 + 3.0 * t2) * p[i + 1][1] + (t3 - t2) * hgt * m[i + 1];
        y.clamp(0.0, 1.0)
    })
}

fn curve_tables(p: &Params) -> Option<[[f32; 256]; 4]> {
    p.curves.iter().any(|c| c.len() >= 2).then(|| std::array::from_fn(|i| curve_table(&p.curves[i])))
}

/// Look up a 0..1 value in a table, interpolating between entries.
pub fn table(t: &[f32; 256], v: f32) -> f32 {
    let f = v.clamp(0.0, 1.0) * 255.0;
    let i = (f as usize).min(254);
    t[i] + (t[i + 1] - t[i]) * (f - i as f32)
}

// ---------- .cube LUTs ----------

pub struct Lut {
    n: usize,
    data: Vec<[f32; 3]>,
}

impl Lut {
    /// Parse an Adobe/Resolve .cube file (3D only).
    pub fn parse(text: &str) -> Result<Lut, String> {
        let (mut n, mut data, mut lo, mut hi) = (0, vec![], [0.0f32; 3], [1.0f32; 3]);
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let mut it = line.split_whitespace();
            match it.next() {
                Some("LUT_3D_SIZE") => n = it.next().and_then(|v| v.parse().ok()).ok_or("bad LUT_3D_SIZE")?,
                Some("LUT_1D_SIZE") => return Err("1D LUTs are not supported".into()),
                Some("DOMAIN_MIN") => lo = parse3(it)?,
                Some("DOMAIN_MAX") => hi = parse3(it)?,
                Some(first) if first.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.') => {
                    data.push(parse3(std::iter::once(first).chain(it))?)
                }
                _ => {} // TITLE and other keywords
            }
        }
        if n < 2 || data.len() != n * n * n {
            return Err(format!("expected {} entries, found {}", n * n * n, data.len()));
        }
        // Fold the input domain into the table so lookups always take 0..1.
        if lo != [0.0; 3] || hi != [1.0; 3] {
            return Err("only DOMAIN 0..1 is supported".into());
        }
        Ok(Lut { n, data })
    }

    /// Trilinear lookup; red changes fastest in the file.
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let n = self.n;
        let f = c.map(|v| v.clamp(0.0, 1.0) * (n - 1) as f32);
        let i = f.map(|v| (v as usize).min(n - 2));
        let t = [0, 1, 2].map(|k| f[k] - i[k] as f32);
        let at = |r: usize, g: usize, b: usize| self.data[(b * n + g) * n + r];
        let mut out = [0.0; 3];
        for (dr, dg, db) in [(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0), (0, 0, 1), (1, 0, 1), (0, 1, 1), (1, 1, 1)] {
            let w = (if dr == 1 { t[0] } else { 1.0 - t[0] }) * (if dg == 1 { t[1] } else { 1.0 - t[1] }) * (if db == 1 { t[2] } else { 1.0 - t[2] });
            let v = at(i[0] + dr, i[1] + dg, i[2] + db);
            (0..3).for_each(|k| out[k] += v[k] * w);
        }
        out
    }
}

fn parse3<'a>(mut it: impl Iterator<Item = &'a str>) -> Result<[f32; 3], String> {
    let mut v = [0.0; 3];
    for x in &mut v {
        *x = it.next().and_then(|s| s.parse().ok()).ok_or("bad number in LUT")?;
    }
    Ok(v)
}

/// LUTs are read once per path and kept (they're small: a 33³ LUT is about 430 KB).
pub fn load_cube(path: &str) -> Option<std::sync::Arc<Lut>> {
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, Option<Arc<Lut>>>>> = OnceLock::new();
    let mut c = CACHE.get_or_init(Default::default).lock().ok()?;
    c.entry(path.to_string()).or_insert_with(|| std::fs::read_to_string(path).ok().and_then(|t| Lut::parse(&t).ok()).map(Arc::new)).clone()
}

/// Noise reduction, defringe and sharpening, on the display-encoded image.
fn detail(img: &mut Rgb32FImage, p: &Params, scale: f32) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    // Tiny previews and thumbnails: sharpening is invisible noise. Small exports still get it.
    let sharpen = p.sh_amt > 0.0 && (scale > 0.3 || w.max(h) >= 600);
    if !sharpen && p.nr_lum == 0.0 && p.nr_col == 0.0 && p.o_fringe == 0.0 {
        return;
    }
    let y: Vec<f32> = px3(img).par_iter().map(|c| luma(*c)).collect();
    let cb: Vec<f32> = px3(img).par_iter().zip(&y).map(|(c, l)| c[2] - l).collect();
    let cr: Vec<f32> = px3(img).par_iter().zip(&y).map(|(c, l)| c[0] - l).collect();
    let edge_src = blur(&y, w, h, 1.0 * scale.max(0.5));
    let col_sigma = (1.0 + p.nr_col / 12.0) * scale.max(0.25);
    let (cb_b, cr_b) = if p.nr_col > 0.0 || p.o_fringe > 0.0 { (blur(&cb, w, h, col_sigma), blur(&cr, w, h, col_sigma)) } else { (cb.clone(), cr.clone()) };
    let y_nr = (p.nr_lum > 0.0).then(|| blur(&y, w, h, (0.6 + p.nr_lum / 60.0) * scale.max(0.25)));
    let y_sh = sharpen.then(|| blur(&y, w, h, p.sh_rad * scale));
    img.par_chunks_mut(w * 3).enumerate().for_each(|(row_i, row)| {
        for (x, px) in px3_mut(row).iter_mut().enumerate() {
            let i = row_i * w + x;
            let edge = ((y[i] - edge_src[i]).abs() * 12.0).min(1.0);
            let mut l = y[i];
            if let Some(b) = &y_nr {
                let keep = edge * p.nr_det / 100.0;
                l += (b[i] - l) * p.nr_lum / 100.0 * (1.0 - keep);
            }
            if let Some(b) = &y_sh {
                let mask = smooth(p.sh_mask / 100.0 * 0.3, p.sh_mask / 100.0 * 0.3 + 0.02, edge * 0.3);
                let halo = 0.9 + (p.sh_det / 100.0 - 0.25) * 0.4;
                l += (y[i] - b[i]) * p.sh_amt / 100.0 * 1.5 * halo * if p.sh_mask > 0.0 { mask } else { 1.0 };
            }
            let t = (p.nr_col / 100.0).min(1.0);
            let (mut b_, mut r_) = (cb[i] + (cb_b[i] - cb[i]) * t, cr[i] + (cr_b[i] - cr[i]) * t);
            if p.o_fringe > 0.0 {
                // Purple/green fringes: pull chroma toward the local average on strong edges.
                let (hue, _, _) = rgb_to_hsv(*px);
                let fringe_hue = (250.0..330.0).contains(&hue) || (80.0..160.0).contains(&hue);
                if fringe_hue {
                    let f = (p.o_fringe / 20.0 * edge * 2.0).min(1.0);
                    b_ += (cb_b[i] * 0.3 - b_) * f;
                    r_ += (cr_b[i] * 0.3 - r_) * f;
                }
            }
            // Rebuild RGB from luma + two chroma differences.
            let r = l + r_;
            let b = l + b_;
            let g = (l - 0.2126 * r - 0.0722 * b) / 0.7152;
            *px = [r, g, b];
        }
    });
}

/// One-click auto tone: exposure so the median sits at mid grey, mild white/black clipping.
pub fn auto_tone(src: &Rgb32FImage, p: &Params) -> Params {
    let mut q = p.clone();
    q.exposure = 0.0;
    let mut img = geometry(src, &q);
    if q.neg.enabled {
        negative::convert(&mut img, &q.neg); // measure the positive, not the orange negative
    }
    let mut ys: Vec<f32> = px3(&img).par_iter().step_by(7).map(|c| luma(*c)).collect();
    if ys.is_empty() {
        return q;
    }
    ys.sort_by(f32::total_cmp);
    let med = ys[ys.len() / 2].max(1e-5);
    q.exposure = (0.18 / med).log2().clamp(-4.0, 4.0);
    q.whites = 10.0;
    q.blacks = -8.0;
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_pair_matches_band_weight() {
        for k in 0..36000 {
            let h = k as f32 * 0.01;
            let (a, b, t) = band_pair(h);
            for i in 0..8 {
                let want = band_weight(h, i);
                let got = if i == a { 1.0 - t } else if i == b { t } else { 0.0 };
                assert!((want - got).abs() < 1e-4, "hue {h} band {i}: {want} vs {got}");
            }
        }
    }

    #[test]
    fn default_params_keep_mid_grey() {
        let mut img = Rgb32FImage::new(8, 8);
        img.iter_mut().for_each(|v| *v = 0.18);
        let mut p = Params::default();
        p.sh_amt = 0.0;
        p.nr_col = 0.0;
        let out = render(&img, &p, 1.0);
        let v = out.get_pixel(4, 4).0;
        assert!((v[0] - linear_to_srgb(0.18)).abs() < 0.02, "{v:?}");
        assert!((v[0] - v[2]).abs() < 0.01, "neutral grey stays neutral: {v:?}");
    }

    #[test]
    fn exposure_and_temp_move_the_right_way() {
        let mut img = Rgb32FImage::new(4, 4);
        img.iter_mut().for_each(|v| *v = 0.18);
        let base = render(&img, &Params::default(), 1.0).get_pixel(1, 1).0;
        let bright = render(&img, &Params { exposure: 1.0, ..Default::default() }, 1.0).get_pixel(1, 1).0;
        let warm = render(&img, &Params { temp: 8000.0, ..Default::default() }, 1.0).get_pixel(1, 1).0;
        assert!(bright[1] > base[1]);
        assert!(warm[0] > warm[2], "higher temp warms: {warm:?}");
    }

    #[test]
    fn mixer_bands_cover_every_hue() {
        for h in 0..360 {
            let s: f32 = (0..8).map(|b| band_weight(h as f32, b)).sum();
            assert!((s - 1.0).abs() < 1e-3, "hue {h}: {s}");
        }
    }

    #[test]
    fn border_pads_to_aspect() {
        let img = Rgb32FImage::new(300, 200);
        let out = add_border(img.clone(), &Border { width: 5.0, white: true, aspect: 0.8, ..Default::default() });
        assert_eq!(out.dimensions(), (330, 413)); // 15 px each side, then padded to 4:5
        assert_eq!(out.get_pixel(0, 0).0, [1.0; 3]);
        assert_eq!(out.get_pixel(165, 206).0, [0.0; 3], "photo stays centred");
        assert_eq!(add_border(img, &Border::default()).dimensions(), (300, 200));
    }

    #[test]
    fn paste_keeps_this_photos_negative_conversion() {
        let mut scan = Params::default();
        scan.neg.enabled = true;
        let digital = Params { exposure: 0.5, ..Default::default() };
        let pasted = scan.with_look_of(&digital);
        assert!(pasted.neg.enabled && pasted.exposure == 0.5);
    }

    #[test]
    fn auto_tone_measures_the_positive() {
        // An evenly spread negative converts to a positive with its median near mid grey, so Auto stays near 0 EV.
        // Measuring the orange negative itself instead gives about -1 EV.
        let img = Rgb32FImage::from_fn(64, 8, |x, _| image::Rgb([0.9, 0.55, 0.3].map(|b: f32| b * 10f32.powf(-(0.1 + x as f32 / 63.0 * 0.2)))));
        let mut p = Params::default();
        p.neg.enabled = true;
        p.neg.stock = "generic_c41".into();
        let ev = auto_tone(&img, &p).exposure;
        assert!(ev.abs() < 0.5, "{ev}");
    }

    #[test]
    fn caption_adds_a_band_under_the_photo() {
        let img = Rgb32FImage::new(400, 300);
        let b = Border { width: 2.0, white: true, aspect: 0.0, caption: "Portra 400".into() };
        let (cw, ch, ox, oy, px) = border_layout(400, 300, &b);
        assert!(px > 0.0 && ch > 300 + 2 * 8 && ox == 8 && oy == 8 && cw == 416);
        let out = add_border(img, &b);
        // Some caption ink below the photo.
        let inked = (0..cw).flat_map(|x| (300 + 8..ch).map(move |y| (x, y))).any(|(x, y)| out.get_pixel(x, y).0[0] < 0.5);
        assert!(inked);
    }

    #[test]
    fn spot_heal_removes_a_dust_speck() {
        let mut img = Rgb32FImage::from_pixel(100, 100, image::Rgb([0.4; 3]));
        for y in 48..53 {
            for x in 48..53 {
                img.put_pixel(x, y, image::Rgb([0.0; 3]));
            }
        }
        let (sx, sy) = find_source(&img, 50.0, 50.0, 6.0);
        let p = Params { spots: vec![Spot { x: 0.5, y: 0.5, r: 0.06, sx: sx / 100.0, sy: sy / 100.0, ..Default::default() }], ..Default::default() };
        heal(&mut img, &p, (100, 100), (0.0, 0.0));
        assert!((img.get_pixel(50, 50).0[0] - 0.4).abs() < 0.02, "{:?}", img.get_pixel(50, 50));
    }

    #[test]
    fn graduated_filter_darkens_only_its_side() {
        let img = Rgb32FImage::from_pixel(10, 100, image::Rgb([0.18; 3]));
        let g = GradFilter { a: [0.5, 0.0], b: [0.5, 0.5], exposure: -1.0, ..Default::default() };
        let p = Params { grads: vec![g], sh_amt: 0.0, nr_col: 0.0, ..Default::default() };
        let out = render(&img, &p, 1.0);
        let (top, bottom) = (out.get_pixel(5, 0).0[1], out.get_pixel(5, 90).0[1]);
        assert!(top < bottom - 0.1 && (bottom - linear_to_srgb(0.18)).abs() < 0.02, "{top} {bottom}");
    }

    #[test]
    fn window_matches_the_same_part_of_a_full_render() {
        let img = Rgb32FImage::from_fn(120, 80, |x, y| image::Rgb([x as f32 / 120.0, y as f32 / 80.0, 0.3]));
        let p = Params { vig: -50.0, gr_amt: 30.0, clarity: 30.0, t_rot: 3.0, sh_amt: 0.0, nr_col: 0.0, ..Default::default() };
        let full = render(&img, &p, 1.0);
        let part = render_window(&img, &p, 1.0, Some([40, 20, 30, 30]));
        for (x, y) in [(5, 5), (15, 12), (25, 25)] {
            let (a, b) = (full.get_pixel(40 + x, 20 + y).0, part.get_pixel(x, y).0);
            assert!((0..3).all(|k| (a[k] - b[k]).abs() < 0.03), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn halation_glows_red_around_highlights_only() {
        let mut img = Rgb32FImage::from_pixel(400, 400, image::Rgb([0.02; 3]));
        for y in 180..220 {
            for x in 180..220 {
                img.put_pixel(x, y, image::Rgb([4.0; 3]));
            }
        }
        let p = Params { hal_amt: 80.0, hal_size: 100.0, sh_amt: 0.0, nr_col: 0.0, ..Default::default() };
        let near = render(&img, &p, 1.0).get_pixel(228, 200).0;
        let plain = render(&img, &Params { sh_amt: 0.0, nr_col: 0.0, ..Default::default() }, 1.0).get_pixel(228, 200).0;
        assert!(near[0] > plain[0] + 0.05 && near[0] > near[2] + 0.05, "{near:?} vs {plain:?}");
        assert!((render(&img, &p, 1.0).get_pixel(2, 2).0[0] - plain[0]).abs() < 0.03, "far corner untouched");
    }

    #[test]
    fn point_curve_passes_through_its_points_and_stays_monotone() {
        let t = curve_table(&[[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]]);
        assert!((table(&t, 0.25) - 0.15).abs() < 0.01 && (table(&t, 0.75) - 0.85).abs() < 0.01);
        assert!(t.windows(2).all(|w| w[1] >= w[0] - 1e-6));
        assert_eq!(curve_table(&[])[128], 128.0 / 255.0);
    }

    #[test]
    fn cube_lut_identity_and_invert() {
        let mut text = String::from("TITLE \"t\"\nLUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text += &format!("{} {} {}\n", 1 - r, 1 - g, 1 - b);
                }
            }
        }
        let lut = Lut::parse(&text).unwrap();
        let v = lut.apply([0.2, 0.5, 0.9]);
        assert!((v[0] - 0.8).abs() < 1e-4 && (v[1] - 0.5).abs() < 1e-4 && (v[2] - 0.1).abs() < 1e-4);
        assert!(Lut::parse("LUT_3D_SIZE 2\n0 0 0\n").is_err());
    }

    #[test]
    fn brush_mask_only_changes_painted_area() {
        let img = Rgb32FImage::from_pixel(100, 100, image::Rgb([0.18; 3]));
        let b = BrushMask { dabs: vec![[0.2, 0.2, 0.08, 1.0]], exposure: 1.0, ..Default::default() };
        let out = render(&img, &Params { brushes: vec![b], sh_amt: 0.0, nr_col: 0.0, ..Default::default() }, 1.0);
        assert!(out.get_pixel(20, 20).0[1] > out.get_pixel(80, 80).0[1] + 0.1);
    }

    #[test]
    fn eyedropper_neutralises_the_picked_colour() {
        let warm = [0.5, 0.4, 0.25];
        let (t, ti) = solve_wb(warm);
        let g = wb_gains(t, ti);
        let v = [0, 1, 2].map(|k| warm[k] * g[k]);
        assert!((v[0] / v[1] - 1.0).abs() < 0.04 && (v[2] / v[1] - 1.0).abs() < 0.04, "{t} {ti} {v:?}");
        assert!(t < 5500.0, "a warm patch needs a cooler setting: {t}");
    }

    #[test]
    fn quarter_turn_swaps_sides_and_carries_the_crop() {
        let src = Rgb32FImage::from_fn(40, 20, |x, _| image::Rgb([x as f32 / 40.0; 3]));
        let mut p = Params { crop: [0.25, 0.0, 0.0, 0.0], ..Default::default() };
        p.rotate(true);
        let out = geometry(&src, &p);
        assert_eq!(out.dimensions(), (20, 30));
        // Turned clockwise, the source's left edge is the top: the trimmed quarter is gone from the top.
        assert!(out.get_pixel(10, 0).0[0] > 0.24, "{:?}", out.get_pixel(10, 0));
        p.rotate(false);
        assert_eq!(p.crop, [0.25, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn quarter_turn_on_a_flipped_photo_turns_the_way_you_asked() {
        // A bright marker at the source's top-left: flipped, it shows top-right; turned clockwise from there,
        // it must end up bottom-right (top-right corner goes to bottom-right).
        let src = Rgb32FImage::from_fn(40, 20, |x, y| image::Rgb([if x < 4 && y < 4 { 1.0 } else { 0.0 }; 3]));
        let mut p = Params { flip_h: true, ..Default::default() };
        p.rotate(true);
        let out = geometry(&src, &p);
        let (w, h) = out.dimensions();
        assert_eq!((w, h), (20, 40));
        assert!(out.get_pixel(w - 2, h - 2).0[0] > 0.5, "marker should be bottom-right");
        p.rotate(false);
        assert_eq!(p.rot90, 0);
    }

    #[test]
    fn box_blur_matches_the_direct_sum() {
        let (w, h) = (37, 150);
        let src: Vec<f32> = (0..w * h).map(|i| ((i * 7919) % 101) as f32 / 101.0).collect();
        for r in [1, 3, 9, 40] {
            let mut fast = vec![0f32; w * h];
            box_cols(&src, &mut fast, w, h, r);
            for y in 0..h {
                for x in 0..w {
                    let want: f32 = (0..=2 * r).map(|k| src[(y + k).saturating_sub(r).min(h - 1) * w + x]).sum::<f32>() / (2 * r + 1) as f32;
                    assert!((fast[y * w + x] - want).abs() < 1e-4, "r {r} at {x},{y}: {} vs {want}", fast[y * w + x]);
                }
            }
        }
    }

    #[test]
    fn srgb_round_trip() {
        for v in [0.0, 0.01, 0.18, 0.5, 1.0] {
            assert!((srgb_to_linear(linear_to_srgb(v)) - v).abs() < 1e-4);
        }
    }

    #[test]
    fn painted_stroke_heals_a_scratch_and_clone_copies_turned() {
        // Flat grey with a dark scratch along row 50, and a bright square to clone.
        let mut img = Rgb32FImage::from_pixel(100, 100, image::Rgb([0.4, 0.4, 0.4]));
        for x in 10..90 {
            img.put_pixel(x, 50, image::Rgb([0.0, 0.0, 0.0]));
        }
        let stroke = Spot { x: 0.1, y: 0.505, r: 0.03, sx: 0.1, sy: 0.3, path: vec![[0.5, 0.505], [0.9, 0.505]], ..Default::default() };
        let out = geometry(&img, &Params::default());
        let mut healed = out.clone();
        heal(&mut healed, &Params { spots: vec![stroke], ..Default::default() }, (100, 100), (0.0, 0.0));
        for x in 15..85 {
            assert!((healed.get_pixel(x, 50)[0] - 0.4).abs() < 0.02, "x {x}: {}", healed.get_pixel(x, 50)[0]);
        }
        // Clone a marker at (20, 20) onto (70, 70), turned 90°: the marker's right neighbour lands below.
        let mut img = Rgb32FImage::from_pixel(100, 100, image::Rgb([0.2, 0.2, 0.2]));
        img.put_pixel(20, 20, image::Rgb([1.0, 1.0, 1.0]));
        img.put_pixel(23, 20, image::Rgb([0.8, 0.0, 0.0]));
        let c = Spot { x: 0.7, y: 0.7, r: 0.08, sx: 0.2, sy: 0.2, clone: true, angle: -90.0, ..Default::default() };
        heal(&mut img, &Params { spots: vec![c], ..Default::default() }, (100, 100), (0.0, 0.0));
        assert!(img.get_pixel(70, 70)[0] > 0.9);
        assert!(img.get_pixel(70, 73)[0] > 0.6 && img.get_pixel(70, 73)[1] < 0.1, "{:?}", img.get_pixel(70, 73));
    }

    #[test]
    fn masks_combine_shapes_and_apply_their_own_settings() {
        // Left half dark, right half bright.
        let src = Rgb32FImage::from_fn(100, 60, |x, _| image::Rgb(if x < 50 { [0.03; 3] } else { [0.4; 3] }));
        let lum = |img: &Rgb32FImage, x: u32, y: u32| luma(img.get_pixel(x, y).0);
        let base = render(&src, &Params::default(), 1.0);
        let bright = |shape: Shape, op2: Option<(Shape, MaskOp)>| {
            let mut parts = vec![MaskPart { shape, op: MaskOp::Add, invert: false }];
            if let Some((s2, op)) = op2 {
                parts.push(MaskPart { shape: s2, op, invert: false });
            }
            let m = Mask { parts, adj: Params { exposure: 1.0, ..Default::default() }, ..Default::default() };
            render(&src, &Params { masks: vec![m], ..Default::default() }, 1.0)
        };
        // Linear: full effect at the top, none at the bottom.
        let out = bright(Shape::Linear { a: [0.5, 0.0], b: [0.5, 1.0] }, None);
        assert!(lum(&out, 70, 2) > lum(&base, 70, 2) + 0.1 && (lum(&out, 70, 58) - lum(&base, 70, 58)).abs() < 0.02);
        // Radial in the middle of the right half, minus a whole-photo brightness range above the dark half: still on.
        let radial = Shape::Radial { c: [0.75, 0.5], rx: 0.1, ry: 0.1, angle: 0.0, feather: 20.0 };
        let out = bright(radial.clone(), None);
        assert!(lum(&out, 75, 30) > lum(&base, 75, 30) + 0.1 && (lum(&out, 98, 2) - lum(&base, 98, 2)).abs() < 0.01);
        // Intersect with a luminance range that excludes the bright half: nothing left.
        let out = bright(radial, Some((Shape::Luminance { lo: 0.0, hi: 0.3, soft: 0.02 }, MaskOp::Intersect)));
        assert!((lum(&out, 75, 30) - lum(&base, 75, 30)).abs() < 0.01);
        // Whole photo minus the dark range: only the bright half changes.
        let out = bright(Shape::All, Some((Shape::Luminance { lo: 0.0, hi: 0.3, soft: 0.02 }, MaskOp::Subtract)));
        assert!((lum(&out, 20, 30) - lum(&base, 20, 30)).abs() < 0.01 && lum(&out, 80, 30) > lum(&base, 80, 30) + 0.1);
        // Saved settings hold only what changed, and come back the same.
        let m = Mask { parts: vec![MaskPart { shape: Shape::All, op: MaskOp::Add, invert: false }], adj: Params { exposure: 0.7, ..Default::default() }, ..Default::default() };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"exposure\":0.69") && !json.contains("contrast"), "{json}");
        assert!(serde_json::from_str::<Mask>(&json).unwrap() == m);
        // Old graduated filters become masks.
        let mut old: Params = serde_json::from_str(r#"{"grads":[{"a":[0.5,0.0],"b":[0.5,1.0],"exposure":-1.0}]}"#).unwrap();
        old.migrate();
        assert!(old.grads.is_empty() && old.masks.len() == 1 && old.masks[0].adj.exposure == -1.0);
    }

    #[test]
    fn preset_amount_mixes_every_number() {
        let a = Params { exposure: 0.0, contrast: 10.0, rating: 3, ..Default::default() };
        let b = Params { exposure: 1.0, contrast: 30.0, hsl: [[10.0, 0.0, 0.0]; 8], lut: "x.cube".into(), ..Default::default() };
        let h = Params::blend(&a, &b, 0.5);
        assert_eq!((h.exposure, h.contrast, h.hsl[3][0], h.rating), (0.5, 20.0, 5.0, 3));
        assert_eq!(h.lut, "x.cube");
        assert!(Params::blend(&a, &b, 0.0) == a);
        assert_eq!(Params::blend(&a, &b, 2.0).exposure, 2.0);
    }

    #[test]
    fn marks_follow_the_photo_when_the_geometry_changes() {
        let (w, h) = (300, 200);
        let old = Params { spots: vec![Spot { x: 0.7, y: 0.4, r: 0.02, sx: 0.6, sy: 0.4, ..Default::default() }], ..Default::default() };
        let mut new = Params { t_rot: 6.0, t_vert: 20.0, ..old.clone() };
        remap_marks(&old, &mut new, w, h);
        let (a, b) = (Mapper::new(&old, w, h), Mapper::new(&new, w, h));
        let (s0, s1) = (&old.spots[0], &new.spots[0]);
        let p0 = a.map(s0.x * w as f32, s0.y * h as f32);
        let p1 = b.map(s1.x * w as f32, s1.y * h as f32);
        assert!((p0.0 - p1.0).abs() < 0.05 && (p0.1 - p1.1).abs() < 0.05, "{p0:?} {p1:?}");
        assert!(s1.x != s0.x && s1.r != s0.r);
    }

    #[test]
    fn lens_profile_straightens_and_brightens_corners() {
        let name = crate::lens::find(&crate::lens::Shot { make: "Sony".into(), lens: "FE 24-70mm F2.8 GM".into(), ..Default::default() }).unwrap().name();
        let src = Rgb32FImage::from_pixel(300, 200, image::Rgb([0.5; 3]));
        let p = Params { lens: name, lens_focal: 24.0, lens_f: 2.8, lens_crop: 1.0, ..Default::default() };
        let out = geometry(&src, &p);
        assert_eq!(out.dimensions(), (300, 200));
        // Vignetting correction lifts the corners; nothing is pulled in from outside the frame.
        let (c, m) = (out.get_pixel(1, 1).0[1], out.get_pixel(150, 100).0[1]);
        assert!(c > m && c > 0.55 && (m - 0.5).abs() < 0.01, "corner {c} middle {m}");
    }
}

#[cfg(test)]
mod fast_math {
    use super::fpow;
    #[test]
    fn fpow_matches_powf() {
        for p in [1.0 / 2.2, 2.2, 0.5, 2.0, 3.7, 0.25] {
            let mut x = 1e-7f32;
            while x < 1e4 {
                let (a, b) = (fpow(x, p), x.powf(p));
                assert!(((a - b) / b).abs() < 2e-5, "{x}^{p}: {a} vs {b}");
                x *= 1.013;
            }
        }
        assert_eq!(fpow(0.0, 2.2), 0.0);
        assert_eq!(fpow(-1.0, 2.2), 0.0);
    }
}

/// Render timings on a real photo:
/// `BENCH_FILE=photo.NEF cargo test --release bench_pipeline -- --ignored --nocapture`
/// (BENCH_HEAVY=1 or BENCH_DEFAULT=1 repeat one case, for a profiler).
#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;
    fn t<T>(name: &str, f: impl FnOnce() -> T) -> T { let s = Instant::now(); let r = f(); eprintln!("{name:<28} {:>8.1} ms", s.elapsed().as_secs_f64() * 1e3); r }
    #[test]
    #[ignore]
    fn bench_pipeline() {
        let path = std::env::var("BENCH_FILE").unwrap();
        if std::env::var("BENCH_HEAVY").is_ok() {
            let full = crate::load::load(std::path::Path::new(&path)).unwrap();
            let prev = crate::load::fit(&full, 1400);
            let mut p = Params::default();
            p.exposure = 0.5; p.contrast = 20.0; p.highlights = -40.0; p.shadows = 30.0; p.texture = 20.0; p.clarity = 30.0; p.dehaze = 15.0;
            p.vibrance = 20.0; p.sh_amt = 60.0; p.nr_lum = 30.0; p.nr_col = 25.0; p.gr_amt = 30.0; p.hal_amt = 30.0; p.vig = -20.0; p.t_rot = 2.0;
            p.hsl[0] = [10.0, 10.0, 0.0]; p.grade[0] = [200.0, 20.0, 0.0];
            let sc = prev.width() as f32 / full.width() as f32;
            for _ in 0..30 { t("render 1400 heavy", || render(&prev, &p, sc)); }
            return;
        }
        if std::env::var("BENCH_STAGES").is_ok() {
            let full = crate::load::load(std::path::Path::new(&path)).unwrap();
            let prev = crate::load::fit(&full, 1400);
            let sc = prev.width() as f32 / full.width() as f32;
            let mut p = Params::default();
            p.exposure = 0.5; p.contrast = 20.0; p.highlights = -40.0; p.shadows = 30.0; p.texture = 20.0; p.clarity = 30.0; p.dehaze = 15.0;
            p.vibrance = 20.0; p.sh_amt = 60.0; p.nr_lum = 30.0; p.nr_col = 25.0; p.gr_amt = 30.0; p.hal_amt = 30.0; p.vig = -20.0; p.t_rot = 2.0;
            p.hsl[0] = [10.0, 10.0, 0.0]; p.grade[0] = [200.0, 20.0, 0.0];
            let v = |name: &str, f: &dyn Fn(&mut Params)| { let mut q = p.clone(); f(&mut q); render(&prev, &q, sc); let s = Instant::now(); for _ in 0..5 { render(&prev, &q, sc); } eprintln!("{name:<20} {:>7.1} ms", s.elapsed().as_secs_f64() * 200.0); };
            v("heavy", &|_| {});
            v("no clarity/tex", &|q| { q.clarity = 0.0; q.texture = 0.0; });
            v("no dehaze", &|q| q.dehaze = 0.0);
            v("no halation", &|q| q.hal_amt = 0.0);
            v("no NR", &|q| { q.nr_lum = 0.0; q.nr_col = 0.0; });
            v("no sharpen", &|q| q.sh_amt = 0.0);
            v("no grain", &|q| q.gr_amt = 0.0);
            v("no rotate", &|q| q.t_rot = 0.0);
            v("no mixer/grade", &|q| { q.hsl[0] = [0.0; 3]; q.grade[0] = [0.0; 3]; });
            v("no mixer", &|q| { q.hsl[0] = [0.0; 3]; });
            v("no grade", &|q| { q.grade[0] = [0.0; 3]; });
            v("no vignette", &|q| { q.vig = 0.0; });
            v("no sat", &|q| { q.vibrance = 0.0; });
            return;
        }
        if std::env::var("BENCH_DEFAULT").is_ok() {
            let full = crate::load::load(std::path::Path::new(&path)).unwrap();
            for _ in 0..4 { t("render full default", || render(&full, &Params::default(), 1.0)); }
            return;
        }
        let full = t("load full", || crate::load::load(std::path::Path::new(&path)).unwrap());
        eprintln!("size {}x{}", full.width(), full.height());
        let prev = t("fit 1400", || crate::load::fit(&full, 1400));
        let p0 = Params::default();
        for _ in 0..2 { t("render 1400 default", || render(&prev, &p0, prev.width() as f32 / full.width() as f32)); }
        let mut p = Params::default();
        p.exposure = 0.5; p.contrast = 20.0; p.highlights = -40.0; p.shadows = 30.0; p.texture = 20.0; p.clarity = 30.0; p.dehaze = 15.0;
        p.vibrance = 20.0; p.sh_amt = 60.0; p.nr_lum = 30.0; p.nr_col = 25.0; p.gr_amt = 30.0; p.hal_amt = 30.0; p.vig = -20.0; p.t_rot = 2.0;
        p.hsl[0] = [10.0, 10.0, 0.0]; p.grade[0] = [200.0, 20.0, 0.0];
        let sc = prev.width() as f32 / full.width() as f32;
        for _ in 0..2 { t("render 1400 heavy", || render(&prev, &p, sc)); }
        let draft = crate::load::fit(&full, 700);
        t("render 700 heavy (drag)", || render(&draft, &p, draft.width() as f32 / full.width() as f32));
        t("render full heavy (export)", || render(&full, &p, 1.0));
        t("render full default", || render(&full, &p0, 1.0));
    }
}

#[cfg(test)]
mod edge_fuzz {
    use super::*;
    /// Tiny frames and extreme settings must render without panics or NaNs.
    #[test]
    fn tiny_and_extreme() {
        let mut variants: Vec<(&str, Params)> = vec![("default", Params::default())];
        let mut p = Params::default();
        p.clarity = 100.0; p.texture = 100.0; p.dehaze = 100.0; p.hal_amt = 100.0; p.gr_amt = 100.0; p.nr_lum = 100.0; p.nr_col = 100.0; p.o_fringe = 20.0; p.vig = -100.0; p.sh_amt = 150.0;
        variants.push(("heavy", p.clone()));
        let mut q = p.clone(); q.crop = [0.49, 0.49, 0.49, 0.49]; variants.push(("tiny crop", q));
        let mut q = p.clone(); q.crop = [0.6, 0.0, 0.6, 0.0]; variants.push(("overlap crop", q));
        let mut q = p.clone(); q.t_rot = 45.0; q.t_vert = 100.0; q.t_hor = -100.0; q.t_scale = 50.0; q.o_dist = 100.0; variants.push(("geometry", q));
        let mut q = p.clone(); q.rot90 = 3; q.flip_h = true; variants.push(("rot90", q));
        let mut q = p.clone(); q.border = Border { width: 100.0, white: true, aspect: 0.8, caption: "ferrotype".into() }; variants.push(("border", q));
        let mut q = p.clone(); q.neg.enabled = true; variants.push(("negative", q));
        let mut q = p.clone(); q.spots = vec![Spot { x: 0.5, y: 0.5, r: 0.5, sx: 0.9, sy: 0.1, path: vec![[0.1, 0.1], [0.9, 0.9]], clone: false, angle: 30.0 }]; variants.push(("spots", q));
        let mut q = p.clone(); q.masks = vec![Mask { parts: vec![MaskPart { shape: Shape::Radial { c: [0.5, 0.5], rx: 0.0, ry: 0.0, angle: 0.0, feather: 100.0 }, op: MaskOp::Add, invert: false }, MaskPart { shape: Shape::Linear { a: [0.5, 0.5], b: [0.5, 0.5] }, op: MaskOp::Add, invert: false }, MaskPart { shape: Shape::Brush { dabs: vec![[0.5, 0.5, 0.0, 1.0]] }, op: MaskOp::Add, invert: false }, MaskPart { shape: Shape::Luminance { lo: 0.5, hi: 0.5, soft: 0.0 }, op: MaskOp::Intersect, invert: true }], opacity: 100.0, feather: 100.0, ..Default::default() }]; variants.push(("masks", q));
        let mut q = p.clone(); q.curves = [vec![[0.5, 0.5]], vec![[0.0, 1.0], [1.0, 0.0]], vec![[0.3, 0.3], [0.3, 0.6]], vec![]]; variants.push(("curves", q));
        let mut q = p.clone(); q.exposure = 5.0; q.temp = 50000.0; q.tint = 150.0; variants.push(("extreme wb", q));
        let mut q = p.clone(); q.lut = "/nonexistent.cube".into(); q.lut_amt = 100.0; variants.push(("missing lut", q));
        for (w, h) in [(1, 1), (1, 9), (9, 1), (2, 2), (3, 300), (300, 3), (17, 13)] {
            let img = Rgb32FImage::from_fn(w, h, |x, y| image::Rgb([(x as f32 * 0.37).fract(), (y as f32 * 0.21).fract(), 0.5]));
            for (name, p) in &variants {
                for sc in [1.0, 0.05] {
                    let o = render(&img, p, sc);
                    assert!(o.iter().all(|v| v.is_finite()), "NaN in {w}x{h} {name} at scale {sc}");
                }
            }
        }
    }
}

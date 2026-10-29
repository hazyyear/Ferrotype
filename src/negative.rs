//! Film negative conversion: film base → density → per-channel levels → stock profile → print look.
use image::Rgb32FImage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Color,
    Cine,
    Bw,
    Slide,
}

pub struct Stock {
    pub id: &'static str,
    pub brand: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    pub contrast: f32, // print gamma multiplier
    pub sat: f32,
    pub warm: f32, // + warmer (red up, blue down)
    pub green: f32, // + greener
}

const fn s(id: &'static str, brand: &'static str, name: &'static str, kind: Kind, contrast: f32, sat: f32, warm: f32, green: f32) -> Stock {
    Stock { id, brand, name, kind, contrast, sat, warm, green }
}

use Kind::*;
// shortcut: profiles are hand-tuned looks from published descriptions, not measured characteristic curves;
// replace with data from scans of calibration targets when available.
pub const STOCKS: &[Stock] = &[
    s("generic_c41", "Generic", "Generic C-41", Color, 1.0, 1.0, 0.0, 0.0),
    s("generic_ecn2", "Generic", "Generic ECN-2", Cine, 0.95, 0.95, -0.01, 0.0),
    s("generic_bw", "Generic", "Generic B&W", Bw, 1.0, 0.0, 0.0, 0.0),
    s("generic_e6", "Generic", "Slide film (E-6, no inversion)", Slide, 1.0, 1.0, 0.0, 0.0),
    s("portra160", "Kodak", "Portra 160", Color, 0.9, 0.85, 0.02, 0.0),
    s("portra400", "Kodak", "Portra 400", Color, 0.93, 0.9, 0.03, 0.0),
    s("portra800", "Kodak", "Portra 800", Color, 1.0, 1.0, 0.04, 0.0),
    s("ektar100", "Kodak", "Ektar 100", Color, 1.12, 1.35, 0.01, -0.005),
    s("gold200", "Kodak", "Gold 200", Color, 1.02, 1.1, 0.05, -0.005),
    s("colorplus200", "Kodak", "ColorPlus 200", Color, 1.0, 1.05, 0.045, 0.0),
    s("ultramax400", "Kodak", "UltraMax 400", Color, 1.06, 1.2, 0.035, 0.0),
    s("proimage100", "Kodak", "Pro Image 100", Color, 1.0, 1.05, 0.03, 0.005),
    s("vision3_50d", "Kodak", "Vision3 50D", Cine, 0.92, 0.95, 0.0, 0.0),
    s("vision3_250d", "Kodak", "Vision3 250D", Cine, 0.92, 0.95, 0.005, 0.0),
    s("vision3_200t", "Kodak", "Vision3 200T", Cine, 0.92, 0.95, -0.03, 0.0),
    s("vision3_500t", "Kodak", "Vision3 500T", Cine, 0.95, 0.95, -0.035, 0.0),
    s("trix400", "Kodak", "Tri-X 400", Bw, 1.2, 0.0, 0.0, 0.0),
    s("tmax100", "Kodak", "T-Max 100", Bw, 1.08, 0.0, 0.0, 0.0),
    s("tmax400", "Kodak", "T-Max 400", Bw, 1.1, 0.0, 0.0, 0.0),
    s("tmax3200", "Kodak", "T-Max P3200", Bw, 1.25, 0.0, 0.0, 0.0),
    s("doublex", "Kodak", "Double-X 5222", Bw, 1.18, 0.0, 0.0, 0.0),
    s("aerocolor", "Kodak", "Aerocolor (Santa Color 100)", Color, 1.08, 1.15, 0.01, 0.005),
    s("pro400h", "Fujifilm", "Pro 400H", Color, 0.92, 0.85, -0.01, 0.015),
    s("superia400", "Fujifilm", "Superia X-TRA 400", Color, 1.04, 1.1, -0.015, 0.012),
    s("fuji200", "Fujifilm", "Fujicolor 200 / C200", Color, 1.0, 1.05, -0.01, 0.008),
    s("fuji400", "Fujifilm", "Fujicolor 400", Color, 1.02, 1.08, -0.008, 0.01),
    s("acros", "Fujifilm", "Acros II 100", Bw, 1.1, 0.0, 0.0, 0.0),
    s("neopan400", "Fujifilm", "Neopan 400", Bw, 1.12, 0.0, 0.0, 0.0),
    s("cs50d", "CineStill", "50D", Cine, 0.95, 1.0, 0.01, 0.0),
    s("cs400d", "CineStill", "400D", Cine, 1.0, 1.05, 0.02, 0.0),
    s("cs800t", "CineStill", "800T", Cine, 1.05, 1.1, -0.04, 0.0),
    s("hp5", "Ilford", "HP5 Plus", Bw, 1.06, 0.0, 0.0, 0.0),
    s("fp4", "Ilford", "FP4 Plus", Bw, 1.08, 0.0, 0.0, 0.0),
    s("delta100", "Ilford", "Delta 100", Bw, 1.08, 0.0, 0.0, 0.0),
    s("delta400", "Ilford", "Delta 400", Bw, 1.1, 0.0, 0.0, 0.0),
    s("delta3200", "Ilford", "Delta 3200", Bw, 1.3, 0.0, 0.0, 0.0),
    s("panf50", "Ilford", "Pan F Plus 50", Bw, 1.15, 0.0, 0.0, 0.0),
    s("sfx200", "Ilford", "SFX 200", Bw, 1.12, 0.0, 0.0, 0.0),
    s("xp2", "Ilford", "XP2 Super (C-41)", Bw, 1.02, 0.0, 0.0, 0.0),
    s("phoenix200", "Harman", "Phoenix 200", Color, 1.15, 1.4, 0.03, -0.015),
    s("kentmere100", "Harman", "Kentmere Pan 100", Bw, 1.1, 0.0, 0.0, 0.0),
    s("kentmere400", "Harman", "Kentmere Pan 400", Bw, 1.12, 0.0, 0.0, 0.0),
    s("lomo100", "Lomography", "Color 100", Color, 1.08, 1.2, 0.02, 0.0),
    s("lomo400", "Lomography", "Color 400", Color, 1.1, 1.25, 0.025, 0.0),
    s("lomo800", "Lomography", "Color 800", Color, 1.1, 1.3, 0.03, 0.0),
    s("lomo_purple", "Lomography", "LomoChrome Purple", Color, 1.05, 1.2, 0.0, -0.06),
    s("lomo_metropolis", "Lomography", "LomoChrome Metropolis", Color, 1.15, 0.45, 0.01, 0.0),
    s("lomo_turquoise", "Lomography", "LomoChrome Turquoise", Color, 1.05, 1.15, -0.07, 0.02),
    s("foma100", "Foma", "Fomapan 100", Bw, 1.12, 0.0, 0.0, 0.0),
    s("foma200", "Foma", "Fomapan 200", Bw, 1.12, 0.0, 0.0, 0.0),
    s("foma400", "Foma", "Fomapan 400", Bw, 1.15, 0.0, 0.0, 0.0),
    s("apx100", "Agfa", "APX 100", Bw, 1.1, 0.0, 0.0, 0.0),
    s("apx400", "Agfa", "APX 400", Bw, 1.12, 0.0, 0.0, 0.0),
    s("rpx25", "Rollei", "RPX 25", Bw, 1.15, 0.0, 0.0, 0.0),
    s("rpx100", "Rollei", "RPX 100", Bw, 1.1, 0.0, 0.0, 0.0),
    s("rpx400", "Rollei", "RPX 400", Bw, 1.12, 0.0, 0.0, 0.0),
    s("retro80s", "Rollei", "Retro 80S", Bw, 1.2, 0.0, 0.0, 0.0),
    s("reflx_lab", "Reflx Lab", "Reflx Lab 800 (ECN-2)", Cine, 1.0, 1.05, -0.02, 0.0),
    s("velvia50", "Fujifilm", "Velvia 50 (E-6)", Slide, 1.0, 1.0, 0.0, 0.0),
    s("provia100f", "Fujifilm", "Provia 100F (E-6)", Slide, 1.0, 1.0, 0.0, 0.0),
    s("ektachrome", "Kodak", "Ektachrome E100 (E-6)", Slide, 1.0, 1.0, 0.0, 0.0),
];

pub fn stock(id: &str) -> &'static Stock {
    STOCKS.iter().find(|s| s.id == id).unwrap_or(&STOCKS[0])
}

/// Film speed from the stock name ("Portra 400" → 400), if it has one.
pub fn iso(s: &Stock) -> Option<f32> {
    s.name.split([' ', '/']).find_map(|w| w.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse().ok())
}

/// Typical grain (amount, size) and halation for a stock. Faster film: more and bigger grain.
/// CineStill and Reflx Lab have no anti-halation layer (remjet removed), so highlights glow red.
/// shortcut: rule-of-thumb values from film speed and type, not measured from scans.
/// Grain amount, grain size, halation, and how much grain is left in the highlights (0–100) for a stock.
/// Prints from negatives have clean highlights (dense negative, little grain reaches the paper); slides and
/// B&W keep a little more.
pub fn character(s: &Stock) -> (f32, f32, f32, f32) {
    let stops = (iso(s).unwrap_or(200.0) / 100.0).log2();
    let bw = if s.kind == Kind::Bw { 6.0 } else { 0.0 };
    let grain = (12.0 + 8.0 * stops + bw).clamp(5.0, 60.0);
    let size = (20.0 + 8.0 * stops).clamp(10.0, 60.0);
    let halation = match s.kind {
        Kind::Bw => 0.0,
        Kind::Cine if s.brand == "CineStill" || s.brand == "Reflx Lab" => 55.0,
        _ => 12.0,
    };
    let highlights = match s.kind {
        Kind::Bw => 35.0,
        Kind::Slide => 40.0,
        _ => 20.0,
    };
    (grain, size, halation, highlights)
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum Look {
    Neutral,
    Frontier,
    Noritsu,
    Linear,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NegParams {
    pub enabled: bool,
    pub stock: String,
    pub look: Look,
    /// Film base colour (linear). None = auto (brightest part of the scan).
    pub base: Option<[f32; 3]>,
    /// Per-channel density [black, white] points. None = analyse this frame; Some = locked (roll lock).
    pub levels: Option<[[f32; 2]; 3]>,
    pub density: f32,
    pub contrast: f32,
    pub cy: f32,
    pub mg: f32,
    pub yb: f32,
    pub toe: f32,
    pub shoulder: f32,
}

impl Default for NegParams {
    fn default() -> Self {
        Self { enabled: false, stock: "portra400".into(), look: Look::Neutral, base: None, levels: None, density: 0.0, contrast: 0.0, cy: 0.0, mg: 0.0, yb: 0.0, toe: 40.0, shoulder: 55.0 }
    }
}

/// A film stock profile of your own: a starting stock plus your tweaks, saved as a small `.ferrostock` file
/// (JSON) to reuse or share. The scan-specific film base and levels are left out.
#[derive(Clone, Serialize, Deserialize)]
pub struct CustomStock {
    pub name: String,
    pub neg: NegParams,
}

pub const CUSTOM_EXT: &str = "ferrostock";

pub fn custom_dir() -> Option<std::path::PathBuf> {
    crate::keys::dirs_config().map(|d| d.join("stocks"))
}

/// Your saved stock profiles, by name.
pub fn custom_stocks() -> Vec<(std::path::PathBuf, CustomStock)> {
    let mut v: Vec<_> = custom_dir().and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten().filter_map(|e| {
        let p = e.path();
        let c: CustomStock = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
        Some((p, c))
    }).collect();
    v.sort_by(|a, b| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()));
    v
}

impl CustomStock {
    pub fn new(name: &str, neg: &NegParams) -> Self {
        CustomStock { name: name.trim().to_string(), neg: NegParams { enabled: true, base: None, levels: None, ..neg.clone() } }
    }

    pub fn to_file(&self, path: &std::path::Path) -> Result<(), String> {
        crate::write_atomic(path, serde_json::to_string_pretty(self).map_err(|e| e.to_string())?.as_bytes()).map_err(|e| e.to_string())
    }

    /// Keep it in the profiles folder (named after the profile).
    pub fn install(&self) -> Result<std::path::PathBuf, String> {
        let dir = custom_dir().ok_or("no config folder")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let safe: String = self.name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
        let path = dir.join(format!("{}.{CUSTOM_EXT}", if safe.trim().is_empty() { "stock" } else { safe.trim() }));
        self.to_file(&path)?;
        Ok(path)
    }

    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let c: CustomStock = serde_json::from_str(&std::fs::read_to_string(path).map_err(|e| e.to_string())?).map_err(|e| format!("not a stock profile: {e}"))?;
        if !STOCKS.iter().any(|s| s.id == c.neg.stock) {
            return Err(format!("it builds on a stock this version doesn't have ({})", c.neg.stock));
        }
        Ok(c)
    }

    /// Apply to a photo: this profile's conversion, the photo's own film base and levels.
    pub fn apply(&self, to: &NegParams) -> NegParams {
        NegParams { enabled: true, base: to.base, levels: to.levels, ..self.neg.clone() }
    }
}

fn percentile(v: &mut [f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let i = ((v.len() - 1) as f32 * q) as usize;
    *v.select_nth_unstable_by(i, f32::total_cmp).1
}

/// Pixel stride that samples about 100k pixels: plenty for percentiles, and the same cost at any resolution.
fn stride(img: &Rgb32FImage) -> usize {
    (img.width() as usize * img.height() as usize / 100_000).max(1)
}

/// Auto film base: the brightest 0.2% of the scan, which is the unexposed rebate when it is in frame.
/// A colour negative's clear film is orange: clearly more red than green, and more green than blue.
pub fn orange_mask(base: [f32; 3]) -> bool {
    base[0] > base[1] * 1.15 && base[1] > base[2] * 1.15
}

pub fn auto_base(img: &Rgb32FImage) -> [f32; 3] {
    [0, 1, 2].map(|c| {
        let mut v: Vec<f32> = img.chunks(3).step_by(stride(img)).map(|p| p[c]).collect();
        percentile(&mut v, 0.998).max(1e-4)
    })
}

fn bw(st: &Stock) -> bool {
    st.kind == Kind::Bw
}

fn densities(c: &[f32], base: [f32; 3], mono: bool) -> [f32; 3] {
    let d = [0, 1, 2].map(|i| -crate::develop::log2_fast((c[i] / base[i]).clamp(1e-4, 1.0)) * std::f32::consts::LOG10_2);
    if mono { [(d[0] + d[1] + d[2]) / 3.0; 3] } else { d }
}

/// Analyse a frame: base and per-channel density range. Used for auto mode and for "lock roll".
pub fn analyse(img: &Rgb32FImage, np: &NegParams) -> ([f32; 3], [[f32; 2]; 3]) {
    let base = np.base.unwrap_or_else(|| auto_base(img));
    let mono = bw(stock(&np.stock));
    let ds: Vec<[f32; 3]> = img.chunks(3).step_by(stride(img)).map(|c| densities(c, base, mono)).collect();
    let levels = [0, 1, 2].map(|c| {
        let mut v: Vec<f32> = ds.iter().map(|d| d[c]).collect();
        let lo = percentile(&mut v, 0.001);
        let hi = percentile(&mut v, 0.998);
        [lo, hi.max(lo + 0.05)]
    });
    (base, levels)
}

/// Quadratic toe and shoulder joins: C1-continuous soft clipping of the print curve.
fn roll_off(x: f32, toe: f32, shoulder: f32) -> f32 {
    let (a, b) = ((toe / 100.0 * 0.3).max(1e-4), (shoulder / 100.0 * 0.3).max(1e-4));
    if x < -a { 0.0 } else if x < a { (x + a).powi(2) / (4.0 * a) } else if x <= 1.0 - b { x } else if x < 1.0 + b { 1.0 - (1.0 - x + b).powi(2) / (4.0 * b) } else { 1.0 }
}

/// Invert a negative scan in place, producing a linear positive for the develop pipeline.
pub fn convert(img: &mut Rgb32FImage, np: &NegParams) {
    let st = stock(&np.stock);
    if st.kind == Kind::Slide {
        return;
    }
    let (base, levels) = match (np.base, np.levels) {
        (Some(b), Some(l)) => (b, l), // roll lock: nothing to measure
        _ => {
            let (b, l) = analyse(img, np);
            (b, np.levels.unwrap_or(l))
        }
    };
    let mono = bw(st);
    let (look_sat, look_warm, look_con, curve) = match np.look {
        Look::Neutral => (1.0, 0.0, 1.0, true),
        Look::Frontier => (1.08, 0.01, 1.04, true), // Fuji Frontier: a touch warmer, punchy greens
        Look::Noritsu => (0.96, 0.0, 1.02, true),   // Noritsu: neutral, softer colour
        Look::Linear => (0.9, 0.0, 0.85, false),    // flat, for heavy grading later
    };
    let gamma = st.contrast * look_con * (1.0 + np.contrast / 100.0 * 0.5);
    let sat = if mono { 0.0 } else { st.sat * look_sat };
    let warm = st.warm + look_warm;
    let bal = [np.cy / 100.0 * 0.1 + warm, np.mg / 100.0 * 0.1 + st.green, np.yb / 100.0 * 0.1 - warm];
    let dens = np.density / 100.0 * 0.3;
    crate::develop::px3_mut(img).par_iter_mut().for_each(|px| {
        let d = densities(px, base, mono);
        let mut n = [0, 1, 2].map(|c| (d[c] - levels[c][0]) / (levels[c][1] - levels[c][0]));
        n = [0, 1, 2].map(|c| (0.5 + (n[c] - 0.5 - dens) * gamma) + bal[c]);
        let m = (n[0] + n[1] + n[2]) / 3.0;
        n = n.map(|v| m + (v - m) * sat);
        n = n.map(|v| if curve { roll_off(v, np.toe, np.shoulder) } else { v.clamp(0.0, 1.0) });
        *px = n.map(|v| crate::develop::fpow(v, 2.2));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic negative: orange base on the left (rebate), dense (bright scene) on the right.
    fn fake_negative() -> Rgb32FImage {
        let base = [0.9, 0.55, 0.3];
        Rgb32FImage::from_fn(64, 8, |x, _| {
            let scene = x as f32 / 63.0; // 0 = black, 1 = white
            image::Rgb(base.map(|b| b * 10f32.powf(-scene * 1.2)))
        })
    }

    #[test]
    fn inverts_and_neutralises_the_mask() {
        let mut img = fake_negative();
        convert(&mut img, &NegParams { enabled: true, stock: "generic_c41".into(), ..Default::default() });
        let dark = img.get_pixel(0, 0).0;
        let light = img.get_pixel(63, 0).0;
        let mid = img.get_pixel(32, 0).0;
        assert!(dark.iter().all(|v| *v < 0.02), "film base becomes black: {dark:?}");
        assert!(light.iter().all(|v| *v > 0.8), "dense area becomes white: {light:?}");
        assert!((mid[0] - mid[2]).abs() < 0.03, "orange mask removed, mids neutral: {mid:?}");
    }

    #[test]
    fn slide_film_is_untouched_and_bw_is_mono() {
        let mut img = fake_negative();
        let before = img.clone();
        convert(&mut img, &NegParams { enabled: true, stock: "velvia50".into(), ..Default::default() });
        assert_eq!(img, before);
        convert(&mut img, &NegParams { enabled: true, stock: "hp5".into(), ..Default::default() });
        let p = img.get_pixel(20, 0).0;
        assert!((p[0] - p[1]).abs() < 1e-5 && (p[1] - p[2]).abs() < 1e-5);
    }

    #[test]
    fn faster_film_has_more_grain_and_cinestill_glows() {
        let (g160, ..) = character(stock("portra160"));
        let (g800, _, h, _) = character(stock("cs800t"));
        assert!(g800 > g160 && h > 40.0 && iso(stock("cs800t")) == Some(800.0));
    }

    #[test]
    fn stock_ids_are_unique() {
        for (i, a) in STOCKS.iter().enumerate() {
            assert!(STOCKS[i + 1..].iter().all(|b| b.id != a.id), "{}", a.id);
        }
    }
}

#[cfg(test)]
mod custom_tests {
    use super::*;

    #[test]
    fn shared_stock_drops_the_scan_and_keeps_the_look() {
        let neg = NegParams { stock: STOCKS[3].id.to_string(), base: Some([0.8, 0.5, 0.3]), levels: Some([[0.1, 0.9]; 3]), contrast: 12.0, ..Default::default() };
        let c = CustomStock::new("  My Portra  ", &neg);
        assert_eq!(c.name, "My Portra");
        assert!(c.neg.base.is_none() && c.neg.levels.is_none());
        let f = std::env::temp_dir().join("ferrotype-test.ferrostock");
        c.to_file(&f).unwrap();
        let back = CustomStock::from_file(&f).unwrap();
        let other = NegParams { base: Some([0.7, 0.4, 0.2]), ..Default::default() };
        let applied = back.apply(&other);
        assert_eq!(applied.contrast, 12.0);
        assert_eq!(applied.base, Some([0.7, 0.4, 0.2]));
        std::fs::write(&f, "not json").unwrap();
        assert!(CustomStock::from_file(&f).is_err());
    }
}

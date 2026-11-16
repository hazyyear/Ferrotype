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
    /// Box speed. 0 = none to give: a generic profile, a film sold in several speeds (Kodachrome) or one rated
    /// over a range (LomoChrome 100-400).
    pub iso: u16,
    pub contrast: f32, // print gamma multiplier
    pub sat: f32,
    pub warm: f32, // + warmer (red up, blue down)
    pub green: f32, // + greener
}

#[allow(clippy::too_many_arguments)]
const fn s(id: &'static str, brand: &'static str, name: &'static str, kind: Kind, iso: u16, contrast: f32, sat: f32, warm: f32, green: f32) -> Stock {
    Stock { id, brand, name, kind, iso, contrast, sat, warm, green }
}

use Kind::*;
// shortcut: profiles are hand-tuned looks from published descriptions, not measured characteristic curves;
// replace with data from scans of calibration targets when available.
// Names, speeds and processes follow the makers' datasheets. Ids are saved in sidecars, roll notes and
// .ferrostock files: never rename or remove one. Keep each brand's stocks together (the list is headed by brand).
// `warm` is a look left after the conversion has neutralised the film base and each channel's range, not the
// film's colour balance: the tungsten stocks get a slightly cool one, as they look in mixed and street light.
pub const STOCKS: &[Stock] = &[
    s("generic_c41", "Generic", "Generic C-41", Color, 0, 1.0, 1.0, 0.0, 0.0),
    s("generic_ecn2", "Generic", "Generic ECN-2", Cine, 0, 0.95, 0.95, -0.01, 0.0),
    s("generic_bw", "Generic", "Generic B&W", Bw, 0, 1.0, 0.0, 0.0, 0.0),
    s("generic_e6", "Generic", "Slide film (E-6, no inversion)", Slide, 0, 1.0, 1.0, 0.0, 0.0),
    s("portra160", "Kodak", "Portra 160", Color, 160, 0.9, 0.85, 0.02, 0.0),
    s("portra400", "Kodak", "Portra 400", Color, 400, 0.93, 0.9, 0.03, 0.0),
    s("portra800", "Kodak", "Portra 800", Color, 800, 1.0, 1.0, 0.04, 0.0),
    s("ektar100", "Kodak", "Ektar 100", Color, 100, 1.12, 1.35, 0.01, -0.005),
    s("gold200", "Kodak", "Gold 200", Color, 200, 1.02, 1.1, 0.05, -0.005),
    s("colorplus200", "Kodak", "ColorPlus 200", Color, 200, 1.0, 1.05, 0.045, 0.0),
    s("ultramax400", "Kodak", "UltraMax 400", Color, 400, 1.06, 1.2, 0.035, 0.0),
    s("proimage100", "Kodak", "Pro Image 100", Color, 100, 1.0, 1.05, 0.03, 0.005),
    s("kodacolor100", "Kodak", "Kodacolor 100", Color, 100, 1.0, 1.05, 0.035, 0.0),
    s("kodacolor200", "Kodak", "Kodacolor 200", Color, 200, 1.0, 1.05, 0.045, 0.0),
    // Aerial film: no orange mask, rated ISO A 125 by Kodak and sold for cameras as ISO 100.
    s("aerocolor", "Kodak", "Aerocolor IV (Santa Color)", Color, 100, 1.08, 1.15, 0.01, 0.005),
    s("vision3_50d", "Kodak", "Vision3 50D", Cine, 50, 0.92, 0.95, 0.0, 0.0),
    s("vision3_250d", "Kodak", "Vision3 250D", Cine, 250, 0.92, 0.95, 0.005, 0.0),
    s("vision3_200t", "Kodak", "Vision3 200T", Cine, 200, 0.92, 0.95, -0.03, 0.0),
    s("vision3_500t", "Kodak", "Vision3 500T", Cine, 500, 0.95, 0.95, -0.035, 0.0),
    s("trix400", "Kodak", "Tri-X 400", Bw, 400, 1.2, 0.0, 0.0, 0.0),
    s("tmax100", "Kodak", "T-Max 100", Bw, 100, 1.08, 0.0, 0.0, 0.0),
    s("tmax400", "Kodak", "T-Max 400", Bw, 400, 1.1, 0.0, 0.0, 0.0),
    // P3200 and Delta 3200 are about ISO 1000 films made to be pushed: the speed here is the one on the box.
    s("tmax3200", "Kodak", "T-Max P3200", Bw, 3200, 1.25, 0.0, 0.0, 0.0),
    // 5222 is the film code, not a speed: EI 250 in daylight, 200 in tungsten light.
    s("doublex", "Kodak", "Double-X 5222", Bw, 250, 1.18, 0.0, 0.0, 0.0),
    s("ektachrome", "Kodak", "Ektachrome E100 (E-6)", Slide, 100, 1.0, 1.0, 0.0, 0.0),
    s("kodachrome", "Kodak", "Kodachrome (no inversion)", Slide, 0, 1.0, 1.0, 0.0, 0.0),
    s("pro400h", "Fujifilm", "Pro 400H", Color, 400, 0.92, 0.85, -0.01, 0.015),
    s("superia400", "Fujifilm", "Superia X-TRA 400", Color, 400, 1.04, 1.1, -0.015, 0.012),
    s("fuji200", "Fujifilm", "Fujicolor C200", Color, 200, 1.0, 1.05, -0.01, 0.008),
    // The current Fujifilm 200 and 400 are made in the USA by Kodak and look like its consumer films.
    s("fujifilm200", "Fujifilm", "Fujifilm 200", Color, 200, 1.02, 1.1, 0.045, -0.005),
    s("fuji400", "Fujifilm", "Fujifilm 400", Color, 400, 1.05, 1.15, 0.03, 0.0),
    s("acros", "Fujifilm", "Acros II 100", Bw, 100, 1.1, 0.0, 0.0, 0.0),
    s("neopan400", "Fujifilm", "Neopan 400", Bw, 400, 1.12, 0.0, 0.0, 0.0),
    s("velvia50", "Fujifilm", "Velvia 50 (E-6)", Slide, 50, 1.0, 1.0, 0.0, 0.0),
    s("velvia100", "Fujifilm", "Velvia 100 (E-6)", Slide, 100, 1.0, 1.0, 0.0, 0.0),
    s("provia100f", "Fujifilm", "Provia 100F (E-6)", Slide, 100, 1.0, 1.0, 0.0, 0.0),
    s("cs50d", "CineStill", "50D", Cine, 50, 0.95, 1.0, 0.01, 0.0),
    s("cs400d", "CineStill", "400D", Cine, 400, 1.0, 1.05, 0.02, 0.0),
    s("cs800t", "CineStill", "800T", Cine, 800, 1.05, 1.1, -0.04, 0.0),
    s("cs_bwxx", "CineStill", "BwXX", Bw, 250, 1.18, 0.0, 0.0, 0.0),
    s("hp5", "Ilford", "HP5 Plus", Bw, 400, 1.06, 0.0, 0.0, 0.0),
    s("fp4", "Ilford", "FP4 Plus", Bw, 125, 1.08, 0.0, 0.0, 0.0),
    s("delta100", "Ilford", "Delta 100", Bw, 100, 1.08, 0.0, 0.0, 0.0),
    s("delta400", "Ilford", "Delta 400", Bw, 400, 1.1, 0.0, 0.0, 0.0),
    s("delta3200", "Ilford", "Delta 3200", Bw, 3200, 1.3, 0.0, 0.0, 0.0),
    s("panf50", "Ilford", "Pan F Plus 50", Bw, 50, 1.15, 0.0, 0.0, 0.0),
    s("ortho80", "Ilford", "Ortho Plus 80", Bw, 80, 1.12, 0.0, 0.0, 0.0),
    s("sfx200", "Ilford", "SFX 200", Bw, 200, 1.12, 0.0, 0.0, 0.0),
    s("xp2", "Ilford", "XP2 Super (C-41)", Bw, 400, 1.02, 0.0, 0.0, 0.0),
    s("phoenix200", "Harman", "Phoenix 200", Color, 200, 1.15, 1.4, 0.03, -0.015),
    s("phoenix2", "Harman", "Phoenix II 200", Color, 200, 1.06, 1.2, 0.02, -0.005),
    s("kentmere100", "Harman", "Kentmere Pan 100", Bw, 100, 1.1, 0.0, 0.0, 0.0),
    s("kentmere200", "Harman", "Kentmere Pan 200", Bw, 200, 1.1, 0.0, 0.0, 0.0),
    s("kentmere400", "Harman", "Kentmere Pan 400", Bw, 400, 1.12, 0.0, 0.0, 0.0),
    s("lomo100", "Lomography", "Color 100", Color, 100, 1.08, 1.2, 0.02, 0.0),
    s("lomo400", "Lomography", "Color 400", Color, 400, 1.1, 1.25, 0.025, 0.0),
    s("lomo800", "Lomography", "Color 800", Color, 800, 1.1, 1.3, 0.03, 0.0),
    s("lomo_purple", "Lomography", "LomoChrome Purple 100-400", Color, 0, 1.05, 1.2, 0.0, -0.06),
    s("lomo_metropolis", "Lomography", "LomoChrome Metropolis 100-400", Color, 0, 1.15, 0.45, 0.01, 0.0),
    s("lomo_turquoise", "Lomography", "LomoChrome Turquoise 100-400", Color, 0, 1.05, 1.15, -0.07, 0.02),
    s("foma100", "Foma", "Fomapan 100", Bw, 100, 1.12, 0.0, 0.0, 0.0),
    s("foma200", "Foma", "Fomapan 200", Bw, 200, 1.12, 0.0, 0.0, 0.0),
    s("foma400", "Foma", "Fomapan 400", Bw, 400, 1.15, 0.0, 0.0, 0.0),
    // The APX sold today (AgfaPhoto, "new emulsion"); the Agfa originals ended in 2005.
    s("apx100", "AgfaPhoto", "APX 100", Bw, 100, 1.1, 0.0, 0.0, 0.0),
    s("apx400", "AgfaPhoto", "APX 400", Bw, 400, 1.12, 0.0, 0.0, 0.0),
    s("rpx25", "Rollei", "RPX 25", Bw, 25, 1.15, 0.0, 0.0, 0.0),
    s("rpx100", "Rollei", "RPX 100", Bw, 100, 1.1, 0.0, 0.0, 0.0),
    s("rpx400", "Rollei", "RPX 400", Bw, 400, 1.12, 0.0, 0.0, 0.0),
    s("retro80s", "Rollei", "Retro 80S", Bw, 80, 1.2, 0.0, 0.0, 0.0),
    s("adox_chs100", "ADOX", "CHS 100 II", Bw, 100, 1.1, 0.0, 0.0, 0.0),
    s("adox_hr50", "ADOX", "HR-50", Bw, 50, 1.18, 0.0, 0.0, 0.0),
    // NC500 is the product name: the film is ISO 400.
    s("orwo_nc500", "ORWO", "Wolfen NC500", Color, 400, 0.98, 0.8, 0.0, 0.015),
    s("orwo_un54", "ORWO", "Wolfen UN54", Bw, 100, 1.08, 0.0, 0.0, 0.0),
    s("reflx_lab", "Reflx Lab", "800 Tungsten", Cine, 800, 1.0, 1.05, -0.02, 0.0),
];

pub fn stock(id: &str) -> &'static Stock {
    STOCKS.iter().find(|s| s.id == id).unwrap_or(&STOCKS[0])
}

/// Box speed of a stock, if it has one.
pub fn iso(s: &Stock) -> Option<f32> {
    (s.iso > 0).then_some(s.iso as f32)
}

/// No longer made. New old stock and frozen rolls are still shot, so they stay in the list.
pub fn discontinued(s: &Stock) -> bool {
    matches!(s.id, "kodachrome" | "pro400h" | "superia400" | "fuji200" | "neopan400")
}

/// Sold with the remjet backing already washed off, so it can go through C-41: no anti-halation layer is left.
fn remjet_removed(s: &Stock) -> bool {
    s.kind == Kind::Cine && (s.brand == "CineStill" || s.brand == "Reflx Lab")
}

/// What kind of film a stock is and how it is developed, for the line under its name.
pub fn process(s: &Stock) -> &'static str {
    match s.kind {
        // Clear or purplish film instead of orange: the conversion measures the base, so it copes, but such
        // a frame is not picked out as a negative by `orange_mask`.
        Kind::Color if matches!(s.id, "aerocolor" | "phoenix200" | "phoenix2") => "C-41 colour negative, no orange mask",
        Kind::Color => "C-41 colour negative",
        Kind::Cine if remjet_removed(s) => "Motion picture film for C-41",
        Kind::Cine => "ECN-2 motion picture",
        Kind::Bw if s.id == "xp2" => "Black & white, C-41 process",
        Kind::Bw => "Black & white",
        Kind::Slide if s.id == "kodachrome" => "K-14 slide, no inversion",
        Kind::Slide => "E-6 slide, no inversion",
    }
}

/// Grain amount, grain size, halation, and how much grain is left in the highlights (0–100) for a stock.
/// Faster film: more and bigger grain. Halation is the red glow of colour film: strong where the remjet has
/// been removed (CineStill, Reflx Lab), moderate on Harman Phoenix, whose anti-halation is weak, and slight on
/// everything else, including Aerocolor, which has an anti-halation undercoat.
/// Prints from negatives have clean highlights (dense negative, little grain reaches the paper); slides and
/// B&W keep a little more.
/// shortcut: rule-of-thumb values from film speed and type, not measured from scans.
pub fn character(s: &Stock) -> (f32, f32, f32, f32) {
    let stops = (iso(s).unwrap_or(200.0) / 100.0).log2();
    let bw = if s.kind == Kind::Bw { 6.0 } else { 0.0 };
    let grain = (12.0 + 8.0 * stops + bw).clamp(5.0, 60.0);
    let size = (20.0 + 8.0 * stops).clamp(10.0, 60.0);
    let halation = match s.kind {
        Kind::Bw => 0.0,
        _ if remjet_removed(s) => 55.0,
        _ if s.id == "phoenix200" => 30.0,
        _ if s.id == "phoenix2" => 22.0,
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
    /// Paper grade, 0 (soft) to 5 (hard) in half grades, like multigrade filters. Grade 2 is the plain conversion.
    pub grade: f32,
    /// Print exposure in stops: more light under the enlarger makes a darker print.
    pub print_exp: f32,
    /// Neutral greys picked on the scan (linear scan colour, before inversion), at most two. One grey takes out a
    /// cast; two of different brightness also match each channel's contrast, so shadows and highlights both come
    /// out neutral (RawTherapee's way of calibrating a roll). Copied with the roll.
    pub greys: Vec<[f32; 3]>,
}

impl Default for NegParams {
    fn default() -> Self {
        Self { enabled: false, stock: "portra400".into(), look: Look::Neutral, base: None, levels: None, density: 0.0, contrast: 0.0, cy: 0.0, mg: 0.0, yb: 0.0, toe: 40.0, shoulder: 55.0, grade: NEUTRAL_GRADE, print_exp: 0.0, greys: vec![] }
    }
}

/// The grade that leaves the conversion as it is.
pub const NEUTRAL_GRADE: f32 = 2.0;

/// A paper's exposure range (ISO R) for a grade: about R 170 for grade 0 down to R 60 for grade 5, the
/// span multigrade papers give.
pub fn iso_r(grade: f32) -> f32 {
    170.0 - 22.0 * grade.clamp(0.0, 5.0)
}

/// Contrast factor of a grade against the neutral one: a shorter paper range prints the same negative harder.
pub fn grade_factor(grade: f32) -> f32 {
    iso_r(NEUTRAL_GRADE) / iso_r(grade)
}

/// Step sizes for the darkroom views: a test strip band, and one ring-around step on the filter sliders.
pub const STRIP_STOPS: f32 = 0.5;
pub const RING_STEP: f32 = 40.0;

/// The test strip: five bands from -1 to +1 stop around the print exposure.
pub fn strip_variant(np: &NegParams, band: usize) -> NegParams {
    NegParams { print_exp: np.print_exp + (band as f32 - 2.0) * STRIP_STOPS, ..np.clone() }
}

/// Ring-around cells, row by row in a 3×3 grid, with the frame as it is in the middle. The six colours
/// go round in colour-wheel order; lighter above, darker below.
pub const RING: [&str; 9] = ["More red", "Lighter", "More yellow", "More magenta", "As is", "More green", "More blue", "Darker", "More cyan"];

pub fn ring_variant(np: &NegParams, cell: usize) -> NegParams {
    let s = RING_STEP;
    let mut q = np.clone();
    match cell {
        0 => q.cy += s,
        1 => q.print_exp -= STRIP_STOPS,
        2 => q.yb -= s,
        3 => q.mg -= s,
        5 => q.mg += s,
        6 => q.yb += s,
        7 => q.print_exp += STRIP_STOPS,
        8 => q.cy -= s,
        _ => {}
    }
    for v in [&mut q.cy, &mut q.mg, &mut q.yb] {
        *v = v.clamp(-100.0, 100.0);
    }
    q
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

/// Why infrared cleaning can't work on this film, if it can't: silver images (black-and-white film other than
/// chromogenic ones like XP2) and Kodachrome's dyes block infrared, so the picture itself would look like dust.
pub fn ir_blocked(np: &NegParams) -> Option<&'static str> {
    if !np.enabled {
        return None;
    }
    let st = stock(&np.stock);
    if st.id.starts_with("kodachrome") {
        Some("Kodachrome")
    } else if st.kind == Kind::Bw && st.id != "xp2" {
        Some("black-and-white film")
    } else {
        None
    }
}

/// Per-channel gain and offset on the normalised densities that make the picked greys neutral, matched to green
/// (the reference, as in RawTherapee). One grey sets offsets only; two set gains too. Greys too close in
/// density to tell apart count as one.
pub fn grey_fix(greys: &[[f32; 3]], base: [f32; 3], levels: &[[f32; 2]; 3], mono: bool) -> [[f32; 2]; 3] {
    let n = |c: &[f32; 3]| {
        let d = densities(c, base, mono);
        [0, 1, 2].map(|k| (d[k] - levels[k][0]) / (levels[k][1] - levels[k][0]))
    };
    let mut fix = [[1.0, 0.0]; 3];
    match greys {
        [] => {}
        [a] => {
            let a = n(a);
            for k in [0, 2] {
                fix[k][1] = a[1] - a[k];
            }
        }
        [a, b, ..] => {
            let (a, b) = (n(a), n(b));
            for k in [0, 2] {
                let span = b[k] - a[k];
                if span.abs() < 0.05 || (b[1] - a[1]).abs() < 0.05 {
                    fix[k][1] = (a[1] + b[1] - a[k] - b[k]) / 2.0;
                } else {
                    let g = ((b[1] - a[1]) / span).clamp(0.5, 2.0);
                    fix[k] = [g, a[1] - g * a[k]];
                }
            }
        }
    }
    fix
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
    let gamma = st.contrast * look_con * (1.0 + np.contrast / 100.0 * 0.5) * grade_factor(np.grade);
    let sat = if mono { 0.0 } else { st.sat * look_sat };
    let warm = st.warm + look_warm;
    let bal = [np.cy / 100.0 * 0.1 + warm, np.mg / 100.0 * 0.1 + st.green, np.yb / 100.0 * 0.1 - warm];
    let dens = np.density / 100.0 * 0.3;
    // A stop is 0.3 density of light: as a share of this frame's density range, per channel.
    let expo = levels.map(|l| np.print_exp * std::f32::consts::LOG10_2 / (l[1] - l[0]).max(0.05));
    let fix = grey_fix(&np.greys, base, &levels, mono);
    crate::develop::px3_mut(img).par_iter_mut().for_each(|px| {
        let d = densities(px, base, mono);
        let mut n = [0, 1, 2].map(|c| (d[c] - levels[c][0]) / (levels[c][1] - levels[c][0]) * fix[c][0] + fix[c][1]);
        n = [0, 1, 2].map(|c| (0.5 + (n[c] - 0.5 - dens - expo[c]) * gamma) + bal[c]);
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
    fn two_greys_make_shadows_and_highlights_neutral() {
        // A frame whose darkest and brightest parts aren't grey: each channel's levels come out a little off, so
        // greys print warm in the shadows and cool in the highlights.
        let img = fake_negative();
        let mut np = NegParams { enabled: true, ..Default::default() };
        let (b, mut l) = analyse(&img, &np);
        l[0] = [l[0][0] + 0.06, l[0][1] - 0.12];
        (np.base, np.levels) = (Some(b), Some(l));
        let cast = |np: &NegParams| {
            let mut out = img.clone();
            convert(&mut out, np);
            [16u32, 48].map(|x| { let p = out.get_pixel(x, 4).0; (p[0] - p[1]).abs() + (p[2] - p[1]).abs() })
        };
        np.stock = "generic_c41".into();
        let before = cast(&np);
        np.greys = vec![img.get_pixel(16, 4).0, img.get_pixel(48, 4).0];
        let after = cast(&np);
        assert!(after[0] < 0.01 && after[1] < 0.01, "both greys neutral: {after:?} (was {before:?})");
        // One grey: neutral there.
        np.greys.truncate(1);
        assert!(cast(&np)[0] < 0.01);
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
    fn neutral_grade_and_zero_exposure_change_nothing() {
        let np = NegParams { enabled: true, stock: "generic_c41".into(), ..Default::default() };
        let mut a = fake_negative();
        convert(&mut a, &np);
        let mut b = fake_negative();
        convert(&mut b, &NegParams { grade: NEUTRAL_GRADE, print_exp: 0.0, ..np.clone() });
        assert_eq!(a.as_raw(), b.as_raw());
        // Old sidecars without the fields read as grade 2, no exposure change.
        let old: NegParams = serde_json::from_str(r#"{"enabled":true,"stock":"generic_c41"}"#).unwrap();
        assert_eq!(old.grade, NEUTRAL_GRADE);
        assert_eq!(old.print_exp, 0.0);
    }

    #[test]
    fn harder_grade_adds_contrast_and_exposure_darkens() {
        let np = NegParams { enabled: true, stock: "generic_c41".into(), ..Default::default() };
        let spread = |q: &NegParams| {
            let mut img = fake_negative();
            convert(&mut img, q);
            img.get_pixel(44, 0).0[1] - img.get_pixel(20, 0).0[1]
        };
        assert!(spread(&NegParams { grade: 4.0, ..np.clone() }) > spread(&np) + 0.02);
        assert!(spread(&NegParams { grade: 0.0, ..np.clone() }) < spread(&np) - 0.02);
        let mid = |q: &NegParams| {
            let mut img = fake_negative();
            convert(&mut img, q);
            img.get_pixel(32, 0).0[1]
        };
        assert!(mid(&NegParams { print_exp: 1.0, ..np.clone() }) < mid(&np) - 0.03, "more light, darker print");
        assert!(mid(&strip_variant(&np, 0)) > mid(&np), "the first test strip band is the lightest");
        assert!(mid(&ring_variant(&np, 7)) < mid(&np), "Darker is darker");
        assert!(ring_variant(&np, 0).cy > np.cy && ring_variant(&np, 4) == np);
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

    /// Every stock's box speed, pinned. 0 = none shown (generic profiles, Kodachrome, LomoChrome 100-400).
    /// These were the 61 ids of the first releases plus the ones added since: a missing id breaks saved edits.
    #[test]
    fn every_stock_has_its_box_speed() {
        let want: &[(&str, u16)] = &[
            ("generic_c41", 0), ("generic_ecn2", 0), ("generic_bw", 0), ("generic_e6", 0), ("kodachrome", 0),
            ("portra160", 160), ("portra400", 400), ("portra800", 800), ("ektar100", 100), ("gold200", 200),
            ("colorplus200", 200), ("ultramax400", 400), ("proimage100", 100), ("vision3_50d", 50), ("vision3_250d", 250),
            ("vision3_200t", 200), ("vision3_500t", 500), ("trix400", 400), ("tmax100", 100), ("tmax400", 400),
            ("tmax3200", 3200), ("doublex", 250), ("aerocolor", 100), ("pro400h", 400), ("superia400", 400),
            ("fuji200", 200), ("fuji400", 400), ("acros", 100), ("neopan400", 400), ("cs50d", 50), ("cs400d", 400),
            ("cs800t", 800), ("hp5", 400), ("fp4", 125), ("delta100", 100), ("delta400", 400), ("delta3200", 3200),
            ("panf50", 50), ("sfx200", 200), ("xp2", 400), ("phoenix200", 200), ("kentmere100", 100), ("kentmere400", 400),
            ("lomo100", 100), ("lomo400", 400), ("lomo800", 800), ("lomo_purple", 0), ("lomo_metropolis", 0),
            ("lomo_turquoise", 0), ("foma100", 100), ("foma200", 200), ("foma400", 400), ("apx100", 100), ("apx400", 400),
            ("rpx25", 25), ("rpx100", 100), ("rpx400", 400), ("retro80s", 80), ("reflx_lab", 800), ("velvia50", 50),
            ("provia100f", 100), ("ektachrome", 100),
            // Added later.
            ("kodacolor100", 100), ("kodacolor200", 200), ("fujifilm200", 200), ("velvia100", 100), ("cs_bwxx", 250),
            ("ortho80", 80), ("phoenix2", 200), ("kentmere200", 200), ("adox_chs100", 100), ("adox_hr50", 50),
            ("orwo_nc500", 400), ("orwo_un54", 100),
        ];
        for (id, speed) in want {
            let st = STOCKS.iter().find(|s| s.id == *id).unwrap_or_else(|| panic!("stock id {id} is gone"));
            assert_eq!(st.iso, *speed, "{id}");
            assert_eq!(iso(st), (*speed > 0).then_some(*speed as f32), "{id}");
        }
        assert_eq!(STOCKS.len(), want.len(), "a new stock needs its speed pinned here");
        // A number standing alone in the name is the declared speed. Double-X 5222 is the one film code.
        for st in STOCKS.iter().filter(|s| s.id != "doublex") {
            let in_name = st.name.split(" (").next().unwrap_or("").split(' ').find_map(|w| w.trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse::<u16>().ok());
            assert!(in_name.is_none() || in_name == Some(st.iso), "{}: {} vs {:?}", st.id, st.iso, in_name);
        }
    }

    #[test]
    fn stock_types_and_glow_follow_the_film() {
        // Black-and-white film converts to grey, whatever it is developed in; slides are left alone.
        for st in STOCKS {
            let (grain, size, glow, _) = character(st);
            assert!((5.0..=60.0).contains(&grain) && (10.0..=60.0).contains(&size), "{}", st.id);
            assert_eq!(st.kind == Kind::Bw, st.sat == 0.0, "{}", st.id);
            assert_eq!(glow == 0.0, st.kind == Kind::Bw, "{}", st.id);
            assert!(!process(st).is_empty());
        }
        let glow = |id: &str| character(stock(id)).2;
        // Remjet removed: strong glow. Phoenix: weak anti-halation. Vision3 and Aerocolor: protected.
        assert!(glow("cs800t") > 40.0 && glow("cs50d") > 40.0 && glow("reflx_lab") > 40.0);
        assert!(glow("phoenix200") > glow("phoenix2") && glow("phoenix2") > glow("portra400"));
        assert_eq!(glow("vision3_500t"), glow("portra400"));
        assert_eq!(glow("aerocolor"), glow("portra400"));
        assert!(process(stock("xp2")).contains("C-41") && stock("xp2").kind == Kind::Bw);
        assert!(process(stock("aerocolor")).contains("no orange mask") && process(stock("kodachrome")).contains("K-14"));
        assert!(process(stock("cs800t")).contains("C-41") && process(stock("vision3_500t")).contains("ECN-2"));
        assert!(discontinued(stock("pro400h")) && discontinued(stock("kodachrome")) && !discontinued(stock("portra400")));
        // Double-X was read as ISO 5222 once: its grain is that of a 250 film now.
        assert!(character(stock("doublex")).0 < character(stock("tmax3200")).0);
        assert!(character(stock("hp5")).0 > character(stock("fp4")).0);
    }

    #[test]
    fn each_brand_is_listed_in_one_run() {
        let mut seen: Vec<&str> = vec![];
        for st in STOCKS {
            if seen.last() != Some(&st.brand) {
                assert!(!seen.contains(&st.brand), "{} appears twice in the list", st.brand);
                seen.push(st.brand);
            }
        }
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

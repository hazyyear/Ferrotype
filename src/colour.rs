//! Colour management: export colour spaces, the screen's colour profile, and printer soft proofing.
//!
//! Renders are extended sRGB (a channel below 0 is a colour outside sRGB, see `develop::render_wide`). An export
//! in a wider space maps that into the space with a 3×3 matrix taken from the very profile embedded in the file,
//! then clips there, so saturated colours sRGB can't hold reach Display P3, Adobe RGB or ProPhoto files.
//! The screen and printer profiles go through moxcms (already part of the `image` crate's dependencies).

use image::Rgb32FImage;
use moxcms::{ColorProfile, Layout, RenderingIntent, TransformOptions};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};

/// Colour space of an export.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum Space {
    #[default]
    Srgb,
    DisplayP3,
    AdobeRgb,
    ProPhoto,
}

impl Space {
    pub const ALL: [(Space, &'static str); 4] = [(Space::Srgb, "sRGB"), (Space::DisplayP3, "Display P3"), (Space::AdobeRgb, "Adobe RGB"), (Space::ProPhoto, "ProPhoto RGB")];

    pub fn name(self) -> &'static str {
        Space::ALL.iter().find(|s| s.0 == self).map_or("sRGB", |s| s.1)
    }

    pub fn parse(s: &str) -> Option<Space> {
        let k: String = s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_ascii_lowercase();
        match k.as_str() {
            "srgb" => Some(Space::Srgb),
            "p3" | "displayp3" => Some(Space::DisplayP3),
            "adobe" | "adobergb" | "adobergb1998" => Some(Space::AdobeRgb),
            "prophoto" | "prophotorgb" | "romm" => Some(Space::ProPhoto),
            _ => None,
        }
    }

    /// When to pick it, for the export dialog.
    pub fn hint(self) -> &'static str {
        match self {
            Space::Srgb => "Web, phones and most labs",
            Space::DisplayP3 => "Wide-gamut screens: recent Macs, iPhones, many monitors",
            Space::AdobeRgb => "Printing, and labs that ask for Adobe RGB",
            Space::ProPhoto => "Further editing elsewhere; use 16-bit TIFF",
        }
    }

    pub fn profile(self) -> ColorProfile {
        match self {
            Space::Srgb => ColorProfile::new_srgb(),
            Space::DisplayP3 => ColorProfile::new_display_p3(),
            Space::AdobeRgb => ColorProfile::new_adobe_rgb(),
            Space::ProPhoto => ColorProfile::new_pro_photo_rgb(),
        }
    }

    /// The ICC profile to embed. sRGB exports stay untagged (that's what untagged means everywhere).
    pub fn icc(self) -> Vec<u8> {
        if self == Space::Srgb {
            return vec![];
        }
        self.profile().encode().unwrap_or_default()
    }

    /// Linear sRGB → linear this space, from the same colorants the embedded profile carries.
    fn matrix(self) -> [[f32; 3]; 3] {
        let m = ColorProfile::new_srgb().transform_matrix(&self.profile());
        let v = [[m.v[0][0], m.v[0][1], m.v[0][2]], [m.v[1][0], m.v[1][1], m.v[1][2]], [m.v[2][0], m.v[2][1], m.v[2][2]]];
        v.map(|r| r.map(|x| x as f32))
    }

    /// This space's tone curve: linear → encoded.
    fn encode(self, v: f32) -> f32 {
        let v = v.clamp(0.0, 1.0);
        match self {
            Space::Srgb | Space::DisplayP3 => crate::load::linear_to_srgb(v),
            Space::AdobeRgb => v.powf(256.0 / 563.0),
            Space::ProPhoto => v.powf(1.0 / 1.8),
        }
    }
}

/// An extended-sRGB render (encoded) → encoded in `space`, clipped to it.
pub fn to_space(img: &mut Rgb32FImage, space: Space) {
    if space == Space::Srgb {
        img.iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
        return;
    }
    let m = space.matrix();
    img.par_chunks_mut(3).for_each(|px| {
        let l = [0, 1, 2].map(|k| crate::load::srgb_to_linear_ext(px[k]));
        for (k, row) in m.iter().enumerate() {
            px[k] = space.encode(row[0] * l[0] + row[1] * l[1] + row[2] * l[2]);
        }
    });
}

// ---------- the screen ----------

/// How the screen shows colour: sRGB (no change), a standard wide-gamut space, or a monitor ICC profile.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Screen {
    /// "" = sRGB / unmanaged, "p3", "adobe", or a path to an .icc/.icm file. "auto" = ask the system.
    pub profile: String,
}

/// Colour settings kept between sessions: the screen's profile and the printer profile for soft proofing.
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Settings {
    pub screen: Screen,
    /// Printer profile file for soft proofing ("" = none loaded).
    pub printer: String,
    /// Perceptual rendering for the proof (else relative colorimetric).
    pub perceptual: bool,
    /// Show the paper white and ink black too.
    pub paper: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { screen: Screen::default(), printer: String::new(), perceptual: true, paper: false }
    }
}

impl Settings {
    pub fn load() -> Self {
        crate::keys::dirs_config().and_then(|d| std::fs::read_to_string(d.join("colour.json")).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let d = crate::keys::dirs_config().ok_or("no settings folder")?;
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        crate::write_atomic(&d.join("colour.json"), serde_json::to_string_pretty(self).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())
    }
}

/// The transform every on-screen picture goes through (None = sRGB screen, nothing to do).
static SCREEN: std::sync::RwLock<Option<Arc<ScreenTransform>>> = std::sync::RwLock::new(None);

/// Use this screen setting from now on. Returns the profile's name, or None for sRGB / a profile that can't be used.
pub fn set_screen(s: &Screen) -> Option<String> {
    let t = screen_transform(s);
    if let Ok(mut g) = SCREEN.write() {
        *g = t.clone();
    }
    t.and(screen_profile(s).map(|p| p.0))
}

/// sRGB-encoded 8-bit RGB → RGBA as the screen should get it.
pub fn for_screen(rgb: &[u8]) -> Vec<u8> {
    let mut rgba: Vec<u8> = rgb.chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect();
    if let Some(t) = SCREEN.read().ok().and_then(|g| g.clone()) {
        t.apply(&mut rgba);
    }
    rgba
}

/// A ready transform from sRGB to the screen, for 8-bit RGBA texture pixels.
pub struct ScreenTransform {
    t: Arc<moxcms::Transform8BitExecutor>,
}

impl ScreenTransform {
    pub fn apply(&self, rgba: &mut [u8]) {
        let mut out = vec![0u8; rgba.len()];
        // moxcms works on whole rows; textures are small (a preview), so one go is fine.
        if self.t.transform(rgba, &mut out).is_ok() {
            rgba.copy_from_slice(&out);
        }
    }
}

/// The screen's profile as set (or found, for "auto"), parsed. None = sRGB, nothing to do.
pub fn screen_profile(s: &Screen) -> Option<(String, ColorProfile)> {
    match s.profile.trim() {
        "" | "srgb" => None,
        "p3" => Some(("Display P3".into(), ColorProfile::new_display_p3())),
        "adobe" => Some(("Adobe RGB".into(), ColorProfile::new_adobe_rgb())),
        "auto" => system_display_icc().and_then(|b| ColorProfile::new_from_slice(&b).ok()).map(|p| ("System profile".into(), p)),
        path => {
            let bytes = std::fs::read(path).ok()?;
            let p = ColorProfile::new_from_slice(&bytes).ok()?;
            let name = std::path::Path::new(path).file_stem().map_or("Monitor profile".into(), |n| n.to_string_lossy().to_string());
            Some((name, p))
        }
    }
}

/// sRGB → the screen, cached for the current setting.
pub fn screen_transform(s: &Screen) -> Option<Arc<ScreenTransform>> {
    type Cache = Mutex<Option<(Screen, Option<Arc<ScreenTransform>>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let c = CACHE.get_or_init(Default::default);
    if let Ok(g) = c.lock()
        && let Some((k, t)) = &*g
        && k == s
    {
        return t.clone();
    }
    let t = screen_profile(s).and_then(|(_, p)| {
        let opts = TransformOptions { rendering_intent: RenderingIntent::RelativeColorimetric, ..Default::default() };
        ColorProfile::new_srgb().create_transform_8bit(Layout::Rgba, &p, Layout::Rgba, opts).ok().map(|t| Arc::new(ScreenTransform { t }))
    });
    if let Ok(mut g) = c.lock() {
        *g = Some((s.clone(), t.clone()));
    }
    t
}

/// The main display's ICC profile from the operating system, where we can ask for it without extra libraries.
pub fn system_display_icc() -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        mac::display_icc()
    }
    #[cfg(windows)]
    {
        win::display_icc()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        // colord keeps the assigned profile here on most desktops; the X11 atom needs a display connection.
        let home = std::env::var_os("HOME")?;
        let dir = std::path::Path::new(&home).join(".local/share/icc");
        let mut found: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("icc") || e.eq_ignore_ascii_case("icm"))).collect();
        found.sort();
        (found.len() == 1).then(|| std::fs::read(&found[0]).ok()).flatten()
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayCopyColorSpace(display: u32) -> *const c_void;
        fn CGColorSpaceCopyICCData(space: *const c_void) -> *const c_void;
        fn CGColorSpaceRelease(space: *const c_void);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDataGetLength(data: *const c_void) -> isize;
        fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        fn CFRelease(cf: *const c_void);
    }

    pub fn display_icc() -> Option<Vec<u8>> {
        // SAFETY: Core Graphics calls on the main display; every object we get a copy of is released.
        unsafe {
            let space = CGDisplayCopyColorSpace(CGMainDisplayID());
            if space.is_null() {
                return None;
            }
            let data = CGColorSpaceCopyICCData(space);
            CGColorSpaceRelease(space);
            if data.is_null() {
                return None;
            }
            let n = CFDataGetLength(data);
            let p = CFDataGetBytePtr(data);
            let out = (n > 0 && !p.is_null()).then(|| std::slice::from_raw_parts(p, n as usize).to_vec());
            CFRelease(data);
            out
        }
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetDC(hwnd: *mut c_void) -> *mut c_void;
        fn ReleaseDC(hwnd: *mut c_void, hdc: *mut c_void) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetICMProfileW(hdc: *mut c_void, len: *mut u32, name: *mut u16) -> i32;
    }

    pub fn display_icc() -> Option<Vec<u8>> {
        // SAFETY: the screen's device context is released again; the buffer is sized as GetICMProfileW asks.
        let path = unsafe {
            let dc = GetDC(std::ptr::null_mut());
            if dc.is_null() {
                return None;
            }
            let mut len = 0u32;
            GetICMProfileW(dc, &mut len, std::ptr::null_mut());
            let mut buf = vec![0u16; len.max(1) as usize];
            let ok = GetICMProfileW(dc, &mut len, buf.as_mut_ptr());
            ReleaseDC(std::ptr::null_mut(), dc);
            if ok == 0 {
                return None;
            }
            String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())])
        };
        std::fs::read(path).ok()
    }
}

// ---------- printer soft proof ----------

/// A soft proof through a printer (or press) ICC profile: what the print can hold, seen on screen.
pub struct Proof {
    pub name: String,
    to: Arc<moxcms::Transform8BitExecutor>,
    back: Arc<moxcms::Transform8BitExecutor>,
    channels: usize,
}

/// Load a printer profile (RGB or CMYK) for proofing. `perceptual` = Perceptual rendering, else Relative
/// colorimetric. `paper` = also show the paper white and ink black (absolute colorimetric on the way back).
pub fn load_proof(path: &str, perceptual: bool, paper: bool) -> Result<Proof, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let printer = ColorProfile::new_from_slice(&bytes).map_err(|e| format!("not a usable ICC profile ({e:?})"))?;
    let channels = match printer.color_space {
        moxcms::DataColorSpace::Rgb => 3,
        moxcms::DataColorSpace::Cmyk => 4,
        other => return Err(format!("a {other:?} profile can't be proofed; use an RGB or CMYK printer profile")),
    };
    let lay = if channels == 4 { Layout::Rgba } else { Layout::Rgb };
    let srgb = ColorProfile::new_srgb();
    let intent = if perceptual { RenderingIntent::Perceptual } else { RenderingIntent::RelativeColorimetric };
    let to = srgb.create_transform_8bit(Layout::Rgb, &printer, lay, TransformOptions { rendering_intent: intent, ..Default::default() }).map_err(|e| format!("{e:?}"))?;
    let back_intent = if paper { RenderingIntent::AbsoluteColorimetric } else { RenderingIntent::RelativeColorimetric };
    let back = printer.create_transform_8bit(lay, &srgb, Layout::Rgb, TransformOptions { rendering_intent: back_intent, ..Default::default() }).map_err(|e| format!("{e:?}"))?;
    let name = std::path::Path::new(path).file_stem().map_or("Printer".into(), |n| n.to_string_lossy().to_string());
    Ok(Proof { name, to, back, channels })
}

impl Proof {
    /// sRGB-encoded 0..1 → as the print would show it, sRGB-encoded.
    pub fn apply(&self, img: &mut Rgb32FImage) {
        let rgb: Vec<u8> = img.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
        let mut mid = vec![0u8; rgb.len() / 3 * self.channels];
        let mut back = vec![0u8; rgb.len()];
        if self.to.transform(&rgb, &mut mid).is_err() || self.back.transform(&mid, &mut back).is_err() {
            return;
        }
        // Keep the float detail: move each pixel by the proof's change rather than snapping to 8 bits.
        img.iter_mut().zip(rgb.iter().zip(&back)).for_each(|(v, (a, b))| *v = (*v + (*b as f32 - *a as f32) / 255.0).clamp(0.0, 1.0));
    }

    /// Share of pixels the printer can't reproduce (changed by more than a few levels): the gamut warning.
    pub fn out_of_gamut(&self, img: &Rgb32FImage) -> Vec<bool> {
        let rgb: Vec<u8> = img.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
        let mut mid = vec![0u8; rgb.len() / 3 * self.channels];
        let mut back = vec![0u8; rgb.len()];
        if self.to.transform(&rgb, &mut mid).is_err() || self.back.transform(&mid, &mut back).is_err() {
            return vec![false; rgb.len() / 3];
        }
        rgb.chunks(3).zip(back.chunks(3)).map(|(a, b)| (0..3).map(|k| (a[k] as i32 - b[k] as i32).abs()).sum::<i32>() > 18).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_spaces_keep_what_srgb_clips() {
        // A saturated green just outside sRGB: red is negative in linear sRGB.
        let lin = [-0.05f32, 0.6, 0.05];
        let enc = lin.map(crate::load::linear_to_srgb);
        let mut img = Rgb32FImage::from_raw(1, 1, enc.to_vec()).unwrap();
        let mut p3 = img.clone();
        to_space(&mut p3, Space::DisplayP3);
        to_space(&mut img, Space::Srgb);
        assert_eq!(img.as_raw()[0], 0.0, "sRGB clips it");
        // In P3 the colour fits: all channels in range, and converting back gives the original.
        let back: Vec<f32> = {
            let l = p3.as_raw().iter().map(|&v| crate::load::srgb_to_linear(v)).collect::<Vec<_>>();
            let m = ColorProfile::new_display_p3().transform_matrix(&ColorProfile::new_srgb());
            (0..3).map(|k| (m.v[k][0] * l[0] as f64 + m.v[k][1] * l[1] as f64 + m.v[k][2] * l[2] as f64) as f32).collect()
        };
        for k in 0..3 {
            assert!((back[k] - lin[k]).abs() < 0.01, "{back:?}");
        }
    }

    #[test]
    fn white_and_grey_stay_neutral_in_every_space() {
        for (s, _) in Space::ALL {
            let g = crate::load::linear_to_srgb(0.18);
            let mut img = Rgb32FImage::from_raw(2, 1, vec![1.0, 1.0, 1.0, g, g, g]).unwrap();
            to_space(&mut img, s);
            let v = img.as_raw();
            assert!(v[..3].iter().all(|&x| (x - 1.0).abs() < 0.003), "{s:?} white {v:?}");
            assert!((v[3] - v[4]).abs() < 0.003 && (v[4] - v[5]).abs() < 0.003, "{s:?} grey {v:?}");
            assert_eq!(s.icc().is_empty(), s == Space::Srgb);
            assert_eq!(Space::parse(s.name()), Some(s));
        }
    }

    #[test]
    fn screen_transform_for_p3_desaturates_srgb_red() {
        // Pure sRGB red sits inside P3, so on a P3 screen it needs less than full P3 red.
        let t = screen_transform(&Screen { profile: "p3".into() }).unwrap();
        let mut px = [255u8, 0, 0, 255];
        t.apply(&mut px);
        assert!(px[0] < 250 && px[1] > 20, "{px:?}");
        assert!(screen_transform(&Screen::default()).is_none());
    }

    #[test]
    fn proof_through_an_rgb_profile_round_trips() {
        // An Adobe RGB "printer" holds all of sRGB: the proof barely changes anything.
        let dir = std::env::temp_dir().join(format!("ferro-icc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("adobe.icc");
        std::fs::write(&path, Space::AdobeRgb.icc()).unwrap();
        let pr = load_proof(path.to_str().unwrap(), false, false).unwrap();
        let mut img = Rgb32FImage::from_raw(2, 1, vec![0.8, 0.3, 0.2, 0.5, 0.5, 0.5]).unwrap();
        let before = img.clone();
        pr.apply(&mut img);
        for (a, b) in img.iter().zip(before.iter()) {
            assert!((a - b).abs() < 0.02, "{a} {b}");
        }
        assert!(pr.out_of_gamut(&img).iter().all(|o| !o));
        assert!(load_proof(dir.join("nope.icc").to_str().unwrap(), true, false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

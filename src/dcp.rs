//! Camera profiles in Adobe's DNG Camera Profile format (.dcp): the colour tables Lightroom and Camera Raw use
//! for Adobe Standard, Camera Matching and creative profiles, and that RawTherapee and DCamProf write too.
//!
//! Ferrotype works in linear sRGB after the camera's own matrix, so a profile's matrices aren't needed: its
//! hue/saturation map and look table are applied in linear ProPhoto RGB, as the DNG specification describes,
//! and its tone curve only when asked for (Ferrotype otherwise keeps the camera's own tone, matched from the
//! embedded JPEG).
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A 3D table of (hue shift in degrees, saturation scale, value scale), indexed by hue, saturation and value.
#[derive(Clone, Debug, PartialEq)]
pub struct HsvTable {
    pub hue: usize,
    pub sat: usize,
    pub val: usize,
    pub data: Vec<[f32; 3]>,
    /// The value axis is sRGB-encoded rather than linear.
    pub srgb: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dcp {
    pub name: String,
    pub camera: String,
    pub hue_sat: Option<HsvTable>,
    pub look: Option<HsvTable>,
    /// Tone curve points (linear in, linear out), empty = none.
    pub tone: Vec<[f32; 2]>,
}

// Linear sRGB (D65) to linear ProPhoto (D50, Bradford), and back.
const TO_PRO: [[f32; 3]; 3] = [[0.529_345_8, 0.330_072_8, 0.140_581_2], [0.098_374_4, 0.873_461_1, 0.028_164_7], [0.016_883_2, 0.117_672_5, 0.865_444_3]];
const FROM_PRO: [[f32; 3]; 3] = [[2.034_076_3, -0.727_334_3, -0.306_741_7], [-0.228_813_5, 1.231_730_2, -0.002_916_9], [-0.008_569_8, -0.153_286_6, 1.161_856_5]];

fn mul(m: &[[f32; 3]; 3], c: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2])
}

/// Read a .dcp file (a TIFF-like structure that starts "IIRC" or "MMCR").
pub fn parse(b: &[u8]) -> Result<Dcp, String> {
    if b.len() < 8 {
        return Err("too short for a camera profile".into());
    }
    let le = match &b[..4] {
        b"IIRC" => true,
        b"MMCR" => false,
        _ => return Err("not a DNG camera profile (.dcp)".into()),
    };
    let u16_at = |o: usize| -> Option<u16> { let s = b.get(o..o + 2)?; Some(if le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) }) };
    let u32_at = |o: usize| -> Option<u32> { let s = b.get(o..o + 4)?; let a = [s[0], s[1], s[2], s[3]]; Some(if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) }) };
    let ifd = u32_at(4).ok_or("bad header")? as usize;
    let n = u16_at(ifd).ok_or("bad directory")? as usize;
    // tag -> (type, count, where the value is)
    let mut tags = std::collections::HashMap::new();
    for i in 0..n {
        let e = ifd + 2 + 12 * i;
        let (tag, typ, count) = (u16_at(e).ok_or("bad entry")?, u16_at(e + 2).ok_or("bad entry")?, u32_at(e + 4).ok_or("bad entry")? as usize);
        // No value is longer than the file: a damaged count can't make the readers below loop for billions.
        let count = count.min(b.len());
        let size = match typ { 1 | 2 | 6 | 7 => 1, 3 | 8 => 2, 4 | 9 | 11 => 4, 5 | 10 | 12 => 8, _ => 1 } * count;
        let at = if size <= 4 { e + 8 } else { u32_at(e + 8).ok_or("bad entry")? as usize };
        tags.insert(tag, (typ, count, at));
    }
    let text = |tag: u16| -> String {
        tags.get(&tag).and_then(|&(_, c, at)| b.get(at..at + c)).map(|s| String::from_utf8_lossy(s).trim_end_matches('\0').trim().to_string()).unwrap_or_default()
    };
    let longs = |tag: u16| -> Vec<u32> {
        tags.get(&tag).map(|&(typ, c, at)| (0..c).filter_map(|i| if typ == 3 { u16_at(at + 2 * i).map(u32::from) } else { u32_at(at + 4 * i) }).collect()).unwrap_or_default()
    };
    let floats = |tag: u16| -> Vec<f32> {
        tags.get(&tag).filter(|t| t.0 == 11).map(|&(_, c, at)| (0..c).filter_map(|i| u32_at(at + 4 * i).map(f32::from_bits)).collect()).unwrap_or_default()
    };
    let table = |dims: u16, data: &[u16], enc: u16| -> Option<HsvTable> {
        let d = longs(dims);
        let (h, s, v) = (*d.first()? as usize, *d.get(1)? as usize, (*d.get(2).unwrap_or(&1) as usize).max(1));
        // Checked, so damaged sizes can't wrap round to a small number and pass.
        if h == 0 || s < 2 || h.checked_mul(s)?.checked_mul(v)? > 1 << 20 {
            return None;
        }
        // The last of the listed tags that holds a whole table (for the hue/sat map: the D65 one, when there are two).
        let vals = data.iter().rev().map(|t| floats(*t)).find(|f| f.len() == h * s * v * 3)?;
        Some(HsvTable { hue: h, sat: s, val: v, data: vals.chunks(3).map(|c| [c[0], c[1], c[2]]).collect(), srgb: longs(enc).first() == Some(&1) })
    };
    // Which hue/sat map is the daylight one: the one whose calibration illuminant is D65 (21) or nearest to it.
    let (il1, il2) = (longs(50778).first().copied().unwrap_or(0), longs(50779).first().copied().unwrap_or(0));
    let maps = if il1 == 21 && il2 != 21 { vec![50939, 50938] } else { vec![50938, 50939] };
    let tone_f = floats(50940);
    let dcp = Dcp {
        name: text(50936),
        camera: text(50708),
        hue_sat: table(50937, &maps, 51107),
        look: table(50981, &[50982], 51108),
        tone: tone_f.chunks(2).filter(|c| c.len() == 2).map(|c| [c[0], c[1]]).collect(),
    };
    if dcp.hue_sat.is_none() && dcp.look.is_none() && dcp.tone.is_empty() {
        return Err("this profile has no colour tables Ferrotype can use (matrix-only profiles change nothing here)".into());
    }
    Ok(dcp)
}

/// HSV as the DNG SDK defines it: hue 0..6, saturation and value as usual.
fn to_hsv(c: [f32; 3]) -> (f32, f32, f32) {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    if mx <= 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let d = mx - mn;
    let s = d / mx;
    if d <= 0.0 {
        return (0.0, 0.0, mx);
    }
    let h = if c[0] == mx { (c[1] - c[2]) / d } else if c[1] == mx { 2.0 + (c[2] - c[0]) / d } else { 4.0 + (c[0] - c[1]) / d };
    (if h < 0.0 { h + 6.0 } else { h }, s, mx)
}

fn from_hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    if s <= 0.0 {
        return [v; 3];
    }
    let h = h.rem_euclid(6.0);
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i as i32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

impl HsvTable {
    /// Look up (hue shift, sat scale, value scale) for a colour, interpolating between table cells.
    fn lookup(&self, h: f32, s: f32, v: f32) -> [f32; 3] {
        let hs = h * self.hue as f32 / 6.0;
        let h0 = hs.floor();
        let hf = hs - h0;
        let h0 = (h0 as usize) % self.hue;
        let h1 = (h0 + 1) % self.hue;
        let ss = (s * (self.sat - 1) as f32).clamp(0.0, (self.sat - 1) as f32 - 1e-4);
        let s0 = ss.floor() as usize;
        let sf = ss - s0 as f32;
        let (v0, v1, vf) = if self.val > 1 {
            let vv = if self.srgb { crate::load::linear_to_srgb(v.clamp(0.0, 1.0)) } else { v.clamp(0.0, 1.0) };
            let vs = (vv * (self.val - 1) as f32).clamp(0.0, (self.val - 1) as f32 - 1e-4);
            let a = vs.floor() as usize;
            (a, a + 1, vs - a as f32)
        } else {
            (0, 0, 0.0)
        };
        let at = |vi: usize, hi: usize, si: usize| self.data[(vi * self.hue + hi) * self.sat + si];
        let lerp = |a: [f32; 3], b: [f32; 3], t: f32| [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t);
        let plane = |vi: usize| lerp(lerp(at(vi, h0, s0), at(vi, h0, s0 + 1), sf), lerp(at(vi, h1, s0), at(vi, h1, s0 + 1), sf), hf);
        lerp(plane(v0), plane(v1), vf)
    }

    fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let (h, s, v) = to_hsv(c);
        let [dh, ks, kv] = self.lookup(h, s, v);
        from_hsv(h + dh * 6.0 / 360.0, (s * ks).clamp(0.0, 1.0), v * kv)
    }
}

/// A 4096-entry table of the tone curve, for linear values 0..1.
pub fn tone_table(pts: &[[f32; 2]]) -> Vec<f32> {
    (0..4096).map(|i| {
        let x = i as f32 / 4095.0;
        let k = pts.partition_point(|p| p[0] < x);
        if k == 0 {
            return pts.first().map_or(x, |p| p[1]);
        }
        if k >= pts.len() {
            return pts.last().map_or(x, |p| p[1]);
        }
        let (a, b) = (pts[k - 1], pts[k]);
        a[1] + (b[1] - a[1]) * (x - a[0]) / (b[0] - a[0]).max(1e-9)
    }).collect()
}

/// The DNG reference "RGB tone": the curve is applied to the largest and smallest channel, and the middle one
/// keeps its place between them, so hues stay put.
fn rgb_tone(c: [f32; 3], t: &[f32]) -> [f32; 3] {
    let f = |v: f32| { let x = v.clamp(0.0, 1.0) * 4095.0; let i = x as usize; let j = (i + 1).min(4095); t[i] + (t[j] - t[i]) * (x - i as f32) };
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|a, b| c[*b].total_cmp(&c[*a]));
    let (hi, mid, lo) = (c[idx[0]], c[idx[1]], c[idx[2]]);
    let (h2, l2) = (f(hi), f(lo));
    let m2 = if hi > lo { l2 + (h2 - l2) * (mid - lo) / (hi - lo) } else { h2 };
    let mut out = [0.0; 3];
    out[idx[0]] = h2;
    out[idx[1]] = m2;
    out[idx[2]] = l2;
    out
}

/// A profile ready to apply to pixels.
pub struct Ready {
    pub dcp: Dcp,
    tone: Option<Vec<f32>>,
}

impl Ready {
    /// Apply to linear sRGB: into ProPhoto, the hue/sat map, the look table, the tone curve if `with_tone`, and back.
    pub fn apply(&self, c: [f32; 3], with_tone: bool) -> [f32; 3] {
        let mut p = mul(&TO_PRO, c).map(|v| v.max(0.0));
        if let Some(t) = &self.dcp.hue_sat {
            p = t.apply(p);
        }
        if let Some(t) = &self.dcp.look {
            p = t.apply(p);
        }
        if with_tone && let Some(t) = &self.tone {
            p = rgb_tone(p, t);
        }
        mul(&FROM_PRO, p)
    }

    pub fn has_tone(&self) -> bool {
        self.tone.is_some()
    }
}

/// A profile file, read once and kept.
pub fn load(path: &str) -> Option<Arc<Ready>> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<std::collections::HashMap<String, Option<Arc<Ready>>>>> = OnceLock::new();
    if path.is_empty() {
        return None;
    }
    let mut c = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if c.len() > 32 {
        c.clear();
    }
    c.entry(path.to_string()).or_insert_with(|| {
        let dcp = parse(&std::fs::read(path).ok()?).ok()?;
        let tone = (dcp.tone.len() >= 2).then(|| tone_table(&dcp.tone));
        Some(Arc::new(Ready { dcp, tone }))
    }).clone()
}

/// Where camera profiles are usually installed: Adobe's (from Lightroom, Camera Raw or the free DNG Converter),
/// RawTherapee's, and Ferrotype's own folder for profiles you add.
pub fn folders() -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(d) = crate::keys::dirs_config() {
        v.push(d.join("profiles").join("dcp"));
    }
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    if cfg!(windows) {
        if let Some(p) = env("ProgramData") {
            v.push(p.join("Adobe").join("CameraRaw").join("CameraProfiles"));
        }
        if let Some(a) = env("APPDATA") {
            v.push(a.join("Adobe").join("CameraRaw").join("CameraProfiles"));
        }
        for p in [env("ProgramFiles"), env("ProgramFiles(x86)")].into_iter().flatten() {
            v.push(p.join("RawTherapee"));
        }
    } else if cfg!(target_os = "macos") {
        v.push(PathBuf::from("/Library/Application Support/Adobe/CameraRaw/CameraProfiles"));
        if let Some(h) = env("HOME") {
            v.push(h.join("Library/Application Support/Adobe/CameraRaw/CameraProfiles"));
        }
        v.push(PathBuf::from("/Applications/RawTherapee.app/Contents/Resources/dcpprofiles"));
    } else {
        v.push(PathBuf::from("/usr/share/rawtherapee/dcpprofiles"));
        if let Some(h) = env("HOME") {
            v.push(h.join(".local/share/ferrotype/dcp"));
        }
    }
    v
}

/// Every .dcp under the usual folders (a few levels deep), found once.
pub fn all_files() -> &'static [PathBuf] {
    static ALL: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        let mut out = vec![];
        let mut stack: Vec<(PathBuf, u8)> = folders().into_iter().map(|f| (f, 0)).collect();
        while let Some((d, depth)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && depth < 4 {
                    stack.push((p, depth + 1));
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dcp")) {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    })
}

/// Lower-case words of a camera name without the maker's legal suffixes, for matching file names.
fn words(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '-').filter(|w| !w.is_empty() && !["corporation", "corp", "co", "ltd", "imaging", "company", "camera"].contains(w)).map(String::from).collect()
}

/// Profiles for this camera among `files`: those whose path names the model (Adobe and RawTherapee name files
/// and folders after the camera, such as "Nikon D750 Adobe Standard.dcp" or "Camera/Nikon D750/…").
pub fn for_camera<'a>(files: &'a [PathBuf], make: &str, model: &str) -> Vec<&'a PathBuf> {
    let make_w = words(make);
    let model_w: Vec<String> = words(model).into_iter().filter(|w| !make_w.contains(w)).collect();
    if model_w.is_empty() {
        return vec![];
    }
    files.iter().filter(|f| {
        let p: Vec<String> = f.iter().flat_map(|c| words(&c.to_string_lossy())).collect();
        // Every word of the model, in order and next to each other (so "D7" doesn't match "D750").
        p.windows(model_w.len()).any(|w| w == model_w.as_slice())
    }).collect()
}

/// A short name for a profile file: its own name, or the file name.
pub fn label(path: &Path) -> String {
    load(&path.to_string_lossy()).map(|r| r.dcp.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal little-endian profile: a 2×2×1 hue/sat map that adds 10° and halves saturation everywhere.
    fn sample() -> Vec<u8> {
        let mut b = b"IIRC".to_vec();
        b.extend(8u32.to_le_bytes());
        let name = b"Test\0\0\0\0";
        let floats: Vec<f32> = (0..4).flat_map(|_| [10.0, 0.5, 1.0]).collect();
        let entries = 3u16;
        let data_at = 8 + 2 + 12 * entries as u32 + 4;
        b.extend(entries.to_le_bytes());
        let entry = |b: &mut Vec<u8>, tag: u16, typ: u16, count: u32, val: u32| {
            b.extend(tag.to_le_bytes());
            b.extend(typ.to_le_bytes());
            b.extend(count.to_le_bytes());
            b.extend(val.to_le_bytes());
        };
        entry(&mut b, 50936, 2, 8, data_at);
        entry(&mut b, 50937, 4, 3, data_at + 8);
        entry(&mut b, 50938, 11, 12, data_at + 20);
        b.extend(0u32.to_le_bytes());
        b.extend(name);
        for d in [2u32, 2, 1] {
            b.extend(d.to_le_bytes());
        }
        for f in floats {
            b.extend(f.to_bits().to_le_bytes());
        }
        b
    }

    #[test]
    fn a_damaged_profile_is_refused_quickly() {
        let mut b = sample();
        // Table sizes of 4 billion each, and a value count of 4 billion: no hang, no crash, just no table.
        for k in 0..3 {
            b[58 + 4 * k..62 + 4 * k].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        b[38..42].copy_from_slice(&u32::MAX.to_le_bytes());
        let t = std::time::Instant::now();
        assert!(parse(&b).is_err());
        assert!(t.elapsed().as_secs_f32() < 1.0);
    }

    #[test]
    fn reads_and_applies_a_hue_sat_map() {
        let d = parse(&sample()).unwrap();
        assert_eq!(d.name, "Test");
        let t = d.hue_sat.clone().unwrap();
        assert_eq!((t.hue, t.sat, t.val), (2, 2, 1));
        let r = Ready { dcp: d, tone: None };
        // Grey stays grey (no saturation to change); a colour loses half its saturation.
        let g = r.apply([0.2, 0.2, 0.2], false);
        assert!(g.iter().all(|v| (v - 0.2).abs() < 1e-3), "{g:?}");
        let c = r.apply([0.4, 0.1, 0.05], false);
        let sat = |c: [f32; 3]| { let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2])); (mx - mn) / mx };
        assert!(sat(c) < sat([0.4, 0.1, 0.05]), "{c:?}");
    }

    #[test]
    fn finds_profiles_by_camera() {
        let files = [PathBuf::from("/p/Camera/Nikon D750/Camera Standard.dcp"), PathBuf::from("/p/Adobe Standard/Nikon D7500 Adobe Standard.dcp"), PathBuf::from("/p/Adobe Standard/Canon EOS 5D Mark IV Adobe Standard.dcp")];
        let d750: Vec<_> = for_camera(&files, "NIKON CORPORATION", "NIKON D750");
        assert_eq!(d750, vec![&files[0]]);
        assert_eq!(for_camera(&files, "Canon", "Canon EOS 5D Mark IV"), vec![&files[2]]);
        assert!(for_camera(&files, "Sony", "ILCE-7M3").is_empty());
    }

    #[test]
    fn rgb_tone_keeps_hue() {
        let t: Vec<f32> = (0..4096).map(|i| (i as f32 / 4095.0).sqrt()).collect();
        let c = rgb_tone([0.4, 0.2, 0.1], &t);
        assert!(c[0] > c[1] && c[1] > c[2]);
        // The middle channel keeps its place between the others.
        let f = (0.2 - 0.1) / (0.4 - 0.1);
        assert!(((c[1] - c[2]) / (c[0] - c[2]) - f).abs() < 1e-4);
    }

    /// The RawTherapee profile for the Nikon D750, if it was downloaded for testing.
    #[test]
    #[ignore]
    fn reads_a_real_profile() {
        let Ok(b) = std::fs::read("/tmp/claude-0/dcp/d750.dcp") else { return };
        let d = parse(&b).unwrap();
        assert!(d.hue_sat.is_some() && d.look.is_some() && d.tone.len() > 100, "{} {}", d.name, d.camera);
        let r = Ready { tone: Some(tone_table(&d.tone)), dcp: d };
        let c = r.apply([0.3, 0.15, 0.08], true);
        assert!(c.iter().all(|v| v.is_finite()));
        eprintln!("{} for {}: {:?}", r.dcp.name, r.dcp.camera, c);
    }
}

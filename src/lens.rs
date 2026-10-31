//! Lens correction profiles from the lensfun database (CC BY-SA 3.0, bundled in data/): distortion,
//! lateral chromatic aberration and vignetting, matched to the photo's lens from its EXIF data.
use std::path::Path;
use std::sync::OnceLock;

/// Distortion in lensfun's "Hugin" units (1 = half the calibration sensor's short side).
#[derive(Clone, Copy, Debug)]
pub enum Dist {
    /// PTLens a, b, c (poly3's k1 is b with a = c = 0).
    Pt([f32; 3]),
    Poly5([f32; 2]),
}

impl Dist {
    /// Distorted radius for an undistorted radius `r`, both in Hugin units.
    pub fn apply(&self, r: f32) -> f32 {
        match *self {
            Dist::Pt([a, b, c]) => {
                // Lensfun rescales so the centre keeps its size: evaluate at r / d.
                let d = 1.0 - a - b - c;
                let u = r / d;
                u * (((a * u + b) * u + c) * u + d)
            }
            Dist::Poly5([k1, k2]) => {
                let r2 = r * r;
                r * (1.0 + k1 * r2 + k2 * r2 * r2)
            }
        }
    }

    fn lerp(a: Dist, b: Dist, t: f32) -> Dist {
        match (a, b) {
            (Dist::Pt(x), Dist::Pt(y)) => Dist::Pt([0, 1, 2].map(|i| x[i] + (y[i] - x[i]) * t)),
            (Dist::Poly5(x), Dist::Poly5(y)) => Dist::Poly5([0, 1].map(|i| x[i] + (y[i] - x[i]) * t)),
            _ => if t < 0.5 { a } else { b },
        }
    }
}

#[derive(Clone, Debug)]
struct Vig {
    focal: f32,
    aperture: f32,
    distance: f32,
    k: [f32; 3],
}

#[derive(Clone, Debug, Default)]
pub struct Lens {
    pub maker: String,
    /// Every name the database gives the model (the first is shown).
    models: Vec<String>,
    pub crop: f32,
    aspect: f32,
    mounts: Vec<String>,
    dist: Vec<(f32, Dist)>,
    /// [vr, cr, br, vb, cb, bb] by focal length.
    tca: Vec<(f32, [f32; 6])>,
    vig: Vec<Vig>,
}

impl Lens {
    /// "Maker Model", the name stored in a photo's settings.
    pub fn name(&self) -> String {
        let m = &self.models[0];
        if m.to_lowercase().starts_with(&self.maker.to_lowercase()) { m.clone() } else { format!("{} {m}", self.maker) }
    }
}

struct Camera {
    names: Vec<String>, // makers and models, lowercased
    crop: f32,
    mount: String,
}

pub struct Db {
    pub lenses: Vec<Lens>,
    cameras: Vec<Camera>,
}

/// The bundled database, parsed on first use (about 50 ms).
pub fn db() -> &'static Db {
    static DB: OnceLock<Db> = OnceLock::new();
    DB.get_or_init(|| {
        use std::io::Read;
        let mut xml = String::new();
        let _ = flate2::read::GzDecoder::new(&include_bytes!("../data/lensfun.xml.gz")[..]).read_to_string(&mut xml);
        parse(&xml)
    })
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn attrs(tag: &str) -> Vec<(&str, String)> {
    let mut out = vec![];
    let mut rest = tag;
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].rsplit(|c: char| c.is_whitespace()).next().unwrap_or("");
        let after = &rest[eq + 2..];
        let end = after.find('"').unwrap_or(after.len());
        out.push((key, unescape(&after[..end])));
        rest = &after[(end + 1).min(after.len())..];
    }
    out
}

fn num(a: &[(&str, String)], k: &str, default: f32) -> f32 {
    a.iter().find(|(n, _)| *n == k).and_then(|(_, v)| v.parse().ok()).unwrap_or(default)
}

fn aspect(s: &str) -> f32 {
    match s.split_once(':') {
        Some((a, b)) => a.trim().parse::<f32>().unwrap_or(3.0) / b.trim().parse::<f32>().unwrap_or(2.0),
        None => s.trim().parse().unwrap_or(1.5),
    }
}

/// The database is plain machine-written XML: elements, attributes and text, no mixed content.
fn parse(xml: &str) -> Db {
    let (mut lenses, mut cameras) = (vec![], vec![]);
    let (mut lens, mut cam): (Option<Lens>, Option<(Vec<String>, f32, String)>) = (None, None);
    let mut skip = false; // fisheye and other non-rectilinear lenses need a different geometry model
    let mut pos = 0;
    let mut open = String::new();
    while let Some(lt) = xml[pos..].find('<') {
        let text = unescape(xml[pos..pos + lt].trim());
        let start = pos + lt + 1;
        let Some(gt) = xml[start..].find('>') else { break };
        let tag = &xml[start..start + gt];
        pos = start + gt + 1;
        if !text.is_empty() {
            let lang = open.contains("lang=");
            let name = open.split_whitespace().next().unwrap_or("");
            if let Some(l) = &mut lens {
                match name {
                    "maker" if !lang => l.maker = text,
                    "model" => l.models.push(text),
                    "cropfactor" => l.crop = text.parse().unwrap_or(1.0),
                    "mount" => l.mounts.push(text),
                    "aspect-ratio" => l.aspect = aspect(&text),
                    "type" => skip = text != "rectilinear",
                    _ => {}
                }
            } else if let Some((names, crop, mount)) = &mut cam {
                match name {
                    "maker" | "model" => names.push(text.to_lowercase()),
                    "cropfactor" => *crop = text.parse().unwrap_or(1.0),
                    "mount" => *mount = text,
                    _ => {}
                }
            }
        }
        if let Some(name) = tag.strip_prefix('/') {
            match name {
                "lens" => {
                    if let Some(mut l) = lens.take().filter(|l| !skip && !l.models.is_empty() && (!l.dist.is_empty() || !l.tca.is_empty() || !l.vig.is_empty())) {
                        l.dist.sort_by(|a, b| a.0.total_cmp(&b.0));
                        l.tca.sort_by(|a, b| a.0.total_cmp(&b.0));
                        lenses.push(l);
                    }
                    skip = false;
                }
                "camera" => {
                    if let Some((names, crop, mount)) = cam.take() {
                        cameras.push(Camera { names, crop, mount });
                    }
                }
                _ => {}
            }
            continue;
        }
        open = tag.trim_end_matches('/').to_string();
        let name = open.split_whitespace().next().unwrap_or("");
        match name {
            "lens" => lens = Some(Lens { crop: 1.0, aspect: 1.5, ..Default::default() }),
            "camera" => cam = Some((vec![], 1.0, String::new())),
            "distortion" | "tca" | "vignetting" => {
                let Some(l) = &mut lens else { continue };
                let a = attrs(&open);
                let model = a.iter().find(|(k, _)| *k == "model").map(|(_, v)| v.as_str()).unwrap_or("");
                let f = num(&a, "focal", 0.0);
                match (name, model) {
                    ("distortion", "ptlens") => l.dist.push((f, Dist::Pt([num(&a, "a", 0.0), num(&a, "b", 0.0), num(&a, "c", 0.0)]))),
                    ("distortion", "poly3") => l.dist.push((f, Dist::Pt([0.0, num(&a, "k1", 0.0), 0.0]))),
                    ("distortion", "poly5") => l.dist.push((f, Dist::Poly5([num(&a, "k1", 0.0), num(&a, "k2", 0.0)]))),
                    ("tca", "linear") => l.tca.push((f, [num(&a, "kr", 1.0), 0.0, 0.0, num(&a, "kb", 1.0), 0.0, 0.0])),
                    ("tca", "poly3") => l.tca.push((f, [num(&a, "vr", 1.0), num(&a, "cr", 0.0), num(&a, "br", 0.0), num(&a, "vb", 1.0), num(&a, "cb", 0.0), num(&a, "bb", 0.0)])),
                    ("vignetting", "pa") => l.vig.push(Vig {
                        focal: f,
                        aperture: num(&a, "aperture", 8.0),
                        distance: num(&a, "distance", 1000.0),
                        k: [num(&a, "k1", 0.0), num(&a, "k2", 0.0), num(&a, "k3", 0.0)],
                    }),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Db { lenses, cameras }
}

/// Linear interpolation between the two calibrations either side of `focal`.
fn by_focal<T: Copy>(v: &[(f32, T)], focal: f32, lerp: impl Fn(T, T, f32) -> T) -> Option<T> {
    let i = v.iter().position(|(f, _)| *f >= focal);
    match i {
        None => v.last().map(|x| x.1),
        Some(0) => Some(v[0].1),
        Some(i) => {
            let ((f0, a), (f1, b)) = (v[i - 1], v[i]);
            Some(lerp(a, b, if f1 > f0 { (focal - f0) / (f1 - f0) } else { 0.0 }))
        }
    }
}

/// A lens's corrections for one shot.
#[derive(Clone, Debug)]
pub struct Profile {
    pub dist: Option<Dist>,
    pub tca: Option<[f32; 6]>,
    pub vig: Option<[f32; 3]>,
    /// Pixel radius → Hugin units, as a multiple of (radius / half the image diagonal).
    pub hugin: f32,
    /// Same for vignetting (1 = the calibration sensor's corner).
    pub vig_scale: f32,
}

pub fn profile(name: &str, focal: f32, fnum: f32, crop: f32) -> Option<Profile> {
    let l = db().lenses.iter().find(|l| l.name() == name)?;
    let focal = if focal > 0.0 { focal } else { l.dist.first().map(|d| d.0).or(l.tca.first().map(|d| d.0)).unwrap_or(50.0) };
    let dist = by_focal(&l.dist, focal, Dist::lerp);
    let tca = by_focal(&l.tca, focal, |a, b, t| [0, 1, 2, 3, 4, 5].map(|i| a[i] + (b[i] - a[i]) * t));
    // Vignetting: the nearest aperture (in stops) at the farthest distance, interpolated in focal length.
    let vig = (!l.vig.is_empty()).then(|| {
        let fnum = if fnum > 0.0 { fnum } else { 8.0 };
        let mut focals: Vec<f32> = l.vig.iter().map(|v| v.focal).collect();
        focals.sort_by(|a, b| a.total_cmp(b));
        focals.dedup();
        let at = |f: f32| {
            let set: Vec<&Vig> = l.vig.iter().filter(|v| v.focal == f).collect();
            let best = set.iter().map(|v| (v.aperture / fnum).log2().abs()).fold(f32::MAX, f32::min);
            set.iter().filter(|v| ((v.aperture / fnum).log2().abs() - best).abs() < 1e-3)
                .max_by(|a, b| a.distance.total_cmp(&b.distance)).map(|v| v.k).unwrap_or([0.0; 3])
        };
        let table: Vec<(f32, [f32; 3])> = focals.iter().map(|f| (*f, at(*f))).collect();
        by_focal(&table, focal, |a, b, t| [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)).unwrap_or([0.0; 3])
    });
    let ratio = l.crop / crop.max(0.1);
    Some(Profile { dist, tca, vig, hugin: ratio * (l.aspect * l.aspect + 1.0).sqrt(), vig_scale: ratio })
}

fn tokens(s: &str) -> Vec<String> {
    let s = s.to_lowercase().replace("f/", "f").replace("f /", "f");
    s.split(|c: char| !(c.is_alphanumeric() || c == '.'))
        .map(|t| t.trim_matches('.'))
        .map(|t| t.strip_suffix("mm").filter(|n| n.parse::<f32>().is_ok()).unwrap_or(t))
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}

/// What the camera says about a shot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shot {
    pub make: String,
    pub model: String,
    pub lens_make: String,
    pub lens: String,
    pub focal: f32,
    pub fnum: f32,
    /// From EXIF's 35 mm-equivalent focal length, when given.
    pub crop: Option<f32>,
    /// Capture settings: ISO, shutter time in seconds, exposure compensation in EV, and when ("YYYY:MM:DD HH:MM:SS").
    pub iso: f32,
    pub shutter: f32,
    pub bias: f32,
    pub taken: String,
    /// Where it was taken, for exports that ask for it (never read from the camera file).
    pub gps: Option<crate::geotag::Location>,
}

impl Shot {
    /// Exposure value of the capture at ISO 100 (higher = less light reached the sensor). None without the settings.
    pub fn ev100(&self) -> Option<f32> {
        (self.fnum > 0.0 && self.shutter > 0.0).then(|| (self.fnum * self.fnum / self.shutter).log2() - (self.iso.max(1.0) / 100.0).log2())
    }

    /// Seconds since 2000-01-01 from `taken` (no time zone: only for comparing shots from one camera).
    pub fn seconds(&self) -> Option<i64> {
        let n: Vec<i64> = self.taken.split([':', ' ', '-', 'T']).filter_map(|v| v.trim().parse().ok()).collect();
        let [y, mo, d, h, mi, s] = n.get(..6)?.try_into().ok()?;
        let days = (y - 2000) * 366 + (mo - 1) * 31 + d; // monotonic, not calendar-exact: fine for gaps
        Some(((days * 24 + h) * 60 + mi) * 60 + s)
    }
}

fn camera(shot: &Shot) -> Option<&'static Camera> {
    let (mk, md) = (shot.make.to_lowercase(), shot.model.to_lowercase());
    let mk = mk.split_whitespace().next().unwrap_or(""); // "NIKON CORPORATION" → "nikon"
    db().cameras.iter().find(|c| !mk.is_empty() && c.names.iter().any(|n| n.contains(mk)) && c.names.iter().any(|n| *n == md || n.ends_with(&format!(" {md}"))))
}

/// The camera's crop factor: from the database, else from EXIF, else the lens's own.
pub fn camera_crop(shot: &Shot) -> Option<f32> {
    camera(shot).map(|c| c.crop).or(shot.crop)
}

/// The best database lens for this shot, or None when nothing matches well enough.
pub fn find(shot: &Shot) -> Option<&'static Lens> {
    // Compacts and phones have one built-in lens, listed under the camera's own mount.
    let fixed = || {
        let m = &camera(shot)?.mount;
        let mut it = db().lenses.iter().filter(|l| l.mounts.contains(m));
        let first = it.next()?;
        it.next().is_none().then_some(first)
    };
    if shot.lens.trim().is_empty() {
        return fixed();
    }
    let have: Vec<String> = tokens(&format!("{} {} {}", shot.make, shot.lens_make, shot.lens));
    let crop = camera_crop(shot);
    let is_num = |t: &str| t.trim_start_matches('f').parse::<f32>().is_ok();
    db().lenses.iter().filter_map(|l| {
        let score = l.models.iter().map(|m| {
            let want = tokens(m);
            // Every number (focal lengths, aperture) has to match exactly; most words should.
            if want.is_empty() || want.iter().any(|t| is_num(t) && !have.contains(t)) {
                return 0.0;
            }
            let hit = want.iter().filter(|t| have.contains(t)).count() as f32;
            let lens_words = tokens(&shot.lens).len().max(1) as f32;
            hit / want.len() as f32 - 0.02 * (lens_words - hit).max(0.0)
        }).fold(0.0, f32::max);
        // Prefer a profile measured on a sensor like this camera's.
        let near = crop.map_or(0.0, |c| -0.01 * (l.crop - c).abs());
        (score >= 0.75).then_some((score + near, l))
    }).max_by(|a, b| a.0.total_cmp(&b.0)).map(|x| x.1).or_else(fixed)
}

/// Lens and exposure details for a photo: rawler for RAW files, the EXIF block for everything else.
pub fn shot(path: &Path) -> Option<Shot> {
    let real = crate::load::copy_of(path);
    let path = real.as_deref().unwrap_or(path);
    if crate::load::is_raw(path) {
        use rawler::{decoders::RawDecodeParams, rawsource::RawSource};
        let src = RawSource::new(path).ok()?;
        let m = rawler::get_decoder(&src).ok()?.raw_metadata(&src, &RawDecodeParams::default()).ok()?;
        let r = |v: Option<rawler::formats::tiff::Rational>| v.map_or(0.0, |r| r.n as f32 / r.d.max(1) as f32);
        return Some(Shot {
            make: m.make,
            model: m.model,
            lens_make: m.exif.lens_make.unwrap_or_default(),
            lens: m.exif.lens_model.or(m.lens.map(|l| l.lens_model)).unwrap_or_default(),
            focal: r(m.exif.focal_length),
            fnum: r(m.exif.fnumber),
            crop: None,
            iso: m.exif.iso_speed_ratings.map(|v| v as f32).or(m.exif.iso_speed.map(|v| v as f32)).unwrap_or(0.0),
            shutter: r(m.exif.exposure_time),
            bias: m.exif.exposure_bias.map_or(0.0, |b| b.n as f32 / (b.d as f32).max(1.0)),
            taken: m.exif.date_time_original.unwrap_or_default(),
            gps: None,
        });
    }
    let exif = crate::load::exif_block(path)?;
    parse_exif(&exif)
}

/// The few tags we need from a TIFF-structured EXIF block.
pub fn parse_exif(b: &[u8]) -> Option<Shot> {
    let le = match b.get(..2)? { b"II" => true, b"MM" => false, _ => return None };
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| if le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) });
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| { let a = [s[0], s[1], s[2], s[3]]; if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) } });
    let mut shot = Shot::default();
    let mut f35 = 0.0;
    let mut ifds = vec![(u32_at(4)? as usize, false)];
    let mut seen = 0;
    // GPS: latitude, longitude (degrees, minutes, seconds), their N/S E/W references, altitude and its sign.
    let (mut lat, mut lon, mut alt, mut refs) = (None::<f64>, None::<f64>, None::<f64>, [b'N', b'E', 0]);
    while let Some((ifd, is_gps)) = ifds.pop() {
        seen += 1;
        if seen > 4 {
            break;
        }
        let n = u16_at(ifd)? as usize;
        for e in 0..n.min(500) {
            let at = ifd + 2 + e * 12;
            let (tag, typ, count) = (u16_at(at)?, u16_at(at + 2)?, u32_at(at + 4)? as usize);
            let size = count * match typ { 3 => 2, 4 | 9 => 4, 5 | 10 => 8, _ => 1 };
            let data = if size <= 4 { at + 8 } else { u32_at(at + 8)? as usize };
            let text = || b.get(data..data + count).map(|s| String::from_utf8_lossy(s).trim_end_matches('\0').trim().to_string()).unwrap_or_default();
            let rational = || Some(u32_at(data)? as f32 / u32_at(data + 4)?.max(1) as f32);
            let rat64 = |k: usize| Some(u32_at(data + 8 * k)? as f64 / u32_at(data + 8 * k + 4)?.max(1) as f64);
            let dms = || Some(rat64(0)? + rat64(1).unwrap_or(0.0) / 60.0 + rat64(2).unwrap_or(0.0) / 3600.0);
            if is_gps {
                match tag {
                    0x0001 => refs[0] = b.get(at + 8).copied().unwrap_or(b'N'),
                    0x0002 if count == 3 && typ == 5 => lat = dms(),
                    0x0003 => refs[1] = b.get(at + 8).copied().unwrap_or(b'E'),
                    0x0004 if count == 3 && typ == 5 => lon = dms(),
                    0x0005 => refs[2] = b.get(at + 8).copied().unwrap_or(0),
                    0x0006 if typ == 5 => alt = rat64(0),
                    _ => {}
                }
                continue;
            }
            match tag {
                0x010F => shot.make = text(),
                0x0110 => shot.model = text(),
                0x8769 => ifds.push((u32_at(at + 8)? as usize, false)),
                0x8825 => ifds.push((u32_at(at + 8)? as usize, true)),
                0x829D => shot.fnum = rational().unwrap_or(0.0),
                0x829A => shot.shutter = rational().unwrap_or(0.0),
                0x8827 => shot.iso = u16_at(at + 8).unwrap_or(0) as f32,
                0x9003 => shot.taken = text(),
                0x9204 => shot.bias = u32_at(data).zip(u32_at(data + 4)).map_or(0.0, |(n, d)| n as i32 as f32 / (d as i32).max(1) as f32),
                0x920A => shot.focal = rational().unwrap_or(0.0),
                0xA405 => f35 = u16_at(at + 8).unwrap_or(0) as f32,
                0xA433 => shot.lens_make = text(),
                0xA434 => shot.lens = text(),
                _ => {}
            }
        }
    }
    if f35 > 0.0 && shot.focal > 0.0 {
        shot.crop = Some(f35 / shot.focal);
    }
    if let (Some(la), Some(lo)) = (lat, lon) {
        let ele = alt.map(|a| if refs[2] == 1 { -a } else { a });
        shot.gps = Some(crate::geotag::Location { lat: if refs[0] == b'S' { -la } else { la }, lon: if refs[1] == b'W' { -lo } else { lo }, ele });
    }
    Some(shot)
}

/// Settings for a photo: its lens profile name, focal length, aperture and crop factor, if a lens matches.
pub fn detect(path: &Path) -> Option<(String, f32, f32, f32)> {
    let s = shot(path)?;
    let l = find(&s)?;
    Some((l.name(), s.focal, s.fnum, camera_crop(&s).unwrap_or(l.crop)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_endian_exif_reads_camera_and_settings() {
        // "MM" (big-endian) TIFF as Nikon bodies and many scanners write it: IFD0 with Make and an Exif
        // pointer, then an Exif IFD with FNumber (rational, stored after the IFDs), ISO and 35 mm focal length.
        let mut b: Vec<u8> = b"MM\0\x2a".to_vec();
        b.extend(8u32.to_be_bytes());
        let entry = |b: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: [u8; 4]| {
            b.extend(tag.to_be_bytes());
            b.extend(typ.to_be_bytes());
            b.extend(count.to_be_bytes());
            b.extend(value);
        };
        let ifd0 = 8;
        let exif = ifd0 + 2 + 2 * 12 + 4;
        let data = exif + 2 + 4 * 12 + 4;
        b.extend(2u16.to_be_bytes());
        entry(&mut b, 0x010F, 2, 6, (data as u32 + 8).to_be_bytes()); // Make, 6 bytes, after the rationals
        entry(&mut b, 0x8769, 4, 1, (exif as u32).to_be_bytes());
        b.extend(0u32.to_be_bytes());
        b.extend(4u16.to_be_bytes());
        entry(&mut b, 0x829D, 5, 1, (data as u32).to_be_bytes()); // FNumber 28/10
        entry(&mut b, 0x8827, 3, 1, [0x01, 0x90, 0, 0]); // ISO 400: a SHORT sits in the first two bytes
        entry(&mut b, 0x920A, 5, 1, (data as u32).to_be_bytes()); // focal length, same 2.8 for brevity
        entry(&mut b, 0xA405, 3, 1, [0, 42, 0, 0]); // 42 mm equivalent
        b.extend(0u32.to_be_bytes());
        assert_eq!(b.len(), data);
        b.extend(28u32.to_be_bytes());
        b.extend(10u32.to_be_bytes());
        b.extend(b"NIKON\0");
        let s = parse_exif(&b).expect("parses");
        assert_eq!((s.make.as_str(), s.fnum, s.iso, s.focal), ("NIKON", 2.8, 400.0, 2.8));
        assert_eq!(s.crop, Some(15.0));
    }

    #[test]
    fn database_loads_and_matches_exif_names() {
        let d = db();
        assert!(d.lenses.len() > 1000, "{}", d.lenses.len());
        let shot = Shot { make: "Sony".into(), model: "ILCE-7M3".into(), lens: "FE 24-70mm F2.8 GM".into(), focal: 24.0, fnum: 2.8, ..Default::default() };
        let l = find(&shot).expect("lens found");
        assert!(l.name().contains("24-70mm") && l.name().contains("GM"), "{}", l.name());
        assert_eq!(camera_crop(&shot), Some(1.0));
        let p = profile(&l.name(), 24.0, 2.8, 1.0).unwrap();
        assert!(p.dist.is_some() && p.vig.is_some());
        // Wide open the corners are darker: the correction brightens them.
        let k = p.vig.unwrap();
        let r = p.vig_scale;
        assert!(1.0 + k[0] * r * r + k[1] * r.powi(4) + k[2] * r.powi(6) < 0.95);
        // A compact camera gets its built-in lens.
        let s40 = Shot { make: "Canon".into(), model: "Canon PowerShot S40".into(), ..Default::default() };
        assert!(find(&s40).is_some_and(|l| l.name().contains("S30")));
        // A made-up lens finds nothing.
        assert!(find(&Shot { lens: "Mystery 33mm f/1.1".into(), ..Default::default() }).is_none());
    }

    #[test]
    fn ptlens_keeps_the_centre_and_bends_the_edges() {
        let d = Dist::Pt([0.0, -0.05, 0.0]);
        assert!((d.apply(0.01) - 0.01).abs() < 1e-4);
        assert!(d.apply(1.5) < 1.5); // barrel lens: the corner is pulled in, so the correction samples nearer the centre
    }
}

//! Finishing an export: exact canvas sizes (print, social), watermark, file names from a pattern,
//! and the small set of camera and film details written into exports when asked.
use crate::develop::Params;
use crate::lens::Shot;
use image::Rgb32FImage;
use std::path::Path;

/// Paper sizes in inches (short side, long side).
pub const PAPERS: &[(&str, f32, f32)] = &[
    ("4 × 6 in", 4.0, 6.0),
    ("5 × 7 in", 5.0, 7.0),
    ("8 × 10 in", 8.0, 10.0),
    ("8.5 × 11 in", 8.5, 11.0),
    ("11 × 14 in", 11.0, 14.0),
    ("13 × 19 in", 13.0, 19.0),
    ("A5", 5.83, 8.27),
    ("A4", 8.27, 11.69),
    ("A3", 11.69, 16.54),
    ("A3+", 13.0, 19.0),
    ("30 × 40 cm", 11.81, 15.75),
    ("50 × 70 cm", 19.69, 27.56),
];

/// Paper preview (soft proof): how a print on this paper roughly looks. Paper white and ink black
/// (display sRGB, 0–1) and how much colour the paper holds. An approximation, not an ICC printer profile.
pub const PROOF_PAPERS: &[(&str, [f32; 3], f32, f32)] = &[
    ("Glossy", [0.97, 0.97, 0.98], 0.03, 1.0),
    ("Lustre", [0.95, 0.95, 0.94], 0.05, 0.95),
    ("Matte", [0.94, 0.93, 0.91], 0.11, 0.85),
    ("Fine-art rag", [0.95, 0.93, 0.88], 0.13, 0.82),
    ("Newsprint", [0.86, 0.85, 0.80], 0.22, 0.55),
];

/// Squeeze display pixels into the paper's range: lifted blacks, paper-tinted whites, less saturation.
pub fn proof(px: &mut [f32], paper: usize) {
    use rayon::prelude::*;
    let Some(&(_, white, black, sat)) = PROOF_PAPERS.get(paper) else { return };
    px.par_chunks_mut(3).for_each(|p| {
        let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
        for c in 0..3 {
            let v = (y + (p[c] - y) * sat).clamp(0.0, 1.0);
            p[c] = black + v * (white[c] - black);
        }
    });
}

/// One-click export sets: name, canvas (0 = keep the photo's shape), long edge, JPEG quality.
pub const SETS: &[(&str, [u32; 2], u32, u8)] = &[
    ("Instagram", [1080, 1350], 0, 90),
    ("Story", [1080, 1920], 0, 90),
    ("Portfolio", [0, 0], 2048, 90),
];

/// Fit `img` inside a `w`×`h` canvas (turned to match the photo's orientation when `turn`), on white or black;
/// with `fill`, crop to cover the canvas instead.
pub fn fit_canvas(img: Rgb32FImage, mut w: u32, mut h: u32, turn: bool, white: bool, fill: bool) -> Rgb32FImage {
    let (iw, ih) = img.dimensions();
    if turn && (iw > ih) != (w > h) && w != h {
        std::mem::swap(&mut w, &mut h);
    }
    let k = if fill { (w as f32 / iw as f32).max(h as f32 / ih as f32) } else { (w as f32 / iw as f32).min(h as f32 / ih as f32) };
    let (sw, sh) = (((iw as f32 * k).round() as u32).max(1), ((ih as f32 * k).round() as u32).max(1));
    let scaled = image::imageops::resize(&img, sw, sh, image::imageops::FilterType::CatmullRom);
    let mut out = Rgb32FImage::from_pixel(w, h, image::Rgb([if white { 1.0 } else { 0.0 }; 3]));
    image::imageops::replace(&mut out, &scaled, (w as i64 - sw as i64) / 2, (h as i64 - sh as i64) / 2);
    out
}

/// Text or logo in a corner (0 bottom right, 1 bottom left, 2 top right, 3 top left, 4 centre).
/// `size` is the mark's height as a percentage of the photo's short side; `opacity` 0–100.
pub fn watermark(img: &mut Rgb32FImage, text: &str, logo: &str, pos: u8, size: f32, opacity: f32) {
    let (w, h) = img.dimensions();
    let short = w.min(h) as f32;
    let mark_h = (short * size / 100.0).max(8.0);
    let a = (opacity / 100.0).clamp(0.0, 1.0);
    // The mark as (rgb, alpha) rows.
    let mark: Option<(u32, u32, Vec<[f32; 4]>)> = if !logo.trim().is_empty() {
        image::open(logo.trim()).ok().map(|l| {
            let l = l.to_rgba32f();
            let mw = ((l.width() as f32 * mark_h / l.height() as f32).round() as u32).max(1);
            let l = image::imageops::resize(&l, mw, mark_h as u32, image::imageops::FilterType::CatmullRom);
            (l.width(), l.height(), l.pixels().map(|p| p.0).collect())
        })
    } else if !text.trim().is_empty() {
        let px = mark_h;
        let cw = (px * 0.62 * text.chars().count() as f32 + px) as u32;
        let ch = (px * 1.4) as u32;
        let mut canvas = Rgb32FImage::new(cw, ch);
        crate::develop::draw_text(&mut canvas, text.trim(), cw as f32 / 2.0, ch as f32 / 2.0, px, 1.0);
        // Trim to the ink so corners line up with the margin.
        let cov = |x: u32, y: u32| canvas.get_pixel(x, y).0[0];
        let cols: Vec<u32> = (0..cw).filter(|x| (0..ch).any(|y| cov(*x, y) > 0.0)).collect();
        let rows: Vec<u32> = (0..ch).filter(|y| (0..cw).any(|x| cov(x, *y) > 0.0)).collect();
        match (cols.first(), cols.last(), rows.first(), rows.last()) {
            (Some(&x0), Some(&x1), Some(&y0), Some(&y1)) => {
                let (mw, mh) = (x1 - x0 + 1, y1 - y0 + 1);
                Some((mw, mh, (0..mh).flat_map(|y| (0..mw).map(move |x| (x, y))).map(|(x, y)| { let c = cov(x + x0, y + y0); [1.0, 1.0, 1.0, c] }).collect()))
            }
            _ => None,
        }
    } else {
        None
    };
    let Some((mw, mh, px)) = mark else { return };
    let m = (short * 0.03) as i64;
    let (x0, y0) = match pos {
        1 => (m, h as i64 - mh as i64 - m),
        2 => (w as i64 - mw as i64 - m, m),
        3 => (m, m),
        4 => ((w as i64 - mw as i64) / 2, (h as i64 - mh as i64) / 2),
        _ => (w as i64 - mw as i64 - m, h as i64 - mh as i64 - m),
    };
    for y in 0..mh {
        for x in 0..mw {
            let (tx, ty) = (x0 + x as i64, y0 + y as i64);
            if tx < 0 || ty < 0 || tx >= w as i64 || ty >= h as i64 {
                continue;
            }
            let s = px[(y * mw + x) as usize];
            let k = s[3] * a;
            if k > 0.0 {
                let d = img.get_pixel_mut(tx as u32, ty as u32);
                d.0 = [0, 1, 2].map(|c| d.0[c] + (s[c].clamp(0.0, 1.0) - d.0[c]) * k);
            }
        }
    }
}

/// The film stock's full name when the photo is a converted negative.
pub fn stock_name(p: &Params) -> Option<String> {
    p.neg.enabled.then(|| crate::negative::STOCKS.iter().find(|s| s.id == p.neg.stock).map(|s| format!("{} {}", s.brand, s.name))).flatten()
}

/// Tokens for file-name patterns, with what each one gives.
pub const NAME_TOKENS: &[(&str, &str)] = &[
    ("{name}", "original name"), ("{n}", "number 001, 002…"), ("{date}", "date taken 2026-10-26"), ("{time}", "time taken 142530"),
    ("{camera}", "camera"), ("{lens}", "lens"), ("{iso}", "ISO"), ("{focal}", "focal length 35mm"), ("{aperture}", "f-number f2.8"),
    ("{shutter}", "shutter 1-250s"), ("{roll}", "roll"), ("{stock}", "film stock"),
];

/// A file name (no extension) from a pattern using `NAME_TOKENS`.
/// An empty pattern keeps the original name.
pub fn file_name(pattern: &str, path: &Path, n: usize, roll: &str, p: &Params, shot: &Shot) -> String {
    let base = crate::export_stem(path);
    if pattern.trim().is_empty() {
        return base;
    }
    // Date taken from EXIF; the file's date when the photo has none.
    let taken: Vec<&str> = shot.taken.split([':', ' ', 'T', '-']).collect();
    let exif_date = (taken.len() >= 6 && taken[0].len() == 4).then(|| format!("{}-{}-{}", taken[0], taken[1], taken[2]));
    let time = if taken.len() >= 6 { format!("{}{}{}", taken[3], taken[4], taken[5].chars().take(2).collect::<String>()) } else { String::new() };
    let date = exif_date.unwrap_or_else(|| file_date(path));
    let num = |v: f32, pre: &str, post: &str| if v > 0.0 { format!("{pre}{}{post}", (v * 10.0).round() / 10.0) } else { String::new() };
    let shutter = if shot.shutter <= 0.0 { String::new() } else if shot.shutter < 0.5 { format!("1-{}s", (1.0 / shot.shutter).round()) } else { format!("{}s", (shot.shutter * 10.0).round() / 10.0) };
    let camera = if shot.model.to_lowercase().starts_with(&shot.make.to_lowercase()) { shot.model.clone() } else { format!("{} {}", shot.make, shot.model) };
    let out = pattern
        .replace("{name}", &base)
        .replace("{n}", &format!("{n:03}"))
        .replace("{date}", &date)
        .replace("{time}", &time)
        .replace("{roll}", roll.trim())
        .replace("{stock}", &stock_name(p).unwrap_or_default())
        .replace("{camera}", camera.trim())
        .replace("{lens}", shot.lens.trim())
        .replace("{iso}", &num(shot.iso, "ISO", ""))
        .replace("{focal}", &num(shot.focal, "", "mm"))
        .replace("{aperture}", &num(shot.fnum, "f", ""))
        .replace("{shutter}", &shutter);
    // Safe on every file system; collapse what empty fields leave behind.
    let clean: String = out.chars().map(|c| if c.is_alphanumeric() || " -_.()+".contains(c) { c } else { '-' }).collect();
    let mut s = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches(|c: char| c == '-' || c == ' ' || c == '_').to_string();
    if s.is_empty() { base } else { s }
}

/// The file's modification date, YYYY-MM-DD.
fn file_date(path: &Path) -> String {
    std::fs::metadata(crate::load::copy_of(path).as_deref().unwrap_or(path)).and_then(|m| m.modified()).ok()
        .map(|t| {
            let days = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86400).unwrap_or(0) as i64;
            // Civil date from days since 1970 (Howard Hinnant's algorithm).
            let z = days + 719468;
            let era = z.div_euclid(146097);
            let doe = z - era * 146097;
            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            format!("{:04}-{:02}-{:02}", yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
        }).unwrap_or_default()
}

/// What an export carries besides pixels. Camera details only when asked for (`ExportOpts::meta`); never GPS
/// or serial numbers. The colour profile always, when the export isn't plain sRGB.
#[derive(Clone, Default)]
pub struct Embed {
    /// A TIFF-structured EXIF block (see `exif_tiff`), empty = none.
    pub exif: Vec<u8>,
    /// An XMP packet (see `xmp_packet`), empty = none.
    pub xmp: String,
    /// An ICC profile, empty = sRGB, untagged.
    pub icc: Vec<u8>,
    pub dpi: Option<u16>,
}

enum Val {
    Ascii(String),
    Short(u16),
    Long(u32),
    Rational(u32, u32),
    Rationals(Vec<(u32, u32)>),
    Bytes(Vec<u8>),
}

/// n/d with small integers, as cameras write them: 1/125 s, 28/10 for f/2.8.
fn rational(v: f32) -> (u32, u32) {
    if v <= 0.0 {
        return (0, 1);
    }
    if v < 1.0 {
        let inv = 1.0 / v;
        if (inv - inv.round()).abs() < 0.02 * inv {
            return (1, inv.round() as u32);
        }
    }
    ((v * 1000.0).round() as u32, 1000)
}

/// The camera details as a TIFF-structured EXIF block (little-endian): description, make, model and software in
/// IFD0; exposure time, f-number, ISO, date taken, focal length and lens in the Exif IFD. For a JPEG APP1 segment
/// (after "Exif\0\0") or a PNG eXIf chunk.
pub fn exif_tiff(shot: &Shot, description: &str) -> Vec<u8> {
    let mut ifd0: Vec<(u16, Val)> = vec![];
    for (tag, v) in [(0x010Eu16, description), (0x010F, shot.make.as_str()), (0x0110, shot.model.as_str()), (0x0131, "Ferrotype")] {
        if !v.trim().is_empty() {
            ifd0.push((tag, Val::Ascii(v.trim().to_string())));
        }
    }
    let mut exif: Vec<(u16, Val)> = vec![];
    if shot.shutter > 0.0 {
        let (n, d) = rational(shot.shutter);
        exif.push((0x829A, Val::Rational(n, d)));
    }
    if shot.fnum > 0.0 {
        exif.push((0x829D, Val::Rational((shot.fnum * 10.0).round() as u32, 10)));
    }
    if shot.iso > 0.0 {
        exif.push((0x8827, Val::Short(shot.iso.round().min(65535.0) as u16)));
    }
    if shot.taken.trim().len() >= 10 {
        exif.push((0x9003, Val::Ascii(shot.taken.trim().to_string())));
    }
    if shot.focal > 0.0 {
        exif.push((0x920A, Val::Rational((shot.focal * 10.0).round() as u32, 10)));
    }
    if !shot.lens.trim().is_empty() {
        exif.push((0xA434, Val::Ascii(shot.lens.trim().to_string())));
    }
    // GPS IFD: version 2.3, latitude and longitude as degrees, minutes, seconds, and altitude.
    let mut gps: Vec<(u16, Val)> = vec![];
    if let Some(g) = shot.gps.filter(|g| g.lat.is_finite() && g.lon.is_finite()) {
        let dms = |v: f64| {
            let a = v.abs();
            let (d, m) = (a.floor(), (a - a.floor()) * 60.0);
            vec![(d as u32, 1), (m.floor() as u32, 1), (((m - m.floor()) * 60.0 * 10000.0).round() as u32, 10000)]
        };
        gps.push((0x0000, Val::Bytes(vec![2, 3, 0, 0])));
        gps.push((0x0001, Val::Ascii(if g.lat >= 0.0 { "N" } else { "S" }.into())));
        gps.push((0x0002, Val::Rationals(dms(g.lat))));
        gps.push((0x0003, Val::Ascii(if g.lon >= 0.0 { "E" } else { "W" }.into())));
        gps.push((0x0004, Val::Rationals(dms(g.lon))));
        if let Some(e) = g.ele.filter(|e| e.is_finite()) {
            gps.push((0x0005, Val::Bytes(vec![(e < 0.0) as u8])));
            gps.push((0x0006, Val::Rationals(vec![((e.abs() * 10.0).round() as u32, 10)])));
        }
    }
    let has_exif = !exif.is_empty();
    let has_gps = !gps.is_empty();
    if has_exif {
        ifd0.push((0x8769, Val::Long(0))); // offset filled in below
    }
    if has_gps {
        ifd0.push((0x8825, Val::Long(0)));
    }
    let ifd_size = |n: usize| 2 + 12 * n + 4;
    let exif_at = 8 + ifd_size(ifd0.len());
    let gps_at = exif_at + if has_exif { ifd_size(exif.len()) } else { 0 };
    let mut data_at = gps_at + if has_gps { ifd_size(gps.len()) } else { 0 };
    let (mut head, mut data) = (b"II*\0".to_vec(), vec![]);
    head.extend_from_slice(&8u32.to_le_bytes());
    let mut write_ifd = |head: &mut Vec<u8>, entries: &[(u16, Val)]| {
        head.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, v) in entries {
            head.extend_from_slice(&tag.to_le_bytes());
            let (typ, count, bytes): (u16, u32, Vec<u8>) = match v {
                Val::Ascii(t) => {
                    let mut b = t.as_bytes().to_vec();
                    b.push(0);
                    (2, b.len() as u32, b)
                }
                Val::Short(x) => (3, 1, x.to_le_bytes().to_vec()),
                Val::Long(x) => (4, 1, match *tag {
                    0x8769 => (exif_at as u32).to_le_bytes().to_vec(),
                    0x8825 => (gps_at as u32).to_le_bytes().to_vec(),
                    _ => x.to_le_bytes().to_vec(),
                }),
                Val::Rational(n, d) => (5, 1, [n.to_le_bytes(), d.to_le_bytes()].concat()),
                Val::Rationals(v) => (5, v.len() as u32, v.iter().flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat()).collect()),
                Val::Bytes(b) => (1, b.len() as u32, b.clone()),
            };
            head.extend_from_slice(&typ.to_le_bytes());
            head.extend_from_slice(&count.to_le_bytes());
            if bytes.len() <= 4 {
                let mut val = [0u8; 4];
                val[..bytes.len()].copy_from_slice(&bytes);
                head.extend_from_slice(&val);
            } else {
                head.extend_from_slice(&(data_at as u32).to_le_bytes());
                data_at += bytes.len() + bytes.len() % 2;
                data.extend_from_slice(&bytes);
                if bytes.len() % 2 == 1 {
                    data.push(0); // offsets stay even, as TIFF asks
                }
            }
        }
        head.extend_from_slice(&0u32.to_le_bytes());
    };
    // Tags in ascending order within each IFD.
    ifd0.sort_by_key(|e| e.0);
    write_ifd(&mut head, &ifd0);
    if has_exif {
        write_ifd(&mut head, &exif);
    }
    if has_gps {
        write_ifd(&mut head, &gps);
    }
    head.extend(data);
    head
}

/// An EXIF APP1 segment for a JPEG: see `exif_tiff`.
#[cfg(test)]
pub fn exif_segment(shot: &Shot, description: &str) -> Vec<u8> {
    let mut body = b"Exif\0\0".to_vec();
    body.extend(exif_tiff(shot, description));
    segment(0xE1, &body)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The same details as an XMP packet, plus keywords: what TIFF and PNG exports carry (and what Lightroom, Bridge
/// and exiftool read from them).
pub fn xmp_packet(shot: &Shot, description: &str, keywords: &[String]) -> String {
    let mut attrs = vec![("xmp:CreatorTool", "Ferrotype".to_string())];
    let mut add = |k: &'static str, v: String| {
        if !v.trim().is_empty() {
            attrs.push((k, v));
        }
    };
    add("tiff:Make", shot.make.trim().to_string());
    add("tiff:Model", shot.model.trim().to_string());
    add("exifEX:LensModel", shot.lens.trim().to_string());
    add("aux:Lens", shot.lens.trim().to_string());
    let rat = |v: f32| { let (n, d) = rational(v); format!("{n}/{d}") };
    if shot.shutter > 0.0 {
        add("exif:ExposureTime", rat(shot.shutter));
    }
    if shot.fnum > 0.0 {
        add("exif:FNumber", format!("{}/10", (shot.fnum * 10.0).round()));
    }
    if shot.focal > 0.0 {
        add("exif:FocalLength", format!("{}/10", (shot.focal * 10.0).round()));
    }
    // "2026:05:04 12:00:00" → "2026-05-04T12:00:00"
    let t = shot.taken.trim();
    if t.len() >= 19 && t.is_ascii() {
        add("exif:DateTimeOriginal", format!("{}T{}", t[..10].replace(':', "-"), &t[11..19]));
    }
    if let Some(g) = shot.gps.filter(|g| g.lat.is_finite() && g.lon.is_finite()) {
        // XMP writes GPS as "DDD,MM.mmmmmmK".
        let dm = |v: f64, pos: char, neg: char| { let a = v.abs(); format!("{},{:.6}{}", a.floor(), (a - a.floor()) * 60.0, if v >= 0.0 { pos } else { neg }) };
        add("exif:GPSVersionID", "2.3.0.0".into());
        add("exif:GPSLatitude", dm(g.lat, 'N', 'S'));
        add("exif:GPSLongitude", dm(g.lon, 'E', 'W'));
        if let Some(e) = g.ele.filter(|e| e.is_finite()) {
            add("exif:GPSAltitudeRef", if e < 0.0 { "1" } else { "0" }.into());
            add("exif:GPSAltitude", format!("{}/10", (e.abs() * 10.0).round()));
        }
    }
    let mut x = String::from("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
    x += "<rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:tiff=\"http://ns.adobe.com/tiff/1.0/\" xmlns:exif=\"http://ns.adobe.com/exif/1.0/\" xmlns:exifEX=\"http://cipa.jp/exif/1.0/\" xmlns:aux=\"http://ns.adobe.com/exif/1.0/aux/\"";
    for (k, v) in &attrs {
        x += &format!("\n  {k}=\"{}\"", xml_escape(v));
    }
    x += ">\n";
    if shot.iso > 0.0 {
        x += &format!("<exif:ISOSpeedRatings><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></exif:ISOSpeedRatings>\n", shot.iso.round());
    }
    if !description.trim().is_empty() {
        x += &format!("<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>\n", xml_escape(description.trim()));
    }
    let kws: Vec<&String> = keywords.iter().filter(|k| !k.trim().is_empty()).collect();
    if !kws.is_empty() {
        x += "<dc:subject><rdf:Bag>";
        for k in kws {
            x += &format!("<rdf:li>{}</rdf:li>", xml_escape(k.trim()));
        }
        x += "</rdf:Bag></dc:subject>\n";
    }
    x += "</rdf:Description>\n</rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>";
    x
}

/// JPEG APP segments for everything in `e` except the DPI (the encoder writes that): EXIF, XMP and the ICC
/// profile (split over several APP2 segments when it is larger than one can hold).
pub fn jpeg_segments(e: &Embed) -> Vec<u8> {
    let mut out = vec![];
    if !e.exif.is_empty() {
        let mut body = b"Exif\0\0".to_vec();
        body.extend_from_slice(&e.exif);
        out.extend(segment(0xE1, &body));
    }
    if !e.xmp.is_empty() && e.xmp.len() < 65_000 {
        let mut body = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        body.extend_from_slice(e.xmp.as_bytes());
        out.extend(segment(0xE1, &body));
    }
    if !e.icc.is_empty() {
        let chunks: Vec<&[u8]> = e.icc.chunks(65_519).collect();
        for (i, c) in chunks.iter().enumerate() {
            let mut body = b"ICC_PROFILE\0".to_vec();
            body.push(i as u8 + 1);
            body.push(chunks.len() as u8);
            body.extend_from_slice(c);
            out.extend(segment(0xE2, &body));
        }
    }
    out
}

/// A 16-bit TIFF with the XMP packet (camera details and keywords) and ICC profile from `e`.
pub fn write_tiff(img: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>, path: &Path, e: &Embed) -> Result<(), String> {
    use tiff::encoder::{Rational, TiffEncoder, colortype::RGB16};
    use tiff::tags::{ResolutionUnit, Tag};
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = TiffEncoder::new(std::io::BufWriter::new(f)).map_err(|e| e.to_string())?;
    let mut im = enc.new_image::<RGB16>(img.width(), img.height()).map_err(|e| e.to_string())?;
    let err = |e: tiff::TiffError| e.to_string();
    {
        let d = im.encoder();
        d.write_tag(Tag::Software, "Ferrotype").map_err(err)?;
        if !e.xmp.is_empty() {
            d.write_tag(Tag::Unknown(700), e.xmp.as_bytes()).map_err(err)?;
        }
        if !e.icc.is_empty() {
            d.write_tag(Tag::Unknown(34675), &e.icc[..]).map_err(err)?;
        }
    }
    if let Some(dpi) = e.dpi {
        im.resolution(ResolutionUnit::Inch, Rational { n: dpi as u32, d: 1 });
    }
    im.write_data(img.as_raw()).map_err(err)
}

/// An 8-bit PNG with an eXIf chunk, the XMP packet (iTXt) and the ICC profile from `e` (an sRGB chunk when none).
pub fn write_png(img: &image::RgbImage, path: &Path, e: &Embed) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut info = png::Info::with_size(img.width(), img.height());
    info.color_type = png::ColorType::Rgb;
    info.bit_depth = png::BitDepth::Eight;
    if e.icc.is_empty() {
        info.srgb = Some(png::SrgbRenderingIntent::Perceptual);
    } else {
        info.icc_profile = Some(std::borrow::Cow::Owned(e.icc.clone()));
    }
    if !e.exif.is_empty() {
        info.exif_metadata = Some(std::borrow::Cow::Owned(e.exif.clone()));
    }
    if let Some(dpi) = e.dpi {
        let ppm = (dpi as f32 / 0.0254).round() as u32;
        info.pixel_dims = Some(png::PixelDimensions { xppu: ppm, yppu: ppm, unit: png::Unit::Meter });
    }
    let mut enc = png::Encoder::with_info(std::io::BufWriter::new(f), info).map_err(|e| e.to_string())?;
    if !e.xmp.is_empty() {
        enc.add_itxt_chunk("XML:com.adobe.xmp".into(), e.xmp.clone()).map_err(|e| e.to_string())?;
    }
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(img.as_raw()).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())
}

/// A JPEG marker segment: FF, marker, big-endian length (which counts itself), body.
pub fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
    let mut s = vec![0xFF, marker];
    s.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    s.extend_from_slice(body);
    s
}

/// Where new segments go: right after the start marker, and after a JFIF header, which must come first.
pub fn insert_at(jpeg: &[u8]) -> usize {
    if jpeg.get(2..4) == Some(&[0xFF, 0xE0]) { 4 + u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize } else { 2 }
}

/// Put segments (one or several, back to back) at `insert_at`.
pub fn insert_segment(jpeg: &[u8], seg: &[u8]) -> Vec<u8> {
    let at = insert_at(jpeg);
    [&jpeg[..at], seg, &jpeg[at..]].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_a_pattern() {
        let p = Params { neg: crate::negative::NegParams { enabled: true, stock: "portra400".into(), ..Default::default() }, ..Default::default() };
        let shot = Shot { make: "Nikon".into(), model: "F3".into(), ..Default::default() };
        let n = file_name("Roll {roll} - {n} - {stock} {lens}", Path::new("/x/scan_0007.tif"), 7, "12", &p, &shot);
        assert_eq!(n, "Roll 12 - 007 - Kodak Portra 400");
        assert_eq!(file_name("", Path::new("/x/IMG_1.NEF"), 1, "", &p, &shot), "IMG_1");
        assert_eq!(file_name("{camera}/{name}", Path::new("/x/a.jpg"), 1, "", &Params::default(), &shot), "Nikon F3-a");
        // A broken byte in the EXIF date became a 3-byte replacement character, and cutting it panicked.
        let odd = Shot { taken: "2024:05:06 12:34:\u{FFFD}7".into(), ..Default::default() };
        assert_eq!(file_name("{time}", Path::new("/x/a.jpg"), 1, "", &Params::default(), &odd), "1234-7");
    }

    #[test]
    fn exif_has_the_camera_and_lens() {
        let shot = Shot { make: "Sony".into(), model: "ILCE-7M3".into(), lens: "FE 35mm F1.8".into(), ..Default::default() };
        let seg = exif_segment(&shot, "Roll 3 · Kodak Gold 200");
        let back = crate::lens::parse_exif(&seg[10..]).unwrap();
        assert_eq!((back.make.as_str(), back.model.as_str(), back.lens.as_str()), ("Sony", "ILCE-7M3", "FE 35mm F1.8"));
        assert!(String::from_utf8_lossy(&seg).contains("Kodak Gold 200"));
    }

    #[test]
    fn exif_carries_capture_settings() {
        let shot = Shot { make: String::new(), model: "Nikon FM2".into(), lens: "50mm f/1.8".into(), fnum: 2.8, shutter: 1.0 / 125.0, iso: 1600.0, focal: 50.0, taken: "2026:05:04 12:00:00".into(), ..Default::default() };
        let back = crate::lens::parse_exif(&exif_tiff(&shot, "Roll 12 · Kodak Tri-X 400 at 1600 (+2 push)")).unwrap();
        assert_eq!((back.model.as_str(), back.lens.as_str(), back.taken.as_str()), ("Nikon FM2", "50mm f/1.8", "2026:05:04 12:00:00"));
        assert_eq!((back.fnum, back.iso, back.focal), (2.8, 1600.0, 50.0));
        assert!((back.shutter - 0.008).abs() < 1e-6);
        let x = xmp_packet(&shot, "Roll <12>", &["street".into(), "Lisbon".into()]);
        assert!(x.contains("exif:FNumber=\"28/10\"") && x.contains("<rdf:li>1600</rdf:li>") && x.contains("Roll &lt;12&gt;"));
        assert!(x.contains("<rdf:li>Lisbon</rdf:li>") && x.contains("2026-05-04T12:00:00"));
    }

    #[test]
    fn exif_and_xmp_carry_a_location() {
        let at = crate::geotag::Location { lat: -33.856784, lon: 151.215297, ele: Some(12.5) };
        let shot = Shot { model: "X100V".into(), iso: 200.0, gps: Some(at), ..Default::default() };
        let back = crate::lens::parse_exif(&exif_tiff(&shot, "")).unwrap();
        let g = back.gps.unwrap();
        assert!((g.lat - at.lat).abs() < 1e-6 && (g.lon - at.lon).abs() < 1e-6 && g.ele == Some(12.5), "{g:?}");
        assert_eq!((back.model.as_str(), back.iso), ("X100V", 200.0), "the other IFDs still read");
        let x = xmp_packet(&shot, "", &[]);
        assert!(x.contains("exif:GPSLatitude=\"33,51.407040S\"") && x.contains("exif:GPSLongitude=\"151,12.917820E\""), "{x}");
        assert!(crate::lens::parse_exif(&exif_tiff(&Shot { gps: None, ..shot }, "")).unwrap().gps.is_none());
    }

    #[test]
    fn tiff_and_png_keep_their_metadata() {
        let dir = std::env::temp_dir().join(format!("ferro-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = Shot { model: "Leica M6".into(), iso: 400.0, ..Default::default() };
        let e = Embed { exif: exif_tiff(&shot, "Roll 3"), xmp: xmp_packet(&shot, "Roll 3", &["film".into()]), icc: vec![], dpi: Some(300) };
        let img = image::ImageBuffer::from_pixel(8, 6, image::Rgb([30000u16, 20000, 10000]));
        write_tiff(&img, &dir.join("a.tif"), &e).unwrap();
        let t = std::fs::read(dir.join("a.tif")).unwrap();
        assert!(String::from_utf8_lossy(&t).contains("Leica M6"));
        assert_eq!(image::open(dir.join("a.tif")).unwrap().to_rgb16().get_pixel(3, 3).0, [30000, 20000, 10000]);
        let rgb8 = image::RgbImage::from_pixel(8, 6, image::Rgb([200, 100, 50]));
        write_png(&rgb8, &dir.join("a.png"), &e).unwrap();
        let p = std::fs::read(dir.join("a.png")).unwrap();
        let s = String::from_utf8_lossy(&p);
        assert!(s.contains("eXIf") && s.contains("XML:com.adobe.xmp") && s.contains("Leica M6"));
        assert_eq!(image::open(dir.join("a.png")).unwrap().to_rgb8().get_pixel(1, 1).0, [200, 100, 50]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn canvas_and_watermark() {
        let img = Rgb32FImage::from_pixel(300, 200, image::Rgb([0.2; 3]));
        let c = fit_canvas(img, 1080, 1350, false, true, false);
        assert_eq!(c.dimensions(), (1080, 1350));
        assert_eq!(c.get_pixel(5, 5).0, [1.0; 3]); // padded white
        assert!((c.get_pixel(540, 675).0[0] - 0.2).abs() < 0.01);
        let mut m = Rgb32FImage::from_pixel(400, 300, image::Rgb([0.0; 3]));
        watermark(&mut m, "© Ferrotype", "", 0, 6.0, 80.0);
        let lit = m.pixels().filter(|p| p.0[0] > 0.3).count();
        assert!(lit > 50);
        assert!(m.get_pixel(10, 10).0[0] == 0.0); // only the bottom-right corner
    }
}

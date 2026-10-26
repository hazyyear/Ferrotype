//! Finishing an export: exact canvas sizes (print, social), watermark, file names from a pattern,
//! and the small set of camera and film details written into JPEGs when asked.
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
    let time = if taken.len() >= 6 { format!("{}{}{}", taken[3], taken[4], &taken[5][..taken[5].len().min(2)]) } else { String::new() };
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

/// An EXIF APP1 segment with camera, lens and a description (roll and film stock). Nothing else: no GPS,
/// no serial numbers, no capture settings.
pub fn exif_segment(shot: &Shot, description: &str) -> Vec<u8> {
    // IFD0: ImageDescription, Make, Model, Software, ExifIFD pointer; Exif IFD: LensModel.
    let mut ifd0: Vec<(u16, String)> = vec![];
    for (tag, v) in [(0x010Eu16, description), (0x010F, shot.make.as_str()), (0x0110, shot.model.as_str()), (0x0131, "Ferrotype")] {
        if !v.trim().is_empty() {
            ifd0.push((tag, v.trim().to_string()));
        }
    }
    let lens = shot.lens.trim().to_string();
    let n0 = ifd0.len() + usize::from(!lens.is_empty());
    let ifd0_size = 2 + 12 * n0 + 4;
    let exif_at = 8 + ifd0_size;
    let exif_size = if lens.is_empty() { 0 } else { 2 + 12 + 4 };
    let mut data_at = exif_at + exif_size;
    let (mut head, mut data) = (vec![], vec![]);
    let entry = |out: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: [u8; 4]| {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value);
    };
    let text = |out: &mut Vec<u8>, data: &mut Vec<u8>, data_at: &mut usize, tag: u16, v: &str| {
        let mut b = v.as_bytes().to_vec();
        b.push(0);
        if b.len() <= 4 {
            let mut val = [0u8; 4];
            val[..b.len()].copy_from_slice(&b);
            entry(out, tag, 2, b.len() as u32, val);
        } else {
            entry(out, tag, 2, b.len() as u32, (*data_at as u32).to_le_bytes());
            *data_at += b.len();
            data.extend_from_slice(&b);
        }
    };
    head.extend_from_slice(b"II*\0");
    head.extend_from_slice(&8u32.to_le_bytes());
    head.extend_from_slice(&(n0 as u16).to_le_bytes());
    for (tag, v) in &ifd0 {
        text(&mut head, &mut data, &mut data_at, *tag, v);
    }
    if !lens.is_empty() {
        entry(&mut head, 0x8769, 4, 1, (exif_at as u32).to_le_bytes());
    }
    head.extend_from_slice(&0u32.to_le_bytes());
    if !lens.is_empty() {
        head.extend_from_slice(&1u16.to_le_bytes());
        text(&mut head, &mut data, &mut data_at, 0xA434, &lens);
        head.extend_from_slice(&0u32.to_le_bytes());
    }
    let mut body = b"Exif\0\0".to_vec();
    body.extend(head);
    body.extend(data);
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    seg.extend(body);
    seg
}

/// Put a segment right after the JPEG's start marker (and its JFIF header, which must come first).
pub fn insert_segment(jpeg: &[u8], seg: &[u8]) -> Vec<u8> {
    let at = if jpeg.get(2..4) == Some(&[0xFF, 0xE0]) { 4 + u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize } else { 2 };
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

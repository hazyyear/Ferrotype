//! What film scanner software saves, and how to read it right.
//!
//! Epson Scan 2 writes 24/48-bit TIFFs with its own or a standard ICC profile (or none), and can put a whole
//! film holder in one multi-page TIFF. VueScan and SilverFast also save "raw" scans: linear 48-bit RGB, or
//! 64-bit RGBI with the infrared channel as a fourth sample that the file marks as "unspecified". Plain TIFF
//! readers drop that sample, and read the linear data as if it were gamma-encoded, so the scan comes out far
//! too dark. This module reads the file's tags to tell which kind of scan it is.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

/// What a scan's TIFF says about itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    /// The Software tag: "EPSON Scan 2", "VueScan 9.8.30", "SilverFast 9.2.5".
    pub software: String,
    /// Make and Model of the scanner: "EPSON", "Perfection V850".
    pub make: String,
    pub model: String,
    pub bits: u16,
    /// Colour samples per pixel (1 grey, 3 RGB), not counting extra ones.
    pub colours: u16,
    /// Extra samples per pixel: an infrared channel in a raw scan, or alpha.
    pub extra: u16,
    pub pages: usize,
    pub icc: bool,
    /// Scanning resolution in pixels per inch, 0 when not given.
    pub dpi: f32,
}

impl Info {
    /// The scanner's name for the Scan tab: "EPSON Perfection V850", or the software when the model isn't given.
    pub fn scanner(&self) -> String {
        let (make, model) = (self.make.trim(), self.model.trim());
        if model.is_empty() {
            return self.software.trim().to_string();
        }
        if make.is_empty() || model.to_ascii_lowercase().starts_with(&make.to_ascii_lowercase()) { model.into() } else { format!("{make} {model}") }
    }

    /// A raw scan holds the sensor's linear values with no profile: what VueScan and SilverFast save as raw
    /// (48-bit RGB or 64-bit RGBI, untagged). Epson Scan 2 always gamma-encodes, so its files never count.
    pub fn looks_linear(&self) -> bool {
        if self.icc || self.bits < 16 {
            return false;
        }
        let sw = self.software.to_ascii_lowercase();
        let raw_software = sw.contains("vuescan") || sw.contains("silverfast");
        // Infrared alongside the colours only comes in raw files, whoever wrote them.
        raw_software || (self.extra > 0 && !sw.contains("epson"))
    }
}

/// How to read the values of an untagged scan, set per roll in the Scan tab when the guess is wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Data {
    /// From the file's tags (`Info::looks_linear`).
    Auto,
    Linear,
    Gamma,
}

impl Data {
    pub fn parse(s: &str) -> Data {
        match s {
            "linear" => Data::Linear,
            "gamma" => Data::Gamma,
            _ => Data::Auto,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Data::Auto => "",
            Data::Linear => "linear",
            Data::Gamma => "gamma",
        }
    }
}

fn is_tiff(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"))
}

/// The tags of a TIFF scan, or None for anything else.
pub fn info(path: &Path) -> Option<Info> {
    if !is_tiff(path) {
        return None;
    }
    let mut dec = open(path)?;
    let text = |dec: &mut Decoder<_>, t| match dec.find_tag(t) {
        Ok(Some(v)) => v.into_string().unwrap_or_default().trim_end_matches('\0').trim().to_string(),
        _ => String::new(),
    };
    let software = text(&mut dec, Tag::Software);
    let make = text(&mut dec, Tag::Make);
    let model = text(&mut dec, Tag::Model);
    let bits = dec.find_tag_unsigned_vec::<u16>(Tag::BitsPerSample).ok().flatten().and_then(|v| v.first().copied()).unwrap_or(8);
    let samples = dec.find_tag_unsigned::<u16>(Tag::SamplesPerPixel).ok().flatten().unwrap_or(1);
    // Extra samples are whatever the colour model doesn't use, whether or not ExtraSamples lists them.
    let colours = match dec.find_tag_unsigned::<u16>(Tag::PhotometricInterpretation).ok().flatten() {
        Some(0 | 1) => 1,
        Some(5) => 4,
        _ => 3,
    }
    .min(samples);
    let extra = samples - colours;
    let icc = dec.find_tag(Tag::IccProfile).ok().flatten().is_some();
    let dpi = match dec.find_tag(Tag::XResolution) {
        Ok(Some(tiff::decoder::ifd::Value::Rational(n, d))) if d > 0 => n as f32 / d as f32,
        _ => 0.0,
    };
    let unit_cm = dec.find_tag_unsigned::<u16>(Tag::ResolutionUnit).ok().flatten() == Some(3);
    let mut pages = 1;
    while dec.more_images() && dec.next_image().is_ok() {
        pages += 1;
    }
    let dpi = if unit_cm { dpi * 2.54 } else { dpi };
    // Many writers put 1 or 72 when they don't know; a film or flatbed scan is never that coarse.
    let dpi = if dpi >= 100.0 { dpi } else { 0.0 };
    Some(Info { software, make, model, bits, colours, extra, pages, icc, dpi })
}

/// Whether the values of this file (or virtual copy) are linear light: the roll's setting, else the tags.
pub fn is_linear(path: &Path) -> bool {
    let real = crate::load::copy_of(path).unwrap_or_else(|| path.to_path_buf());
    let Some(info) = info(&real) else { return false };
    if info.icc {
        return false; // the profile says how to read it
    }
    match roll_data(&real) {
        Data::Linear => true,
        Data::Gamma => false,
        Data::Auto => info.looks_linear(),
    }
}

/// The roll's "scan data" setting, from the roll notes next to the file.
fn roll_data(path: &Path) -> Data {
    let roll = path.parent().and_then(crate::roll::read).unwrap_or_default();
    Data::parse(&roll.scan_data)
}

/// A TIFF decoder that sees every extra sample. Raw scans mark the infrared sample "unspecified", and the tiff
/// crate then hands back only the colours; we show it the file with that one tag value changed to "alpha".
pub fn open(path: &Path) -> Option<Decoder<Patched<std::io::BufReader<std::fs::File>>>> {
    let mut f = std::fs::File::open(path).ok()?;
    let patches = unspecified_extra_samples(&mut f).unwrap_or_default();
    f.seek(SeekFrom::Start(0)).ok()?;
    let r = Patched { inner: std::io::BufReader::new(f), pos: 0, patches };
    Decoder::new(r).ok().map(|d| d.with_limits(tiff::decoder::Limits::unlimited()))
}

/// A reader that replaces a few bytes of the file as they are read: (offset, bytes).
pub struct Patched<R> {
    inner: R,
    pos: u64,
    patches: Vec<(u64, Vec<u8>)>,
}

impl<R: Read> Read for Patched<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        let (start, end) = (self.pos, self.pos + n as u64);
        for (at, bytes) in &self.patches {
            for (i, b) in bytes.iter().enumerate() {
                let p = at + i as u64;
                if p >= start && p < end {
                    buf[(p - start) as usize] = *b;
                }
            }
        }
        self.pos = end;
        Ok(n)
    }
}

impl<R: Seek> Seek for Patched<R> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = self.inner.seek(to)?;
        Ok(self.pos)
    }
}

/// Where each IFD keeps a single ExtraSamples value of 0 (unspecified), with the bytes that make it 2 (alpha).
fn unspecified_extra_samples<R: Read + Seek>(r: &mut R) -> std::io::Result<Vec<(u64, Vec<u8>)>> {
    let mut head = [0u8; 16];
    r.read_exact(&mut head[..8])?;
    let le = match &head[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Ok(vec![]),
    };
    let u16_at = |b: &[u8]| if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) };
    let u32_at = |b: &[u8]| if le { u32::from_le_bytes(b[..4].try_into().unwrap()) } else { u32::from_be_bytes(b[..4].try_into().unwrap()) };
    let u64_at = |b: &[u8]| if le { u64::from_le_bytes(b[..8].try_into().unwrap()) } else { u64::from_be_bytes(b[..8].try_into().unwrap()) };
    let big = match u16_at(&head[2..4]) {
        42 => false,
        43 => true,
        _ => return Ok(vec![]),
    };
    let mut next = if big {
        r.read_exact(&mut head[8..16])?;
        u64_at(&head[8..16])
    } else {
        u32_at(&head[4..8]) as u64
    };
    let (count_len, entry_len, value_at) = if big { (8u64, 20u64, 12u64) } else { (2, 12, 8) };
    let two = if le { vec![2u8, 0] } else { vec![0u8, 2] };
    let mut out = vec![];
    let mut seen = 0;
    while next != 0 && seen < 4096 {
        seen += 1;
        r.seek(SeekFrom::Start(next))?;
        let mut c = [0u8; 8];
        r.read_exact(&mut c[..count_len as usize])?;
        let n = if big { u64_at(&c) } else { u16_at(&c) as u64 }.min(4096);
        let mut entries = vec![0u8; (n * entry_len) as usize];
        r.read_exact(&mut entries)?;
        for (i, e) in entries.chunks_exact(entry_len as usize).enumerate() {
            let (tag, typ) = (u16_at(&e[0..2]), u16_at(&e[2..4]));
            let count = if big { u64_at(&e[4..12]) } else { u32_at(&e[4..8]) as u64 };
            if tag == 338 && typ == 3 && count == 1 && u16_at(&e[value_at as usize..]) == 0 {
                out.push((next + count_len + i as u64 * entry_len + value_at, two.clone()));
            }
        }
        let mut nb = [0u8; 8];
        let len = if big { 8 } else { 4 };
        if r.read_exact(&mut nb[..len]).is_err() {
            break;
        }
        next = if big { u64_at(&nb) } else { u32_at(&nb) as u64 };
    }
    Ok(out)
}

/// Every sample of one page, as stored (chunky or planar), as f32 0..1: (width, height, samples, interleaved data).
pub fn read_samples(path: &Path, page: usize) -> Option<(u32, u32, usize, Vec<f32>)> {
    let mut dec = open(path)?;
    if page > 0 {
        dec.seek_to_image(page).ok()?;
    }
    let (w, h) = dec.dimensions().ok()?;
    let samples = dec.find_tag_unsigned::<u16>(Tag::SamplesPerPixel).ok().flatten().unwrap_or(1) as usize;
    let planar = dec.find_tag_unsigned::<u16>(Tag::PlanarConfiguration).ok().flatten() == Some(2);
    let mut buf = DecodingResult::U8(vec![]);
    dec.read_image_to_buffer(&mut buf).ok()?;
    let v: Vec<f32> = match buf {
        DecodingResult::U8(v) => v.iter().map(|&x| x as f32 / 255.0).collect(),
        DecodingResult::U16(v) => v.iter().map(|&x| x as f32 / 65535.0).collect(),
        DecodingResult::F32(v) => v,
        _ => return None,
    };
    let n = (w * h) as usize;
    if v.len() != n * samples {
        return None;
    }
    if !planar || samples == 1 {
        return Some((w, h, samples, v));
    }
    let mut out = vec![0f32; v.len()];
    for s in 0..samples {
        for i in 0..n {
            out[i * samples + s] = v[s * n + i];
        }
    }
    Some((w, h, samples, out))
}

/// Split a multi-page TIFF (Epson Scan 2's "Multi-TIFF": one page per frame of the holder) into one 16-bit TIFF
/// per page next to it, `name-1.tif`, `name-2.tif`…, keeping the colour profile and the infrared channel. The
/// original stays. Returns the new files.
pub fn split_pages(path: &Path) -> Result<Vec<PathBuf>, String> {
    let info = info(path).ok_or("not a TIFF")?;
    if info.pages < 2 {
        return Err("this TIFF has only one page".into());
    }
    let icc = open(path).and_then(|mut d| d.get_tag_u8_vec(Tag::IccProfile).ok());
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("scan");
    let mut out = vec![];
    for page in 0..info.pages {
        let (w, h, samples, data) = read_samples(path, page).ok_or(format!("couldn't read page {}", page + 1))?;
        let dest = path.with_file_name(format!("{stem}-{}.tif", page + 1));
        let px: Vec<u16> = data.iter().map(|&v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect();
        write_tiff(&dest, w, h, samples, 16, &px, icc.as_deref(), &info).map_err(|e| format!("{}: {e}", dest.display()))?;
        out.push(dest);
    }
    Ok(out)
}

/// A TIFF of 1–4 samples (grey, grey+IR, RGB, RGB+IR) at 8 or 16 bits, with the scan's profile, tags and
/// resolution. `px` holds 16-bit values either way.
#[allow(clippy::too_many_arguments)]
pub fn write_tiff(dest: &Path, w: u32, h: u32, samples: usize, bits: u16, px: &[u16], icc: Option<&[u8]>, info: &Info) -> Result<(), String> {
    use tiff::encoder::{Rational, TiffEncoder, colortype};
    let f = std::io::BufWriter::new(std::fs::File::create(dest).map_err(|e| e.to_string())?);
    let mut enc = TiffEncoder::new(f).map_err(|e| e.to_string())?;
    let px8: Vec<u8> = if bits == 8 { px.iter().map(|&v| (v >> 8) as u8).collect() } else { vec![] };
    macro_rules! write {
        ($ct:ty, $extra:expr, $data:expr) => {{
            let mut img = enc.new_image::<$ct>(w, h).map_err(|e| e.to_string())?;
            if info.dpi > 0.0 {
                img.resolution(tiff::tags::ResolutionUnit::Inch, Rational { n: (info.dpi * 100.0).round() as u32, d: 100 });
            }
            let d = img.encoder();
            if let Some(icc) = icc {
                d.write_tag(Tag::IccProfile, icc).map_err(|e| e.to_string())?;
            }
            for (tag, s) in [(Tag::Software, &info.software), (Tag::Make, &info.make), (Tag::Model, &info.model)] {
                if !s.is_empty() {
                    d.write_tag(tag, s.as_str()).map_err(|e| e.to_string())?;
                }
            }
            if $extra {
                // Unspecified, as the scanner software wrote it: infrared, not transparency.
                d.write_tag(Tag::ExtraSamples, &[0u16][..]).map_err(|e| e.to_string())?;
            }
            img.write_data($data).map_err(|e| e.to_string())
        }};
    }
    match (samples, bits == 8) {
        (1, true) => write!(colortype::Gray8, false, &px8),
        (3, true) => write!(colortype::RGB8, false, &px8),
        (1, false) => write!(colortype::Gray16, false, px),
        (2, _) => write!(GrayIr16, true, px),
        (3, false) => write!(colortype::RGB16, false, px),
        (4, _) => write!(colortype::RGBA16, true, px),
        (n, _) => Err(format!("{n} samples per pixel")),
    }
}

/// Grey plus an infrared sample, 16 bits each.
struct GrayIr16;
impl tiff::encoder::colortype::ColorType for GrayIr16 {
    type Inner = u16;
    const TIFF_VALUE: tiff::tags::PhotometricInterpretation = tiff::tags::PhotometricInterpretation::BlackIsZero;
    const BITS_PER_SAMPLE: &'static [u16] = &[16, 16];
    const SAMPLE_FORMAT: &'static [tiff::tags::SampleFormat] = &[tiff::tags::SampleFormat::Uint, tiff::tags::SampleFormat::Uint];
    // Only used with a predictor, which we never ask for.
    fn horizontal_predict(row: &[u16], result: &mut Vec<u16>) {
        result.extend_from_slice(row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ferro-scanfile-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    /// A 64-bit RGBI TIFF as VueScan writes it: four 16-bit samples, the fourth marked "unspecified".
    fn vuescan_rgbi(path: &Path, w: u32, h: u32) {
        let px: Vec<u16> = (0..w * h).flat_map(|i| [6554, 13107, 19661, if i % w == 5 { 3277 } else { 52428 }]).collect();
        let info = Info { software: "VueScan 9.8.30".into(), dpi: 3200.0, ..Default::default() };
        write_tiff(path, w, h, 4, 16, &px, None, &info).unwrap();
    }

    #[test]
    fn unspecified_infrared_is_read_not_dropped() {
        let p = tmp("vuescan.tif");
        vuescan_rgbi(&p, 16, 8);
        let i = info(&p).unwrap();
        assert_eq!((i.colours, i.extra, i.bits, i.icc, i.dpi), (3, 1, 16, false, 3200.0));
        assert!(i.looks_linear(), "untagged VueScan raw is linear");
        let (w, h, s, d) = read_samples(&p, 0).unwrap();
        assert_eq!((w, h, s), (16, 8, 4));
        assert!((d[3] - 0.8).abs() < 1e-3 && (d[5 * 4 + 3] - 0.05).abs() < 1e-3);
        let ir = crate::scan::ir_channel(&p).expect("infrared found");
        assert!((ir.data[5] - 0.05).abs() < 1e-3);
    }

    #[test]
    fn epson_and_tagged_scans_are_not_linear() {
        let base = Info { bits: 16, colours: 3, ..Default::default() };
        assert!(!Info { software: "EPSON Scan 2".into(), ..base.clone() }.looks_linear());
        assert!(!Info { software: "VueScan 9".into(), icc: true, ..base.clone() }.looks_linear());
        assert!(!Info { software: "SilverFast 9".into(), bits: 8, ..base.clone() }.looks_linear());
        assert!(Info { software: "SilverFast 9.2".into(), extra: 1, ..base.clone() }.looks_linear());
        assert_eq!(Info { make: "EPSON".into(), model: "Perfection V850".into(), ..base.clone() }.scanner(), "EPSON Perfection V850");
        assert_eq!(Info { make: "EPSON".into(), model: "EPSON Perfection V600".into(), ..base }.scanner(), "EPSON Perfection V600");
    }

    #[test]
    fn multi_page_tiff_splits_into_frames() {
        let p = tmp("multi.tif");
        {
            use tiff::encoder::{TiffEncoder, colortype};
            let mut enc = TiffEncoder::new(std::fs::File::create(&p).unwrap()).unwrap();
            for k in 0..3u16 {
                let px = vec![1000 * (k + 1); 4 * 2 * 3];
                let mut img = enc.new_image::<colortype::RGB16>(4, 2).unwrap();
                img.encoder().write_tag(Tag::Software, "EPSON Scan 2").unwrap();
                img.write_data(&px).unwrap();
            }
        }
        assert_eq!(info(&p).unwrap().pages, 3);
        let files = split_pages(&p).unwrap();
        assert_eq!(files.len(), 3);
        let (_, _, s, d) = read_samples(&files[2], 0).unwrap();
        assert_eq!(s, 3);
        assert!((d[0] - 3000.0 / 65535.0).abs() < 1e-4);
        assert_eq!(info(&files[2]).unwrap().software, "EPSON Scan 2");
    }
}

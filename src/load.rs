//! Decoding: RAW via rawler, everything else via the `image` crate. Output is linear sRGB-primaries f32.
use image::{DynamicImage, ImageDecoder, ImageReader, Rgb32FImage, imageops::FilterType, metadata::Orientation};
use rawler::{decoders::RawDecodeParams, imgop::develop::{ProcessingStep, RawDevelop}, rawsource::RawSource};
use std::path::Path;

pub const OTHER_EXT: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp", "exr", "hdr", "tga", "qoi", "pnm", "jxl", "heic", "heif", "hif", "avif"];

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub fn is_raw(path: &Path) -> bool {
    let e = ext(path);
    // tif/dng are both TIFF containers: DNG goes to rawler, plain TIFF scans to `image`.
    e != "tif" && e != "tiff" && rawler::decoders::supported_extensions().iter().any(|s| s.eq_ignore_ascii_case(&e))
}

pub fn is_supported(path: &Path) -> bool {
    is_raw(path) || OTHER_EXT.contains(&ext(path).as_str())
}

/// A virtual copy is `IMG_0001.NEF.copy2`: its own sidecar, the original's pixels. Returns the original.
pub fn copy_of(path: &Path) -> Option<std::path::PathBuf> {
    let name = path.file_name()?.to_str()?;
    let (orig, n) = name.rsplit_once(".copy")?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then(|| path.with_file_name(orig))
}

/// Full-quality decode, linear light.
pub fn load(path: &Path) -> Result<Rgb32FImage, String> {
    let real = copy_of(path);
    let path = real.as_deref().unwrap_or(path);
    if is_raw(path) { load_raw(path) } else { load_other(path, true) }
}

fn load_raw(path: &Path) -> Result<Rgb32FImage, String> {
    let src = RawSource::new(path).map_err(|e| e.to_string())?;
    let dec = rawler::get_decoder(&src).map_err(|e| e.to_string())?;
    let raw = dec.raw_image(&src, &RawDecodeParams::default(), false).map_err(|e| e.to_string())?;
    let orient = Orientation::from_exif(raw.orientation.to_u16() as u8).unwrap_or(Orientation::NoTransforms);
    // Default steps minus the sRGB gamma: we want linear data.
    let steps: Vec<_> = RawDevelop::default().steps.into_iter().filter(|s| *s != ProcessingStep::SRgb).collect();
    let img = RawDevelop::new_with(&steps).develop_intermediate(&raw).map_err(|e| e.to_string())?;
    let mut img = img.to_dynamic_image().ok_or("unsupported raw layout")?;
    img.apply_orientation(orient);
    Ok(img.to_rgb32f())
}

fn load_other(path: &Path, linearize: bool) -> Result<Rgb32FImage, String> {
    if crate::heif::EXT.contains(&ext(path).as_str()) {
        return crate::heif::load(path).map(|r| r.0);
    }
    jxl_hook();
    let mut dec = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?
        .into_decoder().map_err(|e| e.to_string())?;
    let orient = dec.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(orient);
    let hdr = matches!(img, DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_));
    let mut out = img.to_rgb32f();
    if linearize && !hdr {
        out.iter_mut().for_each(|v| *v = srgb_to_linear(*v));
    }
    Ok(out)
}

/// JPEG XL through jxl-oxide, registered with the `image` crate once.
fn jxl_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        jxl_oxide::integration::register_image_decoding_hook();
    });
}

/// The EXIF block (TIFF structure) of a non-RAW file, for lens detection.
pub fn exif_block(path: &Path) -> Option<Vec<u8>> {
    if crate::heif::EXT.contains(&ext(path).as_str()) {
        return crate::heif::load(path).ok()?.1;
    }
    jxl_hook();
    let mut dec = ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    dec.exif_metadata().ok()?
}

/// Fast small decode for thumbnails: the embedded JPEG preview for RAW files when there is one.
pub fn load_quick(path: &Path, size: u32) -> Result<Rgb32FImage, String> {
    let real = copy_of(path);
    let path = real.as_deref().unwrap_or(path);
    if is_raw(path) {
        let embedded = RawSource::new(path).ok().and_then(|src| {
            let dec = rawler::get_decoder(&src).ok()?;
            let p = RawDecodeParams::default();
            dec.preview_image(&src, &p).ok().flatten().or_else(|| dec.thumbnail_image(&src, &p).ok().flatten())
        });
        if let Some(img) = embedded.filter(|i| i.width() >= size.min(160)) {
            let mut out = img.thumbnail(size, size).to_rgb32f();
            out.iter_mut().for_each(|v| *v = srgb_to_linear(*v));
            return Ok(out);
        }
    }
    Ok(fit(&load(path)?, size))
}

/// Downscale so the long side is at most `size`.
pub fn fit(img: &Rgb32FImage, size: u32) -> Rgb32FImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= size {
        return img.clone();
    }
    let s = size as f32 / w.max(h) as f32;
    image::imageops::resize(img, ((w as f32 * s) as u32).max(1), ((h as f32 * s) as u32).max(1), FilterType::Triangle)
}

pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// sRGB encode through a 4096-step table (linear in between): a powf per channel was a big share of a render.
pub fn linear_to_srgb(v: f32) -> f32 {
    static T: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| (0..=4096).map(|i| srgb_exact(i as f32 / 4096.0)).collect());
    let x = v.clamp(0.0, 1.0) * 4096.0;
    let i = (x as usize).min(4095);
    t[i] + (t[i + 1] - t[i]) * (x - i as f32)
}

fn srgb_exact(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// What the sensor actually recorded, before any rendering: per-channel histogram (128 bins, gamma-spaced) and
/// the share of photosites clipped at the white level. Only for RAW files; no demosaic, so it's fast.
pub struct RawStats {
    pub bins: [[f32; 128]; 3],
    /// Percent of red, green and blue photosites at or above the white level.
    pub clipped: [f32; 3],
}

pub fn raw_stats(path: &Path) -> Option<RawStats> {
    use rawler::{cfa::CFAColor, rawimage::{RawImageData, RawPhotometricInterpretation}};
    let real = copy_of(path);
    let path = real.as_deref().unwrap_or(path);
    if !is_raw(path) {
        return None;
    }
    let src = RawSource::new(path).ok()?;
    let raw = rawler::get_decoder(&src).ok()?.raw_image(&src, &RawDecodeParams::default(), false).ok()?;
    let RawPhotometricInterpretation::Cfa(cfg) = &raw.photometric else { return None };
    let RawImageData::Integer(data) = &raw.data else { return None };
    let white = *raw.whitelevel.0.first()? as f32;
    let black = raw.blacklevel.levels.first().map(|r| r.as_f32()).unwrap_or(0.0);
    let (w, h) = (raw.width, raw.height);
    let mut bins = [[0f32; 128]; 3];
    let (mut clip, mut count) = ([0u64; 3], [0u64; 3]);
    // Every second row and column of each 2×2 block is plenty for statistics.
    for y in (0..h).step_by(3) {
        for x in (0..w).step_by(3) {
            let c = match cfg.cfa.cfa_color_at(y, x) {
                CFAColor::RED => 0,
                CFAColor::GREEN | CFAColor::FUJI_GREEN => 1,
                CFAColor::BLUE => 2,
                _ => continue,
            };
            let v = data[y * w + x] as f32;
            count[c] += 1;
            if v >= white * 0.997 {
                clip[c] += 1;
            }
            let n = ((v - black) / (white - black)).clamp(0.0, 1.0);
            bins[c][((n.powf(1.0 / 2.2)) * 127.0) as usize] += 1.0;
        }
    }
    for b in &mut bins {
        let m = b.iter().cloned().fold(1.0, f32::max);
        b.iter_mut().for_each(|v| *v /= m);
    }
    Some(RawStats { bins, clipped: [0, 1, 2].map(|c| clip[c] as f32 / count[c].max(1) as f32 * 100.0) })
}

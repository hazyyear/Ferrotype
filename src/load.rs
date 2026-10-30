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

/// `is_raw` for a photo or a virtual copy of one (`IMG_0001.NEF.copy2`).
pub fn is_raw_photo(path: &Path) -> bool {
    is_raw(copy_of(path).as_deref().unwrap_or(path))
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

/// Sort photos by name, each one's virtual copies right after it in number order (`copy2` … `copy10`; a plain
/// sort put `copy10` before `copy2`, which scrambled strips of ten frames or more).
pub fn sort_photos(files: &mut [std::path::PathBuf]) {
    files.sort_by_cached_key(|p| match copy_of(p) {
        Some(orig) => {
            let n = p.file_name().and_then(|f| f.to_str()).and_then(|f| f.rsplit_once(".copy")).and_then(|(_, n)| n.parse::<u64>().ok());
            (orig, n.unwrap_or(0))
        }
        None => (p.clone(), 0),
    });
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
    // Our own demosaicing (RCD, X-Trans) and colour where we handle the sensor; rawler's develop otherwise.
    if let Some(img) = crate::demosaic::develop(&raw) {
        let mut img = DynamicImage::ImageRgb32F(img);
        img.apply_orientation(orient);
        return Ok(img.to_rgb32f());
    }
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
    let mut r = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    // The default 512 MB cap refuses our own panoramas and HDR merges.
    r.no_limits();
    let mut dec = r.into_decoder().map_err(|e| e.to_string())?;
    let orient = dec.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| e.to_string())?;
    img.apply_orientation(orient);
    let hdr = matches!(img, DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_));
    let eight_bit = matches!(img, DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_) | DynamicImage::ImageLuma8(_) | DynamicImage::ImageLumaA8(_));
    let mut out = img.to_rgb32f();
    if linearize && !hdr {
        linearize_srgb(&mut out, eight_bit);
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
        return crate::heif::exif(path);
    }
    jxl_hook();
    let mut dec = ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    dec.exif_metadata().ok()?
}

/// The JPEG a camera stores inside a RAW file (its own rendering), or a smaller thumbnail.
fn embedded_preview(path: &Path) -> Option<DynamicImage> {
    let src = RawSource::new(path).ok()?;
    let dec = rawler::get_decoder(&src).ok()?;
    let p = RawDecodeParams::default();
    dec.preview_image(&src, &p).ok().flatten().or_else(|| dec.thumbnail_image(&src, &p).ok().flatten())
}

/// Fast small decode for thumbnails: the embedded JPEG preview for RAW files when there is one.
pub fn load_quick(path: &Path, size: u32) -> Result<Rgb32FImage, String> {
    load_quick_toned(path, size).map(|r| r.0)
}

/// `load_quick`, and whether the picture is the camera's own JPEG (already tone-mapped, so a RAW's camera curve
/// must not be applied to it again).
pub fn load_quick_toned(path: &Path, size: u32) -> Result<(Rgb32FImage, bool), String> {
    let real = copy_of(path);
    let path = real.as_deref().unwrap_or(path);
    if is_raw(path)
        && let Some(img) = embedded_preview(path).filter(|i| i.width() >= size.min(160))
    {
        let mut out = img.thumbnail(size, size).to_rgb32f();
        linearize_srgb(&mut out, false);
        return Ok((out, true));
    }
    Ok((fit(&load(path)?, size), false))
}

/// The camera's own tone curve for a RAW, matched from the JPEG it embedded: pairs of (linear RAW brightness,
/// linear JPEG brightness) at the same quantiles, as RawTherapee's auto-matched curve does. With it an unedited
/// RAW looks like the camera's picture instead of flat linear data. Empty without a preview of 640 px or more.
pub fn camera_curve(path: &Path, linear: &Rgb32FImage) -> Vec<[f32; 2]> {
    let real = copy_of(path);
    let path = real.as_deref().unwrap_or(path);
    let Some(prev) = embedded_preview(path).filter(|i| i.width().max(i.height()) >= 640) else { return vec![] };
    let mut pv = prev.thumbnail(1024, 1024).to_rgb32f();
    linearize_srgb(&mut pv, false);
    match_curve(lumas(linear), lumas(&pv))
}

fn lumas(img: &Rgb32FImage) -> Vec<f32> {
    let step = (img.len() / 3 / 300_000).max(1);
    img.chunks(3).step_by(step).map(|c| crate::develop::luma([c[0], c[1], c[2]])).collect()
}

/// Quantile matching: x rises strictly, y never falls, and the curve stops where the JPEG clips (brighter RAW
/// values keep the last gain, so the Highlights slider can still bring them back).
pub(crate) fn match_curve(mut raw: Vec<f32>, mut jpg: Vec<f32>) -> Vec<[f32; 2]> {
    if raw.len() < 1000 || jpg.len() < 1000 {
        return vec![];
    }
    raw.sort_unstable_by(f32::total_cmp);
    jpg.sort_unstable_by(f32::total_cmp);
    let q = |v: &[f32], t: f32| v[((v.len() - 1) as f32 * t) as usize];
    let mut out: Vec<[f32; 2]> = vec![];
    for i in 0..=64 {
        let t = 0.002 + 0.996 * i as f32 / 64.0;
        let (x, y) = (q(&raw, t), q(&jpg, t));
        // Gains outside 1/16–16 mean the preview isn't this picture (a placeholder, another crop): skip the point.
        if x <= 1e-6 || y <= 0.0 || !(1.0 / 16.0..=16.0).contains(&(y / x)) || out.last().is_some_and(|l| x <= l[0] * 1.001 || y < l[1]) {
            continue;
        }
        out.push([x, y]);
        if y >= 0.98 {
            break;
        }
    }
    if out.len() < 8 { vec![] } else { out }
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

/// sRGB-encoded to linear, in place and on every core. 8-bit sources only have 256 values, so they use a table
/// (a powf per channel was most of the time it took to open a big JPEG).
fn linearize_srgb(img: &mut Rgb32FImage, eight_bit: bool) {
    use rayon::prelude::*;
    if eight_bit {
        let t: [f32; 256] = std::array::from_fn(|i| srgb_to_linear(i as f32 / 255.0));
        img.par_iter_mut().for_each(|v| *v = t[(*v * 255.0).round().clamp(0.0, 255.0) as usize]);
    } else {
        img.par_iter_mut().for_each(|v| *v = srgb_to_linear(*v));
    }
}

pub fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// sRGB encode through a 4096-step table (linear in between): a powf per channel was a big share of a render.
/// Extended below 0 (mirrored), so a colour outside sRGB keeps its negative channel for a wide-gamut export.
pub fn linear_to_srgb(v: f32) -> f32 {
    static T: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    let t = T.get_or_init(|| (0..=4096).map(|i| srgb_exact(i as f32 / 4096.0)).collect());
    let x = v.abs().min(1.0) * 4096.0;
    let i = (x as usize).min(4095);
    let e = t[i] + (t[i + 1] - t[i]) * (x - i as f32);
    if v < 0.0 { -e } else { e }
}

/// The inverse of `linear_to_srgb`, extended the same way.
pub fn srgb_to_linear_ext(v: f32) -> f32 {
    if v < 0.0 { -srgb_to_linear(-v) } else { srgb_to_linear(v) }
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
    // Bin of each level (gamma 2.2, 128 bins), looked up instead of a powf per sample.
    let bin_of: [u8; 4096] = std::array::from_fn(|i| ((i as f32 / 4095.0).powf(1.0 / 2.2) * 127.0) as u8);
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
            bins[c][bin_of[(n * 4095.0) as usize] as usize] += 1.0;
        }
    }
    for b in &mut bins {
        let m = b.iter().cloned().fold(1.0, f32::max);
        b.iter_mut().for_each(|v| *v /= m);
    }
    Some(RawStats { bins, clipped: [0, 1, 2].map(|c| clip[c] as f32 / count[c].max(1) as f32 * 100.0) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_sort_in_number_order_after_their_photo() {
        let mut v: Vec<std::path::PathBuf> = ["b.tif", "a.tif.copy10", "a.tif.copy2", "a.tif", "a.tif.copy3"].iter().map(Into::into).collect();
        sort_photos(&mut v);
        let names: Vec<_> = v.iter().map(|p| p.to_string_lossy().to_string()).collect();
        assert_eq!(names, ["a.tif", "a.tif.copy2", "a.tif.copy3", "a.tif.copy10", "b.tif"]);
    }

    #[test]
    fn matched_curve_follows_the_jpeg() {
        // The "JPEG" is the RAW through a gamma-like curve: the match recovers it and stops where the JPEG clips.
        let raw: Vec<f32> = (0..20_000).map(|i| i as f32 / 20_000.0 * 0.5).collect();
        let jpg: Vec<f32> = raw.iter().map(|x| (x * 2.0).powf(0.7).min(1.0)).collect();
        let c = super::match_curve(raw, jpg);
        assert!(c.len() > 30, "{}", c.len());
        assert!(c.windows(2).all(|w| w[1][0] > w[0][0] && w[1][1] >= w[0][1]));
        for q in &c {
            assert!((q[1] - (q[0] * 2.0).powf(0.7).min(1.0)).abs() < 0.01, "{q:?}");
        }
        assert!(super::match_curve(vec![0.1; 10], vec![0.2; 10]).is_empty());
        // A virtual copy of a RAW is a RAW too (it gets the same camera curve).
        assert!(super::is_raw_photo(std::path::Path::new("IMG_1.NEF.copy2")) && !super::is_raw(std::path::Path::new("IMG_1.NEF.copy2")));
    }
}

//! HDR export as a gain-map JPEG (Ultra HDR / Adobe gain map 1.0): a normal JPEG that every viewer shows,
//! plus a small grey "gain map" that HDR screens use to brighten the highlights beyond SDR white.
use crate::develop::{luma, smooth};
use crate::output::{insert_at, insert_segment, segment};
use crate::load::srgb_to_linear;
use image::{GrayImage, Rgb32FImage, codecs::jpeg::JpegEncoder};

/// How far above SDR white the brightest highlights may go, in stops (2 = 4× brighter).
pub const HEADROOM: f32 = 2.0;
const OFFSET: f32 = 1.0 / 64.0;

/// The gain map for an SDR render: how much brighter each area should be on an HDR screen.
/// `low` is the same photo rendered `HEADROOM` stops darker, which still holds detail where the SDR one clips.
pub fn gain_map(sdr: &Rgb32FImage, low: &Rgb32FImage) -> GrayImage {
    let (w, h) = sdr.dimensions();
    let (gw, gh) = (w.div_ceil(4), h.div_ceil(4));
    let boost = 2f32.powf(HEADROOM);
    // One gain-map row per task: two sRGB decodes and a log per pixel made this the slow half of an HDR export.
    use rayon::prelude::*;
    let mut map = GrayImage::new(gw, gh);
    map.par_chunks_mut(gw as usize).enumerate().for_each(|(gy, row)| {
        let gy = gy as u32;
        for (gx, out) in row.iter_mut().enumerate() {
            let gx = gx as u32;
            // Average a 4×4 block of log gains.
            let (mut acc, mut n) = (0.0, 0.0);
            for y in gy * 4..(gy * 4 + 4).min(h) {
                for x in gx * 4..(gx * 4 + 4).min(w) {
                    let s = sdr.get_pixel(x, y).0;
                    let ys = luma(s.map(srgb_to_linear));
                    let yl = luma(low.get_pixel(x, y).0.map(srgb_to_linear)) * boost;
                    // Only the highlights get brighter; the rest of the picture stays exactly as in SDR.
                    let wgt = smooth(0.55, 0.95, luma(s));
                    let yh = ys + wgt * (yl.max(ys) - ys);
                    acc += ((yh + OFFSET) / (ys + OFFSET)).log2().clamp(0.0, HEADROOM);
                    n += 1.0;
                }
            }
            *out = ((acc / n / HEADROOM) * 255.0).round() as u8;
        }
    });
    map
}

fn xmp(body: &str) -> Vec<u8> {
    let mut b = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    b.extend_from_slice(body.as_bytes());
    segment(0xE1, &b)
}

/// Multi-Picture Format index (CIPA DC-007): where the second image starts, relative to this TIFF header.
fn mpf(primary_len: u32, map_len: u32, map_offset: u32) -> Vec<u8> {
    let mut b = b"MPF\0".to_vec();
    b.extend_from_slice(b"II*\0");
    b.extend_from_slice(&8u32.to_le_bytes()); // first IFD
    b.extend_from_slice(&3u16.to_le_bytes());
    let entry = |b: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: [u8; 4]| {
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&typ.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&value);
    };
    entry(&mut b, 0xB000, 7, 4, *b"0100");
    entry(&mut b, 0xB001, 4, 1, 2u32.to_le_bytes());
    entry(&mut b, 0xB002, 7, 32, (8 + 2 + 3 * 12 + 4u32).to_le_bytes()); // entries follow the IFD
    b.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    for (attr, size, off) in [(0x2003_0000u32, primary_len, 0u32), (0, map_len, map_offset)] {
        b.extend_from_slice(&attr.to_le_bytes());
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&off.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
    }
    segment(0xE2, &b)
}

fn encode(img: image::DynamicImage, quality: u8, dpi: Option<u16>) -> Result<Vec<u8>, String> {
    let mut v = vec![];
    let mut enc = JpegEncoder::new_with_quality(&mut v, quality);
    if let Some(d) = dpi {
        enc.set_pixel_density(image::codecs::jpeg::PixelDensity::dpi(d));
    }
    img.write_with_encoder(enc).map_err(|e| e.to_string())?;
    Ok(v)
}

/// The whole file: primary JPEG (with container XMP and MPF index) followed by the gain-map JPEG.
/// `exif` (a whole APP1 segment, or empty) and `dpi` go on the primary image, as on a normal print.
pub fn encode_ultrahdr(sdr: &Rgb32FImage, map: &GrayImage, quality: u8, exif: &[u8], dpi: Option<u16>) -> Result<Vec<u8>, String> {
        let map_xmp = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" hdrgm:Version="1.0" hdrgm:GainMapMin="0" hdrgm:GainMapMax="{HEADROOM}" hdrgm:Gamma="1" hdrgm:OffsetSDR="{OFFSET}" hdrgm:OffsetHDR="{OFFSET}" hdrgm:HDRCapacityMin="0" hdrgm:HDRCapacityMax="{HEADROOM}" hdrgm:BaseRenditionIsHDR="False"/></rdf:RDF></x:xmpmeta>"#
    );
    let map_jpeg = insert_segment(&encode(image::DynamicImage::ImageLuma8(map.clone()), 90, None)?, &xmp(&map_xmp));
    let primary_xmp = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:Container="http://ns.google.com/photos/1.0/container/" xmlns:Item="http://ns.google.com/photos/1.0/container/item/" xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" hdrgm:Version="1.0"><Container:Directory><rdf:Seq><rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="Primary" Item:Mime="image/jpeg"/></rdf:li><rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="GainMap" Item:Mime="image/jpeg" Item:Length="{}"/></rdf:li></rdf:Seq></Container:Directory></rdf:Description></rdf:RDF></x:xmpmeta>"#,
        map_jpeg.len()
    );
    let base = encode(image::DynamicImage::ImageRgb8(image::DynamicImage::ImageRgb32F(sdr.clone()).to_rgb8()), quality, dpi)?;
    let xmp_seg = xmp(&primary_xmp);
    // The MPF segment has a fixed size, so the offsets are known before it is written.
    let primary_len = (base.len() + exif.len() + xmp_seg.len() + mpf(0, 0, 0).len()) as u32;
    let tiff_at = (insert_at(&base) + exif.len() + xmp_seg.len() + 4 + 4) as u32; // after FF E2, length and "MPF\0"
    let primary = insert_segment(&base, &[exif, &xmp_seg, &mpf(primary_len, map_jpeg.len() as u32, primary_len - tiff_at)].concat());
    Ok([primary, map_jpeg].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_map_brightens_only_highlights_and_the_file_is_well_formed() {
        // Left half mid grey, right half a bright area that clips in SDR but not in the darker render.
        let sdr = Rgb32FImage::from_fn(64, 32, |x, _| image::Rgb(if x < 32 { [0.45; 3] } else { [1.0; 3] }));
        let low = Rgb32FImage::from_fn(64, 32, |x, _| image::Rgb(if x < 32 { [0.2; 3] } else { [0.9; 3] }));
        let map = gain_map(&sdr, &low);
        assert!(map.get_pixel(2, 4).0[0] < 10, "midtones untouched");
        assert!(map.get_pixel(14, 4).0[0] > 150, "highlights boosted");
        // Plain, and with camera details and a print DPI (which shift every offset after them).
        let shot = crate::lens::Shot { make: "Nikon".into(), model: "Z 6".into(), ..Default::default() };
        let exif = crate::output::exif_segment(&shot, "Roll 12");
        for (exif, dpi) in [(&[][..], None), (&exif[..], Some(300))] {
        let f = encode_ultrahdr(&sdr, &map, 90, exif, dpi).unwrap();
        assert_eq!(&f[..2], &[0xFF, 0xD8]);
        // The MPF entry for the gain map points at the second SOI.
        let mpf_at = f.windows(4).position(|w| w == b"MPF\0").unwrap() + 4;
        let e2 = mpf_at + 8 + 2 + 36 + 4 + 16;
        let size = u32::from_le_bytes(f[e2 + 4..e2 + 8].try_into().unwrap()) as usize;
        let off = u32::from_le_bytes(f[e2 + 8..e2 + 12].try_into().unwrap()) as usize;
        assert_eq!(&f[mpf_at + off..mpf_at + off + 2], &[0xFF, 0xD8]);
        assert_eq!(mpf_at + off + size, f.len());
        let primary_size = u32::from_le_bytes(f[mpf_at + 8 + 2 + 36 + 4 + 4..][..4].try_into().unwrap()) as usize;
        assert_eq!(primary_size, mpf_at + off);
        assert!(String::from_utf8_lossy(&f).contains(&format!("Item:Length=\"{size}\"")));
        if dpi.is_some() {
            assert!(f.windows(4).any(|w| w == b"Z 6\0"), "camera model kept");
            // JFIF APP0: units 1 (dots per inch), then X density 300.
            assert_eq!((f[13], u16::from_be_bytes([f[14], f[15]])), (1, 300));
        }
        }
    }
}

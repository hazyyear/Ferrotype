//! HEIC/HEIF and AVIF through the system's libheif, loaded when first needed, so the app has no hard dependency on it.
//! Linux: the libheif1 package. macOS: `brew install libheif`. Windows: libheif.dll next to ferrotype.exe or on PATH.
use image::Rgb32FImage;
use libloading::{Library, Symbol};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;
use std::sync::OnceLock;

pub const EXT: &[&str] = &["heic", "heif", "hif", "avif"];

#[repr(C)]
struct HeifError {
    code: c_int,
    subcode: c_int,
    message: *const c_char,
}

const COLORSPACE_RGB: c_int = 1;
const CHROMA_RGB: c_int = 10;
const CHROMA_RRGGBB_LE: c_int = 14;
const CHANNEL_INTERLEAVED: c_int = 10;

fn lib() -> Result<&'static Library, String> {
    static LIB: OnceLock<Result<Library, String>> = OnceLock::new();
    LIB.get_or_init(|| {
        let names: &[&str] = if cfg!(windows) {
            &["libheif.dll", "heif.dll"]
        } else if cfg!(target_os = "macos") {
            &["libheif.1.dylib", "/opt/homebrew/lib/libheif.1.dylib", "/usr/local/lib/libheif.1.dylib"]
        } else {
            &["libheif.so.1", "libheif.so"]
        };
        for n in names {
            // SAFETY: loading libheif runs only its own initialisers.
            if let Ok(l) = unsafe { Library::new(n) } {
                // heif_init loads the decoder plugins (libheif 1.14+); older versions have no such call and don't need it.
                unsafe {
                    if let Ok(init) = l.get::<unsafe extern "C" fn(*const c_void) -> HeifError>(b"heif_init\0") {
                        init(std::ptr::null());
                    }
                }
                return Ok(l);
            }
        }
        Err(if cfg!(target_os = "macos") { "HEIC and AVIF need libheif: brew install libheif" } else if cfg!(windows) { "HEIC and AVIF need libheif.dll (from vcpkg or a libheif release) next to ferrotype.exe" } else { "HEIC and AVIF need libheif: install the libheif1 package" }.into())
    }).as_ref().map_err(|e| e.clone())
}

fn check(e: HeifError) -> Result<(), String> {
    if e.code == 0 {
        Ok(())
    } else {
        // SAFETY: libheif's error messages are static, always non-null C strings.
        Err(unsafe { CStr::from_ptr(e.message) }.to_string_lossy().into_owned())
    }
}

macro_rules! sym {
    ($l:expr, $name:literal, $t:ty) => {{
        let s: Symbol<$t> = unsafe { $l.get(concat!($name, "\0").as_bytes()) }.map_err(|e| e.to_string())?;
        s
    }};
}

/// The primary image, decoded to linear light (with libheif's rotation and mirroring applied), and its EXIF block.
pub fn load(path: &Path) -> Result<(Rgb32FImage, Option<Vec<u8>>), String> {
    let l = lib()?;
    let alloc = sym!(l, "heif_context_alloc", unsafe extern "C" fn() -> *mut c_void);
    let free = sym!(l, "heif_context_free", unsafe extern "C" fn(*mut c_void));
    let read = sym!(l, "heif_context_read_from_file", unsafe extern "C" fn(*mut c_void, *const c_char, *const c_void) -> HeifError);
    let primary = sym!(l, "heif_context_get_primary_image_handle", unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> HeifError);
    let release_handle = sym!(l, "heif_image_handle_release", unsafe extern "C" fn(*mut c_void));
    let luma_bits = sym!(l, "heif_image_handle_get_luma_bits_per_pixel", unsafe extern "C" fn(*mut c_void) -> c_int);
    let decode = sym!(l, "heif_decode_image", unsafe extern "C" fn(*mut c_void, *mut *mut c_void, c_int, c_int, *const c_void) -> HeifError);
    let release_image = sym!(l, "heif_image_release", unsafe extern "C" fn(*mut c_void));
    let width = sym!(l, "heif_image_get_width", unsafe extern "C" fn(*mut c_void, c_int) -> c_int);
    let height = sym!(l, "heif_image_get_height", unsafe extern "C" fn(*mut c_void, c_int) -> c_int);
    let plane = sym!(l, "heif_image_get_plane_readonly", unsafe extern "C" fn(*mut c_void, c_int, *mut c_int) -> *const u8);
    let range = sym!(l, "heif_image_get_bits_per_pixel_range", unsafe extern "C" fn(*mut c_void, c_int) -> c_int);
    let meta_ids = sym!(l, "heif_image_handle_get_list_of_metadata_block_IDs", unsafe extern "C" fn(*mut c_void, *const c_char, *mut u32, c_int) -> c_int);
    let meta_size = sym!(l, "heif_image_handle_get_metadata_size", unsafe extern "C" fn(*mut c_void, u32) -> usize);
    let meta = sym!(l, "heif_image_handle_get_metadata", unsafe extern "C" fn(*mut c_void, u32, *mut c_void) -> HeifError);

    let name = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: the calls follow libheif's documented ownership: everything allocated here is released below.
    unsafe {
        let ctx = alloc();
        if ctx.is_null() {
            return Err("libheif: out of memory".into());
        }
        let result = (|| {
            check(read(ctx, name.as_ptr(), std::ptr::null()))?;
            let mut h = std::ptr::null_mut();
            check(primary(ctx, &mut h))?;
            let hint = |e: String| if e.contains("Unsupported codec") || e.contains("No decoding plugin") {
                format!("{e}: libheif is missing its decoder for this file (HEIC needs libde265, e.g. the libheif-plugin-libde265 package)")
            } else { e };
            let out = (|| {
                let deep = luma_bits(h) > 8;
                let mut img = std::ptr::null_mut();
                check(decode(h, &mut img, COLORSPACE_RGB, if deep { CHROMA_RRGGBB_LE } else { CHROMA_RGB }, std::ptr::null())).map_err(hint)?;
                let (w, ht) = (width(img, CHANNEL_INTERLEAVED), height(img, CHANNEL_INTERLEAVED));
                let mut stride = 0;
                let p = plane(img, CHANNEL_INTERLEAVED, &mut stride);
                if p.is_null() || w <= 0 || ht <= 0 {
                    release_image(img);
                    return Err("libheif returned no picture".to_string());
                }
                let (w, ht, stride) = (w as usize, ht as usize, stride as usize);
                let max = ((1u32 << range(img, CHANNEL_INTERLEAVED).clamp(8, 16)) - 1) as f32;
                let mut out = Rgb32FImage::new(w as u32, ht as u32);
                for y in 0..ht {
                    let row = std::slice::from_raw_parts(p.add(y * stride), if deep { w * 6 } else { w * 3 });
                    let dst = &mut out.as_mut()[y * w * 3..(y + 1) * w * 3];
                    for (i, d) in dst.iter_mut().enumerate() {
                        let v = if deep { u16::from_le_bytes([row[i * 2], row[i * 2 + 1]]) as f32 } else { row[i] as f32 };
                        *d = crate::load::srgb_to_linear(v / if deep { max } else { 255.0 });
                    }
                }
                release_image(img);
                // EXIF: a 4-byte offset to the TIFF header, then the TIFF structure.
                let exif_type = c"Exif";
                let mut id = 0u32;
                let exif = (meta_ids(h, exif_type.as_ptr(), &mut id, 1) == 1).then(|| {
                    let mut buf = vec![0u8; meta_size(h, id)];
                    check(meta(h, id, buf.as_mut_ptr() as *mut c_void)).ok()?;
                    let off = u32::from_be_bytes(buf.get(..4)?.try_into().ok()?) as usize;
                    buf.get(4 + off..).map(|b| b.to_vec())
                }).flatten();
                Ok((out, exif))
            })();
            release_handle(h);
            out
        })();
        free(ctx);
        result
    }
}

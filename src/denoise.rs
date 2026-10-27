//! AI denoise run by ONNX Runtime: RawNIND UtNet2 (GPL-3.0) for RAW files, which works on linear sensor data,
//! and SCUNet (Apache-2.0) for JPEGs and other already-processed photos. None of it ships with Ferrotype:
//! it is an optional download (about 140 MB) into the app's data folder, so the base app stays small.
use crate::develop::Params;
use crate::load::{linear_to_srgb, srgb_to_linear};
use image::Rgb32FImage;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MODEL_URL: &str = "https://huggingface.co/Heliosoph/scunet-onnx/resolve/6d11417ee2fbcc73783c502a238ac115097754fe/scunet_color_real_psnr.onnx";
const MODEL: &str = "scunet_color_real_psnr.onnx";
const MODEL_DATA: &str = "scunet_color_real_psnr.onnx.data";
const RAWNIND_URL: &str = "https://github.com/Orion-jada/RapidRAW/releases/download/rawnind-v1/rawnind_utnet2_linear_512.onnx";
const RAWNIND: &str = "rawnind_utnet2_linear_512.onnx";
const RAWNIND_SHA: &str = "df957efadcc152c007d5d3b0917bdff9e41c0d4a0efe56584ef30b36393cd181";

/// ONNX Runtime 1.22.1, taken from its official PyPI wheel for this platform: (url, sha256, file inside the wheel).
fn runtime_wheel() -> Option<(&'static str, &'static str, &'static str)> {
    let w = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => (
            "https://files.pythonhosted.org/packages/0a/50/519828a5292a6ccd8d5cd6d2f72c6b36ea528a2ef68eca69647732539ffa/onnxruntime-1.22.1-cp312-cp312-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl",
            "2d39a530aff1ec8d02e365f35e503193991417788641b184f5b1e8c9a6d5ce8d",
            "onnxruntime/capi/libonnxruntime.so.1.22.1",
        ),
        ("linux", "aarch64") => (
            "https://files.pythonhosted.org/packages/29/e5/00b099b4d4f6223b610421080d0eed9327ef9986785c9141819bbba0d396/onnxruntime-1.22.1-cp312-cp312-manylinux_2_27_aarch64.manylinux_2_28_aarch64.whl",
            "984cea2a02fcc5dfea44ade9aca9fe0f7a8a2cd6f77c258fc4388238618f3928",
            "onnxruntime/capi/libonnxruntime.so.1.22.1",
        ),
        ("macos", _) => (
            "https://files.pythonhosted.org/packages/48/70/ca2a4d38a5deccd98caa145581becb20c53684f451e89eb3a39915620066/onnxruntime-1.22.1-cp312-cp312-macosx_13_0_universal2.whl",
            "a938d11c0dc811badf78e435daa3899d9af38abee950d87f3ab7430eb5b3cf5a",
            "onnxruntime/capi/libonnxruntime.1.22.1.dylib",
        ),
        ("windows", "x86_64") => (
            "https://files.pythonhosted.org/packages/5d/54/7139d463bb0a312890c9a5db87d7815d4a8cce9e6f5f28d04f0b55fcb160/onnxruntime-1.22.1-cp312-cp312-win_amd64.whl",
            "6a64291d57ea966a245f749eb970f4fa05a64d26672e05a83fdb5db6b7d62f87",
            "onnxruntime/capi/onnxruntime.dll",
        ),
        _ => return None,
    };
    Some(w)
}

/// Where the optional download lives (`~/.local/share/ferrotype/ai`, `%APPDATA%\ferrotype\ai`, …).
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("FERROTYPE_AI_DIR") {
        return Some(d.into());
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }?;
    Some(base.join("ferrotype").join("ai"))
}

fn runtime_path() -> Option<PathBuf> {
    let (_, _, inner) = runtime_wheel()?;
    Some(dir()?.join(Path::new(inner).file_name()?))
}

pub fn installed() -> bool {
    runtime_path().is_some_and(|p| p.exists()) && dir().is_some_and(|d| [MODEL, MODEL_DATA, RAWNIND].iter().all(|m| d.join(m).exists()))
}

/// Download to `dest` (via a temporary file, so a broken download never looks finished).
fn download(url: &str, dest: &Path, sha256: Option<&str>, progress: &dyn Fn(u64, u64)) -> Result<(), String> {
    // The system's certificates, so downloads also work behind company proxies.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build())
        .max_redirects(0)
        .build()
        .into();
    // Redirects by hand: Hugging Face names the SHA-256 of a large (LFS) file only on its first reply, in
    // `x-linked-etag`. The URL is pinned to a commit, so that hash can't change under us.
    let (mut url, mut linked) = (url.to_string(), None);
    let mut hops = 0;
    let resp = loop {
        let resp = agent.get(&url).call().map_err(|e| format!("download failed: {e}"))?;
        let header = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok()).map(|v| v.trim_matches('"').to_string());
        linked = linked.or_else(|| header("x-linked-etag").filter(|e| e.len() == 64 && e.bytes().all(|b| b.is_ascii_hexdigit())));
        if !resp.status().is_redirection() {
            break resp;
        }
        let to = header("location").ok_or("download failed: redirect without a location")?;
        url = if to.starts_with('/') { format!("{}{to}", url.split('/').take(3).collect::<Vec<_>>().join("/")) } else { to };
        hops += 1;
        if hops > 5 {
            return Err("download failed: too many redirects".into());
        }
    };
    let sha256 = sha256.map(str::to_string).or(linked);
    let total: u64 = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok()).unwrap_or(0);
    let mut body = resp.into_body().into_reader();
    let tmp = dest.with_extension("part");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let (mut buf, mut got, mut hash) = (vec![0u8; 1 << 16], 0u64, Sha256::new());
    loop {
        let n = body.read(&mut buf).map_err(|e| format!("download failed: {e}"))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        hash.update(&buf[..n]);
        got += n as u64;
        progress(got, total);
    }
    drop(f);
    if total > 0 && got != total {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("download was cut off ({got} of {total} bytes)"));
    }
    if let Some(want) = sha256 {
        let have: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if have != want.to_ascii_lowercase() {
            let _ = std::fs::remove_file(&tmp);
            return Err("download was corrupted (checksum mismatch)".into());
        }
    }
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

/// Fetch ONNX Runtime and the model. `progress(fraction, what)`.
pub fn install(progress: &dyn Fn(f32, &str)) -> Result<(), String> {
    let (url, sha, inner) = runtime_wheel().ok_or("AI denoise isn't available for this platform")?;
    let d = dir().ok_or("no data folder")?;
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let lib = runtime_path().unwrap();
    if !lib.exists() {
        let wheel = d.join("onnxruntime.whl");
        download(url, &wheel, Some(sha), &|g, t| progress(0.15 * g as f32 / t.max(1) as f32, "ONNX Runtime"))?;
        let mut z = zip::ZipArchive::new(std::fs::File::open(&wheel).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let mut entry = z.by_name(inner).map_err(|e| e.to_string())?;
        let tmp = lib.with_extension("part");
        std::io::copy(&mut entry, &mut std::fs::File::create(&tmp).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &lib).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&wheel);
    }
    if !d.join(MODEL).exists() {
        download(MODEL_URL, &d.join(MODEL), None, &|g, t| progress(0.15 + 0.01 * g as f32 / t.max(1) as f32, "SCUNet model"))?;
    }
    if !d.join(MODEL_DATA).exists() {
        download(&format!("{MODEL_URL}.data"), &d.join(MODEL_DATA), None, &|g, t| progress(0.16 + 0.64 * g as f32 / t.max(1) as f32, "SCUNet model"))?;
    }
    if !d.join(RAWNIND).exists() {
        download(RAWNIND_URL, &d.join(RAWNIND), Some(RAWNIND_SHA), &|g, t| progress(0.8 + 0.2 * g as f32 / t.max(1) as f32, "RawNIND model"))?;
    }
    Ok(())
}

/// Loaded models, by file name.
static SESSIONS: Mutex<Vec<(&'static str, ort::session::Session)>> = Mutex::new(vec![]);

/// Run `f` with `model` loaded. ONNX Runtime panics when its library can't load, so that is caught here.
fn with_session<T>(model: &'static str, f: impl FnOnce(&mut ort::session::Session) -> Result<T, String>) -> Result<T, String> {
    if !installed() {
        return Err("AI denoise isn't installed".into());
    }
    let mut guard = SESSIONS.lock().map_err(|_| "AI denoise crashed earlier; restart Ferrotype")?;
    if !guard.iter().any(|(m, _)| *m == model) {
        let lib = runtime_path().unwrap();
        let file = dir().unwrap().join(model);
        let s = std::panic::catch_unwind(move || -> Result<ort::session::Session, String> {
            ort::init_from(lib.to_string_lossy()).commit().map_err(|e| e.to_string())?;
            ort::session::Session::builder().and_then(|b| b.commit_from_file(&file)).map_err(|e| e.to_string())
        })
        .map_err(|_| "ONNX Runtime could not be loaded".to_string())??;
        guard.push((model, s));
    }
    f(&mut guard.iter_mut().find(|(m, _)| *m == model).unwrap().1)
}

/// Denoise a display-encoded (sRGB 0–1) image with SCUNet. `progress(done, total)` per tile.
/// Tiles: 224 px kept from a 256 px input (16 px of context on each side), as the model wants multiples of 64.
pub fn denoise_srgb(img: &Rgb32FImage, progress: &dyn Fn(usize, usize)) -> Result<Rgb32FImage, String> {
    tiles(MODEL, img, 224, 16, false, progress)
}

/// Denoise linear RAW data with RawNIND (fixed 512 px tiles: 448 kept, 32 px of context on each side).
pub fn denoise_linear(img: &Rgb32FImage, progress: &dyn Fn(usize, usize)) -> Result<Rgb32FImage, String> {
    tiles(RAWNIND, img, 448, 32, true, progress)
}

/// Run `model` over `img` in tiles of `core` px plus `pad` px of context. `gain_match`: the model's output
/// has an arbitrary scale, so each tile's channels are scaled back to the input's averages.
fn tiles(model: &'static str, img: &Rgb32FImage, core: usize, pad: usize, gain_match: bool, progress: &dyn Fn(usize, usize)) -> Result<Rgb32FImage, String> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let tiles: Vec<(usize, usize)> = (0..h.div_ceil(core)).flat_map(|ty| (0..w.div_ceil(core)).map(move |tx| (tx * core, ty * core))).collect();
    let n = core + 2 * pad;
    let mut out = Rgb32FImage::new(w as u32, h as u32);
    with_session(model, |s| {
        let input_name = s.inputs.first().ok_or("model has no input")?.name.clone();
        for (k, &(x0, y0)) in tiles.iter().enumerate() {
            // Planar 1×3×n×n, edges repeated past the image border.
            let mut t = vec![0f32; 3 * n * n];
            for y in 0..n {
                let sy = (y0 as isize + y as isize - pad as isize).clamp(0, h as isize - 1) as u32;
                for x in 0..n {
                    let sx = (x0 as isize + x as isize - pad as isize).clamp(0, w as isize - 1) as u32;
                    let p = img.get_pixel(sx, sy).0;
                    for c in 0..3 {
                        t[c * n * n + y * n + x] = p[c].clamp(0.0, 1.0);
                    }
                }
            }
            let tensor = ort::value::Tensor::from_array(([1usize, 3, n, n], t.clone())).map_err(|e| e.to_string())?;
            let outputs = s.run(ort::inputs![input_name.as_str() => tensor]).map_err(|e| e.to_string())?;
            let (_, data) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
            let gain: [f32; 3] = std::array::from_fn(|c| {
                if !gain_match {
                    return 1.0;
                }
                let plane = |v: &[f32]| v[c * n * n..(c + 1) * n * n].iter().map(|x| *x as f64).sum::<f64>();
                let (a, b) = (plane(&t), plane(data));
                if a > 1e-6 && b.abs() > 1e-12 { (a / b) as f32 } else { f32::NAN }
            });
            for y in 0..core.min(h - y0) {
                for x in 0..core.min(w - x0) {
                    let i = (y + pad) * n + x + pad;
                    // A blank tile (no gain to measure) passes through unchanged.
                    let v = [0, 1, 2].map(|c| if gain[c].is_finite() { data[c * n * n + i] * gain[c] } else { t[c * n * n + i] });
                    out.put_pixel((x0 + x) as u32, (y0 + y) as u32, image::Rgb(v));
                }
            }
            progress(k + 1, tiles.len());
        }
        Ok(())
    })?;
    Ok(out)
}

/// What denoising takes away from a linear photo: add `amount ×` this to remove the noise.
/// Kept as a difference so highlight detail above 1.0 (which the models can't see) survives.
/// `raw`: the photo is RAW sensor data, so RawNIND works on it directly; otherwise SCUNet, in sRGB.
pub fn noise_delta(img: &Rgb32FImage, raw: bool, progress: &dyn Fn(usize, usize)) -> Result<Vec<f32>, String> {
    if raw {
        let mut lin = img.clone();
        lin.iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
        let den = denoise_linear(&lin, progress)?;
        return Ok(lin.iter().zip(den.iter()).map(|(a, b)| b.max(0.0) - a).collect());
    }
    let mut enc = img.clone();
    enc.iter_mut().for_each(|v| *v = linear_to_srgb(*v));
    let den = denoise_srgb(&enc, progress)?;
    Ok(enc.iter().zip(den.iter()).map(|(a, b)| srgb_to_linear(*b) - srgb_to_linear(*a)).collect())
}

pub fn apply_delta(img: &Rgb32FImage, delta: &[f32], amount: f32) -> Rgb32FImage {
    let mut out = img.clone();
    out.iter_mut().zip(delta).for_each(|(v, d)| *v = (*v + d * amount).max(0.0));
    out
}

/// Cached result for a photo, so the slow part runs once: `<user cache>/ferrotype/denoise/<hash>.bin` (f32 little-endian deltas).
fn cache_file(path: &Path) -> Option<PathBuf> {
    let m = std::fs::metadata(crate::load::copy_of(path).as_deref().unwrap_or(path)).ok()?;
    let key = format!("{}|{}|{:?}", std::fs::canonicalize(path).unwrap_or(path.into()).display(), m.len(), m.modified().ok());
    let h: String = Sha256::digest(key.as_bytes()).iter().take(12).map(|b| format!("{b:02x}")).collect();
    Some(crate::keys::dirs_cache()?.join("denoise").join(format!("{h}.bin")))
}

/// About 10 photos at 24 MP; the oldest results go first.
const CACHE_MAX: u64 = 3 << 30;

pub fn cached_delta(path: &Path, len: usize) -> Option<Vec<f32>> {
    let f = cache_file(path)?;
    let bytes = std::fs::read(&f).ok()?;
    // Mark it as recently used, so trimming the cache keeps it.
    let _ = std::fs::File::options().append(true).open(&f).and_then(|h| h.set_modified(std::time::SystemTime::now()));
    (bytes.len() == len * 4).then(|| bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect())
}

pub fn store_delta(path: &Path, delta: &[f32]) {
    if let Some(f) = cache_file(path) {
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        let bytes: Vec<u8> = delta.iter().flat_map(|v| v.to_le_bytes()).collect();
        let _ = crate::write_atomic(&f, &bytes);
        crate::keys::trim_cache(f.parent().unwrap(), CACHE_MAX);
        // Older builds kept an unbounded cache inside the AI folder.
        if let Some(old) = dir().map(|d| d.join("cache")) {
            let _ = std::fs::remove_dir_all(old);
        }
    }
}

/// The source with AI denoise applied, when the settings ask for it (export and CLI path).
pub fn prepare(img: Rgb32FImage, path: &Path, p: &Params) -> Result<Rgb32FImage, String> {
    if p.ai_nr <= 0.0 {
        return Ok(img);
    }
    let delta = match cached_delta(path, img.len()) {
        Some(d) => d,
        None => {
            let d = noise_delta(&img, is_raw(path), &|_, _| {})?;
            store_delta(path, &d);
            d
        }
    };
    Ok(apply_delta(&img, &delta, p.ai_nr / 100.0))
}

/// Whether a photo (or a virtual copy of one) is a RAW file.
pub fn is_raw(path: &Path) -> bool {
    crate::load::is_raw(crate::load::copy_of(path).as_deref().unwrap_or(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs FERROTYPE_AI_DIR with the runtime and a model in it; `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn runs_the_installed_model_over_tiles() {
        let img = Rgb32FImage::from_fn(300, 250, |x, y| {
            let n = ((x * 7919 + y * 104729) % 97) as f32 / 97.0 - 0.5;
            image::Rgb([0.4 + 0.1 * n; 3])
        });
        let out = denoise_srgb(&img, &|d, t| eprintln!("tile {d}/{t}")).unwrap();
        assert_eq!(out.dimensions(), img.dimensions());
        let var = |im: &Rgb32FImage| {
            let m = im.iter().sum::<f32>() / im.len() as f32;
            im.iter().map(|v| (v - m).powi(2)).sum::<f32>() / im.len() as f32
        };
        assert!(var(&out) < var(&img) * 0.5, "{} vs {}", var(&out), var(&img));
    }

    /// RawNIND on linear data: noise goes down, brightness stays (the output is gain-matched).
    #[test]
    #[ignore]
    fn rawnind_denoises_linear_and_keeps_level() {
        let img = Rgb32FImage::from_fn(700, 520, |x, y| {
            let n = ((x * 7919 + y * 104729) % 97) as f32 / 97.0 - 0.5;
            let base = 0.05 + 0.15 * x as f32 / 700.0;
            image::Rgb([base + 0.03 * n, base * 0.9 + 0.03 * n, base * 0.7 + 0.03 * n])
        });
        let d = noise_delta(&img, true, &|_, _| {}).unwrap();
        let out = apply_delta(&img, &d, 1.0);
        let err = |im: &Rgb32FImage| im.enumerate_pixels().map(|(x, _, p)| (p[0] - (0.05 + 0.15 * x as f32 / 700.0)).abs()).sum::<f32>() / (700.0 * 520.0);
        let mean = |im: &Rgb32FImage| im.iter().sum::<f32>() / im.len() as f32;
        assert!(err(&out) < err(&img) * 0.6, "{} vs {}", err(&out), err(&img));
        assert!((mean(&out) / mean(&img) - 1.0).abs() < 0.03, "{} vs {}", mean(&out), mean(&img));
    }
}

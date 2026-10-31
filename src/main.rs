mod app;
mod denoise;
mod develop;
mod load;
mod negative;
mod output;
mod widgets;
mod hdr;
mod heif;
mod lens;
mod pano;
mod keys;
mod scan;
mod scanfile;
mod scanner;
mod xmp;
mod rename;
mod hdrmerge;
mod gpu;
mod calib;
mod upright;
mod roll;
mod colour;
mod demosaic;
mod library;
mod focus;

use develop::Params;
use image::{DynamicImage, Rgb32FImage, codecs::jpeg::JpegEncoder};
use std::path::{Path, PathBuf};

/// Edits live next to the photo: `IMG_0001.NEF.ferrotype.json`.
pub fn sidecar(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".ferrotype.json");
    s.into()
}

/// The photo's saved edits; without a Ferrotype sidecar, a Lightroom `photo.xmp` next to it is imported.
pub fn read_params(path: &Path) -> Option<Params> {
    let x = std::fs::read_to_string(xmp_path(path)).ok();
    match std::fs::read_to_string(sidecar(path)) {
        Ok(t) => {
            let mut p: Params = serde_json::from_str(&t).ok()?;
            // Saved before the camera curve: its settings were made on the flat look (see `wants_cam_curve`).
            if !t.contains("\"process\"") {
                p.process = 1;
            }
            // Ratings and labels set in Lightroom, darktable or Bridge since win over ours.
            if let Some(x) = &x {
                let (r, l) = xmp::rating_label(x);
                xmp::apply_rating_label(&mut p, r, l);
                // So do keywords set there.
                if let Some(k) = xmp::keywords(x) {
                    p.keywords = k;
                }
            }
            p.migrate();
            Some(p)
        }
        Err(_) => Some(xmp::parse(&x?).1),
    }
}

/// Whether a photo should get the camera's tone curve now: it has none yet, and its settings weren't made on the
/// flat look of older versions (`Params::process`; those would change under it). Lightroom settings imported from
/// an XMP assume Lightroom's own base curve, so they get it.
pub fn wants_cam_curve(p: &Params) -> bool {
    p.cam_curve.is_empty() && p.process >= 2
}

/// A sidecar that exists but can't be read (damaged, or written by a much newer version).
pub fn sidecar_damaged(path: &Path) -> bool {
    std::fs::read_to_string(sidecar(path)).is_ok_and(|t| serde_json::from_str::<Params>(&t).is_err())
}

/// `IMG_0001.xmp`, as Lightroom names it, for RAW files only. A RAW+JPEG pair would otherwise share one file, so
/// rejecting the JPEG rejected the RAW too (Lightroom keeps a JPEG's rating inside the JPEG). Virtual copies have
/// none (they share the photo's).
pub fn xmp_path(path: &Path) -> PathBuf {
    if load::copy_of(path).is_some() || !load::is_raw(path) { PathBuf::new() } else { path.with_extension("xmp") }
}

/// Lightroom presets (.xmp) as named Ferrotype settings.
pub fn import_xmp_preset(path: &Path) -> Option<(String, Params)> {
    let (name, p) = xmp::parse(&std::fs::read_to_string(path).ok()?);
    Some((name.unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().into()), p))
}

/// Frames per contact sheet page: 10 rows of 6. A JPEG can be at most 65,535 px tall (about 760 frames), and a
/// page keeps memory to one page of frames at a time.
const CONTACT_PAGE_ROWS: u32 = 10;

/// Contact sheets of `files` saved to `out`, or to `<out>-p1.jpg`, `-p2`… when they need more than one page.
/// Frames are numbered across the pages. Returns the files written.
pub fn save_contact_sheets(files: &[PathBuf], cols: u32, out: &Path) -> Result<Vec<PathBuf>, String> {
    let cols = cols.clamp(1, files.len().max(1) as u32);
    let per_page = (cols * CONTACT_PAGE_ROWS) as usize;
    let pages = files.len().div_ceil(per_page).max(1);
    let mut written = vec![];
    for (n, chunk) in files.chunks(per_page.max(1)).enumerate() {
        let path = if pages == 1 {
            out.to_path_buf()
        } else {
            let stem = out.file_stem().unwrap_or_default().to_string_lossy();
            let ext = out.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "jpg".into());
            out.with_file_name(format!("{stem}-p{}.{ext}", n + 1))
        };
        save_output(contact_sheet(chunk, cols, n * per_page), &path, 90)?;
        written.push(path);
    }
    if pages > 1 && std::fs::metadata(out).is_ok_and(|m| m.len() == 0) {
        let _ = std::fs::remove_file(out); // the empty name claimed for a single sheet
    }
    Ok(written)
}

/// One image of some frames: numbered (from `first` + 1) on black, like a darkroom contact print.
fn contact_sheet(files: &[PathBuf], cols: u32, first: usize) -> Rgb32FImage {
    use rayon::prelude::*;
    let cell = 600u32; // each frame fits a 600 × 400 box
    let (fw, fh) = (cell, cell * 2 / 3);
    let (gap, label) = (cell / 12, cell / 9);
    let cols = cols.clamp(1, files.len().max(1) as u32);
    let rows = (files.len() as u32).div_ceil(cols).max(1);
    let (w, h) = (cols * (fw + gap) + gap, rows * (fh + label + gap) + gap);
    let frames: Vec<Option<Rgb32FImage>> = files.par_iter().map(|f| {
        let (img, toned) = load::load_quick_toned(f, cell).ok()?;
        let mut p = Params { border: Default::default(), ..read_params(f).unwrap_or_default() };
        if toned {
            p.cam_curve.clear(); // the camera's JPEG already has it
        }
        Some(load::fit(&develop::render(&img, &p, 0.15), cell))
    }).collect();
    let mut sheet = Rgb32FImage::from_pixel(w, h, image::Rgb([0.04; 3]));
    for (i, (f, img)) in files.iter().zip(frames).enumerate() {
        let (c, r) = (i as u32 % cols, i as u32 / cols);
        let (x0, y0) = (gap + c * (fw + gap), gap + r * (fh + label + gap));
        if let Some(img) = img {
            // Fit inside the frame box, keeping the photo's shape.
            let k = (fw as f32 / img.width() as f32).min(fh as f32 / img.height() as f32).min(1.0);
            let img = load::fit(&img, (img.width().max(img.height()) as f32 * k) as u32);
            let (iw, ih) = img.dimensions();
            image::imageops::replace(&mut sheet, &img, (x0 + (fw - iw.min(fw)) / 2) as i64, (y0 + (fh - ih.min(fh)) / 2) as i64);
        }
        let name = f.file_name().unwrap_or_default().to_string_lossy();
        develop::draw_text(&mut sheet, &format!("{}   {name}", first + i + 1), (x0 + fw / 2) as f32, (y0 + fh + label / 2) as f32, label as f32 * 0.32, 0.8);
    }
    sheet
}

/// Save a photo's edits (atomically) and mirror rating and label into its XMP.
pub fn write_params(path: &Path, p: &Params) -> std::io::Result<()> {
    let dest = sidecar(path);
    // Never write over edits we couldn't read: keep them as `….ferrotype.json.damaged` for repair by hand.
    if sidecar_damaged(path) {
        let mut keep = dest.clone().into_os_string();
        keep.push(".damaged");
        std::fs::rename(&dest, &keep)?;
    }
    write_atomic(&dest, serde_json::to_string_pretty(p)?.as_bytes())?;
    // Rating, reject and colour label also go to the standard XMP sidecar so other apps see them.
    let xp = xmp_path(path);
    if !xp.as_os_str().is_empty()
        && let Some(x) = xmp::with_rating_label(std::fs::read_to_string(&xp).ok().as_deref(), p)
    {
        // Lightroom keeps all its edits for the photo in this file: never leave it half written.
        write_atomic(&xp, x.as_bytes())?;
    }
    Ok(())
}

/// Move rejected photos into `dest` (`_rejects`) with their edits, RAW `.xmp` and virtual copies. Never replaces a
/// file already there: a second `DSC_0001.NEF` lands as `DSC_0001-2.NEF`, its sidecars renamed to match. A virtual
/// copy is only shown next to its photo, so a rejected copy whose photo isn't moving stays where it is (still
/// marked rejected). Returns photos moved.
pub fn move_to_rejects(dest: &Path, files: &[PathBuf]) -> std::io::Result<usize> {
    std::fs::create_dir_all(dest)?;
    let free = |p: &Path| !p.exists() && !sidecar(p).exists();
    let mut listings: std::collections::HashMap<PathBuf, Vec<String>> = Default::default();
    let mut moved = 0;
    for f in files.iter().filter(|f| load::copy_of(f).is_none()) {
        let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
        let (stem, ext) = (f.file_stem().unwrap_or_default().to_string_lossy().to_string(), f.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default());
        let to = std::iter::once(dest.join(&name)).chain((2..).map(|k| dest.join(format!("{stem}-{k}{ext}")))).find(|p| free(p) && (xmp_path(p).as_os_str().is_empty() || !xmp_path(p).exists())).unwrap();
        let mut parts = vec![(f.clone(), to.clone()), (sidecar(f), sidecar(&to))];
        let x = xmp_path(f);
        if !x.as_os_str().is_empty() {
            parts.push((x, xmp_path(&to)));
        }
        // Its virtual copies (`<name>.copyN.ferrotype.json`) go with it, under its new name.
        let dir = f.parent().unwrap_or(Path::new("")).to_path_buf();
        let listing = listings.entry(dir.clone()).or_insert_with(|| {
            std::fs::read_dir(if dir.as_os_str().is_empty() { Path::new(".") } else { &dir }).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()
        });
        let to_name = to.file_name().unwrap_or_default().to_string_lossy().to_string();
        for entry in listing.iter() {
            let Some(copy) = entry.strip_suffix(".ferrotype.json").map(|c| dir.join(c)) else { continue };
            if load::copy_of(&copy).as_deref() != Some(f.as_path()) {
                continue;
            }
            let n = copy.file_name().and_then(|c| c.to_str()).and_then(|c| c.rsplit_once(".copy")).map_or("2", |(_, n)| n).to_string();
            let target = std::iter::once(n.clone()).chain((2..).map(|k| k.to_string())).map(|k| dest.join(format!("{to_name}.copy{k}"))).find(|p| free(p)).unwrap();
            parts.push((sidecar(&copy), sidecar(&target)));
        }
        let mut any = false;
        for (a, b) in parts.into_iter().filter(|(a, _)| a.exists()) {
            std::fs::rename(&a, &b)?;
            any = true;
        }
        moved += any as usize;
    }
    Ok(moved)
}

/// Write to a temp file beside `path`, then rename over `path`, so a crash or full disk never leaves a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // A temp name of its own per write: the sync thread and a rating click can save the same sidecar at once.
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.{n}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    std::fs::rename(&tmp, path)
}

/// Export settings: file type, size, quality, and the finishing touches. Remembered between sessions.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ExportOpts {
    #[serde(skip)]
    pub ext: &'static str,
    /// Long edge in pixels, 0 = full size.
    pub long_edge: u32,
    pub quality: u8,
    /// JPEG only: add an HDR gain map so HDR screens show brighter highlights.
    pub hdr: bool,
    /// Exact output canvas in pixels (photo fitted inside, padded), 0 = off. Set by print sizes and export sets.
    #[serde(skip)]
    pub canvas: [u32; 2],
    /// Print size: paper short side, long side (inches) and dots per inch; 0 = not printing.
    pub print: [f32; 3],
    /// Print: crop to fill the paper instead of fitting with white margins.
    pub print_fill: bool,
    pub wm_text: String,
    pub wm_logo: String,
    pub wm_pos: u8,
    pub wm_size: f32,
    pub wm_opacity: f32,
    /// File name pattern ({name} {n} {date} {roll} {stock} {camera} {lens}); empty = the photo's own name.
    pub name: String,
    pub roll: String,
    /// JPEG: write camera, lens, roll and film stock (never GPS or serial numbers).
    pub meta: bool,
    /// Exporting a folder: include its subfolders and recreate the folder tree in the destination.
    pub tree: bool,
    /// Colour space of the file (sRGB, Display P3, Adobe RGB, ProPhoto), with its ICC profile embedded.
    pub space: colour::Space,
    /// Command line: straighten each photo with Upright before exporting.
    #[serde(skip)]
    pub upright: Option<upright::Mode>,
}

impl Default for ExportOpts {
    fn default() -> Self {
        Self {
            ext: "jpg", long_edge: 0, quality: 92, hdr: false, canvas: [0, 0], print: [0.0, 0.0, 300.0], print_fill: false,
            wm_text: String::new(), wm_logo: String::new(), wm_pos: 0, wm_size: 4.0, wm_opacity: 70.0,
            name: String::new(), roll: String::new(), meta: false, tree: false, upright: None, space: colour::Space::Srgb,
        }
    }
}

impl ExportOpts {
    /// Export settings saved by the app last time.
    pub fn load() -> Self {
        keys::dirs_config().and_then(|d| std::fs::read_to_string(d.join("export.json")).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(d) = keys::dirs_config() {
            let _ = std::fs::create_dir_all(&d);
            let _ = write_atomic(&d.join("export.json"), serde_json::to_string_pretty(self).unwrap_or_default().as_bytes());
        }
    }

    /// Output canvas: an export set's, else the print size at its resolution.
    fn canvas_px(&self) -> Option<(u32, u32, bool)> {
        if self.canvas[0] > 0 {
            return Some((self.canvas[0], self.canvas[1], false));
        }
        (self.print[0] > 0.0).then(|| ((self.print[0] * self.print[2]).round() as u32, (self.print[1] * self.print[2]).round() as u32, true))
    }
}

/// Render at the requested size. A smaller export renders from a downscaled source (much faster),
/// and radii scale with it so it looks like the full-size render.
pub fn render_export(full: &Rgb32FImage, p: &Params, o: &ExportOpts) -> Rgb32FImage {
    if o.space == colour::Space::Srgb {
        return render_export_with(full, p, o, develop::render);
    }
    // Wide gamut: render keeping the colours sRGB can't hold, then convert into the export's space.
    let mut out = render_export_with(full, p, o, develop::render_wide);
    colour::to_space(&mut out, o.space);
    out
}

fn render_export_with(full: &Rgb32FImage, p: &Params, o: &ExportOpts, render: impl Fn(&Rgb32FImage, &Params, f32) -> Rgb32FImage) -> Rgb32FImage {
    // The long edge is for the finished picture: size the source so the cropped photo still reaches it.
    let [l, t, r, b] = develop::crop_of(p);
    let (w, h) = develop::oriented(full.width(), full.height(), p);
    let crop_long = (w as f32 * (1.0 - l - r)).max(h as f32 * (1.0 - t - b));
    // A canvas only needs the photo at about the canvas's long side.
    let want = o.canvas_px().map_or(o.long_edge, |(cw, ch, _)| cw.max(ch));
    let o = &ExportOpts { long_edge: if o.long_edge == 0 { want } else { o.long_edge.min(want.max(1)) }, ..o.clone() };
    let out = if o.long_edge == 0 || o.long_edge as f32 >= crop_long {
        render(full, p, 1.0)
    } else {
        let src = load::fit(full, (full.width().max(full.height()) as f32 * o.long_edge as f32 / crop_long).ceil() as u32);
        render(&src, p, src.width() as f32 / full.width() as f32)
    };
    let mut out = if o.long_edge == 0 { out } else { load::fit(&out, o.long_edge) }; // the border made it larger
    if let Some((w, h, print)) = o.canvas_px() {
        // Social sizes pad with the border's colour (black unless it's white); prints with white paper.
        let white = print || p.border.white || p.border.width <= 0.0;
        out = output::fit_canvas(out, w, h, print, white, print && o.print_fill);
    }
    if !o.wm_text.trim().is_empty() || !o.wm_logo.trim().is_empty() {
        output::watermark(&mut out, &o.wm_text, &o.wm_logo, o.wm_pos, o.wm_size, o.wm_opacity);
    }
    out
}

/// Camera and lens details of `src` for an export, with the folder's roll notes filling in what a film scan
/// lacks (camera, lens, speed, date, and the frame's aperture, shutter and focal length).
pub fn export_shot(src: &Path) -> lens::Shot {
    let mut s = lens::shot(src).unwrap_or_default();
    roll::fill_shot(src, &mut s);
    s
}

/// The one-line description written with the camera details: roll, film stock (and push), developer, frame note.
pub fn export_description(p: &Params, o: &ExportOpts, src: &Path) -> String {
    match roll::for_photo(src) {
        Some((mut r, f)) => {
            if !o.roll.trim().is_empty() {
                r.roll = o.roll.trim().to_string();
            }
            if p.neg.enabled {
                r.stock = p.neg.stock.clone();
            }
            [r.summary(), f.note.trim().to_string()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
        }
        None => [(!o.roll.trim().is_empty()).then(|| format!("Roll {}", o.roll.trim())), output::stock_name(p)].into_iter().flatten().collect::<Vec<_>>().join(" · "),
    }
}

/// What the export of `src` carries besides pixels: camera details when `o.meta` asks for them, print DPI.
pub fn export_embed(p: &Params, o: &ExportOpts, src: &Path) -> output::Embed {
    let mut e = output::Embed { dpi: (o.print[0] > 0.0 && o.canvas[0] == 0).then(|| o.print[2].round() as u16), icc: o.space.icc(), ..Default::default() };
    if o.meta {
        let shot = export_shot(src);
        let desc = export_description(p, o, src);
        e.exif = output::exif_tiff(&shot, &desc);
        e.xmp = output::xmp_packet(&shot, &desc, &p.keywords);
    }
    e
}

/// Export one rendered photo to `out`, as an HDR gain-map JPEG when asked. `src` is the original file (for the
/// camera and lens details written with `o.meta`).
pub fn write_export(full: &Rgb32FImage, p: &Params, o: &ExportOpts, out: &Path, src: &Path) -> Result<(), String> {
    let e = export_embed(p, o, src);
    if o.hdr && o.ext == "jpg" {
        // The gain map is defined on sRGB here: HDR exports stay sRGB.
        let o = &ExportOpts { space: colour::Space::Srgb, ..o.clone() };
        let e = output::Embed { icc: vec![], ..e };
        let sdr = render_export(full, p, o);
        jpeg_fits(&sdr)?;
        let low = render_export(full, &Params { exposure: p.exposure - hdr::HEADROOM, ..p.clone() }, o);
        let segs = output::jpeg_segments(&output::Embed { xmp: String::new(), ..e.clone() });
        let bytes = hdr::encode_ultrahdr(&sdr, &hdr::gain_map(&sdr, &low), o.quality.clamp(1, 100), &segs, e.dpi)?;
        return std::fs::write(out, bytes).map_err(|e| e.to_string());
    }
    save_output_with(render_export(full, p, o), out, o.quality, &e)
}

/// Save a rendered (sRGB-encoded) image. `.tif` → 16-bit TIFF, `.png` → 8-bit PNG, anything else → JPEG.
/// Nothing but pixels is written, so exports never carry the camera's GPS location or other metadata.
pub fn save_output(img: Rgb32FImage, out: &Path, quality: u8) -> Result<(), String> {
    save_output_with(img, out, quality, &output::Embed::default())
}

/// `save_output`, plus what `e` holds (camera details, colour profile, DPI) in whichever format it is.
pub fn save_output_with(img: Rgb32FImage, out: &Path, quality: u8, e: &output::Embed) -> Result<(), String> {
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let dynimg = DynamicImage::ImageRgb32F(img);
    match ext.as_str() {
        "tif" | "tiff" => output::write_tiff(&dynimg.to_rgb16(), out, e),
        "png" => output::write_png(&dynimg.to_rgb8(), out, e),
        _ => {
            jpeg_fits(dynimg.as_rgb32f().unwrap())?;
            let mut bytes = vec![];
            let mut enc = JpegEncoder::new_with_quality(&mut bytes, quality.clamp(1, 100));
            if let Some(d) = e.dpi {
                enc.set_pixel_density(image::codecs::jpeg::PixelDensity::dpi(d));
            }
            dynimg.to_rgb8().write_with_encoder(enc).map_err(|e| e.to_string())?;
            let segs = output::jpeg_segments(e);
            if !segs.is_empty() {
                bytes = output::insert_segment(&bytes, &segs);
            }
            std::fs::write(out, bytes).map_err(|e| e.to_string())
        }
    }
}

/// JPEG stores width and height in 16 bits: say so plainly instead of the encoder's error.
fn jpeg_fits(img: &Rgb32FImage) -> Result<(), String> {
    if img.width().max(img.height()) > 65_535 {
        return Err(format!("{} × {} px is too big for JPEG (65,535 px at most): print it as TIFF or PNG, or set a long edge", img.width(), img.height()));
    }
    Ok(())
}

/// Claim `dir/stem.ext`, or `stem-2.ext`, `stem-3.ext`… when taken. Never replaces a file (an original
/// JPEG, or the other half of a RAW+JPEG pair), and parallel exports can't pick the same name.
pub fn claim_output(dir: &Path, stem: &str, ext: &str) -> Result<PathBuf, String> {
    for i in 1..10_000 {
        let out = dir.join(if i == 1 { format!("{stem}.{ext}") } else { format!("{stem}-{i}.{ext}") });
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&out) {
            Ok(_) => return Ok(out),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("no free file name".into())
}

/// `IMG_0001` for `IMG_0001.NEF`, `IMG_0001-copy2` for its virtual copy.
pub fn export_stem(path: &Path) -> String {
    let stem = |p: &Path| p.file_stem().and_then(|s| s.to_str()).unwrap_or("export").to_string();
    match load::copy_of(path) {
        Some(orig) => format!("{}-{}", stem(&orig), path.extension().and_then(|e| e.to_str()).unwrap_or("copy")),
        None => stem(path),
    }
}

/// Render one file with its sidecar edits. `n` is its number in the batch (for `{n}` in the name pattern).
pub fn export_file(path: &Path, out_dir: &Path, o: &ExportOpts, n: usize, overrides: impl Fn(&mut Params)) -> Result<PathBuf, String> {
    export_prepared(path, &export_source(path, overrides)?, out_dir, o, n)
}

/// A photo ready to export: its edits, and its picture after infrared clean and AI denoise. Several exports of one
/// photo (export sets) share it rather than decoding the RAW again for each.
pub fn export_source(path: &Path, overrides: impl Fn(&mut Params)) -> Result<(Params, Rgb32FImage), String> {
    // A damaged edits file would otherwise print the frame unedited, which looks like a success.
    if sidecar_damaged(path) {
        return Err("its edits file is damaged, so it was not printed unedited (open it in the app to start fresh)".into());
    }
    let mut p = read_params(path).unwrap_or_default();
    overrides(&mut p);
    let raw = load::load(path)?;
    // A RAW never opened in the app has no camera curve yet: measure it, as opening it would.
    if load::is_raw_photo(path) && wants_cam_curve(&p) {
        p.cam_curve = load::camera_curve(path, &raw);
    }
    let raw = scan::prepare_ir(raw, path, &p);
    let img = denoise::prepare(raw, path, &p)?;
    Ok((p, img))
}

/// Write one export of a prepared photo (see `export_source`) into `out_dir`.
pub fn export_prepared(path: &Path, (p, img): &(Params, Rgb32FImage), out_dir: &Path, o: &ExportOpts, n: usize) -> Result<PathBuf, String> {
    let p = match o.upright {
        Some(m) => upright::apply(img, p, m),
        None => p.clone(),
    };
    let shot = if !o.name.trim().is_empty() && o.name != "{name}" { export_shot(path) } else { Default::default() };
    // {roll}: the one given for this export, else the folder's roll notes.
    let roll = if o.roll.trim().is_empty() { roll::for_photo(path).map(|r| r.0.roll).unwrap_or_default() } else { o.roll.clone() };
    let out = claim_output(out_dir, &output::file_name(&o.name, path, n, &roll, &p, &shot), o.ext)?;
    write_export(img, &p, o, &out, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&out);
    })?;
    Ok(out)
}

/// The photos among one folder's `entries`, with their virtual copies (`IMG.NEF.copy2`, kept only as
/// `IMG.NEF.copy2.ferrotype.json`; the Scan tab makes a strip's frames this way), in library order.
pub fn photos_in(entries: &[PathBuf]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = entries.iter().filter(|p| load::is_supported(p) && p.is_file()).cloned().collect();
    let real: std::collections::HashSet<&Path> = files.iter().map(PathBuf::as_path).collect();
    let copies: Vec<PathBuf> = entries.iter().filter_map(|e| {
        let v = PathBuf::from(e.to_str()?.strip_suffix(".ferrotype.json")?);
        load::copy_of(&v).is_some_and(|o| real.contains(o.as_path())).then_some(v)
    }).collect();
    files.extend(copies);
    load::sort_photos(&mut files);
    files
}

/// Every photo (and virtual copy) in `dir` and its subfolders, with the subfolder it's in (relative to `dir`).
/// Hidden folders and `_rejects` are left out.
pub fn photos_under(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out = vec![];
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(dir.join(&rel)) else { continue };
        let entries: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| !p.file_name().unwrap_or_default().to_string_lossy().starts_with('.')).collect();
        let mut subdirs: Vec<&PathBuf> = entries.iter().filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "_rejects")).collect();
        subdirs.sort();
        stack.extend(subdirs.into_iter().rev().map(|p| rel.join(p.file_name().unwrap_or_default())));
        out.extend(photos_in(&entries).into_iter().map(|p| (rel.clone(), p)));
    }
    out
}

/// Megapixels all panorama frames may take together once loaded (about 2.4 GB), and the largest result (1.8 GB).
const PANO_INPUT_MP: f64 = 200.0;
const PANO_MAX_MP: f64 = 150.0;

/// Merge photos into a panorama next to the first one (`IMG_0001-pano.tif`, 16-bit), with a crop that trims the
/// empty edges saved as its edits. Each photo's lens profile (or one found from its EXIF) is applied first.
pub fn make_pano(files: &[PathBuf], proj: pano::Projection, progress: &dyn Fn(&str)) -> Result<PathBuf, String> {
    use rayon::prelude::*;
    progress("Loading photos");
    // Memory: 12 bytes a pixel. Six 45 MP frames at full size are 3.2 GB before blending, so the frames share a
    // budget and each is shrunk as soon as it is loaded; two load at a time so full-size copies don't pile up.
    let per_frame = PANO_INPUT_MP * 1e6 / files.len().max(1) as f64;
    let mut imgs: Vec<Rgb32FImage> = Vec::with_capacity(files.len());
    for pair in files.chunks(2) {
        let done: Vec<Rgb32FImage> = pair.par_iter().map(|f| {
            let img = load::load(f)?;
            let prof = read_params(f).filter(|p| !p.lens.is_empty()).map(|p| (p.lens, p.lens_focal, p.lens_f, p.lens_crop)).or_else(|| lens::detect(f));
            let img = match prof {
                Some((lens, lens_focal, lens_f, lens_crop)) => develop::geometry(&img, &Params { lens, lens_focal, lens_f, lens_crop, ..Default::default() }),
                None => img,
            };
            let px = img.width() as f64 * img.height() as f64;
            Ok(if px > per_frame { load::fit(&img, (img.width().max(img.height()) as f64 * (per_frame / px).sqrt()) as u32) } else { img })
        }).collect::<Result<_, String>>()?;
        imgs.extend(done);
    }
    // Focal length in pixels from EXIF (35 mm-equivalent width is 36 mm); the alignment refines it.
    let long = imgs[0].width().max(imgs[0].height()) as f64;
    let hint = lens::shot(&files[0]).filter(|s| s.focal > 0.0).and_then(|s| Some(s.focal as f64 * lens::camera_crop(&s)? as f64 / 36.0 * long));
    let (pano, crop) = pano::stitch(&imgs, hint, proj, PANO_MAX_MP, progress)?;
    progress("Saving");
    let dir = files[0].parent().unwrap_or(Path::new("."));
    let out = claim_output(dir, &format!("{}-pano", export_stem(&files[0])), "tif")?;
    let mut enc = pano;
    enc.par_iter_mut().for_each(|v| *v = load::linear_to_srgb(*v));
    save_output(enc, &out, 100).inspect_err(|_| { let _ = std::fs::remove_file(&out); })?;
    write_params(&out, &Params { crop, ..Default::default() }).map_err(|e| e.to_string())?;
    Ok(out)
}

const USAGE: &str = "ferrotype                      open the editor
ferrotype <folder|file>         open the editor on a folder or photo
ferrotype export [--out DIR] [--tif|--png|--hdr] [--size PX] [--quality Q] [--neg STOCK] [--lens] [--upright MODE] [--space SPACE] FILES...
                [--set instagram|story|portfolio] [--print 8x10|20x30cm] [--dpi N] [--watermark TEXT] [--logo PNG]
                [--name PATTERN] [--roll N] [--meta]
                                render files with their saved edits (headless); --lens adds the lens profile;
                                --name uses {name} {n} {date} {roll} {stock} {camera} {lens};
                                --meta writes camera, lens, roll and stock into the JPEG;
                                a FOLDER is exported with its subfolders, keeping the folder tree
ferrotype rename --name PATTERN [--roll N] [--dry-run] FILES...
                                rename photos and their edits from a pattern (tokens as for export)
ferrotype rename --undo FOLDER  put back the names from the last rename in FOLDER
ferrotype pano [--projection auto|cylindrical|spherical|perspective] FILES...
                                merge overlapping photos into FIRST-pano.tif
ferrotype hdr FILES...          merge an exposure bracket into FIRST-hdr.exr (32-bit, opens like any photo)
ferrotype hdr --auto FOLDER     find brackets (by capture time and exposure) and merge each
ferrotype stack FILES...        focus-stack photos focused at different distances into FIRST-stack.exr
ferrotype contact [--out FILE] [--cols N] FILES...
                                one contact sheet image of all FILES (default contact.jpg, 6 columns)
ferrotype lens FILES...         show which lens profile each photo gets
ferrotype stocks                list film stock ids
ferrotype ai-setup              download the optional AI denoise (ONNX Runtime, RawNIND and SCUNet, about 140 MB)";

fn cli_export(args: &[String]) -> Result<(), String> {
    let (mut out, mut o, mut neg, mut files) = (PathBuf::from("."), ExportOpts::default(), None::<String>, vec![]);
    let mut lens_auto = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = it.next().ok_or("--out needs a folder")?.into(),
            "--tif" => o.ext = "tif",
            "--png" => o.ext = "png",
            "--hdr" => o.hdr = true,
            "--size" => o.long_edge = it.next().and_then(|v| v.parse().ok()).ok_or("--size needs the long edge in pixels")?,
            "--quality" => o.quality = it.next().and_then(|v| v.parse().ok()).ok_or("--quality needs 1-100")?,
            "--neg" => neg = Some(it.next().ok_or("--neg needs a stock id")?.clone()),
            "--lens" => lens_auto = true,
            "--space" => {
                let v = it.next().ok_or("--space needs srgb, p3, adobe or prophoto")?;
                o.space = colour::Space::parse(v).ok_or("--space is srgb, p3, adobe or prophoto")?;
            }
            "--upright" => {
                let v = it.next().ok_or("--upright needs auto, level, vertical or full")?;
                o.upright = Some(upright::Mode::parse(v).ok_or("--upright is auto, level, vertical or full")?);
            }
            "--name" => o.name = it.next().ok_or("--name needs a pattern like \"{roll}-{n}\"")?.clone(),
            "--roll" => o.roll = it.next().ok_or("--roll needs a roll number")?.clone(),
            "--meta" => o.meta = true,
            "--watermark" => o.wm_text = it.next().ok_or("--watermark needs text")?.clone(),
            "--logo" => o.wm_logo = it.next().ok_or("--logo needs an image file")?.clone(),
            "--print" => {
                let v = it.next().ok_or("--print needs a paper size like 8x10 (inches) or 20x30cm")?;
                let cm = v.ends_with("cm");
                let (a, b) = v.trim_end_matches("cm").trim_end_matches("in").split_once('x').ok_or("--print size looks like 8x10 or 20x30cm")?;
                let (a, b): (f32, f32) = (a.parse().map_err(|_| "bad print size")?, b.parse().map_err(|_| "bad print size")?);
                let k = if cm { 1.0 / 2.54 } else { 1.0 };
                o.print = [a.min(b) * k, a.max(b) * k, o.print[2]];
            }
            "--dpi" => o.print[2] = it.next().and_then(|v| v.parse().ok()).filter(|d: &f32| (1.0..=4800.0).contains(d)).ok_or("--dpi needs a number from 1 to 4800")?,
            "--set" => {
                let v = it.next().ok_or("--set needs instagram, story or portfolio")?;
                let (_, canvas, long, q) = output::SETS.iter().find(|s| s.0.eq_ignore_ascii_case(v)).ok_or("--set is instagram, story or portfolio")?;
                (o.canvas, o.long_edge, o.quality, o.ext) = (*canvas, *long, *q, "jpg");
            }
            f => files.push(PathBuf::from(f)),
        }
    }
    if files.is_empty() {
        return Err(USAGE.into());
    }
    // Folders: every photo inside, into the same tree under --out (named after the folder).
    let jobs: Vec<(PathBuf, PathBuf)> = files.iter().flat_map(|f| {
        if f.is_dir() {
            let top = PathBuf::from(f.file_name().unwrap_or_default());
            photos_under(f).into_iter().map(|(rel, p)| (out.join(&top).join(rel), p)).collect()
        } else {
            vec![(out.clone(), f.clone())]
        }
    }).collect();
    if let Some(s) = &neg
        && !negative::STOCKS.iter().any(|st| st.id == s)
    {
        return Err(format!("unknown film stock '{s}' (run `ferrotype stocks` for the list)"));
    }
    let mut failed = 0;
    for (n, (out, f)) in jobs.iter().enumerate() {
        std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
        let found = if lens_auto { lens::detect(f) } else { None };
        if lens_auto && found.is_none() {
            eprintln!("{}: no lens profile found", f.display());
        }
        match export_file(f, out, &o, n + 1, |p| {
            if let Some(s) = &neg {
                p.neg.enabled = true;
                p.neg.stock = s.clone();
            }
            if let Some((l, focal, fnum, crop)) = found.clone().filter(|_| p.lens.is_empty()) {
                (p.lens, p.lens_focal, p.lens_f, p.lens_crop) = (l, focal, fnum, crop);
            }
        }) {
            Ok(o) => println!("{} -> {}", f.display(), o.display()),
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e}", f.display());
            }
        }
    }
    if failed > 0 { Err(format!("{failed} file(s) failed")) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("ferrotype-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("IMG_1.jpg");
        std::fs::write(&original, b"camera jpeg").unwrap();
        // A RAW+JPEG pair exported into its own folder: the RAW's export must not replace the camera JPEG.
        let a = claim_output(&dir, "IMG_1", "jpg").unwrap();
        let b = claim_output(&dir, "IMG_1", "jpg").unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), b"camera jpeg");
        assert_eq!((a.file_name().unwrap(), b.file_name().unwrap()), ("IMG_1-2.jpg".as_ref(), "IMG_1-3.jpg".as_ref()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn damaged_sidecar_is_kept_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("ferrotype-dmg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("b.NEF");
        std::fs::write(sidecar(&photo), "{ \"exposure\": 1.5, oops").unwrap();
        assert!(sidecar_damaged(&photo) && read_params(&photo).is_none());
        // Printing it must fail rather than quietly print the frame unedited.
        assert!(export_file(&photo, &dir, &ExportOpts::default(), 1, |_| {}).unwrap_err().contains("damaged"));
        write_params(&photo, &Params::default()).unwrap();
        let kept = std::fs::read_to_string(dir.join("b.NEF.ferrotype.json.damaged")).unwrap();
        assert!(kept.contains("oops") && !sidecar_damaged(&photo));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejecting_the_jpeg_of_a_pair_leaves_the_raw_alone() {
        let dir = std::env::temp_dir().join(format!("ferrotype-pair-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (raw, jpg) = (dir.join("P.NEF"), dir.join("P.JPG"));
        write_params(&raw, &Params { rating: 4, ..Default::default() }).unwrap();
        write_params(&jpg, &Params { flag: -1, ..Default::default() }).unwrap();
        let back = read_params(&raw).unwrap();
        assert_eq!((back.rating, back.flag), (4, 0));
        assert_eq!(xmp::rating_label(&std::fs::read_to_string(dir.join("P.xmp")).unwrap()).0, Some(4));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_never_replace_a_file_already_in_rejects() {
        let dir = std::env::temp_dir().join(format!("ferrotype-rej-{}", std::process::id()));
        let dest = dir.join("_rejects");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("D.NEF"), "first card").unwrap();
        std::fs::write(dest.join("D.NEF.ferrotype.json"), "{}").unwrap();
        let photo = dir.join("D.NEF");
        std::fs::write(&photo, "second card").unwrap();
        write_params(&photo, &Params { flag: -1, ..Default::default() }).unwrap();
        assert_eq!(move_to_rejects(&dest, &[photo.clone()]).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(dest.join("D.NEF")).unwrap(), "first card");
        assert_eq!(std::fs::read_to_string(dest.join("D-2.NEF")).unwrap(), "second card");
        assert!(dest.join("D-2.NEF.ferrotype.json").exists() && dest.join("D-2.xmp").exists() && !photo.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_take_their_copies_and_leave_lone_copies_visible() {
        let dir = std::env::temp_dir().join(format!("ferrotype-rejc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dest = dir.join("_rejects");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("s.tif"), "older s.tif").unwrap();
        for f in ["s.tif", "keep.tif"] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        for c in ["s.tif.copy2", "s.tif.copy3", "keep.tif.copy2"] {
            write_params(&dir.join(c), &Params { flag: -1, ..Default::default() }).unwrap();
        }
        // The scan and one of its frames are rejected; a frame of another scan is rejected on its own.
        let rejects = [dir.join("s.tif"), dir.join("s.tif.copy2"), dir.join("keep.tif.copy2")];
        assert_eq!(move_to_rejects(&dest, &rejects).unwrap(), 1);
        // Both of the scan's frames went with it, under its new name; the lone copy stayed next to its photo.
        let in_rejects: Vec<PathBuf> = photos_in(&std::fs::read_dir(&dest).unwrap().flatten().map(|e| e.path()).collect::<Vec<_>>());
        assert_eq!(in_rejects, ["s-2.tif", "s-2.tif.copy2", "s-2.tif.copy3", "s.tif"].map(|n| dest.join(n)));
        let left: Vec<PathBuf> = photos_in(&std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect::<Vec<_>>());
        assert_eq!(left, [dir.join("keep.tif"), dir.join("keep.tif.copy2")]);
        // A folder export of the tree sees the frames too.
        assert_eq!(photos_under(&dir).len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidecar_round_trip() {
        let dir = std::env::temp_dir().join(format!("ferrotype-sc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("a.NEF");
        let p = Params { exposure: 1.25, ..Default::default() };
        write_params(&photo, &p).unwrap();
        assert!(read_params(&photo) == Some(p));
        // Edits saved before the camera curve keep the flat look they were made on; new ones get the curve.
        std::fs::write(sidecar(&photo), r#"{"exposure":1.25}"#).unwrap();
        let old = read_params(&photo).unwrap();
        assert!(old.process == 1 && !wants_cam_curve(&old));
        assert!(wants_cam_curve(&Params::default()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

fn cli_hdr(args: &[String]) -> Result<(), String> {
    let (auto, files): (bool, Vec<PathBuf>) = match args.first().map(String::as_str) {
        Some("--auto") => (true, args[1..].iter().map(PathBuf::from).collect()),
        _ => (false, args.iter().map(PathBuf::from).collect()),
    };
    if files.is_empty() {
        return Err(USAGE.into());
    }
    let groups = if auto {
        let all: Vec<PathBuf> = files.iter().flat_map(|f| if f.is_dir() { photos_under(f).into_iter().map(|p| p.1).collect() } else { vec![f.clone()] }).collect();
        let g = hdrmerge::brackets(&all);
        println!("Found {} bracket{}", g.len(), if g.len() == 1 { "" } else { "s" });
        g
    } else {
        vec![files]
    };
    for g in groups {
        let out = hdrmerge::merge_files(&g, &|s| eprintln!("{s}…"))?;
        println!("{} -> {}", g.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(" + "), out.display());
    }
    Ok(())
}

fn cli_stack(args: &[String]) -> Result<(), String> {
    let mut files: Vec<PathBuf> = args.iter().map(PathBuf::from).collect();
    if files.len() < 2 {
        return Err(USAGE.into());
    }
    load::sort_photos(&mut files);
    let out = focus::stack_files(&files, &|s| eprintln!("{s}…"))?;
    println!("{} -> {}", files.len(), out.display());
    Ok(())
}

fn cli_rename(args: &[String]) -> Result<(), String> {
    let (mut pattern, mut roll, mut dry, mut files) = (String::new(), String::new(), false, vec![]);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => pattern = it.next().ok_or("--name needs a pattern like \"{date}-{n}\"")?.clone(),
            "--roll" => roll = it.next().ok_or("--roll needs a roll number")?.clone(),
            "--dry-run" => dry = true,
            "--undo" => {
                let dir = it.next().ok_or("--undo needs the folder")?;
                let n = rename::undo(Path::new(dir))?;
                println!("Put back {n} names");
                return Ok(());
            }
            f => files.push(PathBuf::from(f)),
        }
    }
    if pattern.is_empty() || files.is_empty() {
        return Err(USAGE.into());
    }
    let plan = rename::plan(&files, &pattern, &roll);
    for (a, b) in &plan {
        println!("{} -> {}", a.display(), b.file_name().unwrap_or_default().to_string_lossy());
    }
    if !dry {
        let n = rename::apply(&plan)?;
        println!("Renamed {n} files (undo with: ferrotype rename --undo FOLDER)");
    }
    Ok(())
}

fn cli_pano(args: &[String]) -> Result<(), String> {
    let (mut proj, mut files) = (pano::Projection::Auto, vec![]);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--projection" => proj = it.next().and_then(|v| pano::Projection::parse(v)).ok_or("--projection is auto, cylindrical, spherical or perspective")?,
            f => files.push(PathBuf::from(f)),
        }
    }
    if files.len() < 2 {
        return Err(USAGE.into());
    }
    let out = make_pano(&files, proj, &|m| eprintln!("{m}…"))?;
    println!("{}", out.display());
    Ok(())
}

fn cli_contact(args: &[String]) -> Result<(), String> {
    let (mut out, mut cols, mut files) = (None::<PathBuf>, 6, vec![]);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = Some(it.next().ok_or("--out needs a file name")?.into()),
            "--cols" => cols = it.next().and_then(|v| v.parse().ok()).ok_or("--cols needs a number")?,
            f => files.push(PathBuf::from(f)),
        }
    }
    if files.is_empty() {
        return Err(USAGE.into());
    }
    // An --out you name is yours to replace; the default never replaces an earlier sheet (contact-2.jpg…).
    let out = match out {
        Some(o) => o,
        None => claim_output(Path::new("."), "contact", "jpg")?,
    };
    for p in save_contact_sheets(&files, cols, &out)? {
        println!("{}", p.display());
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("export") => cli_export(&args[1..]),
        Some("contact") => cli_contact(&args[1..]),
        Some("pano") => cli_pano(&args[1..]),
        Some("rename") => cli_rename(&args[1..]),
        Some("hdr") => cli_hdr(&args[1..]),
        Some("stack") => cli_stack(&args[1..]),
        Some("lens") => {
            for f in &args[1..] {
                let shot = lens::shot(Path::new(f)).unwrap_or_default();
                let found = lens::find(&shot).map(|l| l.name()).unwrap_or_else(|| "no profile".into());
                println!("{f}: {} {} / {} {:.0} mm f/{:.1} -> {found}", shot.make, shot.model, shot.lens, shot.focal, shot.fnum);
            }
            Ok(())
        }
        Some("stocks") => {
            negative::STOCKS.iter().for_each(|s| println!("{:<16} {} {}", s.id, s.brand, s.name));
            Ok(())
        }
        Some("ai-setup") => denoise::install(&|f, what| eprint!("\rDownloading {what}… {:.0}%   ", f * 100.0)).map(|()| eprintln!("\nAI denoise installed in {}", denoise::dir().unwrap_or_default().display())),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        other => app::run(other.map(PathBuf::from)).map_err(|e| e.to_string()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

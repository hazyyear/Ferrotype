//! Scanning from inside Ferrotype, into the open folder.
//!
//! - Windows: TWAIN. The scanner's own window opens (Epson Scan 2 for a Perfection V600 or V850, with its film
//!   holders, Digital ICE and frame detection), and each picture it scans arrives here as a 16-bit TIFF.
//! - The Scanner page: Ferrotype drives the scanner itself, over USB for the V800/V850 (`direct`) and through
//!   SANE (`scanimage`, from sane-utils) for anything SANE knows, with a preview to pick frames on.
//! - Everywhere: the scanning apps people already use (Epson Scan 2, VueScan, SilverFast) can be opened from the
//!   Scan tab, saving into the folder Ferrotype watches.
//!
//! A scan runs on its own thread and reports back through `Event`s.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

pub mod direct;
pub mod merge;
#[cfg(windows)]
mod twain;

pub enum Event {
    /// What the scanner is doing, for the status line.
    Status(String),
    /// A scan was saved.
    Saved(PathBuf),
    /// The scanner was closed: how many pictures came in, or what went wrong.
    Done(Result<usize, String>),
}

/// A scanner SANE can drive.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// SANE's device name ("epkowa:interpreter:001:004"), passed back to scan.
    pub id: String,
    /// "EPSON Perfection V600".
    pub name: String,
}

/// The settings a SANE scanner offers, read from `scanimage -A`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    /// Flatbed, Transparency Unit, …
    pub sources: Vec<String>,
    pub resolutions: Vec<u32>,
    pub depths: Vec<u32>,
    /// The colour mode's own name ("Color", "Colour"), if it has one.
    pub color_mode: Option<String>,
    /// The scan area's width and height in millimetres, when the scanner says (it is the most `-x` and `-y`
    /// take, and changes with the source: the film area of a transparency unit is smaller than the glass).
    pub bed_mm: Option<[f32; 2]>,
    /// The gamma setting that gives linear data ("--gamma-correction" "User defined (Gamma=1.0)" on Epson's
    /// SANE driver), when there is one: (option, value).
    pub linear: Option<(String, String)>,
}

/// How to scan with a SANE scanner.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub device: String,
    pub source: String,
    pub resolution: u32,
    pub depth: u32,
    /// Passes per frame, averaged (1 = a single scan).
    pub passes: u32,
    /// Ask the scanner for linear data, for negative conversion.
    pub linear: bool,
    /// The scanner's name, for the files' tags.
    pub device_name: String,
}

/// A frame to scan: left, top, width and height in millimetres on the scanner.
pub type Area = [f32; 4];

/// The next scan's name in `dir`: `scan-0001.tif`, then one more than the highest number used in the folder
/// or its `_rejects`, so a new scan never fills the gap a rejected frame left and the roll stays in order.
pub fn next_name(dir: &Path) -> PathBuf {
    let number = |p: PathBuf| -> Option<u32> { p.file_stem()?.to_str()?.strip_prefix("scan-")?.parse().ok() };
    let used = [dir.to_path_buf(), dir.join("_rejects")].into_iter().flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten()).filter_map(|e| number(e.path())).max().unwrap_or(0);
    (used + 1..).map(|n| dir.join(format!("scan-{n:04}.tif"))).find(|p| !p.exists()).unwrap_or_else(|| dir.join("scan.tif"))
}

/// Run a scan on a thread of its own, reporting through `tx`. It always ends with `Done`, even if the scan
/// panics (a driver sending something unexpected), so the Scan tab never stays busy.
pub fn run(tx: Sender<Event>, wake: impl Fn() + Send + 'static, job: impl FnOnce(&dyn Fn(Event)) -> Result<usize, String> + Send + 'static) {
    std::thread::spawn(move || {
        let say = |e: Event| {
            let _ = tx.send(e);
            wake();
        };
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&say))).unwrap_or_else(|_| Err("the scanner stopped unexpectedly".into()));
        say(Event::Done(done));
    });
}

/// Open the scanner's own window (TWAIN) and save each picture it scans into `dir`. `source` is the TWAIN
/// scanner to use ("" = the system's default).
#[cfg(windows)]
pub fn scan(dir: PathBuf, source: String, tx: Sender<Event>, wake: impl Fn() + Send + 'static) {
    run(tx, wake, move |say| twain::scan(&dir, &source, say));
}

/// Let the user pick a TWAIN scanner (the system's list); returns its name.
#[cfg(windows)]
pub fn choose_source() -> Result<String, String> {
    twain::choose()
}

/// Copy rows of a strip from the scanner into the picture `px` (`w` × `h`, `samples` per pixel), clipped to it:
/// `rows` rows of `cols` pixels at (`x0`, `y0`), `bpr` bytes apart in `buf`, `bytes` per sample (1 or 2,
/// little-endian). 8-bit samples become 16-bit.
#[cfg_attr(not(windows), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
pub fn copy_strip(px: &mut [u16], w: usize, h: usize, samples: usize, bytes: usize, buf: &[u8], bpr: usize, cols: usize, rows: usize, x0: usize, y0: usize) {
    if x0 >= w || bytes == 0 {
        return;
    }
    let n = cols.min(w - x0) * samples;
    for r in 0..rows {
        let y = y0.saturating_add(r);
        let Some(start) = r.checked_mul(bpr).filter(|s| *s < buf.len()) else { break };
        if y >= h {
            break;
        }
        let line = &buf[start..(start + n * bytes).min(buf.len())];
        let out = &mut px[(y * w + x0) * samples..][..n.min(line.len() / bytes)];
        if bytes == 2 {
            for (o, b) in out.iter_mut().zip(line.chunks_exact(2)) {
                *o = u16::from_le_bytes([b[0], b[1]]);
            }
        } else {
            for (o, b) in out.iter_mut().zip(line) {
                *o = *b as u16 * 257;
            }
        }
    }
}

// ---------- SANE ----------

fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(prog)).find(|p| p.is_file())
}

/// Scanners SANE finds (slow: it asks every backend, so call it off the UI thread). None without SANE.
pub fn sane_devices() -> Result<Vec<Device>, String> {
    if which("scanimage").is_none() {
        return Ok(vec![]);
    }
    let out = std::process::Command::new("scanimage").args(["-f", "%d\t%v %m%n"]).output().map_err(|e| format!("scanimage: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_devices(&text))
}

fn parse_devices(text: &str) -> Vec<Device> {
    text.lines().filter_map(|l| {
        let (id, name) = l.split_once('\t')?;
        let name = name.trim();
        Some(Device { id: id.trim().into(), name: if name.is_empty() { id.trim().into() } else { name.into() } })
    }).filter(|d| !d.id.is_empty()).collect()
}

/// What `device` can do with `source` chosen ("" = its default; a transparency unit has a smaller area than
/// the glass).
pub fn sane_options(device: &str, source: &str) -> Result<Options, String> {
    let ask = |source: &str| {
        let mut cmd = std::process::Command::new("scanimage");
        cmd.args(["-d", device]);
        if !source.is_empty() {
            cmd.args(["--source", source]);
        }
        cmd.arg("-A").output().map_err(|e| format!("scanimage: {e}"))
    };
    let mut out = ask(source)?;
    if !out.status.success() && !source.is_empty() {
        out = ask("")?;
    }
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(parse_options(&String::from_utf8_lossy(&out.stdout)))
}

/// The values of one `scanimage -A` option line: "    --source Flatbed|Transparency Unit [Flatbed]".
fn option_values<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("--{name} ");
    let line = text.lines().map(str::trim).find(|l| l.starts_with(&key))?;
    let v = &line[key.len()..];
    // Drop the current value in brackets at the end.
    Some(v.rsplit_once(" [").map_or(v, |(a, _)| a).trim())
}

fn parse_options(text: &str) -> Options {
    let list = |name: &str| option_values(text, name).map(|v| v.split('|').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>()).unwrap_or_default();
    let numbers = |name: &str| -> Vec<u32> {
        let Some(v) = option_values(text, name) else { return vec![] };
        let v = v.trim_end_matches("dpi").trim_end_matches("bit");
        // A range: "50..4800dpi (in steps of 1)". Offer the usual film and print resolutions inside it.
        if let Some((a, b)) = v.split(" (").next().unwrap_or(v).split_once("..") {
            let (a, b) = (num(a), num(b.trim_end_matches("dpi")));
            return [150, 300, 600, 1200, 2400, 3200, 4800, 6400].into_iter().filter(|r| *r >= a && *r <= b).collect();
        }
        v.split('|').map(num).filter(|n| *n > 0).collect()
    };
    let modes = list("mode");
    // "-x 0..215.9mm (in steps of 1) [215.9]": the most it takes.
    let most = |name: &str| -> Option<f32> {
        let key = format!("-{name} ");
        let line = text.lines().map(str::trim).find(|l| l.starts_with(&key))?;
        let range = line[key.len()..].split_whitespace().next()?;
        range.split_once("..")?.1.trim_end_matches("mm").parse().ok()
    };
    let linear = ["gamma-correction", "gamma"].iter().find_map(|name| {
        let values = list(name);
        values.iter().find(|v| v.contains("1.0") || v.to_ascii_lowercase().contains("linear")).map(|v| (name.to_string(), v.clone()))
    });
    Options {
        sources: list("source"),
        resolutions: numbers("resolution"),
        depths: numbers("depth"),
        color_mode: modes.iter().find(|m| m.to_ascii_lowercase().contains("colo")).cloned(),
        bed_mm: most("x").zip(most("y")).map(|(x, y)| [x, y]),
        linear,
    }
}

fn num(s: &str) -> u32 {
    s.trim().trim_end_matches("dpi").split('.').next().unwrap_or("").trim().parse().unwrap_or(0)
}

/// One pass of `scanimage` into `out`: the whole area, or `area`, at `resolution`.
fn sane_pass(s: &Settings, opts: &Options, area: Option<Area>, resolution: u32, depth: u32, out: &Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new("scanimage");
    cmd.args(["-d", &s.device, "--format=tiff"]);
    if let Some(m) = &opts.color_mode {
        cmd.args(["--mode", m]);
    }
    if !s.source.is_empty() && opts.sources.contains(&s.source) {
        cmd.args(["--source", &s.source]);
    }
    if resolution > 0 {
        cmd.args(["--resolution", &resolution.to_string()]);
    }
    if depth > 0 && opts.depths.contains(&depth) {
        cmd.args(["--depth", &depth.to_string()]);
    }
    if s.linear
        && let Some((name, value)) = &opts.linear
    {
        cmd.arg(format!("--{name}={value}"));
    }
    if let Some([l, t, w, h]) = area {
        cmd.args(["-l", &format!("{l:.1}"), "-t", &format!("{t:.1}"), "-x", &format!("{w:.1}"), "-y", &format!("{h:.1}")]);
    }
    cmd.arg(format!("--output-file={}", out.display()));
    let r = cmd.output().map_err(|e| format!("scanimage: {e}"))?;
    if !r.status.success() || !out.exists() {
        let _ = std::fs::remove_file(out);
        let err = String::from_utf8_lossy(&r.stderr).trim().to_string();
        return Err(if err.is_empty() { "the scanner stopped".into() } else { err });
    }
    Ok(())
}

/// Scan each area into `dir` as `scan-0001.tif` and on, with `s.passes` passes each, averaged. `cancel` stops
/// before the next pass.
fn sane_frames(dir: &Path, s: &Settings, areas: &[Area], cancel: &AtomicBool, say: &dyn Fn(Event)) -> Result<usize, String> {
    if s.device.is_empty() {
        return Err("choose a scanner first".into());
    }
    let opts = sane_options(&s.device, &s.source)?;
    let passes = s.passes.clamp(1, 16);
    let mut saved = 0;
    for (i, area) in areas.iter().enumerate() {
        let dest = next_name(dir);
        let name = dest.file_name().unwrap_or_default().to_string_lossy().to_string();
        let (part, tmp) = (dir.join(format!(".{name}.part")), dir.join(format!(".{name}.pass")));
        let what = if areas.len() > 1 { format!("frame {} of {}", i + 1, areas.len()) } else { "the frame".into() };
        let r = (|| {
            let mut sum: Option<merge::Sum> = None;
            for p in 0..passes {
                if cancel.load(Ordering::Relaxed) {
                    return Err("stopped".to_string());
                }
                say(Event::Status(if passes > 1 { format!("Scanning {what}, pass {} of {passes}, at {} ppi…", p + 1, s.resolution) } else { format!("Scanning {what} at {} ppi…", s.resolution) }));
                sane_pass(s, &opts, Some(*area), s.resolution, s.depth, &tmp)?;
                // One pass in memory at a time: added to the running sum, then dropped.
                let (w, h, samples, px) = crate::scanfile::read_u16(&tmp, 0).ok_or("couldn't read what the scanner sent")?;
                match &mut sum {
                    None => sum = Some(merge::Sum::new(w, h, samples, &px)),
                    Some(m) => {
                        m.add(w, h, samples, &px);
                    }
                }
            }
            let sum = sum.ok_or("no pass came in")?;
            let (w, h, samples) = (sum.w, sum.h, sum.samples);
            write_scan(&part, w, h, samples, &sum.average(), s.resolution as f32, passes, s.linear && opts.linear.is_some(), &s.device_name)
        })();
        let _ = std::fs::remove_file(&tmp);
        if let Err(e) = r {
            let _ = std::fs::remove_file(&part);
            return if saved > 0 { Err(format!("{e} (after {saved} saved)")) } else { Err(e) };
        }
        std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
        saved += 1;
        say(Event::Saved(dest));
    }
    Ok(saved)
}

/// A 16-bit TIFF of a scan that says what it is: passes, linear, the scanner.
#[allow(clippy::too_many_arguments)]
fn write_scan(out: &Path, w: u32, h: u32, samples: usize, px: &[u16], dpi: f32, passes: u32, linear: bool, device: &str) -> Result<(), String> {
    let mut sw = format!("Ferrotype {}", env!("CARGO_PKG_VERSION"));
    if passes > 1 {
        sw += &format!(", {passes} passes");
    }
    if linear {
        sw += ", linear";
    }
    let (make, model) = device.split_once(' ').map_or((String::new(), device.to_string()), |(a, b)| (a.to_string(), b.to_string()));
    let info = crate::scanfile::Info { software: sw, make, model, dpi, ..Default::default() };
    crate::scanfile::write_tiff(out, w, h, samples, 16, px, None, &info)
}

/// A quick low-resolution scan of the whole area, for picking frames: saved at `out`, with the area's size.
pub fn preview(s: &Settings, out: &Path) -> Result<[f32; 2], String> {
    if s.device.is_empty() {
        return Err("choose a scanner first".into());
    }
    let opts = sane_options(&s.device, &s.source)?;
    let bed = opts.bed_mm.ok_or("the scanner doesn't say how big its scan area is")?;
    let res = opts.resolutions.iter().copied().filter(|r| *r >= 75).min().unwrap_or(150).clamp(75, 300);
    let res = if opts.resolutions.is_empty() || opts.resolutions.contains(&res) { res } else { opts.resolutions[0] };
    let part = out.with_extension("part");
    sane_pass(s, &opts, None, res, 16, &part)?;
    std::fs::rename(&part, out).map_err(|e| e.to_string())?;
    Ok(bed)
}

/// Scan these frames into `dir` on a thread of its own; `cancel` stops before the next pass.
pub fn scan_frames(dir: PathBuf, settings: Settings, areas: Vec<Area>, cancel: std::sync::Arc<AtomicBool>, tx: Sender<Event>, wake: impl Fn() + Send + 'static) {
    run(tx, wake, move |say| sane_frames(&dir, &settings, &areas, &cancel, say));
}

/// A frame found on the preview (left, top, right, bottom trimmed, as fractions) as an area on the scanner,
/// with a little room around it so the film edge is kept for the base.
pub fn area_of(crop: [f32; 4], bed: [f32; 2], margin_mm: f32) -> Area {
    let [l, t, r, b] = crop;
    let (x0, y0) = ((l * bed[0] - margin_mm).max(0.0), (t * bed[1] - margin_mm).max(0.0));
    let (x1, y1) = (((1.0 - r) * bed[0] + margin_mm).min(bed[0]), ((1.0 - b) * bed[1] + margin_mm).min(bed[1]));
    [x0, y0, (x1 - x0).max(1.0), (y1 - y0).max(1.0)]
}

// ---------- scanning apps ----------

/// Scanning apps found on this computer: (name, how to start it).
pub fn apps() -> Vec<(&'static str, PathBuf)> {
    let mut out = vec![];
    #[cfg(windows)]
    {
        let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
        let known: [(&str, &[&str]); 2] = [
            ("Epson Scan 2", &["EPSON/Epson Scan 2/Core/es2launcher.exe", "epson/Epson Scan 2/Core/es2launcher.exe"]),
            ("VueScan", &["VueScan/vuescan.exe"]),
        ];
        for (name, rel) in known {
            if let Some(p) = roots.iter().flat_map(|r| rel.iter().map(move |x| r.join(x))).find(|p| p.is_file()) {
                out.push((name, p));
            }
        }
        // SilverFast installs as "SilverFast 9", "SilverFast Application/SilverFast 8", …
        if let Some(p) = roots.iter().find_map(|r| find_exe(r, "silverfast", 2)) {
            out.push(("SilverFast", p));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let apps = Path::new("/Applications");
        for (name, rel) in [("Epson Scan 2", "Epson Software/Epson Scan 2.app"), ("VueScan", "VueScan.app")] {
            if apps.join(rel).exists() {
                out.push((name, apps.join(rel)));
            }
        }
        if let Some(p) = std::fs::read_dir(apps).into_iter().flatten().flatten().map(|e| e.path())
            .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("SilverFast") && n.ends_with(".app")))
        {
            out.push(("SilverFast", p));
        }
        let ic = Path::new("/System/Applications/Image Capture.app");
        if ic.exists() {
            out.push(("Image Capture", ic.to_path_buf()));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for (name, prog) in [("Epson Scan 2", "epsonscan2"), ("VueScan", "vuescan"), ("XSane", "xsane"), ("Image Scan!", "iscan"), ("Document Scanner", "simple-scan")] {
            if let Some(p) = which(prog) {
                out.push((name, p));
            }
        }
    }
    out
}

#[cfg(windows)]
fn find_exe(dir: &Path, prefix: &str, depth: u32) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let name = p.file_name()?.to_string_lossy().to_ascii_lowercase();
        if p.is_file() && name.starts_with(prefix) && name.ends_with(".exe") {
            return Some(p);
        }
        if p.is_dir()
            && depth > 0
            && (name.starts_with(prefix) || depth == 2 && name.contains("lasersoft"))
            && let Some(f) = find_exe(&p, prefix, depth - 1)
        {
            return Some(f);
        }
    }
    None
}

/// Start a scanning app.
pub fn launch(app: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-a").arg(app).spawn();
    #[cfg(not(target_os = "macos"))]
    let r = std::process::Command::new(app).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_scanimage_listings() {
        let d = parse_devices("epkowa:interpreter:001:004\tEpson Perfection V600\ntest:0\tNoname frontend-tester\n\n");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], Device { id: "epkowa:interpreter:001:004".into(), name: "Epson Perfection V600".into() });

        let a = "All options specific to device `epkowa:interpreter:001:004':\n  Scan Mode:\n    --mode Binary|Gray|Color [Color]\n        Selects the scan mode.\n    --depth 8|16 [8]\n    --source Flatbed|Transparency Unit [Flatbed]\n    --resolution 50..6400dpi (in steps of 1) [300]\n";
        let o = parse_options(a);
        assert_eq!(o.sources, vec!["Flatbed", "Transparency Unit"]);
        assert_eq!(o.depths, vec![8, 16]);
        assert_eq!(o.color_mode.as_deref(), Some("Color"));
        assert_eq!(o.resolutions, vec![150, 300, 600, 1200, 2400, 3200, 4800, 6400]);
        let o = parse_options("    --resolution 75|150|300|600|1200dpi [75]\n    --mode Lineart|Gray|Colour [Gray]\n");
        assert_eq!(o.resolutions, vec![75, 150, 300, 600, 1200]);
        assert_eq!(o.color_mode.as_deref(), Some("Colour"));
        let o = parse_options("    --gamma-correction User defined (Gamma=1.0)|User defined (Gamma=1.8)|Standard [Standard]\n    -l 0..152.4mm [0]\n    -t 0..228.6mm [0]\n    -x 0..152.4mm [152.4]\n    -y 0..228.6mm [228.6]\n");
        assert_eq!(o.bed_mm, Some([152.4, 228.6]));
        assert_eq!(o.linear, Some(("gamma-correction".into(), "User defined (Gamma=1.0)".into())));
    }

    #[test]
    fn preview_frames_become_scanner_areas() {
        // A frame in the middle half of a 200 × 100 mm area, with 1 mm to spare around it.
        let a = area_of([0.25, 0.25, 0.25, 0.25], [200.0, 100.0], 1.0);
        assert_eq!(a, [49.0, 24.0, 102.0, 52.0]);
        // Never outside the area.
        assert_eq!(area_of([0.0, 0.0, 0.0, 0.0], [200.0, 100.0], 2.0), [0.0, 0.0, 200.0, 100.0]);
    }

    #[test]
    fn next_name_skips_taken_ones() {
        let d = std::env::temp_dir().join(format!("ferro-scanner-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(next_name(&d), d.join("scan-0001.tif"));
        for n in [1, 2, 4] {
            std::fs::write(d.join(format!("scan-{n:04}.tif")), b"x").unwrap();
        }
        // Frame 3 was rejected: the next scan comes after 4, never into the gap.
        assert_eq!(next_name(&d), d.join("scan-0005.tif"));
        std::fs::create_dir_all(d.join("_rejects")).unwrap();
        std::fs::rename(d.join("scan-0004.tif"), d.join("_rejects/scan-0004.tif")).unwrap();
        assert_eq!(next_name(&d), d.join("scan-0005.tif"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn strips_from_the_scanner_are_clipped_to_the_picture() {
        // A 4 × 3 grey picture; a 16-bit strip of 2 rows, 4 bytes apart, at (1, 1).
        let mut px = vec![0u16; 12];
        let buf = [1, 0, 2, 0, 3, 0, 4, 0];
        copy_strip(&mut px, 4, 3, 1, 2, &buf, 4, 2, 2, 1, 1);
        assert_eq!(px, [0, 0, 0, 0, 0, 1, 2, 0, 0, 3, 4, 0]);
        // Rows or columns past the picture, an offset past its width or one left unset: nothing breaks.
        copy_strip(&mut px, 4, 3, 1, 2, &buf, 4, 8, 2, 3, 2);
        assert_eq!(px[11], 1);
        copy_strip(&mut px, 4, 3, 1, 2, &buf, 4, 2, 2, 9, 0);
        copy_strip(&mut px, 4, 3, 1, 2, &buf, 4, 2, 2, u32::MAX as usize, 0);
        copy_strip(&mut px, 4, 3, 1, 2, &buf, u32::MAX as usize, 2, 2, 0, 0);
        // 8-bit samples become 16-bit.
        let mut px8 = vec![0u16; 2];
        copy_strip(&mut px8, 2, 1, 1, 1, &[255, 1], 2, 2, 1, 0, 0);
        assert_eq!(px8, [65535, 257]);
    }
}

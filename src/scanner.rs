//! Scanning from inside Ferrotype, into the open folder.
//!
//! - Windows: TWAIN. The scanner's own window opens (Epson Scan 2 for a Perfection V600 or V850, with its film
//!   holders, Digital ICE and frame detection), and each picture it scans arrives here as a 16-bit TIFF.
//! - Linux: SANE through `scanimage` (sane-utils), with a choice of source, resolution and bit depth.
//! - Everywhere: the scanning apps people already use (Epson Scan 2, VueScan, SilverFast) can be opened from the
//!   Scan tab, saving into the folder Ferrotype watches.
//!
//! A scan runs on its own thread and reports back through `Event`s.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

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

/// What to scan with on Linux, and how.
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

/// Whether this build can drive a scanner itself (rather than only open a scanning app).
pub fn can_scan() -> bool {
    cfg!(windows) || (cfg!(target_os = "linux") && which("scanimage").is_some())
}

/// The next free `scan-0001.tif` in `dir`.
pub fn next_name(dir: &Path) -> PathBuf {
    (1..100_000).map(|n| dir.join(format!("scan-{n:04}.tif"))).find(|p| !p.exists()).unwrap_or_else(|| dir.join("scan.tif"))
}

/// Scan into `dir` on a thread of its own. On Windows the scanner's own window opens; `source` is the TWAIN
/// scanner to use ("" = ask). On Linux, `settings` choose the scanner and how to scan.
pub fn scan(dir: PathBuf, source: String, settings: Settings, tx: Sender<Event>, wake: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let say = |e: Event| {
            let _ = tx.send(e);
            wake();
        };
        #[cfg(windows)]
        let done = {
            let _ = &settings;
            twain::scan(&dir, &source, &say)
        };
        #[cfg(not(windows))]
        let done = {
            let _ = &source;
            sane_scan(&dir, &settings, &say)
        };
        say(Event::Done(done));
    });
}

/// Let the user pick a TWAIN scanner (the system's list); returns its name.
#[cfg(windows)]
pub fn choose_source() -> Result<String, String> {
    twain::choose()
}

// ---------- SANE ----------

fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(prog)).find(|p| p.is_file())
}

/// Scanners SANE finds (slow: it asks every backend, so call it off the UI thread).
pub fn sane_devices() -> Result<Vec<Device>, String> {
    let out = std::process::Command::new("scanimage").args(["-f", "%d\t%v %m%n"]).output().map_err(|e| format!("scanimage: {e} (install sane-utils)"))?;
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

/// What `device` can do.
pub fn sane_options(device: &str) -> Result<Options, String> {
    let out = std::process::Command::new("scanimage").args(["-d", device, "-A"]).output().map_err(|e| format!("scanimage: {e}"))?;
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
#[cfg(not(windows))]
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

#[cfg(not(windows))]
fn sane_scan(dir: &Path, s: &Settings, say: &dyn Fn(Event)) -> Result<usize, String> {
    sane_frames(dir, s, &[], say)
}

/// Scan each area (all of the scanner's area when `areas` is empty) into `dir` as `scan-0001.tif` and so on,
/// with `s.passes` passes each, averaged.
#[cfg(not(windows))]
fn sane_frames(dir: &Path, s: &Settings, areas: &[Area], say: &dyn Fn(Event)) -> Result<usize, String> {
    if s.device.is_empty() {
        return Err("choose a scanner first".into());
    }
    let opts = sane_options_for(&s.device, &s.source)?;
    let all = [None];
    let list: Vec<Option<Area>> = if areas.is_empty() { all.to_vec() } else { areas.iter().copied().map(Some).collect() };
    let passes = s.passes.clamp(1, 16);
    let mut saved = 0;
    for (i, area) in list.iter().enumerate() {
        let dest = next_name(dir);
        let part = dir.join(format!(".{}.part", dest.file_name().unwrap_or_default().to_string_lossy()));
        let what = if list.len() > 1 { format!("frame {} of {}", i + 1, list.len()) } else { "the scan".into() };
        let mut done = vec![];
        for p in 0..passes {
            say(Event::Status(if passes > 1 { format!("Scanning {what}, pass {} of {passes}, at {} ppi…", p + 1, s.resolution) } else { format!("Scanning {what} at {} ppi…", s.resolution) }));
            let tmp = dir.join(format!(".{}.pass{p}", dest.file_name().unwrap_or_default().to_string_lossy()));
            sane_pass(s, &opts, *area, s.resolution, s.depth, &tmp)?;
            done.push(tmp);
        }
        let r = finish(&done, &part, s.resolution as f32, &opts, s);
        done.iter().for_each(|f| {
            let _ = std::fs::remove_file(f);
        });
        r?;
        std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
        saved += 1;
        say(Event::Saved(dest));
    }
    Ok(saved)
}

/// Average the passes (or keep the one) into a 16-bit TIFF that says what it is.
#[cfg(not(windows))]
fn finish(passes: &[PathBuf], out: &Path, dpi: f32, opts: &Options, s: &Settings) -> Result<(), String> {
    let read: Vec<merge::Pass> = passes.iter().filter_map(|p| crate::scanfile::read_samples(p, 0)).collect();
    if read.is_empty() {
        return Err("couldn't read what the scanner sent".into());
    }
    let (w, h, samples, data) = merge::average(&read).ok_or("couldn't combine the passes")?;
    let px: Vec<u16> = data.iter().map(|&v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect();
    let linear = s.linear && opts.linear.is_some();
    let mut sw = format!("Ferrotype {}", env!("CARGO_PKG_VERSION"));
    if read.len() > 1 {
        sw += &format!(", {} passes", read.len());
    }
    if linear {
        sw += ", linear";
    }
    let (make, model) = s.device_name.split_once(' ').map_or((String::new(), s.device_name.clone()), |(a, b)| (a.to_string(), b.to_string()));
    let info = crate::scanfile::Info { software: sw, make, model, dpi, ..Default::default() };
    crate::scanfile::write_tiff(out, w, h, samples, 16, &px, None, &info)
}

/// `sane_options` with `source` chosen (a transparency unit has a smaller area than the glass).
#[cfg(not(windows))]
fn sane_options_for(device: &str, source: &str) -> Result<Options, String> {
    let mut cmd = std::process::Command::new("scanimage");
    cmd.args(["-d", device]);
    if !source.is_empty() {
        cmd.args(["--source", source]);
    }
    let out = cmd.arg("-A").output().map_err(|e| format!("scanimage: {e}"))?;
    if !out.status.success() {
        return sane_options(device);
    }
    Ok(parse_options(&String::from_utf8_lossy(&out.stdout)))
}

/// A quick low-resolution scan of the whole area, for picking frames: saved at `out`, with the area's size.
pub fn preview(s: &Settings, out: &Path) -> Result<[f32; 2], String> {
    #[cfg(windows)]
    {
        let _ = (s, out);
        Err("preview scans come with the next step on Windows".into())
    }
    #[cfg(not(windows))]
    {
        if s.device.is_empty() {
            return Err("choose a scanner first".into());
        }
        let opts = sane_options_for(&s.device, &s.source)?;
        let bed = opts.bed_mm.ok_or("the scanner doesn't say how big its scan area is")?;
        let res = opts.resolutions.iter().copied().filter(|r| *r >= 75).min().unwrap_or(150).max(75).min(300);
        let res = if opts.resolutions.is_empty() || opts.resolutions.contains(&res) { res } else { opts.resolutions[0] };
        let part = out.with_extension("part");
        let quick = Settings { linear: s.linear, ..s.clone() };
        sane_pass(&quick, &opts, None, res, 16, &part)?;
        std::fs::rename(&part, out).map_err(|e| e.to_string())?;
        Ok(bed)
    }
}

/// Scan these frames (from a preview) into `dir`, on a thread of its own.
pub fn scan_frames(dir: PathBuf, settings: Settings, areas: Vec<Area>, tx: Sender<Event>, wake: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let say = |e: Event| {
            let _ = tx.send(e);
            wake();
        };
        #[cfg(windows)]
        let done: Result<usize, String> = {
            let _ = (&dir, &settings, &areas);
            Err("scanning chosen frames comes with the next step on Windows".into())
        };
        #[cfg(not(windows))]
        let done = sane_frames(&dir, &settings, &areas, &say);
        say(Event::Done(done));
    });
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
        std::fs::write(d.join("scan-0001.tif"), b"x").unwrap();
        assert_eq!(next_name(&d), d.join("scan-0002.tif"));
        let _ = std::fs::remove_dir_all(&d);
    }
}

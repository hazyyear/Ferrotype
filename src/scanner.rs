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
}

/// What to scan with on Linux.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub device: String,
    pub source: String,
    pub resolution: u32,
    pub depth: u32,
}

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
    Options {
        sources: list("source"),
        resolutions: numbers("resolution"),
        depths: numbers("depth"),
        color_mode: modes.iter().find(|m| m.to_ascii_lowercase().contains("colo")).cloned(),
    }
}

fn num(s: &str) -> u32 {
    s.trim().trim_end_matches("dpi").split('.').next().unwrap_or("").trim().parse().unwrap_or(0)
}

#[cfg(not(windows))]
fn sane_scan(dir: &Path, s: &Settings, say: &dyn Fn(Event)) -> Result<usize, String> {
    if s.device.is_empty() {
        return Err("choose a scanner first".into());
    }
    let opts = sane_options(&s.device)?;
    let dest = next_name(dir);
    let mut cmd = std::process::Command::new("scanimage");
    cmd.args(["-d", &s.device, "--format=tiff"]);
    if let Some(m) = &opts.color_mode {
        cmd.args(["--mode", m]);
    }
    if !s.source.is_empty() && opts.sources.contains(&s.source) {
        cmd.args(["--source", &s.source]);
    }
    if s.resolution > 0 {
        cmd.args(["--resolution", &s.resolution.to_string()]);
    }
    if s.depth > 0 && opts.depths.contains(&s.depth) {
        cmd.args(["--depth", &s.depth.to_string()]);
    }
    // Into a hidden part file, renamed when complete, so the folder watch never sees half a scan.
    let part = dir.join(format!(".{}.part", dest.file_name().unwrap_or_default().to_string_lossy()));
    cmd.arg(format!("--output-file={}", part.display()));
    say(Event::Status(format!("Scanning at {} ppi…", s.resolution)));
    let out = cmd.output().map_err(|e| format!("scanimage: {e}"))?;
    if !out.status.success() || !part.exists() {
        let _ = std::fs::remove_file(&part);
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { "the scanner stopped".into() } else { err });
    }
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    say(Event::Saved(dest));
    Ok(1)
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

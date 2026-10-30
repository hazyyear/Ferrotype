//! Scanning straight over USB, the way VueScan and SilverFast do, through the epscan library: no Epson software in
//! between. Today that is the Epson Perfection V800/V850 (V700/V750 provisionally). The V500, V550 and V600 only
//! speak through Epson's closed "interpreter" and are listed but can't scan this way.
//!
//! Every scan is saved as a 16-bit TIFF in the roll folder. With "Linear data" the scanner gets neutral tone
//! tables, so the file holds the light it measured (what negative conversion wants) and says so in its Software
//! tag. With infrared on, the separate infrared pass is put in as a fourth sample (RGBI), which Infrared clean
//! reads like a VueScan or SilverFast raw scan.

use super::Event;
use epscan::{Backend, Gamma, ScanMode, ScanOptions, ScanSettings, Session, Source};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub use epscan::capabilities::{HolderLayout, ScannerModel};

/// How long a command may take before the scanner counts as gone (lamp warm-up included).
const TIMEOUT: Duration = Duration::from_secs(90);

/// A scanner found on USB.
#[derive(Clone, Debug)]
pub struct Found {
    pub location: String,
    pub name: String,
    /// Why it can't be used directly, if it can't.
    pub blocked: Option<String>,
}

/// Epson scanners on USB that epscan knows.
pub fn devices() -> Result<Vec<Found>, String> {
    if demo() {
        return Ok(vec![Found { location: "demo".into(), name: "Perfection V800 (demo, no scans)".into(), blocked: None }]);
    }
    let list = epscan::list_devices(Backend::Auto).map_err(|e| e.to_string())?;
    Ok(list.into_iter().map(|d| {
        let blocked = epscan::capabilities::require_native_protocol(d.vid, d.pid).err().map(|_| {
            format!("{} only takes commands through Epson's own driver: scan it with the Scan button in Light table instead", d.name)
        });
        Found { location: d.location, name: d.name, blocked }
    }).collect())
}

/// What a connected scanner can do: its model profile (holders, modes) and its areas.
#[derive(Clone)]
pub struct Scanner {
    pub location: String,
    pub name: String,
    pub model: &'static ScannerModel,
    pub max_dpi: u32,
    pub infrared: bool,
    /// Scan area of each source in millimetres: film holder, film on glass, flatbed.
    pub areas: [(Source, [f64; 2]); 3],
}

impl Scanner {
    fn of(session: &Session, location: &str) -> Result<Scanner, String> {
        let caps = &session.capabilities;
        let model = caps.scanner_model().map_err(|e| e.to_string())?;
        Ok(Scanner {
            location: location.into(),
            name: model.name.into(),
            model,
            max_dpi: caps.max_dpi,
            infrared: model.infrared_mode.is_some() && caps.infrared_advertised,
            areas: [Source::Transparency, Source::Transparency8x10, Source::Flatbed].map(|s| (s, caps.area_mm(s))),
        })
    }

    pub fn area(&self, source: Source) -> [f64; 2] {
        self.areas.iter().find(|(s, _)| *s == source).map_or([0.0; 2], |a| a.1)
    }

    /// The resolutions to offer, up to what the scanner takes.
    pub fn resolutions(&self) -> Vec<u32> {
        [300, 600, 1200, 1600, 2400, 3200, 4800, 6400].into_iter().filter(|r| *r <= self.max_dpi.max(300)).collect()
    }
}

/// Connect, ask the scanner what it is, and let it go again.
pub fn identify(location: &str) -> Result<Scanner, String> {
    if location == "demo" && demo() {
        return demo_session().and_then(|s| Scanner::of(&s, location));
    }
    let mut s = Session::connect(Some(location), Backend::Auto, TIMEOUT).map_err(|e| e.to_string())?;
    let r = Scanner::of(&s, location);
    s.close();
    r
}

/// One frame to scan.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub source: Source,
    /// Left, top, width, height in millimetres on that source.
    pub rect_mm: [f64; 4],
    pub dpi: u32,
    /// Rows read per output row and averaged (the scanner's own multi-sampling along the carriage).
    pub oversample: u32,
    pub linear: bool,
    pub infrared: bool,
}

impl Job {
    fn settings(&self, preview: bool, depth: u8, dpi: u32) -> ScanSettings {
        ScanSettings {
            rect_mm: self.rect_mm,
            dpi,
            y_oversampling: if preview { 1 } else { self.oversample.max(1) },
            depth,
            mode: ScanMode::Rgb,
            source: self.source,
            gamma: if self.linear { Gamma::IdentityLut } else { Gamma::DeviceDefault },
            preview,
        }
    }
}

/// Scan one job on an open session into `out` (a 16-bit TIFF, RGBI with infrared). Work files go in `tmp`.
#[allow(clippy::too_many_arguments)]
pub fn capture(session: &mut Session, job: &Job, preview: bool, out: &Path, tmp: &Path, settle: Duration, cancel: &AtomicBool, say: &dyn Fn(String), progress: &dyn Fn(f32)) -> Result<(), String> {
    let model = session.capabilities.scanner_model().map_err(|e| e.to_string())?;
    let (depth, dpi) = if preview { (model.preview_depth, model.preview_dpi) } else { (16, job.dpi) };
    let settings = job.settings(preview, depth, dpi);
    let options = ScanOptions {
        infrared: job.infrared && !preview,
        ir_depth: 8,
        ir_gamma: if job.linear { Gamma::IdentityLut } else { Gamma::DeviceDefault },
        film: "negative".into(),
        settle_time: settle,
        ..Default::default()
    };
    std::fs::create_dir_all(tmp).map_err(|e| e.to_string())?;
    let mut last = String::new();
    let mut report = |p: epscan::Progress<'_>| {
        if p.total > 0 {
            progress(p.done as f32 / p.total as f32);
        }
        let line = phase(p.phase);
        if line != last {
            say(line.clone());
            last = line;
        }
        true
    };
    let result = session.scan(&settings, &options, &tmp.join("pass"), cancel, &mut report).map_err(|e| e.to_string())?;
    let rgb = result.rgb.as_ref().and_then(|r| r.tiff.clone()).ok_or("the scanner sent no picture")?;
    // 16-bit throughout, and the infrared put in place: a medium-format frame at 6400 ppi is 290 million pixels.
    let (w, h, samples, mut px) = crate::scanfile::read_u16(&rgb, 0).ok_or("couldn't read the scan")?;
    let ir = result.ir.as_ref().and_then(|r| r.tiff.as_deref()).and_then(|p| crate::scanfile::read_u16(p, 0));
    let samples = match ir {
        Some((iw, ih, 1, ird)) if (iw, ih) == (w, h) => {
            with_infrared(&mut px, samples, &ird);
            samples + 1
        }
        Some(_) => {
            say("The infrared pass didn't line up with the picture: saved without it".into());
            samples
        }
        None => samples,
    };
    let mut sw = format!("Ferrotype {} (direct USB)", env!("CARGO_PKG_VERSION"));
    if job.oversample > 1 && !preview {
        sw += &format!(", {}x sampled", job.oversample);
    }
    if job.linear {
        sw += ", linear";
    }
    let info = crate::scanfile::Info { software: sw, make: "EPSON".into(), model: model.name.into(), dpi: dpi as f32, ..Default::default() };
    crate::scanfile::write_tiff(out, w, h, samples, 16, &px, None, &info)
}

/// Put an infrared sample after each pixel's colours, in place: `px` grows from `samples` to `samples + 1`
/// per pixel, filled from the end so nothing is overwritten before it moves.
pub fn with_infrared(px: &mut Vec<u16>, samples: usize, ir: &[u16]) {
    let n = ir.len().min(px.len() / samples);
    px.truncate(n * samples);
    px.resize(n * (samples + 1), 0);
    for i in (0..n).rev() {
        for k in (0..samples).rev() {
            px[i * (samples + 1) + k] = px[i * samples + k];
        }
        px[i * (samples + 1) + samples] = ir[i];
    }
}

fn phase(p: &str) -> String {
    let p = p.replace(['_', '-'], " ");
    let mut c = p.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// A quick scan of a whole source, for placing frames: saved at `out`.
pub fn preview(location: &str, source: Source, linear: bool, out: &Path, cancel: &AtomicBool, say: &dyn Fn(String), progress: &dyn Fn(f32)) -> Result<Scanner, String> {
    if location == "demo" {
        return Err("the demo scanner can't scan".into());
    }
    let mut s = Session::connect(Some(location), Backend::Auto, TIMEOUT).map_err(|e| e.to_string())?;
    let scanner = Scanner::of(&s, location)?;
    let [w, h] = scanner.area(source);
    let job = Job { source, rect_mm: [0.0, 0.0, w, h], dpi: 0, oversample: 1, linear, infrared: false };
    let tmp = out.with_extension("work");
    let r = capture(&mut s, &job, true, out, &tmp, Duration::ZERO, cancel, say, progress);
    s.close();
    let _ = std::fs::remove_dir_all(&tmp);
    r.map(|()| scanner)
}

/// Scan each job into `dir` as `scan-0001.tif` and on, on one connection.
pub fn frames(location: &str, jobs: &[Job], dir: &Path, cancel: &AtomicBool, say: &dyn Fn(Event)) -> Result<usize, String> {
    if location == "demo" {
        return Err("the demo scanner can't scan".into());
    }
    let mut s = Session::connect(Some(location), Backend::Auto, TIMEOUT).map_err(|e| e.to_string())?;
    let r = frames_on(&mut s, jobs, dir, Duration::from_secs(10), cancel, say);
    s.close();
    r
}

fn frames_on(s: &mut Session, jobs: &[Job], dir: &Path, settle: Duration, cancel: &AtomicBool, say: &dyn Fn(Event)) -> Result<usize, String> {
    let mut saved = 0;
    for (i, job) in jobs.iter().enumerate() {
        let dest = super::next_name(dir);
        let name = dest.file_name().unwrap_or_default().to_string_lossy().to_string();
        let part = dir.join(format!(".{name}.part"));
        let tmp = dir.join(format!(".{name}.work"));
        let what = if jobs.len() > 1 { format!("Frame {} of {}", i + 1, jobs.len()) } else { "Scan".into() };
        let all = jobs.len() as f32;
        let r = capture(s, job, false, &part, &tmp, settle, cancel, &|t| say(Event::Status(format!("{what}: {t}"))), &|f| say(Event::Progress((i as f32 + f) / all)));
        let _ = std::fs::remove_dir_all(&tmp);
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

/// A holder's name without the maker and model it is for ("Epson V800/V850 35 mm Slide Holder" → "35 mm Slide Holder").
fn holder_name(name: &str) -> String {
    let rest = name.strip_prefix("Epson ").unwrap_or(name);
    match rest.split_once(' ') {
        Some((model, tail)) if model.starts_with('V') || model.starts_with("GT") => tail.to_string(),
        _ => rest.to_string(),
    }
}

/// The holder layouts of a model, each with its format variants: (name, layout).
pub fn layouts(model: &ScannerModel) -> Vec<(String, HolderLayout)> {
    let mut out = vec![];
    for h in model.holders {
        if h.formats.is_empty() {
            out.push((holder_name(h.name), *h));
        }
        for f in h.formats {
            if let Ok(l) = h.for_format(Some(f.format)) {
                out.push((format!("{} · {}", holder_name(h.name), f.name), l));
            }
        }
    }
    out
}

/// `FERROTYPE_DEMO_SCANNER=1` lists a pretend V800 (its recorded identity), so the Scanner page can be tried and
/// shown without one. It can't scan.
fn demo() -> bool {
    std::env::var_os("FERROTYPE_DEMO_SCANNER").is_some_and(|v| v == "1")
}

// The V800's extended identity reply (from epscan's test fixture, MIT).
const IDENTITY: &str = include_str!("v800-identity.hex");

fn identity() -> Vec<u8> {
    let hex = IDENTITY.trim();
    (0..hex.len()).step_by(2).filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect()
}

/// A session on a transport that answers the identity questions and nothing more.
fn demo_session() -> Result<Session, String> {
    struct Replay(std::collections::VecDeque<u8>);
    impl epscan::transport::Transport for Replay {
        fn read(&mut self, size: usize, _: Duration) -> epscan::Result<Vec<u8>> {
            let n = size.min(self.0.len());
            Ok(self.0.drain(..n).collect())
        }
        fn write(&mut self, bytes: &[u8], _: Duration) -> epscan::Result<usize> {
            Ok(bytes.len())
        }
    }
    let mut replies = vec![2, 0x12, 2, 0, b'B', b'8'];
    replies.extend(identity());
    let device = epscan::Device { location: "demo".into(), name: "demo".into(), vid: 0x04b8, pid: 0x0151, backend: Backend::Nusb };
    Session::with_transport(device, Box::new(Replay(replies.into())), Duration::from_secs(1)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use epscan::transport::Transport;
    use std::collections::VecDeque;

    /// A V800 as recorded by epscan's author: replies are queued up front.
    struct Sim(VecDeque<u8>);
    impl Transport for Sim {
        fn read(&mut self, size: usize, _: Duration) -> epscan::Result<Vec<u8>> {
            let n = size.min(self.0.len());
            Ok(self.0.drain(..n).collect())
        }
        fn write(&mut self, bytes: &[u8], _: Duration) -> epscan::Result<usize> {
            Ok(bytes.len())
        }
    }

    /// The replies for one RGB pass of `job` whose every sample is `value`.
    fn pass(job: &Job, caps: &epscan::Capabilities, value: u8) -> Vec<u8> {
        let settings = job.settings(false, 16, job.dpi);
        let plan = ScanOptions { settle_time: Duration::ZERO, ..Default::default() }.plan(&settings, caps).unwrap();
        let p = &plan.passes[0];
        let mut r = vec![6; 5];
        r.extend(p.settings.parameters_with_capabilities(caps, false).unwrap());
        r.extend([0; 16]);
        r.extend([2, 0x12]);
        let bytes = p.expected_bytes as u32;
        for word in [0, 0, bytes] {
            r.extend(word.to_le_bytes());
        }
        r.extend(std::iter::repeat_n(value, bytes as usize));
        r.push(0);
        r
    }

    #[test]
    fn frames_scan_into_numbered_16_bit_tiffs() {
        let ext = identity();
        let caps = epscan::Capabilities::parse(&ext).unwrap();
        let job = Job { source: Source::Transparency, rect_mm: [10.0, 20.0, 12.0, 8.0], dpi: 300, oversample: 1, linear: false, infrared: false };
        let mut replies = vec![2, 0x12, 2, 0, b'B', b'8'];
        replies.extend(ext);
        replies.extend(pass(&job, &caps, 0x40));
        replies.extend(pass(&job, &caps, 0x80));
        let device = epscan::Device { location: "sim".into(), name: "simulator".into(), vid: 0x04b8, pid: 0x0151, backend: Backend::Nusb };
        let mut s = Session::with_transport(device, Box::new(Sim(replies.into())), Duration::from_secs(1)).unwrap();
        let scanner = Scanner::of(&s, "sim").unwrap();
        assert!(scanner.area(Source::Transparency)[0] > 100.0);
        assert!(!layouts(scanner.model).is_empty());

        let dir = std::env::temp_dir().join(format!("ferro-direct-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let saved = std::sync::Mutex::new(vec![]);
        let n = frames_on(&mut s, &[job.clone(), job], &dir, Duration::ZERO, &AtomicBool::new(false), &|e| {
            if let Event::Saved(p) = e {
                saved.lock().unwrap().push(p);
            }
        }).unwrap();
        assert_eq!(n, 2);
        let saved = saved.into_inner().unwrap();
        assert_eq!(saved, vec![dir.join("scan-0001.tif"), dir.join("scan-0002.tif")]);
        let info = crate::scanfile::info(&saved[1]).unwrap();
        assert_eq!((info.bits, info.colours, info.extra, info.dpi), (16, 3, 0, 300.0));
        assert!(info.software.contains("direct USB") && info.model.contains("V8"), "{info:?}");
        let (_, _, _, d) = crate::scanfile::read_samples(&saved[1], 0).unwrap();
        assert!((d[0] - 0x8080 as f32 / 65535.0).abs() < 1e-3);
        // No work files left behind.
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 2, "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn infrared_becomes_the_fourth_sample() {
        let mut px = vec![1, 2, 3, 4, 5, 6];
        with_infrared(&mut px, 3, &[9, 8]);
        assert_eq!(px, vec![1, 2, 3, 9, 4, 5, 6, 8]);
    }
}

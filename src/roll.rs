//! Roll notes: what a film roll was shot on and how it was developed, kept once per folder.
//!
//! Film scans carry no camera data, so this is where it comes from: camera, lens, film stock, box speed and the
//! speed it was exposed at (push or pull), developer, date and notes for the roll, plus aperture, shutter, focal
//! length and a note for single frames. Stored as `<folder>/.ferrotype-roll.json`. Export uses it for file names
//! and metadata, and the film stock and push set up the negative conversion.

use crate::develop::Params;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE: &str = ".ferrotype-roll.json";

#[derive(Clone, PartialEq, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Roll {
    /// Roll number or name ("12", "Lisbon 2").
    pub roll: String,
    pub camera: String,
    pub lens: String,
    /// A film stock id from `negative::STOCKS` (or a name, for a stock Ferrotype doesn't know).
    pub stock: String,
    /// Box speed of the film.
    pub iso: f32,
    /// The speed it was exposed at; 0 = box speed. 1600 on a 400 film is a push of two stops.
    pub ei: f32,
    pub developer: String,
    /// When it was shot, YYYY-MM-DD.
    pub date: String,
    pub notes: String,
    /// Per frame, by file name.
    pub frames: BTreeMap<String, Frame>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Frame {
    /// f-number, 0 = not noted.
    pub aperture: f32,
    /// As written on the camera: "1/125", "2", "B".
    pub shutter: String,
    /// Millimetres, 0 = the roll's lens / not noted.
    pub focal: f32,
    pub note: String,
}

impl Frame {
    pub fn is_empty(&self) -> bool {
        *self == Frame::default()
    }

    /// Shutter time in seconds ("1/125" → 0.008, "2" → 2, "2s" → 2), 0 when unknown.
    pub fn shutter_secs(&self) -> f32 {
        let s = self.shutter.trim().trim_end_matches(['s', '"']).trim();
        match s.split_once('/') {
            Some((a, b)) => a.trim().parse::<f32>().ok().zip(b.trim().parse::<f32>().ok()).filter(|(_, b)| *b > 0.0).map_or(0.0, |(a, b)| a / b),
            None => s.parse().unwrap_or(0.0),
        }
    }
}

impl Roll {
    pub fn is_empty(&self) -> bool {
        *self == Roll::default()
    }

    /// Stops pushed (positive) or pulled (negative): log2(exposed at / box speed). 0 when either is unknown.
    pub fn push_stops(&self) -> f32 {
        if self.iso > 0.0 && self.ei > 0.0 { (self.ei / self.iso).log2() } else { 0.0 }
    }

    /// The speed the film was exposed at (box speed if no EI is noted).
    pub fn speed(&self) -> f32 {
        if self.ei > 0.0 { self.ei } else { self.iso }
    }

    /// "+1 push", "−1 pull" or "" (rounded to a third of a stop).
    pub fn push_label(&self) -> String {
        let s = (self.push_stops() * 3.0).round() / 3.0;
        if s.abs() < 0.1 {
            return String::new();
        }
        let n = if (s - s.round()).abs() < 0.05 { format!("{}", s.abs().round()) } else { format!("{:.1}", s.abs()) };
        if s > 0.0 { format!("+{n} push") } else { format!("−{n} pull") }
    }

    /// The film stock's display name: a known stock's full name, else what was typed.
    pub fn stock_name(&self) -> String {
        crate::negative::STOCKS.iter().find(|s| s.id == self.stock).map_or_else(|| self.stock.trim().to_string(), |s| format!("{} {}", s.brand, s.name))
    }

    /// One line for metadata and the info panel: "Roll 12 · Kodak Tri-X 400 at 1600 (+2 push) · D-76".
    pub fn summary(&self) -> String {
        let mut parts = vec![];
        if !self.roll.trim().is_empty() {
            parts.push(format!("Roll {}", self.roll.trim()));
        }
        let mut film = self.stock_name();
        if self.ei > 0.0 && self.iso > 0.0 && (self.ei - self.iso).abs() > 0.5 {
            film = format!("{film} at {}", self.ei.round());
            let p = self.push_label();
            if !p.is_empty() {
                film = format!("{film} ({p})");
            }
        }
        if !film.trim().is_empty() {
            parts.push(film);
        }
        if !self.developer.trim().is_empty() {
            parts.push(self.developer.trim().to_string());
        }
        parts.join(" · ")
    }
}

pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

pub fn read(dir: &Path) -> Option<Roll> {
    serde_json::from_str(&std::fs::read_to_string(path_in(dir)).ok()?).ok()
}

/// Save the roll notes; an empty roll removes the file.
pub fn write(dir: &Path, r: &Roll) -> Result<(), String> {
    let path = path_in(dir);
    if r.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    let text = serde_json::to_string_pretty(r).map_err(|e| e.to_string())?;
    crate::write_atomic(&path, text.as_bytes()).map_err(|e| e.to_string())
}

/// The key a frame is stored under: the photo's own file name. A virtual copy (`scan.tif.copy2`) has its own key,
/// because the frames split from one strip scan are virtual copies of it and each needs its own notes.
pub fn frame_key(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().to_string()
}

impl Roll {
    /// The notes for the frame at `path`; a virtual copy with none of its own shows its original's.
    pub fn frame(&self, path: &Path) -> Frame {
        let own = || self.frames.get(&frame_key(path));
        let orig = || crate::load::copy_of(path).and_then(|o| self.frames.get(&frame_key(&o)));
        own().or_else(orig).cloned().unwrap_or_default()
    }
}

/// The roll the photo belongs to and its own frame notes.
pub fn for_photo(path: &Path) -> Option<(Roll, Frame)> {
    let r = read(path.parent()?)?;
    let f = r.frame(path);
    Some((r, f))
}

/// Year, month and day of a date written with any separators ("2026-05-04", "2026–05–04", "2026/5/4",
/// "2026年5月4日", or an EXIF "2026:05:04 12:00:00"): the first three runs of digits, year first. None otherwise.
pub fn date_ymd(s: &str) -> Option<(u32, u32, u32)> {
    let mut n = s.split(|c: char| !c.is_ascii_digit()).filter(|t| !t.is_empty());
    let y = n.next().filter(|y| y.len() == 4)?.parse().ok()?;
    let (m, d): (u32, u32) = (n.next()?.parse().ok()?, n.next()?.parse().ok()?);
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some((y, m, d))
}

/// Contrast added to the negative conversion per stop of push (pushed film comes out contrastier, pulled film
/// flatter). The stock's own curve stays; this only nudges the Contrast slider.
pub const CONTRAST_PER_STOP: f32 = 12.0;

/// Set the photo up for this roll: the film stock for the negative conversion (when it's one Ferrotype knows) and
/// a contrast nudge for a push or pull. Turns the conversion on only for a known stock.
pub fn apply_stock(r: &Roll, p: &mut Params) {
    let known = crate::negative::STOCKS.iter().any(|s| s.id == r.stock);
    let custom = (!known && !r.stock.trim().is_empty()).then(|| crate::negative::custom_stocks().into_iter().find(|(_, c)| c.name == r.stock.trim())).flatten().map(|x| x.1);
    if let Some(c) = custom {
        let base = p.neg.base;
        p.neg = crate::negative::NegParams { enabled: true, base, levels: p.neg.levels, ..c.neg };
    } else if known {
        p.neg.enabled = true;
        p.neg.stock = r.stock.clone();
    } else {
        return;
    }
    let push = r.push_stops().clamp(-3.0, 3.0);
    if push.abs() > 0.05 {
        p.neg.contrast = (p.neg.contrast + push * CONTRAST_PER_STOP).clamp(-100.0, 100.0);
    }
}

/// Fill in what a scan's own metadata lacks (a camera's file keeps its own values) from the roll notes.
pub fn fill_shot(path: &Path, shot: &mut crate::lens::Shot) {
    if let Some((r, f)) = for_photo(path) {
        fill_shot_from(&r, &f, shot);
    }
}

/// `fill_shot` with the roll and frame notes already read.
pub fn fill_shot_from(r: &Roll, f: &Frame, shot: &mut crate::lens::Shot) {
    let empty = |s: &String| s.trim().is_empty();
    // A scanner's own make and model describe the scanner, not the camera the film went through.
    if !empty(&r.camera) {
        (shot.make, shot.model) = (String::new(), r.camera.trim().to_string());
    }
    if !empty(&r.lens) && (empty(&shot.lens) || !empty(&r.camera)) {
        shot.lens = r.lens.trim().to_string();
    }
    if r.speed() > 0.0 && (shot.iso <= 0.0 || !empty(&r.camera)) {
        shot.iso = r.speed();
    }
    if f.aperture > 0.0 {
        shot.fnum = f.aperture;
    }
    if f.shutter_secs() > 0.0 {
        shot.shutter = f.shutter_secs();
    }
    if f.focal > 0.0 {
        shot.focal = f.focal;
    }
    // The date is typed by hand: use it only when it reads as a date (a pasted en dash used to crash here).
    if let Some((y, m, d)) = date_ymd(&r.date)
        && (empty(&shot.taken) || !empty(&r.camera))
    {
        shot.taken = format!("{y:04}:{m:02}:{d:02} 12:00:00");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_pull() {
        let r = Roll { iso: 400.0, ei: 1600.0, ..Default::default() };
        assert!((r.push_stops() - 2.0).abs() < 1e-5);
        assert_eq!(r.push_label(), "+2 push");
        assert_eq!(Roll { iso: 400.0, ei: 200.0, ..Default::default() }.push_label(), "−1 pull");
        assert_eq!(Roll { iso: 400.0, ..Default::default() }.push_label(), "");
        assert_eq!(Roll { iso: 100.0, ei: 125.0, ..Default::default() }.push_label(), "+0.3 push");
    }

    #[test]
    fn shutter_parsing() {
        let f = |s: &str| Frame { shutter: s.into(), ..Default::default() }.shutter_secs();
        assert!((f("1/125") - 0.008).abs() < 1e-6);
        assert_eq!(f("2"), 2.0);
        assert_eq!(f("2s"), 2.0);
        assert_eq!(f("B"), 0.0);
    }

    #[test]
    fn saved_per_folder_and_used_by_frames() {
        let dir = std::env::temp_dir().join(format!("ferro-roll-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut r = Roll { roll: "12".into(), camera: "Nikon FM2".into(), stock: "trix400".into(), iso: 400.0, ei: 1600.0, date: "2026-05-04".into(), developer: "D-76".into(), ..Default::default() };
        r.frames.insert("scan_03.tif".into(), Frame { aperture: 2.8, shutter: "1/60".into(), focal: 50.0, ..Default::default() });
        write(&dir, &r).unwrap();
        let (back, f) = for_photo(&dir.join("scan_03.tif")).unwrap();
        assert_eq!(back, r);
        assert_eq!(f.aperture, 2.8);
        assert!(for_photo(&dir.join("scan_04.tif")).unwrap().1.is_empty());
        let mut shot = crate::lens::Shot { make: "EPSON".into(), model: "V850".into(), ..Default::default() };
        fill_shot(&dir.join("scan_03.tif"), &mut shot);
        assert_eq!((shot.model.as_str(), shot.iso, shot.fnum, shot.focal), ("Nikon FM2", 1600.0, 2.8, 50.0));
        assert_eq!(shot.taken, "2026:05:04 12:00:00");
        assert!(r.summary().contains("at 1600 (+2 push)"), "{}", r.summary());
        write(&dir, &Roll::default()).unwrap();
        assert!(read(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dates_are_read_whatever_the_dashes() {
        assert_eq!(date_ymd("2026-05-04"), Some((2026, 5, 4)));
        assert_eq!(date_ymd("2026–05–04"), Some((2026, 5, 4)));
        assert_eq!(date_ymd("2026年5月4日"), Some((2026, 5, 4)));
        assert_eq!(date_ymd("2026:05:04 12:00:00"), Some((2026, 5, 4)));
        for bad in ["May 2026", "4.5.2026", "2026-13-01", "", "été"] {
            assert_eq!(date_ymd(bad), None, "{bad}");
        }
        let dir = std::env::temp_dir().join(format!("ferro-roll-date-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (typed, want) in [("2026–05–04", "2026:05:04 12:00:00"), ("May 2026", "")] {
            write(&dir, &Roll { camera: "FM2".into(), date: typed.into(), ..Default::default() }).unwrap();
            let mut shot = crate::lens::Shot::default();
            fill_shot(&dir.join("a.tif"), &mut shot);
            assert_eq!(shot.taken, want, "{typed}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_frames_have_their_own_notes() {
        let mut r = Roll::default();
        let (scan, copy2, copy3) = (Path::new("/r/scan.tif"), Path::new("/r/scan.tif.copy2"), Path::new("/r/scan.tif.copy3"));
        r.frames.insert(frame_key(scan), Frame { aperture: 2.8, ..Default::default() });
        r.frames.insert(frame_key(copy2), Frame { aperture: 8.0, ..Default::default() });
        assert_eq!((r.frame(scan).aperture, r.frame(copy2).aperture), (2.8, 8.0));
        // A copy with no notes of its own shows its original's.
        assert_eq!(r.frame(copy3).aperture, 2.8);
    }

    #[test]
    fn stock_and_push_set_up_the_conversion() {
        let r = Roll { stock: "portra400".into(), iso: 400.0, ei: 800.0, ..Default::default() };
        let mut p = Params::default();
        apply_stock(&r, &mut p);
        assert!(p.neg.enabled && p.neg.stock == "portra400");
        assert!((p.neg.contrast - CONTRAST_PER_STOP).abs() < 1e-3);
        // An unknown stock leaves the photo alone.
        let mut q = Params::default();
        apply_stock(&Roll { stock: "Mystery 200".into(), ..Default::default() }, &mut q);
        assert!(!q.neg.enabled);
    }
}

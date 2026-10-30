//! Finding photos: a search over what we know about each one (name, keywords, camera, lens, date, roll and film
//! notes, rating), and collections, kept in the settings folder without a catalog: a smart collection is a saved
//! search over a folder and its subfolders; a plain one is a hand-picked list of files from anywhere.

use crate::develop::Params;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What a search looks at for one photo.
#[derive(Clone, Default, Debug)]
pub struct Entry {
    /// Lower-case words to match: file name, keywords, camera, lens, date, roll, film, developer, notes.
    pub text: String,
    pub iso: f32,
    pub focal: f32,
    pub fnum: f32,
    pub rating: u8,
    /// When taken, YYYY-MM-DD ("" if unknown).
    pub date: String,
}

/// Gather the searchable details of `path` (reads its EXIF, so do it off the UI thread).
pub fn entry(path: &Path, p: &Params) -> Entry {
    let shot = crate::export_shot(path);
    let mut words = vec![path.file_name().unwrap_or_default().to_string_lossy().to_string()];
    words.extend(p.keywords.iter().cloned());
    words.extend([shot.make.clone(), shot.model.clone(), shot.lens.clone()]);
    if let Some((r, f)) = crate::roll::for_photo(path) {
        words.extend([r.roll.clone(), r.stock_name(), r.developer.clone(), r.notes.clone(), f.note.clone()]);
        if !r.roll.trim().is_empty() {
            words.push(format!("roll {}", r.roll.trim()));
        }
        let push = r.push_label();
        if !push.is_empty() {
            words.push(push);
        }
    }
    if p.neg.enabled {
        if let Some(s) = crate::output::stock_name(p) {
            words.push(s);
        }
        words.push("negative film scan".into());
    }
    let t: Vec<&str> = shot.taken.split([':', ' ', 'T', '-']).collect();
    let date = if t.len() >= 3 && t[0].len() == 4 { format!("{}-{}-{}", t[0], t[1], t[2]) } else { String::new() };
    words.push(date.clone());
    if p.label > 0 {
        words.push(crate::xmp::LABELS[p.label.min(5) as usize].to_string());
    }
    Entry { text: words.join(" \u{1f} ").to_lowercase(), iso: shot.iso, focal: shot.focal, fnum: shot.fnum, rating: p.rating, date }
}

/// One condition of a search.
#[derive(Debug, PartialEq)]
enum Term {
    /// Must appear somewhere (or must not, with a leading `-`).
    Word(String, bool),
    /// iso, focal, f, stars or date compared with a value: `iso>=1600`, `focal<35`, `f<2.8`, `stars>=3`, `date>2026-05`.
    Cmp(String, String, String),
}

fn parse(q: &str) -> Vec<Term> {
    let mut out = vec![];
    // "quoted phrases" stay together.
    let mut rest = q.trim();
    while !rest.is_empty() {
        let (tok, next) = if let Some(r) = rest.strip_prefix('"') {
            match r.find('"') {
                Some(e) => (&r[..e], &r[e + 1..]),
                None => (r, ""),
            }
        } else {
            match rest.find(char::is_whitespace) {
                Some(e) => (&rest[..e], &rest[e..]),
                None => (rest, ""),
            }
        };
        rest = next.trim_start();
        let tok = tok.trim().to_lowercase();
        if tok.is_empty() {
            continue;
        }
        let field = ["iso", "focal", "f", "stars", "rating", "date"].iter().find_map(|f| {
            let r = tok.strip_prefix(f)?;
            let op = [">=", "<=", "=", ">", "<", ":"].iter().find(|o| r.starts_with(**o))?;
            let v = r[op.len()..].trim_end_matches("mm").to_string();
            (!v.is_empty()).then(|| Term::Cmp(f.to_string(), op.to_string(), v))
        });
        out.push(field.unwrap_or_else(|| match tok.strip_prefix('-') {
            Some(w) if !w.is_empty() => Term::Word(w.to_string(), false),
            _ => Term::Word(tok, true),
        }));
    }
    out
}

/// Whether `e` matches the search `q` (every condition must hold; empty matches everything).
pub fn matches(e: &Entry, q: &str) -> bool {
    parse(q).iter().all(|t| match t {
        Term::Word(w, want) => e.text.contains(w.as_str()) == *want,
        Term::Cmp(f, op, v) => {
            if f == "date" {
                // Compare as text: 2026-05 is before 2026-05-04, which is before 2026-06.
                if e.date.is_empty() {
                    return false;
                }
                let d = &e.date[..e.date.len().min(v.len())];
                return match op.as_str() {
                    ">" => d > v.as_str(),
                    ">=" => d >= v.as_str(),
                    "<" => d < v.as_str(),
                    "<=" => d <= v.as_str(),
                    _ => d == v.as_str(),
                };
            }
            let Ok(v) = v.trim_start_matches('f').trim_start_matches('/').parse::<f32>() else { return false };
            let x = match f.as_str() {
                "iso" => e.iso,
                "focal" => e.focal,
                "f" => e.fnum,
                _ => e.rating as f32,
            };
            if f != "stars" && f != "rating" && x <= 0.0 {
                return false; // unknown: doesn't match a number
            }
            match op.as_str() {
                ">" => x > v,
                ">=" => x >= v,
                "<" => x < v,
                "<=" => x <= v,
                _ => (x - v).abs() < 0.05 * v.abs().max(1.0),
            }
        }
    })
}

/// A collection: hand-picked files, or (with a query) a saved search over `folder` and its subfolders.
#[derive(Clone, PartialEq, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Collection {
    pub name: String,
    pub files: Vec<PathBuf>,
    pub query: String,
    pub folder: PathBuf,
}

impl Collection {
    pub fn is_smart(&self) -> bool {
        !self.query.trim().is_empty()
    }
}

fn file() -> Option<PathBuf> {
    crate::keys::dirs_config().map(|d| d.join("collections.json"))
}

pub fn load() -> Vec<Collection> {
    file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(c: &[Collection]) -> Result<(), String> {
    let f = file().ok_or("no settings folder")?;
    if let Some(d) = f.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    crate::write_atomic(&f, serde_json::to_string_pretty(c).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())
}

/// The photos of a collection that still exist: its own list, or for a smart one every photo under its folder
/// that matches (reads sidecars and EXIF: run it off the UI thread).
pub fn members(c: &Collection) -> Vec<PathBuf> {
    if !c.is_smart() {
        return c.files.iter().filter(|f| f.exists() || crate::load::copy_of(f).is_some_and(|o| o.exists())).cloned().collect();
    }
    let mut out: Vec<PathBuf> = crate::photos_under(&c.folder).into_iter().map(|(_, p)| p).filter(|p| {
        let params = crate::read_params(p).unwrap_or_default();
        matches(&entry(p, &params), &c.query)
    }).collect();
    out.sort();
    out
}

/// Keywords as typed ("street, Lisbon; night") → a clean list without duplicates (case-insensitive).
pub fn split_keywords(s: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for k in s.split([',', ';', '\n']) {
        let k = k.split_whitespace().collect::<Vec<_>>().join(" ");
        if !k.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(&k)) {
            out.push(k);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e() -> Entry {
        Entry { text: "dsc_0042.nef \u{1f} street \u{1f} lisbon \u{1f} nikon \u{1f} z 6 \u{1f} 35mm f/1.8 \u{1f} 2026-05-04".into(), iso: 3200.0, focal: 35.0, fnum: 1.8, rating: 4, date: "2026-05-04".into() }
    }

    #[test]
    fn words_and_numbers() {
        let e = e();
        assert!(matches(&e, ""));
        assert!(matches(&e, "lisbon street"));
        assert!(matches(&e, "Lisbon -porto"));
        assert!(!matches(&e, "lisbon -street"));
        assert!(matches(&e, "iso>=1600 focal<50 f<2"));
        assert!(!matches(&e, "iso<800"));
        assert!(matches(&e, "stars>=3"));
        assert!(!matches(&e, "stars>4"));
        assert!(matches(&e, "focal:35mm"));
        assert!(matches(&e, "date>=2026-05 date<2026-06"));
        assert!(!matches(&e, "date:2025"));
        assert!(matches(&e, "\"z 6\""));
        assert!(!matches(&e, "\"z 7\""));
    }

    #[test]
    fn keyword_lists_are_cleaned() {
        assert_eq!(split_keywords(" street, Lisbon ;night,  street , LISBON"), vec!["street", "Lisbon", "night"]);
        assert!(split_keywords(" , ;").is_empty());
    }

    #[test]
    fn entries_include_roll_notes_and_keywords() {
        let dir = std::env::temp_dir().join(format!("ferro-lib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("scan_01.tif");
        image::RgbImage::new(4, 4).save(&photo).unwrap();
        crate::roll::write(&dir, &crate::roll::Roll { roll: "7".into(), stock: "trix400".into(), iso: 400.0, ei: 1600.0, ..Default::default() }).unwrap();
        let p = Params { keywords: vec!["Harbour".into()], rating: 5, ..Default::default() };
        let en = entry(&photo, &p);
        assert!(matches(&en, "harbour tri-x \"roll 7\" stars:5"), "{}", en.text);
        assert!(matches(&en, "push"));
        // A smart collection over the folder finds it once its sidecar has the keyword.
        crate::write_params(&photo, &p).unwrap();
        let c = Collection { name: "Harbour".into(), query: "harbour".into(), folder: dir.clone(), ..Default::default() };
        assert_eq!(members(&c), vec![photo.clone()]);
        let plain = Collection { name: "Picks".into(), files: vec![photo.clone(), dir.join("gone.tif")], ..Default::default() };
        assert_eq!(members(&plain), vec![photo]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

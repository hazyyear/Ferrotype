//! Import from a memory card (or any folder): copy the photos into dated folders under your photo folder, renamed
//! from a pattern if you like, with an optional second copy on another drive. Photos imported before are skipped,
//! so the card can stay as it is between shoots. Nothing on the card is changed or deleted.
use crate::lens::Shot;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ImportOpts {
    /// Where the photos go, and the folder layout under it: {yyyy} {mm} {dd} {date} from when each was taken.
    pub dest: String,
    pub folders: String,
    /// New file names from a pattern (export tokens: {date} {n} {camera}…), "" = keep the camera's names.
    pub name: String,
    /// A second copy of everything, for example on an external drive. "" = none.
    pub backup: String,
    /// Leave out photos already imported (by name, size and time taken).
    pub skip_known: bool,
}

impl Default for ImportOpts {
    fn default() -> Self {
        let dest = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| PathBuf::from(h).join("Pictures").display().to_string()).unwrap_or_default();
        ImportOpts { dest, folders: "{yyyy}/{date}".into(), name: String::new(), backup: String::new(), skip_known: true }
    }
}

impl ImportOpts {
    fn path() -> Option<PathBuf> {
        Some(crate::keys::dirs_config()?.join("import.json"))
    }

    pub fn load() -> ImportOpts {
        Self::path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = Self::path() {
            let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
            let _ = crate::keep_if_damaged::<ImportOpts>(&p);
            let _ = crate::write_atomic(&p, serde_json::to_string_pretty(self).unwrap_or_default().as_bytes());
        }
    }
}

/// Folder layouts to choose from: (pattern, what it looks like).
pub const LAYOUTS: [(&str, &str); 4] = [("{yyyy}/{date}", "2026/2026-11-08"), ("{yyyy}/{mm}", "2026/11"), ("{date}", "2026-11-08"), ("", "All in one folder")];

/// One file to copy, and where to.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Already imported before (left out when `skip_known`).
    pub known: bool,
    /// What the import history remembers it by (name, size, time taken).
    pub key: String,
}

/// Memory cards and cameras that are plugged in: drives or volumes with a DCIM folder.
pub fn cards() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = vec![];
    if cfg!(windows) {
        roots.extend((b'D'..=b'Z').map(|c| PathBuf::from(format!("{}:\\", c as char))));
    } else if cfg!(target_os = "macos") {
        roots.extend(std::fs::read_dir("/Volumes").into_iter().flatten().flatten().map(|e| e.path()));
    } else {
        let user = std::env::var("USER").unwrap_or_default();
        for base in [format!("/media/{user}"), format!("/run/media/{user}"), "/media".into(), "/mnt".into()] {
            roots.extend(std::fs::read_dir(base).into_iter().flatten().flatten().map(|e| e.path()));
        }
    }
    let mut out: Vec<PathBuf> = roots.into_iter().map(|r| r.join("DCIM")).filter(|d| d.is_dir()).collect();
    out.dedup();
    out
}

/// Every photo under `src` (any depth), with RAW+JPEG pairs kept together by the sort.
pub fn photos_under(src: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![(src.to_path_buf(), 0u8)];
    // A card holds a few thousand photos: stop looking in a folder that is clearly not one (a whole disk).
    let mut dirs = 0;
    while let Some((d, depth)) = stack.pop() {
        dirs += 1;
        if dirs > 5000 || out.len() > 50_000 {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let hidden = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
            if hidden {
                continue;
            }
            if p.is_dir() && depth < 6 {
                stack.push((p, depth + 1));
            } else if crate::load::is_supported(&p) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// "YYYY-MM-DD" when the photo was taken (EXIF), else the file's date.
fn taken_date(shot: &Shot, path: &Path) -> String {
    let t: Vec<&str> = shot.taken.split([':', ' ', 'T', '-']).collect();
    if t.len() >= 3 && t[0].len() == 4 && t[0].chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", t[0], t[1], t[2])
    } else {
        crate::output::file_date(path)
    }
}

/// The folder (relative) for a photo taken on `date` ("YYYY-MM-DD").
pub fn folder_for(pattern: &str, date: &str) -> PathBuf {
    let (y, m, d) = (date.get(..4).unwrap_or("0000"), date.get(5..7).unwrap_or("00"), date.get(8..10).unwrap_or("00"));
    let s = pattern.replace("{yyyy}", y).replace("{mm}", m).replace("{dd}", d).replace("{date}", date);
    s.split(['/', '\\']).filter(|c| !c.trim().is_empty() && *c != ".." && *c != ".").map(|c| c.trim().to_string()).collect()
}

/// What makes a photo the same one next time: its name, size and time taken.
fn key(path: &Path, shot: &Shot) -> String {
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    format!("{}|{size}|{}", path.file_name().unwrap_or_default().to_string_lossy(), shot.taken.trim())
}

fn history_path() -> Option<PathBuf> {
    Some(crate::keys::dirs_config()?.join("imported.json"))
}

fn history() -> std::collections::HashSet<String> {
    history_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Work out what to copy where. Destination names that are taken (by another file, or by an earlier job of
/// this import) get -2, -3… like exports do; a RAW and its JPEG keep matching names.
pub fn plan(src: &Path, o: &ImportOpts) -> Vec<Job> {
    plan_cached(src, o, &mut Default::default())
}

/// `plan`, keeping each photo's camera details in `shots` so planning again (as settings change) is quick.
pub fn plan_cached(src: &Path, o: &ImportOpts, shots: &mut std::collections::HashMap<PathBuf, Shot>) -> Vec<Job> {
    let known = if o.skip_known { history() } else { Default::default() };
    let files = photos_under(src);
    // What each destination folder holds already: (stem, file) pairs, read once per folder.
    let mut existing: std::collections::HashMap<PathBuf, Vec<(String, PathBuf)>> = Default::default();
    let mut jobs = vec![];
    let mut taken: std::collections::HashSet<PathBuf> = Default::default();
    // Name per stem, so IMG_1.NEF and IMG_1.JPG get the same new name.
    let mut names: std::collections::HashMap<PathBuf, String> = Default::default();
    let mut n = 0;
    for f in files {
        let shot = shots.entry(f.clone()).or_insert_with(|| crate::lens::shot(&f).unwrap_or_default()).clone();
        let date = taken_date(&shot, &f);
        let dir = PathBuf::from(&o.dest).join(folder_for(&o.folders, &date));
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
        let stem_key = f.with_extension("");
        let stem = match names.get(&stem_key) {
            Some(s) => s.clone(),
            None => {
                n += 1;
                let s = if o.name.trim().is_empty() {
                    f.file_stem().unwrap_or_default().to_string_lossy().to_string()
                } else {
                    crate::output::file_name(&o.name, &f, n, "", &crate::develop::Params::default(), &shot)
                };
                // Unique for every extension of this stem in that folder.
                let mut s2 = s.clone();
                let mut i = 1;
                let there = existing.entry(dir.clone()).or_insert_with(|| {
                    std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| (e.path().file_stem().unwrap_or_default().to_string_lossy().to_string(), e.path())).collect()
                });
                // This very photo already there (imported before) keeps its name, even with its .xmp beside it;
                // any other file of that name, a sidecar included, means a new name.
                let mine = |s2: &str| there.iter().any(|(st, p)| st == s2 && same_file(p, &f));
                while taken.contains(&dir.join(&s2)) || (!mine(&s2) && there.iter().any(|(st, _)| *st == s2)) {
                    i += 1;
                    s2 = format!("{s}-{i}");
                }
                taken.insert(dir.join(&s2));
                names.insert(stem_key, s2.clone());
                s2
            }
        };
        let to = dir.join(format!("{stem}.{ext}"));
        let k = key(&f, &shot);
        jobs.push(Job { known: known.contains(&k) || same_file(&to, &f), from: f, to, key: k });
    }
    jobs
}

/// Same size and name: a copy imported before (the file is never compared byte by byte).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.len() == y.len() && a.file_name() == b.file_name(),
        _ => false,
    }
}

/// Copy `from` to `to` through a temporary name, then check the size, so a pulled card never leaves half a photo.
fn copy_checked(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = to.with_extension("ferrotype-import.tmp");
    let n = std::fs::copy(from, &tmp).map_err(|e| { let _ = std::fs::remove_file(&tmp); format!("{}: {e}", from.display()) })?;
    if std::fs::metadata(from).map(|m| m.len()).ok() != Some(n) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{} didn't copy completely", from.display()));
    }
    if to.exists() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{} already exists", to.display()));
    }
    std::fs::rename(&tmp, to).map_err(|e| e.to_string())?;
    // Keep the time it was taken as the file's time, as the card had it.
    if let Ok(t) = std::fs::metadata(from).and_then(|m| m.modified())
        && let Ok(f) = std::fs::File::options().write(true).open(to)
    {
        let _ = f.set_modified(t);
    }
    Ok(())
}

/// Run the jobs (leaving out known ones when asked): copy each, plus a backup copy, and remember what was imported.
/// Reports progress as (done, total). Returns the files now in the destination and any errors.
pub fn run(jobs: &[Job], o: &ImportOpts, progress: &dyn Fn(usize, usize)) -> (Vec<PathBuf>, Vec<String>) {
    let todo: Vec<&Job> = jobs.iter().filter(|j| !(o.skip_known && j.known)).collect();
    let mut hist = history();
    let (mut done, mut errs) = (vec![], vec![]);
    for (i, j) in todo.iter().enumerate() {
        progress(i, todo.len());
        match copy_checked(&j.from, &j.to) {
            Ok(()) => {
                // A camera's own sidecar (an .xmp written in camera, or edits made elsewhere) comes along.
                let side = j.from.with_extension("xmp");
                if side.is_file() {
                    let _ = copy_checked(&side, &j.to.with_extension("xmp"));
                }
                if !o.backup.trim().is_empty()
                    && let Ok(rel) = j.to.strip_prefix(&o.dest)
                {
                    let b = PathBuf::from(&o.backup).join(rel);
                    if !b.exists()
                        && let Err(e) = copy_checked(&j.from, &b)
                    {
                        errs.push(format!("backup: {e}"));
                    }
                }
                hist.insert(j.key.clone());
                done.push(j.to.clone());
            }
            Err(e) => errs.push(e),
        }
    }
    progress(todo.len(), todo.len());
    if let Some(p) = history_path().filter(|_| !cfg!(test)) {
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(&p));
        // A damaged list is kept as .damaged, not replaced by this import's photos alone.
        let _ = crate::keep_if_damaged::<std::collections::HashSet<String>>(&p);
        let _ = crate::write_atomic(&p, serde_json::to_string(&hist).unwrap_or_default().as_bytes());
    }
    (done, errs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_from_the_date() {
        assert_eq!(folder_for("{yyyy}/{date}", "2026-11-08"), PathBuf::from("2026").join("2026-11-08"));
        assert_eq!(folder_for("{yyyy}/{mm}", "2026-11-08"), PathBuf::from("2026").join("11"));
        assert_eq!(folder_for("", "2026-11-08"), PathBuf::new());
        // No climbing out of the destination.
        assert_eq!(folder_for("../{yyyy}", "2026-11-08"), PathBuf::from("2026"));
    }

    #[test]
    fn imports_once_and_keeps_pairs_together() {
        let dir = std::env::temp_dir().join(format!("ft-import-{}", std::process::id()));
        let card = dir.join("card").join("DCIM").join("100CANON");
        std::fs::create_dir_all(&card).unwrap();
        image::RgbImage::new(8, 8).save(card.join("IMG_1.jpg")).unwrap();
        image::RgbImage::new(8, 8).save(card.join("IMG_1.png")).unwrap();
        image::RgbImage::new(4, 4).save(card.join("IMG_2.jpg")).unwrap();
        std::fs::write(card.join("notes.txt"), "not a photo").unwrap();
        let o = ImportOpts { dest: dir.join("photos").display().to_string(), folders: String::new(), name: "trip-{n}".into(), backup: dir.join("backup").display().to_string(), skip_known: false };
        let jobs = plan(&dir.join("card"), &o);
        assert_eq!(jobs.len(), 3);
        let stems: Vec<String> = jobs.iter().map(|j| j.to.file_stem().unwrap().to_string_lossy().to_string()).collect();
        assert_eq!(stems, ["trip-001", "trip-001", "trip-002"], "the pair shares a name");
        let (done, errs) = run(&jobs, &o, &|_, _| {});
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(done.len(), 3);
        assert!(dir.join("backup").join("trip-002.jpg").is_file());
        // The same card again: everything is there already.
        let again = plan(&dir.join("card"), &ImportOpts { name: String::new(), ..o.clone() });
        assert!(again.iter().all(|j| j.to.file_stem().is_some_and(|s| s.to_string_lossy().starts_with("IMG_"))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_photo_with_its_xmp_beside_it_counts_as_imported() {
        let dir = std::env::temp_dir().join(format!("ft-import-xmp-{}", std::process::id()));
        let card = dir.join("card");
        std::fs::create_dir_all(&card).unwrap();
        image::RgbImage::new(8, 8).save(card.join("IMG_2.jpg")).unwrap();
        let o = ImportOpts { dest: dir.join("photos").display().to_string(), folders: String::new(), name: String::new(), backup: String::new(), skip_known: true };
        let (done, errs) = run(&plan(&card, &o), &o, &|_, _| {});
        assert!(errs.is_empty() && done.len() == 1, "{errs:?}");
        // Rating it later writes IMG_2.xmp next to it. The same card again must not make an IMG_2-2.jpg.
        std::fs::write(dir.join("photos").join("IMG_2.xmp"), "<x/>").unwrap();
        let again = plan(&card, &o);
        assert_eq!(again[0].to, dir.join("photos").join("IMG_2.jpg"));
        assert!(again[0].known);
        // A different photo of the same name still gets a new one.
        image::RgbImage::new(16, 16).save(card.join("IMG_2.jpg")).unwrap();
        assert_eq!(plan(&card, &o)[0].to, dir.join("photos").join("IMG_2-2.jpg"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
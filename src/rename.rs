//! Batch rename from a pattern, moving each photo's sidecars along, with one level of undo.
//! Photos that share a name (a RAW+JPEG pair) keep sharing one; the undo list is kept in the folder.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const LOG: &str = ".ferrotype-renames.json";

/// Old and new path for every file that moves: the photos themselves and their companions
/// (`IMG_1.NEF.ferrotype.json`, virtual copies `IMG_1.NEF.copy2.ferrotype.json`, `IMG_1.xmp`).
pub fn plan(files: &[PathBuf], pattern: &str, roll: &str) -> Vec<(PathBuf, PathBuf)> {
    // Group by folder + stem so RAW+JPEG pairs get the same new name; virtual copies travel with their photo.
    let mut groups: Vec<(PathBuf, String, Vec<PathBuf>)> = vec![];
    for f in files.iter().filter(|f| crate::load::copy_of(f).is_none()) {
        let (dir, stem) = (f.parent().unwrap_or(Path::new("")).to_path_buf(), f.file_stem().unwrap_or_default().to_string_lossy().to_string());
        match groups.iter_mut().find(|g| g.0 == dir && g.1 == stem) {
            Some(g) => g.2.push(f.clone()),
            None => groups.push((dir, stem, vec![f.clone()])),
        }
    }
    let mut taken: HashSet<PathBuf> = HashSet::new();
    let moving: HashSet<PathBuf> = groups.iter().flat_map(|g| g.2.iter().cloned()).collect();
    let mut out = vec![];
    for (n, (dir, stem, members)) in groups.iter().enumerate() {
        let first = &members[0];
        let p = crate::read_params(first).unwrap_or_default();
        let shot = crate::lens::shot(first).unwrap_or_default();
        let base = crate::output::file_name(pattern, first, n + 1, roll, &p, &shot);
        // A free stem: no member's new name may clash with a file that stays, or with another new name.
        let mut new_stem = base.clone();
        for k in 2.. {
            let clash = members.iter().any(|m| {
                let t = dir.join(with_stem(m, &new_stem));
                taken.contains(&t) || (t.exists() && !moving.contains(&t))
            });
            if !clash {
                break;
            }
            new_stem = format!("{base}-{k}");
        }
        if new_stem == *stem {
            members.iter().for_each(|m| { taken.insert(m.clone()); });
            continue;
        }
        let mut xmp_done = false;
        for m in members {
            let to = dir.join(with_stem(m, &new_stem));
            taken.insert(to.clone());
            out.push((m.clone(), to.clone()));
            // Companions named after the whole file name: sidecar, damaged sidecar, virtual copies.
            let name = m.file_name().unwrap_or_default().to_string_lossy().to_string();
            let new_name = to.file_name().unwrap_or_default().to_string_lossy().to_string();
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().to_string();
                    if let Some(rest) = n.strip_prefix(&format!("{name}.")) {
                        out.push((e.path(), dir.join(format!("{new_name}.{rest}"))));
                    }
                }
            }
            let xmp = m.with_extension("xmp");
            if !xmp_done && xmp.exists() && xmp != *m {
                out.push((xmp, dir.join(format!("{new_stem}.xmp"))));
                xmp_done = true;
            }
        }
    }
    out
}

fn with_stem(f: &Path, stem: &str) -> String {
    match f.extension() {
        Some(e) => format!("{stem}.{}", e.to_string_lossy()),
        None => stem.to_string(),
    }
}

/// Rename everything in `plan` (in two steps, so names can swap) and remember it for `undo`.
pub fn apply(plan: &[(PathBuf, PathBuf)]) -> Result<usize, String> {
    if plan.is_empty() {
        return Ok(0);
    }
    refuse_clash(plan)?;
    let log: Vec<(String, String)> = plan.iter().map(|(a, b)| (a.to_string_lossy().into(), b.to_string_lossy().into())).collect();
    let text = serde_json::to_string_pretty(&log).map_err(|e| e.to_string())?;
    // Every folder involved gets the whole list, so undo works from any of them.
    for dir in folders(plan.iter().map(|(a, _)| a.as_path())) {
        crate::write_atomic(&dir.join(LOG), text.as_bytes()).map_err(|e| format!("could not save the undo list: {e}"))?;
    }
    move_all(plan)
}

/// Err when a move would land on a file that exists and isn't itself moving out of the way.
fn refuse_clash(plan: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    // On Windows and macOS `img.jpg` → `IMG.jpg` names the same file: that is a rename, not a clash.
    let same = |a: &Path, b: &Path| a == b || std::fs::canonicalize(a).ok().is_some_and(|x| std::fs::canonicalize(b).ok() == Some(x));
    match plan.iter().find(|(from, to)| to.exists() && !plan.iter().any(|(f, _)| f == to) && !same(from, to)) {
        Some((_, to)) => Err(format!("{} already exists", to.display())),
        None => Ok(()),
    }
}

fn folders<'a>(files: impl Iterator<Item = &'a Path>) -> std::collections::BTreeSet<PathBuf> {
    files.map(|f| f.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf()).collect()
}

fn move_all(plan: &[(PathBuf, PathBuf)]) -> Result<usize, String> {
    let tmp: Vec<PathBuf> = plan.iter().enumerate().map(|(i, (a, _))| a.with_file_name(format!(".ferrotype-renaming-{i}-{}", std::process::id()))).collect();
    for (i, (a, _)) in plan.iter().enumerate() {
        if let Err(e) = std::fs::rename(a, &tmp[i]) {
            // Put back what already moved.
            for (j, (b, _)) in plan.iter().enumerate().take(i) {
                let _ = std::fs::rename(&tmp[j], b);
            }
            return Err(format!("{}: {e}", a.display()));
        }
    }
    let mut failed = vec![];
    for (i, (_, b)) in plan.iter().enumerate() {
        if let Err(e) = std::fs::rename(&tmp[i], b) {
            let _ = std::fs::rename(&tmp[i], &plan[i].0);
            failed.push(format!("{}: {e}", b.display()));
        }
    }
    if failed.is_empty() { Ok(plan.len()) } else { Err(failed.join("; ")) }
}

/// Whether `dir` has a rename that can be undone.
pub fn can_undo(dir: &Path) -> bool {
    dir.join(LOG).exists()
}

/// Put back the names from the last rename in `dir`.
pub fn undo(dir: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(dir.join(LOG)).map_err(|_| "nothing to undo".to_string())?;
    let log: Vec<(String, String)> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let back: Vec<(PathBuf, PathBuf)> = log.into_iter().rev().map(|(a, b)| (PathBuf::from(b), PathBuf::from(a))).filter(|(from, _)| from.exists()).collect();
    // A new photo may have taken an old name since (cameras restart numbering): never rename over it.
    refuse_clash(&back).map_err(|e| format!("{e}, so the rename can't be undone"))?;
    let n = move_all(&back)?;
    for d in folders(back.iter().map(|(_, a)| a.as_path())).into_iter().chain([dir.to_path_buf()]) {
        let _ = std::fs::remove_file(d.join(LOG));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_moves_sidecars_keeps_pairs_and_undoes() {
        let dir = std::env::temp_dir().join(format!("ferrotype-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["A.NEF", "A.JPG", "A.NEF.ferrotype.json", "A.NEF.copy2.ferrotype.json", "A.xmp", "B.JPG", "roll-002.JPG"] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        let files = vec![dir.join("A.NEF"), dir.join("A.JPG"), dir.join("B.JPG")];
        let plan = plan(&files, "roll-{n}", "");
        let names: HashSet<String> = plan.iter().map(|(_, b)| b.file_name().unwrap().to_string_lossy().to_string()).collect();
        // The pair shares roll-001; B would be roll-002, which exists and isn't moving, so it becomes roll-002-2.
        for n in ["roll-001.NEF", "roll-001.JPG", "roll-001.NEF.ferrotype.json", "roll-001.NEF.copy2.ferrotype.json", "roll-001.xmp", "roll-002-2.JPG"] {
            assert!(names.contains(n), "{n} in {names:?}");
        }
        assert_eq!(apply(&plan).unwrap(), 6);
        assert_eq!(std::fs::read_to_string(dir.join("roll-001.NEF.ferrotype.json")).unwrap(), "A.NEF.ferrotype.json");
        assert_eq!(std::fs::read_to_string(dir.join("roll-002.JPG")).unwrap(), "roll-002.JPG");
        assert!(can_undo(&dir));
        // A new photo took B.JPG's old name: undo must refuse rather than rename over it.
        std::fs::write(dir.join("B.JPG"), "new card").unwrap();
        assert!(undo(&dir).unwrap_err().contains("already exists"));
        assert_eq!(std::fs::read_to_string(dir.join("B.JPG")).unwrap(), "new card");
        std::fs::remove_file(dir.join("B.JPG")).unwrap();
        undo(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("A.xmp")).unwrap(), "A.xmp");
        assert!(!dir.join("roll-001.NEF").exists() && !can_undo(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn undo_works_from_either_folder() {
        let root = std::env::temp_dir().join(format!("ferrotype-rename2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (root.join("a"), root.join("b"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(a.join("x.jpg"), "x").unwrap();
        std::fs::write(b.join("y.jpg"), "y").unwrap();
        apply(&[(a.join("x.jpg"), a.join("x1.jpg")), (b.join("y.jpg"), b.join("y1.jpg"))]).unwrap();
        assert!(can_undo(&a) && can_undo(&b));
        assert_eq!(undo(&b).unwrap(), 2);
        assert!(a.join("x.jpg").exists() && b.join("y.jpg").exists());
        assert!(!can_undo(&a) && !can_undo(&b));
        std::fs::remove_dir_all(&root).unwrap();
    }
}

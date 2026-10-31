//! "Edit in…": hand a photo to another program (GIMP, Photoshop, Affinity Photo, Krita) for work ferrotype
//! doesn't do, such as layers or compositing. The photo is rendered with its edits to a 16-bit TIFF next to the
//! original (`IMG_0001-edit.tif`), the program opens it, and when it is saved there the Library picks it up
//! beside the original.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Editor {
    pub name: String,
    /// The program: an .exe on Windows, an .app bundle on macOS, a command or file elsewhere.
    pub path: PathBuf,
}

fn saved_path() -> Option<PathBuf> {
    Some(crate::keys::dirs_config()?.join("editors.json"))
}

/// Programs the user added with "Choose a program…".
pub fn saved() -> Vec<Editor> {
    saved_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(list: &[Editor]) -> Result<(), String> {
    let p = saved_path().ok_or("no settings folder")?;
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    crate::write_atomic(&p, &serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Add a program chosen by the user (kept first, no duplicates) and return the new list.
pub fn remember(path: &Path) -> Result<Vec<Editor>, String> {
    let mut list = saved();
    list.retain(|e| e.path != path);
    list.insert(0, Editor { name: display_name(path), path: path.to_path_buf() });
    save(&list)?;
    Ok(list)
}

fn display_name(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let lower = stem.to_lowercase();
    for (k, n) in [("gimp", "GIMP"), ("photoshop", "Photoshop"), ("affinity", "Affinity Photo"), ("photo", "Affinity Photo"), ("krita", "Krita")] {
        if lower.contains(k) {
            return n.into();
        }
    }
    stem
}

/// Children of `dir` whose names start with `prefix` (newest version last, by name).
fn versions(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix))).collect();
    v.sort();
    v
}

/// Well-known image editors installed in their usual places.
pub fn detected() -> Vec<Editor> {
    let mut out = vec![];
    let mut add = |name: &str, p: PathBuf| {
        if p.exists() && !out.iter().any(|e: &Editor| e.path == p) {
            out.push(Editor { name: name.into(), path: p });
        }
    };
    if cfg!(windows) {
        for base in [std::env::var_os("ProgramFiles"), std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Programs").into_os_string())].into_iter().flatten() {
            let base = PathBuf::from(base);
            for g in versions(&base, "GIMP") {
                for exe in versions(&g.join("bin"), "gimp-").into_iter().rev().filter(|p| p.extension().is_some_and(|e| e == "exe") && !p.to_string_lossy().contains("console")) {
                    add("GIMP", exe);
                    break;
                }
            }
            for ps in versions(&base.join("Adobe"), "Adobe Photoshop").into_iter().rev() {
                add("Photoshop", ps.join("Photoshop.exe"));
            }
            add("Affinity Photo", base.join("Affinity").join("Photo 2").join("Photo.exe"));
            add("Affinity", base.join("Affinity").join("Affinity").join("Affinity.exe"));
            add("Krita", base.join("Krita (x64)").join("bin").join("krita.exe"));
        }
    } else if cfg!(target_os = "macos") {
        let apps = Path::new("/Applications");
        add("GIMP", apps.join("GIMP.app"));
        for g in versions(apps, "GIMP-") {
            add("GIMP", g);
        }
        for ps in versions(apps, "Adobe Photoshop").into_iter().rev() {
            let name = ps.file_name().unwrap_or_default().to_string_lossy().to_string();
            add("Photoshop", ps.join(format!("{name}.app")));
        }
        add("Affinity Photo", apps.join("Affinity Photo 2.app"));
        add("Affinity", apps.join("Affinity.app"));
        add("Krita", apps.join("krita.app"));
    } else {
        let path = std::env::var_os("PATH").unwrap_or_default();
        for (name, cmd) in [("GIMP", "gimp"), ("Krita", "krita")] {
            if let Some(p) = std::env::split_paths(&path).map(|d| d.join(cmd)).find(|p| p.is_file()) {
                add(name, p);
            }
        }
    }
    out
}

/// What the "Edit in" menu lists: the user's own choices first, then detected programs.
pub fn all() -> Vec<Editor> {
    let mut list = saved();
    for e in detected() {
        if !list.iter().any(|s| s.path == e.path) {
            list.push(e);
        }
    }
    list
}

/// Open `file` in `editor`, without waiting for it to close.
pub fn launch(editor: &Path, file: &Path) -> Result<(), String> {
    let mut cmd = if cfg!(target_os = "macos") && editor.extension().is_some_and(|e| e == "app") {
        let mut c = std::process::Command::new("open");
        c.arg("-a").arg(editor);
        c
    } else {
        std::process::Command::new(editor)
    };
    cmd.arg(file).spawn().map(|_| ()).map_err(|e| format!("could not start {}: {e}", editor.display()))
}

/// Render `photo` with its edits to a full-size 16-bit TIFF next to it (`<name>-edit.tif`, never overwriting) in
/// colour space `space`, and return the file.
pub fn render_for_editing(photo: &Path, space: crate::colour::Space) -> Result<PathBuf, String> {
    let dir = photo.parent().unwrap_or(Path::new("."));
    let o = crate::ExportOpts { ext: "tif", name: "{name}-edit".into(), space, ..Default::default() };
    crate::export_file(photo, dir, &o, 1, |_| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_program() {
        assert_eq!(display_name(Path::new("C:/Program Files/GIMP 3/bin/gimp-3.exe")), "GIMP");
        assert_eq!(display_name(Path::new("/Applications/Adobe Photoshop 2026/Adobe Photoshop 2026.app")), "Photoshop");
        assert_eq!(display_name(Path::new("/opt/paint/mypaint")), "mypaint");
    }

    #[test]
    fn edit_copy_is_a_tiff_next_to_the_photo() {
        let dir = std::env::temp_dir().join(format!("ferrotype-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("frame.png");
        image::RgbImage::from_pixel(8, 6, image::Rgb([200, 100, 50])).save(&src).unwrap();
        let out = render_for_editing(&src, crate::colour::Space::Srgb).unwrap();
        assert_eq!(out, dir.join("frame-edit.tif"));
        let again = render_for_editing(&src, crate::colour::Space::Srgb).unwrap();
        assert_ne!(again, out, "a second edit copy never overwrites the first");
        assert_eq!(image::open(&out).unwrap().to_rgb16().dimensions(), (8, 6));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

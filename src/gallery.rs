//! Web gallery export: a folder holding `index.html`, the photos and their thumbnails, with nothing else to
//! install. Upload the folder to any web host (or open `index.html` from disk) to show a roll or a collection.
//! The page is plain HTML and CSS with a few lines of script for the full-screen viewer and arrow keys.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Options {
    pub title: String,
    /// Long edge of the large photos, in pixels.
    pub size: u32,
    pub quality: u8,
    /// Show the camera, lens, roll and stock under each photo (and write them into the JPEGs).
    pub details: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { title: "Photos".into(), size: 2048, quality: 88, details: true }
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A folder name from a title: letters, digits and dashes.
pub fn slug(title: &str) -> String {
    let s: String = title.trim().chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() { "gallery".into() } else { s }
}

/// A new, empty folder `<parent>/<slug>-gallery` (or `-gallery-2` and so on).
pub fn new_folder(parent: &Path, title: &str) -> Result<PathBuf, String> {
    let base = format!("{}-gallery", slug(title));
    for i in 1..1000 {
        let d = parent.join(if i == 1 { base.clone() } else { format!("{base}-{i}") });
        match std::fs::create_dir(&d) {
            Ok(()) => return Ok(d),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("no free folder name".into())
}

/// One caption line: date taken, then camera details or the roll and stock.
fn caption(path: &Path, p: &crate::develop::Params, o: &Options) -> String {
    let shot = crate::export_shot(path);
    let mut parts: Vec<String> = vec![];
    let t = shot.taken.trim();
    if t.len() >= 10 {
        parts.push(t[..10].replace(':', "-"));
    }
    if o.details {
        let d = crate::export_description(p, &crate::ExportOpts::default(), path);
        if !d.is_empty() {
            parts.push(d);
        }
        let cam = [shot.model.trim(), shot.lens.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        if !cam.is_empty() {
            parts.push(cam);
        }
    }
    parts.join("  ·  ")
}

/// Render `files` with their edits into `dir` (which should be new and empty) and write the page.
/// `progress(done, total)` is called after each photo. Returns the page's path.
pub fn make(files: &[PathBuf], dir: &Path, o: &Options, progress: &dyn Fn(usize, usize)) -> Result<PathBuf, String> {
    if files.is_empty() {
        return Err("no photos to put in the gallery".into());
    }
    let (big_dir, small_dir) = (dir.join("photos"), dir.join("thumbs"));
    std::fs::create_dir_all(&big_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&small_dir).map_err(|e| e.to_string())?;
    let mut items = vec![];
    for (i, f) in files.iter().enumerate() {
        let n = i + 1;
        let src = crate::export_source(f, |_| {}).map_err(|e| format!("{}: {e}", f.file_name().unwrap_or_default().to_string_lossy()))?;
        let big = crate::ExportOpts { ext: "jpg", long_edge: o.size, quality: o.quality, name: "{n}".into(), meta: o.details, ..Default::default() };
        let small = crate::ExportOpts { ext: "jpg", long_edge: 640, quality: 80, name: "{n}".into(), ..Default::default() };
        let b = crate::export_prepared(f, &src, &big_dir, &big, n)?;
        let s = crate::export_prepared(f, &src, &small_dir, &small, n)?;
        let (w, h) = image::image_dimensions(&s).unwrap_or((3, 2));
        let rel = |p: &Path, sub: &str| format!("{sub}/{}", p.file_name().unwrap_or_default().to_string_lossy());
        items.push((rel(&b, "photos"), rel(&s, "thumbs"), w, h, caption(f, &src.0, o)));
        progress(n, files.len());
    }
    let page = dir.join("index.html");
    std::fs::write(&page, html(&o.title, &items)).map_err(|e| e.to_string())?;
    Ok(page)
}

/// The page: a justified grid of thumbnails, and a full-screen viewer (click, arrows, Escape).
fn html(title: &str, items: &[(String, String, u32, u32, String)]) -> String {
    let mut grid = String::new();
    for (k, (big, small, w, h, cap)) in items.iter().enumerate() {
        // Each tile grows in proportion to its aspect ratio, so rows fill the width without cropping.
        let ar = *w as f32 / (*h).max(1) as f32;
        grid += &format!(
            "<a class=\"t\" href=\"{}\" data-i=\"{k}\" data-cap=\"{}\" style=\"flex-grow:{:.3};flex-basis:{:.0}px\"><i style=\"padding-bottom:{:.3}%\"></i><img src=\"{}\" alt=\"{}\" loading=\"lazy\"></a>\n",
            esc(big), esc(cap), ar, ar * 220.0, 100.0 / ar, esc(small), esc(cap)
        );
    }
    format!(r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="generator" content="ferrotype">
<title>{t}</title>
<style>
:root {{ --ink:#141416; --panel:#1c1c1f; --line:#2a2a2e; --bone:#ece8e1; --dim:#9a968f; }}
* {{ box-sizing:border-box; }}
html,body {{ margin:0; background:var(--ink); color:var(--bone); font:15px/1.5 "IBM Plex Sans",system-ui,-apple-system,"Segoe UI",sans-serif; }}
header {{ padding:40px 24px 20px; max-width:1600px; margin:0 auto; }}
h1 {{ margin:0; font-size:32px; font-weight:700; letter-spacing:-0.01em; }}
header p {{ margin:4px 0 0; color:var(--dim); font:13px "IBM Plex Mono",ui-monospace,monospace; }}
.g {{ display:flex; flex-wrap:wrap; gap:8px; padding:0 24px 48px; max-width:1600px; margin:0 auto; }}
.g::after {{ content:""; flex-grow:999; }}
.t {{ position:relative; display:block; background:var(--panel); overflow:hidden; }}
.t i {{ display:block; }}
.t img {{ position:absolute; inset:0; width:100%; height:100%; object-fit:cover; transition:opacity .15s; }}
.t:hover img {{ opacity:.85; }}
.v {{ position:fixed; inset:0; background:rgba(10,10,11,.97); display:none; flex-direction:column; align-items:center; justify-content:center; padding:24px; }}
.v.on {{ display:flex; }}
.v img {{ max-width:100%; max-height:calc(100vh - 96px); object-fit:contain; }}
.v p {{ margin:12px 0 0; color:var(--dim); font:13px "IBM Plex Mono",ui-monospace,monospace; text-align:center; min-height:1.5em; }}
.v button {{ position:absolute; background:none; border:0; color:var(--bone); font-size:28px; cursor:pointer; padding:16px; opacity:.7; }}
.v button:hover,.v button:focus-visible {{ opacity:1; }}
.x {{ top:8px; right:8px; }} .p {{ left:8px; top:50%; transform:translateY(-50%); }} .n {{ right:8px; top:50%; transform:translateY(-50%); }}
footer {{ text-align:center; color:var(--dim); font-size:12px; padding:0 0 32px; }}
@media (max-width:600px) {{ header {{ padding:24px 16px 12px; }} .g {{ padding:0 16px 32px; gap:4px; }} }}
</style>
</head>
<body>
<header><h1>{t}</h1><p>{n} photo{s}</p></header>
<main class="g">
{grid}</main>
<div class="v" role="dialog" aria-modal="true" aria-label="Photo viewer"><img alt=""><p></p>
<button class="x" aria-label="Close">×</button><button class="p" aria-label="Previous">‹</button><button class="n" aria-label="Next">›</button></div>
<footer>Made with ferrotype</footer>
<script>
const ts=[...document.querySelectorAll('.t')],v=document.querySelector('.v'),vi=v.querySelector('img'),vp=v.querySelector('p');let at=-1;
function show(i){{at=(i+ts.length)%ts.length;const t=ts[at];vi.src=t.href;vi.alt=t.dataset.cap;vp.textContent=t.dataset.cap;v.classList.add('on');}}
function hide(){{v.classList.remove('on');vi.src='';if(at>=0)ts[at].focus();at=-1;}}
ts.forEach((t,i)=>t.addEventListener('click',e=>{{e.preventDefault();show(i);}}));
v.querySelector('.x').onclick=hide;v.querySelector('.p').onclick=()=>show(at-1);v.querySelector('.n').onclick=()=>show(at+1);
v.addEventListener('click',e=>{{if(e.target===v)hide();}});
document.addEventListener('keydown',e=>{{if(at<0)return;if(e.key==='Escape')hide();if(e.key==='ArrowLeft')show(at-1);if(e.key==='ArrowRight')show(at+1);}});
</script>
</body>
</html>
"#, t = esc(title), n = items.len(), s = if items.len() == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_folders() {
        assert_eq!(slug("Roll 12 — Lisbon, May"), "roll-12-lisbon-may");
        assert_eq!(slug("  !!  "), "gallery");
    }

    #[test]
    fn gallery_has_a_page_photos_and_thumbs() {
        let dir = std::env::temp_dir().join(format!("ferrotype-gallery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<PathBuf> = (0..3).map(|i| {
            let f = dir.join(format!("frame{i}.png"));
            image::RgbImage::from_fn(300, 200, |x, _| image::Rgb([(x % 255) as u8, 90, 40 * i as u8])).save(&f).unwrap();
            f
        }).collect();
        let out = new_folder(&dir, "Roll <12>").unwrap();
        assert_eq!(out.file_name().unwrap(), "roll-12-gallery");
        assert_eq!(new_folder(&dir, "Roll <12>").unwrap().file_name().unwrap(), "roll-12-gallery-2");
        let page = make(&files, &out, &Options { title: "Roll <12>".into(), ..Default::default() }, &|_, _| {}).unwrap();
        let html = std::fs::read_to_string(&page).unwrap();
        assert!(html.contains("<title>Roll &lt;12&gt;</title>") && html.contains("3 photos"));
        assert_eq!(html.matches("class=\"t\"").count(), 3);
        assert!(out.join("photos/001.jpg").exists() && out.join("thumbs/003.jpg").exists());
        assert!(html.contains("href=\"photos/002.jpg\""));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

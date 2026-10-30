//! The editor window: Library, Develop and Negative modules.
use crate::develop::{self, HUE_CENTERS, Params, hue_rotate, luma, wb_gains};

mod masks;
mod scan_view;
mod scanner_view;
use masks::{Draw, MaskUi};
use scan_view::ScanUi;
use crate::load::{self, linear_to_srgb};
use crate::negative::{self, Kind, Look, STOCKS};
use crate::widgets::*;
use crate::keys::{self, Keymap};
use crate::{ExportOpts, save_contact_sheets, denoise, export_file, import_xmp_preset, read_params, scan, write_export, write_params};
use eframe::egui;
use egui::{Align, Align2, Color32, ColorImage, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Ui, Vec2, pos2, vec2};
use image::Rgb32FImage;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

const PREVIEW: u32 = 1400;
/// Smaller copy rendered while a slider is being dragged, so dragging stays smooth (about 4× faster).
const DRAFT: u32 = 700;
const THUMB: u32 = 320;
/// Develop and Negative's settings panel. 360 px gives slider tracks about 175 px: at 320 a pixel was 0.075 EV.
const RIGHT_W: f32 = 360.0;
/// Thumbnail textures kept in memory (about 270 KB each); the grid only shows a few dozen at a time.
const THUMB_KEEP: usize = 600;
/// Undo steps kept per photo.
const HISTORY_MAX: usize = 200;
/// The thumbnail cache on disk is trimmed to this at start-up (about 50,000 thumbnails).
const THUMB_CACHE_MAX: u64 = 1 << 30;

#[derive(PartialEq, Clone, Copy)]
enum Module {
    Library,
    /// Bringing film in: strip scans, camera scanning, the whole roll.
    Scan,
    Cull,
    Develop,
    Negative,
}

/// What a click or drag on the photo does in Develop.
#[derive(PartialEq, Clone, Copy)]
enum Tool {
    None,
    Crop,
    Heal,
    Mask,
    Sample,
    /// Place a colour chart on the photo to build a camera or scanner profile.
    Chart,
}

/// The chart calibration tool: which chart, where its corner patches are (frame fractions), and the last fit.
struct ChartUi {
    target: crate::calib::Target,
    q: [[f32; 2]; 4],
    grab: Option<usize>,
    name: String,
    fit: Option<Result<crate::calib::Fit, String>>,
    /// A build is running on a worker thread.
    busy: bool,
}

impl ChartUi {
    fn new() -> Self {
        ChartUi { target: crate::calib::colorchecker(), q: [[0.3, 0.35], [0.7, 0.35], [0.7, 0.65], [0.3, 0.65]], grab: None, name: String::new(), fit: None, busy: false }
    }
}

/// What the Library needs to know about a photo's sidecar, cached per folder.
#[derive(Clone, Copy, Default)]
struct Meta {
    edited: bool,
    neg: bool,
    rating: u8,
    flag: i8,
    label: u8,
}

impl Meta {
    fn of(p: &Params) -> Self {
        Meta { edited: p.is_edited(), neg: p.neg.enabled, rating: p.rating, flag: p.flag, label: p.label }
    }
}

/// Lightroom's colour label colours: red, yellow, green, blue, purple.
const LABEL_COLORS: [Color32; 5] = [
    Color32::from_rgb(0xE5, 0x48, 0x4D),
    Color32::from_rgb(0xF5, 0xD9, 0x0A),
    Color32::from_rgb(0x46, 0xA7, 0x58),
    Color32::from_rgb(0x3E, 0x63, 0xDD),
    Color32::from_rgb(0x8E, 0x4E, 0xC6),
];

/// One photo ready for culling: the picture, where it is in focus, and what the sensor recorded.
struct CullImg {
    tex: TextureHandle,
    focus: TextureHandle,
    bins: [[f32; 128]; 3],
    /// Clipped share per channel, in percent; from the raw data when `raw`.
    clipped: [f32; 3],
    raw: bool,
}

/// Pixels for a `CullImg`, made off the UI thread.
struct CullData {
    img: ColorImage,
    focus: ColorImage,
    bins: [[f32; 128]; 3],
    clipped: [f32; 3],
    raw: bool,
}

#[derive(Default)]
struct CullState {
    cache: std::collections::HashMap<PathBuf, CullImg>,
    pending: std::collections::HashSet<PathBuf>,
    /// Frames near the selection. A queued culling job for any other frame is skipped, so stepping fast
    /// through a card never piles up full RAW reads.
    wanted: Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
    compare: bool,
    focus: bool,
    /// What the last key did ("★★★", "Reject") and when, shown large on the photo for a moment.
    flash: Option<(String, std::time::Instant)>,
    /// Frames that could not be read: shown as such, never retried every frame.
    failed: std::collections::HashSet<PathBuf>,
}

/// Full decodes run one at a time: holding an arrow key would otherwise start one per frame passed, each
/// several hundred MB for a large RAW.
static DECODE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 1:1 view: the visible part rendered at full resolution.
struct Zoom {
    /// Centre of the view, as a fraction of the output.
    center: [f32; 2],
    /// Screen pixels per photo pixel (1 = 100%). Set from `from_fit` on the first frame when zooming in from Fit.
    scale: f32,
    from_fit: Option<f32>,
    /// Keep the photo point at `center` under this screen position (the pointer when zooming with wheel or pinch).
    anchor: Option<Pos2>,
    tex: Option<TextureHandle>,
    /// What `tex` shows: settings, window (x, y, w, h in output pixels) and the scale it was rendered at (at most 1).
    shown: Option<(Params, [u32; 4], f32)>,
    /// A render is running in the background.
    busy: bool,
    /// When the view last moved; the sharp render waits until it settles.
    moved: f64,
}

/// Wheel, pinch and trackpad input over `area`: (zoom factor, pan in points). A mouse wheel zooms;
/// two fingers on a trackpad pan, and pinching (or Ctrl/Cmd + scrolling) zooms.
impl Zoom {
    fn at(center: [f32; 2], scale: f32, anchor: Option<Pos2>) -> Zoom {
        Zoom { center, scale, from_fit: None, anchor, tex: None, shown: None, busy: false, moved: 0.0 }
    }
}

fn view_gestures(ui: &Ui, area: Rect) -> (f32, Vec2, Option<Pos2>) {
    let Some(pos) = ui.input(|i| i.pointer.hover_pos()).filter(|p| area.contains(*p)) else { return (1.0, Vec2::ZERO, None) };
    ui.input(|i| {
        use egui::{MouseWheelSource, MouseWheelUnit};
        let mut zoom = i.zoom_delta();
        let mut pan = Vec2::ZERO;
        for e in &i.events {
            if let egui::Event::MouseWheel { unit, delta, source, modifiers, .. } = e {
                if modifiers.ctrl || modifiers.command {
                    continue; // already in zoom_delta
                }
                let wheel = match source {
                    MouseWheelSource::Wheel => true,
                    MouseWheelSource::Unknown => *unit != MouseWheelUnit::Point,
                    _ => false,
                };
                let steps = match unit { MouseWheelUnit::Point => delta.y / 40.0, MouseWheelUnit::Line => delta.y, MouseWheelUnit::Page => delta.y * 4.0 };
                if wheel {
                    zoom *= 1.2f32.powf(steps);
                } else {
                    pan += if *unit == MouseWheelUnit::Point { *delta } else { *delta * 40.0 };
                }
            }
        }
        (zoom, pan, Some(pos))
    })
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    After,
    Before,
    Split,
    /// Before and after on one photo, divided by a line you drag.
    Wipe,
}

enum Msg {
    /// Folder generation (so thumbs of a folder you left are dropped), photo, picture, and its sidecar summary.
    Thumb(usize, PathBuf, ColorImage, Option<Meta>),
    Loaded(PathBuf, Result<Box<Opened>, String>),
    /// A preview render finished on the render thread.
    Rendered(Box<RenderOut>),
    Status(String),
    /// A chart profile was fitted off the UI thread: photo generation, profile name, result.
    ChartFit(usize, String, Result<crate::calib::Fit, String>),
    /// A culling view: the data, or None when it could not be read. A frame skipped because you stepped past
    /// it sends nothing but `skipped`, so it is tried again when you come back.
    Cull(PathBuf, Option<CullData>, bool),
    Denoised(PathBuf, Result<Vec<f32>, String>),
    /// Search details for a folder generation.
    Indexed(usize, Vec<(PathBuf, crate::library::Entry)>),
    /// A collection's photos, found (by name).
    Collection(String, Vec<PathBuf>),
    /// Infrared cleaning finished: the photo, the strength, the cleaned source (None: the scan has no infrared).
    IrCleaned(PathBuf, f32, Option<Arc<Rgb32FImage>>),
    AiInstalled(Result<(), String>),
    /// A 1:1 window finished rendering: the settings and window it shows.
    Zoomed(Params, [u32; 4], f32, ColorImage),
    /// A panorama was written (or failed).
    Pano(Result<PathBuf, String>),
    /// HDR merges finished: the files written (or the first error).
    Hdr(Result<Vec<PathBuf>, String>),
    /// A batch rename finished: what moved where.
    Renamed(Result<Vec<(PathBuf, PathBuf)>, String>),
    /// Settings synced to these frames (with their new summaries).
    Synced(Vec<(PathBuf, Meta)>),
}

/// A drag with the heal tool: painting a new stroke, or moving a spot's source (with the grab offset).
#[derive(Clone, Copy)]
enum HealDrag {
    Paint(usize),
    Source(usize, [f32; 2]),
}

/// The batch rename window: which photos, the pattern, and a preview of the first few new names.
struct RenameDlg {
    files: Vec<PathBuf>,
    pattern: String,
    roll: String,
    preview: Vec<(String, String)>,
    previewed: (String, String),
}

/// A photo read on the loader thread, with its preview sizes already made, so opening doesn't stall the UI.
struct Opened {
    full: Rgb32FImage,
    delta: Option<Vec<f32>>,
    preview: Rgb32FImage,
    draft: Rgb32FImage,
    before: ColorImage,
    auto_base: [f32; 3],
    /// The camera's tone curve for a RAW (empty for other files).
    cam: Vec<[f32; 2]>,
}

/// Settings, clipping view, draft, proof, mask overlay, and which source image (the cleaned or denoised one
/// replaces it in place, so a render of the old one must not count).
type RenderKey = (Params, bool, bool, usize, Option<usize>, usize);

/// What the render thread hands back for the preview.
struct RenderOut {
    /// Which photo this is for (see `App::photo_gen`) and the settings it shows.
    generation: usize,
    key: RenderKey,
    /// For the screen (its colour profile applied), and plain sRGB for colour readouts.
    image: ColorImage,
    srgb: ColorImage,
    bins: [[f32; 128]; 3],
    view_map: [f32; 8],
    photo_wh: [f32; 2],
    tex_scale: f32,
}

struct Photo {
    /// What renders use: `orig` with AI denoise applied at `nr_applied` strength.
    full: Arc<Rgb32FImage>,
    orig: Arc<Rgb32FImage>,
    /// AI denoise result as a difference from `orig`, once computed.
    delta: Option<Arc<Vec<f32>>>,
    nr_applied: f32,
    /// Infrared-cleaned `orig` and the strength it was cleaned at; whether the scan has infrared (None = not
    /// looked yet), a clean running, and the strength `full` was built with.
    ir_base: Option<(f32, Arc<Rgb32FImage>)>,
    has_ir: Option<bool>,
    ir_busy: bool,
    ir_applied: f32,
    preview: Arc<Rgb32FImage>,
    draft: Arc<Rgb32FImage>,
    auto_base: [f32; 3],
}

pub struct App {
    module: Module,
    /// Search details per photo (built in the background the first time a search is typed in a folder), the
    /// folder generation they belong to, and whether a build is running.
    search: std::collections::HashMap<PathBuf, crate::library::Entry>,
    search_gen: usize,
    search_busy: bool,
    /// Collections, the one open (shown instead of a folder), a new one being named (with its files), and the
    /// keyword being typed.
    collections: Vec<crate::library::Collection>,
    collection: Option<String>,
    new_coll: Option<(String, Vec<PathBuf>)>,
    kw_text: String,
    /// Roll notes of the open folder (see `roll`), and which folder they were read from.
    roll: crate::roll::Roll,
    roll_dir: Option<PathBuf>,
    folder: Option<PathBuf>,
    files: Vec<PathBuf>,
    /// Thumbnail textures, at most THUMB_KEEP; the rest are reloaded from the disk cache when they come into view.
    thumbs: std::collections::HashMap<PathBuf, TextureHandle>,
    /// When each thumbnail was last drawn (frame number), and the ones wanted but not loaded.
    thumb_used: std::cell::RefCell<std::collections::HashMap<PathBuf, u64>>,
    thumb_want: std::cell::RefCell<Vec<PathBuf>>,
    thumb_pending: std::collections::HashSet<PathBuf>,
    /// Photos whose thumbnail was made at least once in this folder (for the progress bar).
    thumb_ready: std::collections::HashSet<PathBuf>,
    frame: u64,
    thumb_gen: Arc<std::sync::atomic::AtomicUsize>,
    /// Bumped by every `select`: a load still queued for an earlier frame is dropped before decoding.
    load_gen: Arc<std::sync::atomic::AtomicUsize>,
    /// Why the open frame could not be opened, shown in place of the photo.
    open_error: Option<String>,
    /// Edited / film scan / rating / flag of each photo with a sidecar. Read once per folder, not every frame.
    meta: std::collections::HashMap<PathBuf, Meta>,
    /// 0 all, 1 = 1 star or more, 2 = 3 stars or more, 3 = picks only. Rejects are hidden except under "All".
    lib_rating: usize,
    subdirs: Vec<PathBuf>,
    thumb_size: f32,
    lib_sel: Option<PathBuf>,
    lib_query: String,
    lib_filter: [bool; 5], // RAW, JPEG, TIFF, Other, Edited only
    current: Option<PathBuf>,
    photo: Option<Photo>,
    loading: bool,
    params: Params,
    prev_params: Option<Params>,
    rendered: Option<RenderKey>,
    /// A preview render is running on the render thread (only one at a time; the newest settings go next).
    render_busy: bool,
    /// Counts photos opened, so a render of the previous photo that finishes late is dropped.
    photo_gen: usize,
    tex_scale: f32,
    tex: Option<TextureHandle>,
    before_tex: Option<TextureHandle>,
    last_rgb: Option<ColorImage>,
    hover_rgb: Option<[u8; 3]>,
    shown_w: f32,
    bins: [[f32; 128]; 3],
    clip_view: bool,
    history: Vec<Params>,
    hist_pos: usize,
    copied: Option<Params>,
    view: View,
    holding_before: bool,
    picking_base: bool,
    status: String,
    presets: Vec<(String, Params)>,
    preset_tex: Vec<TextureHandle>,
    last_preset: Option<usize>,
    /// Preset Amount: the settings before the preset, what the preset gave at 100%, and the amount (0–200).
    preset_mix: Option<(Params, Params)>,
    preset_amt: f32,
    preset_name: String,
    stock_query: String,
    /// Your own film stock profiles, and the name for the next one saved.
    my_stocks: Vec<(PathBuf, negative::CustomStock)>,
    my_stock_name: String,
    /// Paper preview (soft proof): 0 off, else an index into output::PROOF_PAPERS + 1.
    proof: usize,
    stock_tab: usize,
    mixer_tab: usize,
    open: [bool; 14],
    zoom: Option<Zoom>,
    curve_chan: usize,
    curve_grab: Option<usize>,
    /// The tone curve in its own large window.
    curve_big: bool,
    /// Colour sample points pinned on the photo (fractions of the photo without its border), for this session.
    samples: Vec<[f32; 2]>,
    rename_dlg: Option<RenameDlg>,
    mask_ui: MaskUi,
    tool: Tool,
    chart: ChartUi,
    /// Saved camera and scanner profiles (Calibration).
    profiles: Vec<crate::calib::ColorProfile>,
    crop_grab: Option<usize>,
    crop_aspect: usize,
    spot_size: f32,
    /// Heal tool: new strokes clone instead of heal; the spot being edited; what a drag is doing.
    heal_clone: bool,
    spot_sel: Option<usize>,
    heal_drag: Option<HealDrag>,
    /// Canvas w/h, photo offset on it, uncropped frame w/h and the crop's offset in it (render pixels).
    view_map: [f32; 8],
    photo_wh: [f32; 2],
    export: ExportOpts,
    /// Some(false) = export this photo, Some(true) = export the folder.
    export_dialog: Option<bool>,
    keymap: Keymap,
    keys_open: bool,
    /// Screen profile and printer proofing settings, and the loaded printer profile.
    colour: crate::colour::Settings,
    printer: Option<Arc<crate::colour::Proof>>,
    /// Shortcut being set in the editor: waiting for a key.
    binding: Option<&'static str>,
    picking_wb: bool,
    /// The filmstrip thumbnail needs the next full render.
    thumb_due: bool,
    lens_query: String,
    /// Library multi-selection (Ctrl/Cmd-click, Shift-click), for panoramas.
    lib_multi: Vec<PathBuf>,
    pano_busy: bool,
    /// Photos picked in the library for "Export selected" (empty = the whole folder).
    export_sel: Vec<PathBuf>,
    /// Which one-click sets (Instagram, Story, Portfolio) to make.
    export_sets: [bool; 3],
    /// Folder watch: pick up new files, open them, give them the current settings.
    watch: bool,
    watch_open: bool,
    watch_apply: bool,
    watch_at: f64,
    watch_sizes: std::collections::HashMap<PathBuf, u64>,
    /// Before/after split: where the divider is (0..1 across the photo).
    wipe: f32,
    wipe_tex: Option<(Params, TextureHandle)>,
    cull: CullState,
    scan: ScanUi,
    ai_busy: bool,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
}

pub fn run(start: Option<PathBuf>) -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1440.0, 900.0]).with_min_inner_size([1100.0, 640.0]).with_title("ferrotype").with_icon(app_icon()),
        ..Default::default()
    };
    crate::gpu::warm_up();
    std::thread::spawn(|| {
        if let Some(d) = crate::keys::dirs_cache() {
            crate::keys::trim_cache(&d.join("thumbs"), THUMB_CACHE_MAX);
        }
    });
    eframe::run_native("Ferrotype", opts, Box::new(move |cc| {
        install_style(&cc.egui_ctx);
        let mut app = App::new();
        crate::colour::set_screen(&app.colour.screen);
        if !app.colour.printer.is_empty() {
            app.load_printer();
        }
        if let Some(p) = start {
            app.open_path(&cc.egui_ctx, &p);
        }
        Ok(Box::new(app))
    }))
}

/// The window and taskbar icon: the brand's light tile (assets/brand/icon.svg rendered to 256 px).
fn app_icon() -> std::sync::Arc<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../assets/brand/png/ferrotype-256.png")).map(|i| i.to_rgba8());
    std::sync::Arc::new(match img {
        Ok(i) => egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() },
        Err(_) => egui::IconData::default(),
    })
}

fn user_preset_dir() -> Option<PathBuf> {
    Some(crate::keys::dirs_config()?.join("presets"))
}

fn builtin_presets() -> Vec<(String, Params)> {
    let d = Params::default();
    vec![
        ("Ferro Natural".into(), d.clone()),
        ("Punchy".into(), Params { contrast: 25.0, vibrance: 25.0, clarity: 15.0, blacks: -10.0, ..d.clone() }),
        ("Soft film".into(), Params { contrast: -15.0, highlights: -30.0, shadows: 20.0, blacks: 15.0, saturation: -10.0, gr_amt: 20.0, c_sh: 15.0, ..d.clone() }),
        ("Warm matte".into(), Params { temp: 6300.0, blacks: 25.0, c_sh: 20.0, c_hi: -10.0, vibrance: -5.0, grade: [[35.0, 20.0, 0.0], [0.0; 3], [45.0, 15.0, 0.0], [0.0; 3]], ..d.clone() }),
        ("Teal & orange".into(), Params { grade: [[195.0, 30.0, 0.0], [0.0; 3], [35.0, 25.0, 0.0], [0.0; 3]], contrast: 15.0, vibrance: 10.0, ..d.clone() }),
        ("B&W classic".into(), Params { saturation: -100.0, contrast: 30.0, clarity: 20.0, gr_amt: 15.0, ..d.clone() }),
        ("B&W high key".into(), Params { saturation: -100.0, exposure: 0.6, highlights: -40.0, whites: 20.0, contrast: -10.0, ..d }),
    ]
}

/// The preview as shown: the photo rendered, the mask overlay and clipping marks, the border, the paper proof.
/// Runs on the render thread; `key` and `generation` are filled in by the caller.
fn render_preview(src: &Rgb32FImage, full_w: u32, shown: &Params, overlay: Option<usize>, clip_view: bool, proof: usize, printer: Option<Arc<crate::colour::Proof>>) -> RenderOut {
    let tex_scale = src.width() as f32 / full_w as f32;
    // Histogram from the photo alone; the border is added afterwards so it does not skew it.
    let out = develop::render(src, &Params { border: Default::default(), ..shown.clone() }, tex_scale);
    let (w, h) = out.dimensions();
    let (cw, ch, ox, oy, _) = develop::border_layout(w, h, &shown.border);
    let (fw, fh, fox, foy) = develop::frame_of(shown, w as usize, h as usize);
    let bins = histogram_bins(&out);
    let mut out = out;
    // The selected mask in red, like Lightroom's overlay.
    if let Some(m) = overlay.and_then(|i| shown.masks.get(i)) {
        let wts = develop::mask_weights(m, shown, &out, (w, h), (0, 0));
        out.par_chunks_mut(3).zip(wts.par_iter()).for_each(|(px, wt)| {
            let a = wt * 0.5;
            for (k, red) in [0.95, 0.15, 0.15].into_iter().enumerate() {
                px[k] = px[k] * (1.0 - a) + red * a;
            }
        });
    }
    if clip_view {
        // Mark clipping on the photo only, so a white or black border is not flagged.
        out.par_chunks_mut(3).for_each(|p| {
            if p.iter().any(|v| *v >= 254.5 / 255.0) {
                p.copy_from_slice(&[1.0, 0.16, 0.16]);
            } else if p.iter().all(|v| *v < 0.5 / 255.0) {
                p.copy_from_slice(&[0.16, 0.35, 1.0]);
            }
        });
    }
    let mut out = develop::add_border(out, &shown.border);
    // `proof` past the papers: the printer profile (+1), with the gamut warning (+2).
    let papers = crate::output::PROOF_PAPERS.len();
    if let Some(pr) = printer.filter(|_| proof > papers) {
        // Through the printer profile; the gamut warning marks what the print can't hold in grey-blue.
        let off = (proof == papers + 2).then(|| pr.out_of_gamut(&out));
        pr.apply(&mut out);
        if let Some(off) = off {
            out.par_chunks_mut(3).zip(off.par_iter()).for_each(|(p, o)| {
                if *o {
                    p.copy_from_slice(&[0.45, 0.55, 0.75]);
                }
            });
        }
    } else if proof > 0 && proof <= papers {
        crate::output::proof(&mut out, proof - 1);
    }
    RenderOut {
        generation: 0,
        key: (Params::default(), false, false, 0, None, 0),
        image: to_color_image(&out),
        srgb: srgb_color_image(&out),
        bins,
        view_map: [cw as f32, ch as f32, ox as f32, oy as f32, fw, fh, fox, foy],
        photo_wh: [w as f32, h as f32],
        tex_scale,
    }
}

/// An sRGB-encoded picture as 8-bit sRGB pixels (for colour readouts).
fn srgb_color_image(img: &Rgb32FImage) -> ColorImage {
    let bytes: Vec<u8> = img.par_iter().map(|v| (v * 255.0 + 0.5) as u8).collect();
    ColorImage::from_rgb([img.width() as usize, img.height() as usize], &bytes)
}

/// An sRGB-encoded picture as a texture, through the screen's colour profile when one is set.
fn to_color_image(img: &Rgb32FImage) -> ColorImage {
    let bytes: Vec<u8> = img.par_iter().map(|v| (v * 255.0 + 0.5) as u8).collect();
    ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &crate::colour::for_screen(&bytes))
}

fn encode(img: &Rgb32FImage) -> Rgb32FImage {
    let mut o = img.clone();
    o.par_iter_mut().for_each(|v| *v = linear_to_srgb(*v));
    o
}

/// UV rect that crops a texture to fill `aspect` (w/h), centred.
fn cover_uv(tex: &TextureHandle, aspect: f32) -> Rect {
    let s = tex.size_vec2();
    let a = s.x / s.y;
    if a > aspect {
        let w = aspect / a;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = a / aspect;
        Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
    }
}

/// Rendered thumbnails are kept in the user cache folder, keyed by the photo and its edits, so reopening a folder is instant.
fn thumb_cache(f: &Path) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let modified = |p: &Path| std::fs::metadata(p).ok().and_then(|m| m.modified().ok());
    // `v2`: embedded colour profiles are read since; older thumbnails of tagged JPEGs (Display P3) are stale.
    let mut key = format!("v2|{}|{:?}|{:?}", f.display(), modified(load::copy_of(f).as_deref().unwrap_or(f)), modified(&crate::sidecar(f)));
    // TIFF scans: read through their colour profile, and the roll can say its untagged scans are linear.
    if f.to_string_lossy().to_ascii_lowercase().contains(".tif") {
        let data = f.parent().and_then(crate::roll::read).map(|r| r.scan_data).unwrap_or_default();
        key += &format!("|scan2|{data}");
    }
    let h: String = Sha256::digest(key.as_bytes()).iter().take(12).map(|b| format!("{b:02x}")).collect();
    Some(crate::keys::dirs_cache()?.join("thumbs").join(format!("{h}.jpg")))
}

/// Where the picture is sharp: the strongest edges (Laplacian of luma), marked green on a transparent layer.
fn focus_map(img: &Rgb32FImage) -> ColorImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let l: Vec<f32> = img.chunks(3).map(|c| luma([c[0], c[1], c[2]])).collect();
    let mut e = vec![0f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w - 1 {
            let i = y * w + x;
            e[i] = (4.0 * l[i] - l[i - 1] - l[i + 1] - l[i - w] - l[i + w]).abs();
        }
    }
    let mut sorted: Vec<f32> = e.iter().step_by(7).cloned().collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let t = sorted.get(sorted.len() * 985 / 1000).copied().unwrap_or(1.0).max(0.06);
    let mut out = ColorImage::filled([w, h], Color32::TRANSPARENT);
    for (i, v) in e.iter().enumerate() {
        if *v >= t {
            out.pixels[i] = Color32::from_rgba_unmultiplied(60, 255, 90, 230);
        }
    }
    out
}

fn ext_of(p: &Path) -> String {
    p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_uppercase()
}

/// Rough film-base colour for a stock's chip in the list.
fn stock_chip(s: &negative::Stock) -> Color32 {
    match s.kind {
        Kind::Color => hex(0xD3824C),
        Kind::Cine => hex(0xB8744A),
        Kind::Bw if s.id == "xp2" => hex(0xD9C7A8),
        Kind::Bw => hex(0xB7B2AA),
        Kind::Slide => hex(0x5C6E8A),
    }
}

fn stock_iso(s: &negative::Stock) -> String {
    negative::iso(s).map(|v| v.to_string()).unwrap_or_default()
}

fn kind_label(k: Kind) -> &'static str {
    match k { Kind::Color => "C-41 colour negative", Kind::Cine => "ECN-2 motion picture", Kind::Bw => "Black & white", Kind::Slide => "E-6 slide, no inversion" }
}

/// Human label + value for the most significant change between two settings (history list).
fn describe_change(a: &Params, b: &Params) -> (String, String) {
    let (ja, jb) = (serde_json::to_value(a).unwrap_or_default(), serde_json::to_value(b).unwrap_or_default());
    let names: &[(&str, &str)] = &[
        ("temp", "Temp"), ("tint", "Tint"), ("exposure", "Exposure"), ("contrast", "Contrast"), ("highlights", "Highlights"), ("shadows", "Shadows"),
        ("whites", "Whites"), ("blacks", "Blacks"), ("texture", "Texture"), ("clarity", "Clarity"), ("dehaze", "Dehaze"), ("vibrance", "Vibrance"),
        ("saturation", "Saturation"), ("c_hi", "Curve highlights"), ("c_li", "Curve lights"), ("c_da", "Curve darks"), ("c_sh", "Curve shadows"),
        ("hsl", "Color mixer"), ("grade", "Color grading"), ("g_blend", "Grading blend"), ("g_bal", "Grading balance"), ("sh_amt", "Sharpening"),
        ("sh_rad", "Sharpen radius"), ("sh_det", "Sharpen detail"), ("sh_mask", "Sharpen masking"), ("nr_lum", "Luminance NR"), ("nr_det", "NR detail"),
        ("nr_col", "Color NR"), ("o_dist", "Distortion"), ("o_vig", "Lens vignetting"), ("o_fringe", "Defringe"), ("t_vert", "Vertical"), ("t_hor", "Horizontal"),
        ("t_rot", "Rotate"), ("t_scale", "Scale"), ("flip_h", "Flip"), ("crop", "Crop"), ("vig", "Vignette"), ("vig_mid", "Vignette midpoint"),
        ("vig_round", "Vignette roundness"), ("vig_fea", "Vignette feather"), ("vig_hi", "Vignette highlights"), ("gr_amt", "Grain"), ("gr_size", "Grain size"),
        ("gr_rough", "Grain roughness"), ("cal_sh", "Shadows tint"), ("cal", "Calibration"), ("neg", "Negative"), ("border", "Border"), ("spots", "Spot removal"), ("masks", "Mask"), ("hal_amt", "Halation"), ("hal_size", "Halation size"), ("hal_thr", "Halation threshold"), ("curves", "Point curve"), ("lut", "LUT"), ("lut_amt", "LUT amount"), ("rating", "Rating"), ("flag", "Flag"),
        ("rot90", "Rotate 90°"), ("profile", "Profile"), ("label", "Label"), ("ai_nr", "AI denoise"), ("ir_clean", "Infrared clean"), ("snapshots", "Snapshot"),
    ];
    let changed: Vec<&(&str, &str)> = names.iter().filter(|(k, _)| ja.get(k) != jb.get(k)).collect();
    match changed.as_slice() {
        [] => ("No change".into(), String::new()),
        [(k, n)] => {
            let val = jb.get(k).and_then(|v| v.as_f64()).map(|v| if v.abs() < 10.0 && v.fract() != 0.0 { format!("{v:+.2}") } else if *k == "temp" { format!("{v:.0}") } else { format!("{v:+.0}") }).unwrap_or_default();
            (n.to_string(), val)
        }
        // Upright (or its Off button) moves several transform sliders at once.
        many if many.iter().all(|(k, _)| k.starts_with("t_")) => ("Straighten".into(), String::new()),
        many => (format!("{} +{}", many[0].1, many.len() - 1), String::new()),
    }
}

// ---------- gradient generators: each returns the colour a slider value actually produces ----------

fn neutral_through(gains: [f32; 3]) -> Color32 {
    let m = gains[0].max(gains[1]).max(gains[2]);
    lin(gains.map(|g| g / m * 0.55))
}

impl App {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let mut presets = builtin_presets();
        if let Some(dir) = user_preset_dir() {
            let mut user: Vec<(String, Params)> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| {
                let p = e.path();
                let name = p.file_stem()?.to_str()?.to_string();
                let mut q: Params = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
                q.migrate();
                Some((name, q))
            }).collect();
            user.sort_by(|a, b| a.0.cmp(&b.0));
            presets.extend(user);
        }
        Self {
            module: Module::Library, folder: None, files: vec![], thumbs: Default::default(), thumb_used: Default::default(), thumb_want: Default::default(), thumb_pending: Default::default(), thumb_ready: Default::default(), frame: 0, thumb_gen: Default::default(), load_gen: Default::default(), open_error: None, meta: Default::default(), lib_rating: 0,
            subdirs: vec![], tex_scale: 1.0, thumb_size: 190.0, lib_sel: None, lib_query: String::new(),
            lib_filter: [true, true, true, true, false], current: None, photo: None, loading: false, params: Params::default(), prev_params: None,
            rendered: None, render_busy: false, photo_gen: 0, tex: None, before_tex: None, last_rgb: None, hover_rgb: None, shown_w: 0.0, bins: [[0.0; 128]; 3], clip_view: false,
            history: vec![Params::default()], hist_pos: 0, copied: None, view: View::After, holding_before: false, picking_base: false,
            status: String::new(), presets, preset_tex: vec![], last_preset: None, preset_mix: None, preset_amt: 100.0, preset_name: String::new(), stock_query: String::new(), my_stocks: negative::custom_stocks(), my_stock_name: String::new(), proof: 0,
            stock_tab: 0, mixer_tab: 0, open: [true, false, false, false, false, false, false, false, false, false, false, false, false, false], tool: Tool::None,
            zoom: None, curve_chan: 0, curve_grab: None, curve_big: false, samples: vec![], rename_dlg: None, mask_ui: MaskUi::default(), chart: ChartUi::new(), profiles: crate::calib::saved(),
            crop_grab: None, crop_aspect: 0, spot_size: 2.0, heal_clone: false, spot_sel: None, heal_drag: None, view_map: [1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0], photo_wh: [1.0, 1.0],
            export: ExportOpts::load(), export_dialog: None, export_sel: vec![], export_sets: [false; 3], keymap: Keymap::load(), keys_open: false, colour: crate::colour::Settings::load(), printer: None, binding: None, picking_wb: false,
            thumb_due: false, lens_query: String::new(), lib_multi: vec![], pano_busy: false,
            watch: false, watch_open: true, watch_apply: false, watch_at: 0.0, watch_sizes: Default::default(), wipe: 0.5, wipe_tex: None,
            cull: Default::default(), scan: ScanUi::default(), ai_busy: false, tx, rx, roll: Default::default(), roll_dir: None,
            search: Default::default(), search_gen: 0, search_busy: false, collections: crate::library::load(), collection: None, new_coll: None, kw_text: String::new(),
        }
    }

    // ---------- files ----------

    fn open_path(&mut self, ctx: &egui::Context, path: &Path) {
        // `ferrotype photo.jpg` has an empty parent: that means the current folder.
        let (dir, file) = if path.is_dir() { (path.to_path_buf(), None) } else { (path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf(), Some(path.to_path_buf())) };
        let entries: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        let files = crate::photos_in(&entries);
        self.subdirs = entries.into_iter().filter(|p| p.is_dir() && !p.file_name().unwrap_or_default().to_string_lossy().starts_with('.')).collect();
        self.subdirs.sort();
        self.collection = None;
        // Ratings and edit marks arrive with each thumbnail: reading every sidecar here froze the window on cloud folders.
        self.meta.clear();
        self.cull.failed.clear();
        self.show_files(ctx, files, Some(dir));
        if let Some(f) = file {
            self.select(ctx, f);
        }
    }

    /// Show these photos in the Library: a folder's (with `dir`), or a collection's (from anywhere).
    fn show_files(&mut self, ctx: &egui::Context, files: Vec<PathBuf>, dir: Option<PathBuf>) {
        if dir.is_none() {
            self.subdirs.clear();
            self.meta.clear();
        }
        self.folder = dir;
        self.files = files.clone();
        self.thumbs.clear();
        self.thumb_used.borrow_mut().clear();
        self.thumb_pending.clear();
        self.thumb_ready.clear();
        self.lib_sel = files.first().cloned();
        self.lib_multi.clear();
        self.search.clear();
        use std::sync::atomic::Ordering::Relaxed;
        let gen_now = self.thumb_gen.fetch_add(1, Relaxed) + 1;
        self.spawn_thumbs(ctx, files, gen_now);
    }

    /// Build the search details for the photos shown, in the background, the first time they're searched.
    fn ensure_search(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let generation = self.thumb_gen.load(Relaxed);
        if self.lib_query.trim().is_empty() || self.search_busy || self.search_gen == generation {
            return;
        }
        self.search_busy = true;
        let (files, tx) = (self.files.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let rolls = crate::library::Rolls::default();
            let entries: Vec<(PathBuf, crate::library::Entry)> = files.par_iter().map(|f| {
                let p = read_params(f).unwrap_or_default();
                (f.clone(), rolls.entry(f, &p))
            }).collect();
            let _ = tx.send(Msg::Indexed(generation, entries));
        });
    }

    /// Refresh one photo's search details after its keywords or rating changed.
    fn reindex(&mut self, f: &Path) {
        if self.search.contains_key(f) {
            let p = read_params(f).unwrap_or_default();
            self.search.insert(f.to_path_buf(), crate::library::entry(f, &p));
        }
    }

    /// Open a collection: its photos are found off the UI thread (a smart one searches its folder tree).
    fn open_collection(&mut self, i: usize) {
        let Some(c) = self.collections.get(i).cloned() else { return };
        self.status = format!("Opening {}…", c.name);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let files = crate::library::members(&c);
            let _ = tx.send(Msg::Collection(c.name, files));
        });
    }

    fn save_collections(&mut self) {
        if let Err(e) = crate::library::save(&self.collections) {
            self.status = format!("Could not save collections: {e}");
        }
    }

    /// Decode (or read from the cache) thumbnails for `files` in the background.
    fn spawn_thumbs(&self, ctx: &egui::Context, files: Vec<PathBuf>, gen_now: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let (tx, ctx2, gen_) = (self.tx.clone(), ctx.clone(), self.thumb_gen.clone());
        std::thread::spawn(move || {
            files.par_iter().for_each(|f| {
                if gen_.load(Relaxed) != gen_now {
                    return; // another folder was opened: stop decoding this one
                }
                let params = read_params(f);
                let meta = params.as_ref().map(Meta::of);
                let cached = thumb_cache(f).and_then(|c| image::open(c).ok()).map(|i| {
                    let i = i.to_rgb8();
                    ColorImage::from_rgba_unmultiplied([i.width() as usize, i.height() as usize], &crate::colour::for_screen(i.as_raw()))
                });
                let made = cached.or_else(|| {
                    let (img, toned) = load::load_quick_toned(f, THUMB).ok()?;
                    let mut p = params.clone().unwrap_or_default();
                    if toned {
                        p.cam_curve.clear(); // the camera's JPEG already has it
                    }
                    let out = develop::render(&img, &p, 0.05);
                    // The cache keeps sRGB; the screen's profile is applied when it's shown.
                    if let Some(c) = thumb_cache(f) {
                        let _ = std::fs::create_dir_all(c.parent().unwrap());
                        let rgb: Vec<u8> = out.iter().map(|v| (v * 255.0 + 0.5) as u8).collect();
                        let _ = image::save_buffer(&c, &rgb, out.width(), out.height(), image::ExtendedColorType::Rgb8);
                    }
                    Some(to_color_image(&out))
                });
                // Sent even without a picture, so the rating still arrives.
                let _ = tx.send(Msg::Thumb(gen_now, f.clone(), made.unwrap_or_default(), meta));
                ctx2.request_repaint();
            });
        });
    }

    /// Folder watch: every two seconds, pick up photos that arrived in the open folder (a scanner or tethered camera
    /// saving there). A file counts once its size stops changing. Optionally open it with the current photo's settings.
    fn watch_folder(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if !self.watch || now - self.watch_at < 2.0 {
            if self.watch {
                ctx.request_repaint_after(std::time::Duration::from_secs(2));
            }
            return;
        }
        self.watch_at = now;
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
        let Some(dir) = self.folder.clone() else { return };
        let mut ready = vec![];
        // A set, not `files.contains`: this runs every two seconds and big folders made it quadratic.
        let known: std::collections::HashSet<&PathBuf> = self.files.iter().collect();
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let f = e.path();
            if known.contains(&f) || !f.is_file() || !load::is_supported(&f) {
                continue;
            }
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            if size > 0 && self.watch_sizes.get(&f) == Some(&size) {
                ready.push(f);
            } else {
                self.watch_sizes.insert(f, size);
            }
        }
        drop(known);
        if ready.is_empty() {
            return;
        }
        ready.sort();
        for f in &ready {
            self.watch_sizes.remove(f);
            if self.watch_apply
                && self.current.is_some()
                && !crate::sidecar(f).exists()
            {
                // The new frame gets this photo's look and film conversion, but its own geometry and rating.
                let mut p = Params::default().with_look_of(&self.params);
                p.neg = self.params.neg.clone();
                p.profile = self.params.profile.clone();
                let _ = write_params(f, &p);
                self.meta.insert(f.clone(), Meta::of(&p));
            }
        }
        self.files.extend(ready.iter().cloned());
        load::sort_photos(&mut self.files);
        self.spawn_thumbs(ctx, ready.clone(), self.thumb_gen.load(std::sync::atomic::Ordering::Relaxed));
        let newest = ready.last().unwrap().clone();
        self.status = format!("New: {}", newest.file_name().unwrap_or_default().to_string_lossy());
        if self.watch_open {
            self.select(ctx, newest);
        }
    }

    fn select(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.commit();
        if self.current.is_some() {
            self.prev_params = Some(self.params.clone());
        }
        self.params = read_params(&path).unwrap_or_default();
        if crate::sidecar_damaged(&path) {
            self.status = "This photo's edits file was damaged, so it starts fresh. The old file is kept as .damaged".into();
        }
        self.history = vec![self.params.clone()];
        self.hist_pos = 0;
        self.current = Some(path.clone());
        self.lib_sel = Some(path.clone());
        self.photo = None;
        self.rendered = None;
        self.loading = true;
        self.open_error = None;
        self.picking_base = false;
        self.last_preset = None;
        self.samples.clear();
        self.spot_sel = None;
        self.zoom = None;
        if self.module == Module::Library {
            self.module = if self.params.neg.enabled { Module::Negative } else { Module::Develop };
        }
        let (tx, ctx2) = (self.tx.clone(), ctx.clone());
        let nr = self.params.ai_nr > 0.0;
        let (load_gen, mine) = (self.load_gen.clone(), self.load_gen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1);
        std::thread::spawn(move || {
            let _one = DECODE.lock().unwrap_or_else(|e| e.into_inner());
            if load_gen.load(std::sync::atomic::Ordering::Relaxed) != mine {
                return; // another frame was chosen while this one waited
            }
            // A finished AI denoise is read back from its cache rather than run again.
            // The camera's JPEG (for its tone curve) is read while the RAW decodes.
            let raw_photo = load::is_raw_photo(&path);
            let (r, jpg) = rayon::join(|| load::load(&path), || if raw_photo { load::camera_jpeg_lumas(&path) } else { None });
            let r = r.map(|full| {
                let delta = if nr { denoise::cached_delta(&path, full.len()) } else { None };
                let preview = load::fit(&full, PREVIEW);
                let draft = load::fit(&preview, DRAFT);
                let cam = load::camera_curve_from(&full, jpg);
                // "Before" is the photo as the camera rendered it: with its tone curve, without edits.
                let mut shown = preview.clone();
                develop::apply_cam_curve(&mut shown, &cam);
                let before = to_color_image(&encode(&shown));
                let auto_base = negative::auto_base(&preview);
                Box::new(Opened { full, delta, preview, draft, before, auto_base, cam })
            });
            let _ = tx.send(Msg::Loaded(path, r));
            ctx2.request_repaint();
        });
    }

    fn step(&mut self, ctx: &egui::Context, d: isize) {
        let Some(cur) = &self.current else { return };
        if let Some(i) = self.files.iter().position(|f| f == cur) {
            let j = (i as isize + d).clamp(0, self.files.len() as isize - 1) as usize;
            if j != i {
                self.select(ctx, self.files[j].clone());
            }
        }
    }

    /// The thumbnail texture for `f`, if loaded; otherwise asks for it. Marks it as in use.
    fn thumb(&self, f: &Path) -> Option<&TextureHandle> {
        let t = self.thumbs.get(f);
        if t.is_some() {
            self.thumb_used.borrow_mut().insert(f.to_path_buf(), self.frame);
        } else if self.thumb_ready.contains(f) {
            self.thumb_want.borrow_mut().push(f.to_path_buf());
        }
        t
    }

    /// Keep at most THUMB_KEEP textures (dropping the longest unseen), and reload the evicted ones that came back into view.
    fn manage_thumbs(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        if self.thumbs.len() > THUMB_KEEP {
            let mut used = self.thumb_used.borrow_mut();
            let mut by_age: Vec<(u64, PathBuf)> = self.thumbs.keys().map(|k| (used.get(k).copied().unwrap_or(0), k.clone())).collect();
            by_age.sort();
            for (_, k) in by_age.into_iter().take(self.thumbs.len() - THUMB_KEEP * 3 / 4) {
                self.thumbs.remove(&k);
                used.remove(&k);
            }
        }
        let want: Vec<PathBuf> = std::mem::take(&mut *self.thumb_want.borrow_mut()).into_iter()
            .filter(|f| !self.thumbs.contains_key(f) && self.thumb_pending.insert(f.clone())).collect();
        if !want.is_empty() {
            self.spawn_thumbs(ctx, want, self.thumb_gen.load(std::sync::atomic::Ordering::Relaxed));
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        self.manage_thumbs(ctx);
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Thumb(g, p, img, meta) => {
                    if g == self.thumb_gen.load(std::sync::atomic::Ordering::Relaxed) {
                        self.thumb_pending.remove(&p);
                        // An edit made while this was in flight is newer: keep it.
                        if let Some(m) = meta {
                            self.meta.entry(p.clone()).or_insert(m);
                        }
                        if img.size[0] > 0 {
                            let t = ctx.load_texture(p.display().to_string(), img, TextureOptions::LINEAR);
                            self.thumbs.insert(p.clone(), t);
                            self.thumb_used.borrow_mut().entry(p.clone()).or_insert(self.frame);
                            self.thumb_ready.insert(p);
                        }
                    }
                }
                Msg::Loaded(p, r) if Some(&p) == self.current.as_ref() => {
                    self.loading = false;
                    match r {
                        Ok(o) => {
                            let Opened { full, delta, preview, draft, before, auto_base, cam } = *o;
                            // First open of a RAW: measure the camera's curve once. It's part of the photo, so the
                            // whole history gets it and it isn't an edit. Edits saved before Ferrotype had it were
                            // made on the flat look: those keep it (`main::wants_cam_curve`).
                            if crate::wants_cam_curve(&self.params) && !cam.is_empty() {
                                self.params.cam_curve = cam.clone();
                                for h in &mut self.history {
                                    h.cam_curve = cam.clone();
                                }
                            }
                            self.before_tex = Some(ctx.load_texture("before", before, TextureOptions::LINEAR));
                            // Keep the damaged-edits warning from `select` visible: the size alone used to replace it
                            // the moment the photo opened.
                            self.status = if crate::sidecar_damaged(&p) {
                                format!("{} × {}. This photo's edits file was damaged, so it starts fresh. The old file is kept as .damaged", full.width(), full.height())
                            } else {
                                format!("{} × {}", full.width(), full.height())
                            };
                            let full = Arc::new(full);
                            self.photo = Some(Photo { full: full.clone(), orig: full, delta: delta.map(Arc::new), nr_applied: 0.0, ir_base: None, has_ir: None, ir_busy: false, ir_applied: 0.0, preview: Arc::new(preview), draft: Arc::new(draft), auto_base });
                            // The previous photo's picture must not show under this one's name.
                            self.tex = None;
                            self.photo_gen += 1;
                            self.preset_tex.clear();
                            self.convert_if_negative();
                        }
                        Err(e) => {
                            self.status = format!("Could not open: {e}");
                            self.open_error = Some(e);
                        }
                    }
                }
                Msg::Loaded(..) => {}
                Msg::Rendered(r) => {
                    self.render_busy = false;
                    if r.generation == self.photo_gen && self.photo.is_some() {
                        self.apply_render(ctx, *r);
                    }
                }
                Msg::Status(s) => self.status = s,
                Msg::ChartFit(generation, name, fit) => {
                    self.chart.busy = false;
                    if generation == self.photo_gen {
                        self.finish_chart_profile(name, fit);
                    }
                }
                Msg::Synced(done) => {
                    self.status = format!("Synced to {} frame{}", done.len(), if done.len() == 1 { "" } else { "s" });
                    // Only what the sync changed: a rating, flag or label set while it ran is newer than the
                    // thread's copy, and Move rejects reads this list.
                    for (p, m) in done {
                        let e = self.meta.entry(p).or_insert(m);
                        e.edited = m.edited;
                        e.neg = m.neg;
                    }
                }
                Msg::Cull(p, d, skipped) => {
                    self.cull.pending.remove(&p);
                    if d.is_none() && !skipped {
                        self.cull.failed.insert(p.clone());
                    }
                    if let Some(d) = d {
                        let name = p.display().to_string();
                        let tex = ctx.load_texture(format!("cull {name}"), d.img, TextureOptions::LINEAR);
                        let focus = ctx.load_texture(format!("focus {name}"), d.focus, TextureOptions::LINEAR);
                        self.cull.cache.insert(p, CullImg { tex, focus, bins: d.bins, clipped: d.clipped, raw: d.raw });
                    }
                }
                Msg::Indexed(generation, entries) => {
                    self.search_busy = false;
                    if generation == self.thumb_gen.load(std::sync::atomic::Ordering::Relaxed) {
                        self.search = entries.into_iter().collect();
                        self.search_gen = generation;
                    }
                }
                Msg::Collection(name, files) => {
                    self.show_files(ctx, files, None);
                    self.status = format!("{name}: {} photo{}", self.files.len(), if self.files.len() == 1 { "" } else { "s" });
                    self.collection = Some(name);
                }
                Msg::IrCleaned(p, amt, r) => {
                    if let Some(ph) = &mut self.photo
                        && Some(&p) == self.current.as_ref()
                    {
                        ph.ir_busy = false;
                        ph.has_ir = Some(r.is_some());
                        match r {
                            Some(img) => ph.ir_base = Some((amt, img)),
                            None => self.status = "This scan has no infrared channel".into(),
                        }
                    }
                }
                Msg::Denoised(p, r) => {
                    self.ai_busy = false;
                    match r {
                        Ok(d) => {
                            if let Some(ph) = &mut self.photo
                                && Some(&p) == self.current.as_ref()
                            {
                                ph.delta = Some(Arc::new(d));
                                ph.nr_applied = -1.0; // re-apply
                            }
                            self.status = "AI denoise ready".into();
                        }
                        Err(e) => {
                            self.status = format!("AI denoise failed: {e}");
                            self.params.ai_nr = 0.0;
                        }
                    }
                }
                Msg::Zoomed(p, win, sc, ci) => {
                    if let Some(z) = &mut self.zoom {
                        // Pixels stay crisp when magnified; smooth when shown at or below their size.
                        let filter = if sc >= 1.0 && z.scale > 1.5 { TextureOptions::NEAREST } else { TextureOptions::LINEAR };
                        match &mut z.tex {
                            Some(t) => t.set(ci, filter),
                            None => z.tex = Some(ctx.load_texture("zoom", ci, filter)),
                        }
                        z.shown = Some((p, win, sc));
                        z.busy = false;
                    }
                }
                Msg::Pano(r) => {
                    self.pano_busy = false;
                    match r {
                        Ok(out) => {
                            self.status = format!("Panorama saved as {}", out.file_name().unwrap_or_default().to_string_lossy());
                            if let Some(d) = self.folder.clone() {
                                self.open_path(ctx, &d);
                            }
                            self.lib_sel = Some(out);
                            self.lib_multi.clear();
                        }
                        Err(e) => self.status = format!("Panorama failed: {e}"),
                    }
                }
                Msg::Hdr(r) => {
                    self.pano_busy = false;
                    match r {
                        Ok(outs) if outs.is_empty() => self.status = "No exposure brackets found. A bracket is photos taken within 3 seconds at different exposures".into(),
                        Ok(outs) => {
                            self.status = if outs.len() == 1 { format!("HDR saved as {}", outs[0].file_name().unwrap_or_default().to_string_lossy()) } else { format!("Merged {} brackets into HDR photos", outs.len()) };
                            if let Some(d) = self.folder.clone() {
                                self.open_path(ctx, &d);
                            }
                            self.lib_sel = outs.first().cloned();
                            self.lib_multi.clear();
                        }
                        Err(e) => self.status = format!("HDR merge failed: {e}"),
                    }
                }
                Msg::Renamed(r) => match r {
                    Ok(moved) => {
                        let photos = moved.iter().filter(|(a, _)| load::is_supported(a)).count();
                        self.status = format!("Renamed {photos} photos. Right-click a photo to undo.");
                        let map = |p: &mut Option<PathBuf>| if let Some(new) = p.as_ref().and_then(|c| moved.iter().find(|(a, _)| a == c)).map(|(_, b)| b.clone()) { *p = Some(new) };
                        map(&mut self.current);
                        map(&mut self.lib_sel);
                        self.lib_multi.clear();
                        if let Some(d) = self.folder.clone() {
                            self.open_path(ctx, &d);
                        }
                    }
                    Err(e) => self.status = format!("Rename failed: {e}"),
                },
                Msg::AiInstalled(r) => {
                    self.ai_busy = false;
                    self.status = match r { Ok(()) => "AI denoise installed".into(), Err(e) => format!("AI denoise download failed: {e}") };
                }
            }
        }
    }

    /// Record a history step and save the sidecar once a slider is released.
    /// In the Negative view, a frame with an orange film base (C-41, ECN-2) is converted straight away, so the
    /// Positive view shows a positive. Other frames (a slide, B&W, a positive opened here by mistake) wait for the
    /// switch or a stock. It's an ordinary edit: undo or the switch turns it off.
    fn convert_if_negative(&mut self) {
        let Some(base) = self.auto_base() else { return };
        if self.module == Module::Negative && !self.params.neg.enabled && negative::orange_mask(base) {
            self.params.neg.enabled = true;
            self.status = "Found an orange film base, so this was converted as a colour negative".into();
        }
    }

    fn commit(&mut self) {
        if self.history.get(self.hist_pos) == Some(&self.params) {
            return;
        }
        // Straightening or a lens profile moves the photo: heal spots, filters and brush strokes go with it.
        if let (Some(prev), Some(ph)) = (self.history.get(self.hist_pos), &self.photo) {
            develop::remap_marks(prev, &mut self.params, ph.preview.width(), ph.preview.height());
        }
        self.history.truncate(self.hist_pos + 1);
        self.history.push(self.params.clone());
        // Each step is a full copy (brush masks included), so keep the most recent ones only.
        if self.history.len() > HISTORY_MAX {
            self.history.remove(0);
        }
        self.hist_pos = self.history.len() - 1;
        self.save();
    }

    /// Write the current settings to the sidecar and refresh this photo's thumbnail from the last render.
    fn save(&mut self) {
        let Some(cur) = self.current.clone() else { return };
        match write_params(&cur, &self.params) {
            Ok(()) => {
                self.meta.insert(cur.clone(), Meta::of(&self.params));
            }
            Err(e) => self.status = format!("Could not save edits: {e}"),
        }
        // The thumbnail follows from the next full render: the last one may predate this change (undo).
        self.thumb_due = true;
    }

    fn refresh_thumb(&mut self) {
        let Some(cur) = self.current.clone() else { return };
        if let (Some(ci), Some(t)) = (&self.last_rgb, self.thumbs.get_mut(&cur))
            && let Some(rgba) = image::RgbaImage::from_raw(ci.size[0] as u32, ci.size[1] as u32, ci.as_raw().to_vec())
        {
            let small = image::imageops::thumbnail(&rgba, THUMB, THUMB * ci.size[1] as u32 / ci.size[0].max(1) as u32);
            let rgb: Vec<u8> = small.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
            t.set(ColorImage::from_rgba_unmultiplied([small.width() as usize, small.height() as usize], &crate::colour::for_screen(&rgb)), TextureOptions::LINEAR);
        }
        self.thumb_due = false;
    }

    fn refresh_render(&mut self, ctx: &egui::Context) {
        self.update_denoise(ctx);
        let Some(photo) = &self.photo else { return };
        if self.preset_tex.is_empty() {
            let small = load::fit(&photo.preview, 96);
            self.preset_tex = self.presets.iter().enumerate().map(|(i, (_, p))| {
                let q = self.params.with_look_of(&Params { border: self.params.border.clone(), ..p.clone() });
                ctx.load_texture(format!("preset{i}"), to_color_image(&develop::render(&small, &q, 0.05)), TextureOptions::LINEAR)
            }).collect();
        }
        // While a slider is held, render the small draft; the full preview follows on release.
        let busy = ctx.input(|i| i.pointer.any_down()) || ctx.memory(|m| m.has_focus(egui::Id::NULL.with("caption")));
        let draft = busy && self.history.get(self.hist_pos) != Some(&self.params);
        // Cropping and placing a chart show the whole frame, without the border, with the crop drawn on top.
        let shown = if matches!(self.tool, Tool::Crop | Tool::Chart) { Params { crop: [0.0; 4], border: Default::default(), ..self.params.clone() } } else { self.params.clone() };
        let overlay = (self.tool == Tool::Mask && self.mask_ui.overlay && self.mask_ui.sel < shown.masks.len()).then_some(self.mask_ui.sel);
        let key = (shown.clone(), self.clip_view, draft, self.proof, overlay, Arc::as_ptr(&photo.preview) as usize);
        if self.rendered.as_ref() == Some(&key) {
            return;
        }
        // One render at a time, off the UI thread, so sliders and panning never wait for it. When it
        // lands, the next frame sees whether the settings moved on meanwhile and starts the newest.
        if self.render_busy {
            return;
        }
        self.render_busy = true;
        let src = if draft { photo.draft.clone() } else { photo.preview.clone() };
        let full_w = photo.full.width();
        let (proof, clip_view, generation, tx, ctx2) = (self.proof, self.clip_view, self.photo_gen, self.tx.clone(), ctx.clone());
        let printer = self.printer.clone();
        std::thread::spawn(move || {
            let out = render_preview(&src, full_w, &shown, overlay, clip_view, proof, printer);
            let _ = tx.send(Msg::Rendered(Box::new(RenderOut { generation, key, ..out })));
            ctx2.request_repaint();
        });
    }

    fn apply_render(&mut self, ctx: &egui::Context, r: RenderOut) {
        let draft = r.key.2;
        let overlay = r.key.4;
        self.tex_scale = r.tex_scale;
        self.view_map = r.view_map;
        self.photo_wh = r.photo_wh;
        self.bins = r.bins;
        match &mut self.tex {
            Some(t) => t.set(r.image.clone(), TextureOptions::LINEAR),
            None => self.tex = Some(ctx.load_texture("preview", r.image.clone(), TextureOptions::LINEAR)),
        }
        self.last_rgb = Some(r.srgb);
        self.rendered = Some(r.key);
        if self.thumb_due && !draft && !matches!(self.tool, Tool::Crop | Tool::Chart) && overlay.is_none() {
            self.refresh_thumb();
        }
    }

    /// AI denoise: start the (slow, one-off) analysis when asked for, and blend its result in when the strength changes.
    fn update_denoise(&mut self, ctx: &egui::Context) {
        let amt = self.params.ai_nr;
        let (Some(ph), Some(cur)) = (&mut self.photo, &self.current) else { return };
        if amt > 0.0 && ph.delta.is_none() && !self.ai_busy && denoise::installed() {
            self.ai_busy = true;
            let (img, path, tx, ctx2) = (ph.orig.clone(), cur.clone(), self.tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let tx2 = tx.clone();
                let r = denoise::noise_delta(&img, denoise::is_raw(&path), &|d, t| {
                    let _ = tx2.send(Msg::Status(format!("AI denoise {}%", d * 100 / t.max(1))));
                    ctx2.request_repaint();
                });
                if let Ok(d) = &r {
                    denoise::store_delta(&path, d);
                }
                let _ = tx.send(Msg::Denoised(path, r));
                ctx2.request_repaint();
            });
        }
        // Infrared cleaning: off the UI thread, once the slider is let go.
        let held = ctx.input(|i| i.pointer.any_down());
        let ir_amt = self.params.ir_clean;
        if ir_amt > 0.0 && ph.has_ir != Some(false) && !ph.ir_busy && ph.ir_base.as_ref().map(|b| b.0) != Some(ir_amt) && !held {
            ph.ir_busy = true;
            let (img, path, tx, ctx2) = (ph.orig.clone(), cur.clone(), self.tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let r = scan::ir_channel(&path).map(|ir| Arc::new(scan::ir_clean(&img, &ir, ir_amt)));
                let _ = tx.send(Msg::IrCleaned(path, ir_amt, r));
                ctx2.request_repaint();
            });
        }
        let ir_use = if ir_amt > 0.0 { ph.ir_base.as_ref().map_or(0.0, |b| b.0) } else { 0.0 };
        let want = if ph.delta.is_some() { amt } else { 0.0 };
        if (want != ph.nr_applied || ir_use != ph.ir_applied) && !held {
            let base = match &ph.ir_base {
                Some((_, img)) if ir_use > 0.0 => img.clone(),
                _ => ph.orig.clone(),
            };
            ph.full = match &ph.delta {
                Some(d) if want > 0.0 => Arc::new(denoise::apply_delta(&base, d, want / 100.0)),
                _ => base,
            };
            ph.ir_applied = ir_use;
            ph.preview = Arc::new(load::fit(&ph.full, PREVIEW));
            ph.draft = Arc::new(load::fit(&ph.preview, DRAFT));
            ph.nr_applied = want;
            self.rendered = None;
            if let Some(z) = &mut self.zoom {
                z.shown = None;
            }
        }
    }

    /// White balance eyedropper: make the clicked area neutral.
    fn pick_wb(&mut self, u: f32, v: f32) {
        let Some(photo) = &self.photo else { return };
        let mut geo = develop::geometry(&photo.preview, &self.params);
        if self.params.neg.enabled {
            negative::convert(&mut geo, &self.params.neg);
        }
        let (w, h) = geo.dimensions();
        let (cw, ch, ox, oy, _) = develop::border_layout(w, h, &self.params.border);
        let (cx, cy) = ((u * cw as f32) as i64 - ox as i64, (v * ch as f32) as i64 - oy as i64);
        let (mut acc, mut n) = ([0f32; 3], 0.0);
        for y in (cy - 3).max(0)..(cy + 4).min(h as i64) {
            for x in (cx - 3).max(0)..(cx + 4).min(w as i64) {
                let p = geo.get_pixel(x as u32, y as u32).0;
                (0..3).for_each(|c| acc[c] += p[c]);
                n += 1.0;
            }
        }
        if n > 0.0 {
            (self.params.temp, self.params.tint) = develop::solve_wb(acc.map(|a| a / n));
            self.picking_wb = false;
            self.status = format!("White balance {:.0} K, tint {:+.0}", self.params.temp, self.params.tint);
        }
    }

    /// Mark dust and specks on the scan as heal spots (undo removes them all; right-click removes one).
    fn find_dust(&mut self) {
        let Some(photo) = &self.photo else { return };
        let src = load::fit(&photo.full, 3000);
        let found = scan::find_dust(&scan::frame(&src, &self.params), develop::crop_of(&self.params), 150);
        let n = found.len();
        self.params.spots.extend(found);
        self.tool = Tool::Heal;
        self.module = Module::Develop;
        self.open[10] = true;
        self.status = if n == 0 { "No dust found".into() } else { format!("Found {n} specks. Right-click any circle that isn't dust") };
    }

    /// A new virtual copy of `f` with these settings; returns its path.
    fn virtual_copy(&mut self, f: &Path, p: &Params) -> Option<PathBuf> {
        self.virtual_copy_as(f, &Params { strip_frame: false, ..p.clone() })
    }

    /// `virtual_copy` with the settings exactly as given (the Scan tab marks the frames it makes).
    pub(crate) fn virtual_copy_as(&mut self, f: &Path, p: &Params) -> Option<PathBuf> {
        let orig = load::copy_of(f).unwrap_or(f.to_path_buf());
        let name = orig.file_name()?.to_string_lossy().to_string();
        let copy = (2..1000).map(|i| orig.with_file_name(format!("{name}.copy{i}"))).find(|c| !crate::sidecar(c).exists())?;
        write_params(&copy, p).ok()?;
        self.meta.insert(copy.clone(), Meta::of(p));
        let at = self.files.iter().position(|x| *x == orig).map_or(self.files.len(), |i| i + 1);
        self.files.insert(at, copy.clone());
        if let Some(t) = self.thumbs.get(f).cloned() {
            self.thumbs.insert(copy.clone(), t);
            self.thumb_ready.insert(copy.clone());
        }
        Some(copy)
    }

    /// Delete a virtual copy (its settings file); the photo itself is never touched.
    fn remove_copy(&mut self, f: &Path) {
        if load::copy_of(f).is_none() {
            return;
        }
        // If it is open, close it first, or the next edit would write its settings file back.
        if self.current.as_deref() == Some(f) {
            self.current = None;
            self.photo = None;
            self.module = Module::Library;
        }
        let _ = std::fs::remove_file(crate::sidecar(f));
        self.files.retain(|x| x != f);
        self.meta.remove(f);
        if self.lib_sel.as_deref() == Some(f) {
            self.lib_sel = self.files.first().cloned();
        }
    }

    /// Move every rejected photo (with its sidecars) into `_rejects` in the folder. Nothing is deleted.
    fn move_rejects(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.folder.clone() else { return };
        let dest = dir.join("_rejects");
        let rejects: Vec<PathBuf> = self.files.iter().filter(|f| self.meta.get(*f).is_some_and(|m| m.flag < 0)).cloned().collect();
        if rejects.is_empty() {
            self.status = "No rejected photos".into();
            return;
        }
        let r = crate::move_to_rejects(&dest, &rejects);
        // Reload either way: an error part way through still moved the frames before it.
        self.cull.cache.clear();
        self.open_path(ctx, &dir);
        self.status = match r {
            Ok(n) => format!("Moved {n} rejected frame{} to _rejects", if n == 1 { "" } else { "s" }),
            Err(e) => format!("Could not move every reject: {e}"),
        };
    }

    fn export_current(&mut self) {
        let (Some(photo), Some(cur)) = (&self.photo, &self.current) else { return };
        let stem = crate::export_stem(cur);
        let ext = self.export.ext;
        // A JPEG/PNG/TIFF source would be offered its own name: suggest "-edit" so one click can't replace it.
        let stem = if self.export.name.trim().is_empty() { stem } else {
            let shot = crate::export_shot(cur);
            let roll = if self.export.roll.trim().is_empty() { crate::roll::for_photo(cur).map(|r| r.0.roll).unwrap_or_default() } else { self.export.roll.clone() };
            crate::output::file_name(&self.export.name, cur, 1, &roll, &self.params, &shot)
        };
        let name = if load::is_raw(load::copy_of(cur).as_deref().unwrap_or(cur)) || !self.export.name.trim().is_empty() { format!("{stem}.{ext}") } else { format!("{stem}-edit.{ext}") };
        let Some(mut out) = rfd::FileDialog::new().set_file_name(name).add_filter(ext.to_uppercase(), &[ext]).save_file() else { return };
        // Some save dialogs (GTK) don't add the extension; the format is chosen from it, so a TIFF would
        // otherwise come out as a JPEG.
        if out.extension().is_none() {
            out.set_extension(ext);
            if out.exists() {
                // The dialog only asked about replacing the name without the extension.
                self.status = format!("{} already exists: print again under another name", out.file_name().unwrap_or_default().to_string_lossy());
                return;
            }
        }
        if out.canonicalize().ok() == cur.canonicalize().ok() {
            self.status = "Pick a different name, because that one belongs to the original frame".into();
            return;
        }
        let (orig, delta, params, tx, o, cur) = (photo.orig.clone(), photo.delta.clone(), self.params.clone(), self.tx.clone(), self.export.clone(), cur.clone());
        self.status = "Printing…".into();
        std::thread::spawn(move || {
            let base = scan::prepare_ir((*orig).clone(), &cur, &params);
            let src = match (&delta, params.ai_nr > 0.0) {
                (Some(d), true) => Ok(denoise::apply_delta(&base, d, params.ai_nr / 100.0)),
                (None, true) => denoise::prepare(base, &cur, &params),
                _ => Ok(base),
            };
            let r = src.and_then(|src| write_export(&src, &params, &o, &out, &cur));
            let _ = tx.send(Msg::Status(match r { Ok(()) => format!("Printed {}", out.file_name().unwrap_or_default().to_string_lossy()), Err(e) => format!("Print failed: {e}") }));
        });
    }

    fn export_all(&mut self) {
        let Some(dir) = rfd::FileDialog::new().set_title("Print frames to…").pick_folder() else { return };
        self.commit();
        // Each photo with the folder it goes into: subfolders mirrored when asked for.
        let jobs: Vec<(PathBuf, PathBuf)> = match (&self.folder, self.export_sel.is_empty() && self.export.tree) {
            // Rejected frames stay out, as they do from every other export.
            (Some(src), true) => crate::photos_under(src).into_iter().filter(|(_, f)| read_params(f).is_none_or(|p| p.flag >= 0)).map(|(rel, f)| (dir.join(rel), f)).collect(),
            _ => self.print_frames().into_iter().map(|f| (dir.clone(), f)).collect(),
        };
        let files: Vec<PathBuf> = jobs.iter().map(|j| j.1.clone()).collect();
        let (tx, o) = (self.tx.clone(), self.export.clone());
        std::thread::spawn(move || {
            let done = std::sync::atomic::AtomicUsize::new(0);
            // One photo at a time: each render already uses every core, and parallel full-size renders
            // of 45 MP RAWs need several GB each.
            let failed: Vec<String> = files.iter().enumerate().filter_map(|(i, f)| {
                let out = &jobs[i].0;
                let r = std::fs::create_dir_all(out).map_err(|e| e.to_string()).and_then(|_| export_file(f, out, &o, i + 1, |_| {}));
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let _ = tx.send(Msg::Status(format!("Printed {n}/{}", files.len())));
                r.err().map(|e| format!("{}: {e}", f.display()))
            }).collect();
            let _ = tx.send(Msg::Status(if failed.is_empty() { format!("Printed {} frames", files.len()) } else { format!("{} failed, first: {}", failed.len(), failed[0]) }));
        });
    }

    /// One click, several destinations: each chosen set (Instagram 4:5, Story 9:16, Portfolio 2048 px) into its own
    /// subfolder, for this photo, the library selection or the whole folder.
    fn export_set_files(&mut self, files: Vec<PathBuf>) {
        let Some(dir) = rfd::FileDialog::new().set_title("Print sets to…").pick_folder() else { return };
        self.commit();
        let sets: Vec<_> = crate::output::SETS.iter().zip(self.export_sets).filter(|(_, on)| *on).map(|(s, _)| *s).collect();
        let (tx, base) = (self.tx.clone(), self.export.clone());
        std::thread::spawn(move || {
            let total = files.len() * sets.len();
            let mut failed = 0;
            let mut done = 0;
            let targets: Vec<Option<(PathBuf, ExportOpts)>> = sets.iter().map(|(name, canvas, long, q)| {
                let sub = dir.join(name.to_lowercase());
                std::fs::create_dir_all(&sub).ok()?;
                Some((sub, ExportOpts { ext: "jpg", hdr: false, canvas: *canvas, long_edge: *long, quality: *q, print: [0.0, 0.0, 300.0], ..base.clone() }))
            }).collect();
            // Photo by photo, each decoded once for all its sets (it was decoded again for every set).
            for (i, f) in files.iter().enumerate() {
                let src = crate::export_source(f, |_| {});
                for t in &targets {
                    let ok = match (t, &src) {
                        (Some((sub, o)), Ok(src)) => crate::export_prepared(f, src, sub, o, i + 1).is_ok(),
                        _ => false,
                    };
                    failed += !ok as usize;
                    done += 1;
                    let _ = tx.send(Msg::Status(format!("Printed {done}/{total}")));
                }
            }
            let _ = tx.send(Msg::Status(if failed == 0 { format!("Printed {total} files into {} folders", sets.len()) } else { format!("{failed} of {total} prints failed") }));
        });
    }

    /// Apply `f` to every other photo's saved settings in the folder.
    /// On a thread: a cloud or network folder of a thousand frames is thousands of slow file operations.
    fn sync_folder(&mut self, f: impl Fn(&Params) -> Params + Send + 'static) {
        self.commit();
        let cur = self.current.clone();
        let files: Vec<PathBuf> = self.files.iter().filter(|p| Some(*p) != cur.as_ref()).cloned().collect();
        let tx = self.tx.clone();
        self.status = format!("Syncing {} frames…", files.len());
        std::thread::spawn(move || {
            let done: Vec<(PathBuf, Meta)> = files.into_iter().filter_map(|p| {
                let q = f(&read_params(&p).unwrap_or_default());
                write_params(&p, &q).ok().map(|()| (p, Meta::of(&q)))
            }).collect();
            let _ = tx.send(Msg::Synced(done));
        });
    }

    /// Jump in history. Saved straight away, or an undone edit would come back next time the photo opens.
    fn undo(&mut self, d: isize) {
        self.goto_history((self.hist_pos as isize + d).clamp(0, self.history.len() as isize - 1) as usize);
    }

    fn goto_history(&mut self, i: usize) {
        if i != self.hist_pos {
            self.hist_pos = i;
            self.params = self.history[i].clone();
            self.save();
        }
    }

    /// Batch rename: pattern with tokens, a live preview, then rename on a background thread.
    fn rename_window(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.rename_dlg else { return };
        if d.previewed != (d.pattern.clone(), d.roll.clone()) {
            d.previewed = (d.pattern.clone(), d.roll.clone());
            let first: Vec<PathBuf> = d.files.iter().take(4).cloned().collect();
            let name = |p: &Path| p.file_name().unwrap_or_default().to_string_lossy().to_string();
            d.preview = crate::rename::plan(&first, &d.pattern, &d.roll).into_iter().filter(|(a, _)| first.contains(a)).map(|(a, b)| (name(&a), name(&b))).collect();
        }
        let mut go = false;
        let title = format!("Rename {} photo{}", d.files.len(), if d.files.len() == 1 { "" } else { "s" });
        let open = dialog(ctx, "rename", &title, 380.0, |ui| {
            ui.add(egui::TextEdit::singleline(&mut d.pattern).desired_width(f32::INFINITY));
            token_chips(ui, &mut d.pattern);
            ui.add(egui::TextEdit::singleline(&mut d.roll).hint_text("Roll number (for {roll})").desired_width(f32::INFINITY));
            ui.add_space(6.0);
            for (a, b) in &d.preview {
                ui.label(RichText::new(format!("{a}  →  {b}")).monospace().size(11.0).color(TEXT2));
            }
            if d.files.len() > 4 {
                ui.label(RichText::new("…").color(TEXT3));
            }
            ui.label(RichText::new("Edits, Lightroom .xmp files and virtual copies are renamed too, and RAW+JPEG pairs keep matching names. Undo from the right-click menu.").size(11.5).color(TEXT2));
            go = primary(ui, "Rename", !d.pattern.trim().is_empty()).clicked();
        });
        if go {
            let (files, pattern, roll) = (d.files.clone(), d.pattern.clone(), d.roll.clone());
            self.commit();
            let (tx, ctx2) = (self.tx.clone(), ctx.clone());
            self.status = "Renaming…".into();
            std::thread::spawn(move || {
                let plan = crate::rename::plan(&files, &pattern, &roll);
                let _ = tx.send(Msg::Renamed(crate::rename::apply(&plan).map(|_| plan)));
                ctx2.request_repaint();
            });
        }
        if go || !open {
            self.rename_dlg = None;
        }
    }

    /// Export settings: format, size and quality, then the file or folder dialog.
    fn export_window(&mut self, ctx: &egui::Context) {
        let Some(all) = self.export_dialog else { return };
        let mut go = false;
        let n = if all { self.print_frames().len() } else { 1 };
        let title = if !all { "Print".to_string() } else { format!("Print {n} frame{}", if n == 1 { "" } else { "s" }) };
        let open = dialog(ctx, "print", &title, 320.0, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let o = &mut self.export;
                ui.label(caps("Format"));
                let exts = ["jpg", "tif", "png"];
                let mut i = exts.iter().position(|e| *e == o.ext).unwrap_or(0);
                segmented(ui, &mut i, &["JPEG", "TIFF 16-bit", "PNG"]);
                o.ext = exts[i];
                ui.label(caps("Size"));
                let sizes = [0, 4096, 2048, 1080];
                let printing = o.print[0] > 0.0;
                let mut j = if printing { 4 } else { sizes.iter().position(|v| *v == o.long_edge).unwrap_or(0) };
                segmented(ui, &mut j, &["Full", "4096", "2048", "1080", "Paper"]);
                if j == 4 {
                    if !printing {
                        let (_, a, b) = crate::output::PAPERS[2];
                        o.print = [a, b, o.print[2].max(72.0)];
                    }
                    o.long_edge = 0;
                    let cur = crate::output::PAPERS.iter().position(|p| (p.1 - o.print[0]).abs() < 0.01 && (p.2 - o.print[1]).abs() < 0.01).unwrap_or(2);
                    let mut pi = cur;
                    egui::ComboBox::from_id_salt("paper").width(140.0).selected_text(crate::output::PAPERS[pi].0).show_ui(ui, |ui| {
                        for (k, p) in crate::output::PAPERS.iter().enumerate() {
                            pick(ui, &mut pi, k, p.0);
                        }
                    });
                    if pi != cur {
                        o.print = [crate::output::PAPERS[pi].1, crate::output::PAPERS[pi].2, o.print[2]];
                    }
                    let dpis = [240.0, 300.0, 360.0];
                    let mut di = dpis.iter().position(|d| *d == o.print[2]).unwrap_or(1);
                    segmented(ui, &mut di, &["240 dpi", "300 dpi", "360 dpi"]);
                    o.print[2] = dpis[di];
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Fill the paper (crop)").size(12.0).color(LABEL));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, &mut o.print_fill));
                    });
                    ui.label(RichText::new(format!("{:.0} × {:.0} px at {:.0} dpi, turned to match the photo. {}", o.print[0] * o.print[2], o.print[1] * o.print[2], o.print[2],
                        if o.print_fill { "The photo is cropped to the paper's shape." } else { "White margins fill the difference in shape." })).size(11.5).color(TEXT2));
                } else {
                    o.print[0] = 0.0;
                    o.long_edge = sizes[j];
                }
                if o.ext == "jpg" {
                    let mut q = o.quality as f32;
                    slider(ui, "Quality", &mut q, 50.0, 100.0, 92.0, None);
                    o.quality = q as u8;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("HDR (gain map)").size(12.0).color(LABEL));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, &mut o.hdr));
                    });
                    if o.hdr {
                        ui.label(RichText::new("Highlights glow up to 2 stops brighter on HDR screens, such as recent phones, Macs and Chrome. Everywhere else it shows as a normal JPEG.").size(11.5).color(TEXT2));
                    }
                }
                ui.label(caps("Colour space"));
                let mut si = crate::colour::Space::ALL.iter().position(|x| x.0 == o.space).unwrap_or(0);
                segmented(ui, &mut si, &crate::colour::Space::ALL.map(|x| x.1));
                o.space = crate::colour::Space::ALL[si].0;
                let mut note = o.space.hint().to_string();
                if o.space != crate::colour::Space::Srgb {
                    if o.ext == "jpg" && o.hdr {
                        note = "HDR JPEGs are always sRGB.".into();
                    } else if o.space == crate::colour::Space::ProPhoto && o.ext != "tif" {
                        note = "ProPhoto is too wide for 8 bits, so smooth skies may show bands. Use 16-bit TIFF instead.".into();
                    } else {
                        note += ". The profile is embedded in the file.";
                    }
                }
                ui.label(RichText::new(note).size(11.5).color(TEXT2));
                disclose(ui, "Watermark", "wm", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut o.wm_text).hint_text("© Your name").desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        let logo = if o.wm_logo.is_empty() { "Logo image…".to_string() } else { Path::new(&o.wm_logo).file_name().unwrap_or_default().to_string_lossy().to_string() };
                        if ui.button(logo).on_hover_text("A PNG with transparency works best. It replaces the text").clicked()
                            && let Some(f) = rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "webp"]).pick_file()
                        {
                            o.wm_logo = f.display().to_string();
                        }
                        if !o.wm_logo.is_empty() && ui.small_button("Remove").clicked() {
                            o.wm_logo.clear();
                        }
                    });
                    let mut pos = o.wm_pos as usize;
                    segmented(ui, &mut pos, &["↘", "↙", "↗", "↖", "Centre"]);
                    o.wm_pos = pos as u8;
                    slider(ui, "Size", &mut o.wm_size, 1.0, 20.0, 4.0, None);
                    slider(ui, "Opacity", &mut o.wm_opacity, 10.0, 100.0, 70.0, None);
                });
                disclose(ui, "File names & details", "names", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut o.name).hint_text("{name}  ·  try Roll {roll}-{n} {stock}").desired_width(f32::INFINITY));
                    token_chips(ui, &mut o.name);
                    ui.add(egui::TextEdit::singleline(&mut o.roll).hint_text("Roll number").desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Camera, lens, roll & stock in JPEG").size(12.0).color(LABEL));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, &mut o.meta));
                    });
                });
                if all && self.export_sel.is_empty() {
                    switch_row(ui, "Include subfolders, keep the folder tree", &mut o.tree);
                }
                disclose(ui, "Print sets", "sets", |ui| {
                    for (k, (name, canvas, long, _)) in crate::output::SETS.iter().enumerate() {
                        let what = if canvas[0] > 0 { format!("{name}  {}×{}", canvas[0], canvas[1]) } else { format!("{name}  {long} px") };
                        ui.checkbox(&mut self.export_sets[k], what);
                    }
                    ui.label(RichText::new("One click makes each set in its own subfolder, padded to shape with the border colour.").size(11.5).color(TEXT2));
                });
                ui.label(RichText::new("Prints hold only the picture unless you add the details above. GPS and serial numbers are never included.").size(11.5).color(TEXT2));
                ui.add_space(4.0);
                let sets = self.export_sets.iter().any(|b| *b);
                go = primary(ui, if sets { "Print sets to folder…" } else if all { "Print to folder…" } else { "Print to file…" }, true).clicked();
            });
        if go || !open {
            self.export_dialog = None;
        }
        if go {
            self.export.save();
            if self.export_sets.iter().any(|b| *b) {
                let files = if !all { self.current.iter().cloned().collect() } else { self.print_frames() };
                self.export_set_files(files);
            } else if all { self.export_all() } else { self.export_current() }
        }
    }

    // ---------- tools on the photo ----------

    fn to_frame(&self, uv: egui::Vec2) -> [f32; 2] {
        let [cw, ch, ox, oy, fw, fh, fox, foy] = self.view_map;
        [(uv.x * cw - ox + fox) / fw, (uv.y * ch - oy + foy) / fh]
    }

    /// The rendered colour at a sample point, averaged over 3×3 pixels.
    fn sample_rgb(&self, f: [f32; 2]) -> Option<[u8; 3]> {
        let ci = self.last_rgb.as_ref()?;
        let [cw, ch, ox, oy, ..] = self.view_map;
        let [pw, ph] = self.photo_wh;
        let (sx, sy) = (ci.size[0] as f32 / cw, ci.size[1] as f32 / ch);
        let (x, y) = (((ox + f[0] * pw) * sx) as i64, ((oy + f[1] * ph) * sy) as i64);
        let mut sum = [0u32; 3];
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (xx, yy) = ((x + dx).clamp(0, ci.size[0] as i64 - 1) as usize, (y + dy).clamp(0, ci.size[1] as i64 - 1) as usize);
                let c = ci.pixels[yy * ci.size[0] + xx];
                sum = [sum[0] + c.r() as u32, sum[1] + c.g() as u32, sum[2] + c.b() as u32];
            }
        }
        Some(sum.map(|v| (v / 9) as u8))
    }

    /// Sample tool: click to pin a colour readout (up to 8), right-click one to remove it. Pins are always drawn.
    fn sample_marks(&mut self, ui: &mut Ui, resp: &egui::Response, r: Rect, active: bool) {
        let [cw, ch, ox, oy, ..] = self.view_map;
        let [pw, ph] = self.photo_wh;
        let to_s = |f: [f32; 2]| r.min + vec2((ox + f[0] * pw) / cw * r.width(), (oy + f[1] * ph) / ch * r.height());
        if active {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            if let Some(p) = resp.interact_pointer_pos() {
                let near = self.samples.iter().position(|f| to_s(*f).distance(p) < 10.0);
                if resp.secondary_clicked() {
                    if let Some(i) = near {
                        self.samples.remove(i);
                    }
                } else if resp.clicked() && near.is_none() && self.samples.len() < 8 {
                    let uv = (p - r.min) / r.size();
                    let f = [(uv.x * cw - ox) / pw, (uv.y * ch - oy) / ph];
                    if (0.0..=1.0).contains(&f[0]) && (0.0..=1.0).contains(&f[1]) {
                        self.samples.push(f);
                    }
                }
            }
        }
        let pt = ui.painter_at(r);
        for (k, f) in self.samples.iter().enumerate() {
            let c = to_s(*f);
            pt.circle_stroke(c, 7.0, Stroke::new(3.0, Color32::from_black_alpha(140)));
            pt.circle_stroke(c, 7.0, Stroke::new(1.5, Color32::WHITE));
            pt.text(c + vec2(10.0, -10.0), Align2::LEFT_CENTER, format!("{}", k + 1), FontId::new(11.0, semi()), Color32::WHITE);
        }
    }

    /// Chart tool on the photo: drag the four numbered corners onto the centres of the chart's corner patches
    /// (or drag inside to move it). Each patch shows its reference colour, so a misplaced grid stands out.
    fn chart_tool(&mut self, ui: &Ui, resp: &egui::Response, r: Rect) {
        let corners = self.chart.q.map(|f| self.to_screen(r, f));
        let inside = |p: Pos2| {
            // Inside the quad: same side of all four edges.
            let s: [f32; 4] = std::array::from_fn(|i| { let (a, b) = (corners[i], corners[(i + 1) % 4]); (b - a).x * (p - a).y - (b - a).y * (p - a).x });
            s.iter().all(|v| *v >= 0.0) || s.iter().all(|v| *v <= 0.0)
        };
        if resp.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            self.chart.grab = corners.iter().position(|c| c.distance(p) < 16.0).or(inside(p).then_some(4));
        }
        if resp.dragged()
            && let (Some(g), Some(p)) = (self.chart.grab, resp.interact_pointer_pos())
        {
            let f = self.to_frame((p - r.min) / r.size());
            if g < 4 {
                self.chart.q[g] = f.map(|v| v.clamp(0.0, 1.0));
            } else {
                let was = self.to_frame((p - resp.drag_delta() - r.min) / r.size());
                let d = [f[0] - was[0], f[1] - was[1]];
                let lo = [0, 1].map(|k| self.chart.q.iter().map(|q| q[k]).fold(1.0, f32::min));
                let hi = [0, 1].map(|k| self.chart.q.iter().map(|q| q[k]).fold(0.0, f32::max));
                let d = [d[0].clamp(-lo[0], 1.0 - hi[0]), d[1].clamp(-lo[1], 1.0 - hi[1])];
                for q in &mut self.chart.q {
                    (q[0], q[1]) = (q[0] + d[0], q[1] + d[1]);
                }
            }
            self.chart.fit = None;
        }
        if resp.drag_stopped() {
            self.chart.grab = None;
        }
        let pt = ui.painter_at(r);
        let t = &self.chart.target;
        let sq = corners.map(|c| [c.x, c.y]);
        // A third of the patch: big enough to compare, small enough to see the patch around it.
        let half = (crate::calib::sample_half(t, &sq, 1.0) * 0.55).max(2.0);
        for i in 0..t.rows * t.cols {
            let Some(lab) = t.lab[i] else { continue };
            let c = crate::calib::patch_center(t, &sq, i / t.cols, i % t.cols);
            let rect = Rect::from_center_size(pos2(c[0], c[1]), vec2(half, half) * 2.0);
            pt.rect_filled(rect, 1.0, lin(crate::calib::lab_to_rgb(lab).map(|v| v.clamp(0.0, 1.0))));
            pt.rect_stroke(rect, 1.0, Stroke::new(1.0, Color32::from_black_alpha(160)), egui::StrokeKind::Outside);
        }
        for i in 0..4 {
            pt.line_segment([corners[i], corners[(i + 1) % 4]], Stroke::new(1.0, Color32::from_white_alpha(140)));
        }
        // Which patch marker 1 belongs on, on the photo itself.
        let first = if t.cols == 6 && t.rows == 4 { "dark skin" } else { "A1" };
        let tag = pt.layout_no_wrap(first.into(), FontId::new(11.0, semi()), Color32::WHITE);
        let at = corners[0] + vec2(-14.0 - tag.size().x, -8.0 - tag.size().y);
        pt.rect_filled(Rect::from_min_size(at, tag.size()).expand(3.0), 3.0, Color32::from_black_alpha(170));
        pt.galley(at, tag, Color32::WHITE);
        for (i, c) in corners.iter().enumerate() {
            pt.circle_filled(*c, 9.0, Color32::from_black_alpha(170));
            pt.circle_stroke(*c, 9.0, Stroke::new(1.5, Color32::WHITE));
            pt.text(*c, Align2::CENTER_CENTER, format!("{}", i + 1), FontId::new(11.0, semi()), Color32::WHITE);
        }
        if let Some(h) = resp.hover_pos() {
            let icon = if corners.iter().any(|c| c.distance(h) < 16.0) { egui::CursorIcon::Crosshair } else if inside(h) { egui::CursorIcon::Grab } else { egui::CursorIcon::Default };
            ui.ctx().set_cursor_icon(icon);
        }
    }

    /// The settings panel while the chart tool is on: which chart, a name, and Build.
    fn chart_panel(&mut self, ui: &mut Ui) {
        let mut done = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Chart calibration").size(14.0).family(semi()).color(TEXT));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| done = ui.button("Done").clicked());
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Shoot or scan the chart under the same light as your photos, without clipping the white patch. Then drag the numbered markers onto the centres of the corner patches.").size(12.0).color(TEXT2));
                ui.add_space(4.0);
                let first = if self.chart.target.cols == 6 { "1 on dark skin (brown), 2 on bluish green, 3 on black and 4 on white" } else { "1 on A1, 2 on the last patch of row A, 3 on the last patch of the last row and 4 on the first patch of the last row" };
                ui.label(RichText::new(first).size(12.0).color(TEXT2));
                ui.add_space(10.0);
                ui.label(caps("Chart"));
                ui.horizontal(|ui| {
                    let cc = self.chart.target.cols == 6 && self.chart.target.rows == 4 && self.chart.target.name == "ColorChecker 24";
                    if small_action(ui, "ColorChecker 24", cc).clicked() {
                        self.chart.target = crate::calib::colorchecker();
                        self.chart.fit = None;
                    }
                    if small_action(ui, if cc { "IT8 reference…" } else { &self.chart.target.name }, !cc).on_hover_text("For an IT8.7 chart, open the reference file (CGATS .txt) that came with it").clicked()
                        && let Some(f) = rfd::FileDialog::new().add_filter("IT8 reference", &["txt", "it8", "cie", "cgats"]).pick_file()
                    {
                        let name = f.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|t| crate::calib::parse_cgats(&t, &name)) {
                            Ok(t) => {
                                self.chart.target = t;
                                self.chart.fit = None;
                            }
                            Err(e) => self.chart.fit = Some(Err(format!("Could not read the reference file: {e}"))),
                        }
                    }
                });
                ui.add_space(10.0);
                ui.label(caps("Profile name"));
                ui.add(egui::TextEdit::singleline(&mut self.chart.name).hint_text("for example X-T5 daylight, or Scanner + CRI light").desired_width(ui.available_width()));
                ui.add_space(8.0);
                let can = self.photo.is_some() && !self.chart.name.trim().is_empty() && !self.chart.busy;
                if primary(ui, if self.chart.busy { "Building…" } else { "Build profile" }, can).on_disabled_hover_text("Name the profile first").clicked() {
                    self.build_chart_profile(ui.ctx());
                }
                ui.add_space(8.0);
                match &self.chart.fit {
                    Some(Ok(f)) => {
                        ui.label(RichText::new(format!("Average colour error ΔE {:.1} → {:.1} (worst patch {:.1}), from {} patches.", f.de_before, f.de_after, f.de_worst, f.used)).size(12.0).color(TEXT));
                        ui.label(RichText::new("Saved and applied to this photo. For other photos, pick it under Calibration › Profile or paste these settings onto them. Set white balance on the chart's grey patch as usual.").size(11.5).color(TEXT2));
                        if f.de_after > 6.0 {
                            ui.label(RichText::new("The error is still high. Check that each marker sits on the right patch.").size(11.5).color(WARN));
                        }
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).size(12.0).color(WARN));
                    }
                    None => {}
                }
            });
        });
        if done {
            self.tool = Tool::None;
        }
    }

    /// Measure the chart on the uncorrected frame, fit, save and apply the profile.
    /// Measures on a worker thread (a full geometry pass of the preview), so the window never stalls.
    fn build_chart_profile(&mut self, ctx: &egui::Context) {
        let Some(ph) = &self.photo else { return };
        let (src, p) = (ph.preview.clone(), Params { crop: [0.0; 4], profile: None, ..self.params.clone() });
        let (target, q, name) = (self.chart.target.clone(), self.chart.q, self.chart.name.trim().to_string());
        let (tx, ctx, generation) = (self.tx.clone(), ctx.clone(), self.photo_gen);
        self.chart.busy = true;
        std::thread::spawn(move || {
            let frame = develop::geometry(&src, &p);
            let fit = crate::calib::fit(&target, &crate::calib::sample(&frame, &target, &q));
            let _ = tx.send(Msg::ChartFit(generation, name, fit));
            ctx.request_repaint();
        });
    }

    /// Save and apply a fitted profile (or show why it failed).
    fn finish_chart_profile(&mut self, name: String, fit: Result<crate::calib::Fit, String>) {
        if let Ok(f) = &fit {
            let prof = crate::calib::ColorProfile { name: name.clone(), m: f.m };
            match prof.install() {
                Ok(_) => {
                    self.set_profile(Some(prof));
                    self.profiles = crate::calib::saved();
                    self.status = format!("Saved profile {name}");
                }
                Err(e) => {
                    self.chart.fit = Some(Err(format!("Could not save the profile: {e}")));
                    return;
                }
            }
        }
        self.chart.fit = Some(fit);
    }

    /// Switch the colour profile. A film base and locked levels were measured through the old one, so they move
    /// with it: the base exactly (it is an average of pixels), locked levels by measuring again.
    fn set_profile(&mut self, new: Option<crate::calib::ColorProfile>) {
        let old = self.params.profile.take();
        let np = &mut self.params.neg;
        if let Some(b) = np.base {
            np.base = Some(crate::calib::reprofile(b, old.as_ref(), new.as_ref()));
        }
        if np.levels.is_some()
            && let Some(ph) = &self.photo
        {
            let geo = develop::geometry(&ph.preview, &Params { neg: np.clone(), profile: new.clone(), ..Default::default() });
            np.levels = Some(negative::analyse(&geo, np).1);
        }
        self.params.profile = new;
    }

    /// The film base found when the photo was opened (measured without a profile), as this photo's profile sees it.
    fn auto_base(&self) -> Option<[f32; 3]> {
        self.photo.as_ref().map(|p| crate::calib::reprofile(p.auto_base, None, self.params.profile.as_ref()))
    }

    fn to_screen(&self, r: Rect, f: [f32; 2]) -> egui::Pos2 {
        let [cw, ch, ox, oy, fw, fh, fox, foy] = self.view_map;
        r.min + vec2((f[0] * fw - fox + ox) / cw * r.width(), (f[1] * fh - foy + oy) / ch * r.height())
    }

    /// Set a centred crop of `aspect` (w/h, in pixels), as large as the frame allows.
    fn set_crop_aspect(&mut self, aspect: f32) {
        let Some(ph) = &self.photo else { return };
        let (pw, phh) = develop::oriented(ph.preview.width(), ph.preview.height(), &self.params);
        let fa = pw as f32 / phh as f32;
        let (w, h) = if fa > aspect { (aspect / fa, 1.0) } else { (1.0, fa / aspect) };
        self.params.crop = [(1.0 - w) / 2.0, (1.0 - h) / 2.0, (1.0 - w) / 2.0, (1.0 - h) / 2.0];
    }

    fn crop_tool(&mut self, ui: &Ui, resp: &egui::Response, r: Rect) {
        let c = self.params.crop;
        let (mut x0, mut y0, mut x1, mut y1) = (c[0], c[1], 1.0 - c[2], 1.0 - c[3]);
        let sel = Rect::from_min_max(r.lerp_inside(vec2(x0, y0)), r.lerp_inside(vec2(x1, y1)));
        let corners = [sel.left_top(), sel.right_top(), sel.right_bottom(), sel.left_bottom()];
        // Where the press began: by the time a drag registers, the pointer has already moved.
        if resp.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            self.crop_grab = corners.iter().position(|c| c.distance(p) < 16.0).or(sel.contains(p).then_some(4));
        }
        if resp.dragged()
            && let (Some(g), Some(p)) = (self.crop_grab, resp.interact_pointer_pos())
        {
            let uv = ((p - r.min) / r.size()).clamp(vec2(0.0, 0.0), vec2(1.0, 1.0));
            const MIN: f32 = 0.05;
            if g == 4 {
                let d = resp.drag_delta() / r.size();
                let dx = d.x.clamp(-x0, 1.0 - x1);
                let dy = d.y.clamp(-y0, 1.0 - y1);
                (x0, x1, y0, y1) = (x0 + dx, x1 + dx, y0 + dy, y1 + dy);
            } else {
                if g == 0 || g == 3 { x0 = uv.x.min(x1 - MIN) } else { x1 = uv.x.max(x0 + MIN) }
                if g == 0 || g == 1 { y0 = uv.y.min(y1 - MIN) } else { y1 = uv.y.max(y0 + MIN) }
                if let Some(a) = ASPECTS[self.crop_aspect].1 {
                    // Keep the ratio: height follows width, anchored at the opposite corner; shrink width if it won't fit.
                    let fa = r.width() / r.height();
                    let room = if g <= 1 { y1 } else { 1.0 - y0 };
                    let mut w = x1 - x0;
                    let hgt = (w * fa / a).min(room);
                    w = hgt * a / fa;
                    if g == 0 || g == 3 { x0 = x1 - w } else { x1 = x0 + w }
                    if g <= 1 { y0 = y1 - hgt } else { y1 = y0 + hgt }
                }
            }
            self.params.crop = [x0, y0, 1.0 - x1, 1.0 - y1];
        }
        if resp.drag_stopped() {
            self.crop_grab = None;
        }
        // Dim outside the crop, frame, thirds and corner handles.
        let pt = ui.painter_at(r);
        let dim = Color32::from_black_alpha(150);
        for d in [Rect::from_min_max(r.min, pos2(r.max.x, sel.min.y)), Rect::from_min_max(pos2(r.min.x, sel.max.y), r.max),
                  Rect::from_min_max(pos2(r.min.x, sel.min.y), pos2(sel.min.x, sel.max.y)), Rect::from_min_max(pos2(sel.max.x, sel.min.y), pos2(r.max.x, sel.max.y))] {
            pt.rect_filled(d, 0.0, dim);
        }
        for i in 1..3 {
            let f = i as f32 / 3.0;
            pt.vline(sel.left() + sel.width() * f, sel.y_range(), Stroke::new(1.0, Color32::from_white_alpha(70)));
            pt.hline(sel.x_range(), sel.top() + sel.height() * f, Stroke::new(1.0, Color32::from_white_alpha(70)));
        }
        pt.rect_stroke(sel, 0.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Inside);
        for c in corners {
            ui.painter().rect_filled(Rect::from_center_size(c, vec2(10.0, 10.0)), 2.0, Color32::WHITE);
        }
        if let Some(h) = resp.hover_pos() {
            let icon = if corners.iter().any(|c| c.distance(h) < 16.0) { egui::CursorIcon::Crosshair } else if sel.contains(h) { egui::CursorIcon::Grab } else { egui::CursorIcon::Default };
            ui.ctx().set_cursor_icon(icon);
        }
    }

    /// Automatic source for a spot or stroke: the nearby area whose surroundings match best.
    fn auto_source(&self, sp: &develop::Spot) -> Option<[f32; 2]> {
        let ph = self.photo.as_ref()?;
        let geo = develop::geometry(&ph.preview, &self.params);
        let (gw, gh, gx, gy) = develop::frame_of(&self.params, geo.width() as usize, geo.height() as usize);
        let pts: Vec<[f32; 2]> = sp.points().map(|q| [q[0] * gw - gx, q[1] * gh - gy]).collect();
        let n = pts.len() as f32;
        let c = [pts.iter().map(|q| q[0]).sum::<f32>() / n, pts.iter().map(|q| q[1]).sum::<f32>() / n];
        let reach = pts.iter().map(|q| ((q[0] - c[0]).powi(2) + (q[1] - c[1]).powi(2)).sqrt()).fold(0.0, f32::max);
        let (sx, sy) = develop::find_source(&geo, c[0], c[1], sp.r * gw.max(gh) + reach);
        // The source belongs to the first point; shift by the first point's offset from the centre.
        Some([(sx - c[0] + pts[0][0] + gx) / gw, (sy - c[1] + pts[0][1] + gy) / gh])
    }

    /// Where the source of point `q` of spot `s` is, in frame fractions (follows the source's angle).
    fn source_of(&self, s: &develop::Spot, q: [f32; 2]) -> [f32; 2] {
        let [.., fw, fh, _, _] = self.view_map;
        let (sin, cos) = s.angle.to_radians().sin_cos();
        let (dx, dy) = ((q[0] - s.x) * fw, (q[1] - s.y) * fh);
        [s.sx + (dx * cos - dy * sin) / fw, s.sy + (dx * sin + dy * cos) / fh]
    }

    /// Heal and clone: click a spot, or paint a stroke; drag a source circle to choose where the texture comes
    /// from; right-click a spot to remove it.
    fn heal_tool(&mut self, ui: &Ui, resp: &egui::Response, r: Rect) {
        let [cw, _, _, _, fw, fh, ..] = self.view_map;
        let to_px = r.width() / cw * fw.max(fh); // frame fraction of the long side → screen points
        let rad = self.spot_size / 100.0;
        let frame_at = |s: &Self, pos: Pos2| s.to_frame((pos - r.min) / r.size());
        // What's under a screen position: a source (index) or a spot's own area.
        let source_hit = |s: &Self, pos: Pos2| s.params.spots.iter().rposition(|sp| sp.points().any(|q| s.to_screen(r, s.source_of(sp, q)).distance(pos) < sp.r * to_px));
        let spot_hit = |s: &Self, pos: Pos2| s.params.spots.iter().rposition(|sp| sp.points().any(|q| s.to_screen(r, q).distance(pos) < sp.r * to_px));
        if resp.drag_started()
            && let Some(pos) = ui.input(|i| i.pointer.press_origin())
        {
            let f = frame_at(self, pos);
            self.heal_drag = match source_hit(self, pos) {
                Some(i) => {
                    let sp = &self.params.spots[i];
                    self.spot_sel = Some(i);
                    Some(HealDrag::Source(i, [f[0] - sp.sx, f[1] - sp.sy]))
                }
                None => {
                    self.params.spots.push(develop::Spot { x: f[0], y: f[1], r: rad, sx: f[0], sy: f[1], clone: self.heal_clone, ..Default::default() });
                    Some(HealDrag::Paint(self.params.spots.len() - 1))
                }
            };
        }
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let f = frame_at(self, pos);
            match self.heal_drag {
                Some(HealDrag::Paint(i)) if i < self.params.spots.len() => {
                    let last = self.params.spots[i].points().last().unwrap_or(f);
                    if self.to_screen(r, last).distance(pos) > rad * to_px * 0.3 {
                        self.params.spots[i].path.push(f);
                    }
                }
                Some(HealDrag::Source(i, off)) if i < self.params.spots.len() => {
                    (self.params.spots[i].sx, self.params.spots[i].sy) = (f[0] - off[0], f[1] - off[1]);
                }
                _ => {}
            }
        }
        if resp.drag_stopped() {
            if let Some(HealDrag::Paint(i)) = self.heal_drag
                && i < self.params.spots.len()
                && let Some(src) = self.auto_source(&self.params.spots[i])
            {
                (self.params.spots[i].sx, self.params.spots[i].sy) = (src[0], src[1]);
                self.spot_sel = Some(i);
            }
            self.heal_drag = None;
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            if let Some(i) = spot_hit(self, pos).or_else(|| source_hit(self, pos)) {
                self.spot_sel = Some(i); // select it to change its angle or mode
            } else {
                let f = frame_at(self, pos);
                let mut sp = develop::Spot { x: f[0], y: f[1], r: rad, sx: f[0], sy: f[1], clone: self.heal_clone, ..Default::default() };
                if let Some(src) = self.auto_source(&sp) {
                    (sp.sx, sp.sy) = (src[0], src[1]);
                    self.params.spots.push(sp);
                    self.spot_sel = Some(self.params.spots.len() - 1);
                }
            }
        }
        if resp.secondary_clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && let Some(i) = spot_hit(self, pos)
        {
            self.params.spots.remove(i);
            self.spot_sel = None;
        }
        let pt = ui.painter_at(r);
        let painting = matches!(self.heal_drag, Some(HealDrag::Paint(_)));
        for (i, s) in self.params.spots.iter().enumerate() {
            let sel = self.spot_sel == Some(i);
            let w = s.r * to_px;
            let target: Vec<Pos2> = s.points().map(|q| self.to_screen(r, q)).collect();
            let col = if sel { ACCENT } else { Color32::WHITE };
            if target.len() > 1 {
                pt.add(egui::Shape::line(target.clone(), Stroke::new(w * 2.0, Color32::from_white_alpha(28))));
            }
            let dragging_this = matches!(self.heal_drag, Some(HealDrag::Paint(k)) if k == i);
            if !dragging_this {
                let source: Vec<Pos2> = s.points().map(|q| self.to_screen(r, self.source_of(s, q))).collect();
                pt.line_segment([target[0], source[0]], Stroke::new(1.0, Color32::from_white_alpha(90)));
                if source.len() > 1 {
                    pt.add(egui::Shape::line(source.clone(), Stroke::new(w * 2.0, Color32::from_white_alpha(14))));
                }
                pt.circle_stroke(source[0], w, Stroke::new(1.0, Color32::from_white_alpha(if sel { 200 } else { 110 })));
            }
            pt.circle_stroke(target[0], w, Stroke::new(1.5, col));
            if target.len() > 1 {
                pt.circle_stroke(*target.last().unwrap(), w, Stroke::new(1.0, col));
            }
        }
        if let Some(h) = resp.hover_pos() {
            let over_source = !painting && source_hit(self, h).is_some();
            if over_source {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
            } else {
                pt.circle_stroke(h, rad * to_px, Stroke::new(1.0, ACCENT));
            }
        }
    }

    fn toggle_zoom(&mut self, center: [f32; 2]) {
        self.zoom = if self.zoom.is_some() || self.photo.is_none() { None } else { Some(Zoom::at(center, 1.0, None)) };
        if self.zoom.is_some() {
            self.tool = Tool::None;
            self.view = View::After;
        }
    }

    /// Zoomed view: the preview scaled up straight away, then a sharp render of just the visible part of the
    /// full-resolution photo once the view settles. Drag, scroll with two fingers, wheel or pinch.
    fn zoom_view(&mut self, ui: &mut Ui, avail: Rect) {
        let (Some(ph), Some(z)) = (&self.photo, &mut self.zoom) else { return };
        let ppp = ui.ctx().pixels_per_point();
        let now = ui.input(|i| i.time);
        let p = Params { border: Default::default(), ..self.params.clone() };
        let (ow, oh) = develop::output_size(ph.full.width(), ph.full.height(), &p);
        let (owf, ohf) = (ow as f32, oh as f32);
        let fit = (avail.width() * ppp / owf).min(avail.height() * ppp / ohf);
        if let Some(f) = z.from_fit.take() {
            z.scale = fit * f;
        }
        let resp = ui.interact(avail, ui.scope_id().with("zoom"), Sense::click_and_drag());
        // Photo pixel under a screen position, and back.
        let mut c = [z.center[0] * owf, z.center[1] * ohf];
        if let Some(a) = z.anchor.take() {
            let d = (a - avail.center()) * ppp / z.scale;
            c = [c[0] - d.x, c[1] - d.y];
        }
        let (factor, pan, at) = view_gestures(ui, avail);
        if factor != 1.0
            && let Some(at) = at
        {
            let d = (at - avail.center()) * ppp;
            let under = [c[0] + d.x / z.scale, c[1] + d.y / z.scale];
            let ns = (z.scale * factor).clamp(fit, 16.0);
            if ns <= fit * 1.001 && factor < 1.0 {
                self.zoom = None;
                return;
            }
            z.scale = ns;
            c = [under[0] - d.x / ns, under[1] - d.y / ns];
        }
        let mut pan = pan;
        if resp.dragged() {
            pan += resp.drag_delta();
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        c = [c[0] - pan.x * ppp / z.scale, c[1] - pan.y * ppp / z.scale];
        // Keep the photo on screen: centred when it's smaller than the view.
        let (vw, vh) = (avail.width() * ppp / z.scale, avail.height() * ppp / z.scale);
        let keep = |c: f32, v: f32, o: f32| if v >= o { o / 2.0 } else { c.clamp(v / 2.0, o - v / 2.0) };
        c = [keep(c[0], vw, owf), keep(c[1], vh, ohf)];
        if factor != 1.0 || pan != Vec2::ZERO {
            z.moved = now;
        }
        z.center = [c[0] / owf, c[1] / ohf];
        if resp.double_clicked() {
            self.zoom = None;
            return;
        }
        let to_screen = |x: f32, y: f32| avail.center() + vec2(x - c[0], y - c[1]) * z.scale / ppp;
        let painter = ui.painter_at(avail);
        // 1. The preview, scaled: instant, if soft.
        if let Some(t) = &self.tex {
            let [cw, ch, ox, oy, ..] = self.view_map;
            let [pw, phh] = self.photo_wh;
            let uv = Rect::from_min_max(pos2(ox / cw, oy / ch), pos2((ox + pw) / cw, (oy + phh) / ch));
            painter.image(t.id(), Rect::from_min_max(to_screen(0.0, 0.0), to_screen(owf, ohf)), uv, Color32::WHITE);
        }
        // 2. The sharp render on top, wherever it was made (it slides along while you move).
        if let (Some(t), Some((sp, sw, _))) = (&z.tex, &z.shown)
            && *sp == p
        {
            let r = Rect::from_min_max(to_screen(sw[0] as f32, sw[1] as f32), to_screen((sw[0] + sw[2]) as f32, (sw[1] + sw[3]) as f32));
            painter.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // Sharp render needed? Only above the preview's own resolution.
        let preview_scale = ph.preview.width() as f32 / ph.full.width() as f32;
        let x0 = (c[0] - vw / 2.0).max(0.0).floor();
        let y0 = (c[1] - vh / 2.0).max(0.0).floor();
        let win = [x0 as u32, y0 as u32, ((c[0] + vw / 2.0).min(owf).ceil() - x0).max(1.0) as u32, ((c[1] + vh / 2.0).min(ohf).ceil() - y0).max(1.0) as u32];
        let rscale = z.scale.min(1.0);
        let settled = now - z.moved > 0.2 && !ui.input(|i| i.pointer.any_down());
        let stale = z.shown.as_ref().is_none_or(|(sp, sw, ss)| *sp != p || *ss != rscale || sw[0] > win[0] || sw[1] > win[1] || sw[0] + sw[2] < win[0] + win[2] || sw[1] + sw[3] < win[1] + win[3]);
        if z.scale > preview_scale * 1.05 && stale && !z.busy {
            if settled {
                z.busy = true;
                let (full, tx, ctx) = (ph.full.clone(), self.tx.clone(), ui.ctx().clone());
                let preview = (p.neg.enabled && p.neg.levels.is_none()).then(|| ph.preview.clone());
                std::thread::spawn(move || {
                    let mut zp = p.clone();
                    if let Some(pv) = preview {
                        // A window can't be analysed on its own: use the whole frame's film base and levels.
                        let (b, l) = negative::analyse(&develop::geometry(&pv, &zp), &zp.neg);
                        (zp.neg.base, zp.neg.levels) = (Some(b), Some(l));
                    }
                    let mut img = develop::render_window(&full, &zp, 1.0, Some(win));
                    if rscale < 1.0 {
                        img = load::fit(&img, ((win[2].max(win[3]) as f32 * rscale).ceil() as u32).max(1));
                    }
                    let _ = tx.send(Msg::Zoomed(p, win, rscale, to_color_image(&img)));
                    ctx.request_repaint();
                });
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
        if !resp.dragged() && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }

    /// Set the rating and/or flag of a photo. The open photo changes through its settings (so undo covers it).
    fn rate(&mut self, f: &Path, rating: Option<u8>, flag: Option<i8>, label: Option<u8>) {
        if self.module == Module::Cull {
            let text = match (rating, flag, label) {
                (Some(0), ..) => "No stars".to_string(),
                (Some(r), ..) => "★".repeat(r as usize),
                (_, Some(1), _) => "Pick".into(),
                (_, Some(-1), _) => "Reject".into(),
                (_, Some(_), _) => "Flag cleared".into(),
                (.., Some(l)) => ["Red", "Yellow", "Green", "Blue", "Purple"].get(l.saturating_sub(1) as usize).unwrap_or(&"Label").to_string(),
                _ => String::new(),
            };
            self.cull.flash = Some((text, std::time::Instant::now()));
        }
        if let (Some(r), Some(e)) = (rating, self.search.get_mut(f)) {
            e.rating = r;
        }
        let apply = |p: &mut Params| {
            if let Some(r) = rating {
                p.rating = r;
            }
            if let Some(fl) = flag {
                p.flag = fl;
            }
            if let Some(l) = label {
                p.label = if p.label == l { 0 } else { l }; // pressing a label again clears it
            }
        };
        if self.current.as_deref() == Some(f) {
            apply(&mut self.params);
            return;
        }
        let mut p = read_params(f).unwrap_or_default();
        apply(&mut p);
        match write_params(f, &p) {
            Ok(()) => {
                self.meta.insert(f.to_path_buf(), Meta::of(&p));
            }
            Err(e) => self.status = format!("Could not save rating: {e}"),
        }
    }

    fn make_contact_sheet(&mut self) {
        let files = self.lib_visible();
        let Some(mut out) = rfd::FileDialog::new().set_file_name("contact-sheet.jpg").add_filter("JPEG", &["jpg"]).save_file() else { return };
        if out.extension().is_none() {
            out.set_extension("jpg"); // some save dialogs (GTK) drop it
        }
        self.status = format!("Making a contact sheet of {} photos…", files.len());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = save_contact_sheets(&files, 6, &out);
            let _ = tx.send(Msg::Status(match r {
                Ok(w) if w.len() == 1 => format!("Saved {}", out.file_name().unwrap_or_default().to_string_lossy()),
                Ok(w) => format!("Saved {} contact sheet pages", w.len()),
                Err(e) => format!("Contact sheet failed: {e}"),
            }));
        });
    }

    // ---------- culling ----------

    /// Ask for a photo's culling image (fast embedded preview, focus map and raw statistics), off the UI thread.
    fn request_cull(&mut self, ctx: &egui::Context, f: &Path) {
        if self.cull.cache.contains_key(f) || self.cull.failed.contains(f) || !self.cull.pending.insert(f.to_path_buf()) {
            return;
        }
        let (f, tx, ctx2, wanted) = (f.to_path_buf(), self.tx.clone(), ctx.clone(), self.cull.wanted.clone());
        std::thread::spawn(move || {
            let _one = DECODE.lock().unwrap_or_else(|e| e.into_inner());
            if !wanted.lock().is_ok_and(|w| w.contains(&f)) {
                let _ = tx.send(Msg::Cull(f, None, true)); // stepped past it while it waited
                return;
            }
            let data = (|| {
                let (img, toned) = load::load_quick_toned(&f, 2560).ok()?;
                let mut p = Params { border: Default::default(), ..read_params(&f).unwrap_or_default() };
                if toned {
                    p.cam_curve.clear(); // the camera's JPEG already has it
                }
                let out = develop::render(&img, &p, 0.3);
                let raw = load::raw_stats(&f);
                let (bins, clipped) = match &raw {
                    Some(r) => (r.bins, r.clipped),
                    None => {
                        let n = out.len() as f32 / 3.0;
                        let c = [0, 1, 2].map(|k| out.pixels().filter(|p| p.0[k] >= 0.998).count() as f32 / n * 100.0);
                        (histogram_bins(&out), c)
                    }
                };
                Some(CullData { focus: focus_map(&out), img: to_color_image(&out), bins, clipped, raw: raw.is_some() })
            })();
            let _ = tx.send(Msg::Cull(f, data, false));
            ctx2.request_repaint();
        });
    }

    fn cull_view(&mut self, ui: &mut Ui) {
        let vis = self.lib_visible();
        if self.lib_sel.as_ref().is_none_or(|s| !vis.contains(s)) {
            self.lib_sel = vis.first().cloned();
        }
        let Some(sel) = self.lib_sel.clone() else {
            ui.centered_and_justified(|ui| ui.label(RichText::new("No frames to cull").color(TEXT3)));
            return;
        };
        let i = vis.iter().position(|f| *f == sel).unwrap_or(0);
        // The one shown, the next two and the previous, so stepping through is instant.
        let ctx = ui.ctx().clone();
        if let Ok(mut w) = self.cull.wanted.lock() {
            *w = [i, i + 1, i.wrapping_sub(1), i + 2].iter().filter_map(|j| vis.get(*j).cloned()).collect();
        }
        for j in [i, i + 1, i.wrapping_sub(1), i + 2] {
            if let Some(f) = vis.get(j) {
                self.request_cull(&ctx, f);
            }
        }
        // Keep the cache small: only photos near the selection.
        let near: Vec<PathBuf> = vis.iter().skip(i.saturating_sub(4)).take(10).cloned().collect();
        self.cull.cache.retain(|k, _| near.contains(k));

        let avail = ui.available_rect_before_wrap().shrink(16.0);
        let shown: Vec<PathBuf> = if self.cull.compare { vis.iter().skip(i).take(2).cloned().collect() } else { vec![sel.clone()] };
        let n = shown.len().max(1) as f32;
        let slot_w = (avail.width() - 12.0 * (n - 1.0)) / n;
        for (k, f) in shown.iter().enumerate() {
            let slot = Rect::from_min_size(pos2(avail.left() + k as f32 * (slot_w + 12.0), avail.top()), vec2(slot_w, avail.height() - 30.0));
            let Some(c) = self.cull.cache.get(f) else {
                if self.cull.failed.contains(f) {
                    ui.put(slot, egui::Label::new(RichText::new(format!("Can't open {}", f.file_name().unwrap_or_default().to_string_lossy())).color(TEXT2)));
                } else {
                    ui.put(slot, egui::Spinner::new());
                }
                continue;
            };
            let s = c.tex.size_vec2();
            let r = Rect::from_center_size(slot.center(), s * (slot.width() / s.x).min(slot.height() / s.y));
            egui::Image::new(&c.tex).corner_radius(2).paint_at(ui, r);
            if self.cull.focus {
                egui::Image::new(&c.focus).paint_at(ui, r);
            }
            if self.cull.compare && *f == sel {
                ui.painter().rect_stroke(r.expand(3.0), 4.0, Stroke::new(2.0, ACCENT), egui::StrokeKind::Outside);
            }
            let resp = ui.interact(r, ui.scope_id().with(("cull", k)), Sense::click());
            if resp.clicked() {
                self.lib_sel = Some(f.clone());
            }
            if resp.double_clicked() {
                self.module = Module::Develop;
                self.select(ui.ctx(), f.clone());
            }
            // Name, stars, flag and label under the photo.
            let m = self.meta.get(f).copied().unwrap_or_default();
            let y = r.bottom() + 16.0;
            let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
            ui.painter().text(pos2(r.left(), y), Align2::LEFT_CENTER, name, FontId::proportional(12.5), if m.flag < 0 { TEXT3 } else { TEXT });
            let mut x = r.right();
            if m.label > 0 {
                ui.painter().circle_filled(pos2(x - 6.0, y), 6.0, LABEL_COLORS[(m.label - 1).min(4) as usize]);
                x -= 22.0;
            }
            // Pick or reject in the same filled form as the panel's buttons, so the state reads at a glance.
            if m.flag != 0 {
                let t = if m.flag > 0 { "Pick" } else { "Reject" };
                let g = ui.painter().layout_no_wrap(t.into(), FontId::new(12.5, semi()), INK);
                let b = Rect::from_min_max(pos2(x - g.size().x - 16.0, y - 11.0), pos2(x, y + 11.0));
                ui.painter().rect_filled(b, 3.0, if m.flag > 0 { BONE } else { hex(0xE5484D) });
                ui.painter().galley(b.center() - g.size() / 2.0, g, INK);
                x = b.left() - 14.0;
            }
            // All five stars, empty ones dim: an unrated frame shows that it is unrated.
            let (sr, first) = (7.0, x - 4.0 * 7.0 * 2.4 - 7.0);
            paint_stars(ui.painter(), pos2(first, y), 5, sr, BTN_BORDER);
            paint_stars(ui.painter(), pos2(first, y), m.rating, sr, ACCENT);
            // What the key just did, large on the photo, fading out over a second.
            if *f == sel
                && let Some((text, t0)) = &self.cull.flash
            {
                let age = t0.elapsed().as_secs_f32();
                if age < 1.0 {
                    let a = (1.0 - (age - 0.5).max(0.0) * 2.0).clamp(0.0, 1.0);
                    let stars = text.chars().filter(|c| *c == '★').count();
                    let fill = Color32::from_rgba_unmultiplied(20, 20, 22, (210.0 * a) as u8);
                    if stars > 0 {
                        // Drawn, not typed: the brand fonts have no star.
                        let b = Rect::from_center_size(r.center(), vec2(stars as f32 * 34.0 + 30.0, 64.0));
                        ui.painter().rect_filled(b, 8.0, fill);
                        paint_stars(ui.painter(), pos2(b.left() + 32.0, b.center().y), stars as u8, 14.0, BONE.gamma_multiply(a));
                    } else {
                        let g = ui.painter().layout_no_wrap(text.clone(), FontId::new(34.0, semi()), BONE.gamma_multiply(a));
                        let b = Rect::from_center_size(r.center(), g.size() + vec2(40.0, 20.0));
                        ui.painter().rect_filled(b, 8.0, fill);
                        ui.painter().galley(b.center() - g.size() / 2.0, g, BONE);
                    }
                    ui.ctx().request_repaint();
                }
            }
        }
        ui.allocate_rect(avail, Sense::hover());
    }

    fn cull_panel(&mut self, ui: &mut Ui) {
        let sel = self.lib_sel.clone();
        let k = |id: &str| keys::display(self.keymap.key(id));
        let (pick_key, reject_key, nav) = (k("pick"), k("reject"), format!("{} {}", k("next"), k("prev")));
        // Cull's one question is keep or not, so the answer comes first, each control with its key.
        if let Some(f) = sel.clone() {
            let m = self.meta.get(&f).copied().unwrap_or_default();
            ui.label(caps("Rating"));
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (r, resp) = ui.allocate_exact_size(vec2(5.0 * 30.0, 30.0), Sense::click());
                let hover = resp.hover_pos().map(|p| (((p.x - r.left()) / 30.0) as u8 + 1).min(5));
                let first = pos2(r.left() + 15.0, r.center().y);
                for i in 0..5u8 {
                    let lit = i < hover.unwrap_or(m.rating);
                    paint_stars(ui.painter(), first + vec2(i as f32 * 30.0, 0.0), 1, 10.0, if lit { if hover.is_some() { TEXT2 } else { ACCENT } } else { BTN_BORDER });
                }
                if resp.on_hover_text("Click a star, or press 0–5").clicked()
                    && let Some(n) = hover
                {
                    self.rate(&f, Some(if n == m.rating { 0 } else { n }), None, None);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new("0–5").monospace().size(11.0).color(TEXT3)));
            });
            ui.add_space(4.0);
            ui.columns(2, |c| {
                for (i, (flag, name, key)) in [(1i8, "Pick", &pick_key), (-1, "Reject", &reject_key)].into_iter().enumerate() {
                    let on = m.flag == flag;
                    let b = egui::Button::new(RichText::new(format!("{name}   {key}")).color(if on { INK } else { TEXT }))
                        .fill(if on { if flag > 0 { BONE } else { hex(0xE5484D) } } else { Color32::TRANSPARENT })
                        .stroke(Stroke::new(1.0, if on { Color32::TRANSPARENT } else { BTN_BORDER }))
                        .min_size(vec2(c[i].available_width(), 32.0));
                    if c[i].add(b).clicked() {
                        self.rate(&f, None, Some(if on { 0 } else { flag }), None);
                    }
                }
            });
            ui.add_space(16.0);
        }
        let c = sel.as_ref().and_then(|f| self.cull.cache.get(f));
        ui.label(caps(if c.is_some_and(|c| c.raw) { "Raw histogram" } else { "Histogram" }));
        ui.add_space(4.0);
        if let Some(c) = c {
            histogram(ui, &c.bins);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Clipped").size(12.0).color(TEXT3));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (k, n) in ["B", "G", "R"].iter().enumerate() {
                        let v = c.clipped[2 - k];
                        ui.label(RichText::new(format!("{n} {v:.2}%")).monospace().size(11.5).color(if v > 0.5 { hex(0xE5484D) } else { TEXT2 }));
                    }
                });
            });
            ui.label(RichText::new(if c.raw { "From the sensor data, so it shows what can still be recovered." } else { "From the picture as rendered." }).size(11.0).color(TEXT3));
        } else {
            ui.add_space(72.0);
        }
        ui.add_space(14.0);
        ui.label(caps("View"));
        ui.horizontal(|ui| {
            ui.label(RichText::new("Focus check").size(12.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, &mut self.cull.focus));
        });
        ui.label(RichText::new("Green marks the sharpest edges. No green on the subject means it missed focus.").size(11.0).color(TEXT3));
        ui.horizontal(|ui| {
            ui.label(RichText::new("Compare with next").size(12.5));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, &mut self.cull.compare));
        });
        ui.add_space(14.0);
        ui.label(RichText::new(format!("{nav} next / previous. Press ? for every key.")).size(11.5).color(TEXT3));
        ui.add_space(14.0);
        let rejects = self.meta.values().filter(|m| m.flag < 0).count();
        // Only offered when there is something to move.
        if rejects > 0 && ui.add(egui::Button::new(format!("Move {rejects} reject{} to _rejects", if rejects == 1 { "" } else { "s" })).min_size(vec2(ui.available_width(), 32.0)))
            .on_hover_text("Moves rejected photos and their settings into a _rejects folder here. Nothing is deleted.").clicked()
        {
            self.move_rejects(ui.ctx());
        }
    }

    // ---------- chrome ----------

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            // Brand lockup: the bare mark and the lowercase wordmark in Bricolage, both bone.
            ui.spacing_mut().item_spacing.x = 8.0;
            let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
            paint_mark(ui.painter(), r, BONE);
            ui.label(RichText::new("ferrotype").family(display()).size(17.0).color(BONE).extra_letter_spacing(0.3));
            ui.add_space(18.0);
            ui.spacing_mut().item_spacing.x = 2.0;
            for (m, name) in [(Module::Library, "Library"), (Module::Scan, "Scan"), (Module::Cull, "Cull"), (Module::Develop, "Develop"), (Module::Negative, "Negative")] {
                if tab(ui, name, self.module == m).clicked() {
                    self.module = m;
                    self.tool = Tool::None;
                    self.convert_if_negative();
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                // Shortcuts is a dialog, not a view: a small button at the end of the bar, not a tab.
                if ui.add(egui::Button::new(RichText::new("?").family(mono500()).size(13.0).color(TEXT2)).min_size(vec2(28.0, 28.0))).on_hover_text(format!("Keyboard shortcuts and screen colour\nRendering: {}", crate::gpu::status())).clicked() {
                    self.keys_open = true;
                }
                let develop = !matches!(self.module, Module::Library | Module::Cull);
                if develop && primary(ui, "Print", self.photo.is_some()).on_hover_text("Print this frame to a file").clicked() {
                    self.export_dialog = Some(false);
                }
                // In every view, so a new roll is always one click away.
                if ui.add(egui::Button::new("Open roll…").min_size(vec2(0.0, 28.0))).on_hover_text("Open a folder of frames").clicked()
                    && let Some(d) = rfd::FileDialog::new().pick_folder()
                {
                    self.open_path(ui.ctx(), &d);
                }
                if !develop {
                    ui.add(egui::TextEdit::singleline(&mut self.lib_query).hint_text("Search keywords, camera, lens, iso>1600…").desired_width(260.0).margin(vec2(10.0, 6.0)))
                        .on_hover_text("Words match the name, keywords, camera, lens, date, roll and film notes. Put a minus in front to leave a word out (-word). You can also use iso>1600, focal<35, f<2.8, stars>=3, date:2026-05 and \"two words\".");
                    self.ensure_search();
                } else {
                    let name = self.current.as_ref().and_then(|c| c.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let line = [name, self.status.clone()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                    ui.label(RichText::new(line).monospace().color(TEXT3).size(11.0));
                }
            });
        });
    }

    fn canvas_toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let neg = self.module == Module::Negative;
            // Mockup order: Negative | Split | Positive in the Negative module.
            let (labels, order): (&[&str], &[View]) = if neg { (&["Positive", "Negative", "Split"], &[View::After, View::Before, View::Split]) } else { (&["After", "Before", "Split", "2-up"], &[View::After, View::Before, View::Wipe, View::Split]) };
            let mut i = order.iter().position(|v| *v == self.view).unwrap_or(0);
            segmented(ui, &mut i, labels);
            self.view = order[i];
            // Groups: views | tools ... zoom & paper. Copy, paste and reset live under the settings panel.
            let divider = |ui: &mut Ui| {
                ui.add_space(4.0);
                let (r, _) = ui.allocate_exact_size(vec2(1.0, 20.0), Sense::hover());
                ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, BORDER));
                ui.add_space(4.0);
            };
            if !neg {
                divider(ui);
                for (t, name, tip) in [(Tool::Crop, "Crop", "Drag the corners, or drag inside to move"), (Tool::Heal, "Heal", "Click a spot to heal it. Right-click a circle to remove it"),
                                       (Tool::Mask, "Mask", "Adjust part of the photo with gradients, circles, painting, or brightness and colour ranges"), (Tool::Sample, "Sample", "Click to pin a colour readout (up to 8). Right-click one to remove it")] {
                    if ghost(ui, name, self.tool == t).on_hover_text(tip).clicked() {
                        self.tool = if self.tool == t { Tool::None } else { t };
                        self.view = View::After;
                        self.zoom = None;
                        // Open the matching panel section.
                        match self.tool { Tool::Crop => self.open[6] = true, Tool::Heal => self.open[10] = true, Tool::Mask => self.open[11] = true, Tool::None | Tool::Sample | Tool::Chart => {} }
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !neg {
                    let papers = crate::output::PROOF_PAPERS.len();
                    let names: Vec<&str> = std::iter::once("Screen").chain(crate::output::PROOF_PAPERS.iter().map(|p| p.0)).collect();
                    let printer_name = self.printer.as_ref().map(|p| p.name.clone());
                    let shown = match (self.proof, &printer_name) {
                        (0, _) => "Proof: off".to_string(),
                        (k, Some(n)) if k > papers => format!("Proof: {n}"),
                        (k, _) => format!("Proof: {}", names[k.min(papers)]),
                    };
                    let before = (self.proof, self.colour.perceptual, self.colour.paper);
                    let mut load = false;
                    egui::ComboBox::from_id_salt("proof").width(110.0).selected_text(shown).show_ui(ui, |ui| {
                        for (k, n) in names.iter().enumerate() {
                            pick(ui, &mut self.proof, k, *n);
                        }
                        ui.separator();
                        if let Some(n) = &printer_name {
                            pick(ui, &mut self.proof, papers + 1, format!("Printer: {n}"));
                            let mut warn = self.proof == papers + 2;
                            if ui.checkbox(&mut warn, "Show what it can't print").changed() {
                                self.proof = if warn { papers + 2 } else { papers + 1 };
                            }
                            ui.checkbox(&mut self.colour.perceptual, "Perceptual (else relative)");
                            ui.checkbox(&mut self.colour.paper, "Simulate paper and ink");
                        }
                        load = ui.button("Printer profile…").on_hover_text("Load the ICC profile for your printer and paper (RGB or CMYK)").clicked();
                    }).response.on_hover_text("Soft proof shows how a print will look. The papers listed are approximations, and a printer profile is exact");
                    if load
                        && let Some(f) = rfd::FileDialog::new().add_filter("ICC profile", &["icc", "icm"]).pick_file()
                    {
                        self.colour.printer = f.to_string_lossy().to_string();
                        self.load_printer();
                        if self.printer.is_some() {
                            self.proof = papers + 1;
                        }
                    }
                    if before.1 != self.colour.perceptual || before.2 != self.colour.paper {
                        self.load_printer();
                    }
                    if before != (self.proof, self.colour.perceptual, self.colour.paper) {
                        self.rendered = None;
                    }
                }
                if let Some(t) = &self.tex
                    && self.photo.is_some()
                {
                    let pct = self.shown_w / (t.size()[0] as f32 / self.tex_scale) * 100.0 * ui.ctx().pixels_per_point();
                    let label = match &self.zoom { Some(z) if z.from_fit.is_none() => format!("{:.0}%", z.scale * 100.0), Some(_) => "…".into(), None => format!("Fit {pct:.0}%") };
                    if ghost(ui, &label, self.zoom.is_some()).on_hover_text("Press Z or double-click the photo for a 100% view. Zoom with the mouse wheel or a pinch, and move around by dragging or scrolling with two fingers").clicked() {
                        self.toggle_zoom([0.5, 0.5]);
                    }
                }
            });
        });
    }

    fn filmstrip(&mut self, ui: &mut Ui) {
        let mut open = None;
        let (size, gap) = (vec2(93.0, 62.0), 6.0);
        let step = size.x + gap;
        let here = if self.module == Module::Cull { self.lib_sel.clone() } else { self.current.clone() };
        let here_i = here.as_ref().and_then(|h| self.files.iter().position(|f| f == h));
        // Bring the current frame into view when it changes (stepping with the arrow keys).
        let id = ui.scope_id().with("strip_here");
        let moved = ui.data(|d| d.get_temp::<Option<usize>>(id)) != Some(here_i);
        ui.data_mut(|d| d.insert_temp(id, here_i));
        let mut area = egui::ScrollArea::horizontal().auto_shrink([false, true]);
        if moved && let Some(i) = here_i {
            let w = ui.available_width();
            let (x, visible) = (i as f32 * step, ui.data(|d| d.get_temp::<f32>(id.with("x"))).unwrap_or(0.0));
            if x < visible || x + size.x > visible + w {
                area = area.horizontal_scroll_offset((x - (w - size.x) / 2.0).max(0.0));
            }
        }
        // Only the frames in view are drawn: a roll of thousands would otherwise touch (and reload) every thumbnail
        // each frame, far more than the texture budget holds.
        let out = area.show_viewport(ui, |ui, vp| {
            let n = self.files.len();
            let h = ui.available_height();
            ui.set_min_size(vec2((n as f32 * step - gap).max(0.0), h));
            let origin = pos2(ui.max_rect().left(), ui.max_rect().top() + ((h - size.y) / 2.0).max(0.0));
            let first = (vp.min.x / step).floor().max(0.0) as usize;
            let last = ((vp.max.x / step).ceil().max(0.0) as usize + 1).min(n);
            for i in first..last {
                let f = &self.files[i];
                let rect = Rect::from_min_size(origin + vec2(i as f32 * step, 0.0), size);
                let r = ui.interact(rect, ui.scope_id().with(("strip", i)), Sense::click());
                match self.thumb(f) {
                    Some(t) => egui::Image::new(t).uv(cover_uv(t, 1.5)).corner_radius(3).paint_at(ui, rect),
                    None => {
                        ui.painter().rect_filled(rect, 3.0, CARD);
                    }
                }
                // Marks, so a culled roll reads at a glance: stars, label dot, rejects dimmed.
                let m = self.meta.get(f).copied().unwrap_or_default();
                let p = ui.painter_at(rect);
                if m.flag < 0 {
                    p.rect_filled(rect, 3.0, Color32::from_black_alpha(150));
                    p.text(rect.center(), Align2::CENTER_CENTER, "×", FontId::proportional(20.0), TEXT2);
                }
                if m.rating > 0 {
                    let w = m.rating as f32 * 9.0 + 6.0;
                    let b = Rect::from_min_size(pos2(rect.left() + 3.0, rect.bottom() - 15.0), vec2(w, 12.0));
                    p.rect_filled(b, 3.0, Color32::from_rgba_unmultiplied(20, 20, 22, 210));
                    paint_stars(&p, pos2(b.left() + 7.5, b.center().y), m.rating, 3.5, BONE);
                }
                if m.flag > 0 {
                    let b = Rect::from_min_size(pos2(rect.left() + 3.0, rect.top() + 3.0), vec2(14.0, 14.0));
                    p.rect_filled(b, 3.0, Color32::from_rgba_unmultiplied(20, 20, 22, 210));
                    paint_flag(&p, b.center(), BONE);
                }
                if m.label > 0 {
                    p.circle_filled(pos2(rect.right() - 8.0, rect.top() + 8.0), 4.5, LABEL_COLORS[(m.label - 1).min(4) as usize]);
                    p.circle_stroke(pos2(rect.right() - 8.0, rect.top() + 8.0), 4.5, Stroke::new(1.0, INK));
                }
                if Some(i) == here_i {
                    ui.painter().rect_stroke(rect.expand(2.0), 4.0, Stroke::new(2.0, ACCENT), egui::StrokeKind::Outside);
                } else if r.hovered() {
                    ui.painter().rect_stroke(rect.expand(1.0), 4.0, Stroke::new(1.0, hex(0x55555C)), egui::StrokeKind::Outside);
                }
                if r.clicked() {
                    open = Some(f.clone());
                }
            }
        });
        ui.data_mut(|d| d.insert_temp(id.with("x"), out.state.clamped_offset().x));
        if let Some(f) = open {
            if self.module == Module::Cull {
                self.lib_sel = Some(f);
            } else {
                self.select(ui.ctx(), f);
            }
        }
    }

    fn image_view(&mut self, ui: &mut Ui) {
        let avail = ui.available_rect_before_wrap().shrink(24.0);
        let (Some(tex), Some(before)) = (self.tex.clone(), self.before_tex.clone()) else {
            ui.centered_and_justified(|ui| {
                if self.loading {
                    ui.spinner();
                } else if let Some(e) = &self.open_error {
                    ui.label(RichText::new(format!("Can't open this frame\n{e}")).color(WARN));
                } else {
                    ui.label(RichText::new("Pick a frame in the Library").color(TEXT3));
                }
            });
            return;
        };
        if self.photo.is_none() {
            ui.centered_and_justified(|ui| ui.spinner());
            return;
        }
        let fit = |t: &TextureHandle, area: egui::Vec2| {
            let s = t.size_vec2();
            s * (area.x / s.x).min(area.y / s.y)
        };
        let badge = |ui: &Ui, r: Rect, text: &str| {
            let g = ui.painter().layout_no_wrap(text.into(), FontId::new(11.0, semi()), TEXT);
            let b = Rect::from_min_size(r.min + vec2(10.0, 10.0), g.size() + vec2(16.0, 6.0));
            ui.painter().rect_filled(b, 4.0, Color32::from_rgba_unmultiplied(20, 20, 22, 215));
            ui.painter().galley(b.min + vec2(8.0, 3.0), g, TEXT);
        };
        let show_before = self.view == View::Before || self.holding_before;
        let neg = self.module == Module::Negative;
        match self.view {
            View::Split => {
                let half = vec2(avail.width() / 2.0 - 8.0, avail.height());
                let (sa, sb) = (fit(&before, half), fit(&tex, half));
                let ra = Rect::from_center_size(pos2(avail.center().x - avail.width() / 4.0 - 4.0, avail.center().y), sa);
                let rb = Rect::from_center_size(pos2(avail.center().x + avail.width() / 4.0 + 4.0, avail.center().y), sb);
                egui::Image::new(&before).corner_radius(2).paint_at(ui, ra);
                egui::Image::new(&tex).corner_radius(2).paint_at(ui, rb);
                badge(ui, ra, if neg { "Negative" } else { "Before" });
                badge(ui, rb, if neg { "Positive" } else { "After" });
                self.shown_w = sb.x;
            }
            View::Wipe if self.zoom.is_none() => {
                // "Before" here is this photo with its framing but none of the look, so both halves line up.
                let bp = Params { border: self.params.border.clone(), ..self.params.with_look_of(&Params::default()) };
                if self.wipe_tex.as_ref().is_none_or(|(k, _)| *k != bp)
                    && let Some(ph) = &self.photo
                {
                    let img = develop::add_border(develop::render(&ph.preview, &Params { border: Default::default(), ..bp.clone() }, ph.preview.width() as f32 / ph.full.width() as f32), &bp.border);
                    let t = ui.ctx().load_texture("wipe", to_color_image(&img), TextureOptions::LINEAR);
                    self.wipe_tex = Some((bp, t));
                }
                let size = fit(&tex, avail.size());
                let r = Rect::from_center_size(avail.center(), size);
                self.shown_w = size.x;
                let resp = ui.interact(r, ui.scope_id().with("wipe"), Sense::click_and_drag());
                if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
                    self.wipe = ((p.x - r.left()) / r.width()).clamp(0.0, 1.0);
                }
                egui::Image::new(&tex).corner_radius(2).paint_at(ui, r);
                let x = r.left() + r.width() * self.wipe;
                if let Some((_, bt)) = &self.wipe_tex {
                    let left = Rect::from_min_max(r.min, pos2(x, r.bottom()));
                    ui.painter().image(bt.id(), left, Rect::from_min_max(pos2(0.0, 0.0), pos2(self.wipe, 1.0)), Color32::WHITE);
                }
                ui.painter().line_segment([pos2(x, r.top()), pos2(x, r.bottom())], Stroke::new(2.0, Color32::WHITE));
                ui.painter().circle_filled(pos2(x, r.center().y), 9.0, Color32::WHITE);
                ui.painter().text(pos2(x, r.center().y), Align2::CENTER_CENTER, "‹›", FontId::new(11.0, semi()), hex(0x202024));
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                badge(ui, r, "Before");
                let g = ui.painter().layout_no_wrap("After".into(), FontId::new(11.0, semi()), TEXT);
                let b = Rect::from_min_size(pos2(r.right() - g.size().x - 26.0, r.top() + 10.0), g.size() + vec2(16.0, 6.0));
                ui.painter().rect_filled(b, 4.0, Color32::from_rgba_unmultiplied(20, 20, 22, 215));
                ui.painter().galley(b.min + vec2(8.0, 3.0), g, TEXT);
            }
            _ if self.zoom.is_some() && !show_before => self.zoom_view(ui, avail),
            _ => {
                let t = if show_before { &before } else { &tex };
                let size = fit(t, avail.size());
                let r = Rect::from_center_size(avail.center(), size);
                let resp = ui.interact(r, ui.scope_id().with("canvas"), Sense::click_and_drag());
                egui::Image::new(t).corner_radius(2).paint_at(ui, r);
                self.shown_w = size.x;
                if show_before {
                    badge(ui, r, if neg { "Negative" } else { "Before" });
                }
                // Hold on the image to compare with the original (released = back to the edit).
                let tool = if neg { Tool::None } else { self.tool };
                let picking = self.picking_base || self.picking_wb;
                self.holding_before = tool == Tool::None && !picking && resp.is_pointer_button_down_on();
                if tool == Tool::None
                    && !picking
                    && resp.double_clicked()
                    && let Some(p) = resp.interact_pointer_pos()
                {
                    // Canvas → fraction of the photo (without its border).
                    let [cw, ch, ox, oy, ..] = self.view_map;
                    let uv = (p - r.min) / r.size();
                    let [pw, ph] = self.photo_wh;
                    self.toggle_zoom([((uv.x * cw - ox) / pw).clamp(0.0, 1.0), ((uv.y * ch - oy) / ph).clamp(0.0, 1.0)]);
                }
                // Wheel or pinch on the fitted photo zooms in around the pointer.
                let (factor, _, at) = view_gestures(ui, r);
                if factor > 1.0
                    && let Some(at) = at
                    && self.photo.is_some()
                    && !neg
                {
                    let [cw, ch, ox, oy, ..] = self.view_map;
                    let uv = (at - r.min) / r.size();
                    let [pw, ph] = self.photo_wh;
                    let mut z = Zoom::at([((uv.x * cw - ox) / pw).clamp(0.0, 1.0), ((uv.y * ch - oy) / ph).clamp(0.0, 1.0)], 1.0, Some(at));
                    // Fit for the photo without its border is close to this canvas's; the first frame sets it exactly.
                    z.from_fit = Some(factor);
                    self.zoom = Some(z);
                    self.tool = Tool::None;
                    self.view = View::After;
                }
                if !show_before {
                    match tool {
                        Tool::Crop => self.crop_tool(ui, &resp, r),
                        Tool::Heal => self.heal_tool(ui, &resp, r),
                        Tool::Mask => self.mask_tool(ui, &resp, r),
                        Tool::Chart => self.chart_tool(ui, &resp, r),
                        Tool::None | Tool::Sample => {}
                    }
                    self.sample_marks(ui, &resp, r, tool == Tool::Sample);
                }
                self.hover_rgb = resp.hover_pos().and_then(|p| {
                    let ci = self.last_rgb.as_ref()?;
                    let uv = (p - r.min) / r.size();
                    let (x, y) = ((uv.x * ci.size[0] as f32) as usize, (uv.y * ci.size[1] as f32) as usize);
                    let c = ci.pixels.get(y.min(ci.size[1] - 1) * ci.size[0] + x.min(ci.size[0] - 1))?;
                    Some([c.r(), c.g(), c.b()])
                });
                if self.picking_wb && !show_before {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                    badge(ui, r, "Click something that should be neutral grey");
                    if resp.clicked()
                        && let Some(pos) = resp.interact_pointer_pos()
                    {
                        let uv = (pos - r.min) / r.size();
                        self.pick_wb(uv.x, uv.y);
                    }
                }
                if self.picking_base {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                    badge(ui, r, "Click the unexposed film edge");
                    if resp.clicked()
                        && let Some(pos) = resp.interact_pointer_pos()
                    {
                        let uv = (pos - r.min) / r.size();
                        self.pick_base(uv.x, uv.y);
                    }
                }
            }
        }
    }

    /// Eyedropper for the film base: average a small patch of the (un-inverted) scan.
    fn pick_base(&mut self, u: f32, v: f32) {
        let Some(photo) = &self.photo else { return };
        let geo = develop::geometry(&photo.preview, &self.params);
        let (w, h) = geo.dimensions();
        // The view may include a border: map the click from the framed canvas back onto the photo.
        let (cw, ch, ox, oy, _) = develop::border_layout(w, h, &self.params.border);
        let (cx, cy) = ((u * cw as f32) as i64 - ox as i64, (v * ch as f32) as i64 - oy as i64);
        let mut acc = [0f32; 3];
        let mut n = 0.0;
        for y in (cy - 3).max(0)..(cy + 4).min(h as i64) {
            for x in (cx - 3).max(0)..(cx + 4).min(w as i64) {
                let p = geo.get_pixel(x as u32, y as u32).0;
                (0..3).for_each(|c| acc[c] += p[c]);
                n += 1.0;
            }
        }
        if n > 0.0 {
            self.params.neg.base = Some(acc.map(|a| (a / n).max(1e-4)));
            self.params.neg.levels = None;
            self.picking_base = false;
        }
    }

    fn histogram_block(&mut self, ui: &mut Ui) {
        ui.label(caps("Histogram"));
        ui.add_space(4.0);
        histogram(ui, &self.bins);
        // Pinned sample points: live readouts from the rendered photo.
        let mut remove = None;
        for (k, f) in self.samples.iter().enumerate() {
            let c = self.sample_rgb(*f);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}", k + 1)).monospace().size(11.0).color(ACCENT));
                let (sw, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                if let Some(c) = c {
                    ui.painter().rect_filled(sw, 2.0, Color32::from_rgb(c[0], c[1], c[2]));
                }
                let t = c.map(|c| format!("R {:>3}  G {:>3}  B {:>3}  L {:>3}%", c[0], c[1], c[2], (develop::luma(c.map(|v| v as f32 / 255.0)) * 100.0).round())).unwrap_or_default();
                ui.label(RichText::new(t).monospace().size(11.0).color(TEXT2));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Label::new(RichText::new("×").size(12.0).color(TEXT3)).sense(Sense::click())).on_hover_text("Remove this sample point").clicked() {
                        remove = Some(k);
                    }
                });
            });
        }
        if let Some(k) = remove {
            self.samples.remove(k);
        }
        ui.horizontal(|ui| {
            let rgb = self.hover_rgb.map(|c| format!("R {:>3}  G {:>3}  B {:>3}", c[0], c[1], c[2])).unwrap_or_default(); // values appear while the pointer is over the photo
            ui.label(RichText::new(rgb).monospace().size(11.0).color(TEXT3));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // A switch, like Cull's Focus check: a button reading "Clipping off" looked like a state, not a control.
                switch(ui, &mut self.clip_view).on_hover_text("Show clipped highlights and shadows on the photo (J)");
                ui.label(RichText::new("Clipping").size(12.0).color(LABEL));
            });
        });
    }

    // ---------- Library ----------

    /// Save a look to the stocks folder under a name no other stock uses ("Matte 2" when "Matte" exists), with
    /// characters Windows forbids replaced. It joins the list only once the file is written. Returns the name used.
    fn add_preset(&mut self, name: &str, p: Params) -> Result<String, String> {
        let clean: String = name.trim().chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '-' } else { c }).collect();
        let clean = clean.trim_matches(|c: char| c == '.' || c == ' ').to_string();
        let clean = if clean.is_empty() { "Stock".to_string() } else { clean };
        let dir = user_preset_dir().ok_or("no settings folder")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let taken = |n: &str| self.presets.iter().any(|(x, _)| x.eq_ignore_ascii_case(n)) || dir.join(format!("{n}.json")).exists();
        let name = std::iter::once(clean.clone()).chain((2..).map(|k| format!("{clean} {k}"))).find(|n| !taken(n)).unwrap();
        crate::write_atomic(&dir.join(format!("{name}.json")), serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?.as_bytes()).map_err(|e| e.to_string())?;
        self.presets.push((name.clone(), p));
        self.preset_tex.clear();
        Ok(name)
    }

    /// What "Print all" prints: the selection, else what Library shows. Rejects are never printed.
    fn print_frames(&self) -> Vec<PathBuf> {
        if !self.export_sel.is_empty() {
            return self.export_sel.clone();
        }
        self.lib_visible().into_iter().filter(|f| self.meta.get(f).is_none_or(|m| m.flag >= 0)).collect()
    }

    fn lib_visible(&self) -> Vec<PathBuf> {
        let q = self.lib_query.to_lowercase();
        self.files.iter().filter(|f| {
            let real = load::copy_of(f).unwrap_or(f.to_path_buf());
            let e = ext_of(&real).to_lowercase();
            let kind = if load::is_raw(&real) { 0 } else if e == "jpg" || e == "jpeg" { 1 } else if e == "tif" || e == "tiff" { 2 } else { 3 };
            let m = self.meta.get(*f).copied().unwrap_or_default();
            let rating_ok = match self.lib_rating { 1 => m.rating >= 1 && m.flag >= 0, 2 => m.rating >= 3 && m.flag >= 0, 3 => m.flag > 0, _ => true };
            // The full search once its details are in; the file name meanwhile.
            let found = q.trim().is_empty() || match self.search.get(*f) {
                Some(e) => crate::library::matches(e, &q),
                None => f.file_name().unwrap_or_default().to_string_lossy().to_lowercase().contains(q.trim()),
            };
            self.lib_filter[kind] && (!self.lib_filter[4] || m.edited) && rating_ok && found
        }).cloned().collect()
    }

    fn library_left(&mut self, ui: &mut Ui) {
        ui.label(caps("Roll"));
        ui.add_space(2.0);
        match self.folder.clone() {
            None if self.collection.is_none() => {
                ui.label(RichText::new("No roll open").color(TEXT3));
            }
            None => {
                ui.label(RichText::new("Showing a collection").size(12.0).color(TEXT2));
            }
            Some(dir) => {
                if let Some(parent) = dir.parent()
                    && ui.add(egui::Button::new(RichText::new("‹  Up one level").color(TEXT2)).fill(Color32::TRANSPARENT).stroke(Stroke::NONE)).clicked()
                {
                    self.open_path(ui.ctx(), &parent.to_path_buf());
                    return;
                }
                let name = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
                list_row(ui, &name, &self.files.len().to_string(), true);
                for s in self.subdirs.clone().iter().take(40) {
                    if list_row(ui, &format!("   {}", s.file_name().unwrap_or_default().to_string_lossy()), "", false).clicked() {
                        self.open_path(ui.ctx(), s);
                        return;
                    }
                }
            }
        }
        self.collections_panel(ui);
        ui.add_space(18.0);
        ui.label(caps("Show rated"));
        segmented(ui, &mut self.lib_rating, &["All", "1+ stars", "3+", "Picks"]);
        ui.add_space(18.0);
        ui.label(caps("Status"));
        list_row(ui, "Edited", &self.meta.values().filter(|m| m.edited).count().to_string(), false);
        list_row(ui, "Film scans", &self.meta.values().filter(|m| m.neg).count().to_string(), false);
        list_row(ui, "Rejects", &self.meta.values().filter(|m| m.flag < 0).count().to_string(), false);
        ui.add_space(18.0);
        ui.label(caps("Watch folder"));
        let toggle = |ui: &mut Ui, label: &str, v: &mut bool, tip: &str| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(12.5).color(LABEL)).on_hover_text(tip);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, v));
            });
        };
        toggle(ui, "Pick up new frames", &mut self.watch, "For a scanner or tethered camera that saves into this folder. New files appear within two seconds");
        if self.watch {
            toggle(ui, "Open each new photo", &mut self.watch_open, "Jump straight to the newest arrival");
            toggle(ui, "Give it these settings", &mut self.watch_apply, "New frames get the open photo's look and film conversion (handy when scanning a roll)");
        }
        // Previews progress only while it is still working.
        if self.thumb_ready.len() >= self.files.len() {
            return;
        }
        ui.add_space((ui.available_height() - 62.0).max(18.0));
        let loaded = self.thumb_ready.len() as f32 / self.files.len().max(1) as f32;
        egui::Frame::new().fill(CARD).corner_radius(8).inner_margin(10.0).show(ui, |ui| ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Previews").size(12.0).color(TEXT2));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(format!("{}/{}", self.thumb_ready.len(), self.files.len())).monospace().color(TEXT2)));
            });
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, BTN_BORDER);
            ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() * loaded, 4.0)), 2.0, ACCENT);
        }));
    }

    fn library_toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            ui.label(caps("Show"));
            ui.add_space(4.0);
            for (i, n) in ["RAW", "JPEG", "TIFF", "Other"].into_iter().enumerate() {
                if filter_chip(ui, n, self.lib_filter[i]).clicked() {
                    self.lib_filter[i] = !self.lib_filter[i];
                }
            }
            ui.add_space(10.0);
            if filter_chip(ui, "Edited only", self.lib_filter[4]).clicked() {
                self.lib_filter[4] = !self.lib_filter[4];
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(!self.files.is_empty(), egui::Button::new("Print all…")).clicked() {
                    self.export_sel.clear();
                    self.export_dialog = Some(true);
                }
                if ui.add_enabled(!self.files.is_empty(), egui::Button::new("Contact sheet…")).on_hover_text("One image of every photo shown, numbered, like a darkroom contact print").clicked() {
                    self.make_contact_sheet();
                }
                ui.add_space(8.0);
                plain_slider(ui, &mut self.thumb_size, 120.0, 320.0, 110.0).on_hover_text("Thumbnail size");
                ui.label(RichText::new("Size").size(12.0).color(TEXT3));
            });
        });
    }

    fn library_grid(&mut self, ui: &mut Ui) {
        let visible = self.lib_visible();
        if self.files.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 2.0 - 110.0);
                    let (r, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
                    paint_mark(ui.painter(), r, TEXT2);
                    ui.add_space(10.0);
                    ui.label(RichText::new("No roll yet").family(display()).size(30.0).color(BONE).extra_letter_spacing(-0.4));
                    ui.label(RichText::new("Open a folder of RAW, JPEG or TIFF frames, or drop them here.").color(TEXT2));
                    ui.add_space(8.0);
                    if primary(ui, "Open roll…", true).clicked()
                        && let Some(d) = rfd::FileDialog::new().pick_folder()
                    {
                        self.open_path(ui.ctx(), &d);
                    }
                });
            });
            return;
        }
        let (mut select, mut open, mut copy, mut remove) = (None, None, None, None);
        let mut pano = None::<crate::pano::Projection>;
        let mut export_sel = false;
        let (mut rename, mut rename_one, mut undo_rename) = (None::<bool>, None::<PathBuf>, false);
        let mut hdr = None::<bool>;
        let (mut add_to, mut add_files) = (None::<Option<usize>>, Vec::<PathBuf>::new());
        let lib_multi = self.lib_multi.clone();
        let can_undo_rename = self.folder.as_deref().is_some_and(crate::rename::can_undo);
        let (cmd, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        let gap = 12.0;
        let cols = ((ui.available_width() + gap) / (self.thumb_size + gap)).floor().max(1.0) as usize;
        let cell = (ui.available_width() - gap * (cols - 1) as f32) / cols as f32 - 0.5;
        let rows: Vec<&[PathBuf]> = visible.chunks(cols).collect();
        // Only the rows on screen are laid out, so folders of thousands of photos scroll smoothly.
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, cell / 1.5 + 26.0, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing = vec2(gap, gap);
            for row in &rows[range] {
                ui.horizontal(|ui| {
                    for f in row.iter() {
                        ui.allocate_ui_with_layout(vec2(cell, cell / 1.5 + 26.0), Layout::top_down(Align::LEFT), |ui| {
                            ui.spacing_mut().item_spacing.y = 6.0;
                            let size = vec2(cell, cell / 1.5);
                            let resp = match self.thumb(f) {
                                Some(t) => ui.add(egui::Image::new(t).uv(cover_uv(t, 1.5)).fit_to_exact_size(size).maintain_aspect_ratio(false).corner_radius(6).sense(Sense::click())),
                                None => {
                                    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
                                    ui.painter().rect_filled(r, 6.0, CARD);
                                    resp
                                }
                            };
                            let r = resp.rect;
                            // Type badge and edited mark, as in the mockup.
                            let g = ui.painter().layout_no_wrap(ext_of(load::copy_of(f).as_deref().unwrap_or(f)), FontId::monospace(10.0), TEXT);
                            let b = Rect::from_min_size(r.min + vec2(6.0, 6.0), g.size() + vec2(12.0, 4.0));
                            ui.painter().rect_filled(b, 4.0, Color32::from_rgba_unmultiplied(20, 20, 22, 215));
                            ui.painter().galley(b.min + vec2(6.0, 2.0), g, TEXT);
                            let m = self.meta.get(f).copied().unwrap_or_default();
                            if m.flag < 0 {
                                ui.painter().rect_filled(r, 6.0, Color32::from_black_alpha(150)); // rejected: dimmed
                            }
                            if m.rating > 0 || m.flag > 0 {
                                let b = Rect::from_min_size(pos2(r.left() + 6.0, r.bottom() - 22.0), vec2(m.rating as f32 * 12.0 + if m.flag > 0 { 18.0 } else { 6.0 }, 16.0));
                                ui.painter().rect_filled(b, 4.0, Color32::from_rgba_unmultiplied(20, 20, 22, 215));
                                paint_stars(ui.painter(), pos2(b.left() + 9.0, b.center().y), m.rating, 5.0, ACCENT);
                                if m.flag > 0 {
                                    paint_flag(ui.painter(), pos2(b.right() - 9.0, b.center().y), ACCENT);
                                }
                            }
                            if m.edited {
                                let c = pos2(r.right() - 14.0, r.top() + 14.0);
                                ui.painter().rect_filled(Rect::from_center_size(c, vec2(18.0, 18.0)), 4.0, Color32::from_rgba_unmultiplied(20, 20, 22, 215));
                                ui.painter().circle_filled(c, 3.5, ACCENT);
                            }
                            if m.label > 0 {
                                ui.painter().rect_filled(Rect::from_min_max(pos2(r.right() - 26.0, r.bottom() - 6.0), pos2(r.right() - 6.0, r.bottom() - 2.0)), 2.0, LABEL_COLORS[(m.label - 1).min(4) as usize]);
                            }
                            let is_copy = load::copy_of(f).is_some();
                            let colls: Vec<String> = self.collections.iter().filter(|c| !c.is_smart()).map(|c| c.name.clone()).collect();
                            let multi = self.lib_multi.len();
                            let in_multi = self.lib_multi.contains(f);
                            let pano_busy = self.pano_busy;
                            resp.context_menu(|ui| {
                                if multi >= 2 && in_multi {
                                    if ui.button(format!("Print {multi} frames…")).clicked() {
                                        export_sel = true;
                                        ui.close();
                                    }
                                    if ui.add_enabled(!pano_busy, egui::Button::new(format!("Merge {multi} photos to HDR"))).on_hover_text("Lines up an exposure bracket of one scene and merges it into a 32-bit photo that keeps the highlight and shadow detail").clicked() {
                                        hdr = Some(true);
                                        ui.close();
                                    }
                                    if ui.button(format!("Rename {multi} photos…")).clicked() {
                                        rename = Some(true);
                                        ui.close();
                                    }
                                    ui.add_enabled_ui(!pano_busy, |ui| {
                                        ui.menu_button(format!("Merge {multi} photos to panorama"), |ui| {
                                            for (pr, name) in crate::pano::Projection::ALL {
                                                if ui.button(name).clicked() {
                                                    pano = Some(pr);
                                                    ui.close();
                                                }
                                            }
                                        });
                                    });
                                    ui.separator();
                                }
                                if !(multi >= 2 && in_multi) && !is_copy && ui.button("Rename…").clicked() {
                                    rename = Some(false);
                                    rename_one = Some(f.clone());
                                    ui.close();
                                }
                                if ui.add_enabled(!pano_busy, egui::Button::new("Find and merge HDR brackets")).on_hover_text("Finds photos taken within a few seconds at different exposures, and merges each set").clicked() {
                                    hdr = Some(false);
                                    ui.close();
                                }
                                if can_undo_rename && ui.button("Undo last rename").clicked() {
                                    undo_rename = true;
                                    ui.close();
                                }
                                ui.menu_button(if multi >= 2 && in_multi { format!("Add {multi} to collection") } else { "Add to collection".into() }, |ui| {
                                    for (k, n) in colls.iter().enumerate() {
                                        if ui.button(n).clicked() {
                                            add_to = Some(Some(k));
                                            add_files = if multi >= 2 && in_multi { lib_multi.clone() } else { vec![f.clone()] };
                                            ui.close();
                                        }
                                    }
                                    if ui.button("New collection…").clicked() {
                                        add_to = Some(None);
                                        add_files = if multi >= 2 && in_multi { lib_multi.clone() } else { vec![f.clone()] };
                                        ui.close();
                                    }
                                });
                                if ui.button("Make a virtual copy").clicked() {
                                    copy = Some(f.clone());
                                    ui.close();
                                }
                                if is_copy && ui.button("Remove this virtual copy").clicked() {
                                    remove = Some(f.clone());
                                    ui.close();
                                }
                            });
                            let sel = self.lib_sel.as_ref() == Some(f) || in_multi;
                            if sel {
                                ui.painter().rect_stroke(r.expand(2.0), 7.0, Stroke::new(2.0, ACCENT), egui::StrokeKind::Outside);
                            } else if resp.hovered() {
                                ui.painter().rect_stroke(r.expand(1.0), 7.0, Stroke::new(1.0, hex(0x55555C)), egui::StrokeKind::Outside);
                            }
                            if resp.clicked() {
                                select = Some(f.clone());
                            }
                            if resp.double_clicked() {
                                open = Some(f.clone());
                            }
                            let name = match load::copy_of(f) {
                                Some(o) => format!("{} · {}", o.file_name().unwrap_or_default().to_string_lossy(), f.extension().unwrap_or_default().to_string_lossy().replace("copy", "copy ")),
                                None => f.file_name().unwrap_or_default().to_string_lossy().to_string(),
                            };
                            ui.add(egui::Label::new(RichText::new(name).size(12.0).color(if sel { TEXT } else { LABEL })).truncate());
                        });
                    }
                });
            }
        });
        if let Some(f) = select {
            // Ctrl/Cmd-click adds or removes one photo; Shift-click selects a run (for panoramas).
            if cmd || shift {
                if self.lib_multi.is_empty()
                    && let Some(s) = &self.lib_sel
                {
                    self.lib_multi.push(s.clone());
                }
                if shift {
                    let (a, b) = (self.lib_sel.as_ref().and_then(|s| visible.iter().position(|v| v == s)), visible.iter().position(|v| *v == f));
                    if let (Some(a), Some(b)) = (a, b) {
                        for v in &visible[a.min(b)..=a.max(b)] {
                            if !self.lib_multi.contains(v) {
                                self.lib_multi.push(v.clone());
                            }
                        }
                    }
                } else if let Some(i) = self.lib_multi.iter().position(|v| *v == f) {
                    self.lib_multi.remove(i);
                } else {
                    self.lib_multi.push(f.clone());
                }
                if self.lib_multi.len() >= 2 {
                    self.status = format!("{} photos selected: right-click to merge them into a panorama", self.lib_multi.len());
                }
            } else {
                self.lib_multi.clear();
            }
            self.lib_sel = Some(f);
        }
        if let Some(sel) = hdr {
            let groups = sel.then(|| {
                let mut files = self.lib_multi.clone();
                load::sort_photos(&mut files);
                vec![files]
            });
            self.merge_hdr(ui.ctx(), groups);
        }
        if let Some(multi) = rename {
            let mut files = if multi { self.lib_multi.clone() } else { rename_one.into_iter().collect() };
            load::sort_photos(&mut files);
            self.rename_dlg = Some(RenameDlg { files, pattern: "{date}-{n}".into(), roll: self.export.roll.clone(), preview: vec![], previewed: Default::default() });
        }
        if undo_rename && let Some(d) = self.folder.clone() {
            self.commit();
            let r = crate::rename::undo(&d);
            self.status = match &r { Ok(n) => format!("Put back {n} names"), Err(e) => format!("Could not undo: {e}") };
            if r.is_ok() {
                self.current = None;
                self.photo = None;
                self.open_path(ui.ctx(), &d);
            }
        }
        match add_to {
            Some(Some(k)) => {
                // Index among the plain collections (smart ones can't hold a hand-picked file).
                if let Some(c) = self.collections.iter_mut().filter(|c| !c.is_smart()).nth(k) {
                    let n = add_files.iter().filter(|f| !c.files.contains(f)).count();
                    for f in add_files {
                        if !c.files.contains(&f) {
                            c.files.push(f);
                        }
                    }
                    self.status = format!("Added {n} to {}", c.name);
                }
                self.save_collections();
            }
            Some(None) => self.new_coll = Some((String::new(), add_files)),
            None => {}
        }
        if export_sel {
            let mut files = self.lib_multi.clone();
            load::sort_photos(&mut files);
            self.export_sel = files;
            self.export_dialog = Some(true);
        }
        if let Some(pr) = pano {
            let mut files = self.lib_multi.clone();
            load::sort_photos(&mut files);
            self.merge_pano(ui.ctx(), files, pr);
        }
        if let Some(f) = open {
            self.select(ui.ctx(), f);
        }
        if let Some(f) = copy {
            let p = read_params(&f).unwrap_or_default();
            if let Some(c) = self.virtual_copy(&f, &p) {
                self.lib_sel = Some(c);
            }
        }
        if let Some(f) = remove {
            self.remove_copy(&f);
        }
    }

    /// Collections: open one, save the search as a smart one, name a new one, delete one (right-click).
    fn collections_panel(&mut self, ui: &mut Ui) {
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.label(caps("Collections"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let q = self.lib_query.trim().to_string();
                if !q.is_empty()
                    && let Some(dir) = self.folder.clone()
                    && small_action(ui, "Save search", false).on_hover_text("A smart collection runs this search over this folder and its subfolders, and stays up to date").clicked()
                {
                    let name = format!("{q} · {}", dir.file_name().unwrap_or_default().to_string_lossy());
                    self.collections.push(crate::library::Collection { name, query: q, folder: dir, ..Default::default() });
                    self.save_collections();
                }
                if self.new_coll.is_none() && small_action(ui, "+ New", false).on_hover_text("A collection of the selected photos (or an empty one)").clicked() {
                    let files = if self.lib_multi.is_empty() { self.lib_sel.clone().into_iter().collect() } else { self.lib_multi.clone() };
                    self.new_coll = Some((String::new(), files));
                }
            });
        });
        if let Some((name, files)) = &mut self.new_coll {
            let n = files.len();
            let resp = ui.add(egui::TextEdit::singleline(name).id_salt("new_coll").hint_text(format!("Name for {n} photo{}, then Enter", if n == 1 { "" } else { "s" })).desired_width(f32::INFINITY));
            // Focus it once when it appears, not every frame (that would trap the keyboard).
            let id = resp.id;
            if ui.memory(|m| m.data.get_temp::<bool>(id)).is_none() {
                resp.request_focus();
                ui.memory_mut(|m| m.data.insert_temp(id, true));
            }
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let done = (resp.has_focus() || resp.lost_focus()) && enter;
            let cancel = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
            if done || cancel {
                ui.memory_mut(|m| m.data.remove::<bool>(id));
            }
            if done && !name.trim().is_empty() {
                let c = crate::library::Collection { name: name.trim().to_string(), files: files.clone(), ..Default::default() };
                self.status = format!("Collection {} made", c.name);
                self.collections.push(c);
                self.new_coll = None;
                self.save_collections();
            } else if cancel || (done && name.trim().is_empty()) {
                self.new_coll = None;
            }
        }
        if self.collections.is_empty() && self.new_coll.is_none() {
            ui.label(RichText::new("Right-click photos to add them to one, or save a search.").size(11.5).color(TEXT3));
        }
        let mut open = None;
        let mut delete = None;
        for (i, c) in self.collections.iter().enumerate() {
            let on = self.collection.as_ref() == Some(&c.name);
            let count = if c.is_smart() { "smart".to_string() } else { c.files.len().to_string() };
            let resp = list_row(ui, &c.name, &count, on).on_hover_text(if c.is_smart() { format!("Search \"{}\" in {}", c.query, c.folder.display()) } else { "Right-click to delete".into() });
            if resp.clicked() {
                open = Some(i);
            }
            resp.context_menu(|ui| {
                if ui.button("Delete collection").on_hover_text("Deletes only the collection. The photos stay where they are").clicked() {
                    delete = Some(i);
                    ui.close();
                }
            });
        }
        if let Some(i) = open {
            self.open_collection(i);
        }
        if let Some(i) = delete {
            let c = self.collections.remove(i);
            if self.collection.as_ref() == Some(&c.name) {
                self.collection = None;
            }
            self.save_collections();
        }
    }

    /// Keywords of the selected photo (or of every selected photo, when several are): chips with ×, and a field.
    fn keywords_panel(&mut self, ui: &mut Ui, f: &Path) {
        let targets: Vec<PathBuf> = if self.lib_multi.len() >= 2 && self.lib_multi.iter().any(|m| m == f) { self.lib_multi.clone() } else { vec![f.to_path_buf()] };
        let kws = read_params(f).unwrap_or_default().keywords;
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(caps("Keywords"));
            if targets.len() > 1 {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(format!("{} photos", targets.len())).size(11.0).color(TEXT3)));
            }
        });
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            for k in &kws {
                if chip(ui, &format!("{k}  ×"), false).on_hover_text("Remove").clicked() {
                    remove = Some(k.clone());
                }
            }
        });
        let resp = ui.add(egui::TextEdit::singleline(&mut self.kw_text).hint_text("Add: street, Lisbon, night").desired_width(f32::INFINITY));
        let add = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let mut change: Option<(Vec<String>, Option<String>)> = None;
        if add {
            let new = crate::library::split_keywords(&self.kw_text);
            self.kw_text.clear();
            if !new.is_empty() {
                change = Some((new, None));
            }
            resp.request_focus();
        }
        if let Some(r) = remove {
            change = Some((vec![], Some(r)));
        }
        if let Some((add, rm)) = change {
            for t in &targets {
                let mut q = read_params(t).unwrap_or_default();
                for k in &add {
                    if !q.keywords.iter().any(|o| o.eq_ignore_ascii_case(k)) {
                        q.keywords.push(k.clone());
                    }
                }
                if let Some(r) = &rm {
                    q.keywords.retain(|o| !o.eq_ignore_ascii_case(r));
                }
                if write_params(t, &q).is_ok() {
                    self.meta.insert(t.clone(), Meta::of(&q));
                    // The open photo's settings in memory must not write the old keywords back.
                    if self.current.as_deref() == Some(t.as_path()) {
                        self.params.keywords = q.keywords.clone();
                        for h in &mut self.history {
                            h.keywords = q.keywords.clone();
                        }
                    }
                }
                self.reindex(t);
            }
        }
    }

    fn library_info(&mut self, ui: &mut Ui) {
        let Some(f) = self.lib_sel.clone() else {
            ui.label(RichText::new("Select a frame").color(TEXT3));
            return;
        };
        let w = ui.available_width();
        match self.thumb(&f) {
            Some(t) => {
                ui.add(egui::Image::new(t).uv(cover_uv(t, 1.5)).fit_to_exact_size(vec2(w, w / 1.5)).maintain_aspect_ratio(false).corner_radius(6));
            }
            None => {
                let (r, _) = ui.allocate_exact_size(vec2(w, w / 1.5), Sense::hover());
                ui.painter().rect_filled(r, 6.0, CARD);
            }
        }
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(f.file_name().unwrap_or_default().to_string_lossy()).family(semi()).size(14.0)).truncate());
        ui.add_space(8.0);
        let real = load::copy_of(&f).unwrap_or(f.clone());
        let meta = std::fs::metadata(&real).ok();
        let mm = self.meta.get(&f).copied();
        let rows = [
            ("Type", if load::is_raw(&real) { format!("{} raw", ext_of(&real)) } else { ext_of(&real) }),
            ("File size", meta.as_ref().map(|m| if m.len() < 1_000_000 { format!("{:.0} KB", (m.len() as f64 / 1e3).ceil()) } else { format!("{:.1} MB", m.len() as f64 / 1e6) }).unwrap_or_default()),
            ("Status", match mm { Some(m) if m.neg => "Film scan".into(), Some(m) if m.edited => "Edited".into(), _ => if real != f { "Virtual copy".into() } else { "Unedited".into() } }),
        ];
        for (k, v) in rows {
            ui.horizontal(|ui| {
                ui.label(RichText::new(k).size(12.0).color(TEXT3));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new(v).size(12.0)));
            });
        }
        // Rating and flag, also on the keyboard: 0–5, P pick, X reject, U clear.
        let m = self.meta.get(&f).copied().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Rating").size(12.0).color(TEXT3));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for (flag, name) in [(-1i8, "Reject"), (1, "Pick")] {
                    if chip(ui, name, m.flag == flag).on_hover_text(if flag > 0 { "P" } else { "X" }).clicked() {
                        self.rate(&f, None, Some(if m.flag == flag { 0 } else { flag }), None);
                    }
                }
                // 24 px per star: the minimum target size. Empty stars use the control border colour (3:1).
                let (r, resp) = ui.allocate_exact_size(vec2(120.0, 24.0), Sense::click());
                let hover = resp.hover_pos().map(|p| (((p.x - r.left()) / 24.0) as u8 + 1).min(5));
                for i in 0..5u8 {
                    let c = pos2(r.left() + 12.0 + i as f32 * 24.0, r.center().y);
                    let lit = i < hover.unwrap_or(m.rating);
                    paint_stars(ui.painter(), c, 1, 6.5, if lit { if hover.is_some() { TEXT2 } else { ACCENT } } else { BTN_BORDER });
                }
                if resp.clicked()
                    && let Some(p) = resp.interact_pointer_pos()
                {
                    let n = (((p.x - r.left()) / 24.0) as u8 + 1).min(5);
                    self.rate(&f, Some(if n == m.rating { 0 } else { n }), None, None);
                }
            });
        });
        ui.horizontal(|ui| {
            // The colour's name too, so the label doesn't rely on colour alone.
            let name = m.label.checked_sub(1).and_then(|k| ["Red", "Yellow", "Green", "Blue", "Purple"].get(k as usize)).map_or(String::new(), |n| format!(" · {n}"));
            ui.label(RichText::new(format!("Label{name}")).size(12.0).color(TEXT3));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for k in (0..5u8).rev() {
                    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                    let on = m.label == k + 1;
                    ui.painter().circle_filled(r.center(), if on { 6.5 } else { 5.5 }, LABEL_COLORS[k as usize].gamma_multiply(if on { 1.0 } else { 0.8 }));
                    if on {
                        ui.painter().circle_stroke(r.center(), 7.5, Stroke::new(1.0, TEXT));
                    }
                    if resp.on_hover_text(["Red", "Yellow", "Green", "Blue", "Purple"][k as usize]).clicked() {
                        self.rate(&f, None, None, Some(k + 1));
                    }
                }
            });
        });
        self.keywords_panel(ui, &f);
        ui.add_space(12.0);
        ui.columns(2, |c| {
            if c[0].add(egui::Button::new("Open as negative").min_size(vec2(c[0].available_width(), 32.0))).clicked() {
                let mut q = read_params(&f).unwrap_or_default();
                q.neg.enabled = true;
                if write_params(&f, &q).is_ok() {
                    self.meta.insert(f.clone(), Meta::of(&q));
                }
                self.module = Module::Negative;
                self.select(c[0].ctx(), f.clone());
            }
            c[1].scope(|ui| {
                ui.spacing_mut().interact_size.x = ui.available_width();
                if ui.add(egui::Button::new(RichText::new("Open in Develop").color(INK).family(semi())).fill(BONE).stroke(Stroke::NONE).min_size(vec2(ui.available_width(), 32.0))).clicked() {
                    self.module = Module::Develop;
                    self.select(ui.ctx(), f.clone());
                }
            });
        });
    }

    /// Roll notes for the open folder, and the selected frame's own notes. Saved as they are typed.
    fn roll_panel(&mut self, ui: &mut Ui) {
        if self.folder.is_none() {
            return;
        }
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.label(caps("Roll notes"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new("for this folder").size(11.0).color(TEXT3));
            });
        });
        ui.add_space(4.0);
        self.roll_fields(ui);
    }

    /// The roll notes of the open folder, read when the folder changes.
    pub(super) fn roll_loaded(&mut self) -> Option<&crate::roll::Roll> {
        let dir = self.folder.clone()?;
        if self.roll_dir.as_ref() != Some(&dir) {
            self.roll = crate::roll::read(&dir).unwrap_or_default();
            self.roll_dir = Some(dir);
        }
        Some(&self.roll)
    }

    /// The roll notes' fields, without a heading.
    pub(super) fn roll_fields(&mut self, ui: &mut Ui) {
        let Some(dir) = self.folder.clone() else { return };
        self.roll_loaded();
        let mut changed = false;
        let r = &mut self.roll;
        let row = |ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui) -> egui::Response| -> bool {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(vec2(78.0, 22.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.set_min_width(78.0);
                    ui.label(RichText::new(label).size(12.0).color(TEXT3));
                });
                add(ui).changed()
            }).inner
        };
        fn text<'a>(v: &'a mut String, hint: &'a str) -> impl FnMut(&mut Ui) -> egui::Response + 'a {
            move |ui: &mut Ui| ui.add(egui::TextEdit::singleline(v).hint_text(hint).desired_width(f32::INFINITY))
        }
        changed |= row(ui, "Roll", &mut text(&mut r.roll, "12"));
        changed |= row(ui, "Camera", &mut text(&mut r.camera, "Nikon FM2"));
        changed |= row(ui, "Lens", &mut text(&mut r.lens, "50mm f/1.8"));
        // Film: one of the known stocks (so the conversion can use it), or any name.
        let known = negative::STOCKS.iter().find(|s| s.id == r.stock);
        let shown = known.map_or_else(|| if r.stock.is_empty() { "Not noted".to_string() } else { r.stock.clone() }, |s| format!("{} {}", s.brand, s.name));
        changed |= row(ui, "Film", &mut |ui: &mut Ui| {
            let mut resp = egui::ComboBox::from_id_salt("roll_stock").selected_text(shown.clone()).width(ui.available_width()).height(320.0).show_ui(ui, |ui| {
                let mut c = false;
                if picked(ui, r.stock.is_empty(), "Not noted").clicked() {
                    r.stock.clear();
                    c = true;
                }
                for s in STOCKS {
                    if picked(ui, r.stock == s.id, format!("{} {}", s.brand, s.name)).clicked() {
                        r.stock = s.id.to_string();
                        // Fill in the box speed from the name when it has one ("Portra 400").
                        if let Some(iso) = s.name.split_whitespace().filter_map(|w| w.trim_end_matches(['D', 'T']).parse::<f32>().ok()).last() {
                            r.iso = iso;
                        }
                        c = true;
                    }
                }
                c
            });
            if resp.inner == Some(true) {
                resp.response.mark_changed();
            }
            resp.response
        });
        if known.is_none() {
            changed |= row(ui, "", &mut text(&mut r.stock, "or type a film name"));
        }
        changed |= row(ui, "Box speed", &mut |ui: &mut Ui| ui.add(egui::DragValue::new(&mut r.iso).range(0.0..=25600.0).speed(5.0).suffix(" ISO")));
        changed |= row(ui, "Shot at", &mut |ui: &mut Ui| {
            let resp = ui.add(egui::DragValue::new(&mut r.ei).range(0.0..=25600.0).speed(5.0).custom_formatter(|v, _| if v <= 0.0 { "box speed".into() } else { format!("{v:.0} EI") }));
            let p = r.push_label();
            if !p.is_empty() {
                ui.label(RichText::new(p).size(11.5).color(TEXT2));
            }
            resp
        });
        changed |= row(ui, "Developer", &mut text(&mut r.developer, "D-76 1+1, 9:45"));
        changed |= row(ui, "Date", &mut text(&mut r.date, "YYYY-MM-DD"));
        ui.add_space(2.0);
        changed |= ui.add(egui::TextEdit::multiline(&mut r.notes).hint_text("Notes for the roll").desired_rows(2).desired_width(f32::INFINITY)).changed();
        // This frame's settings, when one is selected.
        if let Some(f) = self.lib_sel.clone() {
            let key = crate::roll::frame_key(&f);
            let mut fr = r.frame(&f);
            ui.add_space(10.0);
            ui.label(caps("This frame"));
            let mut fc = false;
            fc |= row(ui, "Aperture", &mut |ui: &mut Ui| ui.add(egui::DragValue::new(&mut fr.aperture).range(0.0..=64.0).speed(0.1).custom_formatter(|v, _| if v <= 0.0 { "—".into() } else { format!("f/{v:.1}") })));
            fc |= row(ui, "Shutter", &mut text(&mut fr.shutter, "1/125"));
            fc |= row(ui, "Focal length", &mut |ui: &mut Ui| ui.add(egui::DragValue::new(&mut fr.focal).range(0.0..=2000.0).speed(0.5).custom_formatter(|v, _| if v <= 0.0 { "roll's lens".into() } else { format!("{v:.0} mm") })));
            fc |= row(ui, "Note", &mut text(&mut fr.note, ""));
            if fc {
                if fr.is_empty() {
                    r.frames.remove(&key);
                } else {
                    r.frames.insert(key, fr);
                }
                changed = true;
            }
        }
        if changed {
            if let Err(e) = crate::roll::write(&dir, &self.roll) {
                self.status = format!("Could not save the roll notes: {e}");
            }
        }
        // The film and its push set up every frame's conversion in one go.
        let usable = negative::STOCKS.iter().any(|s| s.id == self.roll.stock) || self.my_stocks.iter().any(|(_, c)| c.name == self.roll.stock.trim());
        if usable {
            ui.add_space(8.0);
            let push = self.roll.push_label();
            let label = if push.is_empty() { "Convert the roll with this film".to_string() } else { format!("Convert the roll with this film ({push})") };
            if ui.add(egui::Button::new(label).min_size(vec2(ui.available_width(), 28.0)))
                .on_hover_text("Every frame in this folder is converted as this film. A push adds contrast and a pull takes it away").clicked()
            {
                let mut n = 0;
                for f in self.files.clone() {
                    let mut q = read_params(&f).unwrap_or_default();
                    let before = q.neg.clone();
                    // Re-applying shouldn't stack the push contrast: start from the stock's own contrast.
                    if q.neg.enabled && q.neg.stock == self.roll.stock {
                        q.neg.contrast = 0.0;
                    }
                    crate::roll::apply_stock(&self.roll, &mut q);
                    if q.neg != before && write_params(&f, &q).is_ok() {
                        self.meta.insert(f.clone(), Meta::of(&q));
                        n += 1;
                    }
                }
                if let Some(cur) = self.current.clone()
                    && cur.parent() == Some(dir.as_path())
                    && let Some(q) = read_params(&cur)
                {
                    self.params.neg = q.neg;
                    self.commit();
                }
                self.status = format!("{n} frame{} set to {}", if n == 1 { "" } else { "s" }, self.roll.stock_name());
            }
        }
    }

    // ---------- Develop ----------

    fn develop_left(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(caps("Presets"));
        });
        let mut apply = None;
        for (i, (name, _)) in self.presets.iter().enumerate() {
            let on = self.last_preset == Some(i);
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
            if on || resp.hovered() {
                ui.painter().rect_filled(r, 3.0, if on { BONE } else { CARD });
            }
            let th = Rect::from_min_size(r.min + vec2(4.0, 4.0), vec2(40.0, 28.0));
            match self.preset_tex.get(i) {
                Some(t) => egui::Image::new(t).uv(cover_uv(t, 40.0 / 28.0)).corner_radius(4).paint_at(ui, th),
                None => {
                    ui.painter().rect_filled(th, 4.0, CARD);
                }
            }
            ui.painter().text(pos2(th.right() + 10.0, r.center().y), Align2::LEFT_CENTER, name, FontId::proportional(13.0), if on { INK } else { TEXT });
            if resp.clicked() {
                apply = Some(i);
            }
        }
        // Film stocks you saved in Negative live in the same list: picking one converts this frame with it.
        let mut film = None;
        if !self.my_stocks.is_empty() {
            ui.add_space(6.0);
            ui.label(caps("My film"));
            for (k, (_, c)) in self.my_stocks.iter().enumerate() {
                let on = self.params.neg.enabled && c.apply(&self.params.neg) == self.params.neg;
                if ui.add(egui::Button::new(RichText::new(&c.name).size(13.0).color(if on { INK } else { TEXT })).fill(if on { BONE } else { Color32::TRANSPARENT }).stroke(Stroke::NONE).min_size(vec2(ui.available_width(), 30.0))).clicked() {
                    film = Some(k);
                }
            }
        }
        if let Some(k) = film {
            self.params.neg = self.my_stocks[k].1.apply(&self.params.neg);
            self.status = format!("Converting with {}", self.my_stocks[k].1.name);
        }
        if let Some(i) = apply {
            // Switching between presets starts from the same original, unless it was edited since.
            let base = match (&self.preset_mix, self.last_preset) {
                (Some((b, full)), Some(_)) if Params::blend(b, full, self.preset_amt / 100.0) == self.params => b.clone(),
                _ => self.params.clone(),
            };
            let full = base.with_look_of(&Params { border: base.border.clone(), ..self.presets[i].1.clone() });
            self.preset_amt = 100.0;
            self.params = full.clone();
            self.preset_mix = Some((base, full));
            self.last_preset = Some(i);
        }
        if self.last_preset.is_some()
            && let Some((base, full)) = &self.preset_mix
        {
            let before = self.preset_amt;
            ui.add_space(4.0);
            slider(ui, "Amount", &mut self.preset_amt, 0.0, 200.0, 100.0, None);
            if self.preset_amt != before {
                self.params = Params::blend(base, full, self.preset_amt / 100.0);
            }
        }
        let mut save = None;
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.preset_name).hint_text("New preset").desired_width(ui.available_width() - 56.0).margin(vec2(8.0, 5.0)));
            if ui.add_enabled(!self.preset_name.trim().is_empty(), egui::Button::new("Save")).clicked() {
                save = Some(self.preset_name.clone());
            }
        });
        if let Some(name) = save {
            let p = self.params.clone();
            self.status = match self.add_preset(&name, p) {
                Ok(n) => format!("Saved stock {n}"),
                Err(e) => format!("Could not save stock: {e}"),
            };
            self.preset_name.clear();
        }
        if ui.add(egui::Button::new(RichText::new("Import Lightroom presets…").size(12.0).color(TEXT2)).fill(Color32::TRANSPARENT).stroke(Stroke::NONE)).clicked()
            && let Some(files) = rfd::FileDialog::new().add_filter("Lightroom preset", &["xmp"]).pick_files()
        {
            let (mut n, mut failed) = (0, 0);
            for f in files {
                match import_xmp_preset(&f).map(|(name, p)| self.add_preset(&name, p)) {
                    Some(Ok(_)) => n += 1,
                    _ => failed += 1,
                }
            }
            self.status = if failed == 0 { format!("Imported {n} stocks") } else { format!("Imported {n} stocks, {failed} could not be read or saved") };
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label(caps("Snapshots"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(self.current.is_some(), egui::Button::new(RichText::new("+ New").size(11.0).color(TEXT2)).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, BTN_BORDER)).corner_radius(3).min_size(vec2(0.0, 22.0)))
                    .on_hover_text(format!("Save these settings to come back to ({})", keys::display(self.keymap.key("snapshot")))).clicked()
                {
                    self.add_snapshot();
                }
            });
        });
        let (mut restore, mut delete) = (None, None);
        for (i, sn) in self.params.snapshots.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(r, 5.0, hex(0x222226));
            }
            let current = Params { snapshots: vec![], ..self.params.clone() } == sn.params;
            ui.painter().text(pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER, &sn.name, FontId::proportional(12.0), if current { TEXT } else { TEXT2 });
            if current {
                ui.painter().circle_filled(pos2(r.right() - 10.0, r.center().y), 3.0, ACCENT);
            }
            let resp = resp.on_hover_text("Click to go back to it. Right-click to delete");
            if resp.clicked() {
                restore = Some(i);
            }
            if resp.secondary_clicked() {
                delete = Some(i);
            }
        }
        if let Some(i) = restore {
            let snaps = std::mem::take(&mut self.params.snapshots);
            let snap = snaps[i].params.clone();
            self.params = Params { snapshots: snaps, ..snap };
        }
        if let Some(i) = delete {
            self.params.snapshots.remove(i);
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label(caps("History"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if ui.add_enabled(self.hist_pos + 1 < self.history.len(), egui::Button::new(RichText::new("Redo").size(11.0)).fill(Color32::TRANSPARENT).stroke(Stroke::NONE)).clicked() {
                    self.undo(1);
                }
                if ui.add_enabled(self.hist_pos > 0, egui::Button::new(RichText::new("Undo").size(11.0)).fill(Color32::TRANSPARENT).stroke(Stroke::NONE)).clicked() {
                    self.undo(-1);
                }
            });
        });
        let mut jump = None;
        egui::ScrollArea::vertical().id_salt("hist").auto_shrink([false, false]).show(ui, |ui| {
            for i in (0..self.history.len()).rev() {
                let (label, val) = if i == 0 { ("Opened".to_string(), String::new()) } else { describe_change(&self.history[i - 1], &self.history[i]) };
                let fg = if i == self.hist_pos { INK } else if i > self.hist_pos { TEXT3 } else { TEXT2 };
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                if i == self.hist_pos || resp.hovered() {
                    ui.painter().rect_filled(r, 3.0, if i == self.hist_pos { BONE } else { CARD });
                }
                ui.painter().text(pos2(r.left() + 6.0, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(12.0), fg);
                ui.painter().text(pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, val, FontId::monospace(11.0), fg);
                if resp.clicked() {
                    jump = Some(i);
                }
            }
        });
        if let Some(i) = jump {
            self.goto_history(i);
        }
    }

    /// The tone curve in a large window you can move and resize.
    fn curve_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("Tone curve").open(&mut open).default_size([460.0, 470.0]).resizable(true).collapsible(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                segmented(ui, &mut self.curve_chan, &["RGB", "Red", "Green", "Blue"]);
                ui.add_space(12.0);
                ui.label(RichText::new("Brightness only").size(12.0).color(TEXT2));
                switch(ui, &mut self.params.curve_lum);
            });
            ui.add_space(6.0);
            let w = ui.available_width().max(240.0);
            let h = (ui.available_height() - 24.0).max(200.0);
            curve_editor(ui, &mut self.params, self.curve_chan, &mut self.curve_grab, Some(vec2(w, h)), &self.bins);
            ui.label(RichText::new("Click to add a point, drag to move it, double-click to remove it.").size(11.5).color(TEXT2));
        });
        if !open {
            self.curve_big = false;
        }
    }

    fn develop_right(&mut self, ui: &mut Ui) {
        if self.tool == Tool::Chart {
            self.chart_panel(ui);
            return;
        }
        let d = Params::default();
        let photo = &self.photo;
        let mixer_tab = &mut self.mixer_tab;
        let [o_basic, o_curve, o_mixer, o_grade, o_detail, o_lens, o_geo, o_fx, o_cal, o_border, o_spot, o_masks, _o_brush, o_lut] = &mut self.open;
        let (curve_chan, curve_grab, curve_big) = (&mut self.curve_chan, &mut self.curve_grab, &mut self.curve_big);
        let bins = self.bins;
        let mask_ui = &mut self.mask_ui;
        let mut mask_tool_on = false;
        let profiles = &self.profiles;
        let mut chart_on = false;
        let mut pick_profile: Option<Option<crate::calib::ColorProfile>> = None;
        let p = &mut self.params;
        let (spot_size, crop_aspect) = (&mut self.spot_size, &mut self.crop_aspect);
        let (heal_clone, spot_sel) = (&mut self.heal_clone, &mut self.spot_sel);
        let mut set_aspect = None;
        let frame_aspect = photo.as_ref().map(|ph| {
            let (w, h) = develop::oriented(ph.preview.width(), ph.preview.height(), p);
            w as f32 / h as f32
        }).unwrap_or(1.5);
        let (mut pick_wb, mut dust, mut install_ai) = (false, false, false);
        let (mut lens_auto, mut lens_pick) = (false, None::<String>);
        let lens_query = &mut self.lens_query;
        let ai_busy = self.ai_busy;
        let picking_wb = self.picking_wb;

        // Gradients sampled from the same maths the pipeline uses.
        // Neutral ramp for ±100 tone sliders, even in perceived brightness (a, b are linear light).
        let grey_ramp = |a: f32, b: f32| move |t: f32| { let (ea, eb) = (linear_to_srgb(a), linear_to_srgb(b)); enc([ea + (eb - ea) * (t + 100.0) / 200.0; 3]) };
        let temp_g = |k: f32| neutral_through(wb_gains(k, 0.0));
        let tint_g = |t: f32| neutral_through(wb_gains(5500.0, t));
        let expo_g = |ev: f32| lin([(0.18 * 2f32.powf(ev)).min(1.0); 3]);
        let tone_g = grey_ramp(0.01, 0.8);
        let sat_ref = |k: f32| {
            // Sample: a mid orange-red, scaled like the Saturation slider does in linear light.
            let c = [0.55f32, 0.16, 0.06];
            let y = luma(c);
            lin(c.map(|v| (y + (v - y) * k).max(0.0)))
        };
        let sat_g = |t: f32| sat_ref(1.0 + t / 100.0);
        let vib_g = |t: f32| sat_ref(1.0 + t / 100.0 * 0.7);
        let dehaze_g = |t: f32| { let h = (t / 100.0 * 0.5).clamp(-0.5, 0.5); lin([0.12, 0.18, 0.28].map(|v: f32| if h > 0.0 { v * (1.0 + h) } else { v + (-h) * (0.6 - v) })) };
        let rainbow = |h: f32| hsv(h, 0.75, 0.9);

        // While the Mask tool is on, the panel is just the masks, as in Lightroom.
        if self.tool == Tool::Mask {
            let mut done = false;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Masks").size(14.0).family(semi()).color(TEXT));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| done = ui.button("Done").clicked());
                    });
                    ui.add_space(6.0);
                    masks::masks_panel(ui, mask_ui, p);
                });
            });
            if done {
                self.tool = Tool::None;
            }
            return;
        }
        // Which sections hold edits (compared as saved, so every field type counts the same way). The lists that can
        // grow large (masks with thousands of brush dabs, snapshots holding whole settings) are set aside while the
        // rest is serialised, since this runs every frame; spots and masks are compared by count instead.
        let (spots, masks) = (std::mem::take(&mut p.spots), std::mem::take(&mut p.masks));
        let (snaps, brushes) = (std::mem::take(&mut p.snapshots), std::mem::take(&mut p.brushes));
        let pj = serde_json::to_value(&*p).unwrap_or_default();
        (p.spots, p.masks, p.snapshots, p.brushes) = (spots, masks, snaps, brushes);
        let dj = serde_json::to_value(&d).unwrap_or_default();
        let (has_spots, has_masks) = (!p.spots.is_empty(), !p.masks.is_empty() || !p.brushes.is_empty() || !p.grads.is_empty());
        let changed = |keys: &[&str]| keys.iter().any(|k| match *k { "spots" => has_spots, "masks" => has_masks, _ => pj.get(k) != dj.get(k) });
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            section(ui, "Basic", o_basic, changed(&["temp","tint","exposure","contrast","highlights","shadows","whites","blacks","texture","clarity","dehaze","vibrance","saturation"]), |ui| {
                // The eyedropper sits with "As shot" in the group's header, as Lightroom's does: an action, not a field.
                ui.horizontal(|ui| {
                    ui.label(caps("White balance"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if small_action(ui, "As shot", false).clicked() {
                            (p.temp, p.tint) = (d.temp, d.tint);
                        }
                        pick_wb = small_action(ui, if picking_wb { "Cancel" } else { "Pick neutral" }, picking_wb)
                            .on_hover_text("Eyedropper (W). Click something that should be grey or white").clicked();
                    });
                });
                slider(ui, "Temp", &mut p.temp, 2000.0, 12000.0, d.temp, Some(&temp_g));
                slider(ui, "Tint", &mut p.tint, -150.0, 150.0, d.tint, Some(&tint_g));
                if group(ui, "Tone", Some("Auto"))
                    && let Some(ph) = photo
                {
                    *p = develop::auto_tone(&ph.preview, p); // only on click: it costs a full analysis
                }
                slider(ui, "Exposure", &mut p.exposure, -5.0, 5.0, 0.0, Some(&expo_g));
                slider(ui, "Contrast", &mut p.contrast, -100.0, 100.0, 0.0, None);
                slider(ui, "Highlights", &mut p.highlights, -100.0, 100.0, 0.0, Some(&grey_ramp(0.25, 0.95)));
                slider(ui, "Shadows", &mut p.shadows, -100.0, 100.0, 0.0, Some(&grey_ramp(0.005, 0.25)));
                slider(ui, "Whites", &mut p.whites, -100.0, 100.0, 0.0, Some(&grey_ramp(0.4, 1.0)));
                slider(ui, "Blacks", &mut p.blacks, -100.0, 100.0, 0.0, Some(&tone_g.clone()));
                group(ui, "Presence", None);
                slider(ui, "Texture", &mut p.texture, -100.0, 100.0, 0.0, None);
                slider(ui, "Clarity", &mut p.clarity, -100.0, 100.0, 0.0, None);
                slider(ui, "Dehaze", &mut p.dehaze, -100.0, 100.0, 0.0, Some(&dehaze_g));
                slider(ui, "Vibrance", &mut p.vibrance, -100.0, 100.0, 0.0, Some(&vib_g));
                slider(ui, "Saturation", &mut p.saturation, -100.0, 100.0, 0.0, Some(&sat_g));
            });
            section(ui, "Tone curve", o_curve, changed(&["c_hi","c_li","c_da","c_sh","curves","curve_lum"]), |ui| {
                ui.horizontal(|ui| {
                    segmented(ui, curve_chan, &["RGB", "Red", "Green", "Blue"]);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ghost(ui, "⛶", *curve_big).on_hover_text("Open the curve in a large window").clicked() {
                            *curve_big = !*curve_big;
                        }
                    });
                });
                if !*curve_big {
                    curve_editor(ui, p, *curve_chan, curve_grab, None, &bins);
                } else {
                    ui.label(RichText::new("The curve is open in its own window.").size(11.5).color(TEXT2));
                }
                switch_row(ui, "Brightness only", &mut p.curve_lum).on_hover_text("In luminosity mode the RGB curve changes brightness without shifting colours or saturation");
                ui.label(RichText::new("Click to add a point, drag to move it, double-click to remove it.").size(11.5).color(TEXT2));
                group(ui, "Regions", None);
                slider(ui, "Highlights", &mut p.c_hi, -100.0, 100.0, 0.0, None);
                slider(ui, "Lights", &mut p.c_li, -100.0, 100.0, 0.0, None);
                slider(ui, "Darks", &mut p.c_da, -100.0, 100.0, 0.0, None);
                slider(ui, "Shadows", &mut p.c_sh, -100.0, 100.0, 0.0, None);
            });
            section(ui, "Color mixer", o_mixer, changed(&["hsl"]), |ui| {
                ui.horizontal(|ui| segmented(ui, mixer_tab, &["Hue", "Saturation", "Luminance"]));
                for (b, name) in ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"].into_iter().enumerate() {
                    let h = HUE_CENTERS[b];
                    // Same mappings as the mixer in develop.rs: ±100 hue = ±30°, saturation ×(1±1), luminance ×(1±0.4·s).
                    let g: Box<dyn Fn(f32) -> Color32> = match *mixer_tab {
                        0 => Box::new(move |t| hsv(h + t * 0.3, 0.8, 0.92)),
                        1 => Box::new(move |t| hsv(h, 0.6 * (1.0 + t / 100.0), 0.85)),
                        _ => Box::new(move |t| hsv(h, 0.7, 0.62 * (1.0 + t / 100.0 * 0.4 * 0.7) + t.max(0.0) / 100.0 * 0.2)),
                    };
                    slider_ex(ui, name, Some(hsv(h, 0.7, 0.9)), &mut p.hsl[b][*mixer_tab], -100.0, 100.0, 0.0, Some(&*g));
                }
            });
            section(ui, "Color grading", o_grade, changed(&["grade","g_blend","g_bal"]), |ui| {
                grading_wheels(ui, p);
                for (z, name) in ["Shadows", "Midtones", "Highlights", "Global"].into_iter().enumerate() {
                    group(ui, name, None);
                    let h = p.grade[z][0];
                    slider_ex(ui, "Hue", Some(hsv(h, (p.grade[z][1] / 100.0).max(0.15), 0.9)), &mut p.grade[z][0], 0.0, 360.0, 0.0, Some(&rainbow));
                    slider(ui, "Saturation", &mut p.grade[z][1], 0.0, 100.0, 0.0, Some(&move |t: f32| hsv(h, t / 100.0, 0.85)));
                    slider(ui, "Luminance", &mut p.grade[z][2], -100.0, 100.0, 0.0, Some(&grey_ramp(0.03, 0.75)));
                }
                group(ui, "Blend", None);
                slider(ui, "Blending", &mut p.g_blend, 0.0, 100.0, d.g_blend, None);
                slider(ui, "Balance", &mut p.g_bal, -100.0, 100.0, 0.0, None);
            });
            section(ui, "Detail", o_detail, changed(&["sh_amt","sh_rad","sh_det","sh_mask","nr_lum","nr_det","nr_col","ai_nr"]), |ui| {
                group(ui, "Sharpening", None);
                slider(ui, "Amount", &mut p.sh_amt, 0.0, 150.0, d.sh_amt, None);
                slider(ui, "Radius", &mut p.sh_rad, 0.5, 3.0, d.sh_rad, None);
                slider(ui, "Detail", &mut p.sh_det, 0.0, 100.0, d.sh_det, None);
                slider(ui, "Masking", &mut p.sh_mask, 0.0, 100.0, d.sh_mask, None);
                group(ui, "Noise reduction", None);
                slider(ui, "Luminance", &mut p.nr_lum, 0.0, 100.0, 0.0, None);
                slider(ui, "Detail", &mut p.nr_det, 0.0, 100.0, d.nr_det, None);
                slider(ui, "Color", &mut p.nr_col, 0.0, 100.0, d.nr_col, None);
                group(ui, "AI denoise", None);
                if denoise::installed() {
                    slider(ui, "Strength", &mut p.ai_nr, 0.0, 100.0, 0.0, None);
                    ui.label(RichText::new(if ai_busy { "Working… The first run on a photo takes a while, and the result is kept for next time." } else { "Removes noise with a neural network (RawNIND for RAW files, SCUNet for the rest). The sliders above fine-tune the result." }).size(11.5).color(TEXT2));
                } else {
                    install_ai = ui.add_enabled(!ai_busy, egui::Button::new(if ai_busy { "Downloading…" } else { "Download AI denoise (about 140 MB)" }).min_size(vec2(ui.available_width(), 28.0))).clicked();
                    ui.label(RichText::new("Optional. Downloads ONNX Runtime and the RawNIND and SCUNet models once, so ferrotype itself stays small.").size(11.5).color(TEXT2));
                }
            });
            section(ui, "Lens corrections", o_lens, changed(&["o_dist","o_vig","o_fringe","lens","lens_dist","lens_vig"]), |ui| {
                group(ui, "Profile", None);
                if p.lens.is_empty() {
                    lens_auto = ui.add(egui::Button::new("Enable profile corrections").min_size(vec2(ui.available_width(), 28.0)))
                        .on_hover_text("Find this lens in the lensfun database (about 1,500 lenses) from the photo's EXIF").clicked();
                } else {
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(RichText::new(&p.lens).size(12.0).color(TEXT)).truncate());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Off").clicked() {
                                p.lens.clear();
                            }
                        });
                    });
                    if p.lens_focal > 0.0 {
                        ui.label(RichText::new(format!("{:.0} mm{}", p.lens_focal, if p.lens_f > 0.0 { format!(" · f/{:.1}", p.lens_f) } else { String::new() })).size(11.5).color(TEXT2));
                    } else {
                        slider(ui, "Focal length", &mut p.lens_focal, 8.0, 300.0, 0.0, None);
                    }
                    slider(ui, "Distortion", &mut p.lens_dist, 0.0, 200.0, 100.0, None);
                    slider(ui, "Vignetting", &mut p.lens_vig, 0.0, 200.0, 100.0, None);
                }
                ui.add(egui::TextEdit::singleline(lens_query).hint_text("Search lenses…").desired_width(f32::INFINITY));
                let q = lens_query.to_lowercase();
                if q.len() >= 2 {
                    let words: Vec<&str> = q.split_whitespace().collect();
                    let mut shown = 0;
                    for l in &crate::lens::db().lenses {
                        let name = l.name();
                        let low = name.to_lowercase();
                        if words.iter().all(|w| low.contains(w)) {
                            if ui.selectable_label(p.lens == name, RichText::new(&name).size(12.0).color(if p.lens == name { INK } else { TEXT })).clicked() {
                                lens_pick = Some(name);
                            }
                            shown += 1;
                            if shown == 8 {
                                break;
                            }
                        }
                    }
                    if shown == 0 {
                        ui.label(RichText::new("No lens with that name").size(11.5).color(TEXT3));
                    }
                }
                group(ui, "Manual", None);
                slider(ui, "Distortion", &mut p.o_dist, -100.0, 100.0, 0.0, None);
                slider(ui, "Vignetting", &mut p.o_vig, -100.0, 100.0, 0.0, Some(&grey_ramp(0.05, 0.6)));
                slider(ui, "Defringe", &mut p.o_fringe, 0.0, 20.0, 0.0, None);
            });
            section(ui, "Transform & crop", o_geo, changed(&["t_vert","t_hor","t_rot","t_scale","flip_h","crop","rot90"]), |ui| {
                // Upright: found from the photo's straight lines, then written into the three sliders below, so
                // the result stays adjustable by hand.
                ui.horizontal(|ui| {
                    ui.label(caps("Upright"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if small_action(ui, "Off", false).on_hover_text("Rotate, Vertical and Horizontal back to 0, Scale to 100").clicked() {
                            (p.t_rot, p.t_vert, p.t_hor, p.t_scale) = (0.0, 0.0, 0.0, 100.0);
                        }
                        for (m, name) in crate::upright::Mode::ALL.iter().rev() {
                            let tip = match m {
                                crate::upright::Mode::Auto => "Level, plus a gentle fix for leaning verticals when the photo clearly has them",
                                crate::upright::Mode::Level => "Level the horizon from the photo's straight lines",
                                crate::upright::Mode::Vertical => "Level and make converging verticals parallel (buildings)",
                                crate::upright::Mode::Full => "Level and straighten verticals and horizontals",
                            };
                            if small_action(ui, name, false).on_hover_text(tip).clicked()
                                && let Some(ph) = photo
                            {
                                *p = crate::upright::apply(&ph.preview, p, *m);
                            }
                        }
                    });
                });
                slider(ui, "Vertical", &mut p.t_vert, -100.0, 100.0, 0.0, None);
                slider(ui, "Horizontal", &mut p.t_hor, -100.0, 100.0, 0.0, None);
                slider(ui, "Rotate", &mut p.t_rot, -45.0, 45.0, 0.0, None);
                slider(ui, "Scale", &mut p.t_scale, 50.0, 150.0, 100.0, None);
                ui.columns(2, |c| {
                    if c[0].add(egui::Button::new("Rotate left").min_size(vec2(c[0].available_width(), 26.0))).on_hover_text("[").clicked() {
                        p.rotate(false);
                    }
                    if c[1].add(egui::Button::new("Rotate right").min_size(vec2(c[1].available_width(), 26.0))).on_hover_text("]").clicked() {
                        p.rotate(true);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Flip horizontal").size(12.0).color(LABEL));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, &mut p.flip_h));
                });
                if group(ui, "Crop", Some("Reset")) {
                    p.crop = [0.0; 4];
                }
                let mut a = *crop_aspect;
                segmented(ui, &mut a, &ASPECTS.map(|x| x.0));
                if a != *crop_aspect {
                    *crop_aspect = a;
                    set_aspect = ASPECTS[a].1;
                }
                let c = p.crop;
                let (cw, ch) = (1.0 - c[0] - c[2], 1.0 - c[1] - c[3]);
                ui.label(RichText::new(format!("Ratio {:.2} · {:.0}% of the frame. Use Crop in the toolbar to drag the corners.", cw * frame_aspect / ch.max(1e-3), cw * ch * 100.0)).size(11.5).color(TEXT2));
            });
            section(ui, "Spot removal", o_spot, changed(&["spots"]), |ui| {
                let mut mode = *heal_clone as usize;
                segmented(ui, &mut mode, &["Heal", "Clone"]);
                *heal_clone = mode == 1;
                slider(ui, "Size", spot_size, 0.5, 10.0, 2.0, None);
                if let Some(sp) = spot_sel.and_then(|i| p.spots.get_mut(i)) {
                    group(ui, "Selected spot", None);
                    let mut m = sp.clone as usize;
                    segmented(ui, &mut m, &["Heal", "Clone"]);
                    sp.clone = m == 1;
                    slider(ui, "Source angle", &mut sp.angle, -180.0, 180.0, 0.0, None);
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} spots", p.spots.len())).size(12.0).color(LABEL));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add_enabled(!p.spots.is_empty(), egui::Button::new("Remove all")).clicked() {
                            p.spots.clear();
                        }
                        dust = ui.button("Find dust").on_hover_text("Marks small dark specks (dust on the scan or sensor) for healing").clicked();
                    });
                });
                ui.label(RichText::new("Choose Heal in the toolbar, then click a spot or paint over a larger area. Drag the thin source circle to take texture from somewhere else, and right-click a spot to remove it. Clone copies the source exactly, while Heal blends it in.").size(11.5).color(TEXT2));
            });
            section(ui, "Masks", o_masks, changed(&["masks"]), |ui| {
                mask_tool_on = masks::masks_panel(ui, mask_ui, p);
            });
            section(ui, "Effects", o_fx, changed(&["vig","vig_mid","vig_round","vig_fea","vig_hi","gr_amt","gr_size","gr_rough","gr_hi","hal_amt","hal_size","hal_thr"]), |ui| {
                group(ui, "Vignette", None);
                slider(ui, "Amount", &mut p.vig, -100.0, 100.0, 0.0, Some(&grey_ramp(0.0, 0.85)));
                slider(ui, "Midpoint", &mut p.vig_mid, 0.0, 100.0, d.vig_mid, None);
                slider(ui, "Roundness", &mut p.vig_round, -100.0, 100.0, 0.0, None);
                slider(ui, "Feather", &mut p.vig_fea, 0.0, 100.0, d.vig_fea, None);
                slider(ui, "Highlights", &mut p.vig_hi, 0.0, 100.0, 0.0, None);
                group(ui, "Halation", None);
                slider(ui, "Amount", &mut p.hal_amt, 0.0, 100.0, 0.0, Some(&|t: f32| lin([0.05 + t / 100.0 * 0.7, 0.03 + t / 100.0 * 0.15, 0.03])));
                slider(ui, "Size", &mut p.hal_size, 0.0, 100.0, d.hal_size, None);
                slider(ui, "Threshold", &mut p.hal_thr, 0.0, 100.0, d.hal_thr, Some(&grey_ramp(0.15, 1.0)));
                egui::ComboBox::from_id_salt("match_stock").width(ui.available_width()).selected_text("Match a film stock's grain and halation…").show_ui(ui, |ui| {
                    for st in STOCKS {
                        if picked(ui, false, format!("{} {}", st.brand, st.name)).clicked() {
                            (p.gr_amt, p.gr_size, p.hal_amt, p.gr_hi) = negative::character(st);
                        }
                    }
                });
                group(ui, "Grain", None);
                slider(ui, "Amount", &mut p.gr_amt, 0.0, 100.0, 0.0, None);
                slider(ui, "Size", &mut p.gr_size, 0.0, 100.0, d.gr_size, None);
                slider(ui, "Roughness", &mut p.gr_rough, 0.0, 100.0, d.gr_rough, None);
                slider(ui, "Highlight grain", &mut p.gr_hi, 0.0, 100.0, d.gr_hi, None);
            });
            section(ui, "Border", o_border, changed(&["border"]), |ui| {
                group(ui, "Color", None);
                let mut c = p.border.white as usize;
                segmented(ui, &mut c, &["Black", "White"]);
                p.border.white = c == 1;
                slider(ui, "Width", &mut p.border.width, 0.0, 20.0, 0.0, None);
                group(ui, "Canvas", None);
                let aspects = [0.0, 1.0, 0.8, 0.5625, 1.5];
                let mut a = aspects.iter().position(|v| (*v - p.border.aspect).abs() < 1e-3).unwrap_or(0);
                segmented(ui, &mut a, &["Photo", "1:1", "4:5", "9:16", "3:2"]);
                p.border.aspect = aspects[a];
                group(ui, "Caption", None);
                ui.add(egui::TextEdit::singleline(&mut p.border.caption).id(egui::Id::NULL.with("caption")).hint_text("Optional, for example Portra 400 · Lisbon").desired_width(f32::INFINITY).margin(vec2(8.0, 5.0)));
                ui.label(RichText::new("Width is a share of the long side. Canvas pads the frame out to a fixed shape, such as 4:5 for Instagram posts. A caption adds a band under the photo.").size(11.5).color(TEXT2));
            });
            section(ui, "Color look (LUT)", o_lut, changed(&["lut","lut_amt"]), |ui| {
                let name = std::path::Path::new(&p.lut).file_name().map(|n| n.to_string_lossy().to_string());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(name.as_deref().unwrap_or("None")).size(12.0).color(LABEL));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !p.lut.is_empty() && ui.button("Remove").clicked() {
                            p.lut.clear();
                        }
                        if ui.button("Load .cube…").clicked()
                            && let Some(f) = rfd::FileDialog::new().add_filter("3D LUT", &["cube"]).pick_file()
                        {
                            p.lut = f.to_string_lossy().into();
                        }
                    });
                });
                if !p.lut.is_empty() {
                    if develop::load_cube(&p.lut).is_none() {
                        ui.label(RichText::new("Could not read this file. Only 3D .cube LUTs with a 0–1 range are supported.").size(11.5).color(WARN));
                    }
                    slider(ui, "Amount", &mut p.lut_amt, 0.0, 100.0, 100.0, None);
                }
                ui.label(RichText::new("Applied after the tone and colour settings, on display-ready colours (Rec.709 / sRGB LUTs).").size(11.5).color(TEXT2));
            });
            section(ui, "Calibration", o_cal, changed(&["cal_sh","cal","profile"]), |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Profile").size(12.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if small_action(ui, "From chart…", false).on_hover_text("Build a camera or scanner profile from a photo of a ColorChecker or IT8 chart").clicked() {
                            chart_on = true;
                        }
                        let shown = p.profile.as_ref().map_or("Standard", |c| c.name.as_str());
                        egui::ComboBox::from_id_salt("cal_profile").width(ui.available_width() - 8.0).selected_text(shown).show_ui(ui, |ui| {
                            if picked(ui, p.profile.is_none(), "Standard").clicked() {
                                pick_profile = Some(None);
                            }
                            for c in profiles {
                                if picked(ui, p.profile.as_ref().is_some_and(|x| x.name == c.name), &c.name).clicked() {
                                    pick_profile = Some(Some(c.clone()));
                                }
                            }
                        });
                    });
                });
                if profiles.is_empty() && p.profile.is_none() {
                    ui.label(RichText::new("Profiles correct a camera's or scanner's colours. Build one from a chart shot under your light.").size(11.5).color(TEXT2));
                }
                ui.add_space(4.0);
                slider(ui, "Shadows tint", &mut p.cal_sh, -100.0, 100.0, 0.0, Some(&|t: f32| lin([0.12, 0.12 + t / 100.0 * 0.06, 0.12])));
                for (j, name) in ["Red", "Green", "Blue"].into_iter().enumerate() {
                    let mut e = [0.0f32; 3];
                    e[j] = 1.0;
                    // Same as calibration_matrix: hue rotates the primary around the grey axis by t·0.3°.
                    let hue_g = move |t: f32| { let c = hue_rotate(e, t * 0.3).map(|v| v.max(0.0)); let m = c[0].max(c[1]).max(c[2]).max(1e-3); lin(c.map(|v| v / m * 0.75)) };
                    let sat_g = move |t: f32| { let y = luma(e); lin(e.map(|v| (y + (v - y) * (1.0 + t / 100.0)).clamp(0.0, 1.0) * 0.75)) };
                    slider_ex(ui, &format!("{name} primary hue"), Some(lin(e.map(|v| v * 0.8))), &mut p.cal[j][0], -100.0, 100.0, 0.0, Some(&hue_g));
                    slider(ui, "Saturation", &mut p.cal[j][1], -100.0, 100.0, 0.0, Some(&sat_g));
                }
            });
        });
        if let Some(a) = set_aspect {
            self.set_crop_aspect(a);
        }
        if let Some(pr) = pick_profile {
            self.set_profile(pr);
        }
        if chart_on {
            self.tool = Tool::Chart;
            self.chart.fit = None;
            self.zoom = None;
            self.view = View::After;
        }
        if mask_tool_on && self.tool != Tool::Mask {
            self.tool = Tool::Mask;
            self.zoom = None;
            self.view = View::After;
        }
        if pick_wb {
            self.picking_wb = !self.picking_wb;
            self.tool = Tool::None;
            self.zoom = None;
            self.view = View::After;
        }
        if dust {
            self.find_dust();
        }
        if lens_auto {
            self.auto_lens();
        }
        if let Some(name) = lens_pick {
            self.set_lens(name);
            self.lens_query.clear();
        }
        if install_ai {
            self.install_ai(ui.ctx());
        }
    }

    /// Turn on the lens profile matching this photo's EXIF.
    fn auto_lens(&mut self) {
        let Some(cur) = self.current.clone() else { return };
        match crate::lens::detect(&cur) {
            Some((name, focal, fnum, crop)) => {
                self.status = format!("Lens profile: {name}");
                (self.params.lens, self.params.lens_focal, self.params.lens_f, self.params.lens_crop) = (name, focal, fnum, crop);
            }
            None => {
                self.status = match crate::lens::shot(&cur) {
                    Some(s) if !s.lens.is_empty() => format!("No profile for \"{}\". Search for the lens by name", s.lens),
                    _ => "This photo doesn't say which lens took it. Search for the lens by name".into(),
                };
            }
        }
    }

    /// Use a lens profile picked by name, with the focal length and aperture from EXIF when there are any.
    fn set_lens(&mut self, name: String) {
        let shot = self.current.as_ref().and_then(|c| crate::lens::shot(c)).unwrap_or_default();
        let own = crate::lens::db().lenses.iter().find(|l| l.name() == name).map(|l| l.crop);
        self.params.lens_crop = crate::lens::camera_crop(&shot).or(own).unwrap_or(1.0);
        (self.params.lens_focal, self.params.lens_f) = (shot.focal, shot.fnum);
        self.params.lens = name;
    }

    /// Merge the selected photos into a panorama in the background.
    fn merge_pano(&mut self, ctx: &egui::Context, files: Vec<PathBuf>, proj: crate::pano::Projection) {
        if self.pano_busy {
            return;
        }
        self.pano_busy = true;
        self.status = format!("Merging {} photos…", files.len());
        let (tx, ctx2) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let tx2 = tx.clone();
            let c3 = ctx2.clone();
            let r = crate::make_pano(&files, proj, &move |m| {
                let _ = tx2.send(Msg::Status(format!("Panorama: {m}…")));
                c3.request_repaint();
            });
            let _ = tx.send(Msg::Pano(r));
            ctx2.request_repaint();
        });
    }

    /// Merge exposure brackets into HDR photos: the given groups, or (None) every bracket found in the folder.
    fn merge_hdr(&mut self, ctx: &egui::Context, groups: Option<Vec<Vec<PathBuf>>>) {
        if self.pano_busy {
            return;
        }
        self.pano_busy = true;
        self.status = "Merging to HDR…".into();
        let all = self.files.clone();
        let (tx, ctx2) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let groups = groups.unwrap_or_else(|| crate::hdrmerge::brackets(&all));
            let mut outs = vec![];
            for (k, g) in groups.iter().enumerate() {
                let (tx2, c3, n) = (tx.clone(), ctx2.clone(), groups.len());
                let r = crate::hdrmerge::merge_files(g, &move |m| {
                    let _ = tx2.send(Msg::Status(format!("HDR {}/{n}: {m}…", k + 1)));
                    c3.request_repaint();
                });
                match r {
                    Ok(o) => outs.push(o),
                    Err(e) => {
                        let _ = tx.send(Msg::Hdr(Err(e)));
                        ctx2.request_repaint();
                        return;
                    }
                }
            }
            let _ = tx.send(Msg::Hdr(Ok(outs)));
            ctx2.request_repaint();
        });
    }

    fn install_ai(&mut self, ctx: &egui::Context) {
        self.ai_busy = true;
        let (tx, ctx2) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let tx2 = tx.clone();
            let r = denoise::install(&|f, what| {
                let _ = tx2.send(Msg::Status(format!("Downloading {what}… {:.0}%", f * 100.0)));
                ctx2.request_repaint();
            });
            let _ = tx.send(Msg::AiInstalled(r));
            ctx2.request_repaint();
        });
    }

    fn develop_footer(&mut self, ui: &mut Ui) {
        // Everything that moves settings between frames in one place, Reset last and quiet: it throws work away.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if ghost(ui, "Copy", false).on_hover_text("Copy these settings (Ctrl/Cmd+C)").clicked() {
                self.copied = Some(self.params.clone());
            }
            if ui.add_enabled(self.copied.is_some(), egui::Button::new(RichText::new("Paste").color(TEXT2)).fill(Color32::TRANSPARENT).stroke(Stroke::NONE))
                .on_hover_text("Paste the copied settings (Ctrl/Cmd+V). Crop and negative conversion are kept").clicked() {
                self.params = self.params.with_look_of(self.copied.as_ref().unwrap());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ghost(ui, "Reset", false).on_hover_text("Clear this photo's Develop settings (undo brings them back)").clicked() {
                    // The conversion and the profile it was measured with stay together.
                    self.params = Params { neg: self.params.neg.clone(), profile: self.params.profile.clone(), ..Params::default() };
                }
            });
        });
        ui.add_space(4.0);
        ui.columns(2, |c| {
            if c[0].add_enabled(self.prev_params.is_some(), egui::Button::new("Paste previous").min_size(vec2(c[0].available_width(), 32.0))).on_hover_text("Apply the settings of the frame you edited before this one").clicked() {
                let prev = self.prev_params.clone().unwrap();
                self.params = self.params.with_look_of(&prev);
            }
            if c[1].add(egui::Button::new("Sync settings…").min_size(vec2(c[1].available_width(), 32.0))).on_hover_text("Copy these settings (not crop or transform) to every photo in the folder").clicked() {
                let me = self.params.clone();
                self.sync_folder(move |other| other.with_look_of(&me));
            }
        });
    }

    // ---------- Negative ----------

    fn negative_left(&mut self, ui: &mut Ui) {
        egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 12, top: 16, bottom: 10 }).show(ui, |ui| {
            ui.label(caps("Film stock"));
            ui.add(egui::TextEdit::singleline(&mut self.stock_query).hint_text(format!("Search {} stocks", STOCKS.len())).desired_width(f32::INFINITY).margin(vec2(10.0, 7.0)));
            // One choice at a time, so a segmented control on one row rather than chips.
            ui.scope(|ui| {
                ui.spacing_mut().button_padding = vec2(8.0, 4.0);
                segmented(ui, &mut self.stock_tab, &["All", "Color", "B&W", "Cine", "Slide"]);
            });
        });
        let y = ui.cursor().top();
        ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, DIVIDER));
        let q = self.stock_query.to_lowercase();
        let tab = self.stock_tab;
        let keep = |s: &&negative::Stock| {
            let kind_ok = match tab { 1 => s.kind == Kind::Color, 2 => s.kind == Kind::Bw, 3 => s.kind == Kind::Cine, 4 => s.kind == Kind::Slide, _ => true };
            kind_ok && (q.is_empty() || format!("{} {}", s.brand, s.name).to_lowercase().contains(&q))
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin { left: 12, right: 12, top: 4, bottom: 16 }).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                // Your own profiles first.
                ui.add_space(8.0);
                ui.label(caps("My film"));
                let mut apply = None;
                let mut remove = None;
                let mut share = None;
                for (k, (_, c)) in self.my_stocks.iter().enumerate() {
                    if !q.is_empty() && !c.name.to_lowercase().contains(&q) {
                        continue;
                    }
                    let base = STOCKS.iter().find(|s| s.id == c.neg.stock);
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(r, 6.0, hex(0x222226));
                    }
                    ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, &c.name, FontId::proportional(13.0), TEXT);
                    let resp = resp.on_hover_text(format!("Based on {}. Right-click to share or delete.", base.map_or(String::new(), |b| format!("{} {}", b.brand, b.name))));
                    if resp.clicked() {
                        apply = Some(k);
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Share as file…").clicked() {
                            share = Some(k);
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            remove = Some(k);
                            ui.close();
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.my_stock_name).hint_text("Name").desired_width(ui.available_width() - 150.0));
                    let can = !self.my_stock_name.trim().is_empty() && self.params.neg.enabled;
                    if ui.add_enabled(can, egui::Button::new("Save")).on_hover_text("Save this photo's stock and adjustments as your own profile").on_disabled_hover_text("Convert a negative first, then name the profile").clicked() {
                        match negative::CustomStock::new(&self.my_stock_name, &self.params.neg).install() {
                            Ok(_) => {
                                self.status = format!("Saved stock profile {}", self.my_stock_name.trim());
                                self.my_stock_name.clear();
                                self.my_stocks = negative::custom_stocks();
                            }
                            Err(e) => self.status = format!("Could not save: {e}"),
                        }
                    }
                    if ui.button("Import…").on_hover_text("Add a .ferrostock file someone shared").clicked()
                        && let Some(f) = rfd::FileDialog::new().add_filter("Film stock profile", &[negative::CUSTOM_EXT]).pick_file()
                    {
                        self.status = match negative::CustomStock::from_file(&f).and_then(|c| c.install().map(|_| c.name)) {
                            Ok(n) => format!("Added stock profile {n}"),
                            Err(e) => format!("Could not import: {e}"),
                        };
                        self.my_stocks = negative::custom_stocks();
                    }
                });
                if let Some(k) = apply {
                    self.params.neg = self.my_stocks[k].1.apply(&self.params.neg);
                }
                if let Some(k) = share {
                    let c = self.my_stocks[k].1.clone();
                    if let Some(f) = rfd::FileDialog::new().set_file_name(format!("{}.{}", c.name, negative::CUSTOM_EXT)).save_file() {
                        self.status = match c.to_file(&f) { Ok(()) => format!("Saved {}", f.display()), Err(e) => format!("Could not save: {e}") };
                    }
                }
                if let Some(k) = remove {
                    let _ = std::fs::remove_file(&self.my_stocks[k].0);
                    self.my_stocks = negative::custom_stocks();
                }
                let mut last_brand = "";
                for s in STOCKS.iter().filter(keep) {
                    if s.brand != last_brand {
                        ui.add_space(8.0);
                        ui.label(caps(s.brand));
                        last_brand = s.brand;
                    }
                    // Marked only while converting: a highlighted stock on an unconverted frame read as "in use".
                    let on = self.params.neg.enabled && self.params.neg.stock == s.id;
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                    if on || resp.hovered() {
                        ui.painter().rect_filled(r, 3.0, if on { BONE } else { CARD });
                    }
                    ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, s.name, FontId::proportional(13.0), if on { INK } else { TEXT });
                    ui.painter().text(pos2(r.right() - 8.0, r.center().y), Align2::RIGHT_CENTER, stock_iso(s), FontId::monospace(11.0), if on { ON_BONE } else { TEXT3 });
                    if resp.clicked() {
                        self.params.neg.stock = s.id.into();
                        self.params.neg.enabled = true;
                    }
                }
            });
        });
    }

    fn negative_right(&mut self, ui: &mut Ui) {
        let auto_base = self.auto_base();
        let profile = self.params.profile.clone();
        let has_ir = self.photo.as_ref().map(|p| (p.has_ir, p.ir_busy));
        let ir_clean = &mut self.params.ir_clean;
        let np = &mut self.params.neg;
        let st = negative::stock(&np.stock);
        let mut add_character = false;
        let mut scan_action = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(16.0).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    // Until the frame is converted the stock is only a choice, so the header says so.
                    let on = np.enabled;
                    let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
                    ui.painter().rect_filled(r, 6.0, if on { stock_chip(st) } else { stock_chip(st).gamma_multiply(0.35) });
                    ui.painter().rect_stroke(r, 6.0, Stroke::new(1.0, hex(0x3A3A40)), egui::StrokeKind::Inside);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(format!("{} {}", st.brand, st.name)).family(semi()).size(15.0).color(if on { TEXT } else { TEXT2 }));
                        let iso = stock_iso(st);
                        let what = if iso.is_empty() { kind_label(st.kind).to_string() } else { format!("ISO {iso} · {}", kind_label(st.kind)) };
                        ui.label(RichText::new(if on { what } else { format!("Not converted · {what}") }).size(12.0).color(TEXT2));
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Convert negative").size(12.0).color(LABEL));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, &mut np.enabled));
                });
                // Steps 1–3 do nothing until the frame is converted: say so, and grey them out.
                if !np.enabled {
                    ui.label(RichText::new("Off. Turn on, or pick a stock, to convert this frame.").size(11.5).color(TEXT2));
                    // egui reads this when the ui is disabled, so it goes first. 0.75 keeps dim captions above 3:1
                    // and labels near 5:1 (egui's 0.5 gave 2:1), so you can still read what turning it on gives.
                    ui.visuals_mut().disabled_alpha = 0.75;
                    ui.disable();

                }
                ui.add_space(6.0);

                ui.label(caps("1 · Film base"));
                egui::Frame::new().fill(CARD).corner_radius(8).inner_margin(10.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let base = np.base.or(auto_base);
                        let (r, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                        ui.painter().rect_filled(r, 4.0, base.map(lin).unwrap_or(hex(0x3A3A40)));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(RichText::new(if np.base.is_some() { "Sampled from rebate" } else { "Auto · brightest area" }).size(12.0));
                            let rgb = base.map(|b| b.map(|v| (linear_to_srgb(v) * 255.0) as u8)).map(|c| format!("R {}  G {}  B {}", c[0], c[1], c[2])).unwrap_or("—".into());
                            ui.label(RichText::new(rgb).monospace().size(11.0).color(TEXT2));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if np.base.is_some() && ghost(ui, "Auto", false).clicked() {
                                np.base = None;
                                np.levels = None;
                            }
                            if ui.add(egui::Button::new(RichText::new(if self.picking_base { "Cancel" } else { "Resample" }).size(11.5)).min_size(vec2(0.0, 26.0))).clicked() {
                                self.picking_base = !self.picking_base;
                                self.view = View::After;
                            }
                        });
                    });
                });
                ui.add_space(6.0);

                ui.label(caps("2 · Look"));
                let w = (ui.available_width() - 4.0) / 2.0;
                for row in [[(Look::Neutral, "Neutral"), (Look::Frontier, "Frontier")], [(Look::Noritsu, "Noritsu"), (Look::Linear, "Linear")]] {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for (l, n) in row {
                            // Nothing looks chosen while conversion is off; picking one turns it on, like a stock.
                            if tile(ui, n, np.enabled && np.look == l, w).clicked() {
                                np.look = l;
                                np.enabled = true;
                            }
                        }
                    });
                }
                ui.add_space(6.0);

                add_character = ui.button(format!("Add {}'s grain and halation", st.name)).on_hover_text("Sets Grain and Halation in Develop's Effects").clicked();
                ui.add_space(6.0);

                ui.label(caps("3 · Balance"));
                let d = negative::NegParams::default();
                // Balance shifts one print channel; tracks show that shift on mid grey (×3 so the hue reads).
                let shift = |ch: usize, sign: f32| move |t: f32| { let mut c = [0.5f32; 3]; c[ch] += sign * t / 100.0 * 0.3; enc(c) };
                slider(ui, "Density", &mut np.density, -100.0, 100.0, 0.0, Some(&|t: f32| enc([0.5 - t / 100.0 * 0.3; 3])));
                slider(ui, "Contrast", &mut np.contrast, -100.0, 100.0, 0.0, None);
                slider(ui, "Cyan–Red", &mut np.cy, -100.0, 100.0, 0.0, Some(&shift(0, 1.0)));
                slider(ui, "Magenta–Green", &mut np.mg, -100.0, 100.0, 0.0, Some(&shift(1, 1.0)));
                slider(ui, "Yellow–Blue", &mut np.yb, -100.0, 100.0, 0.0, Some(&shift(2, 1.0)));
                slider(ui, "Shadow roll-off", &mut np.toe, 0.0, 100.0, d.toe, None);
                slider(ui, "Highlight roll-off", &mut np.shoulder, 0.0, 100.0, d.shoulder, None);
                ui.add_space(6.0);

                ui.label(caps("4 · Roll"));
                let mut locked = np.levels.is_some();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Lock balance across roll").size(12.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, &mut locked));
                });
                ui.label(RichText::new(if np.levels.is_some() { "Base and levels come from this frame. Use Apply to roll so every frame matches." } else { "Each frame is balanced on its own." }).size(12.0).color(TEXT2));
                if locked != np.levels.is_some() {
                    if locked {
                        if let Some(ph) = &self.photo {
                            let geo = develop::geometry(&ph.preview, &Params { neg: np.clone(), profile: profile.clone(), ..Default::default() });
                            let (b, l) = negative::analyse(&geo, np);
                            (np.base, np.levels) = (Some(b), Some(l));
                        }
                    } else {
                        np.levels = None;
                    }
                }
                if ui.button("Apply to roll").on_hover_text("Copy this conversion and colour profile to every photo in the folder").clicked() {
                    let neg = np.clone();
                    let cur = self.current.clone();
                    let mut n = 0;
                    for f in self.files.iter().filter(|f| Some(*f) != cur.as_ref()) {
                        let q = Params { neg: neg.clone(), profile: profile.clone(), ..read_params(f).unwrap_or_default() };
                        if write_params(f, &q).is_ok() {
                            self.meta.insert(f.clone(), Meta::of(&q));
                            n += 1;
                        }
                    }
                    self.status = format!("Applied to {n} frames");
                }
                ui.add_space(6.0);

                ui.label(caps("5 · Scanning"));
                ui.columns(2, |c| {
                    let w = c[0].available_width();
                    if c[0].add(egui::Button::new("Find frame").min_size(vec2(w, 28.0))).on_hover_text("Crop away the holder and film edge").clicked() {
                        scan_action = Some(0);
                    }
                    if c[1].add(egui::Button::new("Split strip").min_size(vec2(w, 28.0))).on_hover_text("Splits a scan of several frames so each frame becomes its own photo (as virtual copies)").clicked() {
                        scan_action = Some(1);
                    }
                    if c[0].add(egui::Button::new("Find dust").min_size(vec2(w, 28.0))).on_hover_text("Mark specks on the scan for healing").clicked() {
                        scan_action = Some(2);
                    }
                    if c[1].add(egui::Button::new("Base from shot…").min_size(vec2(w, 28.0))).on_hover_text("Use a photo of blank film (no frame) as the film base, for example for the whole roll").clicked() {
                        scan_action = Some(3);
                    }
                });
                // Infrared cleaning (Digital ICE style) for scans saved with their infrared channel.
                slider(ui, "Infrared clean", ir_clean, 0.0, 100.0, 0.0, None);
                let note = match has_ir {
                    Some((Some(false), _)) => Some("This scan has no infrared channel. Save scans as 64-bit RGBI TIFF (VueScan, SilverFast) to use it."),
                    Some((_, true)) => Some("Cleaning with the infrared channel…"),
                    _ if st.kind == Kind::Bw => Some("Silver black-and-white film blocks infrared, so this works on colour film and C-41 black-and-white only."),
                    _ if *ir_clean <= 0.0 => Some("Removes dust and scratches found in the scan's infrared channel."),
                    _ => None,
                };
                if let Some(n) = note {
                    ui.label(RichText::new(n).size(11.5).color(TEXT2));
                }
            });
        });
        match scan_action {
            Some(0) => self.scan_find_frame(),
            Some(1) => self.scan_split_strip(),
            Some(2) => self.find_dust(),
            Some(3) => self.scan_base_from_shot(),
            _ => {}
        }
        if add_character {
            (self.params.gr_amt, self.params.gr_size, self.params.hal_amt, self.params.gr_hi) = negative::character(st);
        }
    }

    fn scan_find_frame(&mut self) {
        let Some(ph) = &self.photo else { return };
        match scan::auto_crop(&scan::frame(&ph.preview, &self.params), self.params.neg.enabled) {
            Some(c) => {
                self.params.crop = c;
                self.params.neg.levels = None; // re-balance on the picture alone
                self.status = "Cropped to the frame".into();
            }
            None => self.status = "Couldn't find the frame edges".into(),
        }
    }

    fn scan_split_strip(&mut self) {
        let (Some(ph), Some(cur)) = (&self.photo, self.current.clone()) else { return };
        let frames = scan::split_strip(&scan::frame(&ph.preview, &self.params), self.params.neg.enabled);
        if frames.len() < 2 {
            self.status = "Found only one frame. Try Find frame instead".into();
            return;
        }
        let n = frames.len();
        for c in &frames[1..] {
            let p = Params { crop: *c, snapshots: vec![], ..self.params.clone() };
            self.virtual_copy(&cur, &p);
        }
        self.params.crop = frames[0];
        self.status = format!("Split into {n} frames (this one and {} virtual copies)", n - 1);
    }

    fn scan_base_from_shot(&mut self) {
        let Some(f) = rfd::FileDialog::new().set_title("Photo of blank film").pick_file() else { return };
        match load::load_quick(&f, 800) {
            Ok(img) => {
                // Average the middle of the frame: it is all film base.
                let (w, h) = img.dimensions();
                let (mut acc, mut n) = ([0f32; 3], 0f32);
                for y in h / 4..h * 3 / 4 {
                    for x in w / 4..w * 3 / 4 {
                        let p = img.get_pixel(x, y).0;
                        (0..3).for_each(|k| acc[k] += p[k]);
                        n += 1.0;
                    }
                }
                // The blank frame is read without a profile; the scans it applies to go through this photo's.
                self.params.neg.base = Some(crate::calib::reprofile(acc.map(|a| (a / n.max(1.0)).max(1e-4)), None, self.params.profile.as_ref()));
                self.params.neg.levels = None;
                self.status = format!("Film base taken from {}", f.file_name().unwrap_or_default().to_string_lossy());
            }
            Err(e) => self.status = format!("Could not open: {e}"),
        }
    }

    fn negative_footer(&mut self, ui: &mut Ui) {
        ui.columns(2, |c| {
            if c[0].add(egui::Button::new("Reset conversion").min_size(vec2(c[0].available_width(), 32.0))).on_hover_text("Back to this stock's defaults").clicked() {
                self.params.neg = negative::NegParams { enabled: true, stock: self.params.neg.stock.clone(), ..Default::default() };
            }
            // Secondary: Print in the top bar is this view's one filled button.
            if c[1].add(egui::Button::new("Open in Develop").min_size(vec2(c[1].available_width(), 32.0))).clicked() {
                self.module = Module::Develop;
            }
        });
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // Setting a shortcut in the editor: the next key press is the new binding.
        if let Some(id) = self.binding {
            let pressed = ctx.input(|i| i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, modifiers.shift)),
                _ => None,
            }));
            if let Some((key, shift)) = pressed {
                if key != egui::Key::Escape {
                    self.keymap.set(id, keys::combo(key, shift));
                    if let Err(e) = self.keymap.save() {
                        self.status = format!("Could not save shortcuts: {e}");
                    }
                }
                self.binding = None;
            }
            return;
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let km = &self.keymap;
        let hit: Vec<&'static str> = ctx.input(|i| keys::ACTIONS.iter().map(|a| a.0).filter(|id| km.pressed(i, id)).collect());
        let browsing = matches!(self.module, Module::Library | Module::Cull);
        // Ratings and labels work on the selection while browsing, or on the open photo.
        let target = if browsing { self.lib_sel.clone() } else { self.current.clone() };
        for id in hit {
            let (stars, flag, label) = match id {
                "rate0" | "rate1" | "rate2" | "rate3" | "rate4" | "rate5" => (Some(id[4..].parse::<u8>().unwrap_or(0)), None, None),
                "pick" => (None, Some(1i8), None),
                "reject" => (None, Some(-1), None),
                "unflag" => (None, Some(0), None),
                "red" => (None, None, Some(1u8)),
                "yellow" => (None, None, Some(2)),
                "green" => (None, None, Some(3)),
                "blue" => (None, None, Some(4)),
                "purple" => (None, None, Some(5)),
                _ => (None, None, None),
            };
            if stars.is_some() || flag.is_some() || label.is_some() {
                if let Some(f) = &target {
                    self.rate(f, stars, flag, label);
                    self.status = match (stars, flag, label) {
                        (Some(n), ..) => format!("Rated {n} stars"),
                        (_, Some(1), _) => "Picked".into(),
                        (_, Some(-1), _) => "Rejected".into(),
                        (_, Some(_), _) => "Flag cleared".into(),
                        _ => "Label set".into(),
                    };
                }
                continue;
            }
            match id {
                "next" | "prev" => {
                    let d = if id == "next" { 1 } else { -1 };
                    if browsing {
                        self.step_selection(d);
                    }
                }
                "cull" => {
                    self.module = if self.module == Module::Cull { Module::Library } else { Module::Cull };
                }
                "compare" => self.cull.compare = !self.cull.compare,
                "focus" => self.cull.focus = !self.cull.focus,
                "library" => self.module = Module::Library,
                "develop" => {
                    if let Some(f) = self.lib_sel.clone()
                        && browsing
                    {
                        self.module = Module::Develop;
                        if self.current.as_ref() != Some(&f) {
                            self.select(ctx, f);
                        }
                    } else {
                        self.module = Module::Develop;
                    }
                }
                "vcopy" => self.new_copy_of_current(),
                _ if browsing => {}
                "zoom" => self.toggle_zoom([0.5, 0.5]),
                "before" => self.view = if self.view == View::Before { View::After } else { View::Before },
                "clipping" => self.clip_view = !self.clip_view,
                "crop" | "heal" | "gradient" | "brush" if self.module == Module::Develop => {
                    let t = match id { "crop" => Tool::Crop, "heal" => Tool::Heal, _ => Tool::Mask };
                    if t == Tool::Mask {
                        self.mask_ui.draw = if id == "brush" { Draw::Brush } else { Draw::Linear };
                    }
                    self.tool = if self.tool == t { Tool::None } else { t };
                    self.zoom = None;
                    self.view = View::After;
                }
                "wb" => {
                    self.picking_wb = !self.picking_wb;
                    self.tool = Tool::None;
                    self.zoom = None;
                }
                "rot_left" => self.params.rotate(false),
                "rot_right" => self.params.rotate(true),
                "snapshot" => self.add_snapshot(),
                _ => {}
            }
        }
        let (undo, redo, prev, next, copy, paste) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (cmd && !i.modifiers.shift && i.key_pressed(egui::Key::Z), cmd && i.modifiers.shift && i.key_pressed(egui::Key::Z),
             i.key_pressed(egui::Key::ArrowLeft) && cmd, i.key_pressed(egui::Key::ArrowRight) && cmd,
             cmd && i.key_pressed(egui::Key::C), cmd && i.key_pressed(egui::Key::V))
        });
        if undo { self.undo(-1); }
        if redo { self.undo(1); }
        if prev { if browsing { self.step_selection(-1) } else { self.step(ctx, -1) } }
        if next { if browsing { self.step_selection(1) } else { self.step(ctx, 1) } }
        if copy { self.copied = Some(self.params.clone()); }
        if paste && let Some(c) = &self.copied { self.params = self.params.with_look_of(c); }
    }

    /// Move the Library / culling selection through the photos shown.
    fn step_selection(&mut self, d: isize) {
        let vis = self.lib_visible();
        if vis.is_empty() {
            return;
        }
        let i = self.lib_sel.as_ref().and_then(|s| vis.iter().position(|f| f == s)).map_or(0, |i| (i as isize + d).clamp(0, vis.len() as isize - 1) as usize);
        self.lib_sel = Some(vis[i].clone());
    }

    fn new_copy_of_current(&mut self) {
        let (f, p) = match self.module {
            Module::Library | Module::Cull => match &self.lib_sel { Some(f) => (f.clone(), read_params(f).unwrap_or_default()), None => return },
            _ => match &self.current { Some(f) => (f.clone(), self.params.clone()), None => return },
        };
        if let Some(c) = self.virtual_copy(&f, &p) {
            self.status = format!("Made {}", c.file_name().unwrap_or_default().to_string_lossy());
            self.lib_sel = Some(c);
        }
    }

    fn add_snapshot(&mut self) {
        if self.current.is_none() {
            return;
        }
        let name = format!("Snapshot {}", self.params.snapshots.len() + 1);
        let params = Params { snapshots: vec![], ..self.params.clone() };
        self.params.snapshots.push(develop::Snapshot { name: name.clone(), params });
        self.status = format!("Saved {name}");
    }

    /// The shortcut editor: click a key, press the new one.
    /// (Re)load the printer profile for soft proofing from the settings, and save them.
    fn load_printer(&mut self) {
        self.printer = None;
        if !self.colour.printer.is_empty() {
            match crate::colour::load_proof(&self.colour.printer, self.colour.perceptual, self.colour.paper) {
                Ok(p) => self.printer = Some(Arc::new(p)),
                Err(e) => self.status = format!("Printer profile: {e}"),
            }
        }
        let _ = self.colour.save();
        self.rendered = None;
    }

    /// Apply the screen colour setting to every picture shown from now on, and save it.
    fn apply_screen(&mut self) {
        let name = crate::colour::set_screen(&self.colour.screen);
        if self.colour.screen.profile.trim().is_empty() || self.colour.screen.profile == "srgb" {
            self.status = "Screen colour: sRGB".into();
        } else {
            self.status = match name { Some(n) => format!("Screen colour: {n}"), None => "That screen profile can't be used, so colours are shown as sRGB".into() };
        }
        if let Err(e) = self.colour.save() {
            self.status = format!("Could not save the colour settings: {e}");
        }
        // Everything on screen was made for the old screen setting.
        self.rendered = None;
        self.thumbs.clear();
        self.thumb_ready.clear();
        self.preset_tex.clear();
    }

    fn keys_window(&mut self, ctx: &egui::Context) {
        if !self.keys_open {
            return;
        }
        // Esc while waiting for a key cancels the binding, not the dialog.
        let binding = self.binding.is_some();
        let mut screen_changed = false;
        let open = dialog(ctx, "keys", "Keyboard shortcuts and screen", 380.0, |ui| {
            ui.label(caps("Screen colour"));
            let cur = self.colour.screen.profile.clone();
            let shown = match cur.as_str() {
                "" | "srgb" => "sRGB (as is)".to_string(),
                "p3" => "Display P3".into(),
                "adobe" => "Adobe RGB".into(),
                "auto" => "System profile".into(),
                p => Path::new(p).file_name().map_or(p.to_string(), |n| n.to_string_lossy().to_string()),
            };
            egui::ComboBox::from_id_salt("screen").width(ui.available_width()).selected_text(shown).show_ui(ui, |ui| {
                for (v, n) in [("", "sRGB (as is)"), ("p3", "Display P3"), ("adobe", "Adobe RGB"), ("auto", "System profile")] {
                    if picked(ui, cur == v, n).clicked() {
                        self.colour.screen.profile = v.into();
                        screen_changed = true;
                    }
                }
                if ui.button("Profile file…").clicked()
                    && let Some(f) = rfd::FileDialog::new().add_filter("ICC profile", &["icc", "icm"]).pick_file()
                {
                    self.colour.screen.profile = f.to_string_lossy().to_string();
                    screen_changed = true;
                }
            });
            ui.label(RichText::new("If colours look too strong on a wide-gamut monitor, choose its profile here. macOS usually manages colour itself, so keep sRGB there unless photos look oversaturated.").size(11.5).color(TEXT2));
            ui.add_space(10.0);
            ui.label(caps("Shortcuts"));
            ui.label(RichText::new("Click a shortcut, then press the new key (Esc cancels). Undo, copy, paste and Ctrl/Cmd+arrows stay fixed.").size(11.5).color(TEXT2));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
                egui::Grid::new("keys").num_columns(2).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                    for (id, name, _) in keys::ACTIONS {
                        ui.label(RichText::new(*name).size(12.5));
                        let waiting = self.binding == Some(*id);
                        let text = if waiting { "Press a key…".to_string() } else { keys::display(self.keymap.key(id)) };
                        if ui.add(egui::Button::new(RichText::new(text).monospace().size(12.0)).min_size(vec2(120.0, 24.0)).selected(waiting)).clicked() {
                            self.binding = Some(id);
                        }
                        ui.end_row();
                    }
                });
            });
            ui.add_space(6.0);
            if ui.button("Reset to defaults").clicked() {
                self.keymap.reset();
                let _ = self.keymap.save();
            }
            ui.add_space(4.0);
        });
        if screen_changed {
            self.apply_screen();
        }
        if !open && !(binding && ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
            self.keys_open = false;
            self.binding = None;
        }
    }

}

const ASPECTS: [(&str, Option<f32>); 6] = [("Free", None), ("1:1", Some(1.0)), ("4:5", Some(0.8)), ("2:3", Some(2.0 / 3.0)), ("3:2", Some(1.5)), ("16:9", Some(16.0 / 9.0))];

/// `n` five-point stars in a row starting at `c` (centre of the first), each `r` in radius.
fn paint_stars(p: &egui::Painter, c: egui::Pos2, n: u8, r: f32, col: Color32) {
    for i in 0..n {
        let c = c + vec2(i as f32 * r * 2.4, 0.0);
        let pt = |k: usize, rad: f32| {
            let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
            c + vec2(a.cos(), a.sin()) * rad
        };
        // Inner pentagon plus five tips: all convex pieces, which egui fills cleanly.
        p.add(egui::Shape::convex_polygon((0..5).map(|k| pt(2 * k + 1, r * 0.42)).collect(), col, Stroke::NONE));
        for k in 0..5 {
            p.add(egui::Shape::convex_polygon(vec![pt((2 * k + 9) % 10, r * 0.42), pt(2 * k, r), pt(2 * k + 1, r * 0.42)], col, Stroke::NONE));
        }
    }
}

fn paint_flag(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    p.line_segment([c + vec2(-3.0, -5.0), c + vec2(-3.0, 6.0)], Stroke::new(1.5, col));
    p.add(egui::Shape::convex_polygon(vec![c + vec2(-3.0, -5.0), c + vec2(4.0, -2.5), c + vec2(-3.0, 0.5)], col, Stroke::NONE));
}

/// Plain list row with a right-aligned count, like the mockup's folder list.
fn list_row(ui: &mut Ui, text: &str, count: &str, on: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
    if on || resp.hovered() {
        ui.painter().rect_filled(r, 3.0, if on { BONE } else { CARD });
    }
    ui.painter().text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(13.0), if on { INK } else { TEXT });
    ui.painter().text(pos2(r.right() - 8.0, r.center().y), Align2::RIGHT_CENTER, count, FontId::monospace(11.5), if on { ON_BONE } else { TEXT3 });
    resp
}

/// Point curve editor for one channel (0 = RGB, 1–3 = R, G, B). The parametric curve shows faintly under RGB.
/// Point-curve editor over the photo's histogram; `size` overrides the default (panel width, 4:5).
fn curve_editor(ui: &mut Ui, p: &mut Params, chan: usize, grab: &mut Option<usize>, size: Option<egui::Vec2>, bins: &[[f32; 128]; 3]) {
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(size.unwrap_or(vec2(w, w * 0.8)), Sense::click_and_drag());
    let pt = ui.painter_at(r.expand(4.0));
    pt.rect_filled(r, 4.0, BG);
    // Histogram of the channel being edited (all three for RGB), faint, behind the grid.
    let hist: Vec<f32> = (0..128).map(|i| if chan == 0 { (bins[0][i] + bins[1][i] + bins[2][i]) / 3.0 } else { bins[chan - 1][i] }).collect();
    let top = hist.iter().cloned().fold(1e-6, f32::max);
    for (i, v) in hist.iter().enumerate() {
        let x = r.left() + r.width() * i as f32 / 127.0;
        pt.vline(x, (r.bottom() - r.height() * 0.9 * (v / top).sqrt())..=r.bottom(), Stroke::new(r.width() / 127.0 + 0.5, Color32::from_white_alpha(14)));
    }
    for i in 1..4 {
        let f = i as f32 / 4.0;
        pt.vline(r.left() + r.width() * f, r.y_range(), Stroke::new(1.0, DIVIDER));
        pt.hline(r.x_range(), r.top() + r.height() * f, Stroke::new(1.0, DIVIDER));
    }
    pt.line_segment([r.left_bottom(), r.right_top()], Stroke::new(1.0, hex(0x3A3A40)));
    let to_s = |q: [f32; 2]| pos2(r.left() + r.width() * q[0], r.bottom() - r.height() * q[1]);
    let to_q = |s: egui::Pos2| [((s.x - r.left()) / r.width()).clamp(0.0, 1.0), ((r.bottom() - s.y) / r.height()).clamp(0.0, 1.0)];
    let col = [TEXT, hex(0xE0605A), hex(0x5DBB6E), hex(0x5C8DE0)][chan];
    if chan == 0 {
        let q = Params { c_hi: p.c_hi, c_li: p.c_li, c_da: p.c_da, c_sh: p.c_sh, ..Default::default() };
        let pts: Vec<_> = (0..=64).map(|i| { let x = i as f32 / 64.0; to_s([x, develop::tone_curve(x, &q).clamp(0.0, 1.0)]) }).collect();
        pt.add(egui::Shape::line(pts, Stroke::new(1.0, hex(0x55555C))));
    }
    // Interaction.
    let pts = &mut p.curves[chan];
    let near = |pts: &Vec<[f32; 2]>, s: egui::Pos2| pts.iter().position(|q| to_s(*q).distance(s) < 10.0);
    if resp.double_clicked()
        && let Some(s) = resp.interact_pointer_pos()
        && let Some(i) = near(pts, s)
        && pts.len() > 2
    {
        pts.remove(i);
    } else if resp.drag_started() || resp.clicked() {
        if let Some(s) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
            if pts.len() < 2 {
                *pts = vec![[0.0, 0.0], [1.0, 1.0]];
            }
            *grab = near(pts, s).or_else(|| {
                pts.push(to_q(s));
                pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
                pts.iter().position(|q| *q == to_q(s))
            });
        }
    }
    if resp.dragged()
        && let (Some(i), Some(s)) = (*grab, resp.interact_pointer_pos())
        && i < pts.len()
    {
        let mut q = to_q(s);
        // Points keep their order; end points stay at the edges.
        let lo = if i == 0 { 0.0 } else { pts[i - 1][0] + 0.01 };
        let hi = if i + 1 == pts.len() { 1.0 } else { pts[i + 1][0] - 0.01 };
        q[0] = if i == 0 { 0.0 } else if i + 1 == pts.len() { 1.0 } else { q[0].clamp(lo, hi) };
        pts[i] = q;
    }
    if resp.drag_stopped() {
        *grab = None;
    }
    let t = develop::curve_table(pts);
    let line: Vec<_> = (0..=128).map(|i| { let x = i as f32 / 128.0; to_s([x, develop::table(&t, x)]) }).collect();
    pt.add(egui::Shape::line(line, Stroke::new(2.0, col)));
    for q in pts.iter() {
        pt.circle(to_s(*q), 4.5, BG, Stroke::new(1.5, col));
    }
}

/// Three colour-grading wheels showing hue and saturation of shadows, midtones and highlights. Drag a wheel to set both.
fn grading_wheels(ui: &mut Ui, p: &mut Params) {
    let w = ui.available_width();
    let size = ((w - 16.0) / 3.0).min(84.0);
    let (area, _) = ui.allocate_exact_size(vec2(w, size + 20.0), Sense::hover());
    let gap = (w - size * 3.0) / 2.0;
    for (z, name) in ["Shadows", "Midtones", "Highlights"].into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(area.left() + z as f32 * (size + gap), area.top()), vec2(size, size));
        let resp = ui.interact(r, ui.scope_id().with(("wheel", z)), Sense::click_and_drag());
        let c = r.center();
        let rad = size / 2.0;
        paint_color_wheel(ui, r);
        if resp.double_clicked() {
            p.grade[z][0] = 0.0;
            p.grade[z][1] = 0.0;
        } else if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
            let v = pos - c;
            p.grade[z][0] = (-v.y).atan2(v.x).to_degrees().rem_euclid(360.0);
            p.grade[z][1] = (v.length() / rad * 100.0).clamp(0.0, 100.0).round();
        }
        let (sin, cos) = p.grade[z][0].to_radians().sin_cos();
        let k = p.grade[z][1] / 100.0 * rad;
        ui.painter().circle_stroke(pos2(c.x + cos * k, c.y - sin * k), 4.0, Stroke::new(2.0, Color32::WHITE));
        ui.painter().text(pos2(c.x, r.bottom() + 11.0), Align2::CENTER_CENTER, name, FontId::proportional(11.0), TEXT2);
    }
}

impl eframe::App for App {
    /// A caption still being typed is not saved until its field loses focus: save it when the window closes.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.commit();
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.scan_poll(&ctx);
        self.watch_folder(&ctx);
        self.shortcuts(&ctx);
        if let Some(f) = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf())) {
            self.open_path(&ctx, &f);
        }
        let side = |fill| egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, BORDER));
        let pad = |fill, x: i8, y: i8| egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(x, y));

        egui::Panel::top("top").exact_size(48.0).frame(pad(PANEL, 16, 0)).show(ui, |ui| self.top_bar(ui));
        match self.module {
            Module::Cull => {
                egui::Panel::right("cull_right").exact_size(280.0).resizable(false).frame(pad(PANEL, 16, 16)).show(ui, |ui| self.cull_panel(ui));
                egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS)).show(ui, |ui| {
                    egui::Panel::bottom("cull_strip").exact_size(84.0).frame(pad(BAR, 12, 10)).show(ui, |ui| self.filmstrip(ui));
                    egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS)).show(ui, |ui| self.cull_view(ui));
                });
            }
            Module::Scan => self.scan_module(ui),
            Module::Library => {
                egui::Panel::left("lib_left").exact_size(260.0).resizable(false).frame(pad(PANEL, 12, 16)).show(ui, |ui| self.library_left(ui));
                egui::Panel::right("lib_right").exact_size(320.0).resizable(false).frame(pad(PANEL, 16, 16)).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("lib_info").auto_shrink([false, false]).show(ui, |ui| {
                        self.library_info(ui);
                        self.roll_panel(ui);
                    });
                });
                egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show(ui, |ui| {
                    if !self.files.is_empty() { egui::Panel::top("lib_tools").exact_size(48.0).frame(pad(BG, 16, 0)).show(ui, |ui| self.library_toolbar(ui)); }
                    egui::CentralPanel::default().frame(pad(BG, 16, 16)).show(ui, |ui| self.library_grid(ui));
                });
            }
            m => {
                let neg = m == Module::Negative;
                egui::Panel::left("left").exact_size(260.0).resizable(false)
                    .frame(if neg { side(PANEL) } else { pad(PANEL, 12, 16) })
                    .show(ui, |ui| if neg { self.negative_left(ui) } else { self.develop_left(ui) });
                egui::Panel::right("right").exact_size(RIGHT_W).resizable(false).frame(side(PANEL)).show(ui, |ui| {
                    egui::Panel::bottom("right_footer").frame(pad(PANEL, 16, 10)).show(ui, |ui| if neg { self.negative_footer(ui) } else { self.develop_footer(ui) });
                    egui::Panel::top("hist").frame(pad(PANEL, 16, 12)).show(ui, |ui| self.histogram_block(ui));
                    egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| if neg { self.negative_right(ui) } else { self.develop_right(ui) });
                });
                if !neg && self.curve_big {
                    self.curve_window(&ctx);
                }
                self.refresh_render(&ctx);
                egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS)).show(ui, |ui| {
                    egui::Panel::top("tools").exact_size(44.0).frame(pad(BAR, 12, 0)).show(ui, |ui| self.canvas_toolbar(ui));
                    egui::Panel::bottom("strip").exact_size(84.0).frame(pad(BAR, 12, 10)).show(ui, |ui| self.filmstrip(ui));
                    egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS)).show(ui, |ui| self.image_view(ui));
                });
            }
        }
        self.export_window(&ctx);
        self.rename_window(&ctx);
        self.keys_window(&ctx);
        // One history step (and one sidecar write) per caption, not per keystroke.
        if !ctx.input(|i| i.pointer.any_down()) && !ctx.memory(|m| m.has_focus(egui::Id::NULL.with("caption"))) {
            self.commit();
        }
    }
}

//! The Scan view: bringing film in.
//!
//! - Light table: one scan of a strip. Finds its frames, lets you turn, flip, trim or skip each one, takes the film
//!   base from a gap between frames, finds dust, and adds the frames to the roll (this scan plus virtual copies).
//! - Capture: camera scanning into a watched folder. Each new frame opens as a positive with quick checks
//!   (exposure, corner sharpness, frame edges, dust) and a counter for the roll.
//! - Roll sheet: every frame of the roll at once, with batch fixes and roll-wide film base, balance, dust and names.
use super::*;
use crate::develop::Spot;
use std::collections::HashMap;

/// Pages of the Scan view, in the order of its switch.
pub(super) const PAGES: [&str; 3] = ["Light table", "Capture", "Roll sheet"];
const ROLL_LENGTHS: [usize; 4] = [12, 24, 36, 72];

/// One frame found on a strip scan.
#[derive(Clone)]
struct StripFrame {
    /// Crop in the scan's uncropped frame (left, top, right, bottom trimmed).
    crop: [f32; 4],
    skip: bool,
    /// Quarter turns clockwise.
    turns: u8,
    flip: bool,
}

/// The light table: the strip scan it was made from and what was found on it.
struct Strip {
    file: PathBuf,
    /// `App::photo_gen` it was made for.
    generation: usize,
    tex: TextureHandle,
    frames: Vec<StripFrame>,
    sel: usize,
    /// Extra trim on every side, in percent of the frame.
    trim: f32,
    /// Film base sampled from each gap between frames, and which one is used.
    gaps: Vec<(usize, [f32; 3])>,
    gap: usize,
    /// Dust found on each frame (in the frame's own fractions), once looked for.
    dust: Option<Vec<Vec<Spot>>>,
    show_neg: bool,
    /// The scan has an infrared channel (None until looked at).
    has_ir: Option<bool>,
    /// Frame preview: settings it was made for, and the picture.
    preview: Option<(u64, TextureHandle)>,
    /// Frames were added with these settings: adding again would make duplicates.
    added: Option<u64>,
    /// Dragging a frame box along the strip: the frame and where the drag started (fraction of the strip).
    drag: Option<(usize, f32, [f32; 4])>,
}

/// Quick checks on the last captured frame.
#[derive(Clone)]
struct Checks {
    exposure: (bool, String),
    sharp: (bool, String),
    framing: (bool, String),
    dust: Option<usize>,
}

/// What the roll sheet shows on each frame, read from its edits.
#[derive(Clone, Copy, Default)]
struct FrameInfo {
    dust: usize,
    turned: bool,
    flipped: bool,
    ir: bool,
}

impl FrameInfo {
    fn of(p: &Params) -> Self {
        FrameInfo { dust: p.spots.len(), turned: !p.rot90.is_multiple_of(4), flipped: p.flip_h, ir: p.ir_clean > 0.0 }
    }
}

/// A change a roll job wants made to one frame. Made on the UI thread, so the open photo's edits stay in step.
enum Edit {
    Crop([f32; 4]),
    /// Dust found by a job: added to the frame's heal spots.
    Spots(Vec<Spot>),
    Density(f32),
    /// Film base and locked levels.
    Balance([f32; 3], [[f32; 2]; 3]),
}

enum ScanMsg {
    Preview(PathBuf, u64, ColorImage),
    StripDust(PathBuf, usize, Vec<Vec<Spot>>),
    /// Whether a scan carries an infrared channel.
    HasIr(PathBuf, bool),
    Checks(PathBuf, Checks),
    Info(Vec<(PathBuf, FrameInfo)>),
    Edits(Vec<(PathBuf, Edit)>, String),
}

pub(super) struct ScanUi {
    pub page: usize,
    strip: Option<Strip>,
    /// A frame preview is rendering, and the settings wanted next.
    preview_busy: bool,
    checks: Option<(PathBuf, Option<Checks>)>,
    focus_checked: bool,
    /// The photo (`App::photo_gen`) and page last checked for an orange film base and, in Capture, its frame.
    converted: usize,
    roll_len: usize,
    sel: Vec<PathBuf>,
    anchor: Option<usize>,
    info: HashMap<PathBuf, FrameInfo>,
    info_folder: Option<(PathBuf, usize)>,
    /// A roll job is running: what it is doing.
    busy: Option<String>,
    tx: mpsc::Sender<ScanMsg>,
    rx: mpsc::Receiver<ScanMsg>,
}

impl Default for ScanUi {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        ScanUi { page: 0, strip: None, preview_busy: false, checks: None, focus_checked: false, converted: usize::MAX, roll_len: 2, sel: vec![], anchor: None, info: HashMap::new(), info_folder: None, busy: None, tx, rx }
    }
}

fn key_of(p: &Params, extra: impl std::hash::Hash) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(p).unwrap_or_default().hash(&mut h);
    extra.hash(&mut h);
    h.finish()
}

/// Mirror a frame left to right as you see it, carrying the crop and heal spots along.
fn flip_frame(p: &mut Params) {
    p.flip_h = !p.flip_h;
    let [l, t, r, b] = p.crop;
    p.crop = [r, t, l, b];
    for s in &mut p.spots {
        s.x = 1.0 - s.x;
        s.sx = 1.0 - s.sx;
        for q in &mut s.path {
            q[0] = 1.0 - q[0];
        }
    }
}

/// Dust found on one frame (fractions of that frame) as heal spots on the whole scan, whose uncropped frame has
/// the aspect `aspect` (width / height). A spot's radius is a share of the long side, which differs between the two.
fn spots_in_scan(crop: [f32; 4], aspect: f32, found: &[Spot]) -> Vec<Spot> {
    let [l, t, r, b] = crop;
    let (fw, fh) = (1.0 - l - r, 1.0 - t - b);
    // Long sides in the scan's height units.
    let frame_long = (fw * aspect).max(fh);
    let scan_long = aspect.max(1.0);
    found.iter().map(|sp| Spot { x: l + sp.x * fw, y: t + sp.y * fh, sx: l + sp.sx * fw, sy: t + sp.sy * fh, r: sp.r * frame_long / scan_long, ..sp.clone() }).collect()
}

/// Average colour of a region (fractions of the image), linear.
fn mean_rgb(img: &Rgb32FImage, x0: f32, y0: f32, x1: f32, y1: f32) -> Option<[f32; 3]> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (ax, bx) = ((x0 * w) as u32, ((x1 * w) as u32).min(img.width()));
    let (ay, by) = ((y0 * h) as u32, ((y1 * h) as u32).min(img.height()));
    let (mut acc, mut n) = ([0f32; 3], 0f32);
    for y in ay..by {
        for x in ax..bx {
            let p = img.get_pixel(x, y).0;
            (0..3).for_each(|k| acc[k] += p[k]);
            n += 1.0;
        }
    }
    (n > 0.0).then(|| acc.map(|a| (a / n).max(1e-4)))
}

/// Film base from the gaps between frames on a strip: the middle of each gap, away from the frame edges.
/// Returns (the frame before the gap, colour).
fn gap_bases(img: &Rgb32FImage, frames: &[[f32; 4]]) -> Vec<(usize, [f32; 3])> {
    let horizontal = img.width() >= img.height();
    let mut out = vec![];
    for k in 0..frames.len().saturating_sub(1) {
        let (a, b) = (frames[k], frames[k + 1]);
        // Along the strip: from the end of one frame to the start of the next. Across: where both frames are.
        let (s0, s1, c0, c1) = if horizontal { (1.0 - a[2], b[0], a[1].max(b[1]), 1.0 - a[3].max(b[3])) } else { (1.0 - a[3], b[1], a[0].max(b[0]), 1.0 - a[2].max(b[2])) };
        if s1 - s0 < 0.002 || c1 - c0 < 0.1 {
            continue;
        }
        let (m0, m1) = (s0 + (s1 - s0) * 0.3, s1 - (s1 - s0) * 0.3);
        let (n0, n1) = (c0 + (c1 - c0) * 0.25, c1 - (c1 - c0) * 0.25);
        let rgb = if horizontal { mean_rgb(img, m0, n0, m1, n1) } else { mean_rgb(img, n0, m0, n1, m1) };
        if let Some(rgb) = rgb {
            out.push((k, rgb));
        }
    }
    out
}

/// Film base from a photo of blank film: the middle of the frame, read through `profile` like the scans.
fn base_from_file(f: &Path, profile: Option<&crate::calib::ColorProfile>) -> Result<[f32; 3], String> {
    let img = load::load_quick(f, 800)?;
    let b = mean_rgb(&img, 0.25, 0.25, 0.75, 0.75).ok_or("empty picture")?;
    Ok(crate::calib::reprofile(b, None, profile))
}

/// Edge energy (mean absolute difference to the right and below) of a region of a luma plane.
fn edge_energy(l: &[f32], w: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> f32 {
    let (mut acc, mut n) = (0.0, 0.0);
    for y in y0..y1.saturating_sub(1) {
        for x in x0..x1.saturating_sub(1) {
            let i = y * w + x;
            acc += (l[i + 1] - l[i]).abs() + (l[i + w] - l[i]).abs();
            n += 1.0;
        }
    }
    if n > 0.0 { acc / n } else { 0.0 }
}

/// Exposure, corner sharpness, frame edges and dust on a captured frame.
fn check_frame(preview: &Rgb32FImage, full: &Rgb32FImage, p: &Params) -> Checks {
    let g = scan::frame(preview, p);
    let auto_base = scan::film_base(&g);
    let neg = p.neg.enabled || negative::orange_mask(auto_base);
    let n = (g.width() * g.height()).max(1) as f32;
    let clipped = g.pixels().filter(|q| q.0.iter().any(|v| *v >= 0.995)).count() as f32 / n;
    let exposure = if clipped > 0.002 {
        (false, format!("{:.1}% clipped: give less light or exposure", clipped * 100.0))
    } else if neg {
        let b = luma(auto_base) / 0.995;
        if b < 0.45 {
            (false, format!("Film base at {:.0}%: room for more exposure", b * 100.0))
        } else {
            (true, format!("Film base at {:.0}%, nothing clipped", b * 100.0))
        }
    } else {
        (true, "Nothing clipped".into())
    };
    let crop = scan::auto_crop(&g, neg);
    let framing = match crop {
        Some(_) => (true, "Frame edges found".to_string()),
        None => (false, "Couldn't find the frame edges: is the whole frame in view?".to_string()),
    };
    // Corners against the centre, inside the frame: a tilted or bowed negative goes soft at one end.
    let c = crop.unwrap_or([0.05; 4]);
    let (w, h) = (g.width() as usize, g.height() as usize);
    let l: Vec<f32> = g.pixels().map(|q| luma(q.0).max(0.0).sqrt()).collect();
    let (x0, y0) = ((c[0] * w as f32) as usize, (c[1] * h as f32) as usize);
    let (x1, y1) = (w - (c[2] * w as f32) as usize, h - (c[3] * h as f32) as usize);
    let (bw, bh) = ((x1.saturating_sub(x0)) / 5, (y1.saturating_sub(y0)) / 5);
    let mid = edge_energy(&l, w, (x0 + x1) / 2 - bw / 2, (y0 + y1) / 2 - bh / 2, (x0 + x1) / 2 + bw / 2, (y0 + y1) / 2 + bh / 2);
    let corners = [(x0, y0), (x1 - bw, y0), (x0, y1 - bh), (x1 - bw, y1 - bh)].map(|(x, y)| edge_energy(&l, w, x, y, x + bw, y + bh));
    let worst = corners.iter().cloned().fold(f32::MAX, f32::min);
    let names = ["top left", "top right", "bottom left", "bottom right"];
    let wi = corners.iter().position(|v| *v == worst).unwrap_or(0);
    let sharp = if mid < 1e-4 || bw < 4 {
        (true, "Not enough detail to judge".to_string())
    } else if worst / mid < 0.45 {
        (false, format!("The {} corner is much softer than the centre: check the film is flat and square to the lens", names[wi]))
    } else {
        (true, "Corners about as sharp as the centre".into())
    };
    let src = load::fit(full, 3000);
    let dust = Some(scan::find_dust(&scan::frame(&src, p), crop.unwrap_or([0.0; 4]), 200).len());
    Checks { exposure, sharp, framing, dust }
}

impl App {
    /// Collect finished background work for the Scan view. Called every frame.
    pub(super) fn scan_poll(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.scan.rx.try_recv() {
            match m {
                ScanMsg::Preview(f, key, img) => {
                    self.scan.preview_busy = false;
                    if let Some(s) = &mut self.scan.strip
                        && s.file == f
                    {
                        let tex = ctx.load_texture("scan_frame", img, TextureOptions::LINEAR);
                        s.preview = Some((key, tex));
                    }
                }
                ScanMsg::StripDust(f, generation, d) => {
                    self.scan.busy = None;
                    if let Some(s) = &mut self.scan.strip
                        && s.file == f
                        && s.generation == generation
                    {
                        let n: usize = d.iter().map(|v| v.len()).sum();
                        self.status = format!("Found {n} specks of dust on {} frames", d.len());
                        s.dust = Some(d);
                    }
                }
                ScanMsg::HasIr(f, ir) => {
                    if let Some(s) = &mut self.scan.strip
                        && s.file == f
                    {
                        s.has_ir = Some(ir);
                    }
                }
                ScanMsg::Checks(f, c) => {
                    if self.scan.checks.as_ref().is_some_and(|(g, _)| *g == f) {
                        self.scan.checks = Some((f, Some(c)));
                    }
                }
                ScanMsg::Info(v) => {
                    for (f, i) in v {
                        self.scan.info.insert(f, i);
                    }
                }
                ScanMsg::Edits(edits, say) => {
                    self.scan.busy = None;
                    for (f, e) in edits {
                        self.edit_frame(&f, |p| match e {
                            Edit::Crop(c) => {
                                p.crop = c;
                                p.neg.levels = None;
                            }
                            Edit::Spots(s) => p.spots.extend(s),
                            Edit::Density(d) => p.neg.density = d,
                            Edit::Balance(b, l) => {
                                p.neg.enabled = true;
                                p.neg.base = Some(b);
                                p.neg.levels = Some(l);
                            }
                        });
                    }
                    self.status = say;
                }
            }
        }
        if self.scan.busy.is_some() || self.scan.preview_busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// Change one frame's edits: the open photo in memory, any other frame in its sidecar.
    fn edit_frame(&mut self, f: &Path, change: impl FnOnce(&mut Params)) {
        if self.current.as_deref() == Some(f) {
            change(&mut self.params);
            self.scan.info.insert(f.to_path_buf(), FrameInfo::of(&self.params));
            return;
        }
        let mut p = read_params(f).unwrap_or_default();
        change(&mut p);
        match write_params(f, &p) {
            Ok(()) => {
                self.meta.insert(f.to_path_buf(), Meta::of(&p));
                self.scan.info.insert(f.to_path_buf(), FrameInfo::of(&p));
                // The thumbnail cache is keyed by the sidecar's time: dropping the texture makes a fresh one.
                self.thumbs.remove(f);
            }
            Err(e) => self.status = format!("Could not save {}: {e}", f.file_name().unwrap_or_default().to_string_lossy()),
        }
    }

    /// Frames of the roll in order, not counting rejected (skipped) ones.
    fn roll_frames(&self) -> Vec<PathBuf> {
        self.files.iter().filter(|f| self.meta.get(*f).is_none_or(|m| m.flag >= 0)).cloned().collect()
    }

    /// The Scan view's panels.
    pub(super) fn scan_module(&mut self, ui: &mut Ui) {
        let pad = |fill, x: i8, y: i8| egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(x, y));
        let side = egui::Frame::new().fill(PANEL).stroke(Stroke::new(1.0, BORDER));
        let ctx = ui.ctx().clone();
        let page = self.scan.page;
        egui::Panel::left("scan_left").exact_size(264.0).resizable(false).frame(pad(PANEL, 16, 16)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                // Room for the scroll bar.
                egui::Frame::new().inner_margin(egui::Margin { right: 10, ..Default::default() }).show(ui, |ui| match page {
                    0 => self.light_table_left(ui),
                    1 => self.capture_left(ui),
                    _ => self.roll_left(ui),
                });
            });
        });
        egui::Panel::right("scan_right").exact_size(if page == 2 { 300.0 } else { 320.0 }).resizable(false).frame(side).show(ui, |ui| {
            egui::Panel::bottom("scan_right_footer").frame(pad(PANEL, 16, 10)).show(ui, |ui| match page {
                0 => self.light_table_footer(ui),
                1 => self.capture_footer(ui),
                _ => self.roll_footer(ui),
            });
            egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    egui::Frame::new().inner_margin(16.0).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 10.0;
                        match page {
                            0 => self.light_table_right(ui),
                            1 => self.capture_right(ui),
                            _ => self.roll_right(ui),
                        }
                    });
                });
            });
        });
        // Light table renders its own previews, but the infrared clean (and AI denoise) of the scan still runs.
        match page {
            0 => self.update_denoise(&ctx),
            1 => self.refresh_render(&ctx),
            _ => {}
        }
        // A scan with an orange film base is a colour negative: show it converted, as Negative does.
        // Once per photo and page: 1 + page, so Light table and Capture each get their turn.
        let seen = self.photo_gen.wrapping_mul(4) + page;
        if page != 2
            && let Some(ph) = &self.photo
            && self.scan.converted != seen
        {
            self.scan.converted = seen;
            let g = scan::frame(&ph.preview, &self.params);
            let neg = negative::orange_mask(scan::film_base(&g));
            if !self.params.neg.enabled && neg {
                self.params.neg.enabled = true;
            }
            // A camera scan shows the holder around the frame: crop to the frame, so the conversion is
            // balanced on the picture alone. Only for frames not cropped yet.
            if page == 1
                && self.params.crop == [0.0; 4]
                && let Some(c) = scan::auto_crop(&g, neg || self.params.neg.enabled)
            {
                self.params.crop = c;
                self.params.neg.levels = None;
                self.status = "Cropped to the frame".into();
            }
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS)).show(ui, |ui| {
            egui::Panel::top("scan_tools").exact_size(44.0).frame(pad(BAR, 12, 0)).show(ui, |ui| self.scan_toolbar(ui));
            match page {
                0 => {
                    egui::Panel::bottom("scan_strip").exact_size(84.0).frame(pad(BAR, 12, 10)).show(ui, |ui| self.filmstrip(ui));
                    egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS)).show(ui, |ui| self.light_table(ui));
                }
                1 => {
                    egui::Panel::bottom("scan_counter").exact_size(92.0).frame(pad(BAR, 16, 10)).show(ui, |ui| self.roll_counter(ui));
                    egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS)).show(ui, |ui| {
                        if self.files.is_empty() {
                            self.scan_empty(ui, "Nothing captured yet", "Pick the folder your camera or tether app saves to. New frames open here as they arrive.");
                        } else {
                            self.image_view(ui);
                        }
                    });
                }
                _ => {
                    egui::CentralPanel::default().frame(pad(CANVAS, 20, 16)).show(ui, |ui| self.roll_grid(ui));
                }
            }
        });
    }

    fn scan_toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let mut page = self.scan.page;
            segmented(ui, &mut page, &PAGES);
            if page != self.scan.page {
                self.scan.page = page;
                self.zoom = None;
                self.tool = Tool::None;
                self.view = View::After;
            }
            ui.add_space(8.0);
            match self.scan.page {
                0 => {
                    let can = self.photo.is_some();
                    if ui.add_enabled(can, egui::Button::new("Find frames")).on_hover_text("Look for the frames on this scan again").clicked() {
                        self.scan.strip = None;
                    }
                    if let Some(s) = &mut self.scan.strip {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            // Laid out right to left, so listed backwards: it reads Negative | Positive.
                            let mut i = if s.show_neg { 1 } else { 0 };
                            segmented(ui, &mut i, &["Positive", "Negative"]);
                            s.show_neg = i == 1;
                        });
                    }
                }
                1 => {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let next = self.roll_frames().len() + 1;
                        let (text, col) = if self.watch { (format!("Waiting for frame {next}: shoot it and it opens here"), TEXT2) } else { ("Folder watch is off".to_string(), WARN) };
                        ui.label(RichText::new(text).monospace().size(11.5).color(col));
                    });
                }
                _ => {
                    let n = self.scan.sel.len();
                    let label = if n == 0 { "None selected".to_string() } else { format!("{n} selected") };
                    ui.label(RichText::new(label).monospace().size(11.5).color(if n > 0 { TEXT } else { TEXT3 }));
                    ui.add_space(6.0);
                    let any = n > 0;
                    let mut act = None;
                    for (k, (name, tip)) in [("Rotate", "Turn a quarter clockwise"), ("Flip", "Mirror left to right (emulsion side scanned the wrong way)"), ("Find frame", "Crop away the holder and film edge"), ("Skip", "Leave out of the roll (marks the frame rejected); again to bring it back")].iter().enumerate() {
                        if ui.add_enabled(any, egui::Button::new(*name)).on_hover_text(*tip).clicked() {
                            act = Some(k);
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ghost(ui, "Select all", false).clicked() {
                            self.scan.sel = self.files.clone();
                        }
                        if any && ghost(ui, "Clear", false).clicked() {
                            self.scan.sel.clear();
                        }
                    });
                    match act {
                        Some(0) => self.roll_each(|p| p.rotate(true)),
                        Some(1) => self.roll_each(flip_frame),
                        Some(2) => self.roll_find_frames(),
                        Some(3) => {
                            let skip = self.scan.sel.iter().any(|f| self.meta.get(f).is_none_or(|m| m.flag >= 0));
                            for f in self.scan.sel.clone() {
                                self.rate(&f, None, Some(if skip { -1 } else { 0 }), None);
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
    }

    fn scan_empty(&mut self, ui: &mut Ui, title: &str, line: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() / 2.0 - 100.0).max(0.0));
            let (r, _) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
            paint_mark(ui.painter(), r, TEXT2);
            ui.add_space(10.0);
            ui.label(RichText::new(title).family(display()).size(26.0).color(BONE).extra_letter_spacing(-0.3));
            ui.label(RichText::new(line).color(TEXT2));
            ui.add_space(8.0);
            if primary(ui, "Open roll…", true).clicked()
                && let Some(d) = rfd::FileDialog::new().pick_folder()
            {
                self.open_path(ui.ctx(), &d);
            }
        });
    }

    // ---------- Light table ----------

    /// Look for frames on the open scan (again) once it has loaded.
    fn ensure_strip(&mut self, ctx: &egui::Context) {
        let (Some(ph), Some(cur)) = (&self.photo, &self.current) else { return };
        if self.scan.strip.as_ref().is_some_and(|s| s.file == *cur && s.generation == self.photo_gen) {
            return;
        }
        let g = scan::frame(&ph.preview, &self.params);
        let neg = self.params.neg.enabled || negative::orange_mask(scan::film_base(&g));
        let mut crops = scan::split_strip(&g, neg);
        if crops.is_empty() {
            crops = vec![scan::auto_crop(&g, neg).unwrap_or([0.0; 4])];
        }
        let gaps = gap_bases(&g, &crops);
        let tex = ctx.load_texture("scan_strip", to_color_image(&encode(&g)), TextureOptions::LINEAR);
        if neg && !self.params.neg.enabled {
            self.params.neg.enabled = true;
        }
        // Reading the infrared channel means reading the whole TIFF: off the UI thread (it is cached for the clean).
        let (tx, ctx2, f) = (self.scan.tx.clone(), ctx.clone(), cur.clone());
        std::thread::spawn(move || {
            let _ = tx.send(ScanMsg::HasIr(f.clone(), scan::ir_channel(&f).is_some()));
            ctx2.request_repaint();
        });
        self.status = match crops.len() {
            1 => "Found one frame on this scan".into(),
            n => format!("Found {n} frames on this strip"),
        };
        self.scan.strip = Some(Strip {
            file: cur.clone(),
            generation: self.photo_gen,
            tex,
            frames: crops.into_iter().map(|crop| StripFrame { crop, skip: false, turns: 0, flip: false }).collect(),
            sel: 0,
            trim: 0.0,
            gap: gaps.len() / 2,
            gaps,
            dust: None,
            show_neg: false,
            has_ir: None,
            preview: None,
            added: None,
            drag: None,
        });
    }

    /// This scan's settings for frame `i` of the strip, with or without its dust.
    fn strip_frame_params(&self, i: usize, with_dust: bool) -> Params {
        let s = self.scan.strip.as_ref().unwrap();
        let fr = &s.frames[i];
        let t = s.trim / 100.0;
        let [l, tp, r, b] = fr.crop;
        // Trim is a share of the frame's own size.
        let (fw, fh) = (1.0 - l - r, 1.0 - tp - b);
        let mut p = Params { crop: [l + fw * t, tp + fh * t, r + fw * t, b + fh * t], snapshots: vec![], spots: vec![], rating: 0, flag: 0, label: 0, ..self.params.clone() };
        if let Some((_, base)) = s.gaps.get(s.gap) {
            p.neg.base = Some(*base);
            p.neg.levels = None;
        }
        if with_dust && let Some(d) = s.dust.as_ref().and_then(|d| d.get(i)) {
            let (w, h) = (s.tex.size()[0] as f32, s.tex.size()[1] as f32);
            p.spots = spots_in_scan(fr.crop, w / h, d);
        }
        if fr.flip {
            flip_frame(&mut p);
        }
        for _ in 0..fr.turns % 4 {
            p.rotate(true);
        }
        p
    }

    /// Render the selected frame of the strip on a worker thread when its settings change.
    fn strip_preview(&mut self, ctx: &egui::Context) {
        let Some(s) = &self.scan.strip else { return };
        let Some(ph) = &self.photo else { return };
        if self.scan.preview_busy || s.frames.is_empty() {
            return;
        }
        let mut p = self.strip_frame_params(s.sel, false);
        if s.show_neg {
            p = Params { crop: p.crop, rot90: p.rot90, flip_h: p.flip_h, profile: p.profile.clone(), ..Default::default() };
        }
        let key = key_of(&p, (s.sel, s.show_neg, ph.ir_applied.to_bits()));
        if s.preview.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        self.scan.preview_busy = true;
        let (tx, ctx2, full, f) = (self.scan.tx.clone(), ctx.clone(), ph.full.clone(), s.file.clone());
        std::thread::spawn(move || {
            let out = develop::render(&full, &p, 1.0);
            let out = if out.width().max(out.height()) > 1600 { load::fit(&out, 1600) } else { out };
            let _ = tx.send(ScanMsg::Preview(f, key, to_color_image(&out)));
            ctx2.request_repaint();
        });
    }

    /// Look for dust on every frame of the strip, at full resolution, on a worker thread.
    fn strip_find_dust(&mut self) {
        let (Some(s), Some(ph)) = (&self.scan.strip, &self.photo) else { return };
        let (tx, full, p, f, generation) = (self.scan.tx.clone(), ph.full.clone(), self.params.clone(), s.file.clone(), s.generation);
        let crops: Vec<[f32; 4]> = s.frames.iter().map(|fr| fr.crop).collect();
        self.scan.busy = Some("Looking for dust…".into());
        std::thread::spawn(move || {
            let g = scan::frame(&full, &p);
            let (w, h) = (g.width() as f32, g.height() as f32);
            let found = crops.par_iter().map(|c| {
                let (x, y) = ((c[0] * w) as u32, (c[1] * h) as u32);
                let (cw, ch) = (((1.0 - c[0] - c[2]) * w) as u32, ((1.0 - c[1] - c[3]) * h) as u32);
                if cw < 16 || ch < 16 {
                    return vec![];
                }
                let sub = image::imageops::crop_imm(&g, x, y, cw, ch).to_image();
                scan::find_dust(&sub, [0.0; 4], 150)
            }).collect();
            let _ = tx.send(ScanMsg::StripDust(f, generation, found));
        });
    }

    fn light_table(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if self.photo.is_none() {
            if self.loading {
                ui.centered_and_justified(|ui| ui.spinner());
            } else if self.files.is_empty() {
                self.scan_empty(ui, "No scans yet", "Open the folder your scanner saves to, or drop a strip scan here.");
            } else {
                ui.centered_and_justified(|ui| ui.label(RichText::new("Pick a strip scan below").color(TEXT3)));
            }
            return;
        }
        self.ensure_strip(&ctx);
        self.strip_preview(&ctx);
        let Some(s) = &mut self.scan.strip else { return };
        let full = ui.available_rect_before_wrap();
        // The strip across the top.
        let ts = s.tex.size_vec2();
        let strip_h = (full.height() * 0.3).clamp(90.0, 220.0);
        let sc = ((full.width() - 64.0) / ts.x).min((strip_h - 30.0) / ts.y);
        let sr = Rect::from_center_size(pos2(full.center().x, full.top() + 18.0 + ts.y * sc / 2.0 + 6.0), ts * sc);
        let resp = ui.interact(sr.expand(6.0), ui.scope_id().with("strip"), Sense::click_and_drag());
        egui::Image::new(&s.tex).paint_at(ui, sr);
        let to = |c: [f32; 4]| Rect::from_min_max(pos2(sr.left() + c[0] * sr.width(), sr.top() + c[1] * sr.height()), pos2(sr.right() - c[2] * sr.width(), sr.bottom() - c[3] * sr.height()));
        let horizontal = ts.x >= ts.y;
        for (i, fr) in s.frames.iter().enumerate() {
            let r = to(fr.crop);
            let on = i == s.sel;
            let col = if fr.skip { TEXT3 } else { BONE };
            if on {
                ui.painter().rect_stroke(r.expand(2.0), 2.0, Stroke::new(2.0, BONE), egui::StrokeKind::Outside);
            } else {
                ui.painter().rect_stroke(r, 1.0, Stroke::new(1.0, col.gamma_multiply(0.8)), egui::StrokeKind::Outside);
            }
            if fr.skip {
                ui.painter().rect_filled(r, 1.0, Color32::from_black_alpha(150));
                ui.painter().line_segment([r.left_top(), r.right_bottom()], Stroke::new(1.0, TEXT3));
            }
            let g = ui.painter().layout_no_wrap(format!("{}", i + 1), FontId::monospace(10.5), if on { INK } else { BONE });
            let tag = Rect::from_min_size(pos2(r.left() - 2.0, r.top() - 18.0), g.size() + vec2(10.0, 4.0));
            ui.painter().rect_filled(tag, 3.0, if on { BONE } else { INK });
            ui.painter().galley(tag.min + vec2(5.0, 2.0), g, BONE);
        }
        // Where the film base comes from.
        if let Some((k, _)) = s.gaps.get(s.gap)
            && let (Some(a), Some(b)) = (s.frames.get(*k), s.frames.get(*k + 1))
        {
            let (ra, rb) = (to(a.crop), to(b.crop));
            let gr = if horizontal { Rect::from_min_max(pos2(ra.right() + 1.0, ra.top() + 6.0), pos2(rb.left() - 1.0, ra.bottom() - 6.0)) } else { Rect::from_min_max(pos2(ra.left() + 6.0, ra.bottom() + 1.0), pos2(ra.right() - 6.0, rb.top() - 1.0)) };
            if gr.is_positive() {
                ui.painter().rect_stroke(gr, 1.0, Stroke::new(1.0, BONE), egui::StrokeKind::Inside);
            }
        }
        // Click picks a frame; dragging a frame slides its box along the strip.
        if let Some(pos) = resp.interact_pointer_pos() {
            let u = if horizontal { (pos.x - sr.left()) / sr.width() } else { (pos.y - sr.top()) / sr.height() };
            if resp.drag_started() || resp.clicked() {
                if let Some(i) = s.frames.iter().position(|fr| to(fr.crop).expand(4.0).contains(pos)) {
                    s.sel = i;
                    s.drag = Some((i, u, s.frames[i].crop));
                }
            } else if resp.dragged()
                && let Some((i, u0, c0)) = s.drag
            {
                let d = u - u0;
                let mut c = c0;
                if horizontal {
                    let d = d.clamp(-c0[0], c0[2]);
                    (c[0], c[2]) = (c0[0] + d, c0[2] - d);
                } else {
                    let d = d.clamp(-c0[1], c0[3]);
                    (c[1], c[3]) = (c0[1] + d, c0[3] - d);
                }
                s.frames[i].crop = c;
            }
        }
        if resp.drag_stopped() {
            s.drag = None;
        }
        let found = s.frames.len();
        let base_line = match s.gaps.get(s.gap) {
            Some((k, _)) => format!("film base from the gap between {} and {}", k + 1, k + 2),
            None => "film base: brightest area (no gap between frames)".into(),
        };
        let line = format!("{found} frame{} · drag a box to move it · {base_line}", if found == 1 { "" } else { "s" });
        ui.painter().text(pos2(sr.left(), sr.bottom() + 16.0), Align2::LEFT_CENTER, line, FontId::monospace(11.0), TEXT3);

        // The chosen frame, large.
        let area = Rect::from_min_max(pos2(full.left() + 24.0, sr.bottom() + 34.0), pos2(full.right() - 24.0, full.bottom() - 16.0));
        match &s.preview {
            Some((_, t)) if area.is_positive() => {
                let ts = t.size_vec2();
                let k = (area.width() / ts.x).min(area.height() / ts.y);
                let r = Rect::from_center_size(area.center(), ts * k);
                egui::Image::new(t).corner_radius(2).paint_at(ui, r);
                let fr = &s.frames[s.sel.min(found - 1)];
                // Dust marks, in the frame's own (unturned) fractions: turned like the picture is.
                if let Some(d) = s.dust.as_ref().and_then(|d| d.get(s.sel)) {
                    for sp in d {
                        let (mut x, mut y) = (sp.x, sp.y);
                        if fr.flip {
                            x = 1.0 - x;
                        }
                        for _ in 0..fr.turns % 4 {
                            (x, y) = (1.0 - y, x);
                        }
                        let c = pos2(r.left() + x * r.width(), r.top() + y * r.height());
                        let rad = (sp.r * r.width().max(r.height())).max(5.0);
                        ui.painter().circle_stroke(c, rad + 1.0, Stroke::new(1.0, INK.gamma_multiply(0.6)));
                        ui.painter().circle_stroke(c, rad, Stroke::new(1.5, BONE));
                    }
                }
                if self.scan.preview_busy {
                    ui.put(Rect::from_min_size(r.right_top() + vec2(-28.0, 8.0), vec2(20.0, 20.0)), egui::Spinner::new());
                }
            }
            _ => {
                ui.put(Rect::from_center_size(area.center(), vec2(24.0, 24.0)), egui::Spinner::new());
            }
        }
    }

    fn light_table_left(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 10.0;
        ui.label(caps("Source"));
        ui.label(RichText::new("Scan with your scanner's own software into a folder, then open it here. Strips of several frames are split up; single frames are cropped.").size(12.0).color(TEXT2));
        if ui.add(egui::Button::new("Open strip scan…").min_size(vec2(ui.available_width(), 30.0))).clicked()
            && let Some(f) = rfd::FileDialog::new().add_filter("Scans", &["tif", "tiff", "dng", "jpg", "jpeg", "png"]).pick_file()
        {
            self.scan.strip = None;
            self.open_path(ui.ctx(), &f);
        }
        if let Some(c) = &self.current {
            ui.label(RichText::new(c.file_name().unwrap_or_default().to_string_lossy()).monospace().size(11.0).color(TEXT3));
        }
        ui.add_space(6.0);
        self.roll_notes(ui);
    }

    fn light_table_right(&mut self, ui: &mut Ui) {
        let auto_base = self.auto_base();
        let busy = self.scan.busy.clone();
        let Some(s) = &mut self.scan.strip else {
            ui.label(RichText::new("Open a scan to see its frames.").color(TEXT3));
            return;
        };
        if s.frames.is_empty() {
            return;
        }
        let i = s.sel.min(s.frames.len() - 1);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
            ui.painter().rect_filled(r, 4.0, stock_chip(negative::stock(&self.params.neg.stock)));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(format!("Frame {} of {}", i + 1, s.frames.len())).family(semi()).size(14.0));
                let [l, t, r, b] = s.frames[i].crop;
                let (w, h) = ((1.0 - l - r) * s.tex.size()[0] as f32, (1.0 - t - b) * s.tex.size()[1] as f32);
                ui.label(RichText::new(format!("{:.2} : 1{}", w.max(h) / w.min(h).max(1.0), if s.frames[i].skip { " · skipped" } else { "" })).size(12.0).color(TEXT2));
            });
        });
        ui.add_space(4.0);
        ui.label(caps("1 · Frame"));
        ui.columns(4, |c| {
            let w = c[0].available_width();
            let fr = &mut s.frames[i];
            if c[0].add(egui::Button::new("Left").min_size(vec2(w, 28.0))).on_hover_text("Turn left").clicked() {
                fr.turns = (fr.turns + 3) % 4;
            }
            if c[1].add(egui::Button::new("Right").min_size(vec2(w, 28.0))).on_hover_text("Turn right").clicked() {
                fr.turns = (fr.turns + 1) % 4;
            }
            if c[2].add(egui::Button::new(RichText::new("Flip").color(if fr.flip { INK } else { TEXT })).fill(if fr.flip { BONE } else { Color32::TRANSPARENT }).min_size(vec2(w, 28.0))).on_hover_text("Scanned emulsion side up: mirror it").clicked() {
                fr.flip = !fr.flip;
            }
            if c[3].add(egui::Button::new(RichText::new("Skip").color(if fr.skip { INK } else { TEXT })).fill(if fr.skip { BONE } else { Color32::TRANSPARENT }).min_size(vec2(w, 28.0))).on_hover_text("Leave this frame out (blank, fogged, leader)").clicked() {
                fr.skip = !fr.skip;
            }
        });
        ui.horizontal(|ui| {
            if ui.add_enabled(s.frames.len() > 1, egui::Button::new("Same for all frames")).on_hover_text("Give every frame this frame's turn and flip").clicked() {
                let (t, f) = (s.frames[i].turns, s.frames[i].flip);
                for fr in &mut s.frames {
                    (fr.turns, fr.flip) = (t, f);
                }
            }
        });
        slider(ui, "Edge trim", &mut s.trim, 0.0, 5.0, 0.0, None);
        ui.add_space(4.0);

        ui.label(caps("2 · Film base"));
        egui::Frame::new().fill(CARD).corner_radius(8).inner_margin(10.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let base = s.gaps.get(s.gap).map(|g| g.1).or(self.params.neg.base).or(auto_base);
                let (r, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                ui.painter().rect_filled(r, 4.0, base.map(lin).unwrap_or(hex(0x3A3A40)));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let what = match s.gaps.get(s.gap) { Some((k, _)) => format!("Gap between {} and {}", k + 1, k + 2), None if self.params.neg.base.is_some() => "From a blank-film shot".into(), None => "Auto · brightest area".into() };
                    ui.label(RichText::new(what).size(12.0));
                    let rgb = base.map(|b| b.map(|v| (linear_to_srgb(v) * 255.0) as u8)).map(|c| format!("R {}  G {}  B {}", c[0], c[1], c[2])).unwrap_or("—".into());
                    ui.label(RichText::new(rgb).monospace().size(11.0).color(TEXT2));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if s.gaps.len() > 1 && ui.add(egui::Button::new(RichText::new("Next gap").size(11.5)).min_size(vec2(0.0, 26.0))).on_hover_text("Take the film base from another gap").clicked() {
                        s.gap = (s.gap + 1) % s.gaps.len();
                    }
                });
            });
        });
        let mut from_file = false;
        if ui.button("Blank-film shot…").on_hover_text("Use a scan of unexposed film (the leader) instead").clicked() {
            from_file = true;
        }
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label(caps("3 · Dust"));
            if let Some(d) = &s.dust {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(format!("{} on this frame", d.get(i).map_or(0, |v| v.len()))).monospace().size(11.0).color(TEXT3));
                });
            }
        });
        let mut find_dust = false;
        let mut ir_line = None;
        let has_ir = s.has_ir.or(self.photo.as_ref().and_then(|p| p.has_ir));
        if has_ir == Some(true) {
            // The scan carries an infrared channel: clean with it (Digital ICE style), no marks needed.
            slider(ui, "Infrared clean", &mut self.params.ir_clean, 0.0, 100.0, 0.0, None);
            if self.photo.as_ref().is_some_and(|p| p.ir_busy) {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Cleaning with the infrared channel…").size(12.0).color(TEXT2));
                });
            }
        } else if let Some(b) = &busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(b).size(12.0).color(TEXT2));
            });
        } else if s.dust.is_none() {
            find_dust = ui.add(egui::Button::new("Find dust on every frame").min_size(vec2(ui.available_width(), 28.0))).on_hover_text("Marks specks for healing when the frames are added").clicked();
        } else if ghost(ui, "Clear dust marks", false).clicked() {
            s.dust = None;
        }
        if s.dust.is_some() {
            ir_line = Some("Marked specks are healed when the frames are added. Right-click one in Develop's Heal tool if it isn't dust.");
        } else if has_ir == Some(false) {
            ir_line = Some("No infrared channel in this scan. Save 64-bit RGBI TIFFs (VueScan, SilverFast) to clean with it.");
        }
        if let Some(l) = ir_line {
            ui.label(RichText::new(l).size(11.5).color(TEXT2));
        }
        ui.add_space(4.0);

        ui.label(caps("4 · Convert"));
        let np = &mut self.params.neg;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Convert negative").size(12.0).color(LABEL));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| switch(ui, &mut np.enabled));
        });
        let st = negative::stock(&np.stock);
        ui.label(RichText::new(format!("{} {} · change it in Negative", st.brand, st.name)).size(12.0).color(TEXT2));
        let w = (ui.available_width() - 4.0) / 2.0;
        for row in [[(Look::Neutral, "Neutral"), (Look::Frontier, "Frontier")], [(Look::Noritsu, "Noritsu"), (Look::Linear, "Linear")]] {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (l, n) in row {
                    if tile(ui, n, np.enabled && np.look == l, w).clicked() {
                        np.look = l;
                        np.enabled = true;
                    }
                }
            });
        }
        if from_file
            && let Some(f) = rfd::FileDialog::new().set_title("Scan of blank film").pick_file()
        {
            match base_from_file(&f, self.params.profile.as_ref()) {
                Ok(b) => {
                    s.gaps.clear();
                    self.params.neg.base = Some(b);
                    self.params.neg.levels = None;
                    self.status = format!("Film base taken from {}", f.file_name().unwrap_or_default().to_string_lossy());
                }
                Err(e) => self.status = format!("Could not open: {e}"),
            }
        }
        if find_dust {
            self.strip_find_dust();
        }
    }

    fn light_table_footer(&mut self, ui: &mut Ui) {
        let Some(s) = &self.scan.strip else {
            ui.add_enabled(false, egui::Button::new("Add to roll").min_size(vec2(ui.available_width(), 32.0)));
            return;
        };
        let kept: Vec<usize> = (0..s.frames.len()).filter(|i| !s.frames[*i].skip).collect();
        let key = key_of(&self.params, (s.frames.iter().map(|f| (f.crop.map(f32::to_bits), f.skip, f.turns, f.flip)).collect::<Vec<_>>(), s.trim.to_bits(), s.gap, s.dust.is_some()));
        let done = s.added == Some(key);
        let label = match (done, kept.len()) {
            (true, _) => "Added to roll".to_string(),
            (_, 1) => "Add 1 frame to roll".to_string(),
            (_, n) => format!("Add {n} frames to roll"),
        };
        let mut add = false;
        ui.columns(2, |c| {
            if c[0].add(egui::Button::new("Find again").min_size(vec2(c[0].available_width(), 32.0))).on_hover_text("Forget changes and look for the frames again").clicked() {
                self.scan.strip = None;
            }
            add = c[1].add_enabled(!kept.is_empty() && !done, egui::Button::new(RichText::new(label).color(INK).family(semi())).fill(BONE).stroke(Stroke::NONE).min_size(vec2(c[1].available_width(), 32.0)))
                .on_hover_text("The first frame stays on this scan; the others become virtual copies, each its own frame in the roll").clicked();
        });
        if add {
            self.strip_add(kept, key);
        }
    }

    /// Make the frames: the first kept one on this scan, the rest as virtual copies of it.
    fn strip_add(&mut self, kept: Vec<usize>, key: u64) {
        let Some(cur) = self.current.clone() else { return };
        let mut all: Vec<Params> = kept.iter().map(|i| self.strip_frame_params(*i, true)).collect();
        if all.is_empty() {
            return;
        }
        // Balance every frame the same way: from the first frame, with the film base of the gap.
        if all[0].neg.enabled
            && let Some(ph) = &self.photo
        {
            let p0 = &all[0];
            let geo = develop::geometry(&ph.preview, &Params { crop: p0.crop, rot90: p0.rot90, flip_h: p0.flip_h, neg: p0.neg.clone(), profile: p0.profile.clone(), ..Default::default() });
            let (b, l) = negative::analyse(&geo, &p0.neg);
            let base = p0.neg.base.unwrap_or(b);
            for p in &mut all {
                p.neg.base = Some(base);
                p.neg.levels = Some(l);
            }
        }
        // An earlier Add on this scan made copies: they are replaced, not added to.
        let old: Vec<PathBuf> = self.files.iter().filter(|f| load::copy_of(f).as_deref() == Some(cur.as_path())).cloned().collect();
        if self.scan.strip.as_ref().is_some_and(|s| s.added.is_some()) {
            for f in old {
                self.remove_copy(&f);
            }
        }
        let n = all.len();
        let first = all.remove(0);
        let dust: usize = first.spots.len() + all.iter().map(|p| p.spots.len()).sum::<usize>();
        self.params = Params { rating: self.params.rating, flag: self.params.flag, label: self.params.label, snapshots: std::mem::take(&mut self.params.snapshots), ..first };
        let made: Vec<PathBuf> = all.iter().filter_map(|p| self.virtual_copy(&cur, p)).collect();
        // Copies go in frame order (each is inserted next to the scan, which would reverse them).
        self.files.sort();
        self.commit();
        // Every frame needs its own thumbnail: copies start with the whole strip's, and the scan's has changed.
        for f in made.iter().chain(std::iter::once(&cur)) {
            self.thumbs.remove(f);
            self.thumb_ready.insert(f.clone());
        }
        if let Some(s) = &mut self.scan.strip {
            s.added = Some(key);
        }
        self.status = format!("Added {n} frame{} to the roll{}", if n == 1 { "" } else { "s" }, if dust > 0 { format!(", {dust} specks of dust healed") } else { String::new() });
    }

    // ---------- Capture ----------

    fn capture_left(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 10.0;
        ui.label(caps("Source"));
        ui.label(RichText::new("Shoot each frame on a light source with your camera, saving or tethering into one folder. Ferrotype opens each new frame as it lands.").size(12.0).color(TEXT2));
        let name = self.folder.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "No folder".into());
        ui.label(RichText::new(name).monospace().size(11.0).color(TEXT3));
        if ui.add(egui::Button::new("Choose capture folder…").min_size(vec2(ui.available_width(), 30.0))).clicked()
            && let Some(d) = rfd::FileDialog::new().pick_folder()
        {
            self.open_path(ui.ctx(), &d);
            (self.watch, self.watch_open) = (true, true);
        }
        switch_row(ui, "Watch for new frames", &mut self.watch);
        switch_row(ui, "New frames get this conversion", &mut self.watch_apply).on_hover_text("Each new frame starts with the open frame's film base, stock and look");
        ui.horizontal(|ui| {
            ui.label(RichText::new("Roll length").size(12.0).color(LABEL));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Laid out right to left, so listed backwards: it reads 12 24 36 72.
                let names: Vec<String> = ROLL_LENGTHS.iter().rev().map(|n| n.to_string()).collect();
                let names: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                let mut i = ROLL_LENGTHS.len() - 1 - self.scan.roll_len;
                segmented(ui, &mut i, &names);
                self.scan.roll_len = ROLL_LENGTHS.len() - 1 - i;
            });
        });
        ui.add_space(8.0);
        ui.label(caps("Before the roll"));
        let step = |ui: &mut Ui, done: bool, text: &str, action: &str| -> bool {
            let mut hit = false;
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                if done {
                    ui.painter().circle_filled(r.center(), 7.0, BONE);
                } else {
                    ui.painter().circle_stroke(r.center(), 6.0, Stroke::new(1.5, BTN_BORDER));
                }
                ui.label(RichText::new(text).size(12.5));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| hit = small_action(ui, action, false).clicked());
            });
            hit
        };
        let has_base = self.params.neg.base.is_some();
        if step(ui, has_base, "Film base", if has_base { "Again" } else { "Shoot leader…" }) {
            self.scan_base_from_shot();
        }
        let focus = self.scan.focus_checked;
        if step(ui, focus, "Focus on the grain", "Check") && self.photo.is_some() {
            self.scan.focus_checked = true;
            self.toggle_zoom([0.5, 0.5]);
        }
        let conv = self.params.neg.enabled;
        if step(ui, conv, "Show as positive", if conv { "Off" } else { "On" }) {
            self.params.neg.enabled = !conv;
        }
        ui.add_space(8.0);
        self.roll_notes(ui);
    }

    fn capture_right(&mut self, ui: &mut Ui) {
        let Some(cur) = self.current.clone() else {
            ui.label(RichText::new("The last frame you shot, with a few quick checks, shows here.").color(TEXT3));
            return;
        };
        // Checks for a newly opened frame, worked out off the UI thread.
        if self.scan.checks.as_ref().is_none_or(|(f, _)| *f != cur)
            && let Some(ph) = &self.photo
        {
            self.scan.checks = Some((cur.clone(), None));
            let (tx, ctx, preview, full, p, f) = (self.scan.tx.clone(), ui.ctx().clone(), ph.preview.clone(), ph.full.clone(), self.params.clone(), cur.clone());
            std::thread::spawn(move || {
                let c = check_frame(&preview, &full, &p);
                let _ = tx.send(ScanMsg::Checks(f, c));
                ctx.request_repaint();
            });
        }
        let n = self.files.iter().position(|f| *f == cur).map_or(0, |i| i + 1);
        ui.horizontal(|ui| {
            ui.label(caps(&format!("Last frame · {n}")));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ghost(ui, "Open in Develop", false).clicked() {
                    self.module = Module::Develop;
                }
            });
        });
        ui.label(RichText::new(cur.file_name().unwrap_or_default().to_string_lossy()).monospace().size(11.0).color(TEXT3));
        match self.scan.checks.as_ref().and_then(|(_, c)| c.clone()) {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Checking…").color(TEXT2));
                });
            }
            Some(c) => {
                let dust = match c.dust { Some(0) => (true, "None found".to_string()), Some(d) => (d < 40, format!("{d} specks: heal them in Roll sheet or Develop")), None => (true, "—".into()) };
                for (name, (ok, text)) in [("Exposure", c.exposure), ("Sharpness", c.sharp), ("Framing", c.framing), ("Dust", dust)] {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(12.0, 16.0), Sense::hover());
                        if ok {
                            ui.painter().circle_filled(r.center(), 4.0, BONE);
                        } else {
                            ui.painter().circle_stroke(r.center(), 4.5, Stroke::new(1.5, WARN));
                        }
                        ui.add_sized(vec2(72.0, 16.0), egui::Label::new(RichText::new(name).size(12.5).color(TEXT2)));
                        ui.add(egui::Label::new(RichText::new(text).size(12.5).color(if ok { TEXT } else { WARN })).wrap());
                    });
                }
            }
        }
        ui.add_space(6.0);
        let rejected = self.params.flag < 0;
        if ui.add(egui::Button::new(if rejected { "Keep this frame" } else { "Retake: skip this frame" }).min_size(vec2(ui.available_width(), 30.0))).on_hover_text("Marks it rejected so it leaves the count; shoot the frame again").clicked() {
            self.params.flag = if rejected { 0 } else { -1 };
        }
    }

    fn capture_footer(&mut self, ui: &mut Ui) {
        if ui.add(egui::Button::new(RichText::new("Finish roll · Roll sheet").color(INK).family(semi())).fill(BONE).stroke(Stroke::NONE).min_size(vec2(ui.available_width(), 32.0))).clicked() {
            self.watch = false;
            self.scan.page = 2;
        }
    }

    /// One slot per frame of the roll: shot ones show their picture, the next one is outlined.
    fn roll_counter(&mut self, ui: &mut Ui) {
        let frames = self.roll_frames();
        let len = ROLL_LENGTHS[self.scan.roll_len].max(frames.len() + 1);
        ui.horizontal(|ui| {
            ui.label(caps(&format!("Roll · {} of {}", frames.len(), ROLL_LENGTHS[self.scan.roll_len])));
        });
        let avail = ui.available_rect_before_wrap();
        let gap = 3.0;
        let w = ((avail.width() + gap) / len as f32 - gap).max(4.0);
        let h = avail.height().min(46.0);
        let mut open = None;
        for i in 0..len {
            let r = Rect::from_min_size(pos2(avail.left() + i as f32 * (w + gap), avail.top() + 4.0), vec2(w, h));
            let id = ui.scope_id().with(("slot", i));
            match frames.get(i) {
                Some(f) => {
                    let resp = ui.interact(r, id, Sense::click());
                    match self.thumb(f) {
                        Some(t) => egui::Image::new(t).uv(cover_uv(t, w / h)).corner_radius(2).paint_at(ui, r),
                        None => {
                            ui.painter().rect_filled(r, 2.0, CARD);
                        }
                    }
                    if self.current.as_ref() == Some(f) {
                        ui.painter().rect_stroke(r, 2.0, Stroke::new(2.0, BONE), egui::StrokeKind::Outside);
                    }
                    if resp.on_hover_text(format!("Frame {}", i + 1)).clicked() {
                        open = Some(f.clone());
                    }
                }
                None => {
                    let next = i == frames.len();
                    ui.painter().rect_stroke(r, 2.0, Stroke::new(if next { 2.0 } else { 1.0 }, if next { BONE } else { BORDER }), egui::StrokeKind::Inside);
                    if w >= 14.0 {
                        ui.painter().text(pos2(r.center().x, r.bottom() - 7.0), Align2::CENTER_CENTER, format!("{}", i + 1), FontId::monospace(9.0), if next { BONE } else { TEXT3 });
                    }
                }
            }
        }
        if let Some(f) = open {
            self.select(ui.ctx(), f);
        }
    }

    // ---------- Roll sheet ----------

    /// Read what the roll sheet shows of each frame, once per folder, off the UI thread.
    fn ensure_info(&mut self, ctx: &egui::Context) {
        let key = self.folder.clone().map(|d| (d, self.files.len()));
        if key == self.scan.info_folder {
            return;
        }
        self.scan.info_folder = key;
        let (tx, ctx, files) = (self.scan.tx.clone(), ctx.clone(), self.files.clone());
        std::thread::spawn(move || {
            let v: Vec<(PathBuf, FrameInfo)> = files.par_iter().map(|f| (f.clone(), read_params(f).map(|p| FrameInfo::of(&p)).unwrap_or_default())).collect();
            let _ = tx.send(ScanMsg::Info(v));
            ctx.request_repaint();
        });
    }

    fn roll_grid(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if self.files.is_empty() {
            self.scan_empty(ui, "No roll open", "Open a folder of scans to see the whole roll at once.");
            return;
        }
        self.ensure_info(&ctx);
        let cols = ((ui.available_width() + 12.0) / 170.0).floor().max(2.0) as usize;
        let w = (ui.available_width() - 12.0 * (cols - 1) as f32) / cols as f32;
        let th = w / 1.5;
        let row_h = th + 30.0;
        let n = self.files.len();
        let rows = n.div_ceil(cols);
        let (mut click, mut open) = (None, None);
        let mods = ui.input(|i| i.modifiers);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, rows, |ui, range| {
            for row in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    for c in 0..cols {
                        let i = row * cols + c;
                        if i >= n {
                            break;
                        }
                        let f = self.files[i].clone();
                        let (cell, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click());
                        let r = Rect::from_min_size(cell.min, vec2(w, th));
                        let m = self.meta.get(&f).copied().unwrap_or_default();
                        let info = self.scan.info.get(&f).copied().unwrap_or_default();
                        let skipped = m.flag < 0;
                        match self.thumb(&f) {
                            // Whole frames, so a turned (upright) one shows all of itself.
                            Some(t) => {
                                let ts = t.size_vec2();
                                let k = (r.width() / ts.x).min(r.height() / ts.y);
                                egui::Image::new(t).corner_radius(2).paint_at(ui, Rect::from_center_size(r.center(), ts * k));
                            }
                            None => {
                                ui.painter().rect_filled(r, 2.0, CARD);
                            }
                        }
                        if skipped {
                            ui.painter().rect_filled(r, 2.0, Color32::from_black_alpha(170));
                        }
                        let on = self.scan.sel.contains(&f);
                        if on {
                            ui.painter().rect_stroke(r.expand(3.0), 3.0, Stroke::new(2.0, BONE), egui::StrokeKind::Outside);
                        } else if resp.hovered() {
                            ui.painter().rect_stroke(r.expand(1.0), 3.0, Stroke::new(1.0, BTN_BORDER), egui::StrokeKind::Outside);
                        }
                        // Chips: what was done to the frame.
                        let mut chips = vec![];
                        if skipped {
                            chips.push("skipped".to_string());
                        } else {
                            if info.dust > 0 {
                                chips.push(format!("{} dust", info.dust));
                            }
                            if info.turned {
                                chips.push("turned".into());
                            }
                            if info.flipped {
                                chips.push("flipped".into());
                            }
                        }
                        let mut x = r.left() + 5.0;
                        for ch in chips {
                            let g = ui.painter().layout_no_wrap(ch, FontId::monospace(10.0), BONE);
                            let b = Rect::from_min_size(pos2(x, r.bottom() - 5.0 - g.size().y - 2.0), g.size() + vec2(8.0, 2.0));
                            if b.right() > r.right() - 4.0 {
                                break;
                            }
                            ui.painter().rect_filled(b, 3.0, Color32::from_rgba_unmultiplied(20, 20, 22, 215));
                            ui.painter().galley(b.min + vec2(4.0, 1.0), g, BONE);
                            x = b.right() + 4.0;
                        }
                        let y = r.bottom() + 14.0;
                        ui.painter().text(pos2(r.left(), y), Align2::LEFT_CENTER, format!("{:02}", i + 1), FontId::monospace(11.5), if on { TEXT } else { TEXT2 });
                        let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
                        let name = if name.chars().count() > 22 { format!("…{}", name.chars().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect::<String>()) } else { name };
                        ui.painter().text(pos2(r.right(), y), Align2::RIGHT_CENTER, name, FontId::proportional(11.0), TEXT3);
                        if resp.double_clicked() {
                            open = Some(f.clone());
                        } else if resp.clicked() {
                            click = Some(i);
                        }
                    }
                });
            }
        });
        if let Some(i) = click {
            let f = self.files[i].clone();
            self.lib_sel = Some(f.clone());
            if mods.shift && let Some(a) = self.scan.anchor {
                let (lo, hi) = (a.min(i), a.max(i).min(n - 1));
                self.scan.sel = self.files[lo..=hi].to_vec();
            } else if mods.command {
                if let Some(k) = self.scan.sel.iter().position(|x| *x == f) {
                    self.scan.sel.remove(k);
                } else {
                    self.scan.sel.push(f);
                }
                self.scan.anchor = Some(i);
            } else {
                self.scan.sel = vec![f];
                self.scan.anchor = Some(i);
            }
        }
        if let Some(f) = open {
            self.select(&ctx, f);
            self.module = Module::Negative;
        }
    }

    /// Change every selected frame.
    fn roll_each(&mut self, change: impl Fn(&mut Params)) {
        for f in self.scan.sel.clone() {
            self.edit_frame(&f, &change);
        }
    }

    /// Run `job` on each frame in `files` off the UI thread; its edits are applied when all are done.
    fn roll_job(&mut self, what: &str, files: Vec<PathBuf>, job: impl Fn(&Path, &Params) -> Option<Edit> + Send + Sync + 'static, say: impl Fn(usize) -> String + Send + 'static) {
        if self.scan.busy.is_some() || files.is_empty() {
            return;
        }
        // The open frame's edits may not be saved yet: hand the job the ones in memory.
        let cur = self.current.clone().map(|c| (c, self.params.clone()));
        self.scan.busy = Some(what.to_string());
        let tx = self.scan.tx.clone();
        std::thread::spawn(move || {
            let edits: Vec<(PathBuf, Edit)> = files.par_iter().filter_map(|f| {
                let p = match &cur { Some((c, p)) if c == f => p.clone(), _ => read_params(f).unwrap_or_default() };
                job(f, &p).map(|e| (f.clone(), e))
            }).collect();
            let n = edits.len();
            let _ = tx.send(ScanMsg::Edits(edits, say(n)));
        });
    }

    fn roll_find_frames(&mut self) {
        let files = self.scan.sel.clone();
        self.roll_job("Finding frame edges…", files, |f, p| {
            let img = load::load_quick(f, 1400).ok()?;
            scan::auto_crop(&scan::frame(&img, p), p.neg.enabled).map(Edit::Crop)
        }, |n| format!("Cropped {n} frame{} to their edges", if n == 1 { "" } else { "s" }));
    }

    fn roll_left(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 10.0;
        let title = match self.roll_name() {
            n if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => format!("Roll {n}"),
            n if n.is_empty() => "Roll".into(),
            n => n,
        };
        ui.label(RichText::new(title).family(display()).size(22.0).color(BONE).extra_letter_spacing(-0.2));
        self.roll_notes(ui);
    }

    fn roll_right(&mut self, ui: &mut Ui) {
        let kept = self.roll_frames().len();
        let skipped = self.files.len() - kept;
        let dust: usize = self.files.iter().filter_map(|f| self.scan.info.get(f)).map(|i| i.dust).sum();
        ui.columns(3, |c| {
            for (k, (v, l)) in [(kept, "kept"), (skipped, "skipped"), (dust, "dust spots")].into_iter().enumerate() {
                c[k].label(RichText::new(v.to_string()).family(mono500()).size(18.0));
                c[k].label(RichText::new(l).size(12.0).color(TEXT3));
            }
        });
        if let Some(b) = self.scan.busy.clone() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(b).size(12.0).color(TEXT2));
            });
        }
        let idle = self.scan.busy.is_none();
        ui.add_space(4.0);
        ui.label(caps("1 · Film base"));
        ui.label(RichText::new("One film base for every frame, from a scan of the unexposed leader.").size(12.0).color(TEXT2));
        let mut base_file = None;
        if ui.add_enabled(idle, egui::Button::new("Blank-film shot for the roll…").min_size(vec2(ui.available_width(), 28.0))).clicked() {
            base_file = rfd::FileDialog::new().set_title("Scan of blank film").pick_file();
        }
        ui.add_space(4.0);

        ui.label(caps("2 · Balance"));
        let one = self.scan.sel.len() == 1;
        let match_roll = ui.add_enabled(idle && one, egui::Button::new("Match roll to selected frame").min_size(vec2(ui.available_width(), 28.0)))
            .on_hover_text("Lock every frame's film base and levels to this frame's, so the whole roll prints alike")
            .on_disabled_hover_text("Select one well-exposed frame first").clicked();
        let even = ui.add_enabled(idle, egui::Button::new("Even out exposure").min_size(vec2(ui.available_width(), 28.0)))
            .on_hover_text("Set each converted frame's Density so its brightness matches the roll's middle").clicked();
        ui.add_space(4.0);

        ui.label(caps("3 · Dust"));
        let was = !self.files.is_empty() && self.files.iter().all(|f| self.scan.info.get(f).is_some_and(|i| i.ir));
        let mut ir_all = was;
        switch_row(ui, "Infrared clean, all frames", &mut ir_all).on_hover_text("For scans saved with their infrared channel (64-bit RGBI TIFF). Others are left as they are.");
        let dust_all = ui.add_enabled(idle, egui::Button::new("Find dust on every frame").min_size(vec2(ui.available_width(), 28.0)))
            .on_hover_text("Marks specks for healing, for scans without infrared. Frames that already have heal spots are left alone").clicked();
        ui.add_space(4.0);

        ui.label(caps("4 · File names"));
        let rename = ui.add_enabled(!self.files.is_empty(), egui::Button::new("Rename roll…").min_size(vec2(ui.available_width(), 28.0))).on_hover_text("Name every frame from a pattern, e.g. roll name and frame number").clicked();

        if ir_all != was {
            let v = if ir_all { 60.0 } else { 0.0 };
            for f in self.files.clone() {
                self.edit_frame(&f, |p| p.ir_clean = v);
            }
            self.status = if ir_all { "Infrared clean on for the roll (scans with an infrared channel)".into() } else { "Infrared clean off for the roll".into() };
        }
        if let Some(f) = base_file {
            match base_from_file(&f, self.params.profile.as_ref()) {
                Ok(b) => {
                    for g in self.files.clone() {
                        self.edit_frame(&g, |p| {
                            p.neg.enabled = true;
                            p.neg.base = Some(b);
                            p.neg.levels = None;
                        });
                    }
                    self.status = format!("Film base from {} set on {} frames", f.file_name().unwrap_or_default().to_string_lossy(), self.files.len());
                }
                Err(e) => self.status = format!("Could not open: {e}"),
            }
        }
        if match_roll {
            let src = self.scan.sel[0].clone();
            let p0 = if self.current.as_ref() == Some(&src) { self.params.clone() } else { read_params(&src).unwrap_or_default() };
            match load::load_quick(&src, 1400) {
                Ok(img) => {
                    let mut np = p0.neg.clone();
                    np.enabled = true;
                    np.levels = None;
                    let geo = develop::geometry(&img, &Params { crop: p0.crop, rot90: p0.rot90, flip_h: p0.flip_h, neg: np.clone(), profile: p0.profile.clone(), ..Default::default() });
                    let (b, l) = negative::analyse(&geo, &np);
                    let b = np.base.unwrap_or(b);
                    let files = self.roll_frames();
                    self.roll_job("Matching the roll…", files, move |_, _| Some(Edit::Balance(b, l)), |n| format!("Matched {n} frames to the selected one"));
                }
                Err(e) => self.status = format!("Could not open: {e}"),
            }
        }
        if even {
            self.roll_even_exposure();
        }
        if dust_all {
            let files = self.roll_frames();
            self.roll_job("Looking for dust…", files, |f, p| {
                if !p.spots.is_empty() {
                    return None;
                }
                let img = load::load_quick(f, 3000).ok()?;
                let found = scan::find_dust(&scan::frame(&img, p), develop::crop_of(p), 150);
                (!found.is_empty()).then_some(Edit::Spots(found))
            }, |n| format!("Marked dust on {n} frames: check them with Develop's Heal tool"));
        }
        if rename {
            let roll = self.roll_name();
            let files: Vec<PathBuf> = self.files.iter().filter(|f| load::copy_of(f).is_none()).cloned().collect();
            self.rename_dlg = Some(RenameDlg { files, pattern: "{roll}-{n}".into(), roll, preview: vec![], previewed: Default::default() });
        }
    }

    /// Density per converted frame so its median brightness matches the roll's. Bisects Density on a small render.
    fn roll_even_exposure(&mut self) {
        let files: Vec<PathBuf> = self.roll_frames().into_iter().filter(|f| self.meta.get(f).is_some_and(|m| m.neg) || (self.current.as_ref() == Some(f) && self.params.neg.enabled)).collect();
        if files.len() < 2 {
            self.status = "Convert the roll first (at least two frames)".into();
            return;
        }
        let cur = self.current.clone().map(|c| (c, self.params.clone()));
        let tx = self.scan.tx.clone();
        self.scan.busy = Some("Evening out exposure…".into());
        std::thread::spawn(move || {
            let small: Vec<(PathBuf, Rgb32FImage, Params)> = files.par_iter().filter_map(|f| {
                let p = match &cur { Some((c, p)) if c == f => p.clone(), _ => read_params(f).unwrap_or_default() };
                Some((f.clone(), load::load_quick(f, 360).ok()?, p))
            }).collect();
            // Median brightness of the centre of the rendered frame.
            let level = |img: &Rgb32FImage, p: &Params, d: f32| -> f32 {
                let q = Params { neg: negative::NegParams { density: d, ..p.neg.clone() }, spots: vec![], ..p.clone() };
                let out = develop::render(img, &q, 0.1);
                let (w, h) = out.dimensions();
                let mut v: Vec<f32> = (h / 5..h * 4 / 5).flat_map(|y| (w / 5..w * 4 / 5).map(move |x| (x, y))).map(|(x, y)| luma(out.get_pixel(x, y).0)).collect();
                v.sort_by(f32::total_cmp);
                v.get(v.len() / 2).copied().unwrap_or(0.5)
            };
            let now: Vec<f32> = small.par_iter().map(|(_, img, p)| level(img, p, p.neg.density)).collect();
            let mut sorted = now.clone();
            sorted.sort_by(f32::total_cmp);
            // The roll's middle, kept within about a stop of mid grey: a roll mostly over- or underexposed
            // should come towards a normal print, not pull its few good frames along.
            let target = sorted[sorted.len() / 2].clamp(0.38, 0.54); // encoded: mid grey is 0.46
            let edits: Vec<(PathBuf, Edit)> = small.par_iter().zip(&now).filter(|(_, l)| (**l - target).abs() > 0.02).filter_map(|((f, img, p), _)| {
                // Brightness falls as Density rises.
                let (mut lo, mut hi) = (-100.0f32, 100.0f32);
                for _ in 0..8 {
                    let mid = (lo + hi) / 2.0;
                    if level(img, p, mid) > target { lo = mid } else { hi = mid }
                }
                let d = ((lo + hi) / 2.0).round();
                ((d - p.neg.density).abs() >= 1.0).then_some((f.clone(), Edit::Density(d)))
            }).collect();
            let n = edits.len();
            let _ = tx.send(ScanMsg::Edits(edits, if n == 0 { "The roll is already even".into() } else { format!("Evened out {n} frames") }));
        });
    }

    fn roll_footer(&mut self, ui: &mut Ui) {
        ui.columns(2, |c| {
            if c[0].add_enabled(!self.files.is_empty(), egui::Button::new("Contact sheet").min_size(vec2(c[0].available_width(), 32.0))).clicked() {
                self.make_contact_sheet();
            }
            if c[1].add(egui::Button::new(RichText::new("Open in Library").color(INK).family(semi())).fill(BONE).stroke(Stroke::NONE).min_size(vec2(c[1].available_width(), 32.0))).clicked() {
                self.module = Module::Library;
            }
        });
    }

    /// The roll's name from its notes, else the folder's.
    fn roll_name(&self) -> String {
        let notes = (self.roll_dir == self.folder).then(|| self.roll.roll.trim().to_string()).filter(|r| !r.is_empty());
        notes.or_else(|| self.folder.as_ref().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().to_string())).unwrap_or_default()
    }

    /// The roll's notes (camera, film, development, and the selected frame's), the same panel as Library's.
    fn roll_notes(&mut self, ui: &mut Ui) {
        if self.folder.is_some() {
            self.roll_panel(ui);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_twice_is_no_change() {
        let mut p = Params { crop: [0.1, 0.2, 0.05, 0.0], spots: vec![Spot { x: 0.3, y: 0.4, sx: 0.2, sy: 0.4, r: 0.01, ..Default::default() }], ..Default::default() };
        let orig = p.clone();
        flip_frame(&mut p);
        assert_eq!(p.crop, [0.05, 0.2, 0.1, 0.0]);
        assert!((p.spots[0].x - 0.7).abs() < 1e-6 && p.flip_h);
        flip_frame(&mut p);
        assert_eq!(p.crop, orig.crop);
        assert!(!p.flip_h && (p.spots[0].x - 0.3).abs() < 1e-6 && (p.spots[0].sx - 0.2).abs() < 1e-6);
    }

    #[test]
    fn dust_lands_where_it_was_found() {
        // The second of three frames on a 3:1 strip; a speck in the frame's centre is at the strip's centre.
        let crop = [1.0 / 3.0, 0.1, 1.0 / 3.0, 0.1];
        let found = [Spot { x: 0.5, y: 0.5, sx: 0.6, sy: 0.5, r: 0.01, ..Default::default() }];
        let s = spots_in_scan(crop, 3.0, &found);
        assert!((s[0].x - 0.5).abs() < 1e-6 && (s[0].y - 0.5).abs() < 1e-6, "{:?}", s[0]);
        assert!((s[0].sx - (1.0 / 3.0 + 0.6 / 3.0)).abs() < 1e-6);
        // 1% of the frame's long side (1 unit of height) is a third of that on the strip's (3 units).
        assert!((s[0].r - 0.01 / 3.0).abs() < 1e-6, "{}", s[0].r);
    }

    #[test]
    fn film_base_comes_from_the_gaps() {
        let base = [0.8f32, 0.45, 0.2];
        // Three frames of dark picture with gaps of base between them.
        let img = Rgb32FImage::from_fn(320, 100, |x, _| {
            let in_frame = (x % 110) < 100;
            image::Rgb(if in_frame { [0.1, 0.08, 0.05] } else { base })
        });
        let frames: Vec<[f32; 4]> = (0..3).map(|k| {
            let (a, b) = (k as f32 * 110.0, k as f32 * 110.0 + 100.0);
            [a / 320.0, 0.05, 1.0 - b / 320.0, 0.05]
        }).collect();
        let g = gap_bases(&img, &frames);
        assert_eq!(g.len(), 2);
        for (_, c) in g {
            assert!((0..3).all(|k| (c[k] - base[k]).abs() < 1e-4), "{c:?}");
        }
    }

    #[test]
    fn checks_catch_clipping() {
        let flat = Rgb32FImage::from_fn(200, 140, |x, y| image::Rgb(if x < 20 || y < 14 { [1.0, 1.0, 1.0] } else { [0.5, 0.3, 0.2] }));
        let c = check_frame(&flat, &flat, &Params::default());
        assert!(!c.exposure.0, "{}", c.exposure.1);
        let fine = Rgb32FImage::from_fn(200, 140, |x, y| { let v = 0.2 + 0.1 * ((x / 7 + y / 5) % 3) as f32; image::Rgb([v, v * 0.9, v * 0.8]) });
        assert!(check_frame(&fine, &fine, &Params::default()).exposure.0);
    }
}

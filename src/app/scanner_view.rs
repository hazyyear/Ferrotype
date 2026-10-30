//! The Scan view's Scanner page: Ferrotype drives the scanner itself, the way VueScan and SilverFast do. An Epson
//! V800/V850 goes straight over USB (`scanner::direct`), with its film holders drawn to scale; any scanner SANE
//! knows (Linux) goes through `scanimage`, with the frames found on a preview of its area. Either way: preview,
//! click the frames to scan, and they arrive in the roll folder as 16-bit TIFFs that open on the light table.
use super::*;
use crate::scanner::{self, direct};
use epscan::Source;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Straight over USB (epscan).
    Usb,
    /// Through SANE's `scanimage`.
    Sane,
}

/// A scanner on the list.
#[derive(Clone)]
struct Dev {
    /// The USB location, or SANE's device name.
    key: String,
    name: String,
    /// Why it can't be used this way, if it can't.
    blocked: Option<String>,
    kind: Kind,
}

enum Msg {
    Devices(Vec<Dev>, Option<String>),
    Identified(String, Result<direct::Scanner, String>),
    /// SANE options for (device, source).
    Options((String, String), Result<scanner::Options, String>),
    /// Progress of a preview; on this channel so it can never arrive after `Previewed`.
    Status(String),
    Progress(f32),
    Previewed(Result<Preview, String>),
}

struct Preview {
    path: PathBuf,
    /// What it shows: the scanner and source (`DirectUi::what`).
    what: String,
    /// SANE: the size of the area and the frames found on it, in millimetres.
    bed: Option<[f64; 2]>,
    found: Vec<[f64; 4]>,
    /// USB: the scanner as it answered.
    usb: Option<direct::Scanner>,
}

pub(super) struct DirectUi {
    devices: Option<Vec<Dev>>,
    /// Still looking for scanners.
    looking: bool,
    note: Option<String>,
    key: String,
    usb: Option<(String, Result<direct::Scanner, String>)>,
    sane_opts: Option<((String, String), Result<scanner::Options, String>)>,
    sane: scanner::Settings,
    /// Index into `direct::layouts` of the USB scanner's model.
    layout: usize,
    /// Selected frames, numbered from 1.
    frames: BTreeSet<u32>,
    dpi: u32,
    oversample: u32,
    linear: bool,
    infrared: bool,
    /// Frame size change in percent (+ adds room around the frame, − crops into it).
    overage: f32,
    /// Shift of every frame box, in millimetres (film never sits exactly where the holder was measured).
    nudge: [f32; 2],
    /// The preview: the file, its picture, what it shows, and (SANE) the area it covers.
    preview: Option<(PathBuf, TextureHandle, String, Option<[f64; 2]>)>,
    /// SANE: frames found on the preview, in millimetres.
    found: Vec<[f64; 4]>,
    show_positive: bool,
    busy: bool,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
}

impl Default for DirectUi {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        DirectUi {
            devices: None, looking: false, note: None, key: String::new(), usb: None, sane_opts: None,
            sane: scanner::Settings { depth: 16, resolution: 2400, passes: 1, linear: true, ..Default::default() },
            layout: 0, frames: BTreeSet::new(), dpi: 3200, oversample: 1, linear: true, infrared: true, overage: 4.0, nudge: [0.0; 2],
            preview: None, found: vec![], show_positive: true, busy: false, cancel: Arc::new(AtomicBool::new(false)), tx, rx,
        }
    }
}

impl DirectUi {
    fn dev(&self) -> Option<&Dev> {
        self.devices.as_ref()?.iter().find(|d| d.key == self.key)
    }

    fn kind(&self) -> Option<Kind> {
        self.dev().map(|d| d.kind)
    }

    fn usb_scanner(&self) -> Option<&direct::Scanner> {
        self.usb.as_ref().filter(|(l, _)| *l == self.key).and_then(|(_, s)| s.as_ref().ok())
    }

    fn sane_options(&self) -> Option<&scanner::Options> {
        self.sane_opts.as_ref().filter(|(k, _)| *k == (self.sane.device.clone(), self.sane.source.clone())).and_then(|(_, o)| o.as_ref().ok())
    }

    fn layouts(&self) -> Vec<(String, direct::HolderLayout)> {
        self.usb_scanner().map(|s| direct::layouts(s.model)).unwrap_or_default()
    }

    fn layout_now(&self) -> Option<direct::HolderLayout> {
        self.layouts().into_iter().nth(self.layout).map(|(_, l)| l)
    }

    /// What a preview must show to be of use now: this scanner and source.
    fn what(&self) -> String {
        match self.kind() {
            Some(Kind::Usb) => format!("{}|{:?}", self.key, self.layout_now().map_or(Source::Transparency, |l| l.source)),
            Some(Kind::Sane) => format!("{}|{}", self.key, self.sane.source),
            None => String::new(),
        }
    }

    /// The scanner's area for the chosen holder or source, in millimetres.
    fn area(&self) -> Option<[f64; 2]> {
        match self.kind()? {
            Kind::Usb => Some(self.usb_scanner()?.area(self.layout_now()?.source)),
            Kind::Sane => self.sane_options()?.bed_mm.map(|[w, h]| [w as f64, h as f64]),
        }
    }

    /// Whether the scanner is known and ready to preview or scan.
    fn ready(&self) -> bool {
        match self.kind() {
            Some(Kind::Usb) => self.usb_scanner().is_some() && self.layout_now().is_some(),
            Some(Kind::Sane) => self.sane_options().is_some_and(|o| o.bed_mm.is_some()),
            None => false,
        }
    }

    /// The frame boxes to show and scan (left, top, width, height in mm), with frame size and nudge: the
    /// holder's frames, or on SANE those found on the preview (the whole area until there is one).
    fn boxes(&self) -> Vec<[f64; 4]> {
        let o = self.overage as f64;
        let [nx, ny] = self.nudge.map(|v| v as f64);
        match self.kind() {
            Some(Kind::Usb) => {
                let Some(l) = self.layout_now() else { return vec![] };
                (1..=l.frames_mm.len() as u32).filter_map(|n| l.frame_rect(n, o).ok()).map(|r| [r[0] + nx, r[1] + ny, r[2], r[3]]).collect()
            }
            Some(Kind::Sane) => {
                let Some([aw, ah]) = self.area() else { return vec![] };
                let found = if self.found.is_empty() || !self.preview_fits() { vec![[0.0, 0.0, aw, ah]] } else { self.found.clone() };
                found.iter().map(|&[x, y, w, h]| {
                    let (gw, gh) = (w * o / 100.0, h * o / 100.0);
                    let (x0, y0) = ((x - gw / 2.0 + nx).max(0.0), (y - gh / 2.0 + ny).max(0.0));
                    [x0, y0, (w + gw).min(aw - x0).max(1.0), (h + gh).min(ah - y0).max(1.0)]
                }).collect()
            }
            None => vec![],
        }
    }

    /// The line next to the page tabs: how this scanner is driven.
    pub(super) fn subtitle(&self) -> &'static str {
        match self.kind() {
            Some(Kind::Usb) => "Over USB: ferrotype runs the scanner itself",
            Some(Kind::Sane) => "Through SANE, the Linux scanner driver",
            None => "ferrotype runs the scanner itself, over USB or through SANE",
        }
    }

    fn preview_fits(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| p.2 == self.what())
    }
}

/// `s` cut to `n` characters with an ellipsis (combo boxes don't shrink long names to fit the panel).
fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>().trim_end()) }
}

/// A preview scan as a picture to show: a negative shown as a rough positive (each channel stretched from its
/// film base), which is enough to see what is on each frame.
fn preview_image(path: &Path, positive: bool) -> Option<ColorImage> {
    let img = load::load(path).ok()?;
    let mut img = load::fit(&img, 1600);
    if positive {
        // Per channel: brightest (film base, after a few hot pixels) → black, darkest → white.
        for k in 0..3 {
            let mut v: Vec<f32> = img.chunks(3).map(|p| p[k]).collect();
            v.sort_by(f32::total_cmp);
            let (lo, hi) = (v[v.len() / 200], v[v.len() - 1 - v.len() / 200].max(1e-4));
            for p in img.chunks_mut(3) {
                p[k] = ((hi - p[k]) / (hi - lo).max(1e-4)).clamp(0.0, 1.0).powf(1.8);
            }
        }
    }
    Some(to_color_image(&encode(&img)))
}

/// The frames on a SANE preview of a `bed`-sized area, in millimetres, found as the light table finds them.
/// `neg`: the film is a negative.
fn find_frames(path: &Path, bed: [f32; 2], neg: bool) -> Vec<[f64; 4]> {
    let Ok(img) = load::load(path) else { return vec![] };
    let img = load::fit(&img, 1600);
    scan::split_strip(&img, neg).into_iter().map(|c| scanner::area_of(c, bed, 0.0).map(|v| v as f64)).collect()
}

impl App {
    pub(super) fn direct_poll(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.scan.direct.rx.try_recv() {
            let d = &mut self.scan.direct;
            match m {
                Msg::Devices(list, note) => {
                    if !list.iter().any(|f| f.key == d.key) {
                        d.key = list.iter().find(|f| f.blocked.is_none()).or(list.first()).map(|f| f.key.clone()).unwrap_or_default();
                    }
                    d.devices = Some(list);
                    d.looking = false;
                    d.note = note;
                }
                Msg::Identified(loc, r) => {
                    if let Ok(s) = &r {
                        d.infrared &= s.infrared;
                        d.dpi = d.dpi.min(s.max_dpi.max(300));
                    }
                    d.usb = Some((loc, r));
                }
                Msg::Options(key, o) => {
                    if let Ok(o) = &o {
                        let s = &mut d.sane;
                        // Film wants the transparency unit; otherwise keep what was chosen if it's there.
                        if !o.sources.contains(&s.source) && !o.sources.is_empty() {
                            s.source = o.sources.iter().find(|x| {
                                let l = x.to_ascii_lowercase();
                                l.contains("transparen") || l.contains("film") || l.contains("tpu")
                            }).unwrap_or(&o.sources[0]).clone();
                        }
                        if !o.resolutions.is_empty() && !o.resolutions.contains(&s.resolution) {
                            s.resolution = o.resolutions.iter().copied().filter(|r| *r <= 3200).max().unwrap_or(o.resolutions[0]);
                        }
                        if !o.depths.is_empty() && !o.depths.contains(&s.depth) {
                            s.depth = o.depths.iter().copied().max().unwrap_or(8);
                        }
                    }
                    d.sane_opts = Some((key, o));
                }
                Msg::Status(t) => self.scan.scanning = Some(t),
                Msg::Progress(f) => self.scan.progress = Some(f),
                Msg::Previewed(r) => {
                    d.busy = false;
                    self.scan.scanning = None;
                    self.scan.progress = None;
                    match r {
                        Ok(p) => {
                            if let Some(img) = preview_image(&p.path, d.show_positive) {
                                let tex = ctx.load_texture("scanner_preview", img, TextureOptions::LINEAR);
                                d.preview = Some((p.path, tex, p.what, p.bed));
                            }
                            if let Some(s) = p.usb {
                                d.usb = Some((s.location.clone(), Ok(s)));
                            }
                            if p.bed.is_some() {
                                d.frames = (1..=p.found.len().max(1) as u32).collect();
                                d.found = p.found;
                            }
                            self.status = "Preview ready. Click the frames you want to scan".into();
                        }
                        Err(e) => self.status = format!("Preview failed: {e}"),
                    }
                }
            }
        }
    }

    fn direct_look(&mut self, ctx: &egui::Context) {
        let d = &mut self.scan.direct;
        d.devices = Some(vec![]);
        d.looking = true;
        d.sane_opts = None;
        let (tx, ctx) = (d.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut list = vec![];
            let mut note = None;
            match direct::devices() {
                Ok(v) => list.extend(v.into_iter().map(|f| Dev { key: f.location, name: f.name, blocked: f.blocked, kind: Kind::Usb })),
                Err(e) => note = Some(e),
            }
            match scanner::sane_devices() {
                Ok(v) => list.extend(v.into_iter().map(|s| Dev { key: s.id, name: format!("{} (SANE)", s.name), blocked: None, kind: Kind::Sane })),
                Err(e) => note = Some(e),
            }
            // Two scanners with the same name: tell them apart by where they are.
            let names: Vec<String> = list.iter().map(|d| d.name.clone()).collect();
            for d in &mut list {
                if names.iter().filter(|n| **n == d.name).count() > 1 {
                    d.name = format!("{} · {}", d.name, d.key);
                }
            }
            // A missing route (no USB access, no SANE) only matters when nothing turned up at all.
            let note = if list.is_empty() { note } else { None };
            let _ = tx.send(Msg::Devices(list, note));
            ctx.request_repaint();
        });
    }

    pub(super) fn scanner_left(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 10.0;
        let ctx = ui.ctx().clone();
        if self.scan.direct.devices.is_none() {
            self.direct_look(&ctx);
        }
        ui.label(caps("Scanner"));
        let d = &mut self.scan.direct;
        let list = d.devices.clone().unwrap_or_default();
        if let Some(e) = &d.note {
            ui.label(RichText::new(e).size(12.0).color(WARN));
        }
        let full = ui.available_width();
        if d.looking {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Looking for scanners…").size(12.5).color(TEXT2));
            });
            return;
        }
        let shown = list.iter().find(|f| f.key == d.key).map_or("No scanner found".to_string(), |f| short(&f.name, 30));
        egui::ComboBox::from_id_salt("direct_dev").width(full).truncate().selected_text(shown).show_ui(ui, |ui| {
            for f in &list {
                pick(ui, &mut d.key, f.key.clone(), &f.name);
            }
        });
        if small_action(ui, "Search again", false).on_hover_text("Look for scanners again, after plugging one in or switching it on").clicked() {
            self.direct_look(&ctx);
            return;
        }
        let d = &mut self.scan.direct;
        let Some(dev) = d.dev().cloned() else {
            ui.label(RichText::new("Plug in your scanner, switch it on and press Search again. An Epson Perfection V800 or V850 works over USB. On Linux, any scanner that SANE supports works too.\n\nClose Epson Scan 2, VueScan and SilverFast first, because only one program can use the scanner at a time.").size(12.0).color(TEXT2));
            return;
        };
        if let Some(why) = &dev.blocked {
            ui.label(RichText::new(why).size(12.0).color(WARN));
            return;
        }
        let ok = match dev.kind {
            Kind::Usb => self.usb_settings(ui, &dev),
            Kind::Sane => self.sane_settings(ui, &dev),
        };
        if !ok {
            return;
        }
        let d = &mut self.scan.direct;
        slider(ui, "Frame margin %", &mut d.overage, -20.0, 20.0, 4.0, None);
        slider(ui, "Move across mm", &mut d.nudge[0], -10.0, 10.0, 0.0, None);
        slider(ui, "Move down mm", &mut d.nudge[1], -10.0, 10.0, 0.0, None);

        ui.add_space(4.0);
        ui.label(caps("Into"));
        let folder = self.folder.as_deref().map_or("No roll folder yet".to_string(), |f| f.display().to_string());
        ui.label(RichText::new(folder).monospace().size(11.0).color(TEXT3));
        if ui.add(egui::Button::new("Choose folder…").min_size(vec2(ui.available_width(), 28.0))).clicked()
            && let Some(dir) = rfd::FileDialog::new().set_title("Folder for the roll's scans").pick_folder()
        {
            self.open_path(&ctx, &dir);
        }
        ui.add_space(14.0);
        // Folded to one line here, so the page fits on the screen.
        let summary = self.roll_loaded().map(|r| [r.roll.as_str(), r.camera.as_str()].iter().filter(|v| !v.is_empty()).map(|v| v.to_string()).collect::<Vec<_>>().join(" · "));
        if let Some(summary) = summary {
            let title = if summary.is_empty() { "Roll notes".to_string() } else { format!("Roll notes: {summary}") };
            let mut open = false;
            disclose(ui, &short(&title, 34), "scanner_roll", |_| open = true);
            if open {
                self.roll_fields(ui);
            }
        }
    }

    /// A V800/V850 over USB: what it is, its holders, how to scan. False until it has answered.
    fn usb_settings(&mut self, ui: &mut Ui, dev: &Dev) -> bool {
        let ctx = ui.ctx().clone();
        let full = ui.available_width();
        let d = &mut self.scan.direct;
        if d.usb.as_ref().is_none_or(|(l, _)| *l != dev.key) && !d.busy {
            d.usb = Some((dev.key.clone(), Err("Asking the scanner what model it is…".into())));
            let (tx, ctx, loc) = (d.tx.clone(), ctx.clone(), dev.key.clone());
            std::thread::spawn(move || {
                let r = direct::identify(&loc);
                let _ = tx.send(Msg::Identified(loc, r));
                ctx.request_repaint();
            });
        }
        let scanner = match &d.usb {
            Some((_, Ok(s))) => s.clone(),
            Some((_, Err(e))) => {
                ui.label(RichText::new(e).size(12.0).color(TEXT2));
                if ghost(ui, "Try again", false).clicked() {
                    d.usb = None;
                }
                return false;
            }
            None => return false,
        };
        ui.label(RichText::new(format!("{} · up to {} ppi{}", scanner.name, scanner.max_dpi, if scanner.infrared { " · infrared" } else { "" })).size(12.0).color(TEXT3));

        ui.add_space(4.0);
        ui.label(caps("Holder"));
        let layouts = d.layouts();
        let mut li = d.layout.min(layouts.len().saturating_sub(1));
        egui::ComboBox::from_id_salt("direct_holder").width(full).truncate().selected_text(layouts.get(li).map_or("—".to_string(), |l| short(&l.0, 26))).show_ui(ui, |ui| {
            for (i, (name, _)) in layouts.iter().enumerate() {
                pick(ui, &mut li, i, name);
            }
        });
        if li != d.layout {
            d.layout = li;
            d.frames.clear();
        }

        true
    }

    /// A SANE scanner: source, resolution, depth, passes, linear. False until its options are in.
    fn sane_settings(&mut self, ui: &mut Ui, dev: &Dev) -> bool {
        let d = &mut self.scan.direct;
        if d.sane.device != dev.key {
            d.sane.device = dev.key.clone();
            d.sane.device_name = dev.name.split(" (SANE)").next().unwrap_or_default().to_string();
            d.frames.clear();
            d.found.clear();
        }
        let want = (d.sane.device.clone(), d.sane.source.clone());
        if d.sane_opts.as_ref().is_none_or(|(k, _)| *k != want) && !d.busy {
            d.sane_opts = Some((want.clone(), Err("Asking the scanner which settings it has…".into())));
            let (tx, ctx) = (d.tx.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let o = scanner::sane_options(&want.0, &want.1);
                // The source may be settled only now (film over the glass): key the answer by what it is for.
                let _ = tx.send(Msg::Options(want, o));
                ctx.request_repaint();
            });
        }
        let opts = match &d.sane_opts {
            Some((_, Ok(o))) => o.clone(),
            Some((_, Err(e))) => {
                ui.label(RichText::new(e).size(12.0).color(TEXT2));
                return false;
            }
            None => return false,
        };
        let s = &mut d.sane;
        if !opts.sources.is_empty() {
            egui::Grid::new("sane_src").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
                ui.label(RichText::new("Source").size(12.0).color(TEXT2));
                egui::ComboBox::from_id_salt("sane_src").width(150.0).truncate().selected_text(short(&s.source, 18)).show_ui(ui, |ui| {
                    for o in &opts.sources {
                        pick(ui, &mut s.source, o.clone(), o);
                    }
                });
                ui.end_row();
            });
        }
        if opts.bed_mm.is_none() {
            ui.label(RichText::new("This scanner doesn't report the size of its scan area, so ferrotype can't place frames on it.").size(12.0).color(WARN));
            return false;
        }
        true
    }

    /// Resolution, noise reduction, depth and the rest, for the chosen scanner. In the right-hand panel, next to
    /// the Preview and Scan buttons.
    fn scan_settings(&mut self, ui: &mut Ui) {
        let d = &mut self.scan.direct;
        ui.add_space(8.0);
        ui.label(caps("Scan"));
        let label = |ui: &mut Ui, t: &str| ui.label(RichText::new(t).size(12.0).color(TEXT2));
        // One set of names for both kinds of scanner. USB reads each line several times, SANE scans the frame
        // several times; either way the result is less noise for more time.
        let noise = |n: u32| match n {
            0 | 1 => "Off".to_string(),
            4 => "4× (half the noise)".to_string(),
            n => format!("{n}× (slower)"),
        };
        let noise_tip = "Scans each frame more than once and averages the result. This cleans up noise in the dense parts of a negative, but takes longer";
        let linear_tip = "Keeps the light exactly as the scanner measured it, with no contrast curve. The Negative tab converts this best";
        match d.kind() {
            Some(Kind::Usb) => {
                let Some(scanner) = d.usb_scanner().cloned() else { return };
                egui::Grid::new("direct_opts").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
                    label(ui, "Resolution");
                    egui::ComboBox::from_id_salt("direct_dpi").width(170.0).selected_text(format!("{} ppi", d.dpi)).show_ui(ui, |ui| {
                        for r in scanner.resolutions() {
                            pick(ui, &mut d.dpi, r, format!("{r} ppi"));
                        }
                    });
                    ui.end_row();
                    label(ui, "Depth");
                    label(ui, "16 bits").on_hover_text("Scans from this scanner are always 16 bits per channel");
                    ui.end_row();
                    label(ui, "Noise reduction");
                    egui::ComboBox::from_id_salt("direct_samples").width(170.0).selected_text(noise(d.oversample)).show_ui(ui, |ui| {
                        for n in [1, 2, 4] {
                            pick(ui, &mut d.oversample, n, noise(n));
                        }
                    })
                    .response
                    .on_hover_text(noise_tip);
                    ui.end_row();
                });
                switch_row(ui, "Linear (for Negative)", &mut d.linear).on_hover_text(linear_tip);
                if scanner.infrared {
                    switch_row(ui, "Infrared dust pass", &mut d.infrared).on_hover_text("Adds an infrared pass that shows dust and scratches, so Infrared clean can remove them. Leave it off for black-and-white film, which blocks infrared");
                }
            }
            Some(Kind::Sane) => {
                let Some(opts) = d.sane_options().cloned() else { return };
                let s = &mut d.sane;
                egui::Grid::new("sane_opts").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
                    if !opts.resolutions.is_empty() {
                        label(ui, "Resolution");
                        egui::ComboBox::from_id_salt("sane_res").width(170.0).selected_text(format!("{} ppi", s.resolution)).show_ui(ui, |ui| {
                            for &r in &opts.resolutions {
                                pick(ui, &mut s.resolution, r, format!("{r} ppi"));
                            }
                        });
                        ui.end_row();
                    }
                    let depths: Vec<u32> = opts.depths.iter().copied().filter(|d| *d >= 8).collect();
                    label(ui, "Depth");
                    if depths.len() > 1 {
                        egui::ComboBox::from_id_salt("sane_depth").width(170.0).selected_text(format!("{} bits", s.depth)).show_ui(ui, |ui| {
                            for &d in &depths {
                                pick(ui, &mut s.depth, d, format!("{d} bits"));
                            }
                        });
                    } else {
                        label(ui, &format!("{} bits", depths.first().copied().unwrap_or(8)));
                    }
                    ui.end_row();
                    label(ui, "Noise reduction");
                    egui::ComboBox::from_id_salt("sane_passes").width(170.0).selected_text(noise(s.passes)).show_ui(ui, |ui| {
                        for n in [1, 2, 4, 8] {
                            pick(ui, &mut s.passes, n, noise(n));
                        }
                    })
                    .response
                    .on_hover_text(noise_tip);
                    ui.end_row();
                });
                if opts.linear.is_some() {
                    switch_row(ui, "Linear (for Negative)", &mut s.linear).on_hover_text(linear_tip);
                }
            }
            None => {}
        }
    }

    pub(super) fn scanner_right(&mut self, ui: &mut Ui) {
        let d = &mut self.scan.direct;
        if !d.ready() {
            let t = if d.looking { "Looking for scanners…" } else { "Pick a scanner to see its frames." };
            ui.label(RichText::new(t).color(TEXT3));
            return;
        }
        let boxes = d.boxes();
        let n = boxes.len() as u32;
        let on = d.frames.iter().filter(|f| **f <= n).count();
        ui.label(RichText::new(format!("{on} of {n} frame{}", if n == 1 { "" } else { "s" })).family(semi()).size(14.0));
        ui.horizontal(|ui| {
            if ghost(ui, "All", false).clicked() {
                d.frames = (1..=n).collect();
            }
            if ghost(ui, "None", false).clicked() {
                d.frames.clear();
            }
        });
        let how = if d.kind() == Some(Kind::Sane) {
            "Preview first and ferrotype finds the frames. Click a frame to add it or take it out. Until you preview, the whole area counts as one frame."
        } else {
            "Click a frame on the holder to add it or take it out. Preview first to see where the film sits, then move the boxes onto it."
        };
        ui.label(RichText::new(how).size(12.0).color(TEXT2));
        let mut pos = d.show_positive;
        switch_row(ui, "Show preview as positive", &mut pos);
        if pos != d.show_positive {
            d.show_positive = pos;
            if let Some((path, _, what, bed)) = d.preview.clone()
                && let Some(img) = preview_image(&path, pos)
            {
                d.preview = Some((path, ui.ctx().load_texture("scanner_preview", img, TextureOptions::LINEAR), what, bed));
            }
        }
        self.scan_settings(ui);
        // Rough size, so a 6400 ppi job isn't a surprise.
        let d = &self.scan.direct;
        let (dpi, bits) = if d.kind() == Some(Kind::Sane) { (d.sane.resolution, d.sane.depth.max(8)) } else { (d.dpi, 16) };
        let dpi = dpi as f64;
        let px: f64 = d.frames.iter().filter_map(|&f| boxes.get(f as usize - 1)).map(|r| (r[2] / 25.4 * dpi) * (r[3] / 25.4 * dpi)).sum();
        let samples = if d.kind() == Some(Kind::Usb) && d.infrared && d.usb_scanner().is_some_and(|s| s.infrared) { 4.0 } else { 3.0 };
        if px > 0.0 {
            // Multi-pass SANE scans are averaged to 16 bits.
            let bits = if d.kind() == Some(Kind::Sane) && d.sane.passes > 1 { 16 } else { bits };
            ui.add_space(4.0);
            ui.label(RichText::new(format!("About {:.0} MB as {bits}-bit TIFF", px * samples * (bits as f64 / 8.0) / 1e6)).size(12.0).color(TEXT3));
        }
    }

    pub(super) fn scanner_footer(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let d = &self.scan.direct;
        let w = ui.available_width();
        if d.busy || self.scan.scanning.is_some() {
            if let Some(f) = self.scan.progress {
                ui.add(egui::ProgressBar::new(f).desired_width(w).desired_height(6.0).fill(BONE));
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                if self.scan.progress.is_none() {
                    ui.spinner();
                }
                let pct = self.scan.progress.map_or(String::new(), |f| format!("{:.0}%", f * 100.0));
                ui.label(RichText::new(pct).monospace().size(11.5).color(TEXT2));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Stop").on_hover_text("Stops as soon as the scanner allows. The frame being scanned is not saved").clicked() {
                        self.scan.direct.cancel.store(true, Ordering::Relaxed);
                        self.scan.scanning = Some("Stopping…".into());
                    }
                });
            });
            return;
        }
        let ready = d.ready();
        let n = d.frames.iter().filter(|f| **f as usize <= d.boxes().len()).count();
        // Preview is the next step until there is one; then Scan is.
        let previewed = d.preview_fits();
        ui.columns(2, |c| {
            let size = vec2(w / 2.0 - 4.0, 32.0);
            let tip = "A quick, low-resolution scan of the whole area, to see where the film is";
            let clicked = if previewed {
                c[0].add_enabled(ready, egui::Button::new("Preview again").min_size(size)).on_hover_text(tip).clicked()
            } else {
                primary(&mut c[0], "Preview", ready).on_hover_text(tip).clicked()
            };
            if clicked {
                self.direct_preview(&ctx);
            }
            let label = if n == 0 { "Pick frames to scan".to_string() } else { format!("Scan {n} frame{}", if n == 1 { "" } else { "s" }) };
            let go = if previewed || n > 0 {
                primary(&mut c[1], &label, ready && n > 0).clicked()
            } else {
                c[1].add_enabled(false, egui::Button::new(label).min_size(size)).clicked()
            };
            if go {
                self.direct_scan(&ctx);
            }
        });
    }

    fn direct_preview(&mut self, ctx: &egui::Context) {
        let Some(cache) = crate::keys::dirs_cache() else { return };
        let d = &mut self.scan.direct;
        let Some(kind) = d.kind() else { return };
        let dir = cache.join("scan-preview");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_millis()).unwrap_or(0);
        let out = dir.join(format!("preview-{stamp}.tif"));
        d.busy = true;
        d.cancel.store(false, Ordering::Relaxed);
        self.scan.scanning = Some("Previewing…".into());
        self.scan.progress = None;
        let (tx, ctx2, what, cancel) = (d.tx.clone(), ctx.clone(), d.what(), d.cancel.clone());
        let usb = (d.key.clone(), d.layout_now().map_or(Source::Transparency, |l| l.source));
        let (sane, neg) = (d.sane.clone(), d.show_positive);
        std::thread::spawn(move || {
            let say = |t: String| {
                let _ = tx.send(Msg::Status(format!("Preview: {t}")));
                ctx2.request_repaint();
            };
            let progress = |f: f32| {
                let _ = tx.send(Msg::Progress(f));
                ctx2.request_repaint();
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match kind {
                Kind::Usb => direct::preview(&usb.0, usb.1, false, &out, &cancel, &say, &progress).map(|s| Preview { path: out.clone(), what, bed: None, found: vec![], usb: Some(s) }),
                Kind::Sane => {
                    say("scanning the whole area…".into());
                    scanner::preview(&sane, &out, &cancel, &progress).map(|bed| Preview { found: find_frames(&out, bed, neg), path: out.clone(), what, bed: Some(bed.map(|v| v as f64)), usb: None })
                }
            }))
            .unwrap_or_else(|_| Err("the scanner stopped unexpectedly".into()));
            let _ = tx.send(Msg::Previewed(r));
            ctx2.request_repaint();
        });
    }

    fn direct_scan(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.scan_folder(ctx) else { return };
        let d = &mut self.scan.direct;
        let Some(kind) = d.kind() else { return };
        let boxes = d.boxes();
        let chosen: Vec<[f64; 4]> = d.frames.iter().filter_map(|&f| boxes.get(f as usize - 1).copied()).collect();
        if chosen.is_empty() {
            return;
        }
        d.cancel.store(false, Ordering::Relaxed);
        self.watch = true;
        self.watch_open = true;
        self.scan.scanning = Some(format!("Scanning {} frame{}…", chosen.len(), if chosen.len() == 1 { "" } else { "s" }));
        self.scan.progress = Some(0.0);
        let (sc, ctx2, cancel) = (self.scan.sc_tx.clone(), ctx.clone(), d.cancel.clone());
        match kind {
            Kind::Usb => {
                let Some(layout) = d.layout_now() else { return };
                let infrared = d.infrared && d.usb_scanner().is_some_and(|s| s.infrared);
                let jobs: Vec<direct::Job> = chosen.into_iter().map(|rect_mm| direct::Job { source: layout.source, rect_mm, dpi: d.dpi, oversample: d.oversample, linear: d.linear, infrared }).collect();
                let loc = d.key.clone();
                scanner::run(sc, move || ctx2.request_repaint(), move |say| direct::frames(&loc, &jobs, &dir, &cancel, say));
            }
            Kind::Sane => {
                let areas = chosen.into_iter().map(|r| r.map(|v| v as f32)).collect();
                scanner::scan_frames(dir, d.sane.clone(), areas, cancel, sc, move || ctx2.request_repaint());
            }
        }
    }

    /// The scanner's area drawn to scale (over the preview when there is one), its frames numbered and clickable.
    pub(super) fn scanner_center(&mut self, ui: &mut Ui) {
        let d = &mut self.scan.direct;
        let Some([aw, ah]) = d.area().filter(|_| d.ready()).filter(|a| a[0] > 0.0 && a[1] > 0.0) else {
            ui.centered_and_justified(|ui| {
                let t = if d.looking { "Looking for scanners…" } else { "Plug in a scanner to start" };
                ui.label(RichText::new(t).color(TEXT3));
            });
            return;
        };
        let avail = ui.available_rect_before_wrap().shrink(24.0);
        let k = (avail.width() as f64 / aw).min(avail.height() as f64 / ah) as f32;
        let area = Rect::from_center_size(avail.center(), vec2(aw as f32 * k, ah as f32 * k));
        let resp = ui.allocate_rect(area, Sense::click());
        let p = ui.painter_at(area.expand(2.0));
        p.rect_filled(area, 2.0, Color32::from_gray(26));
        if d.preview_fits()
            && let Some((_, tex, _, _)) = &d.preview
        {
            p.image(tex.id(), area, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        p.rect_stroke(area, 2.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Outside);
        let to_screen = |r: [f64; 4]| Rect::from_min_size(area.min + vec2(r[0] as f32 * k, r[1] as f32 * k), vec2(r[2] as f32 * k, r[3] as f32 * k));
        let click = resp.clicked().then(|| resp.interact_pointer_pos()).flatten();
        for (i, r) in d.boxes().into_iter().enumerate() {
            let n = i as u32 + 1;
            let sr = to_screen(r);
            if let Some(c) = click
                && sr.contains(c)
                && !d.frames.remove(&n)
            {
                d.frames.insert(n);
            }
            let on = d.frames.contains(&n);
            if !on {
                p.rect_filled(sr, 0.0, Color32::from_black_alpha(90));
            }
            p.rect_stroke(sr, 0.0, Stroke::new(if on { 2.0 } else { 1.0 }, if on { BONE } else { TEXT3 }), egui::StrokeKind::Inside);
            let tag = Rect::from_min_size(sr.min + vec2(4.0, 4.0), vec2(22.0, 16.0));
            p.rect_filled(tag, 3.0, if on { BONE } else { Color32::from_black_alpha(160) });
            p.text(tag.center(), Align2::CENTER_CENTER, n.to_string(), FontId::monospace(10.5), if on { INK } else { TEXT2 });
        }
        if !d.preview_fits() && !d.busy {
            let hint = Rect::from_center_size(area.center(), vec2(220.0, 30.0));
            p.rect_filled(hint, 4.0, Color32::from_black_alpha(200));
            p.text(hint.center(), Align2::CENTER_CENTER, "Preview to see your film", FontId::proportional(13.0), TEXT2);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
}

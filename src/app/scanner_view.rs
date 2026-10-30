//! The Scan view's Scanner page: Ferrotype drives the scanner itself over USB (see `scanner::direct`), the way
//! VueScan and SilverFast do. Pick a holder, preview it, click the frames to scan, and they arrive in the roll
//! folder as 16-bit TIFFs (linear, with infrared if asked) that open on the light table.
use super::*;
use crate::scanner::{self, direct};
use epscan::Source;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

enum Msg {
    Devices(Result<Vec<direct::Found>, String>),
    Identified(String, Result<direct::Scanner, String>),
    Previewed(Result<(PathBuf, direct::Scanner), String>),
}

pub(super) struct DirectUi {
    devices: Option<Result<Vec<direct::Found>, String>>,
    location: String,
    scanner: Option<(String, Result<direct::Scanner, String>)>,
    /// Index into `direct::layouts` of the scanner's model.
    layout: usize,
    /// Selected frames, 1-based as the holder numbers them.
    frames: BTreeSet<u32>,
    dpi: u32,
    oversample: u32,
    linear: bool,
    infrared: bool,
    /// Frame size change in percent (+ adds room around the frame, − crops into it).
    overage: f32,
    /// Shift of every frame box, in millimetres (film never sits exactly where the holder was measured).
    nudge: [f32; 2],
    /// The preview: the file, its picture, and which source it shows.
    preview: Option<(PathBuf, TextureHandle, Source)>,
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
            devices: None, location: String::new(), scanner: None, layout: 0, frames: BTreeSet::new(), dpi: 3200, oversample: 1, linear: true, infrared: true,
            overage: 4.0, nudge: [0.0; 2], preview: None, show_positive: true, busy: false, cancel: Arc::new(AtomicBool::new(false)), tx, rx,
        }
    }
}

impl DirectUi {
    fn scanner(&self) -> Option<&direct::Scanner> {
        self.scanner.as_ref().filter(|(l, _)| *l == self.location).and_then(|(_, s)| s.as_ref().ok())
    }

    fn layouts(&self) -> Vec<(String, direct::HolderLayout)> {
        self.scanner().map(|s| direct::layouts(s.model)).unwrap_or_default()
    }

    fn layout_now(&self) -> Option<direct::HolderLayout> {
        self.layouts().into_iter().nth(self.layout).map(|(_, l)| l)
    }

    /// Frame `n`'s rectangle in millimetres, with overage and nudge.
    fn rect(&self, layout: &direct::HolderLayout, n: u32) -> Option<[f64; 4]> {
        let r = layout.frame_rect(n, self.overage as f64).ok()?;
        Some([r[0] + self.nudge[0] as f64, r[1] + self.nudge[1] as f64, r[2], r[3]])
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

impl App {
    pub(super) fn direct_poll(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.scan.direct.rx.try_recv() {
            let d = &mut self.scan.direct;
            match m {
                Msg::Devices(r) => {
                    if let Ok(list) = &r
                        && !list.iter().any(|f| f.location == d.location)
                    {
                        d.location = list.iter().find(|f| f.blocked.is_none()).or(list.first()).map(|f| f.location.clone()).unwrap_or_default();
                    }
                    d.devices = Some(r);
                }
                Msg::Identified(loc, r) => {
                    if let Ok(s) = &r {
                        d.infrared &= s.infrared;
                        d.dpi = d.dpi.min(s.max_dpi.max(300));
                    }
                    d.scanner = Some((loc, r));
                }
                Msg::Previewed(r) => {
                    d.busy = false;
                    self.scan.scanning = None;
                    match r {
                        Ok((path, s)) => {
                            let source = d.layout_now().map_or(Source::Transparency, |l| l.source);
                            if let Some(img) = preview_image(&path, d.show_positive) {
                                let tex = ctx.load_texture("scanner_preview", img, TextureOptions::LINEAR);
                                d.preview = Some((path, tex, source));
                            }
                            d.scanner = Some((s.location.clone(), Ok(s)));
                            self.status = "Preview ready: click the frames to scan".into();
                        }
                        Err(e) => self.status = format!("Preview failed: {e}"),
                    }
                }
            }
        }
    }

    fn direct_look(&mut self, ctx: &egui::Context) {
        let d = &mut self.scan.direct;
        d.devices = Some(Ok(vec![]));
        let (tx, ctx) = (d.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Devices(direct::devices()));
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
        let list = match &d.devices {
            Some(Ok(l)) => l.clone(),
            Some(Err(e)) => {
                ui.label(RichText::new(e).size(12.0).color(WARN));
                vec![]
            }
            None => vec![],
        };
        let full = ui.available_width();
        let mut look = false;
        ui.horizontal(|ui| {
            let shown = list.iter().find(|f| f.location == d.location).map_or("No scanner found".to_string(), |f| short(&f.name, 18));
            egui::ComboBox::from_id_salt("direct_dev").width(full - 70.0).truncate().selected_text(shown).show_ui(ui, |ui| {
                for f in &list {
                    pick(ui, &mut d.location, f.location.clone(), &f.name);
                }
            });
            look = ghost(ui, "Look", false).on_hover_text("Look for scanners again").clicked();
        });
        if look {
            self.direct_look(&ctx);
            return;
        }
        let d = &mut self.scan.direct;
        if list.is_empty() {
            ui.label(RichText::new("Connect an Epson Perfection V800 or V850 and switch it on. Close Epson Scan 2, VueScan or SilverFast first: only one program can talk to the scanner at a time.").size(12.0).color(TEXT2));
            return;
        }
        if let Some(why) = list.iter().find(|f| f.location == d.location).and_then(|f| f.blocked.clone()) {
            ui.label(RichText::new(why).size(12.0).color(WARN));
            return;
        }
        if d.scanner.as_ref().is_none_or(|(l, _)| *l != d.location) && !d.busy {
            d.scanner = Some((d.location.clone(), Err("Asking the scanner what it is…".into())));
            let (tx, ctx, loc) = (d.tx.clone(), ctx.clone(), d.location.clone());
            std::thread::spawn(move || {
                let r = direct::identify(&loc);
                let _ = tx.send(Msg::Identified(loc, r));
                ctx.request_repaint();
            });
        }
        let scanner = match &d.scanner {
            Some((_, Ok(s))) => s.clone(),
            Some((_, Err(e))) => {
                ui.label(RichText::new(e).size(12.0).color(TEXT2));
                if ghost(ui, "Try again", false).clicked() {
                    d.scanner = None;
                }
                return;
            }
            None => return,
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
        slider(ui, "Frame size %", &mut d.overage, -20.0, 20.0, 4.0, None);
        slider(ui, "Move across mm", &mut d.nudge[0], -10.0, 10.0, 0.0, None);
        slider(ui, "Move down mm", &mut d.nudge[1], -10.0, 10.0, 0.0, None);

        ui.add_space(4.0);
        ui.label(caps("Scan"));
        egui::Grid::new("direct_opts").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
            ui.label(RichText::new("Resolution").size(12.0).color(TEXT2));
            egui::ComboBox::from_id_salt("direct_dpi").width(150.0).selected_text(format!("{} ppi", d.dpi)).show_ui(ui, |ui| {
                for r in scanner.resolutions() {
                    pick(ui, &mut d.dpi, r, format!("{r} ppi"));
                }
            });
            ui.end_row();
            ui.label(RichText::new("Samples").size(12.0).color(TEXT2));
            let shown = if d.oversample <= 1 { "1".to_string() } else { format!("{}×", d.oversample) };
            egui::ComboBox::from_id_salt("direct_samples").width(150.0).selected_text(shown).show_ui(ui, |ui| {
                for n in [1, 2, 4] {
                    pick(ui, &mut d.oversample, n, if n == 1 { "1".to_string() } else { format!("{n}×") });
                }
            })
            .response
            .on_hover_text("Each line is read several times and averaged: less noise in dense negatives, slower scans");
            ui.end_row();
        });
        switch_row(ui, "Linear data", &mut d.linear).on_hover_text("Neutral tone tables: the file holds the light the scanner measured, what negative conversion wants");
        if scanner.infrared {
            switch_row(ui, "Infrared pass", &mut d.infrared).on_hover_text("A second pass in infrared, saved with the scan for Infrared clean (dust and scratches). Not for black-and-white silver film");
        }

        ui.add_space(4.0);
        ui.label(caps("Into"));
        let folder = self.folder.as_deref().map_or("No roll folder yet".to_string(), |f| f.display().to_string());
        ui.label(RichText::new(folder).monospace().size(11.0).color(TEXT3));
        if ui.add(egui::Button::new("Choose folder…").min_size(vec2(ui.available_width(), 28.0))).clicked()
            && let Some(dir) = rfd::FileDialog::new().set_title("Folder for the roll's scans").pick_folder()
        {
            self.open_path(&ctx, &dir);
        }
        ui.add_space(6.0);
        self.roll_notes(ui);
    }

    pub(super) fn scanner_right(&mut self, ui: &mut Ui) {
        let d = &mut self.scan.direct;
        let Some(layout) = d.layout_now() else {
            ui.label(RichText::new("Choose a scanner to see its holders.").color(TEXT3));
            return;
        };
        let n = layout.frames_mm.len() as u32;
        ui.label(RichText::new(format!("{} of {n} frames", d.frames.len())).family(semi()).size(14.0));
        ui.horizontal(|ui| {
            if ghost(ui, "All", false).clicked() {
                d.frames = (1..=n).collect();
            }
            if ghost(ui, "None", false).clicked() {
                d.frames.clear();
            }
        });
        ui.label(RichText::new("Click a frame on the holder to add or remove it. Preview first to see where the film sits, then move the boxes onto it.").size(12.0).color(TEXT2));
        let mut pos = d.show_positive;
        switch_row(ui, "Preview as positive", &mut pos);
        if pos != d.show_positive {
            d.show_positive = pos;
            if let Some((path, _, source)) = d.preview.clone()
                && let Some(img) = preview_image(&path, pos)
            {
                d.preview = Some((path, ui.ctx().load_texture("scanner_preview", img, TextureOptions::LINEAR), source));
            }
        }
        if let Some(s) = d.scanner() {
            // Rough size and time, so a 6400 ppi 4× job isn't a surprise.
            let px: f64 = d.frames.iter().filter_map(|&f| d.rect(&layout, f)).map(|r| (r[2] / 25.4 * d.dpi as f64) * (r[3] / 25.4 * d.dpi as f64)).sum();
            let samples = if d.infrared && s.infrared { 4.0 } else { 3.0 };
            if px > 0.0 {
                ui.label(RichText::new(format!("About {:.0} MB of 16-bit TIFF", px * samples * 2.0 / 1e6)).size(12.0).color(TEXT3));
            }
        }
    }

    pub(super) fn scanner_footer(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let d = &self.scan.direct;
        let w = ui.available_width();
        if d.busy || self.scan.scanning.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                if ui.button("Stop").on_hover_text("Stop after the block being read; the frame in progress is dropped").clicked() {
                    self.scan.direct.cancel.store(true, Ordering::Relaxed);
                    self.status = "Stopping…".into();
                }
            });
            return;
        }
        let ready = d.scanner().is_some();
        let n = d.frames.len();
        ui.columns(2, |c| {
            if c[0].add_enabled(ready, egui::Button::new("Preview").min_size(vec2(w / 2.0 - 4.0, 32.0))).on_hover_text("A quick scan of the whole holder").clicked() {
                self.direct_preview(&ctx);
            }
            let label = if n == 0 { "Scan".to_string() } else { format!("Scan {n} frame{}", if n == 1 { "" } else { "s" }) };
            if primary(&mut c[1], &label, ready && n > 0).clicked() {
                self.direct_scan(&ctx);
            }
        });
    }

    fn direct_preview(&mut self, ctx: &egui::Context) {
        let Some(cache) = crate::keys::dirs_cache() else { return };
        let d = &mut self.scan.direct;
        let Some(layout) = d.layout_now() else { return };
        let dir = cache.join("scan-preview");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_millis()).unwrap_or(0);
        let out = dir.join(format!("holder-{stamp}.tif"));
        d.busy = true;
        self.scan.scanning = Some("Previewing the holder…".into());
        let (tx, sc, ctx2, loc, source) = (d.tx.clone(), self.scan.sc_tx.clone(), ctx.clone(), d.location.clone(), layout.source);
        std::thread::spawn(move || {
            let say = |t: String| {
                let _ = sc.send(scanner::Event::Status(format!("Preview: {t}")));
                ctx2.request_repaint();
            };
            let r = direct::preview(&loc, source, false, &out, &say).map(|s| (out, s));
            let _ = tx.send(Msg::Previewed(r));
            ctx2.request_repaint();
        });
    }

    fn direct_scan(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.folder.clone().or_else(|| {
            let d = rfd::FileDialog::new().set_title("Folder for the roll's scans").pick_folder()?;
            self.open_path(ctx, &d);
            Some(d)
        }) else {
            return;
        };
        let d = &mut self.scan.direct;
        let Some(layout) = d.layout_now() else { return };
        let infrared = d.infrared && d.scanner().is_some_and(|s| s.infrared);
        let jobs: Vec<direct::Job> = d.frames.iter().filter_map(|&f| d.rect(&layout, f)).map(|rect_mm| direct::Job {
            source: layout.source, rect_mm, dpi: d.dpi, oversample: d.oversample, linear: d.linear, infrared,
        }).collect();
        d.cancel.store(false, Ordering::Relaxed);
        self.watch = true;
        self.watch_open = true;
        self.scan.scanning = Some(format!("Scanning {} frame{}…", jobs.len(), if jobs.len() == 1 { "" } else { "s" }));
        let (sc, ctx2, loc, cancel) = (self.scan.sc_tx.clone(), ctx.clone(), d.location.clone(), d.cancel.clone());
        std::thread::spawn(move || {
            let say = |e: scanner::Event| {
                let _ = sc.send(e);
                ctx2.request_repaint();
            };
            let r = direct::frames(&loc, &jobs, &dir, &cancel, &say);
            say(scanner::Event::Done(r));
        });
    }

    /// The holder drawn to scale (over the preview when there is one), its frames numbered and clickable.
    pub(super) fn scanner_center(&mut self, ui: &mut Ui) {
        let d = &mut self.scan.direct;
        let (Some(layout), Some(s)) = (d.layout_now(), d.scanner().cloned()) else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Connect a scanner to start").color(TEXT3));
            });
            return;
        };
        let [aw, ah] = s.area(layout.source);
        if aw <= 0.0 || ah <= 0.0 {
            return;
        }
        let avail = ui.available_rect_before_wrap().shrink(24.0);
        let k = (avail.width() as f64 / aw).min(avail.height() as f64 / ah) as f32;
        let size = vec2(aw as f32 * k, ah as f32 * k);
        let area = Rect::from_center_size(avail.center(), size);
        let resp = ui.allocate_rect(area, Sense::click());
        let p = ui.painter_at(area.expand(2.0));
        p.rect_filled(area, 2.0, Color32::from_gray(26));
        if let Some((_, tex, src)) = &d.preview
            && *src == layout.source
        {
            p.image(tex.id(), area, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        p.rect_stroke(area, 2.0, Stroke::new(1.0, BORDER), egui::StrokeKind::Outside);
        let to_screen = |r: [f64; 4]| Rect::from_min_size(area.min + vec2(r[0] as f32 * k, r[1] as f32 * k), vec2(r[2] as f32 * k, r[3] as f32 * k));
        let click = resp.clicked().then(|| resp.interact_pointer_pos()).flatten();
        for n in 1..=layout.frames_mm.len() as u32 {
            let Some(r) = d.rect(&layout, n) else { continue };
            let sr = to_screen(r);
            let on = d.frames.contains(&n);
            if let Some(c) = click
                && sr.contains(c)
            {
                if on {
                    d.frames.remove(&n);
                } else {
                    d.frames.insert(n);
                }
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
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
}

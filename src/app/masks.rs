//! Masks: the panel (list, shapes, settings) and the Mask tool on the photo.
use super::*;
use crate::develop::{Mask, MaskOp, MaskPart, Shape};

/// What a drag on the photo draws when it doesn't grab a handle.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Draw {
    Linear,
    Radial,
    Brush,
}

/// Mask panel and tool state.
pub(super) struct MaskUi {
    pub sel: usize,
    pub draw: Draw,
    /// How the next shape combines with the mask.
    pub op: MaskOp,
    pub overlay: bool,
    /// Next click on the photo picks the colour for a colour-range shape.
    pub pick: bool,
    pub size: f32,
    pub erase: bool,
    tab: usize,
    curve_chan: usize,
    curve_grab: Option<usize>,
    /// Dragging: (part, handle) where handle 0 = new shape, 1 = point a / centre, 2 = point b / x radius, 3 = y radius.
    drag: Option<(usize, u8)>,
}

impl Default for MaskUi {
    fn default() -> Self {
        MaskUi { sel: 0, draw: Draw::Linear, op: MaskOp::Add, overlay: true, pick: false, size: 4.0, erase: false, tab: 0, curve_chan: 0, curve_grab: None, drag: None }
    }
}

const OPS: [(MaskOp, &str); 3] = [(MaskOp::Add, "Add"), (MaskOp::Subtract, "Subtract"), (MaskOp::Intersect, "Intersect")];

/// A new shape of `kind` that needs no drawing, or None for drawn shapes.
fn instant(kind: usize) -> Option<Shape> {
    match kind {
        3 => Some(Shape::Luminance { lo: 0.6, hi: 1.0, soft: 0.1 }),
        4 => Some(Shape::Color { rgb: [0.5, 0.5, 0.5], range: 30.0 }),
        5 => Some(Shape::All),
        _ => None,
    }
}

const KINDS: [&str; 6] = ["Linear", "Radial", "Brush", "Luminance", "Colour", "Whole"];

/// The Masks section of the Develop panel. Returns true when the Mask tool should be switched on (to draw).
pub(super) fn masks_panel(ui: &mut Ui, st: &mut MaskUi, p: &mut Params) -> bool {
    let mut want_tool = false;
    ui.label(RichText::new("New mask").size(11.0).family(semi()).color(TEXT3));
    ui.horizontal_wrapped(|ui| {
        for (k, name) in KINDS.iter().enumerate() {
            if ui.button(*name).clicked() {
                let mut m = Mask { name: format!("Mask {}", p.masks.len() + 1), ..Default::default() };
                if let Some(s) = instant(k) {
                    m.parts.push(MaskPart { shape: s, op: MaskOp::Add, invert: false });
                    st.pick = k == 4;
                } else {
                    st.draw = [Draw::Linear, Draw::Radial, Draw::Brush][k];
                }
                st.op = MaskOp::Add;
                p.masks.push(m);
                st.sel = p.masks.len() - 1;
                want_tool = true;
            }
        }
    });
    if p.masks.is_empty() {
        ui.label(RichText::new("A mask limits settings to part of the photo: a gradient, a circle, painted areas, a range of brightness or colour, or any mix of these.").size(11.5).color(TEXT2));
        return want_tool;
    }
    st.sel = st.sel.min(p.masks.len() - 1);
    ui.add_space(6.0);
    // The list of masks.
    let mut remove = None;
    for (i, m) in p.masks.iter_mut().enumerate() {
        let on = i == st.sel;
        ui.horizontal(|ui| {
            // Show/hide: a filled dot when on, a ring when hidden.
            let (dot, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
            if m.hidden {
                ui.painter().circle_stroke(dot.center(), 4.5, Stroke::new(1.5, TEXT3));
            } else {
                ui.painter().circle_filled(dot.center(), 5.0, ACCENT);
            }
            if resp.on_hover_text("Show or hide this mask's effect").clicked() {
                m.hidden = !m.hidden;
            }
            let name = if m.parts.is_empty() { format!("{} (draw on the photo)", m.name) } else { m.name.clone() };
            if ui.selectable_label(on, name).clicked() {
                st.sel = i;
                want_tool = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(egui::Label::new(RichText::new("×").size(13.0).color(TEXT3)).sense(Sense::click())).on_hover_text("Delete this mask").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        p.masks.remove(i);
        st.sel = st.sel.saturating_sub(usize::from(i <= st.sel && st.sel > 0));
        return want_tool;
    }
    let m = &mut p.masks[st.sel];
    ui.add_space(4.0);
    ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(f32::INFINITY));

    // Shapes in this mask.
    group(ui, "Shapes", None);
    let mut remove_part = None;
    for (k, part) in m.parts.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(part.shape.name()).size(12.0).color(LABEL));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(egui::Label::new(RichText::new("×").size(13.0).color(TEXT3)).sense(Sense::click())).on_hover_text("Remove this shape").clicked() {
                    remove_part = Some(k);
                }
                ui.checkbox(&mut part.invert, "Invert");
                if k > 0 {
                    egui::ComboBox::from_id_salt(("op", k)).width(84.0).selected_text(OPS.iter().find(|o| o.0 == part.op).map_or("", |o| o.1)).show_ui(ui, |ui| {
                        for (op, name) in OPS {
                            ui.selectable_value(&mut part.op, op, name);
                        }
                    });
                }
            });
        });
        match &mut part.shape {
            Shape::Luminance { lo, hi, soft } => {
                let (mut a, mut b, mut c) = (*lo * 100.0, *hi * 100.0, *soft * 100.0);
                slider(ui, "From", &mut a, 0.0, 100.0, 60.0, Some(&|t: f32| enc([t / 100.0; 3])));
                slider(ui, "To", &mut b, 0.0, 100.0, 100.0, Some(&|t: f32| enc([t / 100.0; 3])));
                slider(ui, "Smoothness", &mut c, 0.0, 50.0, 10.0, None);
                (*lo, *hi, *soft) = (a.min(b) / 100.0, b.max(a) / 100.0, c / 100.0);
            }
            Shape::Color { rgb, range } => {
                ui.horizontal(|ui| {
                    let (sw, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                    ui.painter().rect_filled(sw, 4.0, enc(*rgb));
                    if ghost(ui, if st.pick { "Click the photo…" } else { "Pick colour" }, st.pick).clicked() {
                        st.pick = !st.pick;
                        want_tool = true;
                    }
                });
                slider(ui, "Range", range, 0.0, 100.0, 30.0, None);
            }
            Shape::Radial { feather, angle, .. } => {
                slider(ui, "Feather", feather, 0.0, 100.0, 50.0, None);
                slider(ui, "Angle", angle, -180.0, 180.0, 0.0, None);
            }
            _ => {}
        }
    }
    if let Some(k) = remove_part {
        m.parts.remove(k);
    }
    // Add to the mask.
    ui.add_space(4.0);
    ui.label(RichText::new("Add a shape to this mask").size(11.0).family(semi()).color(TEXT3));
    ui.horizontal(|ui| {
        let mut o = OPS.iter().position(|x| x.0 == st.op).unwrap_or(0);
        segmented(ui, &mut o, &["Add", "Subtract", "Intersect"]);
        st.op = OPS[o].0;
    });
    ui.horizontal_wrapped(|ui| {
        for (k, name) in KINDS.iter().enumerate() {
            if ui.small_button(*name).on_hover_text(format!("{} a {} shape", OPS.iter().find(|x| x.0 == st.op).map_or("Add", |x| x.1), name.to_lowercase())).clicked() {
                match instant(k) {
                    Some(s) => {
                        let op = if m.parts.is_empty() { MaskOp::Add } else { st.op };
                        m.parts.push(MaskPart { shape: s, op, invert: false });
                        st.pick = k == 4;
                    }
                    None => st.draw = [Draw::Linear, Draw::Radial, Draw::Brush][k],
                }
                want_tool = true;
            }
        }
    });
    let draw_name = match st.draw { Draw::Linear => "linear gradient", Draw::Radial => "radial gradient", Draw::Brush => "brush" };
    ui.label(RichText::new(format!("With the Mask tool, dragging on the photo draws a {draw_name}. Drag a handle to adjust; right-click a handle to remove that shape.")).size(11.5).color(TEXT2));
    if st.draw == Draw::Brush {
        slider(ui, "Brush size", &mut st.size, 0.5, 20.0, 4.0, None);
        switch_row(ui, "Erase", &mut st.erase);
    }
    group(ui, "Mask", None);
    slider(ui, "Opacity", &mut m.opacity, 0.0, 100.0, 100.0, None);
    slider(ui, "Feather", &mut m.feather, 0.0, 100.0, 0.0, None);
    switch_row(ui, "Invert", &mut m.invert);
    switch_row(ui, "Show overlay", &mut st.overlay);

    // The mask's own settings, as changes to the photo's.
    let a = &mut m.adj;
    group(ui, "Light and colour", None);
    let mut t = (a.temp - 5500.0) / 30.0;
    slider(ui, "Temp", &mut t, -100.0, 100.0, 0.0, Some(&|t: f32| neutral_through(wb_gains(5500.0 + t * 30.0, 0.0))));
    a.temp = 5500.0 + t * 30.0;
    slider(ui, "Tint", &mut a.tint, -100.0, 100.0, 0.0, Some(&|t: f32| neutral_through(wb_gains(5500.0, t))));
    slider(ui, "Exposure", &mut a.exposure, -4.0, 4.0, 0.0, Some(&|ev: f32| lin([(0.18 * 2f32.powf(ev)).min(1.0); 3])));
    for (name, v) in [("Contrast", &mut a.contrast), ("Highlights", &mut a.highlights), ("Shadows", &mut a.shadows), ("Whites", &mut a.whites), ("Blacks", &mut a.blacks)] {
        slider(ui, name, v, -100.0, 100.0, 0.0, None);
    }
    for (name, v) in [("Texture", &mut a.texture), ("Clarity", &mut a.clarity), ("Dehaze", &mut a.dehaze), ("Vibrance", &mut a.vibrance), ("Saturation", &mut a.saturation)] {
        slider(ui, name, v, -100.0, 100.0, 0.0, None);
    }
    disclose(ui, "Colour mixer", "mask_mixer", |ui| {
        segmented(ui, &mut st.tab, &["Hue", "Saturation", "Luminance"]);
        for (b, name) in ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"].into_iter().enumerate() {
            slider_ex(ui, name, Some(hsv(HUE_CENTERS[b], 0.7, 0.9)), &mut a.hsl[b][st.tab], -100.0, 100.0, 0.0, None);
        }
    });
    disclose(ui, "Tone curve", "mask_curve", |ui| {
        segmented(ui, &mut st.curve_chan, &["RGB", "Red", "Green", "Blue"]);
        curve_editor(ui, a, st.curve_chan, &mut st.curve_grab, None, &[[0.0; 128]; 3]);
        ui.label(RichText::new("Applied after the photo's own curve.").size(11.5).color(TEXT2));
    });
    disclose(ui, "Detail", "mask_detail", |ui| {
        let d = Params::default();
        let mut sharp = a.sh_amt - d.sh_amt;
        slider(ui, "Sharpness", &mut sharp, -100.0, 100.0, 0.0, None);
        a.sh_amt = d.sh_amt + sharp;
        slider(ui, "Noise reduction", &mut a.nr_lum, 0.0, 100.0, 0.0, None);
        let mut col = a.nr_col - d.nr_col;
        slider(ui, "Colour noise", &mut col, -25.0, 75.0, 0.0, None);
        a.nr_col = d.nr_col + col;
        slider(ui, "Defringe", &mut a.o_fringe, 0.0, 20.0, 0.0, None);
    });
    if ui.button("Reset mask settings").clicked() {
        m.adj = Params::default();
    }
    want_tool
}

impl App {
    /// The Mask tool on the photo: draw shapes into the selected mask, move their handles, pick colours.
    pub(super) fn mask_tool(&mut self, ui: &Ui, resp: &egui::Response, r: Rect) {
        let [cw, ch, ox, oy, fw, fh, ..] = self.view_map;
        let to_px = r.width() / cw * fw.max(fh); // fraction of the frame's long side → screen points
        let vm = self.view_map;
        // Frame fraction → screen (as `to_screen`, without borrowing self).
        let scr = move |f: [f32; 2]| r.min + vec2((f[0] * vm[4] - vm[6] + vm[2]) / vm[0] * r.width(), (f[1] * vm[5] - vm[7] + vm[3]) / vm[1] * r.height());
        let frame_at = |s: &Self, pos: Pos2| s.to_frame((pos - r.min) / r.size());
        // Make sure a mask is selected to draw into.
        let ensure = |s: &mut Self| {
            if s.params.masks.is_empty() || s.mask_ui.sel >= s.params.masks.len() {
                s.params.masks.push(Mask { name: format!("Mask {}", s.params.masks.len() + 1), ..Default::default() });
                s.mask_ui.sel = s.params.masks.len() - 1;
            }
        };
        // Colour pick.
        if self.mask_ui.pick && resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let uv = (pos - r.min) / r.size();
            let [pw, ph] = self.photo_wh;
            let f = [(uv.x * cw - ox) / pw, (uv.y * ch - oy) / ph];
            if let Some(c) = self.sample_rgb(f) {
                ensure(self);
                let rgb = c.map(|v| v as f32 / 255.0);
                let op = self.mask_ui.op;
                let m = &mut self.params.masks[self.mask_ui.sel];
                match m.parts.iter_mut().rev().find_map(|p| if let Shape::Color { rgb, .. } = &mut p.shape { Some(rgb) } else { None }) {
                    Some(t) => *t = rgb,
                    None => {
                        let op = if m.parts.is_empty() { MaskOp::Add } else { op };
                        m.parts.push(MaskPart { shape: Shape::Color { rgb, range: 30.0 }, op, invert: false });
                    }
                }
            }
            self.mask_ui.pick = false;
            return;
        }
        if self.mask_ui.pick {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        // Handles of the selected mask's shapes, on screen: (part, handle, position).
        let handles: Vec<(usize, u8, Pos2)> = self.params.masks.get(self.mask_ui.sel).map(|m| m.parts.iter().enumerate().flat_map(|(k, part)| match &part.shape {
            Shape::Linear { a, b } => vec![(k, 1, scr(*a)), (k, 2, scr(*b))],
            Shape::Radial { c, rx, ry, angle, .. } => {
                let cs = scr(*c);
                let (sn, co) = angle.to_radians().sin_cos();
                vec![(k, 1, cs), (k, 2, cs + vec2(co, sn) * (*rx * to_px)), (k, 3, cs + vec2(-sn, co) * (*ry * to_px))]
            }
            _ => vec![],
        }).collect()).unwrap_or_default();
        let hit = |pos: Pos2| handles.iter().filter(|h| h.2.distance(pos) < 9.0).min_by(|a, b| a.2.distance(pos).total_cmp(&b.2.distance(pos))).map(|h| (h.0, h.1));
        if resp.secondary_clicked()
            && let Some((k, _)) = resp.interact_pointer_pos().and_then(hit)
        {
            self.params.masks[self.mask_ui.sel].parts.remove(k);
            return;
        }
        let rad = self.mask_ui.size / 100.0;
        if resp.drag_started()
            && let Some(pos) = ui.input(|i| i.pointer.press_origin())
        {
            if let Some(h) = hit(pos) {
                self.mask_ui.drag = Some(h);
            } else if self.mask_ui.draw != Draw::Brush {
                ensure(self);
                let f = frame_at(self, pos);
                let m = &mut self.params.masks[self.mask_ui.sel];
                let op = if m.parts.is_empty() { MaskOp::Add } else { self.mask_ui.op };
                let shape = if self.mask_ui.draw == Draw::Linear { Shape::Linear { a: f, b: f } } else { Shape::Radial { c: f, rx: 0.0, ry: 0.0, angle: 0.0, feather: 50.0 } };
                m.parts.push(MaskPart { shape, op, invert: false });
                self.mask_ui.drag = Some((m.parts.len() - 1, 0));
            }
        }
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
            && let Some((k, h)) = self.mask_ui.drag
        {
            let f = frame_at(self, pos);
            let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
            if let Some(part) = self.params.masks.get_mut(self.mask_ui.sel).and_then(|m| m.parts.get_mut(k)) {
                match (&mut part.shape, h) {
                    (Shape::Linear { b, .. }, 0 | 2) => *b = f,
                    (Shape::Linear { a, .. }, 1) => *a = f,
                    (Shape::Radial { rx, ry, .. }, 0) => {
                        let d = pos - origin;
                        (*rx, *ry) = (d.x.abs().max(d.length() * 0.2) / to_px, d.y.abs().max(d.length() * 0.2) / to_px);
                    }
                    (Shape::Radial { c, .. }, 1) => *c = f,
                    (Shape::Radial { c, rx, ry, angle, .. }, 2 | 3) => {
                        let d = pos - scr(*c);
                        let (sn, co) = angle.to_radians().sin_cos();
                        if h == 2 {
                            *rx = (d.x * co + d.y * sn).abs() / to_px;
                        } else {
                            *ry = (-d.x * sn + d.y * co).abs() / to_px;
                        }
                    }
                    _ => {}
                }
            }
        }
        if resp.drag_stopped() {
            // A click-sized drag draws nothing.
            if let Some((k, 0)) = self.mask_ui.drag
                && let Some(m) = self.params.masks.get_mut(self.mask_ui.sel)
                && let Some(part) = m.parts.get(k)
            {
                let tiny = match &part.shape {
                    Shape::Linear { a, b } => scr(*a).distance(scr(*b)) < 8.0,
                    Shape::Radial { rx, ry, .. } => rx.max(*ry) * to_px < 6.0,
                    _ => false,
                };
                if tiny {
                    m.parts.remove(k);
                }
            }
            self.mask_ui.drag = None;
        }
        // Brush: paint into this mask's last brush shape of the chosen kind (add or subtract), or a new one.
        if self.mask_ui.draw == Draw::Brush
            && self.mask_ui.drag.is_none()
            && (resp.dragged() || resp.clicked())
            && let Some(pos) = resp.interact_pointer_pos()
        {
            ensure(self);
            let f = frame_at(self, pos);
            let (op, erase) = (self.mask_ui.op, self.mask_ui.erase);
            let m = &mut self.params.masks[self.mask_ui.sel];
            let op = if m.parts.is_empty() { MaskOp::Add } else { op };
            if !matches!(m.parts.last(), Some(MaskPart { shape: Shape::Brush { .. }, op: o, .. }) if *o == op) {
                m.parts.push(MaskPart { shape: Shape::Brush { dabs: vec![] }, op, invert: false });
            }
            if let Some(MaskPart { shape: Shape::Brush { dabs }, .. }) = m.parts.last_mut() {
                // A dab every third of a radius along the stroke keeps it smooth however fast the mouse moves.
                let sign = if erase { -1.0 } else { 1.0 };
                let step = rad * fw.max(fh) * 0.33;
                match dabs.last().copied().filter(|_| !resp.drag_started() && !resp.clicked()) {
                    Some(d) => {
                        let n = (((d[0] - f[0]) * fw).hypot((d[1] - f[1]) * fh) / step).floor() as usize;
                        for k in 1..=n.min(200) {
                            let t = k as f32 / n as f32;
                            dabs.push([d[0] + (f[0] - d[0]) * t, d[1] + (f[1] - d[1]) * t, rad, sign]);
                        }
                    }
                    None => dabs.push([f[0], f[1], rad, sign]),
                }
            }
        }
        // Draw the shapes and their handles.
        let pt = ui.painter_at(r);
        if let Some(m) = self.params.masks.get(self.mask_ui.sel) {
            for part in &m.parts {
                let col = if part.op == MaskOp::Subtract { Color32::from_rgb(120, 170, 255) } else { Color32::WHITE };
                match &part.shape {
                    Shape::Linear { a, b } => {
                        let (a, b) = (scr(*a), scr(*b));
                        let d = (b - a).normalized();
                        let n = vec2(-d.y, d.x) * 4000.0;
                        pt.line_segment([a - n, a + n], Stroke::new(1.5, col));
                        pt.line_segment([b - n, b + n], Stroke::new(1.0, col.gamma_multiply(0.6)));
                        pt.line_segment([a, b], Stroke::new(1.0, col.gamma_multiply(0.6)));
                        pt.circle_filled(a, 4.5, ACCENT);
                        pt.circle_stroke(b, 4.5, Stroke::new(1.5, ACCENT));
                    }
                    Shape::Radial { c, rx, ry, angle, feather } => {
                        let cs = scr(*c);
                        let (sn, co) = angle.to_radians().sin_cos();
                        for (k, lw) in [(1.0, 1.5), (1.0 - feather / 100.0, 1.0)] {
                            let ring: Vec<Pos2> = (0..=64).map(|i| {
                                let t = i as f32 / 64.0 * std::f32::consts::TAU;
                                let (x, y) = (t.cos() * rx * k * to_px, t.sin() * ry * k * to_px);
                                cs + vec2(x * co - y * sn, x * sn + y * co)
                            }).collect();
                            pt.add(egui::Shape::line(ring, Stroke::new(lw, col.gamma_multiply(if lw > 1.2 { 1.0 } else { 0.5 }))));
                        }
                        pt.circle_filled(cs, 4.5, ACCENT);
                    }
                    _ => {}
                }
            }
            for h in handles.iter().filter(|h| h.1 >= 2) {
                pt.circle_stroke(h.2, 4.5, Stroke::new(1.5, ACCENT));
            }
        }
        if let Some(hp) = resp.hover_pos() {
            if self.mask_ui.draw == Draw::Brush && !self.mask_ui.pick {
                pt.circle_stroke(hp, rad * to_px, Stroke::new(1.0, if self.mask_ui.erase { Color32::WHITE } else { ACCENT }));
                pt.circle_stroke(hp, rad * to_px * 0.4, Stroke::new(1.0, Color32::from_white_alpha(80)));
            } else if hit(hp).is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
            } else if !self.mask_ui.pick {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }
        }
    }
}

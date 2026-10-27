//! Look and feel: palette, fonts, and the custom controls (slider, section, chips, segmented, switch, histogram).
use eframe::egui;
use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

// Brand palette (ferrotype brand kit, direction F). There is no accent colour: emphasis is bone,
// selection is shown by inverting (ink on bone). Colour belongs to the photograph.
pub const INK: Color32 = hex(0x141416);
pub const BONE: Color32 = hex(0xECE8E1);
pub const BG: Color32 = INK;
pub const PANEL: Color32 = hex(0x1C1C1F);
pub const CANVAS: Color32 = hex(0x0E0E10);
pub const BAR: Color32 = PANEL;
pub const BORDER: Color32 = hex(0x2A2A2E); // "line"
pub const DIVIDER: Color32 = BORDER;
pub const CARD: Color32 = hex(0x242428); // "raised"
pub const SEL: Color32 = hex(0x2E2E33); // hover surface
pub const BTN_BORDER: Color32 = hex(0x75726C); // 3.5:1 on the panel, 3.2:1 on raised cards (WCAG 1.4.11)
pub const TEXT: Color32 = BONE;
pub const TEXT2: Color32 = hex(0xB5B1AA); // "mid"
pub const TEXT3: Color32 = hex(0x8A877F); // "dim": secondary labels and metadata only
pub const LABEL: Color32 = TEXT2;
/// Kept as a name for "emphasis"; in this brand it is bone, never a hue.
pub const ACCENT: Color32 = BONE;
/// Secondary text on a bone (selected) surface.
pub const ON_BONE: Color32 = hex(0x5A5752);
const TRACK: Color32 = hex(0x4A4846); // 3:1 against the panel

pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Colour from linear-light RGB (what the pipeline works in), so swatches match rendered pixels.
pub fn lin(c: [f32; 3]) -> Color32 {
    let e = c.map(|v| (crate::load::linear_to_srgb(v) * 255.0 + 0.5) as u8);
    Color32::from_rgb(e[0], e[1], e[2])
}

/// Colour from display-encoded (sRGB) 0..1 values.
pub fn enc(c: [f32; 3]) -> Color32 {
    let e = c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    Color32::from_rgb(e[0], e[1], e[2])
}

pub fn hsv(h: f32, s: f32, v: f32) -> Color32 {
    enc(crate::develop::hsv_to_rgb(h, s.clamp(0.0, 1.0), v.clamp(0.0, 1.0)))
}

pub fn install_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        ("plex", &include_bytes!("../assets/fonts/IBMPlexSans-400.ttf")[..]),
        ("plex-500", &include_bytes!("../assets/fonts/IBMPlexSans-500.ttf")[..]),
        ("plex-mono", &include_bytes!("../assets/fonts/IBMPlexMono-400.ttf")[..]),
        ("plex-mono-500", &include_bytes!("../assets/fonts/IBMPlexMono-500.ttf")[..]),
        ("bricolage", &include_bytes!("../assets/fonts/BricolageGrotesque-700.ttf")[..]),
    ] {
        fonts.font_data.insert(name.into(), std::sync::Arc::new(egui::FontData::from_static(bytes)));
    }
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    let mono_fallback = fonts.families[&FontFamily::Monospace].clone();
    fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "plex".into());
    fonts.families.get_mut(&FontFamily::Monospace).unwrap().insert(0, "plex-mono".into());
    fonts.families.insert(FontFamily::Name("semi".into()), [vec!["plex-500".to_string()], fallback.clone()].concat());
    fonts.families.insert(FontFamily::Name("mono-500".into()), [vec!["plex-mono-500".to_string()], mono_fallback].concat());
    fonts.families.insert(FontFamily::Name("display".into()), [vec!["bricolage".to_string()], fallback].concat());
    ctx.set_fonts(fonts);

    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    ctx.all_styles_mut(|s| {
        use egui::TextStyle::*;
        s.text_styles = [
            (Heading, FontId::new(15.0, semi())),
            (Body, FontId::proportional(13.0)),
            (Button, FontId::proportional(12.5)),
            (Small, FontId::proportional(11.0)),
            (Monospace, FontId::monospace(11.5)),
        ].into();
        s.spacing.item_spacing = vec2(6.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 5.0);
        s.spacing.interact_size.y = 28.0;
        s.spacing.scroll.bar_width = 6.0;
        s.spacing.scroll.floating = true;
        let v = &mut s.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = PANEL;
        v.window_fill = PANEL;
        v.extreme_bg_color = DIVIDER; // text fields
        v.faint_bg_color = CARD;
        v.override_text_color = Some(TEXT);
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_shadow = egui::Shadow::NONE; // flat, 1 px, no shadows
        v.popup_shadow = egui::Shadow::NONE;
        // Selected text and selected widgets invert: ink on bone.
        v.selection.bg_fill = BONE;
        v.selection.stroke = Stroke::new(1.0, INK);
        v.text_cursor.stroke = Stroke::new(1.5, BONE);
        v.hyperlink_color = BONE;
        v.window_corner_radius = CornerRadius::same(4);
        // Disabled controls must look clearly off, not just a shade darker.
        v.disabled_alpha = 0.35;
        v.menu_corner_radius = CornerRadius::same(4);
        let r = CornerRadius::same(3);
        let w = &mut v.widgets;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT2);
        for (st, fill, border) in [(&mut w.inactive, CARD, BTN_BORDER), (&mut w.hovered, SEL, TEXT3), (&mut w.active, hex(0x38383D), TEXT2), (&mut w.open, SEL, BTN_BORDER)] {
            st.bg_fill = fill;
            st.weak_bg_fill = fill;
            st.bg_stroke = Stroke::new(1.0, border);
            st.corner_radius = r;
            st.expansion = 0.0;
            st.fg_stroke = Stroke::new(1.0, TEXT);
        }
        w.inactive.fg_stroke = Stroke::new(1.0, LABEL);
    });
}

/// IBM Plex Sans 500: emphasised labels and dialog titles.
/// The bare brand mark (assets/brand/mark.svg): a filled frame and an outlined frame, offset down and right.
/// Axis-aligned, two frames, one colour, at any size.
pub fn paint_mark(p: &egui::Painter, r: Rect, color: Color32) {
    let k = r.width().min(r.height()) / 72.0;
    let at = |x: f32, y: f32, w: f32| Rect::from_min_size(r.min + vec2(x, y) * k, vec2(w, w) * k);
    let rad = 9.0 * k;
    p.rect_filled(at(2.0, 2.0, 44.0), rad, color);
    let stroke = (4.0 * k).max(1.25);
    p.rect_stroke(at(26.0, 26.0, 44.0), rad, Stroke::new(stroke, color), egui::StrokeKind::Middle);
}

pub fn semi() -> FontFamily {
    FontFamily::Name("semi".into())
}

/// IBM Plex Mono 500: section labels, values.
pub fn mono500() -> FontFamily {
    FontFamily::Name("mono-500".into())
}

/// Bricolage Grotesque 700: the wordmark, one title per view and empty-state headlines. Never in a panel, button or menu.
pub fn display() -> FontFamily {
    FontFamily::Name("display".into())
}

/// Section label in the brand's mono caps, e.g. "WHITE BALANCE".
pub fn caps(text: &str) -> RichText {
    RichText::new(text.to_uppercase()).size(10.5).color(TEXT3).family(mono500()).extra_letter_spacing(1.6)
}

/// The screen's one primary action: inverted, ink on bone.
pub fn primary(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(text).color(INK).family(semi())).fill(BONE).stroke(Stroke::NONE))
}

/// Borderless text button; `on` inverts it (ink on bone).
pub fn ghost(ui: &mut Ui, text: &str, on: bool) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(if on { INK } else { TEXT2 })).fill(if on { BONE } else { Color32::TRANSPARENT }).stroke(Stroke::NONE))
}

/// Top-bar view tab, as in the brand UI: dim text, the current one bone with a 2 px bone underline.
pub fn tab(ui: &mut Ui, text: &str, on: bool) -> egui::Response {
    let resp = ui.add(egui::Button::new(RichText::new(text).size(13.0).color(if on { BONE } else { TEXT3 }))
        .fill(Color32::TRANSPARENT).stroke(Stroke::NONE).min_size(vec2(0.0, 28.0)));
    let r = resp.rect;
    if on {
        ui.painter().hline(r.x_range().shrink(4.0), r.bottom() + 1.0, Stroke::new(2.0, BONE));
    } else if resp.hovered() {
        ui.painter().hline(r.x_range().shrink(4.0), r.bottom() + 1.0, Stroke::new(1.0, TEXT3));
    }
    resp
}

/// Multi-select filter chip (several can be on): bone outline and bone text when on, dim hairline when off.
/// Calmer than the inverted `chip`, so a row of them doesn't outshine the photos.
pub fn filter_chip(ui: &mut Ui, text: &str, on: bool) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).size(11.5).color(if on { BONE } else { TEXT3 }))
        .fill(if on { CARD } else { Color32::TRANSPARENT })
        .stroke(Stroke::new(1.0, if on { BONE } else { BORDER }))
        .corner_radius(3)
        .min_size(vec2(0.0, 24.0)))
}

/// A dialog: one at a time, over a dimmed page, closed by Esc, a click outside or ×.
/// Title in Plex Sans 500 on the left, as panels are. Returns false once it should close.
pub fn dialog(ctx: &egui::Context, id: &str, title: &str, width: f32, body: impl FnOnce(&mut Ui)) -> bool {
    let frame = egui::Frame::new().fill(PANEL).stroke(Stroke::new(1.0, BORDER)).corner_radius(6).inner_margin(egui::Margin::same(18));
    let r = egui::Modal::new(egui::Id::unique(("dialog", id))).frame(frame).backdrop_color(Color32::from_rgba_unmultiplied(8, 8, 9, 160)).show(ctx, |ui| {
        ui.set_width(width);
        let mut close = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).size(15.0).family(semi()).color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.add(egui::Button::new(RichText::new("×").size(16.0).color(TEXT2)).fill(Color32::TRANSPARENT).stroke(Stroke::NONE).min_size(vec2(24.0, 24.0)))
                    .on_hover_text("Close (Esc)").clicked();
            });
        });
        ui.add_space(8.0);
        body(ui);
        close
    });
    !(r.inner || r.should_close())
}

/// Filter chip: hairline outline, inverted when on.
pub fn chip(ui: &mut Ui, text: &str, on: bool) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).size(11.5).color(if on { INK } else { TEXT2 }))
        .fill(if on { BONE } else { Color32::TRANSPARENT })
        .stroke(Stroke::new(1.0, if on { BONE } else { BORDER }))
        .corner_radius(3)
        .min_size(vec2(0.0, 24.0)))
}

/// Toggle button used in grids (looks, sources): fills the given width; inverted when on.
pub fn tile(ui: &mut Ui, text: &str, on: bool, width: f32) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).size(12.0).color(if on { INK } else { TEXT2 }))
        .fill(if on { BONE } else { Color32::TRANSPARENT })
        .stroke(Stroke::new(1.0, if on { BONE } else { BORDER }))
        .min_size(vec2(width, 30.0)))
}

/// Segmented control: one flat well with the active option inverted.
pub fn segmented(ui: &mut Ui, value: &mut usize, options: &[&str]) {
    egui::Frame::new().fill(INK).stroke(Stroke::new(1.0, BORDER)).corner_radius(3).inner_margin(2.0).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.horizontal(|ui| {
            for (i, o) in options.iter().enumerate() {
                let on = *value == i;
                let b = egui::Button::new(RichText::new(*o).size(12.0).color(if on { INK } else { TEXT2 }))
                    .fill(if on { BONE } else { Color32::TRANSPARENT })
                    .stroke(Stroke::NONE)
                    .corner_radius(2)
                    .min_size(vec2(0.0, 24.0));
                if ui.add(b).clicked() {
                    *value = i;
                }
            }
        });
    });
}

/// On/off switch: a hairline well that fills with bone when on.
pub fn switch(ui: &mut Ui, on: &mut bool) -> egui::Response {
    // 24 px tall hit area (the minimum target size), 17 px tall drawing.
    let (hit, resp) = ui.allocate_exact_size(vec2(30.0, 24.0), Sense::click());
    let rect = Rect::from_center_size(hit.center(), vec2(30.0, 17.0));
    if resp.clicked() {
        *on = !*on;
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, true, *on, ""));
    let t = ui.ctx().animate_bool(resp.id, *on);
    let p = ui.painter();
    p.rect_filled(rect, 8.5, INK.lerp_to_gamma(BONE, t));
    p.rect_stroke(rect, 8.5, Stroke::new(1.0, BTN_BORDER.lerp_to_gamma(BONE, t)), egui::StrokeKind::Inside);
    p.circle_filled(pos2(rect.left() + 8.5 + t * 13.0, rect.center().y), 5.5, TEXT2.lerp_to_gamma(INK, t));
    resp
}

/// Collapsible panel section: 40 px header, "+"/"−" on the right, hairline below.
pub fn section(ui: &mut Ui, title: &str, open: &mut bool, body: impl FnOnce(&mut Ui)) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if resp.clicked() {
        *open = !*open;
    }
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 0.0, hex(0x202024));
    }
    // Panel titles in Plex Sans 500; the mono caps are kept for the groups inside.
    p.text(pos2(rect.left() + 16.0, rect.center().y), Align2::LEFT_CENTER, title, FontId::new(13.0, semi()), if *open || resp.hovered() { TEXT } else { TEXT2 });
    p.text(pos2(rect.right() - 16.0, rect.center().y), Align2::RIGHT_CENTER, if *open { "−" } else { "+" }, FontId::proportional(14.0), TEXT3);
    if *open {
        egui::Frame::new().inner_margin(egui::Margin { left: 16, right: 16, top: 0, bottom: 14 }).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            body(ui);
        });
    }
    let y = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, DIVIDER));
}

/// Group title row with an optional small action on the right ("Auto", "As shot").
pub fn group(ui: &mut Ui, title: &str, action: Option<&str>) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.label(caps(title));
        if let Some(a) = action {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                clicked = ui.add(egui::Label::new(RichText::new(a).size(11.0).color(TEXT2)).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
            });
        }
    });
    clicked
}

pub type Grad<'a> = &'a dyn Fn(f32) -> Color32;

/// Slider in the mockup's layout: label and value on one line, thin full-width track below.
/// `grad` maps a slider value to the colour that value produces, sampled densely so the track is smooth.
/// Drag anywhere on the row; double-click resets; ←/→ nudge while hovered; Shift for fine steps.
pub fn slider(ui: &mut Ui, label: &str, v: &mut f32, min: f32, max: f32, default: f32, grad: Option<Grad>) -> bool {
    slider_ex(ui, label, None, v, min, max, default, grad)
}

#[allow(clippy::too_many_arguments)]
pub fn slider_ex(ui: &mut Ui, label: &str, swatch: Option<Color32>, v: &mut f32, min: f32, max: f32, default: f32, grad: Option<Grad>) -> bool {
    let before = *v;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click_and_drag());
    let step = if max - min <= 20.0 { if max - min <= 4.0 { 0.01 } else { 0.05 } } else { 1.0 };
    let changed = (*v - default).abs() > step * 0.5;

    // Input first so the frame paints the new value.
    let track = Rect::from_min_max(pos2(rect.left() + 6.0, rect.bottom() - 9.0), pos2(rect.right() - 6.0, rect.bottom() - 5.0));
    let frac = |x: f32| ((x - min) / (max - min)).clamp(0.0, 1.0);
    if resp.double_clicked() {
        *v = default;
    } else if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let dx = resp.drag_delta().x / track.width() * (max - min);
        if resp.drag_started() && !fine {
            if let Some(p) = resp.interact_pointer_pos() {
                *v = min + ((p.x - track.left()) / track.width()).clamp(0.0, 1.0) * (max - min);
            }
        } else {
            *v += if fine { dx * 0.15 } else { dx };
        }
        *v = ((v.clamp(min, max)) / step).round() * step;
    } else if resp.clicked() && let Some(p) = resp.interact_pointer_pos() {
        *v = (((min + ((p.x - track.left()) / track.width()).clamp(0.0, 1.0) * (max - min)) / step).round() * step).clamp(min, max);
    }
    // Tab focuses a slider; arrows then nudge it, like hovering does.
    if resp.hovered() || resp.has_focus() {
        // Cmd/Ctrl+arrows switch photos; don't also nudge the slider under the mouse.
        let n = ui.input(|i| if i.modifiers.command { 0.0 } else { (i.key_pressed(egui::Key::ArrowRight) as i32 - i.key_pressed(egui::Key::ArrowLeft) as i32) as f32 });
        let big = ui.input(|i| i.modifiers.shift);
        *v = (*v + n * step * if big { 10.0 } else { 1.0 }).clamp(min, max);
    }

    let p = ui.painter_at(rect.expand(2.0));
    let mut x = rect.left();
    if let Some(c) = swatch {
        p.circle_filled(pos2(x + 4.0, rect.top() + 8.0), 4.0, c);
        x += 14.0;
    }
    p.text(pos2(x, rect.top() + 8.0), Align2::LEFT_CENTER, label, FontId::proportional(12.0), if resp.hovered() || resp.dragged() { TEXT } else { LABEL });
    let shown = if step < 1.0 && min < 0.0 { format!("{:+.2}", *v) } else if step < 1.0 { format!("{:.1}", *v) } else if max > 1000.0 { format!("{:.0} K", *v) } else if min < 0.0 { format!("{:+.0}", *v) } else { format!("{:.0}", *v) };
    let shown = if !changed && min < 0.0 && max <= 1000.0 { shown.trim_start_matches('+').to_string() } else { shown };
    p.text(pos2(rect.right(), rect.top() + 8.0), Align2::RIGHT_CENTER, shown, FontId::monospace(11.0), if changed { ACCENT } else { TEXT3 });

    match grad {
        Some(g) => paint_gradient_track(ui, track, min, max, g),
        None => {
            let thin = Rect::from_center_size(track.center(), vec2(track.width(), 2.0));
            p.rect_filled(thin, 1.0, TRACK);
            let (a, b) = (thin.left() + thin.width() * frac(default.clamp(min, max)), thin.left() + thin.width() * frac(*v));
            if changed {
                p.rect_filled(Rect::from_x_y_ranges(a.min(b)..=a.max(b), thin.y_range()), 1.0, ACCENT.gamma_multiply(0.85));
            }
            // Tick at the default when it is not an end.
            if default > min && default < max {
                p.vline(a, (thin.center().y - 3.0)..=(thin.center().y + 3.0), Stroke::new(1.0, TEXT3));
            }
        }
    }
    let knob = pos2(track.left() + track.width() * frac(*v), track.center().y);
    let active = resp.hovered() || resp.dragged();
    // Flat bone tick, as in the brand UI; an ink outline keeps it visible on colour tracks.
    let knob_r = Rect::from_center_size(knob, if active { vec2(4.0, 16.0) } else { vec2(3.0, 13.0) });
    if grad.is_some() {
        p.rect_filled(knob_r.expand(1.5), 1.5, INK);
    }
    p.rect_filled(knob_r, 1.0, BONE);

    if resp.has_focus() {
        p.rect_stroke(rect.expand(1.0), 4.0, Stroke::new(1.5, ACCENT), egui::StrokeKind::Outside);
    }
    let val = *v as f64;
    resp.widget_info(|| egui::WidgetInfo::slider(true, val, label));
    resp.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    *v != before
}

/// Smooth RGB histogram as overlapping translucent areas, like the mockup.
/// Smooth RGB histogram, drawn per device pixel: anti-aliased tops, channels mixed additively
/// (red + green reads yellow, all three read neutral grey, like Lightroom).
pub fn histogram(ui: &mut Ui, bins: &[[f32; 128]; 3]) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 72.0), Sense::hover());
    let ppp = ui.ctx().pixels_per_point();
    let (w, h) = ((rect.width() * ppp).round().max(2.0) as usize, (rect.height() * ppp).round().max(2.0) as usize);
    // One texture, redrawn only when the histogram or its size changes.
    let id = egui::Id::NULL.with("ferrotype-histogram");
    let bytes: Vec<u8> = bins.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    let key = hash_of(&[&bytes, &(w as u32).to_le_bytes(), &(h as u32).to_le_bytes()]);
    let stored = ui.ctx().memory(|m| m.data.get_temp::<(u64, egui::TextureHandle)>(id));
    if let Some((k, t)) = &stored
        && *k == key
    {
        ui.painter().image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        return;
    }
    let cols = [[0.78f32, 0.27, 0.23], [0.27, 0.62, 0.36], [0.27, 0.43, 0.78]];
    // Column heights in pixels, linearly interpolated between bins.
    let heights: Vec<[f32; 3]> = (0..w).map(|x| {
        let f = x as f32 / (w - 1) as f32 * 127.0;
        let (i, t) = (f.floor() as usize, f.fract());
        [0, 1, 2].map(|c| (bins[c][i] * (1.0 - t) + bins[c][(i + 1).min(127)] * t).min(1.0) * h as f32 * 0.95)
    }).collect();
    let mut img = egui::ColorImage::filled([w, h], Color32::TRANSPARENT);
    for y in 0..h {
        let from_bottom = (h - y) as f32 - 0.5;
        for x in 0..w {
            let cov = heights[x].map(|top| (top - from_bottom + 0.5).clamp(0.0, 1.0));
            let a = cov[0].max(cov[1]).max(cov[2]);
            if a <= 0.0 {
                continue;
            }
            let rgb = [0, 1, 2].map(|k| ((cov[0] * cols[0][k] + cov[1] * cols[1][k] + cov[2] * cols[2][k]) / a).min(0.85));
            img.pixels[y * w + x] = Color32::from_rgba_unmultiplied((rgb[0] * 255.0) as u8, (rgb[1] * 255.0) as u8, (rgb[2] * 255.0) as u8, (a * 0.8 * 255.0) as u8);
        }
    }
    let tex = match stored {
        Some((_, mut t)) => {
            t.set(img, egui::TextureOptions::LINEAR);
            t
        }
        None => ui.ctx().load_texture("histogram", img, egui::TextureOptions::LINEAR),
    };
    ui.ctx().memory_mut(|m| m.data.insert_temp(id, (key, tex.clone())));
    ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}

pub fn histogram_bins(img: &image::Rgb32FImage) -> [[f32; 128]; 3] {
    let mut raw = [[0f32; 128]; 3];
    for px in img.chunks(3).step_by(2) {
        for c in 0..3 {
            raw[c][((px[c] * 127.99) as usize).min(127)] += 1.0;
        }
    }
    // Light smoothing, then normalise to the tallest bin (the square root below keeps small bins visible).
    let smooth = raw.map(|b| std::array::from_fn::<f32, 128, _>(|i| {
        let (a, z) = (i.saturating_sub(2), (i + 2).min(127));
        b[a..=z].iter().sum::<f32>() / (z - a + 1) as f32
    }));
    let top = smooth.iter().flatten().fold(1.0f32, |m, v| m.max(*v));
    smooth.map(|b| b.map(|v| (v / top).sqrt().min(1.0) * 0.98))
}

// ---------- smooth, anti-aliased gradient images (meshes are not anti-aliased in egui) ----------

type TexCache = std::collections::HashMap<u64, egui::TextureHandle>;

/// Texture built once per distinct content and reused; the cache is dropped when it grows large.
fn cached(ctx: &egui::Context, key: u64, make: impl FnOnce() -> egui::ColorImage) -> egui::TextureHandle {
    let id = egui::Id::NULL.with("ferrotype-gradient-cache");
    if let Some(t) = ctx.memory_mut(|m| m.data.get_temp_mut_or_default::<TexCache>(id).get(&key).cloned()) {
        return t;
    }
    let t = ctx.load_texture(format!("grad{key}"), make(), egui::TextureOptions::LINEAR);
    ctx.memory_mut(|m| {
        let c = m.data.get_temp_mut_or_default::<TexCache>(id);
        if c.len() > 400 {
            c.clear();
        }
        c.insert(key, t.clone());
    });
    t
}

fn hash_of(parts: &[&[u8]]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

/// Paint a capsule-shaped track whose colour at each device pixel is `g` of the slider value there.
fn paint_gradient_track(ui: &Ui, track: Rect, min: f32, max: f32, g: Grad) {
    let ppp = ui.ctx().pixels_per_point();
    let (w, h) = ((track.width() * ppp).round().max(2.0) as usize, (track.height() * ppp).round().max(2.0) as usize);
    let cols: Vec<Color32> = (0..w).map(|x| g(min + (x as f32 + 0.5) / w as f32 * (max - min))).collect();
    let bytes: Vec<u8> = cols.iter().flat_map(|c| c.to_array()).collect();
    let key = hash_of(&[&bytes, &(w as u32).to_le_bytes(), &(h as u32).to_le_bytes()]);
    let tex = cached(ui.ctx(), key, || {
        let r = h as f32 / 2.0;
        let mut img = egui::ColorImage::filled([w, h], Color32::TRANSPARENT);
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let cx = px.clamp(r, w as f32 - r);
                let d = ((px - cx).powi(2) + (py - r).powi(2)).sqrt();
                let a = (r - d + 0.5).clamp(0.0, 1.0);
                let mut c = cols[x];
                // Dark-to-light tone tracks read as half filled at 0: dim them, keep colour tracks as they are.
                if c.r().max(c.g()).max(c.b()) - c.r().min(c.g()).min(c.b()) < 24 {
                    c = c.gamma_multiply(0.62);
                }
                img.pixels[y * w + x] = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a * 255.0) as u8);
            }
        }
        img
    });
    ui.painter().image(tex.id(), track, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}

/// Smooth hue/saturation wheel (hue around, saturation outwards), anti-aliased rim.
pub fn paint_color_wheel(ui: &Ui, rect: Rect) {
    let ppp = ui.ctx().pixels_per_point();
    let n = (rect.width() * ppp * 2.0).round().max(8.0) as usize; // 2x supersampled, filtered down by the GPU
    let tex = cached(ui.ctx(), hash_of(&[b"wheel", &(n as u32).to_le_bytes()]), || {
        let r = n as f32 / 2.0;
        let mut img = egui::ColorImage::filled([n, n], Color32::TRANSPARENT);
        for y in 0..n {
            for x in 0..n {
                let (dx, dy) = (x as f32 + 0.5 - r, r - (y as f32 + 0.5));
                let d = (dx * dx + dy * dy).sqrt();
                let a = (r - d).clamp(0.0, 1.5) / 1.5;
                if a > 0.0 {
                    let hue = dy.atan2(dx).to_degrees().rem_euclid(360.0);
                    let c = hsv(hue, (d / r).min(1.0) * 0.85, 0.82);
                    img.pixels[y * n + x] = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a * 255.0) as u8);
                }
            }
        }
        img
    });
    ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}

/// A label with an on/off switch at the right edge of the row.
pub fn switch_row(ui: &mut Ui, label: &str, on: &mut bool) -> egui::Response {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(12.0).color(LABEL));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| switch(ui, on)).inner
    }).inner
}

/// Clickable file-name tokens that add themselves to `pattern`.
pub fn token_chips(ui: &mut Ui, pattern: &mut String) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for (t, what) in crate::output::NAME_TOKENS {
            if ui.add(egui::Button::new(egui::RichText::new(*t).monospace().size(10.5).color(TEXT2)).fill(CARD).corner_radius(4)).on_hover_text(*what).clicked() {
                if !pattern.is_empty() && !pattern.ends_with(['-', '_', ' ']) {
                    pattern.push('-');
                }
                pattern.push_str(t);
            }
        }
    });
}

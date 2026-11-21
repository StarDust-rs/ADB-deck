//! Reusable, themed building blocks. Panels compose these instead of using
//! raw egui widgets for anything that carries visual identity.

use egui::{
    vec2, Align, Align2, Color32, CornerRadius, FontId, Frame, InnerResponse, Layout, Margin, Response, RichText, Sense, Stroke, Ui,
};

pub use egui_phosphor::regular as ph;

use super::theme::{self, pal, Palette, CONTROL_H, RADIUS_CARD, RADIUS_CONTROL};

// ---- semantic colours -----------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

impl Kind {
    pub fn fg(self, p: &Palette) -> Color32 {
        match self {
            Kind::Neutral => p.text2,
            Kind::Accent => p.accent_text,
            Kind::Success => p.success,
            Kind::Warning => p.warning,
            Kind::Danger => p.danger,
        }
    }

    pub fn soft(self, p: &Palette) -> Color32 {
        match self {
            Kind::Neutral => p.hover,
            Kind::Accent => p.accent_soft,
            Kind::Success => p.success_soft,
            Kind::Warning => p.warning_soft,
            Kind::Danger => p.danger_soft,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Kind::Neutral | Kind::Accent => ph::INFO,
            Kind::Success => ph::CHECK_CIRCLE,
            Kind::Warning => ph::WARNING,
            Kind::Danger => ph::WARNING_CIRCLE,
        }
    }
}

// ---- text ---------------------------------------------------------------

pub fn semibold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(theme::family_semibold())
}

pub fn medium(text: impl Into<String>) -> RichText {
    RichText::new(text).family(theme::family_medium())
}

// ---- buttons ----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BtnKind {
    Secondary,
    Primary,
    Subtle,
    Danger,
}

pub struct Btn<'a> {
    text: &'a str,
    icon: Option<&'a str>,
    kind: BtnKind,
    enabled: bool,
    tip: Option<&'a str>,
    min_w: f32,
    selected: bool,
}

impl<'a> Btn<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text, icon: None, kind: BtnKind::Secondary, enabled: true, tip: None, min_w: 0.0, selected: false }
    }

    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn primary(mut self) -> Self {
        self.kind = BtnKind::Primary;
        self
    }

    pub fn subtle(mut self) -> Self {
        self.kind = BtnKind::Subtle;
        self
    }

    pub fn danger(mut self) -> Self {
        self.kind = BtnKind::Danger;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn tip(mut self, tip: &'a str) -> Self {
        self.tip = Some(tip);
        self
    }

    pub fn min_width(mut self, w: f32) -> Self {
        self.min_w = w;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = pal(ui);
        let label = match (self.icon, self.text.is_empty()) {
            (Some(i), true) => i.to_string(),
            (Some(i), false) => format!("{i}  {}", self.text),
            (None, _) => self.text.to_string(),
        };
        let mut min_w = self.min_w;
        if self.text.is_empty() {
            min_w = min_w.max(CONTROL_H);
        }
        let resp = ui
            .scope(|ui| {
                let w = &mut ui.visuals_mut().widgets;
                match self.kind {
                    BtnKind::Secondary => {}
                    BtnKind::Primary => {
                        for (s, fill) in [(&mut w.inactive, p.accent), (&mut w.hovered, p.accent_hover), (&mut w.active, p.accent_press)] {
                            s.weak_bg_fill = fill;
                            s.bg_fill = fill;
                            s.bg_stroke = Stroke::NONE;
                            s.fg_stroke = Stroke::new(1.0_f32, p.on_accent);
                        }
                    }
                    BtnKind::Subtle => {
                        for (s, fill) in [(&mut w.inactive, Color32::TRANSPARENT), (&mut w.hovered, p.hover), (&mut w.active, p.press)] {
                            s.weak_bg_fill = fill;
                            s.bg_fill = fill;
                            s.bg_stroke = Stroke::NONE;
                        }
                        w.inactive.fg_stroke = Stroke::new(1.0_f32, p.text2);
                    }
                    BtnKind::Danger => {
                        for (s, fill) in [(&mut w.inactive, p.control), (&mut w.hovered, p.danger_soft), (&mut w.active, p.danger_soft)] {
                            s.weak_bg_fill = fill;
                            s.bg_fill = fill;
                            s.fg_stroke = Stroke::new(1.0_f32, p.danger);
                        }
                    }
                }
                let mut b = egui::Button::new(RichText::new(label)).min_size(vec2(min_w, CONTROL_H));
                if self.selected {
                    b = b.selected(true);
                }
                ui.add_enabled(self.enabled, b)
            })
            .inner;
        match self.tip {
            Some(t) => resp.on_hover_text(t).on_disabled_hover_text(t),
            None => resp,
        }
    }
}

pub fn btn(ui: &mut Ui, text: &str) -> Response {
    Btn::new(text).show(ui)
}

/// Square icon-only button without a frame until hovered.
pub fn icon_btn(ui: &mut Ui, icon: &str, tip: &str) -> Response {
    Btn::new("").icon(icon).subtle().tip(tip).show(ui)
}

// ---- containers ------------------------------------------------------------

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    card_margin(ui, 16, add)
}

pub fn card_margin<R>(ui: &mut Ui, margin: i8, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let p = pal(ui);
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.stroke))
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .inner_margin(Margin::same(margin))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
}

/// A card whose content is exactly `width` wide (for cards laid out side by side).
pub fn card_sized<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let p = pal(ui);
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.stroke))
        .corner_radius(CornerRadius::same(RADIUS_CARD))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.vertical(add).inner
        })
}

/// A card with an icon + title row.
pub fn card_titled<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    card(ui, |ui| {
        ui.label(semibold(title).size(14.5));
        ui.add_space(10.0);
        add(ui)
    })
}

/// Two titled cards side by side with equal height (measured one frame late).
pub type CardSpec<'a> = (&'a str, &'a mut dyn FnMut(&mut Ui));

pub fn card_pair<'a>(ui: &mut Ui, id_src: &str, left: CardSpec<'a>, right: CardSpec<'a>) {
    let key = ui.id().with(id_src);
    let min_h: f32 = ui.data(|d| d.get_temp(key)).unwrap_or(0.0);
    let mut heights = [0.0_f32; 2];
    ui.columns(2, |cols| {
        for (i, (col, (title, body))) in cols.iter_mut().zip([left, right]).enumerate() {
            let r = card(col, |ui| {
                ui.set_min_height((min_h - 34.0).max(0.0));
                ui.label(semibold(title).size(14.5));
                ui.add_space(10.0);
                body(ui);
            });
            heights[i] = r.response.rect.height();
        }
    });
    let h = heights[0].max(heights[1]);
    ui.data_mut(|d| d.insert_temp(key, h));
    if (h - min_h).abs() > 0.5 {
        ui.ctx().request_repaint();
    }
}

/// Small metric tile: label, big value and an optional detail line.
pub fn stat_tile(ui: &mut Ui, label: &str, value: &str, detail: &str) {
    let p = pal(ui);
    card_margin(ui, 12, |ui| {
        ui.label(RichText::new(label).size(12.0).color(p.text2));
        ui.add(egui::Label::new(semibold(value).size(19.0)).truncate());
        ui.add(egui::Label::new(RichText::new(if detail.is_empty() { " " } else { detail }).size(11.5).color(p.text3)).truncate());
    });
}

/// Grid with the label column wide enough to line up across cards.
pub fn kv_grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).min_col_width(92.0).min_row_height(20.0).spacing([16.0, 6.0]).show(ui, add);
}

/// Slider with an accent knob and an optional value readout.
pub fn slider(ui: &mut Ui, value: &mut f32, range: std::ops::RangeInclusive<f32>, step: f64, decimals: usize, width: f32) -> Response {
    let p = pal(ui);
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 18.0;
        ui.spacing_mut().slider_rail_height = 4.0;
        ui.spacing_mut().slider_width = width;
        let w = &mut ui.visuals_mut().widgets;
        for s in [&mut w.inactive, &mut w.hovered, &mut w.active] {
            s.bg_fill = p.accent;
            s.fg_stroke = Stroke::new(2.0_f32, p.card);
        }
        ui.visuals_mut().selection.bg_fill = p.accent;
        ui.visuals_mut().extreme_bg_color = p.hover;
        let r = ui.add(egui::Slider::new(value, range).step_by(step).show_value(false));
        ui.label(RichText::new(format!("{:.*}", decimals, *value)).monospace().color(p.text2));
        r
    })
    .inner
}

/// A settings row: title and hint on the left, the control right-aligned.
pub fn setting_row(ui: &mut Ui, title: &str, hint: &str, control: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    // An exact width: a plain `horizontal` here makes the side panel grow a little every frame.
    let size = egui::vec2(ui.available_width(), 30.0);
    ui.allocate_ui_with_layout(size, Layout::right_to_left(Align::Center), |ui| {
        control(ui);
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            let resp = ui.add(egui::Label::new(RichText::new(title).size(13.0)).truncate());
            if !hint.is_empty() {
                resp.on_hover_text(RichText::new(hint).color(p.text2));
            }
        });
    });
}

/// Frame for tables and other full-bleed content.
pub fn well<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let p = pal(ui);
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(4))
        .show(ui, add)
}

pub fn vsep(ui: &mut Ui) {
    let p = pal(ui);
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, CONTROL_H - 10.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.stroke_strong);
    ui.add_space(4.0);
}

/// A cell of exactly `width` x `height` with vertically centred content, so
/// the columns of hand-built rows line up whatever the content measures.
pub fn fixed_cell<R>(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(vec2(width, height), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(vec2(width, height));
        add(ui)
    })
    .inner
}

pub fn hsep(ui: &mut Ui) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.stroke);
}

/// Title block at the top of every panel. `actions` lay out right to left.
pub fn page_header(ui: &mut Ui, title: &str, subtitle: &str, actions: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).font(FontId::new(24.0, theme::family_semibold())).color(p.text));
            if !subtitle.is_empty() {
                ui.label(RichText::new(subtitle).size(13.0).color(p.text2));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), actions);
    });
    ui.add_space(18.0);
}

// ---- status -------------------------------------------------------------------

pub fn pill(ui: &mut Ui, text: &str, kind: Kind) -> Response {
    let p = pal(ui);
    Frame::new()
        .fill(kind.soft(p))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).font(FontId::new(11.5, theme::family_medium())).color(kind.fg(p))))
        .response
}

pub fn dot(ui: &mut Ui, kind: Kind) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, kind.fg(p));
}

/// Inline message with an optional action button; true when it was clicked.
pub fn banner(ui: &mut Ui, kind: Kind, text: &str, action: Option<&str>) -> bool {
    let p = pal(ui);
    let mut clicked = false;
    Frame::new().fill(kind.soft(p)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 9)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(kind.icon()).size(16.0).color(kind.fg(p)));
            if let Some(a) = action {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::new(a).show(ui).clicked() {
                        clicked = true;
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(text).color(p.text)).wrap());
                    });
                });
            } else {
                ui.add(egui::Label::new(RichText::new(text).color(p.text)).wrap());
            }
        });
    });
    clicked
}

/// Centered placeholder for empty or unavailable content.
pub fn empty_state(ui: &mut Ui, icon: &str, title: &str, detail: &str, actions: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    let avail = ui.available_height();
    ui.vertical_centered(|ui| {
        ui.add_space((avail * 0.5 - 130.0).clamp(16.0, 150.0));
        let (rect, _) = ui.allocate_exact_size(vec2(76.0, 76.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 38.0, p.accent_soft);
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::proportional(34.0), p.accent_text);
        ui.add_space(12.0);
        ui.label(semibold(title).size(17.0));
        ui.add_space(2.0);
        let w = ui.available_width().min(460.0);
        ui.allocate_ui_with_layout(vec2(w, 0.0), Layout::top_down(Align::Center), |ui| {
            ui.add(egui::Label::new(RichText::new(detail).color(p.text2)).wrap());
            ui.add_space(12.0);
            centered_row(ui, "empty-actions", actions);
        });
    });
}

/// A horizontal group centered in the available width (measured one frame late).
pub fn centered_row(ui: &mut Ui, id_src: &str, add: impl FnOnce(&mut Ui)) {
    let id = ui.id().with(id_src);
    let known: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    let pad = ((ui.available_width() - known) / 2.0).max(0.0);
    let r = ui.horizontal(|ui| {
        ui.add_space(pad);
        add(ui);
    });
    let used = (r.response.rect.width() - pad).max(0.0);
    ui.data_mut(|d| d.insert_temp(id, used));
    if (used - known).abs() > 0.5 {
        ui.ctx().request_repaint();
    }
}

/// Monospace snippet on a dark well (commands, logs).
pub fn code_block(ui: &mut Ui, text: &str) {
    let p = pal(ui);
    Frame::new()
        .fill(p.terminal_bg)
        .stroke(Stroke::new(1.0_f32, p.stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(RichText::new(text).monospace().size(12.5).color(Color32::from_rgb(0xD7, 0xDA, 0xE0))).selectable(true),
            );
        });
}

/// Empty state for a missing external program, with the install command.
pub fn tool_missing(ui: &mut Ui, icon: &str, title: &str, detail: &str, hint: &str) {
    empty_state(ui, icon, title, detail, |_| {});
    ui.add_space(14.0);
    ui.vertical_centered(|ui| code_block(ui, hint));
}

pub fn busy(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(16.0));
        ui.label(RichText::new(text).weak());
    });
}

// ---- inputs ------------------------------------------------------------------------

/// Standard single-line text field.
pub fn text_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(RichText::new(hint).color(pal(ui).text3))
            .desired_width(width)
            .margin(Margin::symmetric(8, 6))
            .vertical_align(Align::Center),
    )
}

pub fn text_field_mono(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    ui.add(
        egui::TextEdit::singleline(text)
            .font(egui::TextStyle::Monospace)
            .hint_text(RichText::new(hint).color(pal(ui).text3))
            .desired_width(width)
            .margin(Margin::symmetric(8, 6))
            .vertical_align(Align::Center),
    )
}

pub fn password_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    ui.add(
        egui::TextEdit::singleline(text)
            .password(true)
            .hint_text(RichText::new(hint).color(pal(ui).text3))
            .desired_width(width)
            .margin(Margin::symmetric(8, 6))
            .vertical_align(Align::Center),
    )
}

/// Search box with a magnifier and a clear button; returns the text edit response.
pub fn search(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let p = pal(ui);
    let mut edit: Option<Response> = None;
    let outer = Frame::new()
        .fill(p.input)
        .stroke(Stroke::new(1.0_f32, p.stroke_strong))
        .corner_radius(CornerRadius::same(RADIUS_CONTROL))
        .inner_margin(Margin::symmetric(8, 0))
        .show(ui, |ui| {
            ui.set_height(CONTROL_H - 2.0);
            ui.set_width(width - 16.0);
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(ph::MAGNIFYING_GLASS).size(15.0).color(p.text3));
                let clear_w = 22.0;
                let w = (ui.available_width() - clear_w).max(40.0);
                let r = ui.add(
                    egui::TextEdit::singleline(text)
                        .frame(false)
                        .hint_text(RichText::new(hint).color(p.text3))
                        .desired_width(w)
                        .vertical_align(Align::Center),
                );
                let mut r = r;
                if !text.is_empty()
                    && ui
                        .add(egui::Button::new(RichText::new(ph::X).size(12.0).color(p.text3)).frame(false))
                        .on_hover_text("Clear")
                        .clicked()
                {
                    text.clear();
                    r.mark_changed();
                }
                edit = Some(r);
            });
        });
    let r = edit.unwrap_or(outer.response.clone());
    if r.has_focus() {
        ui.painter().rect_stroke(
            outer.response.rect,
            CornerRadius::same(RADIUS_CONTROL),
            Stroke::new(1.5_f32, p.accent),
            egui::StrokeKind::Inside,
        );
    }
    r
}

/// Pill-shaped option switch. True when the value changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let p = pal(ui);
    let mut changed = false;
    Frame::new()
        .fill(p.input)
        .stroke(Stroke::new(1.0_f32, p.stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                // Inside a right-to-left row the first widget lands on the right; keep the written order.
                let mut ordered: Vec<&(T, &str)> = options.iter().collect();
                if ui.layout().prefer_right_to_left() {
                    ordered.reverse();
                }
                for (v, label) in ordered {
                    let selected = *value == *v;
                    let color = if selected { p.text } else { p.text2 };
                    let galley = ui.painter().layout_no_wrap((*label).to_string(), FontId::new(13.0, theme::family_medium()), color);
                    let size = vec2(galley.size().x + 22.0, 24.0);
                    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                    if selected {
                        let fill = if p.dark { p.control_hover } else { p.card };
                        ui.painter().rect(
                            rect,
                            CornerRadius::same(6),
                            fill,
                            Stroke::new(1.0_f32, p.stroke_strong),
                            egui::StrokeKind::Inside,
                        );
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(6), p.hover);
                    }
                    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
                    if resp.clicked() && !selected {
                        *value = *v;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// macOS-style switch with a trailing label. True in `changed()` when flipped.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let p = pal(ui);
    // The size of a macOS switch: 38 x 22 with a 2 px inset knob.
    let (w, h, gap) = (38.0_f32, 22.0_f32, 8.0_f32);
    let galley = (!label.is_empty()).then(|| ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(13.5), p.text));
    let total = w + galley.as_ref().map_or(0.0, |g| g.size().x + gap);
    let (rect, mut resp) = ui.allocate_exact_size(vec2(total + 4.0, CONTROL_H), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, label));
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let track = egui::Rect::from_min_size(egui::pos2(rect.left() + 2.0, rect.center().y - h / 2.0), vec2(w, h));
    let off = if resp.hovered() { p.text3 } else { p.stroke_strong };
    ui.painter().rect_filled(track, CornerRadius::same(255), off.lerp_to_gamma(p.accent, t));
    let x = track.left() + h / 2.0 + t * (w - h);
    ui.painter().circle_filled(egui::pos2(x, track.center().y), h / 2.0 - 2.0, Color32::WHITE);
    if let Some(g) = galley {
        ui.painter().galley(egui::pos2(track.right() + gap, rect.center().y - g.size().y / 2.0), g, p.text);
    }
    resp
}

/// Round avatar with the first letter of a name; the colour is derived from the name.
pub fn avatar(ui: &mut Ui, name: &str, size: f32) {
    let p = pal(ui);
    let seed = name.bytes().fold(7_u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    let palette = [p.accent_text, p.success, p.warning, p.danger, Color32::from_rgb(0xB0, 0x7C, 0xF0), Color32::from_rgb(0x2D, 0xB8, 0xB0)];
    let color = palette[(seed % palette.len() as u32) as usize];
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same((size / 4.0) as u8), color.gamma_multiply(0.2));
    let letter = name.rsplit('.').next().and_then(|s| s.chars().next()).unwrap_or('?').to_uppercase().to_string();
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, letter, FontId::new(size * 0.5, theme::family_semibold()), color);
}

/// Thin determinate progress bar.
pub fn progress(ui: &mut Ui, fraction: f32, width: f32) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(3), p.input);
    let mut fill = rect;
    fill.set_width((rect.width() * fraction.clamp(0.0, 1.0)).max(6.0));
    ui.painter().rect_filled(fill, CornerRadius::same(3), p.accent);
}

/// Meter coloured by how full it is.
pub fn meter(ui: &mut Ui, fraction: f32, width: f32) {
    let p = pal(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 6.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(3), p.hover);
    let mut fill = rect;
    fill.set_width((rect.width() * fraction.clamp(0.0, 1.0)).max(4.0));
    let c = if fraction > 0.9 {
        p.danger
    } else if fraction > 0.75 {
        p.warning
    } else {
        p.accent
    };
    ui.painter().rect_filled(fill, CornerRadius::same(3), c);
}

// ---- modal ------------------------------------------------------------------------

/// A centered dialog with dimmed backdrop. Close it when `should_close()` or
/// when the caller's own flag says so.
pub fn dialog<R>(
    ctx: &egui::Context,
    id: &str,
    icon: &str,
    title: &str,
    kind: Kind,
    width: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> egui::ModalResponse<R> {
    let p = theme::palette(ctx);
    egui::Modal::new(egui::Id::new(id)).show(ctx, |ui| {
        ui.set_width(width);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(8), kind.soft(p));
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::proportional(18.0), kind.fg(p));
            ui.label(semibold(title).size(16.0));
        });
        ui.add_space(10.0);
        add(ui)
    })
}

/// Right-aligned button row at the bottom of a dialog.
pub fn dialog_buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.add_space(14.0);
    ui.with_layout(Layout::right_to_left(Align::Center), add);
}

// ---- misc ----------------------------------------------------------------------------

/// Label/value row used in detail cards.
pub fn kv(ui: &mut Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).color(pal(ui).text2));
    ui.add(egui::Label::new(RichText::new(value)).truncate());
    ui.end_row();
}

pub fn file_icon(name: &str, is_dir: bool, p: &Palette) -> (&'static str, Color32) {
    if is_dir {
        return (ph::FOLDER, p.accent_text);
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" => (ph::FILE_IMAGE, p.success),
        "mp4" | "mkv" | "webm" | "mov" | "3gp" => (ph::FILE_VIDEO, p.warning),
        "mp3" | "wav" | "ogg" | "flac" | "m4a" | "aac" => (ph::FILE_AUDIO, p.warning),
        "zip" | "gz" | "tar" | "7z" | "rar" | "xz" => (ph::FILE_ZIP, p.text2),
        "txt" | "log" | "md" | "json" | "xml" | "csv" | "conf" | "ini" | "prop" => (ph::FILE_TEXT, p.text2),
        "apk" | "xapk" | "apks" => (ph::ANDROID_LOGO, p.success),
        "pdf" => (ph::FILE_PDF, p.danger),
        _ => (ph::FILE, p.text3),
    }
}

//! Design tokens: colours, type, spacing and the egui style built from them.
//!
//! The look follows macOS conventions: neutral grey window layers (no colour cast),
//! white (or elevated) grouped cards with hairline separators, the system blue as
//! the one accent, soft semantic tints, 6/10 px radii, Inter (the closest open
//! face to SF Pro) for text and Phosphor for icons. Text and accent colours are
//! chosen to reach 4.5:1 against the surfaces they sit on.

use egui::{
    epaint::Shadow, style::ScrollStyle, Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    Style, TextStyle, Theme, Visuals,
};

pub const RADIUS_CONTROL: u8 = 6;
pub const RADIUS_CARD: u8 = 10;
pub const RADIUS_WINDOW: u8 = 12;
/// Minimum height of buttons, inputs and list rows.
pub const CONTROL_H: f32 = 30.0;
pub const GAP: f32 = 8.0;

pub const FONT_MEDIUM: &str = "inter-medium";
pub const FONT_SEMIBOLD: &str = "inter-semibold";
pub const FONT_ICONS_FILL: &str = "phosphor-fill";

/// Every colour the UI uses. Panels never hard-code colours.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// Window / content background.
    pub bg: Color32,
    /// Navigation sidebar.
    pub sidebar: Color32,
    /// Cards and elevated surfaces.
    pub card: Color32,
    /// Text inputs and recessed wells.
    pub input: Color32,
    /// Hairline strokes.
    pub stroke: Color32,
    pub stroke_strong: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    /// Filled accent (primary buttons, selection bars).
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_press: Color32,
    /// Accent used for text and icons on neutral surfaces.
    pub accent_text: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub success_soft: Color32,
    pub warning: Color32,
    pub warning_soft: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
    /// Neutral button fill and its states.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_press: Color32,
    /// Overlay used for hover / press on transparent controls.
    pub hover: Color32,
    pub press: Color32,
    pub terminal_bg: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_premultiplied(
        (r as u16 * a as u16 / 255) as u8,
        (g as u16 * a as u16 / 255) as u8,
        (b as u16 * a as u16 / 255) as u8,
        a,
    )
}

pub const DARK: Palette = Palette {
    dark: true,
    bg: rgb(0x1E, 0x1E, 0x1E),
    sidebar: rgb(0x25, 0x25, 0x25),
    card: rgb(0x2C, 0x2C, 0x2E),
    input: rgb(0x1C, 0x1C, 0x1E),
    stroke: rgb(0x3A, 0x3A, 0x3C),
    stroke_strong: rgb(0x4A, 0x4A, 0x4D),
    text: rgb(0xF5, 0xF5, 0xF7),
    text2: rgb(0xA1, 0xA1, 0xA6),
    text3: rgb(0x98, 0x98, 0x9D),
    accent: rgb(0x0A, 0x72, 0xEE),
    accent_hover: rgb(0x2E, 0x86, 0xF6),
    accent_press: rgb(0x08, 0x5F, 0xCB),
    accent_text: rgb(0x64, 0xA8, 0xFF),
    accent_soft: rgba(0x0A, 0x84, 0xFF, 0x2E),
    on_accent: rgb(0xFF, 0xFF, 0xFF),
    success: rgb(0x30, 0xD1, 0x58),
    success_soft: rgba(0x30, 0xD1, 0x58, 0x26),
    warning: rgb(0xFF, 0xB3, 0x40),
    warning_soft: rgba(0xFF, 0xB3, 0x40, 0x26),
    danger: rgb(0xFF, 0x6A, 0x60),
    danger_soft: rgba(0xFF, 0x6A, 0x60, 0x26),
    control: rgb(0x3A, 0x3A, 0x3C),
    control_hover: rgb(0x44, 0x44, 0x46),
    control_press: rgb(0x2E, 0x2E, 0x30),
    hover: rgba(0xFF, 0xFF, 0xFF, 0x12),
    press: rgba(0xFF, 0xFF, 0xFF, 0x1C),
    terminal_bg: rgb(0x1B, 0x1B, 0x1D),
};

pub const LIGHT: Palette = Palette {
    dark: false,
    bg: rgb(0xF5, 0xF5, 0xF7),
    sidebar: rgb(0xEC, 0xEC, 0xEE),
    card: rgb(0xFF, 0xFF, 0xFF),
    input: rgb(0xFF, 0xFF, 0xFF),
    stroke: rgb(0xDD, 0xDD, 0xE1),
    stroke_strong: rgb(0xC6, 0xC6, 0xCB),
    text: rgb(0x1D, 0x1D, 0x1F),
    text2: rgb(0x55, 0x55, 0x5B),
    text3: rgb(0x68, 0x68, 0x6E),
    accent: rgb(0x0A, 0x6E, 0xF0),
    accent_hover: rgb(0x08, 0x5E, 0xD0),
    accent_press: rgb(0x06, 0x4E, 0xB0),
    accent_text: rgb(0x05, 0x5F, 0xD8),
    accent_soft: rgba(0x0A, 0x6E, 0xF0, 0x1A),
    on_accent: rgb(0xFF, 0xFF, 0xFF),
    success: rgb(0x17, 0x78, 0x31),
    success_soft: rgba(0x17, 0x78, 0x31, 0x1A),
    warning: rgb(0x9A, 0x5B, 0x00),
    warning_soft: rgba(0xD9, 0x8A, 0x00, 0x22),
    danger: rgb(0xC6, 0x28, 0x28),
    danger_soft: rgba(0xC6, 0x28, 0x28, 0x16),
    control: rgb(0xFF, 0xFF, 0xFF),
    control_hover: rgb(0xF2, 0xF2, 0xF4),
    control_press: rgb(0xE5, 0xE5, 0xE8),
    hover: rgba(0x00, 0x00, 0x00, 0x0C),
    press: rgba(0x00, 0x00, 0x00, 0x16),
    terminal_bg: rgb(0x1E, 0x1E, 0x20),
};

/// Palette for the theme the given context currently renders with.
pub fn palette(ctx: &Context) -> &'static Palette {
    if ctx.style().visuals.dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn pal(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn family_medium() -> FontFamily {
    FontFamily::Name(FONT_MEDIUM.into())
}

pub fn family_semibold() -> FontFamily {
    FontFamily::Name(FONT_SEMIBOLD.into())
}

pub fn family_icons_fill() -> FontFamily {
    FontFamily::Name(FONT_ICONS_FILL.into())
}

fn fonts() -> FontDefinitions {
    let mut f = FontDefinitions::default();
    let load = |f: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        f.font_data.insert(name.to_owned(), std::sync::Arc::new(FontData::from_static(bytes)));
    };
    load(&mut f, "inter", include_bytes!("../../assets/fonts/Inter-Regular.ttf"));
    load(&mut f, FONT_MEDIUM, include_bytes!("../../assets/fonts/Inter-Medium.ttf"));
    load(&mut f, FONT_SEMIBOLD, include_bytes!("../../assets/fonts/Inter-SemiBold.ttf"));
    load(&mut f, "jbmono", include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf"));
    f.font_data.insert("phosphor".into(), std::sync::Arc::new(egui_phosphor::Variant::Regular.font_data()));
    f.font_data.insert(FONT_ICONS_FILL.into(), std::sync::Arc::new(egui_phosphor::Variant::Fill.font_data()));

    // Fallbacks (CJK-less, emoji) stay behind the bundled faces.
    let defaults: Vec<String> = f.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let with = |first: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = first.iter().map(|s| (*s).to_owned()).collect();
        v.extend(defaults.iter().cloned());
        v
    };
    // The bundled Inter/JetBrains Mono are subset without private-use glyphs, so
    // Phosphor's icon code points fall through to the icon font. (Phosphor must
    // not come first: it carries empty glyphs for ASCII letters.)
    f.families.insert(FontFamily::Proportional, with(&["inter", "phosphor"]));
    f.families.insert(family_medium(), with(&[FONT_MEDIUM, "phosphor"]));
    f.families.insert(family_semibold(), with(&[FONT_SEMIBOLD, "phosphor"]));
    f.families.insert(family_icons_fill(), vec![FONT_ICONS_FILL.to_owned()]);
    let mono_defaults: Vec<String> = f.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let mut mono = vec!["jbmono".to_owned(), "phosphor".to_owned()];
    mono.extend(mono_defaults);
    f.families.insert(FontFamily::Monospace, mono);
    f
}

#[allow(clippy::field_reassign_with_default)]
fn style(p: &Palette) -> Style {
    let mut s = Style::default();

    s.text_styles = [
        (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(13.5, family_medium())),
        (TextStyle::Heading, FontId::new(20.0, family_semibold())),
        (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
    ]
    .into();

    let sp = &mut s.spacing;
    sp.item_spacing = egui::vec2(GAP, 6.0);
    sp.button_padding = egui::vec2(12.0, 5.0);
    sp.interact_size = egui::vec2(32.0, CONTROL_H);
    sp.window_margin = Margin::same(20);
    sp.menu_margin = Margin::same(6);
    sp.indent = 16.0;
    sp.icon_width = 18.0;
    sp.icon_width_inner = 10.0;
    sp.icon_spacing = 8.0;
    sp.combo_width = 140.0;
    sp.text_edit_width = 220.0;
    sp.scroll = ScrollStyle {
        bar_width: 9.0,
        floating_width: 3.0,
        floating_allocated_width: 0.0,
        handle_min_length: 32.0,
        foreground_color: false,
        dormant_background_opacity: 0.0,
        active_background_opacity: 0.4,
        interact_background_opacity: 0.6,
        dormant_handle_opacity: 0.0,
        active_handle_opacity: 0.55,
        interact_handle_opacity: 0.9,
        ..ScrollStyle::floating()
    };
    sp.tooltip_width = 360.0;
    s.animation_time = 0.08;
    s.interaction.tooltip_delay = 0.35;
    s.interaction.selectable_labels = false;

    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = None;
    v.hyperlink_color = p.accent_text;
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0_f32, p.stroke);
    v.window_corner_radius = CornerRadius::same(RADIUS_WINDOW);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow { offset: [0, 10], blur: 32, spread: 0, color: Color32::from_black_alpha(if p.dark { 150 } else { 52 }) };
    v.popup_shadow = Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(if p.dark { 130 } else { 44 }) };
    v.extreme_bg_color = p.input;
    v.faint_bg_color = if p.dark { rgba(0xFF, 0xFF, 0xFF, 0x08) } else { rgba(0x00, 0x00, 0x00, 0x06) };
    v.code_bg_color = p.input;
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = if p.dark { rgba(0x5B, 0x9D, 0xFF, 0x40) } else { rgba(0x21, 0x66, 0xD9, 0x2E) };
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    v.slider_trailing_fill = true;
    v.striped = false;
    v.indent_has_left_vline = false;
    v.resize_corner_size = 10.0;
    v.handle_shape = egui::style::HandleShape::Circle;

    let r = CornerRadius::same(RADIUS_CONTROL);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.stroke);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = Stroke::new(1.0_f32, p.stroke_strong);
    w.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.inactive.corner_radius = r;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.control_hover;
    w.hovered.weak_bg_fill = p.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0_f32, p.stroke_strong);
    w.hovered.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.control_press;
    w.active.weak_bg_fill = p.control_press;
    w.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    w.active.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.control_hover;
    w.open.weak_bg_fill = p.control_hover;
    w.open.bg_stroke = Stroke::new(1.0_f32, p.stroke_strong);
    w.open.fg_stroke = Stroke::new(1.0_f32, p.text);
    w.open.corner_radius = r;

    s.visuals = v;
    s
}

/// Installs fonts and both theme styles. Call once at start-up.
pub fn install(ctx: &Context) {
    ctx.set_fonts(fonts());
    ctx.set_style_of(Theme::Dark, style(&DARK));
    ctx.set_style_of(Theme::Light, style(&LIGHT));
}

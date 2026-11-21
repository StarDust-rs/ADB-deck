//! Interactive `adb shell` in a terminal emulator view.

use std::ffi::OsString;
use std::time::Duration;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Key, Modifiers, RichText, Sense, Ui, Vec2};

use adbdeck::pty::Pty;

use super::widgets;
use super::{kit, theme, DeviceCtx, Env};

const SCROLLBACK: usize = 2000;
/// Output chunks handled per frame.
const CHUNKS_PER_FRAME: usize = 64;

pub struct Shell {
    pty: Option<Pty>,
    parser: vt100::Parser,
    size: (u16, u16),
    exit: Option<u32>,
    error: Option<String>,
    scroll: usize,
    started: bool,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            pty: None,
            parser: vt100::Parser::new(30, 100, SCROLLBACK),
            size: (30, 100),
            exit: None,
            error: None,
            scroll: 0,
            started: false,
        }
    }
}

fn palette(idx: u8, dark: bool) -> Color32 {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    let _ = dark;
    match idx {
        0..=15 => {
            let (r, g, b) = BASE[idx as usize];
            Color32::from_rgb(r, g, b)
        }
        16..=231 => {
            let n = idx - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        _ => {
            let v = 8 + (idx - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

fn convert(c: vt100::Color, default: Color32, dark: bool) -> Color32 {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => palette(i, dark),
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

/// Bytes a key press sends to a terminal application.
pub fn key_bytes(key: Key, mods: Modifiers) -> Option<Vec<u8>> {
    let seq: &[u8] = match key {
        Key::Enter => b"\r",
        Key::Backspace => b"\x7f",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",
        Key::ArrowUp => b"\x1b[A",
        Key::ArrowDown => b"\x1b[B",
        Key::ArrowRight => b"\x1b[C",
        Key::ArrowLeft => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::Delete => b"\x1b[3~",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Insert => b"\x1b[2~",
        _ => {
            // Ctrl+letter becomes a control character (Ctrl+C is Event::Copy in egui).
            if mods.ctrl && !mods.alt {
                let name = key.name();
                let mut chars = name.chars();
                if let (Some(c), None) = (chars.next(), chars.next()) {
                    if c.is_ascii_alphabetic() {
                        return Some(vec![(c.to_ascii_uppercase() as u8) & 0x1f]);
                    }
                }
            }
            return None;
        }
    };
    Some(seq.to_vec())
}

impl Shell {
    fn spawn(&mut self, env: &Env, d: &DeviceCtx) {
        self.exit = None;
        self.error = None;
        self.scroll = 0;
        self.parser = vt100::Parser::new(self.size.0, self.size.1, SCROLLBACK);
        let _ = env;
        let args: Vec<OsString> = ["-s", d.dev.serial(), "shell"].iter().map(OsString::from).collect();
        match Pty::spawn(d.dev.adb().path(), &args, self.size.0, self.size.1) {
            Ok(p) => self.pty = Some(p),
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        if !self.started {
            self.started = true;
            self.spawn(env, d);
        }
        // Output.
        if let Some(pty) = self.pty.as_mut() {
            for _ in 0..CHUNKS_PER_FRAME {
                match pty.output.try_recv() {
                    Ok(chunk) => self.parser.process(&chunk),
                    Err(_) => break,
                }
            }
            if let Some(code) = pty.try_exit_code() {
                // Collect what the process printed right before exiting.
                while let Ok(chunk) = pty.output.try_recv() {
                    self.parser.process(&chunk);
                }
                self.exit = Some(code);
                self.pty = None;
            }
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }

        kit::card_margin(ui, 8, |ui| {
            ui.horizontal(|ui| {
                if kit::Btn::new("Restart session").icon(kit::ph::ARROWS_CLOCKWISE).show(ui).clicked() {
                    self.pty = None;
                    self.spawn(env, d);
                }
                if kit::Btn::new("Copy screen text").icon(kit::ph::COPY).show(ui).clicked() {
                    let text = self.parser.screen().contents();
                    ui.ctx().copy_text(text);
                    env.notes.info("Copied the terminal contents");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("Click the terminal to type · Ctrl+C and Ctrl+X go to the device")
                            .size(12.0)
                            .color(theme::pal(ui).text3),
                    );
                });
            });
        });
        ui.add_space(10.0);
        if let Some(e) = &self.error {
            widgets::error_banner(ui, e, false);
            ui.add_space(8.0);
        }
        if let Some(code) = self.exit {
            kit::banner(ui, kit::Kind::Warning, &format!("The shell exited (status {code}). Restart the session to continue."), None);
            ui.add_space(8.0);
        }

        let font = FontId::monospace(13.5);
        let (cell_w, cell_h) = ui.fonts(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        let avail = ui.available_size();
        let cols = ((avail.x - 24.0) / cell_w).floor().clamp(20.0, 400.0) as u16;
        let rows = ((avail.y - 20.0) / cell_h).floor().clamp(5.0, 200.0) as u16;
        if (rows, cols) != self.size {
            self.size = (rows, cols);
            self.parser.set_size(rows, cols);
            if let Some(p) = &self.pty {
                p.resize(rows, cols);
            }
        }

        let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail.x, avail.y.max(cell_h * 5.0)), Sense::click_and_drag());
        let id = resp.id;
        if resp.clicked() {
            ui.memory_mut(|m| m.request_focus(id));
        }
        let focused = ui.memory(|m| m.has_focus(id));
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true })
            });
        }
        // The terminal stays dark in both themes, like a native terminal window.
        let dark = true;
        let (bg, fg) = (theme::pal(ui).terminal_bg, Color32::from_rgb(222, 224, 230));

        if resp.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                let lines = (dy / cell_h).round() as i64;
                self.scroll = (self.scroll as i64 + lines).clamp(0, SCROLLBACK as i64) as usize;
            }
        }
        if focused {
            self.send_input(ui);
        }

        self.parser.set_scrollback(self.scroll);
        let screen = self.parser.screen();
        self.scroll = screen.scrollback();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, egui::CornerRadius::same(theme::RADIUS_CARD), bg);
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(theme::RADIUS_CARD),
            egui::Stroke::new(1.0_f32, theme::pal(ui).stroke),
            egui::StrokeKind::Inside,
        );
        let origin = rect.min + Vec2::new(12.0, 10.0);
        let (srows, scols) = screen.size();
        for r in 0..srows {
            let mut job = LayoutJob::default();
            let mut c = 0;
            while c < scols {
                let Some(cell) = screen.cell(r, c) else { break };
                if cell.is_wide_continuation() {
                    c += 1;
                    continue;
                }
                let (mut f, mut b) = (convert(cell.fgcolor(), fg, dark), convert(cell.bgcolor(), Color32::TRANSPARENT, dark));
                if cell.inverse() {
                    let nb = if b == Color32::TRANSPARENT { bg } else { b };
                    b = f;
                    f = nb;
                }
                let text = if cell.has_contents() { cell.contents() } else { " ".to_string() };
                job.append(
                    &text,
                    0.0,
                    TextFormat {
                        font_id: font.clone(),
                        color: f,
                        background: b,
                        underline: if cell.underline() { egui::Stroke::new(1.0_f32, f) } else { egui::Stroke::NONE },
                        ..Default::default()
                    },
                );
                c += if cell.is_wide() { 2 } else { 1 };
            }
            let galley = ui.fonts(|f| f.layout_job(job));
            painter.galley(origin + Vec2::new(0.0, f32::from(r) * cell_h), galley, fg);
        }
        if focused && self.scroll == 0 && !self.parser.screen().hide_cursor() {
            let (cr, cc) = self.parser.screen().cursor_position();
            let pos = origin + Vec2::new(f32::from(cc) * cell_w, f32::from(cr) * cell_h);
            painter.rect_stroke(
                egui::Rect::from_min_size(pos, Vec2::new(cell_w, cell_h)),
                0.0,
                egui::Stroke::new(1.5_f32, fg),
                egui::StrokeKind::Inside,
            );
        }
        if !focused && self.pty.is_some() {
            painter.text(
                rect.right_bottom() - Vec2::new(8.0, 8.0),
                egui::Align2::RIGHT_BOTTOM,
                "click to focus",
                FontId::proportional(12.0),
                fg.gamma_multiply(0.5),
            );
        }
    }

    fn send_input(&mut self, ui: &Ui) {
        let Some(pty) = self.pty.as_mut() else { return };
        let events = ui.input(|i| i.events.clone());
        let mut out: Vec<u8> = Vec::new();
        for ev in events {
            match ev {
                egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Paste(t) => out.extend_from_slice(t.as_bytes()),
                egui::Event::Copy => out.push(0x03),
                egui::Event::Cut => out.push(0x18),
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    if let Some(b) = key_bytes(key, modifiers) {
                        if modifiers.alt {
                            out.push(0x1b);
                        }
                        out.extend_from_slice(&b);
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty() {
            self.scroll = 0;
            if let Err(e) = pty.write(&out) {
                self.error = Some(e.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_keys_map_to_escape_sequences() {
        assert_eq!(key_bytes(Key::Enter, Modifiers::NONE), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(Key::ArrowUp, Modifiers::NONE), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(Key::D, Modifiers::CTRL), Some(vec![4]));
        assert_eq!(key_bytes(Key::D, Modifiers::NONE), None, "plain letters arrive as text events");
    }

    #[test]
    fn palette_covers_the_cube_and_greys() {
        assert_eq!(palette(16, true), Color32::from_rgb(0, 0, 0));
        assert_eq!(palette(231, true), Color32::from_rgb(255, 255, 255));
        assert_eq!(palette(232, true), Color32::from_rgb(8, 8, 8));
    }
}

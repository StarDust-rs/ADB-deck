//! Screenshots: capture, save, copy to the clipboard.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Mutex;

use egui::{RichText, TextureHandle, Ui};

use adbdeck::error::{Error, Result};
use adbdeck::jobs::Slot;

use super::widgets::{self, Dialog};
use super::{kit, theme, DeviceCtx, Env};

/// Kept alive for the whole run: on Linux the clipboard owner must keep
/// serving the data after `set_image` returns.
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

pub struct Shot {
    pub png: Vec<u8>,
    pub rgba: image::RgbaImage,
}

pub fn decode(png: Vec<u8>) -> Result<Shot> {
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| Error::parse("screenshot", e.to_string()))?;
    Ok(Shot { png, rgba: img.to_rgba8() })
}

pub fn texture(ctx: &egui::Context, name: &str, shot: &Shot) -> TextureHandle {
    let size = [shot.rgba.width() as usize, shot.rgba.height() as usize];
    ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied(size, shot.rgba.as_raw()), egui::TextureOptions::LINEAR)
}

fn copy_to_clipboard(shot: &Shot) -> Result<()> {
    let data = arboard::ImageData {
        width: shot.rgba.width() as usize,
        height: shot.rgba.height() as usize,
        bytes: Cow::Borrowed(shot.rgba.as_raw()),
    };
    let mut guard = CLIPBOARD.lock().map_err(|_| Error::Invalid("clipboard is unavailable".into()))?;
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new().map_err(|e| Error::Invalid(format!("Clipboard is not available: {e}")))?);
    }
    match guard.as_mut() {
        Some(c) => c.set_image(data).map_err(|e| Error::Invalid(format!("Could not copy the image: {e}"))),
        None => Err(Error::Invalid("clipboard is unavailable".into())),
    }
}

#[derive(Default)]
pub struct Screenshot {
    capture: Slot<Shot>,
    shot: Option<std::sync::Arc<Shot>>,
    tex: Option<TextureHandle>,
    error: Option<String>,
    save_dialog: Slot<Vec<PathBuf>>,
    write: Slot<String>,
    copy: Slot<String>,
    started: bool,
}

impl Screenshot {
    fn take(&mut self, env: &Env, d: &DeviceCtx) {
        let dev = d.dev.clone();
        self.error = None;
        self.capture.set(env.sp.spawn(async move { decode(dev.screenshot_png().await?) }));
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        if !self.started {
            self.started = true;
            self.take(env, d);
        }
        if let Some(r) = self.capture.poll() {
            match r {
                Ok(s) => {
                    self.tex = Some(texture(ui.ctx(), "screenshot", &s));
                    self.shot = Some(std::sync::Arc::new(s));
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        for slot in [&mut self.write, &mut self.copy] {
            if let Some(r) = slot.poll() {
                if let Some(m) = env.notes.report("Screenshot", r) {
                    env.notes.info(m);
                }
            }
        }
        if let Some(r) = self.save_dialog.poll() {
            if let (Some(path), Some(shot)) = (env.notes.report("File dialog", r).and_then(|p| p.into_iter().next()), self.shot.clone()) {
                self.write.set(env.sp.spawn(async move {
                    tokio::fs::write(&path, &shot.png).await.map_err(|e| Error::io(format!("writing {}", path.display()), e))?;
                    Ok(format!("Saved {}", path.display()))
                }));
            }
        }

        let p = theme::pal(ui);
        kit::card_margin(ui, 8, |ui| {
            ui.horizontal(|ui| {
                if kit::Btn::new("Take screenshot").icon(kit::ph::CAMERA).primary().enabled(!self.capture.busy()).show(ui).clicked() {
                    self.take(env, d);
                }
                let have = self.shot.is_some();
                if kit::Btn::new("Save as…").icon(kit::ph::FLOPPY_DISK).enabled(have && !self.save_dialog.busy()).show(ui).clicked() {
                    let name = format!("screenshot-{}.png", chrono_stamp());
                    self.save_dialog.set(widgets::dialog(env.sp, Dialog::Save { title: "Save screenshot".into(), name }));
                }
                if kit::Btn::new("Copy")
                    .icon(kit::ph::COPY)
                    .enabled(have && !self.copy.busy())
                    .tip("Copy to the clipboard")
                    .show(ui)
                    .clicked()
                {
                    if let Some(shot) = self.shot.clone() {
                        self.copy.set(env.sp.spawn(async move {
                            tokio::task::spawn_blocking(move || copy_to_clipboard(&shot)).await.map_err(|_| Error::Cancelled)??;
                            Ok("Screenshot copied to the clipboard".to_string())
                        }));
                    }
                }
                if self.capture.busy() {
                    ui.add(egui::Spinner::new().size(16.0));
                }
                if let Some(tex) = &self.tex {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let s = tex.size_vec2();
                        ui.label(RichText::new(format!("{} × {} px", s.x as u32, s.y as u32)).size(12.0).color(p.text3));
                    });
                }
            });
        });
        ui.add_space(10.0);
        if let Some(e) = self.error.clone() {
            if widgets::error_banner(ui, &e, true) {
                self.take(env, d);
            }
            ui.add_space(8.0);
        }
        match &self.tex {
            Some(tex) => {
                let avail = ui.available_size();
                egui::Frame::new()
                    .fill(p.terminal_bg)
                    .stroke(egui::Stroke::new(1.0_f32, p.stroke))
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS_CARD))
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_min_size(avail - egui::vec2(30.0, 30.0));
                        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                            let size = tex.size_vec2();
                            let scale = ((ui.available_width()) / size.x).min((ui.available_height().max(300.0)) / size.y).min(1.0);
                            ui.vertical_centered(|ui| {
                                ui.add(egui::Image::new((tex.id(), size * scale)).corner_radius(egui::CornerRadius::same(8)));
                            });
                        });
                    });
            }
            None if self.capture.busy() => kit::busy(ui, "Capturing…"),
            None => {}
        }
    }
}

/// `YYYYMMDD-HHMMSS` in UTC without a date library.
pub(super) fn chrono_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86400, secs % 86400);
    // Civil-from-days (Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_has_the_expected_shape() {
        let s = chrono_stamp();
        assert_eq!(s.len(), 15);
        assert_eq!(&s[8..9], "-");
    }

    #[test]
    fn non_png_data_is_rejected() {
        assert!(decode(b"garbage".to_vec()).is_err());
    }
}

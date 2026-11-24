//! Connect with a QR code: shows the code, follows the connection and, once the phone is
//! connected, lets the saved settings treat the phone as wanted again.

use std::time::Instant;

use egui::{vec2, Color32, Rect, RichText, Sense, Ui};
use qrcode::{Color, EcLevel, QrCode};
use tokio::sync::watch;

use adbdeck::adb::devices::unsuppress_host;
use adbdeck::adb::qr::{QrSession, QrStage, QrTiming};
use adbdeck::adb::Adb;
use adbdeck::error::{Error, Result};
use adbdeck::jobs::Slot;
use adbdeck::store::AppData;

use super::{kit, theme, Env};

/// Width of the white margin around the code, in modules (the QR specification asks for four).
const QUIET_ZONE: usize = 4;
/// The code is drawn about this many points wide, in whole-point modules so the edges stay sharp.
const CODE_POINTS: f32 = 240.0;

struct Shown {
    /// Dark modules, row by row.
    modules: Vec<bool>,
    width: usize,
    stage: watch::Receiver<QrStage>,
    started: Instant,
}

#[derive(Default)]
pub struct QrConnect {
    shown: Option<Shown>,
    op: Slot<String>,
    error: Option<String>,
}

/// The code's modules (true = dark) and its width. Level M leaves room to scan a code that is
/// partly glared over on a screen.
fn encode(payload: &str) -> Result<(Vec<bool>, usize)> {
    let code =
        QrCode::with_error_correction_level(payload, EcLevel::M).map_err(|e| Error::Invalid(format!("Cannot make the QR code: {e}")))?;
    Ok((code.to_colors().into_iter().map(|c| c == Color::Dark).collect(), code.width()))
}

fn host_of(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(host, _)| host)
}

/// Scanning a code is asking for the connection, so a phone the user disconnected earlier is
/// wanted again. True when the saved data changed.
fn want_phone_again(data: &mut AppData, host: &str) -> bool {
    let mut changed = false;
    for r in data.remote_devices.iter_mut().filter(|r| !r.auto_connect) {
        if host_of(&r.id) == host || r.endpoints.iter().any(|e| host_of(e) == host) {
            r.auto_connect = true;
            changed = true;
        }
    }
    changed
}

impl QrConnect {
    pub fn running(&self) -> bool {
        self.shown.is_some()
    }

    /// Shows a new code and starts waiting for a phone to scan it.
    pub fn start(&mut self, env: &Env, adb: &Adb) {
        self.cancel();
        let prepared = QrSession::new().and_then(|session| encode(&session.payload()).map(|code| (session, code)));
        let (session, (modules, width)) = match prepared {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        let (tx, stage) = watch::channel(QrStage::Waiting);
        let adb = adb.clone();
        self.op.set(env.sp.spawn(async move { adb.connect_with_qr(&session, QrTiming::default(), &tx).await }));
        self.shown = Some(Shown { modules, width, stage, started: Instant::now() });
    }

    /// Stops waiting; the code on screen is no longer accepted.
    pub fn cancel(&mut self) {
        self.op.cancel();
        self.shown = None;
        self.error = None;
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, adb: &Adb) {
        if let Some(result) = self.op.poll() {
            self.shown = None;
            match result {
                Ok(endpoint) => {
                    unsuppress_host(env.suppressed, host_of(&endpoint));
                    if want_phone_again(env.data, host_of(&endpoint)) {
                        *env.data_dirty = true;
                    }
                    env.notes.info(format!("Connected to {endpoint}"));
                }
                Err(Error::Cancelled) => {}
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        let p = theme::pal(ui);
        let (mut start, mut cancel) = (false, false);
        kit::card_titled(ui, "Connect with a QR code (Android 11+)", |ui| match &self.shown {
            None => {
                ui.label(
                    RichText::new("Scan a code with the phone and ADB Deck pairs and connects over Wi-Fi by itself. The phone and this computer must be on the same network.")
                        .size(12.5)
                        .color(p.text2),
                );
                if let Some(e) = &self.error {
                    ui.add_space(6.0);
                    kit::banner(ui, kit::Kind::Warning, e, None);
                }
                ui.add_space(8.0);
                let label = if self.error.is_some() { "Show a new code" } else { "Show QR code" };
                start = kit::Btn::new(label).icon(kit::ph::QR_CODE).primary().show(ui).clicked();
            }
            Some(shown) => cancel = show_code(ui, shown),
        });
        if start {
            self.start(env, adb);
        } else if cancel {
            self.cancel();
        }
    }
}

/// The code with the instructions beside it (below it in a narrow window); true when "Cancel" was clicked.
fn show_code(ui: &mut Ui, shown: &Shown) -> bool {
    let mut cancel = false;
    let wide = ui.available_width() >= 560.0;
    let content = |ui: &mut Ui| {
        draw_code(ui, shown);
        if !wide {
            ui.add_space(8.0);
        }
        ui.vertical(|ui| steps(ui, shown, &mut cancel));
    };
    if wide {
        ui.horizontal_top(content);
    } else {
        ui.vertical(content);
    }
    cancel
}

fn draw_code(ui: &mut Ui, shown: &Shown) {
    let cells = shown.width + 2 * QUIET_ZONE;
    let module = (CODE_POINTS / cells as f32).floor().max(1.0);
    let side = module * cells as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    // Dark on light whatever the theme: scanners expect it.
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::same(8), Color32::WHITE);
    for (i, _) in shown.modules.iter().enumerate().filter(|(_, dark)| **dark) {
        let (x, y) = (i % shown.width + QUIET_ZONE, i / shown.width + QUIET_ZONE);
        let min = rect.min + vec2(x as f32 * module, y as f32 * module);
        painter.rect_filled(Rect::from_min_size(min, vec2(module, module)), egui::CornerRadius::ZERO, Color32::BLACK);
    }
}

fn steps(ui: &mut Ui, shown: &Shown, cancel: &mut bool) {
    let p = theme::pal(ui);
    let stage = *shown.stage.borrow();
    for line in [
        "1. On the phone open Developer options › Wireless debugging and turn it on.",
        "2. Tap Pair device with QR code.",
        "3. Point the camera at this code. ADB Deck does the rest.",
    ] {
        ui.add(egui::Label::new(RichText::new(line).size(12.5).color(p.text2)).wrap());
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(14.0));
        ui.label(match stage {
            QrStage::Waiting => "Waiting for the phone to scan the code…",
            QrStage::Pairing => "Pairing…",
            QrStage::Connecting => "Connecting…",
        });
    });
    if stage == QrStage::Waiting {
        let left = QrTiming::default().lifetime.saturating_sub(shown.started.elapsed()).as_secs();
        ui.label(RichText::new(format!("Code valid for {}:{:02}", left / 60, left % 60)).size(12.0).color(p.text3));
    }
    ui.add_space(8.0);
    *cancel = kit::Btn::new("Cancel").show(ui).clicked();
}

#[cfg(test)]
mod tests {
    use super::*;
    use adbdeck::store::RemoteDevice;

    #[test]
    fn the_code_for_a_session_is_small_enough_to_scan_from_a_window() {
        let session = QrSession::new().unwrap();
        let (modules, width) = encode(&session.payload()).unwrap();
        assert_eq!(modules.len(), width * width);
        assert!(width <= 41, "version 6 or less, got {width} modules");
        // Finder patterns: dark corners at three of the four corners.
        assert!(modules[0] && modules[width - 1] && modules[width * (width - 1)]);
    }

    fn remembered(id: &str, endpoints: &[&str], auto_connect: bool) -> RemoteDevice {
        RemoteDevice {
            id: id.into(),
            endpoints: endpoints.iter().map(|e| e.to_string()).collect(),
            auto_connect,
            ..RemoteDevice::default()
        }
    }

    #[test]
    fn a_phone_disconnected_earlier_is_wanted_again_by_host() {
        let mut data = AppData {
            remote_devices: vec![
                remembered("192.168.1.20:40111", &["192.168.1.20:40111"], false),
                remembered("192.168.1.31:5555", &["10.0.0.7:5555", "192.168.1.31:5555"], false),
                remembered("192.168.1.99:5555", &[], false),
            ],
            ..AppData::default()
        };
        assert!(want_phone_again(&mut data, "192.168.1.20"));
        assert!(want_phone_again(&mut data, "10.0.0.7"), "any address the phone was reached at counts");
        let flags: Vec<_> = data.remote_devices.iter().map(|r| r.auto_connect).collect();
        assert_eq!(flags, [true, true, false], "another phone is not touched");
        assert!(!want_phone_again(&mut data, "192.168.1.20"), "nothing left to change");
    }
}

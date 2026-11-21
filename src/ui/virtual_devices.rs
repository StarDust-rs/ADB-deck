//! Android Virtual Devices.

use egui::{RichText, Ui};

use adbdeck::avd;
use adbdeck::jobs::Slot;
use adbdeck::tools::Tool;

use super::widgets;
use super::{kit, theme, Env};

#[derive(Default)]
pub struct VirtualDevices {
    started: bool,
    load: Slot<Vec<String>>,
    list: Vec<String>,
    error: Option<String>,
    cold_boot: bool,
    stop: Slot<String>,
}

impl VirtualDevices {
    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env) {
        let Some(emulator) = env.tools.emulator.clone() else {
            kit::tool_missing(
                ui,
                kit::ph::DESKTOP,
                "The Android emulator was not found",
                "Virtual devices are created in Android Studio's Device Manager (or with avdmanager) and started here.",
                Tool::Emulator.install_hint(),
            );
            return;
        };
        if !self.started {
            self.started = true;
            self.reload(env, &emulator);
        }
        if let Some(r) = self.load.poll() {
            match r {
                Ok(l) => self.list = l,
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if let Some(r) = self.stop.poll() {
            if let Some(m) = env.notes.report("Stop emulator", r) {
                env.notes.info(m);
            }
        }
        let p = theme::pal(ui);
        kit::card_margin(ui, 8, |ui| {
            ui.horizontal(|ui| {
                kit::toggle(ui, &mut self.cold_boot, "Cold boot (ignore the saved snapshot)");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::Btn::new("Reload").icon(kit::ph::ARROWS_CLOCKWISE).enabled(!self.load.busy()).show(ui).clicked() {
                        self.reload(env, &emulator);
                    }
                    if self.load.busy() {
                        ui.add(egui::Spinner::new().size(16.0));
                    }
                });
            });
        });
        ui.add_space(10.0);
        if let Some(e) = self.error.clone() {
            if widgets::error_banner(ui, &e, true) {
                self.reload(env, &emulator);
            }
            ui.add_space(8.0);
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(kit::semibold("Available").size(13.5));
            ui.add_space(4.0);
            if self.list.is_empty() && !self.load.busy() && self.error.is_none() {
                kit::card(ui, |ui| {
                    ui.label(
                        RichText::new("No virtual devices. Create one in Android Studio's Device Manager, then reload.").color(p.text2),
                    );
                });
            }
            for name in self.list.clone() {
                kit::card_margin(ui, 12, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(kit::ph::DESKTOP).size(22.0).color(p.accent_text));
                        ui.label(kit::medium(&name).size(14.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::Btn::new("Start").icon(kit::ph::PLAY).primary().show(ui).clicked() {
                                match avd::start_avd(&emulator, &name, self.cold_boot) {
                                    Ok(()) => env.notes.info(format!("Starting {name}; it appears in the device list when booted.")),
                                    Err(e) => env.notes.error("Start emulator", &e),
                                }
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
            ui.add_space(10.0);
            ui.label(kit::semibold("Running").size(13.5));
            ui.add_space(4.0);
            let running: Vec<String> = env.devices.iter().filter(|d| d.is_emulator()).map(|d| d.id.clone()).collect();
            if running.is_empty() {
                kit::card(ui, |ui| {
                    ui.label(RichText::new("No emulator is running.").color(p.text2));
                });
            }
            for serial in running {
                kit::card_margin(ui, 12, |ui| {
                    ui.horizontal(|ui| {
                        kit::dot(ui, kit::Kind::Success);
                        ui.label(kit::medium(&serial).size(14.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::Btn::new("Shut down").icon(kit::ph::POWER).danger().enabled(!self.stop.busy()).show(ui).clicked() {
                                if let Some(adb) = env.adb.cloned() {
                                    let s = serial.clone();
                                    self.stop.set(env.sp.spawn(async move {
                                        avd::stop_emulator(&adb, &s).await?;
                                        Ok(format!("Shutting down {s}"))
                                    }));
                                }
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        });
    }

    fn reload(&mut self, env: &Env, emulator: &std::path::Path) {
        let path = emulator.to_path_buf();
        self.error = None;
        self.load.set(env.sp.spawn(async move { avd::list_avds(&path).await }));
    }
}

//! Device controls: radios, display, developer toggles, battery and power.

use std::cell::RefCell;

use egui::{RichText, Ui};

use adbdeck::adb::control::{Command, Controls, DarkMode, RebootTarget, Rotation, StatusBar};
use adbdeck::jobs::Slot;

use super::{kit, theme, widgets, DeviceCtx, Env};

const TIMEOUTS: [(u32, &str); 7] = [
    (15, "15 seconds"),
    (30, "30 seconds"),
    (60, "1 minute"),
    (120, "2 minutes"),
    (300, "5 minutes"),
    (600, "10 minutes"),
    (1800, "30 minutes"),
];
const ANIMATIONS: [(f32, &str); 7] = [(0.0, "Off"), (0.5, "0.5×"), (1.0, "1×"), (1.5, "1.5×"), (2.0, "2×"), (5.0, "5×"), (10.0, "10×")];

#[derive(Default)]
pub struct DeviceControls {
    started: bool,
    load: Slot<Controls>,
    data: Option<Controls>,
    error: Option<String>,
    act: Slot<String>,
    confirm: Option<RebootTarget>,
    /// Edit buffers, filled from the phone when it is read.
    brightness: f32,
    battery: f32,
    width: String,
    height: String,
    density: String,
}

type Request<'a> = &'a RefCell<Option<Command>>;

/// A switch row. Unknown state shows as off with a note in the tooltip.
fn flag(ui: &mut Ui, busy: bool, request: Request, label: &str, hint: &str, state: Option<bool>, make: fn(bool) -> Command) {
    let mut on = state.unwrap_or(false);
    let hint = if state.is_none() { format!("{hint}\nThe current state could not be read from the phone.") } else { hint.to_string() };
    kit::setting_row(ui, label, &hint, |ui| {
        ui.add_enabled_ui(!busy, |ui| {
            if kit::toggle(ui, &mut on, "").changed() {
                request.replace(Some(make(on)));
            }
        });
    });
}

fn parse_size(text: &str) -> Option<(String, String)> {
    let (w, h) = text.split_once('x')?;
    Some((w.trim().to_string(), h.trim().to_string()))
}

impl DeviceControls {
    fn reload(&mut self, env: &Env, d: &DeviceCtx) {
        let dev = d.dev.clone();
        self.error = None;
        self.load.set(env.sp.spawn(async move { dev.controls().await }));
    }

    fn adopt(&mut self, c: &Controls) {
        self.brightness = c.brightness.map_or(50.0, |b| f32::from(b) * 100.0 / 255.0);
        self.battery = c.battery.level.map_or(50.0, f32::from);
        let (w, h) = parse_size(&c.size).unwrap_or_default();
        self.width = w;
        self.height = h;
        self.density = c.density.clone();
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        if !self.started {
            self.started = true;
            self.reload(env, d);
        }
        if let Some(r) = self.load.poll() {
            match r {
                Ok(c) => {
                    self.adopt(&c);
                    self.data = Some(c);
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if let Some(r) = self.act.poll() {
            if let Some(msg) = env.notes.report("Device control", r) {
                env.notes.info(msg);
            }
            // After a reboot the phone is gone; the device list notices that by itself.
            self.reload(env, d);
        }
        self.confirm_dialog(ui.ctx(), env, d);

        if let Some(e) = self.error.clone() {
            if widgets::error_banner(ui, &e, true) {
                self.reload(env, d);
            }
            ui.add_space(8.0);
        }
        let Some(c) = self.data.clone() else {
            if self.error.is_none() {
                kit::busy(ui, "Reading the phone's settings…");
            }
            return;
        };
        let busy = self.act.busy();
        let request: RefCell<Option<Command>> = RefCell::new(None);
        let req: Request = &request;
        let p = theme::pal(ui);
        let mut reload = false;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let Self { brightness, battery, width, height, density, .. } = self;
            kit::card_pair(
                ui,
                "ctl-row1",
                ("Connectivity", &mut |ui: &mut Ui| {
                    flag(ui, busy, req, "Wi-Fi", "Turn the Wi-Fi radio on or off", c.wifi, Command::Wifi);
                    flag(ui, busy, req, "Mobile data", "Cellular data connection", c.mobile_data, Command::MobileData);
                    flag(ui, busy, req, "Bluetooth", "Turn Bluetooth on or off", c.bluetooth, Command::Bluetooth);
                    flag(ui, busy, req, "Airplane mode", "Switches every radio off", c.airplane, Command::Airplane);
                    kit::setting_row(ui, "NFC", "Near-field communication", |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            // Right to left: "On" is added first and ends up on the right.
                            if kit::btn(ui, "On").clicked() {
                                req.replace(Some(Command::Nfc(true)));
                            }
                            if kit::btn(ui, "Off").clicked() {
                                req.replace(Some(Command::Nfc(false)));
                            }
                        });
                    });
                }),
                ("Display", &mut |ui: &mut Ui| {
                    kit::setting_row(ui, "Dark mode", "", |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            let mut mode = c.dark_mode.unwrap_or(DarkMode::Auto);
                            let options = [(DarkMode::Auto, "Auto"), (DarkMode::Light, "Light"), (DarkMode::Dark, "Dark")];
                            if kit::segmented(ui, &mut mode, &options) {
                                req.replace(Some(Command::DarkMode(mode)));
                            }
                        });
                    });
                    kit::setting_row(ui, "Rotation", "Auto follows the sensor; the others lock the screen", |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            let mut rot = c.rotation.unwrap_or(Rotation::Auto);
                            let options = Rotation::ALL.map(|r| (r, r.label()));
                            if kit::segmented(ui, &mut rot, &options) {
                                req.replace(Some(Command::Rotation(rot)));
                            }
                        });
                    });
                    kit::setting_row(ui, "Brightness", "Moving the slider turns adaptive brightness off", |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            let r = kit::slider(ui, brightness, 0.0..=100.0, 1.0, 0, 120.0);
                            if r.drag_stopped() {
                                req.replace(Some(Command::Brightness((*brightness * 255.0 / 100.0).round() as u8)));
                            }
                        });
                    });
                    flag(
                        ui,
                        busy,
                        req,
                        "Adaptive brightness",
                        "Let the phone choose the brightness",
                        c.auto_brightness,
                        Command::AutoBrightness,
                    );
                    kit::setting_row(ui, "Screen timeout", "", |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            let now = c.screen_timeout_secs;
                            let label = TIMEOUTS
                                .iter()
                                .find(|(s, _)| Some(*s) == now)
                                .map_or_else(|| now.map_or("Unknown".to_string(), |s| format!("{s} s")), |(_, l)| (*l).to_string());
                            egui::ComboBox::from_id_salt("ctl-timeout").width(112.0).selected_text(label).show_ui(ui, |ui| {
                                for (secs, text) in TIMEOUTS {
                                    if ui.selectable_label(now == Some(secs), text).clicked() {
                                        req.replace(Some(Command::ScreenTimeoutSecs(secs)));
                                    }
                                }
                            });
                        });
                    });
                    kit::setting_row(ui, "Animation speed", "Turn animations off for faster testing and screenshots", |ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            let now = c.animation_scale;
                            let label = ANIMATIONS
                                .iter()
                                .find(|(s, _)| now.is_some_and(|n| (n - s).abs() < 0.01))
                                .map_or_else(|| now.map_or("Unknown".to_string(), |n| format!("{n}×")), |(_, l)| (*l).to_string());
                            egui::ComboBox::from_id_salt("ctl-anim").width(112.0).selected_text(label).show_ui(ui, |ui| {
                                for (scale, text) in ANIMATIONS {
                                    if ui.selectable_label(now.is_some_and(|n| (n - scale).abs() < 0.01), text).clicked() {
                                        req.replace(Some(Command::AnimationScale(scale)));
                                    }
                                }
                            });
                        });
                    });
                }),
            );
            ui.add_space(12.0);
            kit::card_pair(
                ui,
                "ctl-row2",
                ("Developer", &mut |ui: &mut Ui| {
                    flag(
                        ui,
                        busy,
                        req,
                        "Stay awake while charging",
                        "Keeps the screen on when plugged in",
                        c.stay_awake,
                        Command::StayAwake,
                    );
                    flag(ui, busy, req, "Show touches", "Draws a dot where the screen is touched", c.show_touches, Command::ShowTouches);
                    flag(ui, busy, req, "Pointer location", "Overlay with touch coordinates", c.pointer_location, Command::PointerLocation);
                    ui.add_space(4.0);
                    ui.label(RichText::new("Screen size and density").size(12.5).color(p.text2));
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        kit::text_field(ui, width, "width", 56.0);
                        ui.label("×");
                        kit::text_field(ui, height, "height", 56.0);
                        let size = width.trim().parse::<u32>().ok().zip(height.trim().parse::<u32>().ok());
                        if kit::Btn::new("Set").enabled(!busy && size.is_some()).show(ui).clicked() {
                            req.replace(Some(Command::Size(size)));
                        }
                        if kit::Btn::new("Reset").enabled(!busy).tip("Back to the panel's own size").show(ui).clicked() {
                            req.replace(Some(Command::Size(None)));
                        }
                    });
                    ui.horizontal(|ui| {
                        kit::text_field(ui, density, "dpi", 56.0);
                        ui.label("dpi");
                        let value = density.trim().parse::<u32>().ok();
                        if kit::Btn::new("Set").enabled(!busy && value.is_some()).show(ui).clicked() {
                            req.replace(Some(Command::Density(value)));
                        }
                        if kit::Btn::new("Reset").enabled(!busy).tip("Back to the panel's own density").show(ui).clicked() {
                            req.replace(Some(Command::Density(None)));
                        }
                    });
                    if c.size != c.physical_size || c.density != c.physical_density {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!("Panel: {} @ {} dpi", c.physical_size, c.physical_density)).size(12.0).color(p.text3),
                        );
                    }
                }),
                ("Battery and doze", &mut |ui: &mut Ui| {
                    ui.horizontal(|ui| {
                        let level = c.battery.level.map_or("Unknown".to_string(), |l| format!("{l}%"));
                        ui.label(kit::semibold(level).size(20.0));
                        if c.battery.charging {
                            kit::pill(ui, "Charging", kit::Kind::Success);
                        }
                        if let Some(t) = c.battery.temperature_tenths {
                            ui.label(RichText::new(format!("{:.1} °C", f64::from(t) / 10.0)).color(p.text2));
                        }
                    });
                    if let Some(l) = c.battery.level {
                        ui.add_space(6.0);
                        let w = ui.available_width();
                        kit::progress(ui, f32::from(l) / 100.0, w);
                    }
                    ui.add_space(10.0);
                    ui.label(RichText::new("Pretend the battery is at a level, to test low-battery behaviour").size(12.0).color(p.text3));
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.add_enabled_ui(!busy, |ui| {
                            kit::slider(ui, battery, 0.0..=100.0, 1.0, 0, 120.0);
                        });
                        if kit::Btn::new("Simulate").enabled(!busy).show(ui).clicked() {
                            req.replace(Some(Command::BatteryLevel(battery.round() as u8)));
                        }
                        if kit::Btn::new("Reset").enabled(!busy).tip("Report the real battery again").show(ui).clicked() {
                            req.replace(Some(Command::BatteryReset));
                        }
                    });
                    ui.add_space(10.0);
                    ui.label(RichText::new("Doze").size(12.5).color(p.text2));
                    ui.horizontal(|ui| {
                        if kit::Btn::new("Force doze").enabled(!busy).tip("Puts the device into deep idle at once").show(ui).clicked() {
                            req.replace(Some(Command::Doze(true)));
                        }
                        if kit::Btn::new("Leave doze").enabled(!busy).show(ui).clicked() {
                            req.replace(Some(Command::Doze(false)));
                        }
                    });
                }),
            );
            ui.add_space(12.0);
            kit::card_titled(ui, "Screen and power", |ui| {
                ui.horizontal_wrapped(|ui| {
                    let b = |label: &'static str, icon: &'static str| kit::Btn::new(label).icon(icon).enabled(!busy);
                    for (label, icon, cmd) in [
                        ("Wake", kit::ph::SUN, Command::Wake),
                        ("Sleep", kit::ph::MOON, Command::Sleep),
                        ("Notifications", kit::ph::BELL, Command::StatusBar(StatusBar::Notifications)),
                        ("Quick settings", kit::ph::SLIDERS_HORIZONTAL, Command::StatusBar(StatusBar::QuickSettings)),
                        ("Collapse", kit::ph::CARET_UP, Command::StatusBar(StatusBar::Collapse)),
                    ] {
                        if b(label, icon).show(ui).clicked() {
                            req.replace(Some(cmd));
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for target in [RebootTarget::System, RebootTarget::Recovery, RebootTarget::Bootloader, RebootTarget::PowerOff] {
                        let icon = if target == RebootTarget::PowerOff { kit::ph::POWER } else { kit::ph::ARROWS_CLOCKWISE };
                        if kit::Btn::new(target.label()).icon(icon).danger().enabled(!busy).show(ui).clicked() {
                            req.replace(Some(Command::Reboot(target)));
                        }
                    }
                    if kit::Btn::new("Reload").icon(kit::ph::ARROWS_CLOCKWISE).subtle().enabled(!self.load.busy()).show(ui).clicked() {
                        reload = true;
                    }
                });
            });
        });
        if reload {
            self.reload(env, d);
        }
        if let Some(cmd) = request.take() {
            match cmd {
                Command::Reboot(target) => self.confirm = Some(target),
                cmd => {
                    let dev = d.dev.clone();
                    self.act.set(env.sp.spawn(async move { dev.apply(&cmd).await }));
                }
            }
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, env: &Env, d: &DeviceCtx) {
        let Some(target) = self.confirm else { return };
        let mut answer = None;
        let text = match target {
            RebootTarget::System => "The phone restarts now. Unsaved work on it is lost.",
            RebootTarget::Recovery => "The phone restarts into recovery mode.",
            RebootTarget::Bootloader => "The phone restarts into the bootloader (fastboot).",
            RebootTarget::PowerOff => "The phone shuts down. It has to be switched on by hand.",
        };
        let resp = kit::dialog(ctx, "ctl-confirm", kit::ph::POWER, target.label(), kit::Kind::Danger, 360.0, |ui| {
            ui.add(egui::Label::new(RichText::new(text).color(theme::pal(ui).text2)).wrap());
            kit::dialog_buttons(ui, |ui| {
                if kit::Btn::new(target.label()).danger().show(ui).clicked() {
                    answer = Some(true);
                }
                if kit::btn(ui, "Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
        if resp.should_close() && answer.is_none() {
            answer = Some(false);
        }
        match answer {
            Some(true) => {
                self.confirm = None;
                let dev = d.dev.clone();
                self.act.set(env.sp.spawn(async move { dev.apply(&Command::Reboot(target)).await }));
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

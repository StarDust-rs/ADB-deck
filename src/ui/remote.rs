//! On-screen remote control: sends Android key events.

use egui::Ui;

use adbdeck::adb::remote_keys::Key;
use adbdeck::jobs::Job;

use super::{kit, DeviceCtx, Env};

const MAX_IN_FLIGHT: usize = 8;

#[derive(Default)]
pub struct Remote {
    jobs: Vec<Job<()>>,
}

impl Remote {
    fn press(&mut self, env: &Env, d: &DeviceCtx, key: Key) {
        if self.jobs.len() >= MAX_IN_FLIGHT {
            return;
        }
        let dev = d.dev.clone();
        self.jobs.push(env.sp.spawn(async move { dev.press(key).await }));
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        let mut finished = Vec::new();
        for (i, job) in self.jobs.iter_mut().enumerate() {
            if let Some(r) = job.poll() {
                finished.push((i, r));
            }
        }
        for (i, r) in finished.into_iter().rev() {
            self.jobs.remove(i);
            env.notes.report("Key press", r);
        }

        let mut pressed: Option<Key> = None;
        // Each key: label, optional icon, key code.
        let mut key = |ui: &mut Ui, icon: Option<&str>, label: &str, k: Key, w: f32, primary: bool| {
            let mut b = kit::Btn::new(label).min_width(w);
            if let Some(i) = icon {
                b = b.icon(i);
            }
            if primary {
                b = b.primary();
            }
            if b.show(ui).clicked() {
                pressed = Some(k);
            }
        };
        let heading = |ui: &mut Ui, text: &str| {
            ui.label(kit::semibold(text).size(14.5));
            ui.add_space(10.0);
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut draw = |ui: &mut Ui, which: usize| match which {
                0 => {
                    kit::card_sized(ui, 312.0, |ui| {
                        heading(ui, "Navigation");
                        egui::Grid::new("dpad").spacing([6.0, 6.0]).show(ui, |ui| {
                            ui.label("");
                            key(ui, Some(kit::ph::ARROW_UP), "", Key::DPAD_UP, 100.0, false);
                            ui.end_row();
                            key(ui, Some(kit::ph::ARROW_LEFT), "", Key::DPAD_LEFT, 100.0, false);
                            key(ui, None, "OK", Key::DPAD_CENTER, 100.0, true);
                            key(ui, Some(kit::ph::ARROW_RIGHT), "", Key::DPAD_RIGHT, 100.0, false);
                            ui.end_row();
                            ui.label("");
                            key(ui, Some(kit::ph::ARROW_DOWN), "", Key::DPAD_DOWN, 100.0, false);
                            ui.end_row();
                        });
                        ui.add_space(10.0);
                        kit::hsep(ui);
                        ui.add_space(10.0);
                        egui::Grid::new("sys").spacing([6.0, 6.0]).show(ui, |ui| {
                            key(ui, Some(kit::ph::ARROW_BEND_UP_LEFT), "Back", Key::BACK, 100.0, false);
                            key(ui, Some(kit::ph::HOUSE), "Home", Key::HOME, 100.0, false);
                            key(ui, Some(kit::ph::SQUARES_FOUR), "Recents", Key::APP_SWITCH, 100.0, false);
                            ui.end_row();
                            key(ui, Some(kit::ph::LIST), "Menu", Key::MENU, 100.0, false);
                            key(ui, Some(kit::ph::GEAR), "Settings", Key::SETTINGS, 100.0, false);
                            key(ui, Some(kit::ph::POWER), "Power", Key::POWER, 100.0, false);
                            ui.end_row();
                        });
                    });
                }
                1 => {
                    kit::card_sized(ui, 250.0, |ui| {
                        heading(ui, "Media and TV");
                        egui::Grid::new("media").spacing([6.0, 6.0]).show(ui, |ui| {
                            key(ui, Some(kit::ph::SPEAKER_HIGH), "Vol +", Key::VOLUME_UP, 110.0, false);
                            key(ui, Some(kit::ph::CARET_UP), "Ch +", Key::CHANNEL_UP, 110.0, false);
                            ui.end_row();
                            key(ui, Some(kit::ph::SPEAKER_LOW), "Vol −", Key::VOLUME_DOWN, 110.0, false);
                            key(ui, Some(kit::ph::CARET_DOWN), "Ch −", Key::CHANNEL_DOWN, 110.0, false);
                            ui.end_row();
                            key(ui, Some(kit::ph::SPEAKER_X), "Mute", Key::MUTE, 110.0, false);
                            key(ui, Some(kit::ph::LIST_BULLETS), "Guide", Key::GUIDE, 110.0, false);
                            ui.end_row();
                            key(ui, Some(kit::ph::ARROW_SQUARE_IN), "Input", Key::TV_INPUT, 110.0, false);
                            ui.end_row();
                        });
                    });
                }
                _ => {
                    kit::card_sized(ui, 172.0, |ui| {
                        heading(ui, "Digits");
                        egui::Grid::new("digits").spacing([6.0, 6.0]).show(ui, |ui| {
                            for n in [1, 2, 3, 4, 5, 6, 7, 8, 9] {
                                key(ui, None, &n.to_string(), Key::digit(n), 52.0, false);
                                if n % 3 == 0 {
                                    ui.end_row();
                                }
                            }
                            ui.label("");
                            key(ui, None, "0", Key::digit(0), 52.0, false);
                            ui.end_row();
                        });
                    });
                }
            };
            if ui.available_width() >= 856.0 {
                ui.horizontal_top(|ui| {
                    for i in 0..3 {
                        if i > 0 {
                            ui.add_space(8.0);
                        }
                        draw(ui, i);
                    }
                });
            } else {
                for i in 0..3 {
                    if i > 0 {
                        ui.add_space(8.0);
                    }
                    draw(ui, i);
                }
            }
        });
        if let Some(k) = pressed {
            self.press(env, d, k);
        }
    }
}

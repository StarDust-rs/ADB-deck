//! Settings, external tool status and the About page.

use std::path::PathBuf;

use egui::{RichText, Ui};

use adbdeck::jobs::Slot;
use adbdeck::paths;
use adbdeck::store::{Settings, ThemeChoice};
use adbdeck::tools::{Tool, ToolStatus};

use super::widgets::{self, Dialog};
use super::{kit, theme, Env};

#[derive(Default)]
pub struct SettingsPanel {
    draft: Option<Settings>,
    pick: Slot<Vec<PathBuf>>,
    pick_for: Option<Tool>,
    open: Slot<()>,
}

fn path_field(s: &mut Settings, tool: Tool) -> Option<&mut String> {
    match tool {
        Tool::Adb => Some(&mut s.adb_path),
        Tool::Apksigner => Some(&mut s.apksigner_path),
        Tool::Emulator => None,
    }
}

impl SettingsPanel {
    /// Returns the settings to adopt when the user applied a change.
    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env) -> Option<Settings> {
        let draft = self.draft.get_or_insert_with(|| env.settings.clone());

        if let Some(r) = self.pick.poll() {
            if let (Some(paths), Some(tool)) = (env.notes.report("File dialog", r), self.pick_for.take()) {
                if let (Some(p), Some(field)) = (paths.into_iter().next(), path_field(draft, tool)) {
                    *field = p.to_string_lossy().into_owned();
                }
            }
        }
        if let Some(r) = self.open.poll() {
            env.notes.report("Open folder", r);
        }

        let mut applied = None;
        let p = theme::pal(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            kit::card_titled(ui, "Appearance", |ui| {
                ui.horizontal(|ui| {
                    for (choice, icon, label) in [
                        (ThemeChoice::System, kit::ph::DESKTOP, "Follow system"),
                        (ThemeChoice::Light, kit::ph::SUN, "Light"),
                        (ThemeChoice::Dark, kit::ph::MOON, "Dark"),
                    ] {
                        let selected = draft.theme == choice;
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(130.0, 76.0), egui::Sense::click());
                        let stroke =
                            if selected { egui::Stroke::new(2.0_f32, p.accent) } else { egui::Stroke::new(1.0_f32, p.stroke_strong) };
                        let fill = if selected {
                            p.accent_soft
                        } else if resp.hovered() {
                            p.hover
                        } else {
                            p.input
                        };
                        ui.painter().rect(rect, egui::CornerRadius::same(10), fill, stroke, egui::StrokeKind::Inside);
                        ui.painter().text(
                            rect.center() - egui::vec2(0.0, 12.0),
                            egui::Align2::CENTER_CENTER,
                            icon,
                            egui::FontId::proportional(24.0),
                            if selected { p.accent_text } else { p.text2 },
                        );
                        ui.painter().text(
                            rect.center() + egui::vec2(0.0, 18.0),
                            egui::Align2::CENTER_CENTER,
                            label,
                            egui::FontId::new(13.0, theme::family_medium()),
                            p.text,
                        );
                        if resp.clicked() {
                            draft.theme = choice;
                        }
                    }
                });
            });
            ui.add_space(12.0);

            kit::card_titled(ui, "External tools", |ui| {
                ui.label(RichText::new("Leave a path empty to look the tool up in PATH and the Android SDK.").size(12.5).color(p.text2));
                ui.add_space(8.0);
                for tool in [Tool::Adb, Tool::Apksigner] {
                    ui.horizontal(|ui| {
                        kit::fixed_cell(ui, 84.0, 30.0, |ui| ui.label(kit::medium(tool.name())));
                        if let Some(field) = path_field(draft, tool) {
                            let w = (ui.available_width() - 64.0).clamp(160.0, 520.0);
                            kit::text_field_mono(ui, field, "automatic", w);
                        }
                        if kit::Btn::new("").icon(kit::ph::FOLDER_OPEN).tip("Browse…").show(ui).clicked() && !self.pick.busy() {
                            self.pick_for = Some(tool);
                            self.pick
                                .set(widgets::dialog(env.sp, Dialog::OpenFile { title: format!("Select {}", tool.name()), filter: None }));
                        }
                    });
                }
            });
            ui.add_space(12.0);

            kit::card_titled(ui, "Behaviour", |ui| {
                kit::toggle(ui, &mut draft.kill_adb_on_exit, "Stop the adb server when ADB Deck closes (not while the background service is on)");
                kit::toggle(ui, &mut draft.auto_reconnect, "Find and reconnect Wi-Fi devices automatically (applies after a restart)");
                kit::toggle(ui, &mut draft.background_service, "Keep devices connected in the background (starts at login, no window)");
                kit::toggle(ui, &mut draft.fetch_app_icons, "Fetch app icons and names from the Play Store");
                ui.label(
                    RichText::new("Sends the package names of the apps shown to play.rajkumaar.co.in and downloads icons from Google, using the curl program.")
                        .size(11.5)
                        .color(theme::pal(ui).text3),
                );
            });
            ui.add_space(12.0);

            ui.horizontal(|ui| {
                let changed = *draft != *env.settings;
                if kit::Btn::new("Apply").icon(kit::ph::CHECK).primary().enabled(changed).show(ui).clicked() {
                    applied = Some(draft.clone());
                }
                if kit::Btn::new("Revert").enabled(changed).show(ui).clicked() {
                    *draft = env.settings.clone();
                }
                if changed {
                    ui.label(RichText::new("Unsaved changes").size(12.0).color(p.warning));
                }
            });
            ui.add_space(12.0);

            kit::card_titled(ui, "Where your data is kept", |ui| {
                for (label, dir) in [("Settings", paths::config_dir()), ("Data", paths::data_dir())] {
                    ui.horizontal(|ui| {
                        kit::fixed_cell(ui, 70.0, 22.0, |ui| ui.label(RichText::new(label).color(p.text2)));
                        match dir {
                            Some(dir) => {
                                ui.add(egui::Label::new(RichText::new(dir.display().to_string()).monospace().size(12.0)).selectable(true));
                                if kit::Btn::new("Open").icon(kit::ph::ARROW_SQUARE_OUT).subtle().show(ui).clicked() && !self.open.busy() {
                                    self.open.set(env.sp.spawn(async move { adbdeck::desktop::open_path(&dir).await }));
                                }
                            }
                            None => {
                                ui.label(RichText::new("unknown (no home directory)").color(p.text3));
                            }
                        }
                    });
                }
            });
        });
        applied
    }

    /// Call after a settings change so the form shows the stored values.
    pub fn reset_draft(&mut self) {
        self.draft = None;
    }

    pub fn about(&mut self, ui: &mut Ui, status: &[ToolStatus]) -> bool {
        let mut recheck = false;
        let p = theme::pal(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            kit::card(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(60.0, 60.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, egui::CornerRadius::same(16), p.accent);
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, kit::ph::ANDROID_LOGO, egui::FontId::proportional(36.0), p.on_accent);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.label(kit::semibold("ADB Deck").size(22.0));
                        ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(p.text2));
                        ui.label(RichText::new("GPL-3.0-or-later; bundles the scrcpy server (Apache-2.0) and OpenH264 (BSD-2-Clause)").size(12.0).color(p.text3));
                    });
                });
                ui.add_space(10.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "A desktop companion for Android Debug Bridge on Linux, written in Rust. It reimplements the workflows of ADBPlus. App icons and names come from the Play Store through an open source metadata service. Screen mirroring speaks the scrcpy protocol with the bundled scrcpy 3.1 server.",
                        )
                        .color(p.text2),
                    )
                    .wrap(),
                );
            });
            ui.add_space(12.0);
            kit::card_titled(ui, "External tools", |ui| {
                for (i, s) in status.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(4.0);
                        kit::hsep(ui);
                        ui.add_space(6.0);
                    }
                    ui.horizontal_top(|ui| {
                        kit::fixed_cell(ui, 86.0, 22.0, |ui| ui.label(kit::semibold(s.tool.name()).size(14.0)));
                        match &s.path {
                            Some(path) => {
                                kit::fixed_cell(ui, 140.0, 22.0, |ui| kit::pill(ui, "Found", kit::Kind::Success));
                                ui.add(egui::Label::new(RichText::new(path.display().to_string()).monospace().size(12.0).color(p.text2)).selectable(true));
                            }
                            None => {
                                let (kind, text) =
                                    if s.tool.required() { (kit::Kind::Danger, "Missing · required") } else { (kit::Kind::Warning, "Missing · optional") };
                                kit::fixed_cell(ui, 140.0, 22.0, |ui| kit::pill(ui, text, kind));
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    ui.label(RichText::new(format!("Needed for: {}", s.tool.needed_for())).color(p.text2));
                                    kit::code_block(ui, s.tool.install_hint());
                                });
                            }
                        }
                    });
                }
                ui.add_space(10.0);
                if kit::Btn::new("Check again").icon(kit::ph::ARROWS_CLOCKWISE).show(ui).clicked() {
                    recheck = true;
                }
            });
        });
        recheck
    }
}

//! Inspectable WebViews and Chrome tabs on the device.

use egui::{RichText, Ui};

use adbdeck::adb::DeviceAdb;
use adbdeck::jobs::Slot;
use adbdeck::webview::{frontend_link, DevtoolsSocket, Inspectable};

use super::widgets;
use super::{kit, theme, DeviceCtx, Env};

#[derive(Default)]
pub struct Webview {
    started: bool,
    load: Slot<Vec<Inspectable>>,
    items: Vec<Inspectable>,
    error: Option<String>,
    open: Slot<()>,
    dev: Option<DeviceAdb>,
}

impl Webview {
    fn reload(&mut self, env: &Env, d: &DeviceCtx) {
        self.release();
        let dev = d.dev.clone();
        self.dev = Some(dev.clone());
        self.error = None;
        self.load.set(env.sp.spawn(async move {
            let sockets: Vec<DevtoolsSocket> = dev.devtools_sockets().await?;
            let mut items = Vec::new();
            for s in sockets {
                // One unresponsive WebView must not hide the others.
                if let Ok(i) = dev.inspect(s).await {
                    items.push(i);
                }
            }
            Ok(items)
        }));
    }

    /// Removes the local port forwards this panel created.
    fn release(&mut self) {
        if let Some(dev) = &self.dev {
            for i in self.items.drain(..) {
                let local = format!("tcp:{}", i.port);
                let _ = std::process::Command::new(dev.adb().path()).args(["-s", dev.serial(), "forward", "--remove", &local]).output();
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        if !self.started {
            self.started = true;
            self.reload(env, d);
        }
        if let Some(r) = self.load.poll() {
            match r {
                Ok(i) => self.items = i,
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if let Some(r) = self.open.poll() {
            env.notes.report("Open inspector", r);
        }
        let p = theme::pal(ui);
        kit::banner(
            ui,
            kit::Kind::Accent,
            "Needs a debuggable WebView (apps that call setWebContentsDebuggingEnabled) or Chrome. Inspector links open in your browser and may need Chromium and internet access.",
            None,
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if kit::Btn::new("Refresh").icon(kit::ph::ARROWS_CLOCKWISE).enabled(!self.load.busy()).show(ui).clicked() {
                self.reload(env, d);
            }
            if self.load.busy() {
                ui.add(egui::Spinner::new().size(16.0));
            }
        });
        ui.add_space(8.0);
        if let Some(e) = self.error.clone() {
            if widgets::error_banner(ui, &e, true) {
                self.reload(env, d);
            }
            ui.add_space(8.0);
        }
        if self.items.is_empty() {
            if !self.load.busy() && self.error.is_none() {
                kit::empty_state(
                    ui,
                    kit::ph::GLOBE,
                    "No inspectable WebViews",
                    "Open an app with a debuggable WebView on the device, then refresh.",
                    |_| {},
                );
            }
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for item in &self.items {
                kit::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        kit::avatar(ui, item.package.as_deref().unwrap_or(&item.socket.name), 32.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.label(kit::semibold(item.package.as_deref().unwrap_or(&item.socket.name)).size(14.0));
                            ui.label(
                                RichText::new(format!("socket {} · forwarded to port {}", item.socket.name, item.port))
                                    .size(11.5)
                                    .color(p.text3),
                            );
                        });
                    });
                    ui.add_space(6.0);
                    if item.pages.is_empty() {
                        ui.label(RichText::new("No pages.").color(p.text3));
                    }
                    for pg in &item.pages {
                        kit::hsep(ui);
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.set_max_width((ui.available_width() - 190.0).max(120.0));
                                ui.add(
                                    egui::Label::new(kit::medium(if pg.title.is_empty() { "(untitled)" } else { &pg.title })).truncate(),
                                );
                                ui.add(egui::Label::new(RichText::new(&pg.url).size(11.5).color(p.text3)).truncate());
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if let Some(link) = frontend_link(pg, item.port) {
                                    if kit::Btn::new("Inspect").icon(kit::ph::BUG).primary().show(ui).clicked() && !self.open.busy() {
                                        self.open.set(env.sp.spawn(async move { adbdeck::desktop::open_url(&link).await }));
                                    }
                                }
                                if kit::Btn::new("").icon(kit::ph::COPY).subtle().tip("Copy URL").show(ui).clicked() {
                                    ui.ctx().copy_text(pg.url.clone());
                                }
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
                ui.add_space(10.0);
            }
        });
    }
}

impl Drop for Webview {
    fn drop(&mut self) {
        self.release();
    }
}

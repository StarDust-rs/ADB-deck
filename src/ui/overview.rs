//! Device overview: hardware facts plus font scale, proxy and root.

use egui::{RichText, Ui};

use adbdeck::adb::overview::{format_uptime, validate_proxy, Overview as DeviceOverview};
use adbdeck::jobs::Slot;

use super::widgets::{self, human_size};
use super::{kit, theme, DeviceCtx, Env};

/// One vital sign: a number, what it is of, and optionally how full it is.
struct Tile {
    label: &'static str,
    value: String,
    detail: String,
    fraction: Option<f32>,
    /// A full bar is good news (battery) rather than a warning (storage).
    inverted: bool,
}

impl Tile {
    fn show(&self, ui: &mut Ui) {
        let p = theme::pal(ui);
        kit::card_margin(ui, 14, |ui| {
            ui.label(RichText::new(self.label).size(12.0).color(p.text2));
            ui.add(egui::Label::new(kit::semibold(&self.value).size(22.0)).truncate());
            ui.add(
                egui::Label::new(RichText::new(if self.detail.is_empty() { " " } else { &self.detail }).size(11.5).color(p.text3))
                    .truncate(),
            );
            ui.add_space(6.0);
            let w = ui.available_width();
            match self.fraction {
                Some(f) if self.inverted => kit::progress(ui, f, w),
                Some(f) => kit::meter(ui, f, w),
                None => {
                    ui.add_space(6.0);
                }
            }
        });
    }
}

struct Loaded {
    overview: DeviceOverview,
    proxy: String,
}

#[derive(Default)]
pub struct Overview {
    started: bool,
    load: Slot<Loaded>,
    data: Option<Loaded>,
    error: Option<String>,
    action: Slot<String>,
    font_scale: f32,
    proxy_edit: String,
}

impl Overview {
    fn reload(&mut self, env: &Env, d: &DeviceCtx) {
        let dev = d.dev.clone();
        self.error = None;
        self.load.set(env.sp.spawn(async move {
            let overview = dev.overview().await?;
            let proxy = dev.http_proxy().await.unwrap_or_default();
            Ok(Loaded { overview, proxy })
        }));
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        if !self.started {
            self.started = true;
            self.reload(env, d);
        }
        if let Some(r) = self.load.poll() {
            match r {
                Ok(l) => {
                    self.font_scale = l.overview.font_scale;
                    self.proxy_edit = l.proxy.clone();
                    self.data = Some(l);
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if let Some(r) = self.action.poll() {
            if let Some(msg) = env.notes.report("Device setting", r) {
                env.notes.info(msg);
                self.reload(env, d);
            }
        }

        if let Some(e) = self.error.clone() {
            if widgets::error_banner(ui, &e, true) {
                self.reload(env, d);
            }
            ui.add_space(8.0);
        }
        let Some(data) = self.data.as_ref() else {
            if self.error.is_none() {
                kit::busy(ui, "Reading device information…");
            }
            return;
        };
        let o = &data.overview;
        let p = theme::pal(ui);
        let busy = self.action.busy();
        let mut reload = false;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // ---- hero --------------------------------------------------------
            kit::card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        let name = if o.name.is_empty() { format!("{} {}", o.brand, o.model) } else { o.name.clone() };
                        ui.label(kit::semibold(name).size(20.0));
                        ui.label(
                            RichText::new(format!(
                                "{} {}  ·  Android {} (API {})",
                                o.brand, o.model, d.device.android_version, d.device.sdk_version
                            ))
                            .color(p.text2),
                        );
                        // Plain metadata; only a condition that changes what the user can do gets a badge.
                        let mut meta = o.serialno.clone();
                        if d.device.is_remote() {
                            meta.push_str("  ·  connected over Wi-Fi");
                        }
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(meta).size(12.0).color(p.text3));
                            if o.root {
                                kit::pill(ui, "Root", kit::Kind::Warning);
                            }
                            if o.bootloader_unlocked == Some(true) {
                                kit::pill(ui, "Bootloader unlocked", kit::Kind::Warning);
                            }
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::Btn::new("Reload").icon(kit::ph::ARROWS_CLOCKWISE).enabled(!self.load.busy()).show(ui).clicked() {
                            reload = true;
                        }
                        if self.load.busy() || busy {
                            ui.add(egui::Spinner::new().size(16.0));
                        }
                    });
                });
            });
            ui.add_space(12.0);

            // ---- vital signs -----------------------------------------------------------
            let dash = |v: &str| if v.is_empty() { "–".to_string() } else { v.to_string() };
            let used_ram = o.mem_total.saturating_sub(o.mem_available);
            let tiles = [
                Tile {
                    label: "Battery",
                    value: o.battery.level.map_or("–".into(), |l| format!("{l}%")),
                    detail: match (o.battery.charging, o.battery.temperature_tenths) {
                        (true, Some(t)) => format!("Charging · {:.1} °C", f64::from(t) / 10.0),
                        (false, Some(t)) => format!("On battery · {:.1} °C", f64::from(t) / 10.0),
                        (true, None) => "Charging".into(),
                        (false, None) => String::new(),
                    },
                    fraction: o.battery.level.map(|l| f32::from(l) / 100.0),
                    inverted: true,
                },
                Tile {
                    label: "Storage",
                    value: if o.storage_total > 0 { human_size(o.storage_used) } else { "–".into() },
                    detail: if o.storage_total > 0 { format!("of {}", human_size(o.storage_total)) } else { String::new() },
                    fraction: (o.storage_total > 0).then(|| o.storage_used as f32 / o.storage_total as f32),
                    inverted: false,
                },
                Tile {
                    label: "Memory",
                    value: if o.mem_total > 0 { human_size(used_ram) } else { "–".into() },
                    detail: if o.mem_total > 0 { format!("of {} in use", human_size(o.mem_total)) } else { String::new() },
                    fraction: (o.mem_total > 0).then(|| used_ram as f32 / o.mem_total as f32),
                    inverted: false,
                },
                Tile {
                    label: "Uptime",
                    value: o.uptime_secs.map_or("–".into(), format_uptime),
                    detail: "since the last boot".into(),
                    fraction: None,
                    inverted: false,
                },
            ];
            let columns = if ui.available_width() < 640.0 { 2 } else { 4 };
            for row in tiles.chunks(columns) {
                ui.columns(columns, |cols| {
                    for (col, tile) in cols.iter_mut().zip(row) {
                        tile.show(col);
                    }
                });
                ui.add_space(8.0);
            }
            ui.add_space(4.0);

            // ---- facts -------------------------------------------------------------------
            let flag = |v: Option<bool>, yes: &str, no: &str| v.map_or("–".to_string(), |v| (if v { yes } else { no }).to_string());
            kit::card_pair(
                ui,
                "ov-row1",
                ("Software", &mut |ui: &mut Ui| {
                    kit::kv_grid(ui, "ov-sw", |ui| {
                        kit::kv(ui, "Android", &format!("{} (API {})", d.device.android_version, d.device.sdk_version));
                        kit::kv(ui, "Security patch", &dash(&o.security_patch));
                        kit::kv(ui, "Build", &dash(&o.build_id));
                        kit::kv(ui, "Build type", &dash(&o.build_type));
                        kit::kv(ui, "Kernel", &dash(&o.kernel));
                        kit::kv(ui, "SELinux", &dash(&o.selinux));
                        kit::kv(ui, "Time zone", &dash(&o.timezone));
                    });
                }),
                ("Hardware", &mut |ui: &mut Ui| {
                    kit::kv_grid(ui, "ov-hw", |ui| {
                        kit::kv(ui, "Manufacturer", &dash(&o.manufacturer));
                        kit::kv(ui, "Chipset", &dash(&o.processor));
                        kit::kv(ui, "ABI", &dash(&o.abi));
                        kit::kv(ui, "Cores", &o.cpu_count.to_string());
                        kit::kv(ui, "Display", &format!("{} @ {} dpi", o.resolution, o.density));
                        if o.resolution != o.physical_resolution || o.density != o.physical_density {
                            kit::kv(ui, "Panel", &format!("{} @ {} dpi", o.physical_resolution, o.physical_density));
                        }
                    });
                }),
            );
            ui.add_space(12.0);
            kit::card_pair(
                ui,
                "ov-row2",
                ("Network", &mut |ui: &mut Ui| {
                    kit::kv_grid(ui, "ov-net", |ui| {
                        kit::kv(ui, "Wi-Fi", &dash(&o.wifi));
                        kit::kv(ui, "IP address", &dash(&o.ip));
                        kit::kv(ui, "MAC address", &dash(&o.mac));
                    });
                }),
                ("Security and apps", &mut |ui: &mut Ui| {
                    kit::kv_grid(ui, "ov-sec", |ui| {
                        kit::kv(ui, "Bootloader", &flag(o.bootloader_unlocked, "Unlocked", "Locked"));
                        kit::kv(ui, "Encryption", &flag(o.encrypted, "Encrypted", "Not encrypted"));
                        let apps = match (o.user_apps, o.total_apps) {
                            (Some(u), Some(t)) => format!("{u} installed · {} system", t.saturating_sub(u)),
                            _ => "–".into(),
                        };
                        kit::kv(ui, "Apps", &apps);
                    });
                }),
            );
            ui.add_space(12.0);

            // ---- adjustments ---------------------------------------------------------
            kit::card_titled(ui, "Adjustments", |ui| {
                egui::Grid::new("ov-adjust").num_columns(2).min_col_width(92.0).spacing([20.0, 12.0]).show(ui, |ui| {
                    ui.label(RichText::new("Font scale").color(p.text2));
                    ui.horizontal(|ui| {
                        kit::slider(ui, &mut self.font_scale, 0.5..=2.0, 0.05, 2, 220.0);
                        if kit::Btn::new("Apply").enabled(!busy).show(ui).clicked() {
                            let (dev, scale) = (d.dev.clone(), self.font_scale);
                            self.action.set(env.sp.spawn(async move {
                                dev.set_font_scale(scale).await?;
                                Ok(format!("Font scale set to {scale:.2}"))
                            }));
                        }
                    });
                    ui.end_row();

                    ui.label(RichText::new("HTTP proxy").color(p.text2));
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            kit::text_field(ui, &mut self.proxy_edit, "host:port", 220.0);
                            let valid = validate_proxy(&self.proxy_edit);
                            if kit::Btn::new("Set").enabled(!busy && valid.is_ok() && !self.proxy_edit.trim().is_empty()).show(ui).clicked()
                            {
                                let (dev, proxy) = (d.dev.clone(), self.proxy_edit.clone());
                                self.action.set(env.sp.spawn(async move {
                                    dev.set_http_proxy(Some(&proxy)).await?;
                                    Ok(format!("Proxy set to {}", proxy.trim()))
                                }));
                            }
                            if kit::Btn::new("Clear").enabled(!busy).show(ui).clicked() {
                                let dev = d.dev.clone();
                                self.action.set(env.sp.spawn(async move {
                                    dev.set_http_proxy(None).await?;
                                    Ok("Proxy cleared".to_string())
                                }));
                            }
                        });
                        if !self.proxy_edit.trim().is_empty() {
                            if let Err(e) = validate_proxy(&self.proxy_edit) {
                                ui.label(RichText::new(e.to_string()).size(12.0).color(p.warning));
                            }
                        }
                    });
                    ui.end_row();

                    ui.label(RichText::new("Root access").color(p.text2));
                    if o.root {
                        kit::pill(ui, "adbd runs as root", kit::Kind::Success);
                    } else if kit::Btn::new("Restart adbd as root")
                        .enabled(!busy)
                        .tip("Only works on emulators and userdebug builds")
                        .show(ui)
                        .clicked()
                    {
                        let dev = d.dev.clone();
                        self.action.set(env.sp.spawn(async move {
                            dev.enable_root().await?;
                            Ok("adbd restarted as root".to_string())
                        }));
                    }
                    ui.end_row();
                });
            });
        });
        if reload {
            self.reload(env, d);
        }
    }
}

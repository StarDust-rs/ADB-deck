//! Running processes (a periodically refreshed `top` snapshot).

use std::time::{Duration, Instant};

use egui::{RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use adbdeck::adb::procs::ProcessInfo;
use adbdeck::jobs::Slot;

use adbdeck::text::contains_ci;

use super::widgets::{self, human_size};
use super::{kit, theme, DeviceCtx, Env};

const REFRESH: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Sort {
    #[default]
    Cpu,
    Memory,
    Name,
    Pid,
}

#[derive(Default)]
pub struct Processes {
    load: Slot<Vec<ProcessInfo>>,
    list: Vec<ProcessInfo>,
    loaded_at: Option<Instant>,
    error: Option<String>,
    filter: String,
    sort: Sort,
    auto: Option<bool>,
    selected: Option<u32>,
    op: Slot<String>,
}

impl Processes {
    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        let auto = *self.auto.get_or_insert(true);
        if let Some(r) = self.load.poll() {
            match r {
                Ok(l) => {
                    self.list = l;
                    self.error = None;
                }
                Err(e) => self.error = Some(e.to_string()),
            }
            self.loaded_at = Some(Instant::now());
        }
        if let Some(r) = self.op.poll() {
            if let Some(m) = env.notes.report("Process action", r) {
                env.notes.info(m);
            }
        }
        let due = self.loaded_at.is_none_or(|t| auto && t.elapsed() >= REFRESH);
        if due && !self.load.busy() && !(self.error.is_some() && !auto) {
            let dev = d.dev.clone();
            self.load.set(env.sp.spawn(async move { dev.processes().await }));
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));

        let p = theme::pal(ui);
        kit::card_margin(ui, 8, |ui| {
            ui.horizontal(|ui| {
                let narrow = ui.available_width() < 740.0;
                kit::search(
                    ui,
                    &mut self.filter,
                    if narrow { "Filter" } else { "Filter by name or user" },
                    if narrow { 120.0 } else { 230.0 },
                );
                kit::segmented(
                    ui,
                    &mut self.sort,
                    &[(Sort::Cpu, "CPU"), (Sort::Memory, "Memory"), (Sort::Name, "Name"), (Sort::Pid, "PID")],
                );
                if kit::toggle(ui, &mut env.data.only_package_processes, "Apps only").changed() {
                    *env.data_dirty = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::Btn::new("")
                        .icon(kit::ph::ARROWS_CLOCKWISE)
                        .subtle()
                        .enabled(!self.load.busy())
                        .tip("Refresh now")
                        .show(ui)
                        .clicked()
                    {
                        self.loaded_at = None;
                    }
                    let mut a = auto;
                    if kit::toggle(ui, &mut a, "Auto").changed() {
                        self.auto = Some(a);
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
                self.loaded_at = None;
            }
            ui.add_space(8.0);
        }

        let f = self.filter.as_str();
        let only = env.data.only_package_processes;
        let mut rows: Vec<&ProcessInfo> =
            self.list.iter().filter(|p| (!only || p.package.is_some()) && (contains_ci(&p.name, f) || contains_ci(&p.user, f))).collect();
        match self.sort {
            Sort::Cpu => rows.sort_by(|a, b| b.cpu.total_cmp(&a.cpu)),
            Sort::Memory => rows.sort_by_key(|p| std::cmp::Reverse(p.res_bytes)),
            Sort::Name => rows.sort_by(|a, b| a.name.cmp(&b.name)),
            Sort::Pid => rows.sort_by_key(|p| p.pid),
        }
        if self.list.is_empty() {
            if self.load.busy() {
                kit::busy(ui, "Reading processes…");
            }
            return;
        }
        let selected_pkg = self.selected.and_then(|pid| self.list.iter().find(|p| p.pid == pid)).and_then(|p| p.package.clone());
        ui.horizontal(|ui| {
            let label = selected_pkg.as_deref().map(|p| format!("Force stop {p}")).unwrap_or_else(|| "Force stop app".into());
            if kit::Btn::new(&label).icon(kit::ph::STOP).danger().enabled(selected_pkg.is_some() && !self.op.busy()).show(ui).clicked() {
                if let Some(pkg) = selected_pkg.clone() {
                    let dev = d.dev.clone();
                    self.op.set(env.sp.spawn(async move {
                        dev.force_stop(&pkg).await?;
                        Ok(format!("Stopped {pkg}"))
                    }));
                    self.loaded_at = None;
                }
            }
            ui.label(RichText::new(format!("{} processes", rows.len())).color(p.text2));
        });
        ui.add_space(6.0);
        let mut clicked = None;
        let selected = self.selected;
        let avail_h = ui.available_height();
        kit::well(ui, |ui| {
            ui.set_min_height(avail_h - 10.0);
            ui.set_width(ui.available_width());
            TableBuilder::new(ui)
                .striped(true)
                .sense(Sense::click())
                .resizable(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(70.0))
                .column(Column::remainder().at_least(160.0).clip(true))
                .column(Column::exact(70.0))
                .column(Column::exact(90.0))
                .column(Column::exact(90.0))
                .column(Column::exact(100.0).clip(true))
                .header(30.0, |mut h| {
                    for t in ["PID", "Name", "CPU %", "Memory", "CPU time", "User"] {
                        h.col(|ui| {
                            ui.label(kit::semibold(t).size(12.0).color(p.text2));
                        });
                    }
                })
                .body(|body| {
                    body.rows(28.0, rows.len(), |mut row| {
                        let pr = rows[row.index()];
                        row.set_selected(selected == Some(pr.pid));
                        row.col(|ui| {
                            ui.label(RichText::new(pr.pid.to_string()).monospace().size(12.0).color(p.text2));
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(&pr.name).selectable(false).truncate());
                        });
                        row.col(|ui| {
                            let color = if pr.cpu >= 50.0 {
                                p.danger
                            } else if pr.cpu >= 15.0 {
                                p.warning
                            } else {
                                p.text
                            };
                            ui.label(RichText::new(format!("{:.1}", pr.cpu)).monospace().size(12.5).color(color));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(human_size(pr.res_bytes)).color(p.text2));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&pr.time).monospace().size(12.0).color(p.text2));
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(RichText::new(&pr.user).color(p.text2)).truncate());
                        });
                        if row.response().clicked() {
                            clicked = Some(pr.pid);
                        }
                    });
                });
        });
        if clicked.is_some() {
            self.selected = clicked;
        }
    }
}

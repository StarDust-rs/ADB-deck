//! Installed apps: list, install, and per-app actions.

use std::collections::VecDeque;
use std::path::PathBuf;

use egui::{RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};

use adbdeck::adb::apps::{validate_package, PackageInfo};
use adbdeck::adb::manage::AppDetails;
use adbdeck::error::Error;
use adbdeck::jobs::{Job, Slot};
use adbdeck::text::contains_ci;

use super::app_icons::AppIcons;
use super::app_manage::{AdvancedTab, PermissionsTab, Request};
use super::widgets::{self, human_size, Dialog};
use super::{kit, theme, DeviceCtx, Env};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Sort {
    #[default]
    Name,
    Size,
    Updated,
}

enum Pending {
    Uninstall(String),
    ClearData(String),
    RemoveForUser(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Overview,
    Permissions,
    Advanced,
}

#[derive(PartialEq)]
enum InstallState {
    Queued,
    Running,
    Done,
    Failed(String),
}

struct Install {
    apk: PathBuf,
    state: InstallState,
}

#[derive(Default)]
pub struct Apps {
    started: bool,
    load: Slot<Vec<PackageInfo>>,
    packages: Vec<PackageInfo>,
    error: Option<String>,
    filter: String,
    sort: Sort,
    selected: Option<String>,
    op: Slot<String>,
    pending: Option<Pending>,
    dialog: Slot<Vec<PathBuf>>,
    dialog_for: Option<DialogFor>,
    installs: VecDeque<Install>,
    install_job: Option<Job<()>>,
    /// Changes made from the Permissions and Advanced tabs; unlike `op` they
    /// refresh only the open app, not the whole list.
    act: Slot<String>,
    act_reloads_list: bool,
    tab: Tab,
    detail_load: Slot<AppDetails>,
    app_details: Option<AppDetails>,
    /// The package whose details could not be read, and why.
    detail_error: Option<(String, String)>,
    perm_filter: String,
    /// Text of the "restore a removed app" dialog while it is open.
    restore: Option<String>,
    /// Total cache size when "Clear all caches" started, to report what it freed.
    cache_before: Option<u64>,
    icons: AppIcons,
    /// Indices into `packages` of the rows shown, in order; rebuilt only when
    /// the list or one of the inputs below changes.
    view: Vec<usize>,
    view_key: Option<ViewKey>,
    /// Bumped whenever `packages` is replaced.
    generation: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct ViewKey {
    generation: u64,
    filter: String,
    sort: Sort,
    show_system: bool,
}

enum DialogFor {
    Apks,
    Export(String, String),
}

/// `dir/stem.ext`, or `dir/stem (n).ext` when that file already exists, so an
/// export never replaces a file the user already has.
fn unused_path(dir: &std::path::Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (1..).map(|n| dir.join(format!("{stem} ({n}).{ext}"))).find(|p| !p.exists()).unwrap_or(first)
}

impl Apps {
    /// Stops the on-device helper; called when the app exits.
    pub fn close(&mut self, rt: &tokio::runtime::Runtime) {
        self.icons.close(rt);
    }

    fn reload(&mut self, env: &Env, d: &DeviceCtx) {
        let dev = d.dev.clone();
        self.error = None;
        self.icons.retry_missing();
        self.load.set(env.sp.spawn(async move { dev.list_packages().await }));
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx, queued: &mut Vec<PathBuf>) {
        if !self.started {
            self.started = true;
            self.reload(env, d);
        }
        for apk in queued.drain(..) {
            self.installs.push_back(Install { apk, state: InstallState::Queued });
        }
        self.poll(env, d);
        self.icons.begin_frame(ui.ctx());

        kit::card_margin(ui, 8, |ui| {
            ui.horizontal(|ui| {
                let narrow = ui.available_width() < 1000.0;
                kit::search(ui, &mut self.filter, if narrow { "Filter" } else { "Filter packages" }, if narrow { 130.0 } else { 240.0 });
                kit::segmented(ui, &mut self.sort, &[(Sort::Name, "Name"), (Sort::Size, "Size"), (Sort::Updated, "Updated")]);
                if kit::toggle(ui, &mut env.data.show_system_packages, "System apps").changed() {
                    *env.data_dirty = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::Btn::new("Install APK").icon(kit::ph::DOWNLOAD_SIMPLE).primary().enabled(!self.dialog.busy()).show(ui).clicked()
                    {
                        self.dialog_for = Some(DialogFor::Apks);
                        self.dialog.set(widgets::dialog(
                            env.sp,
                            Dialog::OpenFiles {
                                title: "Choose APK files".into(),
                                filter: Some(("Android packages".into(), vec!["apk".into()])),
                            },
                        ));
                    }
                    if kit::Btn::new("")
                        .icon(kit::ph::ARROWS_CLOCKWISE)
                        .subtle()
                        .enabled(!self.load.busy())
                        .tip("Reload")
                        .show(ui)
                        .clicked()
                    {
                        self.reload(env, d);
                    }
                    if kit::Btn::new("")
                        .icon(kit::ph::ARROW_COUNTER_CLOCKWISE)
                        .subtle()
                        .enabled(!self.op.busy())
                        .tip("Restore an app that was removed for this user")
                        .show(ui)
                        .clicked()
                    {
                        self.restore = Some(String::new());
                    }
                    let clear_all = kit::Btn::new(if narrow { "" } else { "Clear all caches" })
                        .icon(kit::ph::BROOM)
                        .enabled(!self.op.busy() && !self.load.busy() && !self.packages.is_empty())
                        .tip("Frees the cache of every app on the phone");
                    if clear_all.show(ui).clicked() {
                        self.cache_before = Some(self.packages.iter().map(|p| p.cache_size).sum());
                        let dev = d.dev.clone();
                        self.op.set(env.sp.spawn(async move {
                            dev.trim_all_caches().await?;
                            Ok("Cleared the cache of every app".to_string())
                        }));
                    }
                    if self.load.busy() || self.op.busy() || self.act.busy() {
                        ui.add(egui::Spinner::new().size(16.0));
                    }
                });
            });
        });
        ui.add_space(10.0);
        if let Some(e) = self.error.clone() {
            if widgets::error_banner(ui, &e, true) {
                self.reload(env, d);
            }
            ui.add_space(8.0);
        }
        if let Some(why) = self.icons.failure().map(str::to_owned) {
            let text = format!("App icons and names are unavailable: {why}");
            if kit::banner(ui, kit::Kind::Warning, &text, Some("Retry")) {
                self.icons.retry_missing();
            }
            ui.add_space(8.0);
        }
        if !self.installs.is_empty() {
            self.install_status(ui);
            ui.add_space(10.0);
        }
        self.confirm_dialog(ui.ctx(), env, d);
        self.restore_dialog(ui.ctx(), env, d);

        let p = theme::pal(ui);
        let fr = egui::Frame::new()
            .fill(p.card)
            .stroke(egui::Stroke::new(1.0_f32, p.stroke))
            .corner_radius(egui::CornerRadius::same(theme::RADIUS_CARD))
            .inner_margin(egui::Margin::same(14))
            .outer_margin(egui::Margin { left: 10, right: 0, top: 0, bottom: 0 });
        egui::SidePanel::right("app-details")
            .resizable(true)
            .default_width(300.0)
            .width_range(240.0..=420.0)
            .frame(fr)
            .show_separator_line(false)
            .show_inside(ui, |ui| self.details(ui, env, d));
        egui::CentralPanel::default().frame(egui::Frame::NONE).show_inside(ui, |ui| self.table(ui, env));
        self.icons.pump(env, d);
    }

    /// Rebuilds the list of visible rows when the packages or the filter changed.
    fn refresh_view(&mut self, show_system: bool) {
        let key = ViewKey { generation: self.generation, filter: self.filter.clone(), sort: self.sort, show_system };
        if self.view_key.as_ref() == Some(&key) {
            return;
        }
        let needle = self.filter.as_str();
        let packages = &self.packages;
        let mut view: Vec<usize> = (0..packages.len())
            .filter(|&i| {
                let p = &packages[i];
                (show_system || !p.system) && contains_ci(&p.name, needle)
            })
            .collect();
        match self.sort {
            Sort::Name => view.sort_by(|&a, &b| packages[a].name.cmp(&packages[b].name)),
            Sort::Size => view.sort_by_key(|&i| std::cmp::Reverse(packages[i].storage_total())),
            Sort::Updated => view.sort_by(|&a, &b| packages[b].last_update.cmp(&packages[a].last_update)),
        }
        self.view = view;
        self.view_key = Some(key);
    }

    fn table(&mut self, ui: &mut Ui, env: &Env) {
        if self.packages.is_empty() {
            if self.load.busy() {
                kit::busy(ui, "Reading installed apps (this can take a moment)…");
            }
            return;
        }
        self.refresh_view(env.data.show_system_packages);
        if self.view.is_empty() {
            kit::empty_state(ui, kit::ph::PACKAGE, "No apps match", "Change the filter or show system apps.", |_| {});
            return;
        }
        let pal = theme::pal(ui);
        let mut clicked = None;
        let selected = self.selected.clone();
        let avail_h = ui.available_height();
        let (packages, view) = (&self.packages, &self.view);
        kit::well(ui, |ui| {
            ui.set_min_height(avail_h - 10.0);
            ui.set_width(ui.available_width());
            // Version and update date are dropped when the list gets narrow (small window).
            let wide = ui.available_width() > 520.0;
            let mut table = TableBuilder::new(ui)
                .striped(true)
                .sense(Sense::click())
                .resizable(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::remainder().at_least(140.0).clip(true));
            if wide {
                table = table.column(Column::exact(86.0).clip(true));
            }
            table = table.column(Column::exact(80.0));
            if wide {
                table = table.column(Column::exact(88.0));
            }
            table
                .header(30.0, |mut h| {
                    let mut titles = vec!["Package"];
                    if wide {
                        titles.push("Version");
                    }
                    titles.push("Size");
                    if wide {
                        titles.push("Updated");
                    }
                    for t in titles {
                        h.col(|ui| {
                            ui.label(kit::semibold(t).size(12.0).color(pal.text2));
                        });
                    }
                })
                .body(|body| {
                    body.rows(34.0, view.len(), |mut row| {
                        let p = &packages[view[row.index()]];
                        row.set_selected(selected.as_deref() == Some(p.name.as_str()));
                        row.col(|ui| {
                            self.icons.want(p);
                            if !self.icons.show(ui, &p.name, 24.0) {
                                kit::avatar(ui, &p.name, 24.0);
                            }
                            let text = if p.enabled { RichText::new(&p.name) } else { RichText::new(&p.name).color(pal.text3) };
                            ui.add(egui::Label::new(text).selectable(false).truncate());
                            if !p.enabled {
                                kit::pill(ui, "Disabled", kit::Kind::Warning);
                            }
                        });
                        if wide {
                            row.col(|ui| {
                                ui.add(egui::Label::new(RichText::new(&p.version_name).color(pal.text2)).selectable(false).truncate());
                            });
                        }
                        row.col(|ui| {
                            ui.label(
                                RichText::new(if p.storage_total() > 0 { human_size(p.storage_total()) } else { String::new() })
                                    .color(pal.text2),
                            );
                        });
                        if wide {
                            row.col(|ui| {
                                ui.label(RichText::new(p.last_update.split(' ').next().unwrap_or("")).color(pal.text2));
                            });
                        }
                        if row.response().clicked() {
                            clicked = Some(p.name.clone());
                        }
                    });
                });
        });
        if let Some(name) = clicked {
            self.selected = Some(name);
            self.app_details = None;
            self.detail_error = None;
        }
    }

    fn details(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        let p = theme::pal(ui);
        let Some(pkg) = self.selected.as_ref().and_then(|n| self.packages.iter().find(|p| &p.name == n)).cloned() else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(kit::ph::CURSOR_CLICK).size(30.0).color(p.text3));
                ui.add_space(4.0);
                ui.label(RichText::new("Select an app to see\ndetails and actions").color(p.text3));
            });
            return;
        };
        ui.horizontal(|ui| {
            self.icons.want(&pkg);
            if !self.icons.show(ui, &pkg.name, 40.0) {
                kit::avatar(ui, &pkg.name, 40.0);
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                let title = self.icons.label(&pkg.name).unwrap_or_else(|| pkg.name.rsplit('.').next().unwrap_or(&pkg.name));
                ui.add(egui::Label::new(kit::semibold(title).size(15.0)).truncate());
                ui.add(egui::Label::new(RichText::new(&pkg.name).monospace().size(11.0).color(p.text2)).truncate());
            });
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            kit::pill(ui, if pkg.system { "System" } else { "User" }, if pkg.system { kit::Kind::Warning } else { kit::Kind::Accent });
            if !pkg.enabled {
                kit::pill(ui, "Disabled", kit::Kind::Danger);
            }
            if let Some(a) = self.app_details.as_ref().filter(|a| a.package == pkg.name) {
                if a.state.suspended {
                    kit::pill(ui, "Suspended", kit::Kind::Warning);
                }
                if a.state.hidden {
                    kit::pill(ui, "Hidden", kit::Kind::Warning);
                }
            }
        });
        ui.add_space(8.0);
        kit::segmented(ui, &mut self.tab, &[(Tab::Overview, "Overview"), (Tab::Permissions, "Permissions"), (Tab::Advanced, "Advanced")]);
        ui.add_space(8.0);

        // The Permissions and Advanced tabs read their data from the phone once per app.
        if self.tab != Tab::Overview {
            let have = self.app_details.as_ref().is_some_and(|a| a.package == pkg.name);
            let failed = self.detail_error.as_ref().is_some_and(|(n, _)| *n == pkg.name);
            if !have && !failed && !self.detail_load.busy() {
                self.refresh_details(env, d, &pkg.name);
            }
        }

        egui::ScrollArea::vertical().id_salt("app-detail-scroll").auto_shrink([false, false]).show(ui, |ui| match self.tab {
            Tab::Overview => self.overview_tab(ui, env, d, &pkg),
            Tab::Permissions | Tab::Advanced => {
                if let Some((n, why)) = self.detail_error.clone().filter(|(n, _)| *n == pkg.name) {
                    if kit::banner(ui, kit::Kind::Danger, &why, Some("Retry")) {
                        self.detail_error = None;
                        self.refresh_details(env, d, &n);
                    }
                    return;
                }
                let busy = self.op.busy() || self.act.busy();
                let Some(details) = self.app_details.as_ref().filter(|a| a.package == pkg.name) else {
                    kit::busy(ui, "Reading the app's settings…");
                    return;
                };
                let request = if self.tab == Tab::Permissions {
                    PermissionsTab { details, filter: &mut self.perm_filter, busy }.show(ui)
                } else {
                    AdvancedTab { details, cache_size: human_size(pkg.cache_size), busy }.show(ui)
                };
                if let Some(request) = request {
                    self.request(env, d, &pkg.name, request);
                }
            }
        });
    }

    fn refresh_details(&mut self, env: &Env, d: &DeviceCtx, name: &str) {
        let (dev, n) = (d.dev.clone(), name.to_string());
        self.detail_load.set(env.sp.spawn(async move { dev.app_details(&n).await }));
    }

    /// Handles a change asked for in the details panel; removal waits for confirmation.
    fn request(&mut self, env: &Env, d: &DeviceCtx, name: &str, request: Request) {
        if request == Request::RemoveForUser {
            self.pending = Some(Pending::RemoveForUser(name.to_string()));
        } else {
            self.start(env, d, name, request);
        }
    }

    fn start(&mut self, env: &Env, d: &DeviceCtx, name: &str, request: Request) {
        self.act_reloads_list = request.changes_list();
        self.act.set(env.sp.spawn(request.run(d.dev.clone(), name.to_string())));
    }

    fn overview_tab(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx, pkg: &PackageInfo) {
        let p = theme::pal(ui);
        let busy = self.op.busy() || self.act.busy();
        kit::kv_grid(ui, "app-info", |ui| {
            kit::kv(ui, "Version", &format!("{} ({})", pkg.version_name, pkg.version_code));
            kit::kv(
                ui,
                "SDK",
                &format!(
                    "min {} / target {}",
                    pkg.min_sdk.map_or("?".into(), |v| v.to_string()),
                    pkg.target_sdk.map_or("?".into(), |v| v.to_string())
                ),
            );
            kit::kv(ui, "Installed", &pkg.first_install);
            kit::kv(ui, "Updated", &pkg.last_update);
            kit::kv(ui, "App", &human_size(pkg.app_size));
            kit::kv(ui, "Data", &human_size(pkg.data_size));
            kit::kv(ui, "Cache", &human_size(pkg.cache_size));
        });
        ui.add_space(4.0);
        ui.label(RichText::new(&pkg.apk_path).monospace().size(11.0).color(p.text3));
        ui.add_space(12.0);
        let name = pkg.name.clone();
        let run = |this: &mut Self,
                   label: &'static str,
                   fut: std::pin::Pin<Box<dyn std::future::Future<Output = adbdeck::error::Result<()>> + Send>>| {
            this.op.set(env.sp.spawn(async move {
                fut.await?;
                Ok(label.to_string())
            }));
        };
        // Two equal columns: the buttons never push the panel wider than its frame.
        let w = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).floor();
        ui.horizontal(|ui| {
            if kit::Btn::new("Launch").icon(kit::ph::PLAY).primary().min_width(w).enabled(!busy && pkg.enabled).show(ui).clicked() {
                let (dev, n) = (d.dev.clone(), name.clone());
                run(self, "App launched", Box::pin(async move { dev.launch(&n).await }));
            }
            if kit::Btn::new("Force stop").icon(kit::ph::STOP).min_width(w).enabled(!busy).show(ui).clicked() {
                let (dev, n) = (d.dev.clone(), name.clone());
                run(self, "App stopped", Box::pin(async move { dev.force_stop(&n).await }));
            }
        });
        ui.horizontal(|ui| {
            let (label, icon, enable) =
                if pkg.enabled { ("Disable", kit::ph::PROHIBIT, false) } else { ("Enable", kit::ph::CHECK_CIRCLE, true) };
            if kit::Btn::new(label).icon(icon).min_width(w).enabled(!busy).show(ui).clicked() {
                let (dev, n) = (d.dev.clone(), name.clone());
                run(self, "App state changed", Box::pin(async move { dev.set_package_enabled(&n, enable).await }));
            }
            if kit::Btn::new("Clear cache").icon(kit::ph::BROOM).min_width(w).enabled(!busy).show(ui).clicked() {
                self.request(env, d, &name, Request::ClearCache);
            }
        });
        ui.horizontal(|ui| {
            if kit::Btn::new("Export APK")
                .icon(kit::ph::EXPORT)
                .min_width(w)
                .enabled(!self.dialog.busy() && !pkg.apk_path.is_empty())
                .tip("Saves the base APK")
                .show(ui)
                .clicked()
            {
                self.dialog_for = Some(DialogFor::Export(name.clone(), pkg.apk_path.clone()));
                self.dialog.set(widgets::dialog(env.sp, Dialog::Folder { title: "Save APK to folder".into() }));
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if kit::Btn::new("Clear data").icon(kit::ph::ERASER).danger().min_width(w).enabled(!busy).show(ui).clicked() {
                self.pending = Some(Pending::ClearData(name.clone()));
            }
            if kit::Btn::new("Uninstall").icon(kit::ph::TRASH).danger().min_width(w).enabled(!busy).show(ui).clicked() {
                self.pending = Some(Pending::Uninstall(name.clone()));
            }
        });
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, env: &mut Env, d: &DeviceCtx) {
        let Some(p) = &self.pending else { return };
        let (text, verb) = match p {
            Pending::Uninstall(n) => (format!("Uninstall {n}? Its data is removed too."), "Uninstall"),
            Pending::ClearData(n) => (format!("Clear all data of {n}? This cannot be undone."), "Clear data"),
            Pending::RemoveForUser(n) => (
                format!("Remove {n} for this user? Its data is kept and Restore (in the toolbar) brings it back. A user app that was removed this way can only be reinstalled from its APK or the store."),
                "Remove",
            ),
        };
        let mut answer = None;
        let (icon, title) = match p {
            Pending::Uninstall(_) => (kit::ph::TRASH, "Uninstall app"),
            Pending::ClearData(_) => (kit::ph::ERASER, "Clear app data"),
            Pending::RemoveForUser(_) => (kit::ph::TRASH, "Remove app for this user"),
        };
        let resp = kit::dialog(ctx, "app-confirm", icon, title, kit::Kind::Danger, 380.0, |ui| {
            ui.add(egui::Label::new(RichText::new(text).color(theme::pal(ui).text2)).wrap());
            kit::dialog_buttons(ui, |ui| {
                if kit::Btn::new(verb).danger().show(ui).clicked() {
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
                let dev = d.dev.clone();
                match self.pending.take() {
                    Some(Pending::Uninstall(n)) => self.op.set(env.sp.spawn(async move {
                        dev.uninstall(&n).await?;
                        Ok(format!("Uninstalled {n}"))
                    })),
                    Some(Pending::ClearData(n)) => self.op.set(env.sp.spawn(async move {
                        dev.clear_data(&n).await?;
                        Ok(format!("Cleared data of {n}"))
                    })),
                    Some(Pending::RemoveForUser(n)) => self.start(env, d, &n, Request::RemoveForUser),
                    None => {}
                }
            }
            Some(false) => self.pending = None,
            None => {}
        }
    }

    /// Asks for the name of an app to bring back after "Remove for this user".
    fn restore_dialog(&mut self, ctx: &egui::Context, env: &mut Env, d: &DeviceCtx) {
        let Some(text) = self.restore.as_mut() else { return };
        let mut answer = None;
        let valid = validate_package(text.trim()).is_ok();
        let resp =
            kit::dialog(ctx, "app-restore", kit::ph::ARROW_COUNTER_CLOCKWISE, "Restore a removed app", kit::Kind::Accent, 380.0, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Enter the package name of an app removed for this user, for example a system app you removed earlier.",
                        )
                        .color(theme::pal(ui).text2),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
                let edit = kit::text_field_mono(ui, text, "com.example.app", ui.available_width());
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid {
                    answer = Some(true);
                }
                kit::dialog_buttons(ui, |ui| {
                    if kit::Btn::new("Restore").primary().enabled(valid).show(ui).clicked() {
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
                let name = self.restore.take().unwrap_or_default().trim().to_string();
                let dev = d.dev.clone();
                self.op.set(env.sp.spawn(async move {
                    dev.install_existing(&name).await?;
                    Ok(format!("Restored {name}"))
                }));
            }
            Some(false) => self.restore = None,
            None => {}
        }
    }

    fn install_status(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        kit::card_margin(ui, 12, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(kit::ph::DOWNLOAD_SIMPLE).size(16.0).color(p.accent_text));
                ui.label(kit::semibold("Installing").size(13.5));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::Btn::new("Clear finished").subtle().show(ui).clicked() {
                        self.installs.retain(|i| matches!(i.state, InstallState::Queued | InstallState::Running));
                    }
                });
            });
            for i in &self.installs {
                let name = i.apk.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.add_sized([260.0, 20.0], egui::Label::new(name).truncate());
                    match &i.state {
                        InstallState::Queued => {
                            kit::pill(ui, "Waiting", kit::Kind::Neutral);
                        }
                        InstallState::Running => {
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(RichText::new("Installing…").color(p.text2));
                        }
                        InstallState::Done => {
                            kit::pill(ui, "Installed", kit::Kind::Success);
                        }
                        InstallState::Failed(m) => {
                            kit::pill(ui, "Failed", kit::Kind::Danger);
                            ui.add(egui::Label::new(RichText::new(m).size(12.0).color(p.danger)).truncate());
                        }
                    }
                });
            }
        });
    }

    fn poll(&mut self, env: &mut Env, d: &DeviceCtx) {
        if let Some(r) = self.load.poll() {
            match r {
                Ok(p) => {
                    if let Some(before) = self.cache_before.take() {
                        let freed = before.saturating_sub(p.iter().map(|p| p.cache_size).sum());
                        if freed > 0 {
                            env.notes.info(format!("Freed {} of cache", human_size(freed)));
                        }
                    }
                    self.packages = p;
                    self.generation += 1;
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if let Some(r) = self.op.poll() {
            if let Some(msg) = env.notes.report("App action", r) {
                env.notes.info(msg);
            }
            self.reload(env, d);
        }
        if let Some(r) = self.act.poll() {
            if let Some(msg) = env.notes.report("App action", r) {
                env.notes.info(msg);
            }
            if let Some(name) = self.selected.clone() {
                self.refresh_details(env, d, &name);
            }
            if std::mem::take(&mut self.act_reloads_list) {
                self.reload(env, d);
            }
        }
        if let Some(r) = self.detail_load.poll() {
            match r {
                Ok(details) => {
                    self.detail_error = None;
                    self.app_details = Some(details);
                }
                Err(Error::Cancelled) => {}
                Err(e) => self.detail_error = self.selected.clone().map(|n| (n, e.to_string())),
            }
        }
        if let Some(r) = self.dialog.poll() {
            let what = self.dialog_for.take();
            if let (Some(paths), Some(what)) = (env.notes.report("File dialog", r), what) {
                match what {
                    DialogFor::Apks => {
                        for apk in paths {
                            self.installs.push_back(Install { apk, state: InstallState::Queued });
                        }
                    }
                    DialogFor::Export(name, remote) => {
                        if let Some(dir) = paths.into_iter().next() {
                            // The name came from the device; check it before it becomes a path.
                            match validate_package(&name) {
                                Ok(_) => {
                                    let local = unused_path(&dir, &name, "apk");
                                    let dev = d.dev.clone();
                                    self.op.set(env.sp.spawn(async move {
                                        dev.pull_to(&remote, &local).await?;
                                        Ok(format!("Saved {}", local.display()))
                                    }));
                                }
                                Err(e) => env.notes.error("Export APK", &e),
                            }
                        }
                    }
                }
            }
        }
        // Install queue: one APK at a time.
        if let Some(job) = self.install_job.as_mut() {
            if let Some(r) = job.poll() {
                self.install_job = None;
                if let Some(cur) = self.installs.iter_mut().find(|i| i.state == InstallState::Running) {
                    match r {
                        Ok(()) => cur.state = InstallState::Done,
                        Err(Error::Cancelled) => cur.state = InstallState::Failed("cancelled".into()),
                        Err(e) => {
                            env.notes.error("Install", &e);
                            cur.state = InstallState::Failed(e.to_string());
                        }
                    }
                }
                self.reload(env, d);
            }
        }
        if self.install_job.is_none() {
            if let Some(next) = self.installs.iter_mut().find(|i| i.state == InstallState::Queued) {
                next.state = InstallState::Running;
                let (dev, apk) = (d.dev.clone(), next.apk.clone());
                self.install_job = Some(env.sp.spawn(async move { dev.install_apk(&apk).await }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_never_reuse_an_existing_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unused_path(dir.path(), "com.a", "apk"), dir.path().join("com.a.apk"));
        std::fs::write(dir.path().join("com.a.apk"), b"x").unwrap();
        assert_eq!(unused_path(dir.path(), "com.a", "apk"), dir.path().join("com.a (1).apk"));
        std::fs::write(dir.path().join("com.a (1).apk"), b"x").unwrap();
        assert_eq!(unused_path(dir.path(), "com.a", "apk"), dir.path().join("com.a (2).apk"));
    }
}

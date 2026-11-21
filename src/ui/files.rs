//! Device file browser with transfers.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use egui::{RichText, Sense, TextureHandle, Ui};
use egui_extras::{Column, TableBuilder};
use tokio::sync::mpsc;

use adbdeck::adb::files::{self, Direction, FileEntry, TransferEvent};
use adbdeck::adb::DeviceAdb;
use adbdeck::error::Error;
use adbdeck::jobs::{Job, Slot};

use adbdeck::text::contains_ci;

use super::widgets::{self, human_size, Dialog};
use super::{kit, theme, DeviceCtx, Env};

const HOME_DIR: &str = "/sdcard";
const MAX_PREVIEW_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TRANSFERS: usize = 40;

#[derive(Debug, Clone, PartialEq)]
enum TransferState {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

struct Transfer {
    direction: Direction,
    /// Device path for downloads, local path for uploads.
    source: String,
    dest: String,
    runas: Option<String>,
    state: TransferState,
    percent: u8,
    current: String,
    job: Option<Job<()>>,
    events: Option<mpsc::UnboundedReceiver<TransferEvent>>,
}

impl Transfer {
    fn title(&self) -> String {
        let name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
        match self.direction {
            Direction::Download => format!("Download {}", name(&self.source)),
            Direction::Upload => format!("Upload {}", name(&self.source)),
        }
    }
}

enum Confirm {
    Delete(Vec<FileEntry>),
    /// Downloads whose local target already exists.
    Overwrite(Vec<(FileEntry, PathBuf)>),
}

struct Listing {
    /// Folder these entries belong to; `Files::path` moves on before they arrive.
    path: String,
    entries: Vec<FileEntry>,
    rooted: bool,
}

#[derive(Default)]
pub struct Files {
    started: bool,
    path: String,
    path_edit: String,
    listing: Option<Listing>,
    load: Slot<Listing>,
    error: Option<String>,
    selected: BTreeSet<String>,
    filter: String,
    op: Slot<String>,
    dialog: Slot<Vec<PathBuf>>,
    dialog_kind: Option<DialogKind>,
    transfers: VecDeque<Transfer>,
    confirm: Option<Confirm>,
    new_folder: Option<String>,
    rename: Option<(FileEntry, String)>,
    preview_load: Slot<Option<(Vec<u8>, String)>>,
    preview: Option<(String, TextureHandle)>,
    preview_for: Option<String>,
    preview_error: Option<String>,
}

enum DialogKind {
    DownloadTo(Vec<FileEntry>),
    UploadFiles,
    UploadFolder,
}

fn side_frame(ui: &Ui, margin: egui::Margin) -> egui::Frame {
    let p = theme::pal(ui);
    egui::Frame::new()
        .fill(p.card)
        .stroke(egui::Stroke::new(1.0_f32, p.stroke))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS_CARD))
        .inner_margin(egui::Margin::same(12))
        .outer_margin(margin)
}

fn is_image(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".png") || n.ends_with(".jpg") || n.ends_with(".jpeg")
}

impl Files {
    fn go(&mut self, env: &Env, d: &DeviceCtx, target: &str) {
        let path = match files::normalize_path(target) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        let dev = d.dev.clone();
        self.error = None;
        self.path = path.clone();
        self.path_edit = path.clone();
        self.selected.clear();
        self.preview_for = None;
        self.load.set(env.sp.spawn(async move {
            let rooted = dev.is_root().await.unwrap_or(false);
            let mut entries = dev.list_dir(&path, rooted).await?;
            entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            Ok(Listing { path, entries, rooted })
        }));
    }

    fn rooted(&self) -> bool {
        self.listing.as_ref().is_some_and(|l| l.rooted)
    }

    fn selected_entries(&self) -> Vec<FileEntry> {
        self.listing.as_ref().map(|l| l.entries.iter().filter(|e| self.selected.contains(&e.name)).cloned().collect()).unwrap_or_default()
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        if !self.started {
            self.started = true;
            self.go(env, d, HOME_DIR);
        }
        self.poll(ui.ctx(), env, d);
        self.pump_transfers(env, d);

        // Toolbar.
        let loading = self.load.busy();
        let busy = loading || self.op.busy();
        kit::card_margin(ui, 8, |ui| {
            ui.horizontal(|ui| {
                if kit::Btn::new("")
                    .icon(kit::ph::ARROW_UP)
                    .subtle()
                    .enabled(self.path != "/" && !self.path.is_empty())
                    .tip("Parent folder")
                    .show(ui)
                    .clicked()
                {
                    let up = files::parent(&self.path);
                    self.go(env, d, &up);
                }
                let narrow = ui.available_width() < 700.0;
                let right = if narrow { 4.0 * 36.0 + 60.0 } else { 2.0 * 36.0 + 340.0 };
                let r = kit::text_field_mono(ui, &mut self.path_edit, "/sdcard", (ui.available_width() - right).max(160.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let t = self.path_edit.clone();
                    self.go(env, d, &t);
                }
                if kit::Btn::new("").icon(kit::ph::ARROWS_CLOCKWISE).subtle().enabled(!loading).tip("Refresh").show(ui).clicked() {
                    let p = self.path.clone();
                    self.go(env, d, &p);
                }
                if kit::Btn::new("").icon(kit::ph::FOLDER_PLUS).subtle().tip("New folder").show(ui).clicked() {
                    self.new_folder = Some(String::new());
                }
                if kit::Btn::new(if narrow { "" } else { "Upload files" })
                    .tip("Upload files")
                    .icon(kit::ph::UPLOAD_SIMPLE)
                    .primary()
                    .enabled(!self.dialog.busy())
                    .show(ui)
                    .clicked()
                {
                    self.dialog_kind = Some(DialogKind::UploadFiles);
                    self.dialog.set(widgets::dialog(env.sp, Dialog::OpenFiles { title: "Choose files to upload".into(), filter: None }));
                }
                if kit::Btn::new(if narrow { "" } else { "Upload folder" })
                    .tip("Upload folder")
                    .icon(kit::ph::FOLDER_SIMPLE)
                    .enabled(!self.dialog.busy())
                    .show(ui)
                    .clicked()
                {
                    self.dialog_kind = Some(DialogKind::UploadFolder);
                    self.dialog.set(widgets::dialog(env.sp, Dialog::Folder { title: "Choose a folder to upload".into() }));
                }
                if loading || self.op.busy() {
                    ui.add(egui::Spinner::new().size(16.0));
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let compact = ui.available_width() < 660.0;
                let sel = self.selected_entries();
                let none = sel.is_empty();
                kit::search(ui, &mut self.filter, "Filter names", 200.0);
                kit::vsep(ui);
                if kit::Btn::new(if compact { "" } else { "Download" })
                    .tip("Download")
                    .icon(kit::ph::DOWNLOAD_SIMPLE)
                    .enabled(!none && !self.dialog.busy())
                    .show(ui)
                    .clicked()
                {
                    self.dialog_kind = Some(DialogKind::DownloadTo(sel.clone()));
                    self.dialog.set(widgets::dialog(env.sp, Dialog::Folder { title: "Save to folder".into() }));
                }
                if kit::Btn::new(if compact { "" } else { "Rename" })
                    .icon(kit::ph::PENCIL_SIMPLE)
                    .enabled(sel.len() == 1 && !busy)
                    .tip("Rename or move")
                    .show(ui)
                    .clicked()
                {
                    self.rename = Some((sel[0].clone(), files::join(&self.path, &sel[0].name)));
                }
                if kit::Btn::new(if compact { "" } else { "Delete" })
                    .tip("Delete")
                    .icon(kit::ph::TRASH)
                    .danger()
                    .enabled(!none && !busy)
                    .show(ui)
                    .clicked()
                {
                    self.confirm = Some(Confirm::Delete(sel.clone()));
                }
                {
                    kit::vsep(ui);
                    let mut preview = env.data.files_show_preview;
                    if kit::Btn::new("").icon(kit::ph::IMAGE).subtle().selected(preview).tip("Preview images").show(ui).clicked() {
                        preview = !preview;
                        env.data.files_show_preview = preview;
                        *env.data_dirty = true;
                    }
                    let mut transfers = env.data.files_show_transfers;
                    if kit::Btn::new("").icon(kit::ph::ARROWS_DOWN_UP).subtle().selected(transfers).tip("Show transfers").show(ui).clicked()
                    {
                        transfers = !transfers;
                        env.data.files_show_transfers = transfers;
                        *env.data_dirty = true;
                    }
                }
            });
        });
        ui.add_space(10.0);
        if let Some(e) = self.error.clone() {
            if widgets::error_banner(ui, &e, true) {
                let p = if self.path.is_empty() { HOME_DIR.to_string() } else { self.path.clone() };
                self.go(env, d, &p);
            }
            ui.add_space(8.0);
        }

        self.dialogs(ui, env, d);

        let show_transfers = env.data.files_show_transfers && !self.transfers.is_empty();
        if show_transfers {
            let fr = side_frame(ui, egui::Margin { left: 0, right: 0, top: 10, bottom: 0 });
            egui::TopBottomPanel::bottom("transfers")
                .resizable(true)
                .default_height(150.0)
                .frame(fr)
                .show_separator_line(false)
                .show_inside(ui, |ui| self.transfer_list(ui));
        }
        let show_preview = env.data.files_show_preview && ui.available_width() > 700.0;
        if show_preview {
            let fr = side_frame(ui, egui::Margin { left: 10, right: 0, top: 0, bottom: 0 });
            egui::SidePanel::right("file-preview")
                .resizable(true)
                .default_width(280.0)
                .frame(fr)
                .show_separator_line(false)
                .show_inside(ui, |ui| self.preview_ui(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE).show_inside(ui, |ui| self.table(ui, env, d));
    }

    fn table(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        let Some(listing) = self.listing.as_ref() else {
            if self.load.busy() {
                kit::busy(ui, "Loading folder…");
            }
            return;
        };
        let filter = self.filter.as_str();
        let rows: Vec<&FileEntry> = listing.entries.iter().filter(|e| contains_ci(&e.name, filter)).collect();
        if rows.is_empty() && !self.load.busy() {
            let (title, detail) = if filter.is_empty() {
                ("This folder is empty", "Upload files here or drop them onto the window.")
            } else {
                ("No matches", "No file or folder name contains the filter text.")
            };
            kit::empty_state(ui, kit::ph::FOLDER_DASHED, title, detail, |_| {});
            return;
        }
        let p = theme::pal(ui);
        let mut open: Option<String> = None;
        let mut clicked: Option<(String, bool)> = None;
        let selected = &self.selected;
        // While a different folder is loading the old rows stay visible but inert,
        // so a click cannot be applied to a folder the row does not belong to.
        let interactive = listing.path == self.path;
        let avail_h = ui.available_height();
        kit::well(ui, |ui| {
            ui.set_min_height(avail_h - 10.0);
            ui.set_width(ui.available_width());
            TableBuilder::new(ui)
                .striped(true)
                .sense(Sense::click())
                .resizable(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::remainder().at_least(160.0).clip(true))
                .column(Column::exact(90.0))
                .column(Column::exact(140.0))
                .column(Column::exact(100.0))
                .header(30.0, |mut h| {
                    for t in ["Name", "Size", "Modified", "Mode"] {
                        h.col(|ui| {
                            ui.label(kit::semibold(t).size(12.0).color(p.text2));
                        });
                    }
                })
                .body(|body| {
                    body.rows(30.0, rows.len(), |mut row| {
                        let e = rows[row.index()];
                        row.set_selected(selected.contains(&e.name));
                        row.col(|ui| {
                            let (icon, color) = kit::file_icon(&e.name, e.is_dir, p);
                            ui.label(RichText::new(icon).size(17.0).color(color));
                            let label = if e.is_link { format!("{}  →", e.name) } else { e.name.clone() };
                            let text = if e.is_dir { kit::medium(label) } else { RichText::new(label) };
                            ui.add(egui::Label::new(text).selectable(false).truncate());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(e.size.map(human_size).unwrap_or_default()).color(p.text2));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&e.modified).color(p.text2));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&e.mode).monospace().size(11.5).color(p.text3));
                        });
                        let resp = row.response();
                        if !interactive {
                            return;
                        }
                        if resp.double_clicked() {
                            open = Some(e.name.clone());
                        } else if resp.clicked() {
                            clicked = Some((e.name.clone(), resp.ctx.input(|i| i.modifiers.command)));
                        }
                    });
                });
        });

        if let Some((name, multi)) = clicked {
            if multi {
                if !self.selected.remove(&name) {
                    self.selected.insert(name);
                }
            } else {
                self.selected.clear();
                self.selected.insert(name);
            }
            self.request_preview(env, d);
        }
        if let Some(name) = open {
            let entry = self.listing.as_ref().and_then(|l| l.entries.iter().find(|e| e.name == name)).cloned();
            if let Some(entry) = entry {
                if entry.is_dir || entry.is_link {
                    let target = files::join(&self.path, &entry.name);
                    self.go(env, d, &target);
                }
            }
        }
    }

    fn request_preview(&mut self, env: &Env, d: &DeviceCtx) {
        self.preview_error = None;
        if !env.data.files_show_preview || self.selected.len() != 1 {
            return;
        }
        let Some(entry) = self.selected_entries().pop() else { return };
        if entry.is_dir || !is_image(&entry.name) {
            self.preview = None;
            self.preview_for = None;
            return;
        }
        if entry.size.is_some_and(|s| s > MAX_PREVIEW_BYTES) {
            self.preview = None;
            self.preview_error = Some("Too large to preview (over 8 MB).".into());
            return;
        }
        let remote = files::join(&self.path, &entry.name);
        if self.preview_for.as_deref() == Some(remote.as_str()) {
            return;
        }
        self.preview_for = Some(remote.clone());
        let (dev, rooted) = (d.dev.clone(), self.rooted());
        self.preview_load.set(env.sp.spawn(async move { fetch_preview(dev, remote, rooted).await.map(Some) }));
    }

    fn preview_ui(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        ui.label(kit::semibold("Preview").size(13.5));
        ui.add_space(4.0);
        if self.preview_load.busy() {
            kit::busy(ui, "Loading…");
        }
        if let Some(e) = &self.preview_error {
            ui.label(RichText::new(e).color(p.warning));
        }
        match &self.preview {
            Some((name, tex)) => {
                ui.add(egui::Label::new(RichText::new(name).size(12.0).color(p.text2)).truncate());
                ui.add_space(6.0);
                egui::ScrollArea::both().show(ui, |ui| {
                    let size = tex.size_vec2();
                    let scale = (ui.available_width() / size.x).min(1.0);
                    ui.add(egui::Image::new((tex.id(), size * scale)).corner_radius(egui::CornerRadius::same(6)));
                });
            }
            None if !self.preview_load.busy() && self.preview_error.is_none() => {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(kit::ph::IMAGE).size(28.0).color(p.text3));
                    ui.add_space(4.0);
                    ui.label(RichText::new("Select a .png or .jpg file").size(12.5).color(p.text3));
                });
            }
            None => {}
        }
    }

    fn transfer_list(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(kit::semibold("Transfers").size(13.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::Btn::new("Clear finished").subtle().show(ui).clicked() {
                    self.transfers.retain(|t| matches!(t.state, TransferState::Queued | TransferState::Running));
                }
            });
        });
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut cancel: Option<usize> = None;
            for (i, t) in self.transfers.iter().enumerate() {
                ui.horizontal(|ui| {
                    let icon = if t.direction == Direction::Upload { kit::ph::UPLOAD_SIMPLE } else { kit::ph::DOWNLOAD_SIMPLE };
                    ui.label(RichText::new(icon).size(16.0).color(p.accent_text));
                    ui.add_sized([220.0, 20.0], egui::Label::new(t.title()).truncate());
                    match &t.state {
                        TransferState::Queued => {
                            kit::pill(ui, "Queued", kit::Kind::Neutral);
                        }
                        TransferState::Running => {
                            kit::progress(ui, f32::from(t.percent) / 100.0, 160.0);
                            ui.label(RichText::new(format!("{}%", t.percent)).monospace().color(p.text2));
                        }
                        TransferState::Done => {
                            kit::pill(ui, "Done", kit::Kind::Success);
                        }
                        TransferState::Failed(m) => {
                            kit::pill(ui, "Failed", kit::Kind::Danger).on_hover_text(m);
                            ui.add(egui::Label::new(RichText::new(m).size(12.0).color(p.danger)).truncate());
                        }
                        TransferState::Cancelled => {
                            kit::pill(ui, "Cancelled", kit::Kind::Neutral);
                        }
                    }
                    if matches!(t.state, TransferState::Queued | TransferState::Running)
                        && kit::icon_btn(ui, kit::ph::X, "Cancel").clicked()
                    {
                        cancel = Some(i);
                    }
                });
            }
            if let Some(i) = cancel {
                if let Some(t) = self.transfers.get_mut(i) {
                    t.job = None;
                    t.events = None;
                    t.state = TransferState::Cancelled;
                }
            }
        });
    }

    fn dialogs(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        let ctx = ui.ctx().clone();
        let mut do_new_folder = None;
        if let Some(name) = self.new_folder.as_mut() {
            let mut keep = true;
            let resp = kit::dialog(&ctx, "new-folder", kit::ph::FOLDER_PLUS, "New folder", kit::Kind::Accent, 360.0, |ui| {
                let r = kit::text_field(ui, name, "Folder name", 360.0);
                if !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
                    r.request_focus();
                }
                let valid = files::validate_name(name).is_ok();
                let submit = valid && r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                kit::dialog_buttons(ui, |ui| {
                    if kit::Btn::new("Create").primary().enabled(valid).show(ui).clicked() || submit {
                        do_new_folder = Some(name.clone());
                        keep = false;
                    }
                    if kit::btn(ui, "Cancel").clicked() {
                        keep = false;
                    }
                });
            });
            if !keep || resp.should_close() {
                self.new_folder = None;
            }
        }
        if let Some(name) = do_new_folder {
            let (dev, path, rooted) = (d.dev.clone(), files::join(&self.path, &name), self.rooted());
            self.op.set(env.sp.spawn(async move {
                dev.make_dir(&path, rooted).await?;
                Ok(format!("Created {path}"))
            }));
        }

        let mut do_rename = None;
        if let Some((entry, target)) = self.rename.as_mut() {
            let mut keep = true;
            let resp = kit::dialog(&ctx, "rename", kit::ph::PENCIL_SIMPLE, "Rename or move", kit::Kind::Accent, 440.0, |ui| {
                ui.label(RichText::new(format!("New full path for {}", entry.name)).color(theme::pal(ui).text2));
                ui.add_space(4.0);
                kit::text_field_mono(ui, target, "/sdcard/…", 440.0);
                let valid = files::normalize_path(target).is_ok();
                kit::dialog_buttons(ui, |ui| {
                    if kit::Btn::new("Move").primary().enabled(valid).show(ui).clicked() {
                        do_rename = Some((entry.name.clone(), target.clone()));
                        keep = false;
                    }
                    if kit::btn(ui, "Cancel").clicked() {
                        keep = false;
                    }
                });
            });
            if !keep || resp.should_close() {
                self.rename = None;
            }
        }
        if let Some((name, target)) = do_rename {
            let (dev, from, rooted) = (d.dev.clone(), files::join(&self.path, &name), self.rooted());
            self.op.set(env.sp.spawn(async move {
                dev.move_path(&from, &target, rooted).await?;
                Ok(format!("Moved {from} to {target}"))
            }));
        }

        let mut resolution: Option<bool> = None;
        if let Some(c) = &self.confirm {
            let text = match c {
                Confirm::Delete(list) if list.len() == 1 => format!("Delete {} from the device? This cannot be undone.", list[0].name),
                Confirm::Delete(list) => format!("Delete {} items from the device? This cannot be undone.", list.len()),
                Confirm::Overwrite(list) if list.len() == 1 => format!("{} already exists. Replace it?", list[0].1.display()),
                Confirm::Overwrite(list) => format!("{} files already exist in that folder. Replace them?", list.len()),
            };
            let destructive = matches!(c, Confirm::Delete(_));
            let (icon, title, kind) = if destructive {
                (kit::ph::TRASH, "Delete from device", kit::Kind::Danger)
            } else {
                (kit::ph::WARNING, "Replace existing file", kit::Kind::Warning)
            };
            let resp = kit::dialog(&ctx, "confirm", icon, title, kind, 380.0, |ui| {
                ui.add(egui::Label::new(RichText::new(text).color(theme::pal(ui).text2)).wrap());
                kit::dialog_buttons(ui, |ui| {
                    let b = if destructive { kit::Btn::new("Delete").danger() } else { kit::Btn::new("Replace").primary() };
                    if b.show(ui).clicked() {
                        resolution = Some(true);
                    }
                    if kit::btn(ui, "Cancel").clicked() {
                        resolution = Some(false);
                    }
                });
            });
            if resp.should_close() && resolution.is_none() {
                resolution = Some(false);
            }
        }
        if let Some(yes) = resolution {
            if let (true, Some(c)) = (yes, self.confirm.take()) {
                match c {
                    Confirm::Delete(list) => {
                        let (dev, base, rooted) = (d.dev.clone(), self.path.clone(), self.rooted());
                        self.op.set(env.sp.spawn(async move {
                            for e in &list {
                                dev.delete_path(&files::join(&base, &e.name), e.is_dir, rooted).await?;
                            }
                            Ok(format!("Deleted {} item(s)", list.len()))
                        }));
                    }
                    Confirm::Overwrite(list) => {
                        for (entry, dest) in list {
                            self.queue_download(&entry, &dest);
                        }
                    }
                }
            } else {
                self.confirm = None;
            }
        }
    }

    fn queue_download(&mut self, entry: &FileEntry, dest: &Path) {
        let source = files::join(&self.path, &entry.name);
        let runas = DeviceAdb::runas_package(&source, self.rooted());
        self.push_transfer(Transfer {
            direction: Direction::Download,
            source,
            dest: dest.to_string_lossy().into_owned(),
            runas,
            state: TransferState::Queued,
            percent: 0,
            current: String::new(),
            job: None,
            events: None,
        });
    }

    fn push_transfer(&mut self, t: Transfer) {
        self.transfers.push_back(t);
        while self.transfers.len() > MAX_TRANSFERS {
            if let Some(i) = self.transfers.iter().position(|t| !matches!(t.state, TransferState::Queued | TransferState::Running)) {
                self.transfers.remove(i);
            } else {
                break;
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context, env: &mut Env, d: &DeviceCtx) {
        if let Some(r) = self.load.poll() {
            match r {
                Ok(l) => {
                    self.listing = Some(l);
                    self.error = None;
                }
                Err(e) => {
                    if e.is_device_gone() {
                        env.notes.error("Files", &e);
                    }
                    self.error = Some(e.to_string());
                    self.listing = None;
                }
            }
        }
        if let Some(r) = self.op.poll() {
            if let Some(msg) = env.notes.report("File operation", r) {
                env.notes.info(msg);
            }
            let p = self.path.clone();
            self.go(env, d, &p);
        }
        if let Some(r) = self.dialog.poll() {
            let kind = self.dialog_kind.take();
            if let (Some(paths), Some(kind)) = (env.notes.report("File dialog", r), kind) {
                self.dialog_done(paths, kind, env);
            }
        }
        if let Some(r) = self.preview_load.poll() {
            match r {
                Ok(Some((bytes, name))) => match decode_image(&bytes) {
                    Ok(img) => {
                        let tex = ctx.load_texture("file-preview", img, egui::TextureOptions::LINEAR);
                        self.preview = Some((name, tex));
                        self.preview_error = None;
                    }
                    Err(e) => {
                        self.preview = None;
                        self.preview_error = Some(e);
                    }
                },
                Ok(None) => {}
                Err(e) => {
                    self.preview = None;
                    self.preview_error = Some(e.to_string());
                }
            }
        }
    }

    fn dialog_done(&mut self, paths: Vec<PathBuf>, kind: DialogKind, env: &mut Env) {
        match kind {
            DialogKind::DownloadTo(entries) => {
                let Some(dir) = paths.into_iter().next() else { return };
                let mut existing = Vec::new();
                for entry in entries {
                    let dest = dir.join(&entry.name);
                    if dest.exists() {
                        existing.push((entry, dest));
                    } else {
                        self.queue_download(&entry, &dest);
                    }
                }
                if !existing.is_empty() {
                    self.confirm = Some(Confirm::Overwrite(existing));
                }
            }
            DialogKind::UploadFiles | DialogKind::UploadFolder => {
                for p in paths {
                    let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
                    let dest = files::join(&self.path, &name);
                    let runas = DeviceAdb::runas_package(&dest, self.rooted());
                    if runas.is_some() && p.is_dir() {
                        env.notes.error(
                            "Upload",
                            &Error::Invalid("Folders cannot be uploaded into an app's private storage without root.".into()),
                        );
                        continue;
                    }
                    self.push_transfer(Transfer {
                        direction: Direction::Upload,
                        source: p.to_string_lossy().into_owned(),
                        dest,
                        runas,
                        state: TransferState::Queued,
                        percent: 0,
                        current: String::new(),
                        job: None,
                        events: None,
                    });
                }
            }
        }
    }

    /// Runs queued transfers one at a time and drains progress events.
    fn pump_transfers(&mut self, env: &mut Env, d: &DeviceCtx) {
        let mut refresh = false;
        for t in self.transfers.iter_mut() {
            if let Some(rx) = t.events.as_mut() {
                while let Ok(TransferEvent::Progress { percent, current }) = rx.try_recv() {
                    t.percent = percent;
                    t.current = current;
                }
            }
            if t.state == TransferState::Running {
                if let Some(r) = t.job.as_mut().and_then(|j| j.poll()) {
                    t.job = None;
                    t.events = None;
                    match r {
                        Ok(()) => {
                            t.state = TransferState::Done;
                            t.percent = 100;
                            refresh = t.direction == Direction::Upload;
                        }
                        Err(Error::Cancelled) => t.state = TransferState::Cancelled,
                        Err(e) => {
                            env.notes.error(&t.title(), &e);
                            t.state = TransferState::Failed(e.to_string());
                        }
                    }
                }
            }
        }
        if !self.transfers.iter().any(|t| t.state == TransferState::Running) {
            if let Some(t) = self.transfers.iter_mut().find(|t| t.state == TransferState::Queued) {
                let dev = d.dev.clone();
                let (direction, source, dest, runas) = (t.direction, t.source.clone(), t.dest.clone(), t.runas.clone());
                let (tx, rx) = mpsc::unbounded_channel();
                t.events = Some(rx);
                t.state = TransferState::Running;
                t.job = Some(env.sp.spawn(async move {
                    match (runas, direction) {
                        (Some(pkg), Direction::Download) => dev.pull_runas_file(&pkg, &source, Path::new(&dest)).await,
                        (Some(pkg), Direction::Upload) => dev.push_runas_file(&pkg, Path::new(&source), &dest).await,
                        (None, direction) => dev.transfer(direction, &source, &dest, tx).await,
                    }
                }));
            }
        }
        if refresh && !self.load.busy() {
            let p = self.path.clone();
            self.go(env, d, &p);
        }
    }
}

async fn fetch_preview(dev: DeviceAdb, remote: String, rooted: bool) -> adbdeck::error::Result<(Vec<u8>, String)> {
    let dir = adbdeck::paths::preview_cache_dir().ok_or_else(|| Error::Invalid("no cache directory is available".into()))?;
    tokio::fs::create_dir_all(&dir).await.map_err(|e| Error::io("creating the preview cache", e))?;
    let local = dir.join(format!("preview-{}", std::process::id()));
    let result = async {
        match DeviceAdb::runas_package(&remote, rooted) {
            Some(pkg) => dev.pull_runas_file(&pkg, &remote, &local).await?,
            None => dev.pull_to(&remote, &local).await?,
        }
        tokio::fs::read(&local).await.map_err(|e| Error::io("reading the preview", e))
    }
    .await;
    // The cached copy is only needed to decode it.
    let _ = tokio::fs::remove_file(&local).await;
    Ok((result?, remote.rsplit('/').next().unwrap_or("").to_string()))
}

fn decode_image(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("Cannot show this image: {e}"))?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_png_and_jpeg_are_previewed() {
        assert!(is_image("a.PNG") && is_image("b.jpeg") && is_image("c.jpg"));
        assert!(!is_image("d.gif") && !is_image("png"));
    }

    #[test]
    fn broken_image_data_is_an_error_not_a_panic() {
        assert!(decode_image(b"not an image").is_err());
    }
}

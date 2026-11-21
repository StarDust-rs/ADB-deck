//! Live log viewer with filters.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use egui::{Color32, RichText, Ui};

use adbdeck::adb::logcat::{parse_threadtime, Level, LogEntry, LogFilter};
use adbdeck::error::Error;
use adbdeck::jobs::Slot;
use adbdeck::process::LineStream;

use super::widgets::{self, Dialog};
use super::{kit, theme, DeviceCtx, Env};

/// Entries kept in memory; older ones are dropped.
const MAX_ENTRIES: usize = 20_000;
/// Lines taken from the stream per frame, so a flood cannot stall the UI.
const PER_FRAME: usize = 1500;

#[derive(Default)]
pub struct Logcat {
    start: Slot<LineStream>,
    stream: Option<LineStream>,
    entries: VecDeque<LogEntry>,
    unparsed: usize,
    names: HashMap<u32, String>,
    names_job: Slot<HashMap<u32, String>>,
    filter: LogFilter,
    paused: bool,
    follow: Option<bool>,
    save: Slot<Vec<PathBuf>>,
    write: Slot<String>,
    ended: Option<String>,
    dropped: u64,
    names_at: Option<std::time::Instant>,
}

/// Log colours; the viewer is dark in both themes.
fn color(level: Level) -> Color32 {
    match level {
        Level::Verbose => Color32::from_rgb(140, 144, 154),
        Level::Debug => Color32::from_rgb(120, 190, 255),
        Level::Info => Color32::from_rgb(130, 220, 150),
        Level::Warn => Color32::from_rgb(245, 200, 90),
        Level::Error | Level::Fatal => Color32::from_rgb(255, 125, 120),
    }
}

impl Logcat {
    fn start(&mut self, env: &Env, d: &DeviceCtx) {
        let dev = d.dev.clone();
        self.ended = None;
        self.start.set(env.sp.spawn(async move { dev.start_logcat().await }));
        self.refresh_names(env, d);
    }

    fn refresh_names(&mut self, env: &Env, d: &DeviceCtx) {
        if !self.names_job.busy() {
            let dev = d.dev.clone();
            self.names_job.set(env.sp.spawn(async move { dev.pid_names().await }));
        }
    }

    fn matches(&self, e: &LogEntry) -> bool {
        self.filter.matches(e, self.names.get(&e.pid).map(String::as_str))
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        if self.stream.is_none() && !self.start.busy() && self.ended.is_none() {
            self.start(env, d);
        }
        if let Some(r) = self.start.poll() {
            match r {
                Ok(s) => self.stream = Some(s),
                Err(e) => self.ended = Some(e.to_string()),
            }
        }
        if let Some(Ok(n)) = self.names_job.poll() {
            self.names = n;
        }
        if let Some(r) = self.write.poll() {
            if let Some(m) = env.notes.report("Save log", r) {
                env.notes.info(m);
            }
        }
        // Package filtering needs fresh pid -> name data; apps restart often.
        if !self.filter.package.is_empty() && self.names_at.is_none_or(|t| t.elapsed() > Duration::from_secs(8)) {
            self.names_at = Some(std::time::Instant::now());
            self.refresh_names(env, d);
        }
        self.drain();
        if self.stream.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        let follow = *self.follow.get_or_insert(true);
        kit::card_margin(ui, 8, |ui| {
            ui.horizontal_wrapped(|ui| {
                let levels: Vec<(Level, &str)> = Level::FILTERABLE.iter().map(|l| (*l, l.name())).collect();
                kit::segmented(ui, &mut self.filter.min_level, &levels);
                if kit::Btn::new("")
                    .icon(kit::ph::BOOKMARK_SIMPLE)
                    .subtle()
                    .tip("Remember the current package, tag and keyword in the dropdown lists")
                    .show(ui)
                    .clicked()
                {
                    for (field, v) in [("package", &self.filter.package), ("tag", &self.filter.tag), ("keyword", &self.filter.keyword)] {
                        env.data.push_history(field, v);
                    }
                    *env.data_dirty = true;
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let hist = |field: &str, data: &adbdeck::store::AppData| data.logcat_history.get(field).cloned().unwrap_or_default();
                let (ph, th, kh) = (hist("package", env.data), hist("tag", env.data), hist("keyword", env.data));
                let w = ((ui.available_width() - 3.0 * ui.spacing().item_spacing.x) / 3.0).clamp(90.0, 300.0);
                widgets::history_edit(ui, "h-pkg", &mut self.filter.package, "Package", &ph, w);
                widgets::history_edit(ui, "h-tag", &mut self.filter.tag, "Tag", &th, w);
                widgets::history_edit(ui, "h-kw", &mut self.filter.keyword, "Keyword", &kh, w);
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let (icon, label) = if self.paused { (kit::ph::PLAY, "Resume") } else { (kit::ph::PAUSE, "Pause") };
                if kit::Btn::new(label).icon(icon).show(ui).clicked() {
                    self.paused = !self.paused;
                }
                if kit::Btn::new("Clear").icon(kit::ph::ERASER).show(ui).clicked() {
                    self.entries.clear();
                    self.unparsed = 0;
                    // The buffer on the device is cleared when the stream restarts.
                    self.stream = None;
                    self.start(env, d);
                }
                let mut f = follow;
                if kit::toggle(ui, &mut f, "Follow").changed() {
                    self.follow = Some(f);
                }
                kit::vsep(ui);
                if kit::Btn::new("Save")
                    .icon(kit::ph::FLOPPY_DISK)
                    .enabled(!self.save.busy() && !self.entries.is_empty())
                    .tip("Save the visible entries")
                    .show(ui)
                    .clicked()
                {
                    self.save.set(widgets::dialog(env.sp, Dialog::Save { title: "Save log".into(), name: "logcat.txt".into() }));
                }
                if kit::Btn::new("Copy")
                    .icon(kit::ph::COPY)
                    .enabled(!self.entries.is_empty())
                    .tip("Copy the latest visible lines")
                    .show(ui)
                    .clicked()
                {
                    let text = self.visible_text(5000);
                    ui.ctx().copy_text(text);
                    env.notes.info("Copied the latest visible lines");
                }
                ui.label(
                    RichText::new(format!(
                        "{} entries{}",
                        self.entries.len(),
                        if self.dropped > 0 { format!(" ({} older dropped)", self.dropped) } else { String::new() }
                    ))
                    .size(12.0)
                    .color(theme::pal(ui).text3),
                );
            });
        });
        ui.add_space(10.0);
        if let Some(r) = self.save.poll() {
            if let Some(path) = env.notes.report("File dialog", r).and_then(|p| p.into_iter().next()) {
                let text = self.visible_text(usize::MAX);
                self.write.set(env.sp.spawn(async move {
                    tokio::fs::write(&path, text).await.map_err(|e| Error::io(format!("writing {}", path.display()), e))?;
                    Ok(format!("Saved {}", path.display()))
                }));
            }
        }
        if let Some(msg) = self.ended.clone() {
            if kit::banner(ui, kit::Kind::Danger, &format!("Log stream stopped: {msg}"), Some("Restart")) {
                self.ended = None;
            }
            ui.add_space(8.0);
        }

        let pal = theme::pal(ui);
        let rows: Vec<&LogEntry> = self.entries.iter().filter(|e| self.matches(e)).collect();
        if rows.is_empty() {
            if self.start.busy() {
                kit::busy(ui, "Starting logcat…");
            } else {
                kit::empty_state(
                    ui,
                    kit::ph::LIST_MAGNIFYING_GLASS,
                    "No log entries yet",
                    "Entries appear here as the device writes them. Adjust the filters if you expect more.",
                    |_| {},
                );
            }
            return;
        }
        let font = egui::FontId::monospace(12.5);
        let height = ui.fonts(|f| f.row_height(&font)) + 3.0;
        let avail = ui.available_height();
        egui::Frame::new()
            .fill(pal.terminal_bg)
            .stroke(egui::Stroke::new(1.0_f32, pal.stroke))
            .corner_radius(egui::CornerRadius::same(theme::RADIUS_CARD))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_min_height(avail - 18.0);
                ui.set_width(ui.available_width());
                egui::ScrollArea::both().auto_shrink([false, false]).stick_to_bottom(follow && !self.paused).show_rows(
                    ui,
                    height,
                    rows.len(),
                    |ui, range| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        for e in &rows[range] {
                            let name = self.names.get(&e.pid).map(String::as_str).unwrap_or("");
                            let dim = Color32::from_rgb(110, 114, 124);
                            let lvl = color(e.level);
                            let mut job = egui::text::LayoutJob::default();
                            let fmt = |c: Color32| egui::text::TextFormat { font_id: font.clone(), color: c, ..Default::default() };
                            job.append(&format!("{} ", e.time), 0.0, fmt(dim));
                            job.append(&format!("{:>5} ", e.pid), 0.0, fmt(dim));
                            job.append(
                                &format!("{} ", e.level.letter()),
                                0.0,
                                egui::text::TextFormat { background: lvl.gamma_multiply(0.22), ..fmt(lvl) },
                            );
                            job.append(&format!(" {:<22} ", e.tag), 0.0, fmt(Color32::from_rgb(150, 170, 215)));
                            if !name.is_empty() {
                                job.append(&format!("[{name}] "), 0.0, fmt(dim));
                            }
                            job.append(&e.message, 0.0, fmt(if e.level >= Level::Warn { lvl } else { Color32::from_rgb(214, 217, 224) }));
                            ui.add(egui::Label::new(job).extend());
                        }
                    },
                );
            });
    }

    fn drain(&mut self) {
        if self.paused {
            return;
        }
        let mut finished = false;
        if let Some(s) = self.stream.as_mut() {
            for _ in 0..PER_FRAME {
                match s.lines.try_recv() {
                    Ok(line) => match parse_threadtime(&line) {
                        Some(e) => {
                            if self.entries.len() == MAX_ENTRIES {
                                self.entries.pop_front();
                                self.dropped += 1;
                            }
                            self.entries.push_back(e);
                        }
                        None => self.unparsed += 1,
                    },
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
        }
        if finished {
            self.stream = None;
            self.ended = Some("adb stopped sending output (device disconnected?)".into());
        }
    }

    fn visible_text(&self, last: usize) -> String {
        let all: Vec<&LogEntry> = self.entries.iter().filter(|e| self.matches(e)).collect();
        let skip = all.len().saturating_sub(last);
        all[skip..].iter().map(|e| e.to_line()).collect::<Vec<_>>().join("\n")
    }
}

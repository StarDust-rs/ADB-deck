//! Small shared widgets and helpers for the panels.

use std::path::PathBuf;
use std::time::Instant;

use egui::Ui;

use super::kit;

use adbdeck::error::Error;
use adbdeck::jobs::{Job, Spawner};

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// An error line with an optional retry button; true when retry was clicked.
pub fn error_banner(ui: &mut Ui, message: &str, retry: bool) -> bool {
    kit::banner(ui, kit::Kind::Danger, message, retry.then_some("Retry"))
}

// ---- notifications ------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    Info,
    Error,
}

pub struct Note {
    pub kind: NoteKind,
    pub text: String,
    pub at: Instant,
    pub dismissed: bool,
}

/// Messages shown in the status bar. Bounded: old ones are dropped.
#[derive(Default)]
pub struct Notes {
    pub items: Vec<Note>,
}

const NOTE_LIMIT: usize = 20;

impl Notes {
    pub fn push(&mut self, kind: NoteKind, text: String) {
        tracing::debug!(?kind, %text, "note");
        self.items.push(Note { kind, text, at: Instant::now(), dismissed: false });
        if self.items.len() > NOTE_LIMIT {
            self.items.remove(0);
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(NoteKind::Info, text.into());
    }

    pub fn error(&mut self, context: &str, err: &Error) {
        tracing::warn!(%context, error = %err, "operation failed");
        self.push(NoteKind::Error, format!("{context}: {err}"));
    }

    /// Reports an error result and returns the value of an OK one.
    pub fn report<T>(&mut self, context: &str, result: adbdeck::error::Result<T>) -> Option<T> {
        match result {
            Ok(v) => Some(v),
            Err(Error::Cancelled) => None,
            Err(e) => {
                self.error(context, &e);
                None
            }
        }
    }
}

// ---- file dialogs -------------------------------------------------------

pub enum Dialog {
    OpenFiles { title: String, filter: Option<(String, Vec<String>)> },
    OpenFile { title: String, filter: Option<(String, Vec<String>)> },
    Save { title: String, name: String },
    Folder { title: String },
}

/// Opens a native (portal) dialog without blocking the UI thread.
pub fn dialog(sp: &Spawner, kind: Dialog) -> Job<Vec<PathBuf>> {
    sp.spawn(async move {
        let paths = match kind {
            Dialog::OpenFiles { title, filter } => {
                let mut d = rfd::AsyncFileDialog::new().set_title(title);
                if let Some((name, exts)) = filter {
                    d = d.add_filter(name, &exts);
                }
                d.pick_files().await.unwrap_or_default().into_iter().map(|f| f.path().to_path_buf()).collect()
            }
            Dialog::OpenFile { title, filter } => {
                let mut d = rfd::AsyncFileDialog::new().set_title(title);
                if let Some((name, exts)) = filter {
                    d = d.add_filter(name, &exts);
                }
                d.pick_file().await.into_iter().map(|f| f.path().to_path_buf()).collect()
            }
            Dialog::Save { title, name } => rfd::AsyncFileDialog::new()
                .set_title(title)
                .set_file_name(name)
                .save_file()
                .await
                .into_iter()
                .map(|f| f.path().to_path_buf())
                .collect(),
            Dialog::Folder { title } => {
                rfd::AsyncFileDialog::new().set_title(title).pick_folder().await.into_iter().map(|f| f.path().to_path_buf()).collect()
            }
        };
        Ok(paths)
    })
}

/// A text field with a recent-values dropdown.
pub fn history_edit(ui: &mut Ui, id: &str, text: &mut String, hint: &str, history: &[String], width: f32) {
    ui.horizontal(|ui| {
        let w = if history.is_empty() { width } else { width - 36.0 };
        kit::text_field(ui, text, hint, w);
        if !history.is_empty() {
            ui.push_id(id, |ui| {
                ui.menu_button(kit::ph::CLOCK_COUNTER_CLOCKWISE, |ui| {
                    for h in history {
                        if ui.selectable_label(false, h).clicked() {
                            *text = h.clone();
                            ui.close_menu();
                        }
                    }
                })
                .response
                .on_hover_text("Recent values");
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn latest(n: &Notes, max_age: Duration) -> Option<&Note> {
        n.items.last().filter(|x| x.at.elapsed() < max_age)
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn notes_are_bounded_and_cancel_is_silent() {
        let mut n = Notes::default();
        for i in 0..50 {
            n.info(format!("{i}"));
        }
        assert_eq!(n.items.len(), NOTE_LIMIT);
        let before = n.items.len();
        assert!(n.report::<()>("x", Err(Error::Cancelled)).is_none());
        assert_eq!(n.items.len(), before);
        assert!(n.report::<()>("x", Err(Error::Invalid("bad".into()))).is_none());
        assert!(latest(&n, Duration::from_secs(5)).unwrap().text.contains("bad"));
    }
}

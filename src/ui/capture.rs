//! Packet capture with tcpdump on the device.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{RichText, Ui};

use adbdeck::adb::tcpdump::{count_pcap_packets, Capture as DeviceCapture};
use adbdeck::error::Error;
use adbdeck::jobs::Slot;

use super::widgets::{self, human_size, Dialog};
use super::{kit, theme, DeviceCtx, Env};

enum State {
    Idle,
    Running { capture: DeviceCapture, since: Instant, size: Option<u64> },
    Stopped { remote: String, size: Option<u64> },
}

pub struct Capture {
    state: State,
    filter: String,
    counter: u64,
    start: Slot<DeviceCapture>,
    stop: Slot<(String, Option<u64>)>,
    size_job: Slot<Option<u64>>,
    size_at: Option<Instant>,
    save_dialog: Slot<Vec<PathBuf>>,
    save: Slot<String>,
    discard: Slot<()>,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            state: State::Idle,
            filter: String::new(),
            counter: std::process::id() as u64,
            start: Slot::default(),
            stop: Slot::default(),
            size_job: Slot::default(),
            size_at: None,
            save_dialog: Slot::default(),
            save: Slot::default(),
            discard: Slot::default(),
        }
    }
}

impl Capture {
    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        if let Some(r) = self.start.poll() {
            if let Some(c) = env.notes.report("Start capture", r) {
                self.state = State::Running { capture: c, since: Instant::now(), size: None };
            }
        }
        if let Some(r) = self.stop.poll() {
            match env.notes.report("Stop capture", r) {
                Some((remote, size)) => self.state = State::Stopped { remote, size },
                None => self.state = State::Idle,
            }
        }
        if let Some(r) = self.size_job.poll() {
            if let (Ok(s), State::Running { size, .. }) = (r, &mut self.state) {
                *size = s;
            }
        }
        if let Some(r) = self.save.poll() {
            // A failed save keeps the capture on the device and in the Stopped state,
            // so the user can pick another location and try again.
            if let Some(m) = env.notes.report("Save capture", r) {
                env.notes.info(m);
                self.state = State::Idle;
            }
        }
        if let Some(r) = self.discard.poll() {
            env.notes.report("Delete capture", r);
        }

        // The device-side tcpdump may stop by itself (for example when the
        // device disconnects); notice that through the closed stream.
        let mut ended = false;
        if let State::Running { capture, since, .. } = &mut self.state {
            while capture.output.lines.try_recv().is_ok() {}
            ended = capture.output.is_finished();
            ui.ctx().request_repaint_after(Duration::from_secs(1));
            let _ = since;
        }
        if ended {
            env.notes.error("Capture", &Error::Invalid("tcpdump stopped unexpectedly.".into()));
            self.state = State::Idle;
        }
        if let State::Running { capture, .. } = &self.state {
            if !self.size_job.busy() && self.size_at.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
                self.size_at = Some(Instant::now());
                let (dev, path) = (d.dev.clone(), capture.remote_path.clone());
                self.size_job.set(env.sp.spawn(async move { Ok(dev.remote_size(&path).await) }));
            }
        }

        let p = theme::pal(ui);
        kit::banner(
            ui,
            kit::Kind::Accent,
            "Captures all device traffic with tcpdump. This needs tcpdump on the device and usually root (see Overview).",
            None,
        );
        ui.add_space(12.0);
        let starting = self.start.busy();
        match &self.state {
            State::Idle => {
                kit::card(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(kit::semibold("Ready to capture").size(14.5));
                        ui.label(RichText::new("Optionally restrict the capture with a tcpdump filter.").color(p.text2));
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        kit::text_field_mono(ui, &mut self.filter, "Optional filter, e.g. port 443", 300.0);
                        if kit::Btn::new("Start capture").icon(kit::ph::RECORD).primary().enabled(!starting).show(ui).clicked() {
                            self.counter += 1;
                            let (dev, filter, id) = (d.dev.clone(), self.filter.clone(), self.counter);
                            self.start.set(env.sp.spawn(async move { dev.start_capture(id, &filter).await }));
                        }
                        if starting {
                            ui.add(egui::Spinner::new().size(16.0));
                        }
                    });
                });
            }
            State::Running { capture, since, size } => {
                let t = ui.input(|i| i.time);
                let mut stop_clicked = false;
                let stop_busy = self.stop.busy();
                kit::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        let pulse = 0.55 + 0.45 * ((t * 3.0).sin() as f32 * 0.5 + 0.5);
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 6.0, p.danger.gamma_multiply(pulse));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(kit::semibold("Capturing").size(15.0));
                            ui.label(
                                RichText::new(format!(
                                    "{} s · {} written",
                                    since.elapsed().as_secs(),
                                    size.map(human_size).unwrap_or_else(|| "…".into())
                                ))
                                .color(p.text2),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::Btn::new("Stop").icon(kit::ph::STOP).danger().enabled(!stop_busy).show(ui).clicked() {
                                stop_clicked = true;
                            }
                        });
                    });
                });
                if stop_clicked {
                    let (dev, remote) = (d.dev.clone(), capture.remote_path.clone());
                    // Stopping needs the live capture handle; take it out of the state.
                    if let State::Running { capture, .. } = std::mem::replace(&mut self.state, State::Idle) {
                        self.stop.set(env.sp.spawn(async move {
                            dev.stop_capture(&capture).await?;
                            let size = dev.remote_size(&remote).await;
                            Ok((remote, size))
                        }));
                    }
                }
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            State::Stopped { remote, size } => {
                let remote = remote.clone();
                let mut discard = false;
                kit::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(kit::ph::CHECK_CIRCLE).size(26.0).color(p.success));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(kit::semibold("Capture finished").size(15.0));
                            ui.label(
                                RichText::new(format!("{} on the device", size.map(human_size).unwrap_or_else(|| "unknown size".into())))
                                    .color(p.text2),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::Btn::new("Save as pcap")
                                .icon(kit::ph::FLOPPY_DISK)
                                .primary()
                                .enabled(!self.save_dialog.busy() && !self.save.busy())
                                .show(ui)
                                .clicked()
                            {
                                self.save_dialog.set(widgets::dialog(
                                    env.sp,
                                    Dialog::Save { title: "Save capture".into(), name: "capture.pcap".into() },
                                ));
                            }
                            if kit::Btn::new("Discard").icon(kit::ph::TRASH).danger().enabled(!self.save.busy()).show(ui).clicked() {
                                discard = true;
                            }
                        });
                    });
                });
                if discard {
                    let dev = d.dev.clone();
                    self.discard.set(env.sp.spawn(async move {
                        dev.remove_remote(&remote).await;
                        Ok(())
                    }));
                    self.state = State::Idle;
                }
            }
        }
        if let Some(r) = self.save_dialog.poll() {
            if let (Some(local), State::Stopped { remote, .. }) =
                (env.notes.report("File dialog", r).and_then(|p| p.into_iter().next()), &self.state)
            {
                let (dev, remote) = (d.dev.clone(), remote.clone());
                self.save.set(env.sp.spawn(async move {
                    dev.pull_to(&remote, &local).await?;
                    let count_path = local.clone();
                    let packets =
                        tokio::task::spawn_blocking(move || count_pcap_packets(&count_path)).await.map_err(|_| Error::Cancelled)?;
                    dev.remove_remote(&remote).await;
                    Ok(match packets {
                        Ok(n) => format!("Saved {} ({n} packets)", local.display()),
                        Err(_) => format!("Saved {}", local.display()),
                    })
                }));
            }
        }
    }
}

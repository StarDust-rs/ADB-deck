//! Live performance charts sampled once a second.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use egui_plot::{Line, Plot, PlotPoints};

use adbdeck::adb::perf::{cpu_load, CpuTimes, FpsTracker, PerfSample};
use adbdeck::jobs::Slot;

use super::widgets::{self, human_size};
use super::{kit, theme, DeviceCtx, Env};

const POINTS: usize = 120;
const INTERVAL: Duration = Duration::from_secs(1);

struct Frame {
    sample: PerfSample,
    fps: Option<u32>,
}

#[derive(Default)]
pub struct Performance {
    running: Option<bool>,
    job: Slot<Frame>,
    last_start: Option<Instant>,
    prev_cpus: Vec<CpuTimes>,
    tracker: FpsTracker,
    fps_fallback_bad: bool,
    cpu: VecDeque<f32>,
    mem: VecDeque<f32>,
    fps: VecDeque<f32>,
    temp: VecDeque<f32>,
    cores: Vec<(f32, u32)>,
    latest: Option<PerfSample>,
    fps_now: u32,
    error: Option<String>,
}

fn push(q: &mut VecDeque<f32>, v: f32) {
    if q.len() == POINTS {
        q.pop_front();
    }
    q.push_back(v);
}

fn chart(ui: &mut Ui, id: &str, title: &str, value: &str, data: &VecDeque<f32>, max: Option<f64>, color: egui::Color32) {
    let p = theme::pal(ui);
    kit::card_margin(ui, 14, |ui| {
        ui.horizontal(|ui| {
            // The swatch is the series key: it matches the line colour below.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, color);
            ui.label(RichText::new(title).size(12.5).color(p.text2));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(kit::semibold(value).size(16.0));
            });
        });
        ui.add_space(4.0);
        let pts: PlotPoints = data.iter().enumerate().map(|(i, v)| [i as f64, f64::from(*v)]).collect();
        let mut plot = Plot::new(id)
            .height(120.0)
            .allow_zoom(false)
            .allow_drag(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .include_y(0.0)
            .include_x(0.0)
            .include_x((POINTS - 1) as f64)
            .show_x(false)
            .show_y(false)
            .show_grid([false, true])
            .show_background(false)
            .show_axes([false, true])
            .set_margin_fraction(egui::vec2(0.0, 0.08));
        if let Some(m) = max {
            plot = plot.include_y(m);
        }
        plot.show(ui, |pl| pl.line(Line::new(pts).color(color).width(2.0_f32).fill(0.0_f32)));
    });
}

impl Performance {
    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        let running = *self.running.get_or_insert(true);
        if let Some(r) = self.job.poll() {
            match r {
                Ok(f) => self.apply(f),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if running && !self.job.busy() && self.last_start.is_none_or(|t| t.elapsed() >= INTERVAL) {
            self.last_start = Some(Instant::now());
            let dev = d.dev.clone();
            let need_fallback = self.fps_fallback_bad;
            self.job.set(env.sp.spawn(async move {
                let sample = dev.perf_sample().await?;
                let fps = if sample.flips.is_none() && !sample.top.package.is_empty() && need_fallback {
                    dev.fps_by_latency(&sample.top.package).await.ok()
                } else {
                    None
                };
                Ok(Frame { sample, fps })
            }));
        }
        if running {
            ui.ctx().request_repaint_after(Duration::from_millis(400));
        }

        let p = theme::pal(ui);
        kit::card_margin(ui, 8, |ui| {
            ui.horizontal(|ui| {
                let mut r = running;
                if kit::toggle(ui, &mut r, "Live sampling").changed() {
                    self.running = Some(r);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::Btn::new("Clear charts").icon(kit::ph::ERASER).show(ui).clicked() {
                        for q in [&mut self.cpu, &mut self.mem, &mut self.fps, &mut self.temp] {
                            q.clear();
                        }
                    }
                    if self.job.busy() {
                        ui.add(egui::Spinner::new().size(16.0));
                    }
                });
            });
        });
        ui.add_space(10.0);
        if let Some(e) = self.error.clone() {
            widgets::error_banner(ui, &e, false);
            ui.add_space(8.0);
        }
        let Some(s) = self.latest.as_ref() else {
            kit::busy(ui, "Collecting the first sample…");
            return;
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.columns(4, |cols| {
                let fg = if s.top.package.is_empty() {
                    "unknown".to_string()
                } else {
                    s.top.package.rsplit('.').next().unwrap_or("").to_string()
                };
                kit::stat_tile(&mut cols[0], "Foreground", &fg, &s.top.package);
                kit::stat_tile(&mut cols[1], "Battery", &format!("{}%", s.battery.level), &format!("{:.2} V", s.battery.voltage_v));
                kit::stat_tile(&mut cols[2], "Battery temp", &format!("{:.1} °C", s.battery.temperature_c), "");
                kit::stat_tile(&mut cols[3], "Memory", &human_size(s.mem_used), &format!("of {}", human_size(s.mem_total)));
            });
            ui.add_space(12.0);
            let cpu_now = self.cpu.back().copied().unwrap_or(0.0);
            let mem_now = self.mem.back().copied().unwrap_or(0.0);
            ui.columns(2, |cols| {
                chart(&mut cols[0], "cpu", "CPU usage", &format!("{cpu_now:.0}%"), &self.cpu, Some(100.0), p.accent_text);
                chart(&mut cols[1], "mem", "Memory used", &format!("{mem_now:.0}%"), &self.mem, Some(100.0), p.success);
            });
            ui.add_space(12.0);
            ui.columns(2, |cols| {
                chart(&mut cols[0], "fps", "Frames per second", &self.fps_now.to_string(), &self.fps, None, p.warning);
                if self.temp.is_empty() {
                    kit::card_margin(&mut cols[1], 14, |ui| {
                        ui.label(RichText::new("CPU temperature").size(12.5).color(p.text2));
                        ui.add_space(30.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new("This device does not report a CPU temperature.").color(p.text3));
                        });
                        ui.add_space(46.0);
                    });
                } else {
                    let now = self.temp.back().copied().unwrap_or(0.0);
                    chart(&mut cols[1], "temp", "CPU temperature", &format!("{now:.1} °C"), &self.temp, None, p.danger);
                }
            });
            ui.add_space(12.0);
            kit::card_titled(ui, "Cores", |ui| {
                ui.columns(2, |cols| {
                    for (i, (load, mhz)) in self.cores.iter().enumerate() {
                        let ui = &mut cols[i % 2];
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [44.0, 20.0],
                                egui::Label::new(RichText::new(format!("cpu{i}")).monospace().size(12.0).color(p.text2)),
                            );
                            kit::meter(ui, *load, (ui.available_width() - 130.0).max(60.0));
                            ui.add_sized(
                                [40.0, 20.0],
                                egui::Label::new(RichText::new(format!("{:.0}%", load * 100.0)).monospace().size(12.0)),
                            );
                            if *mhz > 0 {
                                ui.label(RichText::new(format!("{mhz} MHz")).size(11.5).color(p.text3));
                            }
                        });
                    }
                });
            });
        });
    }

    fn apply(&mut self, f: Frame) {
        self.error = None;
        let s = f.sample;
        let now = Instant::now();
        if self.prev_cpus.len() == s.cpus.len() && !s.cpus.is_empty() {
            self.cores = s
                .cpus
                .iter()
                .zip(&self.prev_cpus)
                .enumerate()
                .map(|(i, (n, p))| (cpu_load(*p, *n), s.freqs_mhz.get(&i).copied().unwrap_or(0)))
                .collect();
            let avg = self.cores.iter().map(|c| c.0).sum::<f32>() / self.cores.len() as f32;
            push(&mut self.cpu, avg * 100.0);
        }
        self.prev_cpus = s.cpus.clone();
        if s.mem_total > 0 {
            push(&mut self.mem, s.mem_used as f32 / s.mem_total as f32 * 100.0);
        }
        self.fps_now = match (s.flips, f.fps) {
            (Some(flips), _) => self.tracker.update(flips, now),
            (None, Some(v)) => v,
            (None, None) => {
                // Without a flip counter the next samples use the latency fallback.
                self.fps_fallback_bad = true;
                0
            }
        };
        push(&mut self.fps, self.fps_now as f32);
        if let Some(t) = s.cpu_temp_c {
            push(&mut self.temp, t);
        }
        self.latest = Some(s);
    }
}

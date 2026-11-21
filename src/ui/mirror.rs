//! Screen mirror: the device screen inside the app, with touch, keyboard and
//! clipboard forwarded to the phone.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{vec2, Color32, Rect, RichText, Sense, TextureHandle, TextureOptions, Ui};
use tokio::runtime::Runtime;

use adbdeck::jobs::Slot;
use adbdeck::mirror::proto::{button, key, POINTER_MOUSE};
use adbdeck::mirror::{self, Control, Event, Frame, KeyAction, ServerOptions, Session, TouchAction};
use adbdeck::paths;
use adbdeck::store::MirrorSettings;

use super::widgets::{self, Dialog};
use super::{kit, theme, DeviceCtx, Env};

/// Longest-side choices for the stream.
const SIZES: [(u32, &str); 5] = [(0, "Native"), (1920, "1920"), (1280, "1280"), (1024, "1024"), (720, "720")];
const RATES: [(u32, &str); 3] = [(0, "Auto"), (30, "30"), (60, "60")];

#[derive(Default)]
enum Status {
    #[default]
    Idle,
    Connecting,
    Live {
        name: String,
        width: u32,
        height: u32,
    },
}

#[derive(Default)]
pub struct MirrorPanel {
    session: Option<Session>,
    status: Status,
    /// Why the last session ended, when it was not the user's choice.
    failure: Option<String>,
    texture: Option<TextureHandle>,
    last: Option<Frame>,
    fps: Fps,
    /// Device position of the finger while the primary button is held.
    touching: Option<(i32, i32)>,
    /// The video was clicked last, so typing goes to the phone.
    focused: bool,
    screen_off: bool,
    save: Slot<Vec<PathBuf>>,
    saving: Option<Frame>,
    write: Slot<PathBuf>,
}

/// Frames per second over the last second.
#[derive(Default)]
struct Fps {
    since: Option<Instant>,
    count: u32,
    shown: u32,
}

impl Fps {
    fn tick(&mut self) {
        self.count += 1;
        self.update();
    }

    fn update(&mut self) {
        let since = *self.since.get_or_insert_with(Instant::now);
        if since.elapsed() >= Duration::from_secs(1) {
            self.shown = self.count;
            self.count = 0;
            self.since = Some(Instant::now());
        }
    }
}

fn server_options(s: &MirrorSettings) -> ServerOptions {
    ServerOptions {
        scid: mirror::fresh_scid(),
        bit_rate: s.video_bit_rate_mbps.clamp(1, 100) * 1_000_000,
        max_size: s.max_size,
        max_fps: s.max_fps,
        control: true,
        stay_awake: s.stay_awake,
        show_touches: s.show_touches,
        power_off_on_close: s.power_off_on_close,
        clipboard_autosync: false,
    }
}

impl MirrorPanel {
    /// Called every frame with the selected device: a session never outlives
    /// the device it belongs to.
    pub fn sync_device(&mut self, selected: Option<&str>, visible: bool) {
        if let Some(s) = &self.session {
            if selected != Some(s.serial.as_str()) {
                self.end(None);
            } else {
                s.set_visible(visible);
            }
        }
    }

    fn end(&mut self, failure: Option<String>) {
        if let Some(mut s) = self.session.take() {
            s.stop();
        }
        self.status = Status::Idle;
        self.failure = failure;
        self.touching = None;
        self.focused = false;
        self.screen_off = false;
        self.texture = None;
        self.last = None;
    }

    fn begin(&mut self, env: &Env, d: &DeviceCtx, settings: &MirrorSettings) {
        self.failure = None;
        let scratch = paths::cache_dir().unwrap_or_else(std::env::temp_dir).join("mirror");
        let sp = env.sp.clone();
        let wake = move || sp.wake();
        self.session = Some(Session::start(d.dev.clone(), server_options(settings), &scratch, env.sp.handle(), std::sync::Arc::new(wake)));
        self.status = Status::Connecting;
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        self.poll(ui, env, d);
        if let Some(r) = self.save.poll() {
            if let (Some(paths), Some(frame)) = (env.notes.report("File dialog", r), self.saving.take()) {
                if let Some(path) = paths.into_iter().next() {
                    self.write.set(env.sp.spawn(async move { write_png(path, frame).await }));
                }
            }
        }
        if let Some(r) = self.write.poll() {
            if let Some(path) = env.notes.report("Save picture", r) {
                env.notes.info(format!("Saved {}", path.display()));
            }
        }
        if self.session.is_some() {
            self.live(ui, env, d);
        } else {
            self.setup(ui, env, d);
        }
    }

    /// Takes in what the session reported since the last frame.
    fn poll(&mut self, ui: &Ui, env: &mut Env, d: &DeviceCtx) {
        let settings = env.data.mirror.get(&d.device.id).cloned().unwrap_or_default();
        let mut ended = None;
        if let Some(session) = self.session.as_mut() {
            while let Some(event) = session.poll_event() {
                match event {
                    Event::Started { device_name, width, height } => {
                        self.status = Status::Live { name: device_name, width, height };
                        if settings.turn_screen_off {
                            session.send(Control::SetDisplayPower(false));
                            self.screen_off = true;
                        }
                    }
                    Event::Clipboard(text) => {
                        ui.ctx().copy_text(text);
                        env.notes.info("Copied from the device");
                    }
                    Event::Ended(why) => ended = Some(why),
                }
            }
            if let Some(frame) = session.take_frame() {
                self.fps.tick();
                let image = egui::ColorImage::from_rgba_unmultiplied([frame.width, frame.height], &frame.rgba);
                match self.texture.as_mut() {
                    Some(tex) => tex.set(image, TextureOptions::LINEAR),
                    None => self.texture = Some(ui.ctx().load_texture("mirror-frame", image, TextureOptions::LINEAR)),
                }
                self.last = Some(frame);
            }
        }
        if let Some(why) = ended {
            self.end(why);
        }
    }

    fn setup(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        let p = theme::pal(ui);
        let serial = d.device.id.clone();
        let settings: &mut MirrorSettings = env.data.mirror.entry(serial).or_default();
        let before = settings.clone();
        let mut start = false;
        let mut retry_hint = None;
        if let Some(why) = &self.failure {
            retry_hint = Some(why.clone());
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if let Some(why) = &retry_hint {
                if kit::banner(ui, kit::Kind::Danger, &format!("Mirroring stopped: {why}"), Some("Try again")) {
                    start = true;
                }
                ui.add_space(12.0);
            }
            kit::card(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
                    ui.painter().rect_filled(rect, egui::CornerRadius::same(14), p.accent_soft);
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        kit::ph::DEVICE_MOBILE,
                        egui::FontId::proportional(30.0),
                        p.accent_text,
                    );
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        let name = if d.device.name.is_empty() { d.device.id.as_str() } else { d.device.name.as_str() };
                        ui.label(kit::semibold(format!("Mirror {name}")).size(18.0));
                        ui.label(
                            RichText::new("See and control the screen right here. Nothing to install on your computer.").color(p.text2),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::Btn::new("Start mirroring").icon(kit::ph::PLAY).primary().show(ui).clicked() {
                            start = true;
                        }
                    });
                });
            });
            ui.add_space(12.0);
            kit::card_titled(ui, "Quality", |ui| {
                egui::Grid::new("mirror-quality").num_columns(2).min_col_width(130.0).spacing([20.0, 12.0]).show(ui, |ui| {
                    ui.label(RichText::new("Video bit rate").color(p.text2));
                    ui.horizontal(|ui| {
                        let mut v = settings.video_bit_rate_mbps as f32;
                        if kit::slider(ui, &mut v, 1.0..=40.0, 1.0, 0, 220.0).changed() {
                            settings.video_bit_rate_mbps = v.round() as u32;
                        }
                        ui.label(RichText::new("Mbit/s").size(12.0).color(p.text3));
                    });
                    ui.end_row();
                    ui.label(RichText::new("Resolution").color(p.text2));
                    ui.horizontal(|ui| {
                        kit::segmented(ui, &mut settings.max_size, &SIZES);
                        ui.label(RichText::new("longest side, px").size(12.0).color(p.text3));
                    });
                    ui.end_row();
                    ui.label(RichText::new("Frame rate").color(p.text2));
                    ui.horizontal(|ui| {
                        kit::segmented(ui, &mut settings.max_fps, &RATES);
                        ui.label(RichText::new("frames per second, at most").size(12.0).color(p.text3));
                    });
                    ui.end_row();
                });
            });
            ui.add_space(12.0);
            kit::card_titled(ui, "Behaviour", |ui| {
                ui.columns(2, |cols| {
                    kit::toggle(&mut cols[0], &mut settings.stay_awake, "Keep the phone awake");
                    kit::toggle(&mut cols[0], &mut settings.show_touches, "Show touches on the phone");
                    kit::toggle(&mut cols[1], &mut settings.turn_screen_off, "Turn the phone's screen off");
                    kit::toggle(&mut cols[1], &mut settings.power_off_on_close, "Screen off when mirroring ends");
                });
            });
            ui.add_space(12.0);
            kit::card_titled(ui, "Controls", |ui| {
                egui::Grid::new("mirror-keys").num_columns(2).min_col_width(170.0).spacing([20.0, 7.0]).show(ui, |ui| {
                    for (what, how) in [
                        ("Touch and swipe", "Click and drag on the screen"),
                        ("Back", "Right click, or Esc"),
                        ("Home", "Middle click"),
                        ("Scroll", "Mouse wheel"),
                        ("Type text", "Click the screen, then type"),
                        ("Paste to the phone", "Ctrl+V"),
                        ("Copy from the phone", "Ctrl+C"),
                    ] {
                        ui.label(RichText::new(what).color(p.text2));
                        ui.label(how);
                        ui.end_row();
                    }
                });
            });
        });
        if *settings != before {
            *env.data_dirty = true;
        }
        if start {
            let settings = settings.clone();
            self.begin(env, d, &settings);
        }
    }

    fn live(&mut self, ui: &mut Ui, env: &mut Env, d: &DeviceCtx) {
        let p = theme::pal(ui);
        self.fps.update();
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        let mut stop = false;
        let mut shot = false;

        kit::card_margin(ui, 8, |ui| {
            ui.horizontal(|ui| {
                match &self.status {
                    Status::Live { name, width, height } => {
                        kit::dot(ui, kit::Kind::Success);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(kit::semibold(name).size(13.5));
                            let (w, h) = self.last.as_ref().map_or((*width as usize, *height as usize), |f| (f.width, f.height));
                            ui.label(RichText::new(format!("{w}×{h} · {} fps", self.fps.shown)).size(11.5).color(p.text3));
                        });
                    }
                    _ => {
                        ui.add(egui::Spinner::new().size(16.0));
                        ui.label(RichText::new("Starting the screen server on the phone…").color(p.text2));
                    }
                }
                kit::vsep(ui);
                let live = matches!(self.status, Status::Live { .. });
                ui.add_enabled_ui(live, |ui| {
                    let tap = |ui: &mut Ui, icon: &str, tip: &str| kit::Btn::new("").icon(icon).subtle().tip(tip).show(ui).clicked();
                    if tap(ui, kit::ph::ARROW_LEFT, "Back") {
                        self.click_key(key::BACK);
                    }
                    if tap(ui, kit::ph::HOUSE, "Home") {
                        self.click_key(key::HOME);
                    }
                    if tap(ui, kit::ph::SQUARE, "Recent apps") {
                        self.click_key(key::APP_SWITCH);
                    }
                    kit::vsep(ui);
                    if tap(ui, kit::ph::SPEAKER_LOW, "Volume down") {
                        self.click_key(key::VOLUME_DOWN);
                    }
                    if tap(ui, kit::ph::SPEAKER_HIGH, "Volume up") {
                        self.click_key(key::VOLUME_UP);
                    }
                    if tap(ui, kit::ph::POWER, "Power") {
                        self.click_key(key::POWER);
                    }
                    kit::vsep(ui);
                    if tap(ui, kit::ph::DEVICE_ROTATE, "Rotate the screen") {
                        self.send(Control::Rotate);
                    }
                    if tap(ui, kit::ph::BELL, "Notifications") {
                        self.send(Control::ExpandNotifications);
                    }
                    if tap(ui, kit::ph::SLIDERS_HORIZONTAL, "Quick settings") {
                        self.send(Control::ExpandSettings);
                    }
                    let (icon, tip) = if self.screen_off {
                        (kit::ph::SUN, "Turn the phone's screen on")
                    } else {
                        (kit::ph::MOON, "Turn the phone's screen off")
                    };
                    if tap(ui, icon, tip) {
                        self.screen_off = !self.screen_off;
                        self.send(Control::SetDisplayPower(!self.screen_off));
                    }
                    kit::vsep(ui);
                    if tap(ui, kit::ph::CAMERA, "Save the current picture") {
                        shot = true;
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::Btn::new("Stop").icon(kit::ph::STOP).danger().show(ui).clicked() {
                        stop = true;
                    }
                });
            });
        });
        ui.add_space(10.0);
        self.video(ui);
        if shot {
            if let Some(frame) = self.last.clone() {
                self.saving = Some(frame);
                let name = format!("{}-{}.png", d.device.name.replace(' ', "-").to_lowercase(), super::screenshot::chrono_stamp());
                self.save.set(widgets::dialog(env.sp, Dialog::Save { title: "Save picture".into(), name }));
            }
        }
        if stop {
            self.end(None);
        }
    }

    fn send(&self, message: Control) {
        if let Some(s) = &self.session {
            s.send(message);
        }
    }

    /// A key press and release.
    fn click_key(&self, keycode: u32) {
        self.send(Control::Key { action: KeyAction::Down, keycode, repeat: 0, meta: 0 });
        self.send(Control::Key { action: KeyAction::Up, keycode, repeat: 0, meta: 0 });
    }

    /// Draws the picture, letterboxed, and turns pointer and keyboard input into messages.
    fn video(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        let area = ui.available_size();
        let (outer, response) = ui.allocate_exact_size(area, Sense::click_and_drag());
        ui.painter().rect_filled(outer, egui::CornerRadius::same(10), if p.dark { Color32::from_gray(10) } else { Color32::from_gray(28) });
        let (Some(tex), Some(frame)) = (&self.texture, &self.last) else {
            ui.painter().text(
                outer.center(),
                egui::Align2::CENTER_CENTER,
                "Waiting for the first picture…",
                egui::FontId::proportional(14.0),
                Color32::from_gray(150),
            );
            return;
        };
        let (w, h) = (frame.width as f32, frame.height as f32);
        let scale = ((outer.width() - 16.0) / w).min((outer.height() - 16.0) / h).max(0.05);
        let rect = Rect::from_center_size(outer.center(), vec2(w * scale, h * scale));
        egui::Image::new(egui::load::SizedTexture::new(tex.id(), rect.size())).corner_radius(6.0).paint_at(ui, rect);

        let to_device = |pos: egui::Pos2| -> (i32, i32) {
            let fx = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let fy = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
            (((fx * w) as i32).min(frame.width as i32 - 1), ((fy * h) as i32).min(frame.height as i32 - 1))
        };
        let (fw, fh) = (frame.width.min(u16::MAX as usize) as u16, frame.height.min(u16::MAX as usize) as u16);
        let touch = |action: TouchAction, (x, y): (i32, i32)| {
            let down = action != TouchAction::Up;
            Control::Touch {
                action,
                pointer: POINTER_MOUSE,
                x,
                y,
                width: fw,
                height: fh,
                pressure: if down { 1.0 } else { 0.0 },
                action_button: button::PRIMARY,
                buttons: if down { button::PRIMARY } else { 0 },
            }
        };

        let (pointer, pressed, down, released, secondary, middle, scroll) = ui.input(|i| {
            (
                i.pointer.latest_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_down(egui::PointerButton::Primary),
                i.pointer.button_released(egui::PointerButton::Primary),
                i.pointer.button_clicked(egui::PointerButton::Secondary),
                i.pointer.button_clicked(egui::PointerButton::Middle),
                i.smooth_scroll_delta,
            )
        });
        let over = response.hovered() && pointer.is_some_and(|pos| rect.contains(pos));
        if ui.input(|i| i.pointer.any_pressed()) {
            self.focused = over;
        }
        if over {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }

        // Touch: down inside the picture, moves and the release may leave it.
        if let Some(pos) = pointer {
            let at = to_device(pos);
            if pressed && over {
                // Where the button went down, even if the pointer already moved this frame.
                let origin = ui.input(|i| i.pointer.press_origin()).map_or(at, to_device);
                self.touching = Some(origin);
                self.send(touch(TouchAction::Down, origin));
            } else if let Some(last) = self.touching {
                if down && at != last {
                    self.touching = Some(at);
                    self.send(touch(TouchAction::Move, at));
                }
                if released || !down {
                    self.send(touch(TouchAction::Up, at));
                    self.touching = None;
                }
            }
            if over && secondary {
                self.send(Control::BackOrScreenOn(KeyAction::Down));
                self.send(Control::BackOrScreenOn(KeyAction::Up));
            }
            if over && middle {
                self.click_key(key::HOME);
            }
            if over && (scroll.x != 0.0 || scroll.y != 0.0) {
                self.send(Control::Scroll { x: at.0, y: at.1, width: fw, height: fh, h: scroll.x / 40.0, v: scroll.y / 40.0, buttons: 0 });
            }
        } else if let Some(at) = self.touching.take() {
            // The pointer left the window mid-drag: let go where it was.
            self.send(touch(TouchAction::Up, at));
        }

        if self.focused || over {
            self.keyboard(ui);
        }
    }

    /// Typed text, special keys and the clipboard go to the phone.
    fn keyboard(&mut self, ui: &Ui) {
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                egui::Event::Text(text) => {
                    if !text.chars().all(char::is_control) {
                        self.send(Control::Text(text));
                    }
                }
                egui::Event::Paste(text) => self.send(Control::SetClipboard { sequence: 0, paste: true, text }),
                egui::Event::Copy => self.send(Control::GetClipboard { cut: false }),
                egui::Event::Cut => self.send(Control::GetClipboard { cut: true }),
                egui::Event::Key { key: k, pressed, repeat, modifiers, .. } => {
                    if modifiers.command || modifiers.alt {
                        continue;
                    }
                    if let Some(code) = keycode(k) {
                        let action = if pressed { KeyAction::Down } else { KeyAction::Up };
                        self.send(Control::Key { action, keycode: code, repeat: u32::from(repeat), meta: 0 });
                    }
                }
                _ => {}
            }
        }
    }

    /// Ends the session and waits briefly for the phone to be cleaned up (app exit).
    pub fn shutdown(&mut self, rt: &Runtime) {
        if let Some(mut s) = self.session.take() {
            s.finish(rt);
        }
    }
}

/// Keys that have no text of their own.
fn keycode(k: egui::Key) -> Option<u32> {
    use egui::Key as K;
    Some(match k {
        K::Enter => key::ENTER,
        K::Backspace => key::DEL,
        K::Delete => key::FORWARD_DEL,
        K::Tab => key::TAB,
        K::Escape => key::BACK,
        K::ArrowUp => key::DPAD_UP,
        K::ArrowDown => key::DPAD_DOWN,
        K::ArrowLeft => key::DPAD_LEFT,
        K::ArrowRight => key::DPAD_RIGHT,
        K::PageUp => key::PAGE_UP,
        K::PageDown => key::PAGE_DOWN,
        K::Home => key::MOVE_HOME,
        K::End => key::MOVE_END,
        _ => return None,
    })
}

async fn write_png(path: PathBuf, frame: Frame) -> adbdeck::error::Result<PathBuf> {
    tokio::task::spawn_blocking(move || {
        image::save_buffer_with_format(
            &path,
            &frame.rgba,
            frame.width as u32,
            frame.height as u32,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .map_err(|e| adbdeck::error::Error::Invalid(format!("could not save {}: {e}", path.display())))?;
        Ok(path)
    })
    .await
    .map_err(|_| adbdeck::error::Error::Cancelled)?
}

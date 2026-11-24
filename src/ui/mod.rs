//! The egui application: window layout, device selection and panel routing.

pub mod kit;
pub mod theme;
pub mod widgets;

mod app_icons;
mod app_manage;
mod apps;
mod capture;
mod controls;
mod devices;
mod files;
mod layout;
mod logcat;
mod mirror;
mod overview;
mod perf;
mod processes;
mod qr_connect;
mod remote;
mod screenshot;
mod settings;
mod signing;
mod terminal;
mod virtual_devices;
mod webview;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use egui::{RichText, Ui};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use adbdeck::adb::devices::{watch_devices, Device, DeviceState, Registry, Suppressed, WatchEvent, WatchOptions};
use adbdeck::adb::{Adb, DeviceAdb};
use adbdeck::autostart;
use adbdeck::jobs::{Slot, Spawner};
use adbdeck::store::{AppData, Settings, Store, ThemeChoice};
use adbdeck::tools::{detect_all, LookupEnv, Tool, ToolStatus};

use widgets::{NoteKind, Notes};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelId {
    Overview,
    Files,
    Apps,
    Processes,
    Performance,
    Controls,
    Shell,
    Layout,
    Screenshot,
    Logcat,
    Capture,
    Webview,
    Remote,
    Mirror,
    Devices,
    VirtualDevices,
    Signing,
    Settings,
    About,
}

impl PanelId {
    const GROUPS: [(&'static str, &'static [PanelId]); 3] = [
        ("Device", &[PanelId::Overview, PanelId::Files, PanelId::Apps, PanelId::Processes, PanelId::Performance, PanelId::Controls]),
        (
            "Tools",
            &[
                PanelId::Shell,
                PanelId::Layout,
                PanelId::Screenshot,
                PanelId::Logcat,
                PanelId::Capture,
                PanelId::Webview,
                PanelId::Remote,
                PanelId::Mirror,
            ],
        ),
        ("Manage", &[PanelId::Devices, PanelId::VirtualDevices, PanelId::Signing]),
    ];

    const FOOTER: [PanelId; 2] = [PanelId::Settings, PanelId::About];

    fn all() -> impl Iterator<Item = PanelId> {
        Self::GROUPS.iter().flat_map(|(_, ids)| ids.iter().copied()).chain(Self::FOOTER)
    }

    fn icon(self) -> &'static str {
        use kit::ph;
        match self {
            PanelId::Overview => ph::DEVICE_MOBILE,
            PanelId::Files => ph::FOLDER_OPEN,
            PanelId::Apps => ph::PACKAGE,
            PanelId::Processes => ph::CPU,
            PanelId::Performance => ph::GAUGE,
            PanelId::Controls => ph::SLIDERS_HORIZONTAL,
            PanelId::Shell => ph::TERMINAL_WINDOW,
            PanelId::Layout => ph::SELECTION,
            PanelId::Screenshot => ph::CAMERA,
            PanelId::Logcat => ph::LIST_MAGNIFYING_GLASS,
            PanelId::Capture => ph::NETWORK,
            PanelId::Webview => ph::GLOBE,
            PanelId::Remote => ph::GAME_CONTROLLER,
            PanelId::Mirror => ph::MONITOR_PLAY,
            PanelId::Devices => ph::PLUGS_CONNECTED,
            PanelId::VirtualDevices => ph::DESKTOP,
            PanelId::Signing => ph::SEAL_CHECK,
            PanelId::Settings => ph::GEAR,
            PanelId::About => ph::INFO,
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            PanelId::Overview => "Hardware, storage, network and quick settings",
            PanelId::Files => "Browse, upload and download files on the device",
            PanelId::Apps => "Install, inspect and manage applications",
            PanelId::Processes => "Running processes and their resource use",
            PanelId::Performance => "Live CPU, memory, frame rate, battery and temperature",
            PanelId::Controls => "Radios, display, developer options, battery simulation and power",
            PanelId::Shell => "Interactive adb shell on the device",
            PanelId::Layout => "Inspect the view hierarchy of the current screen",
            PanelId::Screenshot => "Capture, copy and save the device screen",
            PanelId::Logcat => "Live system log with filters",
            PanelId::Capture => "Record network traffic on the device with tcpdump",
            PanelId::Webview => "Debug WebViews and Chrome tabs running on the device",
            PanelId::Remote => "Send key presses and navigation to the device",
            PanelId::Mirror => "Show and control the device screen",
            PanelId::Devices => "Connect over Wi-Fi or with a QR code, pair, and manage port mappings",
            PanelId::VirtualDevices => "Start and stop Android emulators",
            PanelId::Signing => "Sign and verify APK files with apksigner",
            PanelId::Settings => "Appearance, tool locations and behaviour",
            PanelId::About => "Version information and external tool status",
        }
    }

    fn label(self) -> &'static str {
        match self {
            PanelId::Overview => "Overview",
            PanelId::Files => "Files",
            PanelId::Apps => "Apps",
            PanelId::Processes => "Processes",
            PanelId::Performance => "Performance",
            PanelId::Controls => "Device controls",
            PanelId::Shell => "Shell",
            PanelId::Layout => "Layout inspector",
            PanelId::Screenshot => "Screenshot",
            PanelId::Logcat => "Logcat",
            PanelId::Capture => "Network capture",
            PanelId::Webview => "WebView inspector",
            PanelId::Remote => "Remote control",
            PanelId::Mirror => "Screen mirror",
            PanelId::Devices => "Connections",
            PanelId::VirtualDevices => "Virtual devices",
            PanelId::Signing => "APK signing",
            PanelId::Settings => "Settings",
            PanelId::About => "About & tools",
        }
    }

    fn key(self) -> &'static str {
        match self {
            PanelId::Overview => "overview",
            PanelId::Files => "files",
            PanelId::Apps => "apps",
            PanelId::Processes => "processes",
            PanelId::Performance => "performance",
            PanelId::Controls => "controls",
            PanelId::Shell => "shell",
            PanelId::Layout => "layout",
            PanelId::Screenshot => "screenshot",
            PanelId::Logcat => "logcat",
            PanelId::Capture => "capture",
            PanelId::Webview => "webview",
            PanelId::Remote => "remote",
            PanelId::Mirror => "mirror",
            PanelId::Devices => "devices",
            PanelId::VirtualDevices => "virtual",
            PanelId::Signing => "signing",
            PanelId::Settings => "settings",
            PanelId::About => "about",
        }
    }

    fn from_key(key: &str) -> Option<PanelId> {
        Self::all().find(|p| p.key() == key)
    }

    /// Panels that operate on the selected device.
    fn needs_device(self) -> bool {
        matches!(
            self,
            PanelId::Overview
                | PanelId::Files
                | PanelId::Apps
                | PanelId::Processes
                | PanelId::Performance
                | PanelId::Controls
                | PanelId::Shell
                | PanelId::Layout
                | PanelId::Screenshot
                | PanelId::Logcat
                | PanelId::Capture
                | PanelId::Webview
                | PanelId::Remote
                | PanelId::Mirror
        )
    }
}

/// The selected, online device as panels use it.
pub struct DeviceCtx {
    pub device: Device,
    pub dev: DeviceAdb,
}

/// Located helper programs.
#[derive(Clone, Default)]
pub struct ToolPaths {
    pub apksigner: Option<PathBuf>,
    pub emulator: Option<PathBuf>,
}

/// Everything a panel may use besides its own state.
pub struct Env<'a> {
    pub sp: &'a Spawner,
    pub adb: Option<&'a Adb>,
    pub data: &'a mut AppData,
    pub settings: &'a Settings,
    pub tools: &'a ToolPaths,
    pub notes: &'a mut Notes,
    pub devices: &'a [Device],
    /// Set when `data` changed and should be written to disk.
    pub data_dirty: &'a mut bool,
    /// Wi-Fi devices the user disconnected on purpose; see [`suppress`](adbdeck::adb::devices::suppress).
    pub suppressed: &'a Suppressed,
}

/// State of panels that belong to one device; dropped (and its background
/// work cancelled) when the device changes or disconnects.
struct DevicePanels {
    serial: String,
    overview: overview::Overview,
    files: files::Files,
    apps: apps::Apps,
    processes: processes::Processes,
    perf: perf::Performance,
    controls: controls::DeviceControls,
    shell: terminal::Shell,
    layout: layout::LayoutInspector,
    screenshot: screenshot::Screenshot,
    logcat: logcat::Logcat,
    capture: capture::Capture,
    webview: webview::Webview,
    remote: remote::Remote,
}

impl DevicePanels {
    fn new(serial: &str) -> Self {
        Self {
            serial: serial.to_string(),
            overview: Default::default(),
            files: Default::default(),
            apps: Default::default(),
            processes: Default::default(),
            perf: Default::default(),
            controls: Default::default(),
            shell: Default::default(),
            layout: Default::default(),
            screenshot: Default::default(),
            logcat: Default::default(),
            capture: Default::default(),
            webview: Default::default(),
            remote: Default::default(),
        }
    }
}

#[derive(Default)]
struct GlobalPanels {
    mirror: mirror::MirrorPanel,
    devices: devices::Connections,
    virtual_devices: virtual_devices::VirtualDevices,
    signing: signing::Signing,
    settings: settings::SettingsPanel,
}

pub struct App {
    rt: Arc<Runtime>,
    sp: Spawner,
    store: Store,
    settings: Settings,
    data: AppData,
    data_dirty: bool,
    status: Vec<ToolStatus>,
    tools: ToolPaths,
    adb: Option<Adb>,
    devices: Vec<Device>,
    device_error: Option<String>,
    devices_loaded: bool,
    device_rx: Option<mpsc::Receiver<WatchEvent>>,
    /// The Wi-Fi device being reconnected: id, attempt, attempts allowed.
    reconnecting: Option<(String, u32, u32)>,
    /// Wi-Fi devices the user disconnected on purpose.
    suppressed: Suppressed,
    watcher: Option<JoinHandle<()>>,
    refresh: Slot<Vec<Device>>,
    selected: Option<String>,
    panel: PanelId,
    notes: Notes,
    dp: Option<DevicePanels>,
    global: GlobalPanels,
    pending_apks: Vec<PathBuf>,
    applied_theme: Option<ThemeChoice>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        rt: Arc<Runtime>,
        store: Store,
        settings: Settings,
        data: AppData,
        startup_notes: Vec<String>,
        apks: Vec<PathBuf>,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        theme::install(&ctx);
        let wake_ctx = ctx.clone();
        let sp = Spawner::new(rt.handle().clone(), Arc::new(move || wake_ctx.request_repaint()));
        let mut notes = Notes::default();
        for n in startup_notes {
            notes.error("Startup", &adbdeck::error::Error::Invalid(n));
        }
        let panel = PanelId::from_key(&data.last_panel).unwrap_or(PanelId::Overview);
        // Phones the user disconnected on purpose stay disconnected across restarts.
        let suppressed: Suppressed = Arc::new(std::sync::Mutex::new(Registry::from_data(&data).blocked));
        let mut app = App {
            rt,
            sp,
            store,
            settings,
            data,
            data_dirty: false,
            status: Vec::new(),
            tools: ToolPaths::default(),
            adb: None,
            devices: Vec::new(),
            device_error: None,
            devices_loaded: false,
            device_rx: None,
            reconnecting: None,
            suppressed,
            watcher: None,
            refresh: Slot::default(),
            selected: None,
            panel,
            notes,
            dp: None,
            global: GlobalPanels::default(),
            pending_apks: apks,
            applied_theme: None,
        };
        app.redetect_tools(&ctx);
        app.sync_background_service();
        app
    }

    /// The service has nothing to do while automatic reconnection is off.
    fn background_service_wanted(&self) -> bool {
        self.settings.background_service && self.settings.auto_reconnect
    }

    /// Makes the login autostart entry match the setting (and the program's current path).
    fn sync_background_service(&mut self) {
        let result = std::env::current_exe()
            .map_err(|e| adbdeck::error::Error::io("locating the ADB Deck program", e))
            .and_then(|exe| autostart::sync(self.background_service_wanted(), &exe));
        if let Err(e) = result {
            self.notes.error("Background service", &e);
        }
    }

    /// Looks the external tools up again and (re)starts the device watcher.
    fn redetect_tools(&mut self, ctx: &egui::Context) {
        self.status = detect_all(&self.settings, &LookupEnv::from_process());
        let path = |t: Tool| self.status.iter().find(|s| s.tool == t).and_then(|s| s.path.clone());
        self.tools = ToolPaths { apksigner: path(Tool::Apksigner), emulator: path(Tool::Emulator) };
        let new_adb = path(Tool::Adb).map(Adb::new);
        let changed = self.adb.as_ref().map(|a| a.path().to_path_buf()) != new_adb.as_ref().map(|a| a.path().to_path_buf());
        if changed || self.watcher.is_none() {
            if let Some(w) = self.watcher.take() {
                w.abort();
            }
            self.device_rx = None;
            self.devices.clear();
            self.devices_loaded = false;
            self.dp = None;
            self.adb = new_adb;
            if let Some(adb) = self.adb.clone() {
                let options = WatchOptions {
                    remembered: self.data.remote_devices.iter().filter(|r| r.auto_connect).map(|r| r.id.clone()).collect(),
                    auto_reconnect: self.settings.auto_reconnect,
                    suppressed: self.suppressed.clone(),
                    known: Registry::from_data(&self.data).known,
                    ..WatchOptions::default()
                };
                let (tx, rx) = mpsc::channel(8);
                let (fwd_tx, fwd_rx) = mpsc::channel(8);
                let wake = ctx.clone();
                self.watcher = Some(self.rt.spawn(async move {
                    let inner = tokio::spawn(watch_devices(adb, tx, options));
                    let mut rx: mpsc::Receiver<_> = rx;
                    while let Some(update) = rx.recv().await {
                        if fwd_tx.send(update).await.is_err() {
                            break;
                        }
                        wake.request_repaint();
                    }
                    inner.abort();
                }));
                self.device_rx = Some(fwd_rx);
            }
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.applied_theme != Some(self.settings.theme) {
            ctx.set_theme(match self.settings.theme {
                ThemeChoice::System => egui::ThemePreference::System,
                ThemeChoice::Light => egui::ThemePreference::Light,
                ThemeChoice::Dark => egui::ThemePreference::Dark,
            });
            self.applied_theme = Some(self.settings.theme);
        }
    }

    fn poll_devices(&mut self) {
        let mut latest = None;
        let mut events = Vec::new();
        if let Some(rx) = self.device_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            match event {
                WatchEvent::Devices(d) => latest = Some(Ok(d)),
                WatchEvent::Failed(e) => latest = Some(Err(e)),
                WatchEvent::Reconnecting { id, attempt, of } => self.reconnecting = Some((id, attempt, of)),
                WatchEvent::Restored(id) => {
                    self.reconnecting = None;
                    self.notes.info(format!("Reconnected to {id}"));
                }
                WatchEvent::Found(id) => self.notes.info(format!("Found {id} on your network and connected")),
                WatchEvent::GaveUp(id) => {
                    self.reconnecting = None;
                    self.notes.push(
                        NoteKind::Error,
                        format!("Could not reconnect to {id}. Connect again from Connections once it is reachable."),
                    );
                }
            }
        }
        if let Some(r) = self.refresh.poll() {
            latest = Some(r);
        }
        match latest {
            Some(Ok(devices)) => {
                if devices::remember_remote(&mut self.data, &devices, unix_now()) {
                    self.data_dirty = true;
                }
                // Done trying once the device is back, whoever brought it.
                if let Some((id, ..)) = &self.reconnecting {
                    if devices.iter().any(|d| &d.id == id && d.is_online()) {
                        self.reconnecting = None;
                    }
                }
                self.devices = devices;
                self.device_error = None;
                self.devices_loaded = true;
                self.reconcile_selection();
            }
            Some(Err(e)) => {
                self.device_error = Some(e.to_string());
                self.devices_loaded = true;
            }
            None => {}
        }
    }

    fn reconcile_selection(&mut self) {
        let online = |d: &Device| d.state == DeviceState::Online;
        let current_ok = self.selected.as_ref().is_some_and(|s| self.devices.iter().any(|d| &d.id == s && online(d)));
        if !current_ok {
            // Prefer the last used device, then the first online one.
            let last = self.devices.iter().find(|d| d.id == self.data.last_device && online(d));
            let first = self.devices.iter().find(|d| online(d));
            let choice = last.or(first).map(|d| d.id.clone());
            if self.selected.is_some() && choice.is_none() {
                // Keep the id so the UI can say which device went away.
                self.dp = None;
            } else {
                self.selected = choice;
            }
        }
        if let Some(sel) = &self.selected {
            let usable = self.devices.iter().any(|d| &d.id == sel && online(d));
            if !usable {
                self.dp = None;
            } else if self.dp.as_ref().map(|p| &p.serial) != Some(sel) {
                self.dp = Some(DevicePanels::new(sel));
                if self.data.last_device != *sel {
                    self.data.last_device = sel.clone();
                    self.data_dirty = true;
                }
                if !self.pending_apks.is_empty() {
                    self.panel = PanelId::Apps;
                }
            }
        }
    }

    fn selected_device(&self) -> Option<Device> {
        let sel = self.selected.as_ref()?;
        self.devices.iter().find(|d| &d.id == sel && d.state == DeviceState::Online).cloned()
    }

    fn refresh_now(&mut self) {
        if let Some(adb) = self.adb.clone() {
            if !self.refresh.busy() {
                self.refresh.set(self.sp.spawn(async move { adb.snapshot().await }));
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if dropped.is_empty() {
            return;
        }
        let apks: Vec<PathBuf> = dropped.into_iter().filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("apk"))).collect();
        if apks.is_empty() {
            self.notes.info("Only .apk files can be dropped to install.");
        } else if self.dp.is_none() {
            self.notes.info("Connect and select a device to install dropped APK files.");
        } else {
            self.pending_apks.extend(apks);
            self.panel = PanelId::Apps;
        }
    }

    fn save_if_dirty(&mut self) {
        if self.data_dirty {
            self.data_dirty = false;
            if let Err(e) = self.store.save_data(&self.data) {
                self.notes.error("Could not save application data", &e);
            }
        }
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.store.save_settings(&self.settings) {
            self.notes.error("Could not save settings", &e);
        }
    }

    // ---- drawing --------------------------------------------------------

    fn nav(&mut self, ui: &mut Ui) {
        if is_compact(ui.ctx()) {
            return self.nav_rail(ui);
        }
        self.brand(ui);
        ui.add_space(10.0);
        self.device_picker(ui);
        ui.add_space(4.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for (group, ids) in PanelId::GROUPS {
                ui.add_space(6.0);
                // Sentence case, like the section headers of a macOS sidebar.
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new(group).size(11.5).family(theme::family_semibold()).color(theme::pal(ui).text3));
                });
                ui.add_space(2.0);
                for &id in ids {
                    self.nav_item(ui, id);
                }
            }
        });
    }

    /// Icon-only navigation used when the window is narrow.
    fn nav_rail(&mut self, ui: &mut Ui) {
        ui.vertical_centered(logo);
        ui.add_space(10.0);
        self.device_picker(ui);
        ui.add_space(6.0);
        // Settings and About stay at the bottom; only the panel list scrolls.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.add_space(2.0);
            for id in PanelId::FOOTER.into_iter().rev() {
                self.nav_item(ui, id);
            }
            ui.add_space(3.0);
            kit::hsep(ui);
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (_, ids) in PanelId::GROUPS {
                        ui.add_space(4.0);
                        kit::hsep(ui);
                        ui.add_space(4.0);
                        for &id in ids {
                            self.nav_item(ui, id);
                        }
                    }
                });
            });
        });
    }

    fn brand(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            logo(ui);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(kit::semibold("ADB Deck").size(15.0));
                ui.label(RichText::new("Android toolkit").size(11.0).color(p.text3));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for id in PanelId::FOOTER.into_iter().rev() {
                    if kit::Btn::new("").icon(id.icon()).subtle().selected(self.panel == id).tip(id.label()).show(ui).clicked() {
                        self.panel = id;
                        if self.data.last_panel != id.key() {
                            self.data.last_panel = id.key().to_string();
                            self.data_dirty = true;
                        }
                    }
                }
            });
        });
    }

    fn nav_item(&mut self, ui: &mut Ui, id: PanelId) {
        use egui::{pos2, vec2, Align2, FontId, Sense};
        let p = theme::pal(ui);
        let selected = self.panel == id;
        let compact = is_compact(ui.ctx());
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 31.0), Sense::click());
        let resp = if compact { resp.on_hover_text(id.label()) } else { resp };
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, egui::CornerRadius::same(8), p.accent_soft);
        } else if resp.hovered() {
            painter.rect_filled(rect, egui::CornerRadius::same(8), p.hover);
        }
        let (icon_color, text_color) = if selected { (p.accent_text, p.text) } else { (p.text2, p.text) };
        let icon_x = if compact { rect.center().x } else { rect.left() + 22.0 };
        painter.text(pos2(icon_x, rect.center().y), Align2::CENTER_CENTER, id.icon(), FontId::proportional(18.0), icon_color);
        if !compact {
            let family = if selected { theme::family_semibold() } else { theme::family_medium() };
            painter.text(pos2(rect.left() + 40.0, rect.center().y), Align2::LEFT_CENTER, id.label(), FontId::new(13.5, family), text_color);
        }
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, id.label()));
        if resp.clicked() {
            self.panel = id;
            if self.data.last_panel != id.key() {
                self.data.last_panel = id.key().to_string();
                self.data_dirty = true;
            }
        }
    }

    fn device_picker(&mut self, ui: &mut Ui) {
        use egui::{vec2, Align, Layout, Sense};
        let p = theme::pal(ui);
        let compact = is_compact(ui.ctx());
        let sel = self.selected.as_ref().and_then(|id| self.devices.iter().find(|d| &d.id == id)).cloned();
        let (title, sub, icon, kind) = match (&sel, &self.selected) {
            (Some(d), _) => {
                let name = if d.name.is_empty() { d.id.clone() } else { d.name.clone() };
                let mut sub = d.id.clone();
                if !d.android_version.is_empty() {
                    sub = format!("Android {} · {}", d.android_version, d.id);
                }
                (name, sub, device_icon(d), kit::Kind::Success)
            }
            (None, Some(id)) => ("Disconnected".to_string(), id.clone(), kit::ph::PLUGS, kit::Kind::Danger),
            (None, None) if self.adb.is_none() => {
                ("adb not found".to_string(), "Install adb to begin".to_string(), kit::ph::WARNING, kit::Kind::Warning)
            }
            (None, None) if self.devices_loaded => {
                ("No device".to_string(), "Connect one over USB or Wi-Fi".to_string(), kit::ph::DEVICE_MOBILE, kit::Kind::Neutral)
            }
            (None, None) => ("Looking for devices".to_string(), "Waiting for adb…".to_string(), kit::ph::DEVICE_MOBILE, kit::Kind::Neutral),
        };
        let frame = egui::Frame::new()
            .fill(p.card)
            .stroke(egui::Stroke::new(1.0_f32, p.stroke))
            .corner_radius(egui::CornerRadius::same(theme::RADIUS_CARD))
            .inner_margin(if compact { egui::Margin::symmetric(4, 6) } else { egui::Margin::symmetric(10, 9) })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if compact {
                    ui.vertical_centered(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                        ui.painter().rect_filled(rect, egui::CornerRadius::same(9), kind.soft(p));
                        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(19.0), kind.fg(p));
                    });
                    return;
                }
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                    ui.painter().rect_filled(rect, egui::CornerRadius::same(9), kind.soft(p));
                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(19.0), kind.fg(p));
                    let w = (ui.available_width() - 22.0).max(40.0);
                    ui.allocate_ui_with_layout(vec2(w, 34.0), Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add(egui::Label::new(kit::semibold(title.as_str()).size(13.5)).truncate());
                        ui.add(egui::Label::new(RichText::new(sub.as_str()).size(11.5).color(p.text2)).truncate());
                    });
                    ui.label(RichText::new(kit::ph::CARET_UP_DOWN).size(14.0).color(p.text3));
                });
            });
        let resp = ui.interact(frame.response.rect, ui.id().with("device-card"), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(frame.response.rect, egui::CornerRadius::same(theme::RADIUS_CARD), p.hover);
        }
        let resp = if compact { resp.on_hover_text(format!("{title}\n{sub}")) } else { resp };
        let popup_id = ui.make_persistent_id("device-popup");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        let mut choose: Option<String> = None;
        let mut action: Option<PanelId> = None;
        let mut refresh = false;
        let mut show_qr = false;
        let width = if compact { 250.0 } else { resp.rect.width() };
        egui::popup_below_widget(ui, popup_id, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
            ui.set_min_width(width - 16.0);
            ui.set_max_width(width - 16.0);
            if self.devices.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new("No devices attached").color(p.text2));
                ui.add_space(4.0);
            }
            for d in &self.devices {
                let usable = d.state == DeviceState::Online;
                let selected = self.selected.as_deref() == Some(d.id.as_str());
                let resp = device_row(ui, d, selected, usable);
                if resp.clicked() && usable {
                    choose = Some(d.id.clone());
                }
                if !usable {
                    resp.on_hover_text(device_state_hint(&d.state));
                }
            }
            ui.add_space(2.0);
            kit::hsep(ui);
            ui.add_space(2.0);
            if kit::Btn::new("Refresh").icon(kit::ph::ARROWS_CLOCKWISE).subtle().show(ui).clicked() {
                refresh = true;
            }
            if kit::Btn::new("Connect over Wi-Fi…").icon(kit::ph::WIFI_HIGH).subtle().show(ui).clicked() {
                action = Some(PanelId::Devices);
            }
            if kit::Btn::new("Connect with QR code…").icon(kit::ph::QR_CODE).subtle().show(ui).clicked() {
                action = Some(PanelId::Devices);
                show_qr = true;
            }
        });
        if self.refresh.busy() {
            ui.ctx().request_repaint();
        }
        if refresh {
            self.refresh_now();
        }
        if let Some(a) = action {
            self.panel = a;
        }
        if show_qr {
            self.global.devices.request_qr();
        }
        if let Some(id) = choose {
            self.selected = Some(id);
            self.reconcile_selection();
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let p = theme::pal(ui);
        ui.horizontal_centered(|ui| {
            let (kind, text) = match (&self.adb, self.selected_device()) {
                (None, _) => (kit::Kind::Danger, "adb not found".to_string()),
                (Some(_), _) if self.reconnecting.is_some() => {
                    let (id, attempt, of) = self.reconnecting.clone().unwrap_or_default();
                    (kit::Kind::Warning, format!("Reconnecting to {id} (attempt {attempt} of {of})…"))
                }
                (Some(_), Some(d)) => (kit::Kind::Success, format!("Connected to {}", device_label(&d))),
                (Some(_), None) if self.selected.is_some() => (kit::Kind::Danger, "Device disconnected".to_string()),
                (Some(_), None) => (kit::Kind::Neutral, "No device selected".to_string()),
            };
            let left_w = (ui.available_width() - 300.0).max(120.0);
            ui.allocate_ui_with_layout(egui::vec2(left_w, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                kit::dot(ui, kind);
                ui.add(egui::Label::new(RichText::new(text).size(12.0).color(p.text2)).truncate());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for s in self.status.iter().rev() {
                    let (kind, tip) = match &s.path {
                        Some(path) => (kit::Kind::Success, format!("{}: {}", s.tool.name(), path.display())),
                        None if s.tool.required() => (kit::Kind::Danger, format!("{} is required and was not found", s.tool.name())),
                        None => (kit::Kind::Neutral, format!("{} not found (optional): {}", s.tool.name(), s.tool.needed_for())),
                    };
                    let r = ui
                        .horizontal(|ui| {
                            ui.label(RichText::new(s.tool.name()).size(12.0).color(p.text3));
                            kit::dot(ui, kind);
                        })
                        .response;
                    r.on_hover_text(tip);
                    ui.add_space(8.0);
                }
            });
        });
    }

    /// Transient messages in the bottom-right corner.
    fn toasts(&mut self, ctx: &egui::Context) {
        let p = theme::palette(ctx);
        let mut close: Vec<usize> = Vec::new();
        let visible: Vec<usize> = self
            .notes
            .items
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                !n.dismissed && n.at.elapsed() < if n.kind == NoteKind::Error { Duration::from_secs(14) } else { Duration::from_secs(6) }
            })
            .map(|(i, _)| i)
            .rev()
            .take(3)
            .collect();
        if visible.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts")).anchor(egui::Align2::RIGHT_BOTTOM, [-18.0, -46.0]).order(egui::Order::Foreground).show(
            ctx,
            |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for &i in visible.iter().rev() {
                    let n = &self.notes.items[i];
                    let kind = if n.kind == NoteKind::Error { kit::Kind::Danger } else { kit::Kind::Accent };
                    egui::Frame::new()
                        .fill(p.card)
                        .stroke(egui::Stroke::new(1.0_f32, p.stroke_strong))
                        .corner_radius(egui::CornerRadius::same(10))
                        .shadow(ctx.style().visuals.popup_shadow)
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width(340.0);
                            ui.horizontal_top(|ui| {
                                ui.label(
                                    RichText::new(if n.kind == NoteKind::Error { kit::ph::WARNING_CIRCLE } else { kit::ph::INFO })
                                        .size(18.0)
                                        .color(kind.fg(p)),
                                );
                                ui.add_sized([280.0, 0.0], egui::Label::new(RichText::new(&n.text).color(p.text)).wrap());
                                if kit::icon_btn(ui, kit::ph::X, "Dismiss").clicked() {
                                    close.push(i);
                                }
                            });
                        });
                }
            },
        );
        for i in close {
            if let Some(n) = self.notes.items.get_mut(i) {
                n.dismissed = true;
            }
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn central(&mut self, ui: &mut Ui) {
        let panel = self.panel;
        kit::page_header(ui, panel.label(), panel.subtitle(), |_| {});
        if panel != PanelId::About && panel != PanelId::Settings && self.adb.is_none() {
            // Settings and About must stay reachable without adb.
            if self.missing_adb(ui) {
                self.panel = PanelId::Settings;
            }
            return;
        }
        let ctx = ui.ctx().clone();
        let device = self.selected_device();
        let missing_selected = self.selected.clone().filter(|_| device.is_none());
        let App {
            sp,
            adb,
            data,
            settings,
            tools,
            notes,
            devices,
            data_dirty,
            dp,
            global,
            pending_apks,
            status,
            device_error,
            suppressed,
            ..
        } = self;
        let mut env = Env { sp, adb: adb.as_ref(), data, settings, tools, notes, devices, data_dirty, suppressed };
        let mut redetect = false;
        let mut new_settings: Option<Settings> = None;

        match panel {
            PanelId::Devices => global.devices.ui(ui, &mut env, device.as_ref()),
            PanelId::VirtualDevices => global.virtual_devices.ui(ui, &mut env),
            PanelId::Signing => global.signing.ui(ui, &mut env),
            PanelId::Settings => new_settings = global.settings.ui(ui, &mut env),
            PanelId::About => redetect = global.settings.about(ui, status),
            p if p.needs_device() => {
                let Some(device) = device else {
                    match no_device(ui, missing_selected.as_deref(), device_error.as_deref()) {
                        Some(Connect::Wifi) => self.panel = PanelId::Devices,
                        Some(Connect::Qr) => {
                            self.panel = PanelId::Devices;
                            global.devices.request_qr();
                        }
                        None => {}
                    }
                    return;
                };
                let Some(adb) = env.adb else { return };
                let d = DeviceCtx { dev: adb.device(&device.id), device };
                if p == PanelId::Mirror {
                    global.mirror.ui(ui, &mut env, &d);
                } else if let Some(dp) = dp.as_mut() {
                    match p {
                        PanelId::Overview => dp.overview.ui(ui, &mut env, &d),
                        PanelId::Files => dp.files.ui(ui, &mut env, &d),
                        PanelId::Apps => dp.apps.ui(ui, &mut env, &d, pending_apks),
                        PanelId::Processes => dp.processes.ui(ui, &mut env, &d),
                        PanelId::Performance => dp.perf.ui(ui, &mut env, &d),
                        PanelId::Controls => dp.controls.ui(ui, &mut env, &d),
                        PanelId::Shell => dp.shell.ui(ui, &mut env, &d),
                        PanelId::Layout => dp.layout.ui(ui, &mut env, &d),
                        PanelId::Screenshot => dp.screenshot.ui(ui, &mut env, &d),
                        PanelId::Logcat => dp.logcat.ui(ui, &mut env, &d),
                        PanelId::Capture => dp.capture.ui(ui, &mut env, &d),
                        PanelId::Webview => dp.webview.ui(ui, &mut env, &d),
                        PanelId::Remote => dp.remote.ui(ui, &mut env, &d),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        if let Some(new) = new_settings {
            self.settings = new;
            self.save_settings();
            self.sync_background_service();
            self.global.settings.reset_draft();
            redetect = true;
        }
        if redetect {
            self.redetect_tools(&ctx);
        }
    }

    /// Returns true when the user asked to open the settings.
    fn missing_adb(&mut self, ui: &mut Ui) -> bool {
        let mut open_settings = false;
        let p = theme::pal(ui);
        kit::empty_state(
            ui,
            kit::ph::WARNING,
            "adb was not found",
            "Without adb, ADB Deck cannot list or control devices. Install it, or point ADB Deck to an existing adb in Settings.",
            |ui| {
                if kit::Btn::new("Check again").icon(kit::ph::ARROWS_CLOCKWISE).primary().show(ui).clicked() {
                    let ctx = ui.ctx().clone();
                    self.redetect_tools(&ctx);
                }
                if kit::Btn::new("Open settings").icon(kit::ph::GEAR).show(ui).clicked() {
                    open_settings = true;
                }
            },
        );
        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            egui::Frame::new()
                .fill(p.terminal_bg)
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.label(RichText::new(Tool::Adb.install_hint()).monospace().color(egui::Color32::from_rgb(0xD7, 0xDA, 0xE0)));
                });
        });
        open_settings
    }
}

fn device_icon(d: &Device) -> &'static str {
    if d.is_emulator() {
        kit::ph::DESKTOP
    } else if d.is_remote() {
        kit::ph::WIFI_HIGH
    } else {
        kit::ph::USB
    }
}

fn device_row(ui: &mut Ui, d: &Device, selected: bool, usable: bool) -> egui::Response {
    use egui::{pos2, vec2, Align2, FontId, Sense};
    let p = theme::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(7), p.accent_soft);
    } else if resp.hovered() && usable {
        ui.painter().rect_filled(rect, egui::CornerRadius::same(7), p.hover);
    }
    let dim = if usable { 1.0 } else { 0.55 };
    let icon_col = p.accent_text.gamma_multiply(dim);
    ui.painter().text(
        pos2(rect.left() + 20.0, rect.center().y),
        Align2::CENTER_CENTER,
        device_icon(d),
        FontId::proportional(18.0),
        icon_col,
    );
    let name = if d.name.is_empty() { d.id.as_str() } else { d.name.as_str() };
    ui.painter().text(
        pos2(rect.left() + 40.0, rect.center().y - 8.0),
        Align2::LEFT_CENTER,
        name,
        FontId::new(13.5, theme::family_medium()),
        p.text.gamma_multiply(dim),
    );
    let sub = if usable { d.id.clone() } else { format!("{} · {}", d.id, d.state.label()) };
    ui.painter().text(
        pos2(rect.left() + 40.0, rect.center().y + 9.0),
        Align2::LEFT_CENTER,
        sub,
        FontId::proportional(11.5),
        p.text2.gamma_multiply(dim),
    );
    if selected {
        ui.painter().text(
            pos2(rect.right() - 14.0, rect.center().y),
            Align2::CENTER_CENTER,
            kit::ph::CHECK,
            FontId::proportional(16.0),
            p.accent_text,
        );
    }
    if usable {
        resp
    } else {
        resp.on_hover_cursor(egui::CursorIcon::NotAllowed)
    }
}

fn device_label(d: &Device) -> String {
    let name = if d.name.is_empty() { d.id.as_str() } else { d.name.as_str() };
    if d.state == DeviceState::Online {
        format!("{name} ({})", d.id)
    } else {
        format!("{name} ({}) - {}", d.id, d.state.label())
    }
}

fn device_state_hint(state: &DeviceState) -> &'static str {
    match state {
        DeviceState::Unauthorized => "Accept the USB debugging prompt on the device.",
        DeviceState::Offline => "The device is offline. Reconnect the cable or restart adb.",
        DeviceState::NoPermissions => "adb has no permission to use this USB device (check udev rules).",
        DeviceState::Connecting => "Still connecting...",
        _ => "This device cannot be used right now.",
    }
}

/// How the user chose to connect a device from the "no device" screen.
enum Connect {
    Wifi,
    Qr,
}

/// Explains why there is no usable device and offers the ways to connect one.
fn no_device(ui: &mut Ui, lost: Option<&str>, error: Option<&str>) -> Option<Connect> {
    let mut goto = None;
    let (icon, title, detail) = match (lost, error) {
        (Some(id), _) => (
            kit::ph::PLUGS,
            "Device disconnected".to_string(),
            format!("{id} is no longer available. Reconnect it or choose another device from the sidebar."),
        ),
        (None, Some(e)) => (kit::ph::WARNING, "adb reported a problem".to_string(), e.to_string()),
        (None, None) => (
            kit::ph::DEVICE_MOBILE,
            "No device selected".to_string(),
            "Connect a device with USB debugging enabled, accept the prompt on the device, or add one over Wi-Fi or with a QR code.".to_string(),
        ),
    };
    kit::empty_state(ui, icon, &title, &detail, |ui| {
        if kit::Btn::new("Connect over Wi-Fi").icon(kit::ph::WIFI_HIGH).primary().show(ui).clicked() {
            goto = Some(Connect::Wifi);
        }
        if kit::Btn::new("Connect with QR code").icon(kit::ph::QR_CODE).primary().show(ui).clicked() {
            goto = Some(Connect::Qr);
        }
    });
    goto
}

/// Windows narrower than this get the icon-only navigation rail.
const COMPACT_BELOW: f32 = 1100.0;

/// Seconds since the Unix epoch; 0 if the clock is set before it.
fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn is_compact(ctx: &egui::Context) -> bool {
    ctx.screen_rect().width() < COMPACT_BELOW
}

/// The accent-coloured app mark.
fn logo(ui: &mut Ui) {
    let p = theme::pal(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, egui::CornerRadius::same(8), p.accent);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, kit::ph::ANDROID_LOGO, egui::FontId::proportional(19.0), p.on_accent);
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
        self.poll_devices();
        let mirrored = self.dp.as_ref().map(|d| d.serial.as_str());
        self.global.mirror.sync_device(mirrored, self.panel == PanelId::Mirror);
        self.handle_dropped_files(ctx);

        let p = theme::palette(ctx);
        let compact = is_compact(ctx);
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(if compact { 64.0 } else { 252.0 })
            .frame(egui::Frame::new().fill(p.sidebar).inner_margin(egui::Margin {
                left: if compact { 8 } else { 14 },
                right: if compact { 8 } else { 14 },
                top: 16,
                bottom: 10,
            }))
            .show(ctx, |ui| self.nav(ui));
        egui::TopBottomPanel::bottom("status")
            .exact_height(30.0)
            .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::symmetric(18, 0)))
            .show(ctx, |ui| self.status_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin { left: 28, right: 28, top: 22, bottom: 12 }))
            .show(ctx, |ui| self.central(ui));
        self.toasts(ctx);

        self.save_if_dirty();
        // Low-rate poll so results and timers are noticed without input.
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Dropping panels cancels their jobs and kills child processes.
        self.dp = None;
        self.global.mirror.shutdown(&self.rt);
        self.save_if_dirty();
        if let Some(w) = self.watcher.take() {
            w.abort();
        }
        // The background service needs the adb server to keep the connections it made.
        if self.settings.kill_adb_on_exit && !self.background_service_wanted() {
            if let Some(adb) = self.adb.clone() {
                self.rt.block_on(async {
                    let _ = tokio::time::timeout(Duration::from_secs(3), adb.kill_server()).await;
                });
            }
        }
    }
}

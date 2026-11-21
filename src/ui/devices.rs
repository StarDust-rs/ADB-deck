//! Connections: wireless adb, pairing, remembered devices and port mappings.

use egui::{RichText, Ui};

use adbdeck::adb::connect::{endpoint, parse_port};
use adbdeck::adb::devices::{is_remote_id, suppress, Device};
use adbdeck::adb::ports::PortMapping;
use adbdeck::error::Error;
use adbdeck::jobs::Slot;
use adbdeck::store::{RemoteDevice, ENDPOINTS_MAX};

use super::widgets;
use super::{kit, theme, Env};

type Ports = (Vec<PortMapping>, Vec<PortMapping>);

#[derive(Default)]
pub struct Connections {
    host: String,
    port: String,
    pair_host: String,
    pair_port: String,
    pair_code: String,
    op: Slot<String>,
    ports: Slot<Ports>,
    port_data: Option<Ports>,
    port_error: Option<String>,
    ports_for: Option<String>,
    fwd_local: String,
    fwd_remote: String,
    rev_remote: String,
    rev_local: String,
}

impl Connections {
    fn reload_ports(&mut self, env: &Env, device: &Device) {
        let Some(adb) = env.adb else { return };
        let dev = adb.device(&device.id);
        self.ports_for = Some(device.id.clone());
        self.port_error = None;
        self.ports.set(env.sp.spawn(async move { Ok((dev.list_forwards().await?, dev.list_reverses().await?)) }));
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env, device: Option<&Device>) {
        let Some(adb) = env.adb.cloned() else {
            kit::empty_state(ui, kit::ph::WARNING, "adb is not available", "Install adb or set its path under Settings.", |_| {});
            return;
        };

        if let Some(r) = self.op.poll() {
            if let Some(msg) = env.notes.report("Connection", r) {
                env.notes.info(msg);
            }
            // Port mappings change through `op`; read them again only once it finished.
            self.ports_for = None;
        }
        if let Some(r) = self.ports.poll() {
            match r {
                Ok(p) => self.port_data = Some(p),
                Err(e) => self.port_error = Some(e.to_string()),
            }
        }
        let current = device.map(|d| d.id.clone());
        if self.ports_for != current {
            // A listing still loading belongs to the previous device or predates a change.
            self.ports.cancel();
            self.port_data = None;
            match device {
                Some(d) => self.reload_ports(env, d),
                None => self.ports_for = None,
            }
        }

        let p = theme::pal(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let busy = self.op.busy();
            // Side by side when there is room, stacked otherwise.
            let mut top_card = |ui: &mut Ui, which: usize| match which {
                0 => {
                    kit::card_titled(ui, "Connect over Wi-Fi", |ui| {
                        ui.label(
                            RichText::new("Enter the address shown under Wireless debugging, or the IP of a device in tcpip mode.")
                                .size(12.5)
                                .color(p.text2),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            kit::text_field(ui, &mut self.host, "192.168.1.20", 140.0);
                            kit::text_field(ui, &mut self.port, "5555", 64.0);
                            let target = parse_target(&self.host, &self.port);
                            if kit::Btn::new("Connect").primary().enabled(!busy && target.is_ok()).show(ui).clicked() {
                                if let Ok((host, port)) = target {
                                    // Asking for the connection again undoes an earlier "Disconnect".
                                    if let Ok(id) = endpoint(&host, port) {
                                        set_auto_connect(env, &id, true);
                                    }
                                    let (adb, suppressed) = (adb.clone(), env.suppressed.clone());
                                    self.op.set(env.sp.spawn(async move {
                                        let id = endpoint(&host, port)?;
                                        suppress(&suppressed, &id, false);
                                        adb.connect(&host, port).await?;
                                        Ok(format!("Connected to {id}"))
                                    }));
                                }
                            }
                            if busy {
                                ui.add(egui::Spinner::new().size(16.0));
                            }
                        });
                        if let (false, Err(e)) = (self.host.trim().is_empty(), parse_target(&self.host, &self.port)) {
                            ui.add_space(4.0);
                            ui.label(RichText::new(e.to_string()).size(12.0).color(p.warning));
                        }
                    });
                }
                _ => {
                    kit::card_titled(ui, "Switch a USB device to Wi-Fi", |ui| {
                        ui.label(
                            RichText::new("Enables TCP/IP debugging, connects, and lets you unplug the cable.").size(12.5).color(p.text2),
                        );
                        ui.add_space(6.0);
                        match device.filter(|d| !d.is_remote()) {
                            Some(d) => {
                                if kit::Btn::new(&format!("Enable on {}", d.id)).icon(kit::ph::WIFI_HIGH).enabled(!busy).show(ui).clicked()
                                {
                                    let (adb, serial) = (adb.clone(), d.id.clone());
                                    self.op.set(env.sp.spawn(async move {
                                        let target = adb.enable_wireless(&serial).await?;
                                        Ok(format!("Wi-Fi debugging enabled; connected to {target}. You can unplug the cable."))
                                    }));
                                }
                            }
                            None => {
                                ui.label(RichText::new("Select a USB-connected device first.").color(p.text3));
                            }
                        }
                    });
                }
            };
            if ui.available_width() >= 760.0 {
                ui.columns(2, |cols| {
                    top_card(&mut cols[0], 0);
                    top_card(&mut cols[1], 1);
                });
            } else {
                top_card(ui, 0);
                ui.add_space(12.0);
                top_card(ui, 1);
            }
            ui.add_space(12.0);

            kit::card_titled(ui, "Pair with a code (Android 11+)", |ui| {
                ui.label(
                    RichText::new("On the device: Developer options › Wireless debugging › Pair device with pairing code.")
                        .size(12.5)
                        .color(p.text2),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    kit::text_field(ui, &mut self.pair_host, "host", 160.0);
                    kit::text_field(ui, &mut self.pair_port, "pairing port", 110.0);
                    kit::text_field_mono(ui, &mut self.pair_code, "6-digit code", 110.0);
                    let port = parse_port(&self.pair_port).ok().flatten();
                    let ready = !self.pair_host.trim().is_empty() && port.is_some() && self.pair_code.trim().len() == 6;
                    if kit::Btn::new("Pair").icon(kit::ph::LINK).primary().enabled(!busy && ready).show(ui).clicked() {
                        if let Some(port) = port {
                            let (adb, host, code) = (adb.clone(), self.pair_host.clone(), self.pair_code.clone());
                            self.pair_code.clear();
                            self.op.set(env.sp.spawn(async move {
                                adb.pair(&host, port, &code).await?;
                                Ok("Paired. Now connect using the address shown on the Wireless debugging screen.".to_string())
                            }));
                        }
                    }
                });
            });
            ui.add_space(12.0);

            kit::card_titled(ui, "Remembered Wi-Fi devices", |ui| {
                if env.data.remote_devices.is_empty() {
                    ui.label(
                        RichText::new("Phones you connect over Wi-Fi are remembered here and reconnected automatically, even when ADB Deck is closed.")
                            .color(p.text3),
                    );
                }
                let now = super::unix_now();
                let mut forget: Option<usize> = None;
                let mut connect: Option<String> = None;
                let mut disconnected: Option<String> = None;
                for (i, r) in env.data.remote_devices.iter().enumerate() {
                    let online = env.devices.iter().any(|d| d.id == r.id && d.is_online());
                    if i > 0 {
                        kit::hsep(ui);
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        kit::dot(ui, if online { kit::Kind::Success } else { kit::Kind::Neutral });
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(kit::medium(if r.name.is_empty() { &r.id } else { &r.name }));
                            ui.label(RichText::new(&r.id).monospace().size(11.5).color(p.text3));
                            if !r.auto_connect {
                                ui.label(RichText::new("Not reconnected automatically").size(11.5).color(p.warning));
                            } else if !online && r.last_seen > 0 {
                                ui.label(RichText::new(format!("Last seen {}", ago(now, r.last_seen))).size(11.5).color(p.text3));
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // A connected device is remembered again on the next refresh;
                            // disconnect it first.
                            let tip = if online { "Disconnect before forgetting this device" } else { "Forget this device" };
                            if kit::Btn::new("").icon(kit::ph::TRASH).subtle().enabled(!online).tip(tip).show(ui).clicked() {
                                forget = Some(i);
                            }
                            if online {
                                if kit::Btn::new("Disconnect").enabled(!busy).show(ui).clicked() {
                                    disconnected = Some(r.id.clone());
                                    let (adb, id, suppressed) = (adb.clone(), r.id.clone(), env.suppressed.clone());
                                    self.op.set(env.sp.spawn(async move {
                                        let (host, port) = split_endpoint(&id)?;
                                        // Otherwise the watcher would bring it straight back.
                                        suppress(&suppressed, &id, true);
                                        adb.disconnect(&host, port).await?;
                                        Ok(format!("Disconnected {id}"))
                                    }));
                                }
                            } else if kit::Btn::new("Connect").primary().enabled(!busy).show(ui).clicked() {
                                connect = Some(r.id.clone());
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
                // Saved, so that the background service leaves a phone alone that was disconnected on purpose.
                if let Some(id) = disconnected {
                    set_auto_connect(env, &id, false);
                }
                if let Some(id) = connect {
                    set_auto_connect(env, &id, true);
                    let (adb, suppressed) = (adb.clone(), env.suppressed.clone());
                    self.op.set(env.sp.spawn(async move {
                        let (host, port) = split_endpoint(&id)?;
                        suppress(&suppressed, &id, false);
                        adb.connect(&host, port).await?;
                        Ok(format!("Connected to {id}"))
                    }));
                }
                if let Some(i) = forget {
                    let gone = env.data.remote_devices.remove(i);
                    suppress(env.suppressed, &gone.id, true);
                    *env.data_dirty = true;
                }
            });
            ui.add_space(12.0);

            match device {
                Some(device) => self.port_ui(ui, env, device),
                None => {
                    kit::card_titled(ui, "Port mappings", |ui| {
                        ui.label(RichText::new("Select a device to manage its port mappings.").color(p.text3));
                    });
                }
            }
        });
    }

    fn port_ui(&mut self, ui: &mut Ui, env: &mut Env, device: &Device) {
        let Some(adb) = env.adb else { return };
        let dev = adb.device(&device.id);
        let p = theme::pal(ui);
        if let Some(e) = self.port_error.clone() {
            if widgets::error_banner(ui, &e, true) {
                self.reload_ports(env, device);
            }
            ui.add_space(8.0);
        }
        let busy = self.op.busy() || self.ports.busy();
        kit::card_titled(ui, "Port mappings", |ui| {
            ui.horizontal(|ui| {
                ui.add_sized([60.0, 28.0], egui::Label::new(kit::medium("Forward")));
                kit::text_field_mono(ui, &mut self.fwd_local, "tcp:8080", 140.0);
                ui.label(RichText::new(kit::ph::ARROW_RIGHT).color(p.text3));
                kit::text_field_mono(ui, &mut self.fwd_remote, "tcp:8080 or localabstract:name", 220.0);
                if kit::Btn::new("Add")
                    .icon(kit::ph::PLUS)
                    .enabled(!busy && !self.fwd_local.is_empty() && !self.fwd_remote.is_empty())
                    .show(ui)
                    .clicked()
                {
                    let (dev, l, r) = (dev.clone(), self.fwd_local.clone(), self.fwd_remote.clone());
                    self.op.set(env.sp.spawn(async move {
                        dev.forward(&l, &r).await?;
                        Ok(format!("Forwarding {l} to device {r}"))
                    }));
                }
            });
            ui.horizontal(|ui| {
                ui.add_sized([60.0, 28.0], egui::Label::new(kit::medium("Reverse")));
                kit::text_field_mono(ui, &mut self.rev_remote, "tcp:9000", 140.0);
                ui.label(RichText::new(kit::ph::ARROW_RIGHT).color(p.text3));
                kit::text_field_mono(ui, &mut self.rev_local, "tcp:9000", 220.0);
                if kit::Btn::new("Add")
                    .icon(kit::ph::PLUS)
                    .enabled(!busy && !self.rev_remote.is_empty() && !self.rev_local.is_empty())
                    .show(ui)
                    .clicked()
                {
                    let (dev, r, l) = (dev.clone(), self.rev_remote.clone(), self.rev_local.clone());
                    self.op.set(env.sp.spawn(async move {
                        dev.reverse(&r, &l).await?;
                        Ok(format!("Device {r} now reaches host {l}"))
                    }));
                }
            });
            ui.add_space(8.0);
            let Some((forwards, reverses)) = self.port_data.as_ref() else {
                if self.ports.busy() {
                    kit::busy(ui, "Reading port mappings…");
                }
                return;
            };
            let mut remove: Option<(bool, String)> = None;
            for (title, list, is_forward) in [("Forwards (host to device)", forwards, true), ("Reverses (device to host)", reverses, false)]
            {
                kit::hsep(ui);
                ui.add_space(4.0);
                ui.label(RichText::new(title).size(12.0).color(p.text2));
                if list.is_empty() {
                    ui.label(RichText::new("none").color(p.text3));
                }
                for m in list {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}  →  {}", m.local, m.remote)).monospace().size(12.5));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::icon_btn(ui, kit::ph::X, "Remove this mapping").clicked() && !busy {
                                remove = Some((is_forward, if is_forward { m.local.clone() } else { m.remote.clone() }));
                            }
                        });
                    });
                }
            }
            if let Some((is_forward, endpoint)) = remove {
                let dev = dev.clone();
                self.op.set(env.sp.spawn(async move {
                    if is_forward {
                        dev.remove_forward_endpoint(&endpoint).await?;
                    } else {
                        dev.remove_reverse(&endpoint).await?;
                    }
                    Ok(format!("Removed {endpoint}"))
                }));
            }
        });
    }
}

type Target = (String, Option<u16>);

fn parse_target(host: &str, port: &str) -> Result<Target, Error> {
    let port = parse_port(port)?;
    endpoint(host, port)?;
    Ok((host.trim().to_string(), port))
}

fn split_endpoint(id: &str) -> Result<Target, Error> {
    match id.rsplit_once(':') {
        Some((h, p)) => Ok((h.to_string(), parse_port(p)?)),
        None => Ok((id.to_string(), None)),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 d ago" for a Unix time in the past.
fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=89 => "just now".into(),
        90..=5399 => format!("{} min ago", (secs + 30) / 60),
        5400..=129_599 => format!("{} h ago", (secs + 1800) / 3600),
        _ => format!("{} d ago", (secs + 43_200) / 86_400),
    }
}

/// `last_seen` is refreshed at most this often, so a phone that stays connected does not
/// rewrite the data file on every device refresh.
const LAST_SEEN_STEP_SECS: u64 = 3600;

/// Brings a remembered phone up to date with what was just seen. The phone's own
/// details are only replaced when they were read (a failed `getprop` leaves them empty).
fn refresh_entry(entry: &mut RemoteDevice, d: &Device, now: u64) {
    entry.id = d.id.clone();
    if !d.serialno.is_empty() {
        entry.name = d.name.clone();
        entry.serialno = d.serialno.clone();
        entry.android_version = d.android_version.clone();
        entry.sdk_version = d.sdk_version.clone();
    } else if entry.name.is_empty() {
        entry.name = d.name.clone();
    }
    entry.endpoints.retain(|e| e != &d.id);
    entry.endpoints.insert(0, d.id.clone());
    entry.endpoints.truncate(ENDPOINTS_MAX);
    if now.saturating_sub(entry.last_seen) >= LAST_SEEN_STEP_SECS {
        entry.last_seen = now;
    }
}

/// Remembers Wi-Fi devices that are currently connected, with every address they
/// were reached at. Wireless debugging picks a new port each time it is switched on,
/// so a phone seen again under a new address updates its entry instead of adding a
/// second one. `now` is the Unix time in seconds.
pub fn remember_remote(data: &mut adbdeck::store::AppData, devices: &[Device], now: u64) -> bool {
    let mut changed = false;
    // Only `ip:port` ids can be connected to again later; mDNS names are rediscovered.
    for d in devices.iter().filter(|d| is_remote_id(&d.id) && d.is_online()) {
        let same_phone = |r: &RemoteDevice| r.id == d.id || (!d.serialno.is_empty() && r.serialno == d.serialno);
        match data.remote_devices.iter_mut().find(|r| same_phone(r)) {
            Some(existing) => {
                let mut updated = existing.clone();
                refresh_entry(&mut updated, d, now);
                if updated != *existing {
                    *existing = updated;
                    changed = true;
                }
            }
            None => {
                let mut entry = RemoteDevice::default();
                refresh_entry(&mut entry, d, now);
                data.remote_devices.push(entry);
                changed = true;
            }
        }
    }
    changed
}

/// Switches automatic connection of one remembered phone on or off, which the
/// background service reads from the saved data.
fn set_auto_connect(env: &mut Env, id: &str, on: bool) {
    if let Some(r) = env.data.remote_devices.iter_mut().find(|r| r.id == id) {
        if r.auto_connect != on {
            r.auto_connect = on;
            *env.data_dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adbdeck::adb::devices::DeviceState;
    use adbdeck::store::AppData;

    fn phone(id: &str, serialno: &str, state: DeviceState) -> Device {
        Device {
            id: id.into(),
            name: "Pixel 7".into(),
            serialno: serialno.into(),
            android_version: "14".into(),
            sdk_version: "34".into(),
            state,
        }
    }

    #[test]
    fn a_phone_on_a_new_port_replaces_its_old_entry() {
        let mut data = AppData::default();
        assert!(remember_remote(&mut data, &[phone("192.168.1.20:37123", "R58M1234", DeviceState::Online)], NOW));
        assert!(remember_remote(&mut data, &[phone("192.168.1.20:41555", "R58M1234", DeviceState::Online)], NOW));
        let ids: Vec<_> = data.remote_devices.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["192.168.1.20:41555"], "one entry per phone, at its latest address");
        assert!(!remember_remote(&mut data, &[phone("192.168.1.20:41555", "R58M1234", DeviceState::Online)], NOW), "unchanged is not a change");
    }

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn last_seen_reads_like_a_status_line() {
        assert_eq!(ago(NOW, NOW), "just now");
        assert_eq!(ago(NOW, NOW + 100), "just now", "a clock set back is not an error");
        assert_eq!(ago(NOW + 120, NOW), "2 min ago");
        assert_eq!(ago(NOW + 3 * 3600, NOW), "3 h ago");
        assert_eq!(ago(NOW + 3 * 86_400, NOW), "3 d ago");
    }

    #[test]
    fn every_address_a_phone_was_reached_at_is_kept_newest_first_and_bounded() {
        let mut data = AppData::default();
        for port in 40001..40010 {
            remember_remote(&mut data, &[phone(&format!("192.168.1.20:{port}"), "R58M1234", DeviceState::Online)], NOW);
        }
        remember_remote(&mut data, &[phone("192.168.42.129:5555", "R58M1234", DeviceState::Online)], NOW);
        let r = &data.remote_devices[0];
        assert_eq!(data.remote_devices.len(), 1);
        assert_eq!(r.endpoints.len(), adbdeck::store::ENDPOINTS_MAX);
        assert_eq!(r.endpoints[0], "192.168.42.129:5555", "the tethering address is the newest");
        assert_eq!(r.endpoints[1], "192.168.1.20:40009");
        assert_eq!(r.id, r.endpoints[0]);
    }

    #[test]
    fn the_time_last_seen_moves_only_after_an_hour() {
        let mut data = AppData::default();
        let online = [phone("192.168.1.20:5555", "R58M1234", DeviceState::Online)];
        assert!(remember_remote(&mut data, &online, NOW));
        assert_eq!(data.remote_devices[0].last_seen, NOW);
        assert!(!remember_remote(&mut data, &online, NOW + 60), "a minute later nothing needs saving");
        assert!(remember_remote(&mut data, &online, NOW + LAST_SEEN_STEP_SECS));
        assert_eq!(data.remote_devices[0].last_seen, NOW + LAST_SEEN_STEP_SECS);
    }

    #[test]
    fn a_failed_property_read_does_not_erase_what_is_known() {
        let mut data = AppData::default();
        remember_remote(&mut data, &[phone("192.168.1.20:5555", "R58M1234", DeviceState::Online)], NOW);
        let mut unread = phone("192.168.1.20:5555", "", DeviceState::Online);
        unread.name = unread.id.clone();
        remember_remote(&mut data, &[unread], NOW);
        let r = &data.remote_devices[0];
        assert_eq!((r.name.as_str(), r.serialno.as_str(), r.android_version.as_str()), ("Pixel 7", "R58M1234", "14"));
    }

    #[test]
    fn switching_off_automatic_connection_survives_new_sightings() {
        let mut data = AppData::default();
        remember_remote(&mut data, &[phone("192.168.1.20:5555", "R58M1234", DeviceState::Online)], NOW);
        data.remote_devices[0].auto_connect = false;
        remember_remote(&mut data, &[phone("192.168.1.20:41555", "R58M1234", DeviceState::Online)], NOW + 5000);
        assert!(!data.remote_devices[0].auto_connect, "only the user switches it back on");
    }

    #[test]
    fn different_phones_and_unknown_serials_stay_separate() {
        let mut data = AppData::default();
        remember_remote(
            &mut data,
            &[phone("192.168.1.20:5555", "AAA", DeviceState::Online), phone("192.168.1.31:5555", "BBB", DeviceState::Online)],
            NOW,
        );
        assert_eq!(data.remote_devices.len(), 2);
        // Without a serial number nothing says these are the same phone.
        remember_remote(
            &mut data,
            &[phone("192.168.1.40:5555", "", DeviceState::Online), phone("192.168.1.41:5555", "", DeviceState::Online)],
            NOW,
        );
        assert_eq!(data.remote_devices.len(), 4);
    }

    #[test]
    fn only_online_addresses_are_remembered() {
        let mut data = AppData::default();
        let devices = [
            phone("192.168.1.20:5555", "AAA", DeviceState::Offline),
            phone("adb-BBB-xyz._adb-tls-connect._tcp", "BBB", DeviceState::Online),
            phone("FAKE0001", "CCC", DeviceState::Online),
        ];
        assert!(!remember_remote(&mut data, &devices, NOW));
        assert!(data.remote_devices.is_empty());
    }
}

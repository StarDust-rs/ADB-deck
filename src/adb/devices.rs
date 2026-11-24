//! Device discovery: parsing `adb devices -l`, reading device properties and
//! following device changes.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

use super::discover::Discoverer;
use super::known::{local_gateways, KnownDevice, Prober};
use super::props::{display_name, parse_getprop};
use super::Adb;
use crate::error::{Error, Result};
use crate::process::{spawn_lines, CommandSpec};
use crate::retry::Backoff;
use crate::store::AppData;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceState {
    Online,
    Offline,
    Unauthorized,
    Connecting,
    NoPermissions,
    Other(String),
}

impl DeviceState {
    fn parse(word: &str) -> Self {
        match word {
            "device" => DeviceState::Online,
            "offline" => DeviceState::Offline,
            "unauthorized" => DeviceState::Unauthorized,
            "connecting" | "authorizing" => DeviceState::Connecting,
            "no permissions" | "no" => DeviceState::NoPermissions,
            other => DeviceState::Other(other.to_string()),
        }
    }

    pub fn label(&self) -> String {
        match self {
            DeviceState::Online => "Online".into(),
            DeviceState::Offline => "Offline".into(),
            DeviceState::Unauthorized => "Unauthorized".into(),
            DeviceState::Connecting => "Connecting".into(),
            DeviceState::NoPermissions => "No permissions".into(),
            DeviceState::Other(s) => s.clone(),
        }
    }
}

/// One row of `adb devices -l`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedDevice {
    pub serial: String,
    pub state: DeviceState,
}

pub fn parse_device_list(text: &str) -> Vec<ListedDevice> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('*') && !l.starts_with("List of devices"))
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?;
            let state = parts.next()?;
            // "no permissions (reason)" spans several words.
            let state = if state == "no" { "no permissions" } else { state };
            Some(ListedDevice { serial: serial.to_string(), state: DeviceState::parse(state) })
        })
        .collect()
}

/// A device as shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub serialno: String,
    pub android_version: String,
    pub sdk_version: String,
    pub state: DeviceState,
}

impl Device {
    pub fn is_online(&self) -> bool {
        self.state == DeviceState::Online
    }

    pub fn is_emulator(&self) -> bool {
        self.id.starts_with("emulator-")
    }

    /// True for network-attached devices: `host:port`, or the mDNS name adb gives
    /// a phone it found and connected to by itself.
    pub fn is_remote(&self) -> bool {
        is_remote_id(&self.id) || is_mdns_id(&self.id)
    }

    pub fn from_listed(listed: &ListedDevice) -> Self {
        Device {
            id: listed.serial.clone(),
            name: listed.serial.clone(),
            serialno: String::new(),
            android_version: String::new(),
            sdk_version: String::new(),
            state: listed.state.clone(),
        }
    }
}

/// `adb-<serialno>-<suffix>._adb-tls-connect._tcp`: not an address `adb connect` accepts.
pub fn is_mdns_id(id: &str) -> bool {
    id.ends_with("._adb-tls-connect._tcp")
}

pub fn is_remote_id(id: &str) -> bool {
    match id.rsplit_once(':') {
        Some((host, port)) => {
            let octets: Vec<&str> = host.split('.').collect();
            port.parse::<u16>().is_ok() && octets.len() == 4 && octets.iter().all(|o| o.parse::<u8>().is_ok())
        }
        None => false,
    }
}

impl Adb {
    pub async fn list_devices(&self) -> Result<Vec<ListedDevice>> {
        let out = self.global(["devices"]).await?;
        if !out.success() {
            return Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() });
        }
        let text = out.stdout_text();
        if !text.contains("List of devices attached") {
            return Err(Error::parse("adb devices", format!("missing the device list header in {:?}", text.lines().next().unwrap_or(""))));
        }
        Ok(parse_device_list(&text))
    }

    /// Fills in name and versions for an online device.
    pub async fn describe(&self, listed: &ListedDevice) -> Device {
        let mut device = Device::from_listed(listed);
        if device.state != DeviceState::Online {
            return device;
        }
        let dev = self.device(&listed.serial);
        if let Ok(text) = dev.shell("getprop").await {
            let props = parse_getprop(&text);
            let get = |k: &str| props.get(k).cloned().unwrap_or_default();
            device.name = display_name(&props);
            device.serialno = get("ro.serialno");
            device.android_version = get("ro.build.version.release");
            device.sdk_version = get("ro.build.version.sdk");
        }
        device
    }

    /// Lists devices and describes the online ones.
    pub async fn snapshot(&self) -> Result<Vec<Device>> {
        let listed = self.list_devices().await?;
        let mut devices = Vec::with_capacity(listed.len());
        for l in &listed {
            devices.push(self.describe(l).await);
        }
        Ok(devices)
    }
}

/// Devices the user disconnected on purpose; the watcher must not bring them back.
pub type Suppressed = Arc<Mutex<HashSet<String>>>;

/// Marks (`true`) or unmarks a device the watcher must leave disconnected.
pub fn suppress(suppressed: &Suppressed, id: &str, on: bool) {
    if let Ok(mut set) = suppressed.lock() {
        if on {
            set.insert(id.to_string());
        } else {
            set.remove(id);
        }
    }
}

/// Lets the watcher connect a phone again whatever port it used when the user disconnected it.
pub fn unsuppress_host(suppressed: &Suppressed, host: &str) {
    if let Ok(mut set) = suppressed.lock() {
        set.retain(|id| id.rsplit_once(':').is_none_or(|(h, _)| h != host));
    }
}

/// What [`watch_devices`] reports.
#[derive(Debug)]
pub enum WatchEvent {
    Devices(Vec<Device>),
    Failed(Error),
    /// A Wi-Fi device dropped and a reconnect attempt is starting.
    Reconnecting {
        id: String,
        attempt: u32,
        of: u32,
    },
    Restored(String),
    GaveUp(String),
    /// A phone found on the local network was connected (`ip:port`).
    Found(String),
}

/// What the watcher takes from the saved data.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    /// Phones to look for when they are not connected.
    pub known: Vec<KnownDevice>,
    /// Ids of phones the user disconnected on purpose.
    pub blocked: HashSet<String>,
}

impl Registry {
    /// Phones with automatic connection on are looked for; the others are blocked.
    pub fn from_data(data: &AppData) -> Self {
        let mut registry = Registry::default();
        for r in &data.remote_devices {
            if r.auto_connect {
                registry.known.push(KnownDevice::from(r));
            } else {
                registry.blocked.insert(r.id.clone());
                registry.blocked.extend(r.endpoints.iter().cloned());
            }
        }
        registry
    }
}

/// Supplies a fresh [`Registry`]; see [`WatchOptions::reload`].
pub type Reload = Arc<dyn Fn() -> Registry + Send + Sync>;

pub struct WatchOptions {
    /// Wi-Fi devices from earlier sessions to connect to at start.
    pub remembered: Vec<String>,
    /// Reconnect remembered devices and connect to phones found on the network.
    pub auto_reconnect: bool,
    pub backoff: Backoff,
    pub suppressed: Suppressed,
    /// How often to look for phones on the network.
    pub scan_every: Duration,
    /// Remembered phones to look for at their old addresses, the standard tcpip port and
    /// the gateways of this computer's networks (USB tethering, phone hotspot).
    pub known: Vec<KnownDevice>,
    /// Asked on every network scan. When set, its answer replaces `known` and
    /// `suppressed`, so a process without a window (the background service) follows
    /// changes the settings UI saves meanwhile.
    pub reload: Option<Reload>,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            remembered: Vec::new(),
            auto_reconnect: true,
            backoff: Backoff::RECONNECT,
            suppressed: Suppressed::default(),
            scan_every: Duration::from_secs(10),
            known: Vec::new(),
            reload: None,
        }
    }
}

enum Track {
    Online,
    /// `tried`: at least one connect attempt was made, so coming back is news.
    Lost {
        attempts: u32,
        due: Instant,
        tried: bool,
    },
}

/// Decides when a dropped Wi-Fi device is worth another connect attempt. Pure:
/// the caller supplies the clock, so the schedule is testable without waiting.
pub struct Reconnector {
    enabled: bool,
    backoff: Backoff,
    tracked: HashMap<String, Track>,
    /// Ids and serial numbers of the devices that were online at the last look.
    online_ids: HashSet<String>,
    online_serials: HashSet<String>,
}

impl Reconnector {
    pub fn new(enabled: bool, backoff: Backoff, remembered: impl IntoIterator<Item = String>, now: Instant) -> Self {
        let tracked = if enabled {
            remembered
                .into_iter()
                .filter(|id| is_remote_id(id))
                .map(|id| (id, Track::Lost { attempts: 0, due: now, tried: false }))
                .collect()
        } else {
            HashMap::new()
        };
        Self { enabled, backoff, tracked, online_ids: HashSet::new(), online_serials: HashSet::new() }
    }

    /// True when the phone is online right now, at one of its endpoints or under its
    /// serial number (plugged in, or connected at an address that was not remembered).
    pub fn is_online(&self, known: &KnownDevice) -> bool {
        (!known.serialno.is_empty() && self.online_serials.contains(&known.serialno))
            || known.endpoints.iter().any(|e| self.online_ids.contains(e))
    }

    /// Looks at a fresh device list; returns the devices that just came back
    /// after at least one failed attempt.
    pub fn observe(&mut self, devices: &[Device], now: Instant) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        self.online_ids = devices.iter().filter(|d| d.is_online()).map(|d| d.id.clone()).collect();
        self.online_serials = devices.iter().filter(|d| d.is_online() && !d.serialno.is_empty()).map(|d| d.serialno.clone()).collect();
        let mut restored = Vec::new();
        // Only `ip:port` ids can be handed to `adb connect`; mDNS names are found by the scan instead.
        for d in devices.iter().filter(|d| is_remote_id(&d.id)) {
            match d.state {
                DeviceState::Online => {
                    if let Some(Track::Lost { tried, .. }) = self.tracked.insert(d.id.clone(), Track::Online) {
                        if tried {
                            restored.push(d.id.clone());
                        }
                    }
                }
                // The user has to approve the computer on the phone; connecting again changes nothing.
                DeviceState::Unauthorized => {
                    self.tracked.remove(&d.id);
                }
                _ => {
                    if matches!(self.tracked.get(&d.id), Some(Track::Online)) {
                        self.tracked.insert(d.id.clone(), Track::Lost { attempts: 0, due: now, tried: false });
                    }
                }
            }
        }
        for (id, track) in &mut self.tracked {
            if matches!(track, Track::Online) && !devices.iter().any(|d| &d.id == id) {
                *track = Track::Lost { attempts: 0, due: now, tried: false };
            }
        }
        restored
    }

    /// Devices to try now, with the number of the attempt about to be made.
    pub fn due(&self, now: Instant) -> Vec<(String, u32)> {
        let mut due: Vec<_> = self
            .tracked
            .iter()
            .filter_map(|(id, t)| match t {
                Track::Lost { attempts, due, .. } if *due <= now => Some((id.clone(), attempts + 1)),
                _ => None,
            })
            .collect();
        due.sort();
        due
    }

    pub fn next_due(&self) -> Option<Instant> {
        self.tracked
            .values()
            .filter_map(|t| match t {
                Track::Lost { due, .. } => Some(*due),
                Track::Online => None,
            })
            .min()
    }

    /// Notes a failed attempt; true when that was the last one and the device is given up on.
    pub fn failed(&mut self, id: &str, now: Instant) -> bool {
        let Some(Track::Lost { attempts, due, tried }) = self.tracked.get_mut(id) else { return false };
        *attempts += 1;
        *tried = true;
        if *attempts >= self.backoff.attempts {
            self.tracked.remove(id);
            return true;
        }
        *due = now + self.backoff.delay(*attempts);
        false
    }

    /// Notes that a connect attempt went through; the device shows up in the next list.
    pub fn attempted(&mut self, id: &str) {
        if let Some(Track::Lost { tried, .. }) = self.tracked.get_mut(id) {
            *tried = true;
        }
    }

    pub fn forget(&mut self, id: &str) {
        self.tracked.remove(id);
    }

    pub fn attempts_allowed(&self) -> u32 {
        self.backoff.attempts
    }
}

impl Adb {
    /// `adb connect` for an id such as `192.168.1.20:5555`.
    pub(super) async fn connect_id(&self, id: &str) -> Result<()> {
        let (host, port) = id.rsplit_once(':').ok_or_else(|| Error::Invalid(format!("'{id}' is not a network address")))?;
        let port = port.parse::<u16>().map_err(|_| Error::Invalid(format!("'{id}' has no valid port")))?;
        tokio::time::timeout(Duration::from_secs(20), self.connect(host, Some(port)))
            .await
            .unwrap_or_else(|_| Err(Error::Timeout { program: "adb connect".into(), secs: 20 }))
    }
}

/// Leave a phone alone this long after a connect attempt that did not bring it online.
const DISCOVER_COOLDOWN: Duration = Duration::from_secs(60);
/// The same for a round of looking for a remembered phone at its known addresses.
const PROBE_COOLDOWN: Duration = Duration::from_secs(30);

/// Follows device changes. `adb track-devices` only signals *that* something
/// changed (its framing is not parsed); each signal triggers one list+describe.
/// Wi-Fi devices that drop are reconnected with growing pauses, phones that
/// show up on the network are connected, and the watcher comes back by itself
/// when the adb server goes away.
pub async fn watch_devices(adb: Adb, tx: mpsc::Sender<WatchEvent>, options: WatchOptions) {
    let WatchOptions { remembered, auto_reconnect, backoff, suppressed, scan_every, mut known, reload } = options;
    let start = Instant::now();
    let mut reconnector = Reconnector::new(auto_reconnect, backoff, remembered, start);
    let mut discoverer = Discoverer::new(DISCOVER_COOLDOWN);
    let mut prober = Prober::new(PROBE_COOLDOWN);
    // The first look is at once: the phone may already be advertising.
    let mut next_scan = auto_reconnect.then_some(start);
    loop {
        if !publish(&adb, &tx, &mut reconnector).await {
            return;
        }
        let spec: CommandSpec = adb.spec().arg("track-devices");
        match spawn_lines(&spec, 8, false) {
            Ok(mut stream) => loop {
                let wait =
                    [reconnector.next_due(), next_scan].into_iter().flatten().min().map(|d| d.saturating_duration_since(Instant::now()));
                tokio::select! {
                    line = stream.lines.recv() => {
                        // Bytes arrive without newlines; lines may be coalesced, so
                        // any received line (or EOF) is treated as a change signal.
                        if line.is_none() {
                            break;
                        }
                        // Let a burst of updates settle before querying.
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        while stream.lines.try_recv().is_ok() {}
                        if !publish(&adb, &tx, &mut reconnector).await {
                            return;
                        }
                    }
                    () = async {
                        match wait {
                            Some(d) => tokio::time::sleep(d).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        let now = Instant::now();
                        if reconnector.next_due().is_some_and(|d| d <= now) && !reconnect_due(&adb, &tx, &mut reconnector, &suppressed).await {
                            return;
                        }
                        if next_scan.is_some_and(|d| d <= now) {
                            next_scan = Some(Instant::now() + scan_every);
                            if let Some(reload) = &reload {
                                let registry = (**reload)();
                                known = registry.known;
                                if let Ok(mut set) = suppressed.lock() {
                                    *set = registry.blocked;
                                }
                            }
                            if !scan(&adb, &tx, &mut reconnector, &mut discoverer, &suppressed).await {
                                return;
                            }
                            if !find_known(&adb, &tx, &mut reconnector, &mut prober, &known, &suppressed).await {
                                return;
                            }
                        }
                    }
                }
            },
            Err(e) => {
                if tx.send(WatchEvent::Failed(e)).await.is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Reads the device list and reports it; false when nobody listens any more.
async fn publish(adb: &Adb, tx: &mpsc::Sender<WatchEvent>, reconnector: &mut Reconnector) -> bool {
    match adb.snapshot().await {
        Ok(devices) => {
            let restored = reconnector.observe(&devices, Instant::now());
            if tx.send(WatchEvent::Devices(devices)).await.is_err() {
                return false;
            }
            for id in restored {
                if tx.send(WatchEvent::Restored(id)).await.is_err() {
                    return false;
                }
            }
            true
        }
        Err(e) => {
            // A stopped server is the usual reason; bring it back before the next look.
            let _ = adb.start_server().await;
            tx.send(WatchEvent::Failed(e)).await.is_ok()
        }
    }
}

/// Connects to phones that advertise Wireless debugging and are not connected
/// yet; false when nobody listens any more. Failures only get logged: a network
/// without mDNS, or a phone that refuses, is the normal case.
async fn scan(
    adb: &Adb,
    tx: &mpsc::Sender<WatchEvent>,
    reconnector: &mut Reconnector,
    discoverer: &mut Discoverer,
    suppressed: &Suppressed,
) -> bool {
    let found = match adb.discover().await {
        Ok(found) => found,
        Err(e) => {
            tracing::debug!(error = %e, "looking for phones on the network failed");
            return true;
        }
    };
    let Ok(listed) = adb.list_devices().await else { return true };
    // An offline entry is a dead transport that connecting again revives, so it does not count.
    let listed: Vec<String> = listed.into_iter().filter(|d| d.state != DeviceState::Offline).map(|d| d.serial).collect();
    let blocked = suppressed.lock().map(|s| s.clone()).unwrap_or_default();
    let mut connected = false;
    for phone in discoverer.plan(&found, &listed, &blocked, Instant::now()) {
        match adb.connect_id(&phone.endpoint).await {
            Ok(()) => {
                connected = true;
                if tx.send(WatchEvent::Found(phone.endpoint)).await.is_err() {
                    return false;
                }
            }
            Err(e) => tracing::debug!(endpoint = %phone.endpoint, error = %e, "connecting to a discovered phone failed"),
        }
    }
    // Show the new device now instead of waiting for the next change signal.
    !connected || publish(adb, tx, reconnector).await
}

/// Looks for remembered phones that are not connected: at the addresses they were
/// reached at, on the standard tcpip port and at the gateways of this computer's
/// networks. False when nobody listens any more.
async fn find_known(
    adb: &Adb,
    tx: &mpsc::Sender<WatchEvent>,
    reconnector: &mut Reconnector,
    prober: &mut Prober,
    known: &[KnownDevice],
    suppressed: &Suppressed,
) -> bool {
    let blocked = suppressed.lock().map(|s| s.clone()).unwrap_or_default();
    let due = prober.plan(known, |k| reconnector.is_online(k), &blocked, Instant::now());
    if due.is_empty() {
        return true;
    }
    let gateways = local_gateways();
    let mut connected = false;
    for phone in due {
        match adb.reconnect_known(phone, &gateways).await {
            Ok(Some(endpoint)) => {
                connected = true;
                if tx.send(WatchEvent::Found(endpoint)).await.is_err() {
                    return false;
                }
            }
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, "looking for a remembered phone failed"),
        }
    }
    !connected || publish(adb, tx, reconnector).await
}

async fn reconnect_due(adb: &Adb, tx: &mpsc::Sender<WatchEvent>, reconnector: &mut Reconnector, suppressed: &Suppressed) -> bool {
    for (id, attempt) in reconnector.due(Instant::now()) {
        let skip = suppressed.lock().map(|s| s.contains(&id)).unwrap_or(false);
        if skip {
            reconnector.forget(&id);
            continue;
        }
        let of = reconnector.attempts_allowed();
        if tx.send(WatchEvent::Reconnecting { id: id.clone(), attempt, of }).await.is_err() {
            return false;
        }
        match adb.connect_id(&id).await {
            Ok(()) => reconnector.attempted(&id),
            Err(e) => {
                tracing::debug!(%id, attempt, error = %e, "reconnect attempt failed");
                if reconnector.failed(&id, Instant::now()) && tx.send(WatchEvent::GaveUp(id)).await.is_err() {
                    return false;
                }
            }
        }
    }
    // Whatever the attempts did shows up in the list; do not wait for the next signal.
    publish(adb, tx, reconnector).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "* daemon not running; starting now at tcp:5037\n\
        * daemon started successfully\n\
        List of devices attached\n\
        28131FDH            device usb:1-1 product:panther model:Pixel_7 device:panther transport_id:1\n\
        emulator-5554       offline transport_id:2\n\
        192.168.1.20:5555   unauthorized transport_id:3\n\
        0123456789ABCDEF    no permissions (user in plugdev group; are your udev rules wrong?); see [http://developer.android.com/tools/device.html]\n\
        R58M1234            connecting\n\
        weird               sideload\n";

    #[test]
    fn a_whole_host_can_be_released_whatever_its_port() {
        let suppressed = Suppressed::default();
        for id in ["192.168.1.20:5555", "192.168.1.20:40111", "192.168.1.200:5555", "R58M1234"] {
            suppress(&suppressed, id, true);
        }
        unsuppress_host(&suppressed, "192.168.1.20");
        let mut left: Vec<_> = suppressed.lock().unwrap().iter().cloned().collect();
        left.sort();
        assert_eq!(left, ["192.168.1.200:5555", "R58M1234"], "other hosts, and ids that are not addresses, stay");
    }

    #[test]
    fn parses_all_states_and_skips_daemon_noise() {
        let list = parse_device_list(LIST);
        let states: Vec<_> = list.iter().map(|d| (d.serial.as_str(), d.state.clone())).collect();
        assert_eq!(
            states,
            vec![
                ("28131FDH", DeviceState::Online),
                ("emulator-5554", DeviceState::Offline),
                ("192.168.1.20:5555", DeviceState::Unauthorized),
                ("0123456789ABCDEF", DeviceState::NoPermissions),
                ("R58M1234", DeviceState::Connecting),
                ("weird", DeviceState::Other("sideload".into())),
            ]
        );
    }

    #[test]
    fn empty_listing_has_no_devices() {
        assert!(parse_device_list("List of devices attached\n\n").is_empty());
        assert!(parse_device_list("").is_empty());
    }

    #[test]
    fn remote_ids_are_recognised() {
        assert!(is_remote_id("192.168.1.20:5555"));
        assert!(!is_remote_id("emulator-5554"));
        assert!(!is_remote_id("192.168.1:5555"));
        assert!(!is_remote_id("192.168.1.20"));
        assert!(!is_remote_id("300.1.1.1:5555"));
        assert!(!is_remote_id("1.1.1.1:99999"));
    }

    #[test]
    fn mdns_names_count_as_wireless_but_cannot_be_reconnected_by_address() {
        let name = "adb-R58M1234-AbCdEf._adb-tls-connect._tcp";
        assert!(is_mdns_id(name) && !is_remote_id(name));
        assert!(dev(name, DeviceState::Online).is_remote());
        assert!(!dev("R58M1234", DeviceState::Online).is_remote());

        let t0 = Instant::now();
        let mut r = Reconnector::new(true, quick(), [name.to_string()], t0);
        r.observe(&[dev(name, DeviceState::Online)], t0);
        r.observe(&[], t0);
        assert!(r.due(t0).is_empty(), "`adb connect` has no use for an mDNS name");
    }

    fn dev(id: &str, state: DeviceState) -> Device {
        Device::from_listed(&ListedDevice { serial: id.into(), state })
    }

    const PHONE: &str = "192.168.1.20:5555";

    fn quick() -> Backoff {
        Backoff { attempts: 3, first: Duration::from_secs(2), cap: Duration::from_secs(8) }
    }

    #[test]
    fn a_dropped_wifi_device_is_retried_with_growing_pauses_then_given_up() {
        let t0 = Instant::now();
        let mut r = Reconnector::new(true, quick(), [], t0);
        assert!(r.observe(&[dev(PHONE, DeviceState::Online)], t0).is_empty());
        assert!(r.due(t0).is_empty(), "online devices are left alone");

        // It vanishes from the list: first attempt at once.
        r.observe(&[], t0);
        assert_eq!(r.due(t0), vec![(PHONE.to_string(), 1)]);
        assert!(!r.failed(PHONE, t0));
        assert!(r.due(t0).is_empty(), "the pause has not passed");
        assert_eq!(r.next_due(), Some(t0 + Duration::from_secs(2)));
        assert_eq!(r.due(t0 + Duration::from_secs(2)), vec![(PHONE.to_string(), 2)]);
        assert!(!r.failed(PHONE, t0 + Duration::from_secs(2)));
        assert_eq!(r.next_due(), Some(t0 + Duration::from_secs(6)), "pauses double");
        assert!(r.failed(PHONE, t0 + Duration::from_secs(6)), "the third failure ends it");
        assert!(r.due(t0 + Duration::from_secs(600)).is_empty());
        assert_eq!(r.next_due(), None);
    }

    #[test]
    fn coming_back_is_reported_only_after_a_failed_attempt() {
        let t0 = Instant::now();
        let mut r = Reconnector::new(true, quick(), [], t0);
        r.observe(&[dev(PHONE, DeviceState::Online)], t0);
        r.observe(&[dev(PHONE, DeviceState::Offline)], t0);
        assert_eq!(r.due(t0).len(), 1, "an offline entry counts as lost");
        assert!(!r.failed(PHONE, t0));
        assert_eq!(r.observe(&[dev(PHONE, DeviceState::Online)], t0), vec![PHONE.to_string()]);
        assert_eq!(r.next_due(), None);
        // A blip that fixed itself before any attempt is not news.
        r.observe(&[], t0);
        assert!(r.observe(&[dev(PHONE, DeviceState::Online)], t0).is_empty());
    }

    #[test]
    fn remembered_devices_are_tried_at_start_but_usb_and_unauthorized_are_not() {
        let t0 = Instant::now();
        let r = Reconnector::new(true, quick(), [PHONE.to_string(), "FAKE0001".to_string()], t0);
        assert_eq!(r.due(t0), vec![(PHONE.to_string(), 1)], "only network ids are reconnected");

        let mut r = Reconnector::new(true, quick(), [PHONE.to_string()], t0);
        r.observe(&[dev(PHONE, DeviceState::Unauthorized)], t0);
        assert!(r.due(t0).is_empty(), "connecting again cannot approve the computer on the phone");

        let mut r = Reconnector::new(true, quick(), [], t0);
        r.observe(&[dev("FAKE0001", DeviceState::Online)], t0);
        r.observe(&[], t0);
        assert!(r.due(t0).is_empty(), "an unplugged USB device is not reconnected");
    }

    #[test]
    fn a_phone_counts_as_online_by_address_or_by_serial_number() {
        let t0 = Instant::now();
        let mut r = Reconnector::new(true, quick(), [], t0);
        let mut usb = dev("FAKE0001", DeviceState::Online);
        usb.serialno = "R58M1234".into();
        let mut offline = dev(PHONE, DeviceState::Offline);
        offline.serialno = "OTHER".into();
        r.observe(&[usb, offline], t0);

        let by_serial = KnownDevice { serialno: "R58M1234".into(), endpoints: vec!["192.168.1.99:5555".into()] };
        assert!(r.is_online(&by_serial), "plugged in under its serial number");
        let by_address = KnownDevice { serialno: String::new(), endpoints: vec!["FAKE0001".into()] };
        assert!(r.is_online(&by_address));
        let offline_one = KnownDevice { serialno: "OTHER".into(), endpoints: vec![PHONE.into()] };
        assert!(!r.is_online(&offline_one), "an offline entry is not a connection");
        assert!(!r.is_online(&KnownDevice::default()), "an empty record matches nothing");
    }

    #[test]
    fn the_registry_looks_for_phones_with_automatic_connection_and_blocks_the_others() {
        use crate::store::RemoteDevice;
        let on = RemoteDevice { id: "192.168.1.20:5555".into(), serialno: "AAA".into(), ..RemoteDevice::default() };
        let off = RemoteDevice {
            id: "192.168.1.31:40001".into(),
            serialno: "BBB".into(),
            endpoints: vec!["192.168.1.31:40001".into(), "192.168.1.31:5555".into()],
            auto_connect: false,
            ..RemoteDevice::default()
        };
        let registry = Registry::from_data(&AppData { remote_devices: vec![on, off], ..AppData::default() });
        assert_eq!(registry.known.len(), 1);
        assert_eq!(registry.known[0].serialno, "AAA");
        assert_eq!(registry.known[0].endpoints, vec!["192.168.1.20:5555"], "records without endpoints fall back to their id");
        let mut blocked: Vec<_> = registry.blocked.iter().map(String::as_str).collect();
        blocked.sort();
        assert_eq!(blocked, ["192.168.1.31:40001", "192.168.1.31:5555"]);
    }

    #[test]
    fn switched_off_it_never_reconnects() {
        let t0 = Instant::now();
        let mut r = Reconnector::new(false, quick(), [PHONE.to_string()], t0);
        r.observe(&[dev(PHONE, DeviceState::Online)], t0);
        r.observe(&[], t0);
        assert!(r.due(t0).is_empty());
        assert_eq!(r.next_due(), None);
    }

    #[test]
    fn a_first_attempt_that_works_is_reported_too() {
        let t0 = Instant::now();
        let mut r = Reconnector::new(true, quick(), [], t0);
        r.observe(&[dev(PHONE, DeviceState::Online)], t0);
        r.observe(&[], t0);
        r.attempted(PHONE);
        assert_eq!(r.observe(&[dev(PHONE, DeviceState::Online)], t0), vec![PHONE.to_string()]);
    }
}

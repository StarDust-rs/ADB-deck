//! Persistent user data: settings (TOML in the XDG config dir) and
//! application data (JSON in the XDG data dir). Writes are atomic and
//! files are private to the user.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

/// User-editable preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// Explicit adb executable; empty means "look it up".
    pub adb_path: String,
    pub apksigner_path: String,
    /// Stop the adb server when the application exits.
    pub kill_adb_on_exit: bool,
    /// Connect to remembered Wi-Fi devices at start, win them back when they drop,
    /// and connect to paired phones found on the local network.
    pub auto_reconnect: bool,
    /// Start the windowless background service at login so remembered devices are
    /// connected without ADB Deck being open (see `autostart` and `daemon`).
    pub background_service: bool,
    /// Look up app icons and names on the Play Store (sends package names to the icon service).
    pub fetch_app_icons: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            adb_path: String::new(),
            apksigner_path: String::new(),
            kill_adb_on_exit: false,
            auto_reconnect: true,
            background_service: true,
            fetch_app_icons: true,
        }
    }
}

/// How many recent `ip:port` endpoints are kept per phone.
pub const ENDPOINTS_MAX: usize = 6;

/// A phone that was connected over the network, with what is needed to find it
/// again: every address it was reached at, the serial number that proves it is
/// the same phone, and whether the user wants it connected automatically.
/// Fields added after the first release have defaults so older files still load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteDevice {
    /// The `ip:port` it was last connected at.
    pub id: String,
    pub name: String,
    pub serialno: String,
    pub android_version: String,
    pub sdk_version: String,
    /// Recent `ip:port` endpoints, most recent first (includes `id`). Wireless
    /// debugging changes its port and DHCP its address, so older ones may still help.
    pub endpoints: Vec<String>,
    /// Unix time (seconds) when the phone was last seen online.
    pub last_seen: u64,
    /// False after the user disconnected it on purpose; the background service
    /// and the watcher then leave it alone until the user connects it again.
    pub auto_connect: bool,
}

impl Default for RemoteDevice {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            serialno: String::new(),
            android_version: String::new(),
            sdk_version: String::new(),
            endpoints: Vec::new(),
            last_seen: 0,
            auto_connect: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MirrorSettings {
    pub video_bit_rate_mbps: u32,
    /// Longest side of the stream in pixels; 0 means the device's own size.
    pub max_size: u32,
    /// Frames per second cap; 0 means the device's default.
    pub max_fps: u32,
    pub stay_awake: bool,
    pub show_touches: bool,
    /// Switch the phone's screen off while it is mirrored (it keeps running).
    pub turn_screen_off: bool,
    /// Switch the phone's screen off when mirroring ends.
    pub power_off_on_close: bool,
    /// Share the clipboard between the computer and the phone.
    pub sync_clipboard: bool,
}

impl Default for MirrorSettings {
    fn default() -> Self {
        // 1280 px keeps software decoding comfortable on any machine.
        Self {
            video_bit_rate_mbps: 8,
            max_size: 1280,
            max_fps: 0,
            stay_awake: true,
            show_touches: false,
            turn_screen_off: false,
            power_off_on_close: false,
            sync_clipboard: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SigningProfile {
    pub id: String,
    pub name: String,
    pub keystore_path: String,
    pub key_alias: String,
    pub scheme: SigningScheme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SigningScheme {
    V1,
    V2,
    V1V2,
}

impl SigningScheme {
    pub const ALL: [SigningScheme; 3] = [SigningScheme::V1, SigningScheme::V2, SigningScheme::V1V2];

    pub fn label(self) -> &'static str {
        match self {
            SigningScheme::V1 => "V1",
            SigningScheme::V2 => "V2",
            SigningScheme::V1V2 => "V1 + V2",
        }
    }

    pub fn v1(self) -> bool {
        matches!(self, SigningScheme::V1 | SigningScheme::V1V2)
    }

    pub fn v2(self) -> bool {
        matches!(self, SigningScheme::V2 | SigningScheme::V1V2)
    }
}

/// Data that is not a preference: remembered devices, filters, profiles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppData {
    pub last_panel: String,
    pub last_device: String,
    pub show_system_packages: bool,
    pub only_package_processes: bool,
    pub files_show_preview: bool,
    pub files_show_transfers: bool,
    pub remote_devices: Vec<RemoteDevice>,
    pub mirror: BTreeMap<String, MirrorSettings>,
    pub signing_profiles: Vec<SigningProfile>,
    /// Most-recent-first filter histories for the log viewer, by field name.
    pub logcat_history: BTreeMap<String, Vec<String>>,
}

pub const HISTORY_MAX: usize = 10;

impl AppData {
    pub fn push_history(&mut self, field: &str, value: &str) {
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        let list = self.logcat_history.entry(field.to_string()).or_default();
        list.retain(|v| v != value);
        list.insert(0, value.to_string());
        list.truncate(HISTORY_MAX);
    }
}

/// File-backed store handle. Cheap to clone the paths; holds no open files.
#[derive(Debug, Clone)]
pub struct Store {
    settings_path: PathBuf,
    data_path: PathBuf,
}

impl Store {
    pub fn at(config_dir: &Path, data_dir: &Path) -> Self {
        Self { settings_path: config_dir.join("settings.toml"), data_path: data_dir.join("data.json") }
    }

    pub fn default_location() -> Result<Self> {
        let config = paths::config_dir().ok_or_else(|| Error::Invalid("cannot determine the user configuration directory".into()))?;
        let data = paths::data_dir().ok_or_else(|| Error::Invalid("cannot determine the user data directory".into()))?;
        Ok(Self::at(&config, &data))
    }

    /// Loads settings. A damaged file is moved aside and defaults are used;
    /// the second tuple member then carries a message for the user.
    pub fn load_settings(&self) -> (Settings, Option<String>) {
        load_or_default(&self.settings_path, |s| toml::from_str::<Settings>(s).map_err(|e| e.to_string()))
    }

    pub fn load_data(&self) -> (AppData, Option<String>) {
        load_or_default(&self.data_path, |s| serde_json::from_str::<AppData>(s).map_err(|e| e.to_string()))
    }

    /// Reads settings without side effects. A missing or damaged file gives `None` and
    /// stays where it is: only the window moves a damaged file aside, because only it
    /// can tell the user (see [`load_settings`](Self::load_settings)).
    pub fn peek_settings(&self) -> Option<Settings> {
        fs::read_to_string(&self.settings_path).ok().and_then(|t| toml::from_str::<Settings>(&t).ok())
    }

    /// The same for the application data.
    pub fn peek_data(&self) -> Option<AppData> {
        fs::read_to_string(&self.data_path).ok().and_then(|t| serde_json::from_str::<AppData>(&t).ok())
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        let text = toml::to_string_pretty(settings).map_err(|e| Error::Invalid(format!("cannot encode settings: {e}")))?;
        write_atomic(&self.settings_path, text.as_bytes())
    }

    pub fn save_data(&self, data: &AppData) -> Result<()> {
        let text = serde_json::to_vec_pretty(data).map_err(|e| Error::Invalid(format!("cannot encode data: {e}")))?;
        write_atomic(&self.data_path, &text)
    }
}

fn load_or_default<T: DeserializeOwned + Default>(
    path: &Path,
    parse: impl Fn(&str) -> std::result::Result<T, String>,
) -> (T, Option<String>) {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (T::default(), None),
        Err(e) => return (T::default(), Some(format!("Could not read {}: {e}. Using defaults.", path.display()))),
    };
    match parse(&text) {
        Ok(v) => (v, None),
        Err(reason) => {
            let backup = path.with_extension("bad");
            let moved = fs::rename(path, &backup).is_ok();
            let note = if moved {
                format!("{} was damaged ({reason}). It was kept as {} and defaults are in use.", path.display(), backup.display())
            } else {
                format!("{} was damaged ({reason}). Defaults are in use.", path.display())
            };
            (T::default(), Some(note))
        }
    }
}

/// Writes through a private temporary file in the same directory, then
/// renames it over the target so readers never see a partial file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or_else(|| Error::Invalid(format!("{} has no parent directory", path.display())))?;
    fs::create_dir_all(dir).map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;

    let tmp =
        dir.join(format!(".{}.tmp{}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), std::process::id()));
    let write = || -> std::io::Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::io(format!("writing {}", path.display()), e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path) -> Store {
        Store::at(&dir.join("config"), &dir.join("data"))
    }

    #[test]
    fn missing_files_give_defaults_without_warning() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let (settings, warn) = s.load_settings();
        assert_eq!(settings, Settings::default());
        assert!(warn.is_none());
        assert!(s.load_data().1.is_none());
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let settings = Settings { theme: ThemeChoice::Dark, adb_path: "/opt/adb".into(), kill_adb_on_exit: true, ..Settings::default() };
        s.save_settings(&settings).unwrap();
        assert_eq!(s.load_settings().0, settings);
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        fs::create_dir_all(dir.path().join("config")).unwrap();
        fs::write(dir.path().join("config/settings.toml"), "theme = \"light\"\nfuture = 1\n").unwrap();
        let (settings, warn) = s.load_settings();
        assert_eq!(settings.theme, ThemeChoice::Light);
        assert!(warn.is_none());
    }

    #[test]
    fn damaged_file_is_moved_aside_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        fs::create_dir_all(dir.path().join("data")).unwrap();
        fs::write(dir.path().join("data/data.json"), "{ not json").unwrap();
        let (data, warn) = s.load_data();
        assert_eq!(data, AppData::default());
        assert!(warn.unwrap().contains("damaged"));
        assert!(dir.path().join("data/data.bad").exists());
        assert!(!dir.path().join("data/data.json").exists());
    }

    #[test]
    fn saved_files_are_private_and_leave_no_temp_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.save_data(&AppData::default()).unwrap();
        let path = dir.path().join("data/data.json");
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let leftovers: Vec<_> = fs::read_dir(dir.path().join("data"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn records_from_before_connection_details_were_kept_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        fs::create_dir_all(dir.path().join("data")).unwrap();
        let old = r#"{"remote_devices":[{"id":"192.168.1.20:5555","name":"Pixel 7","serialno":"R58M1234","android_version":"11","sdk_version":"30"}]}"#;
        fs::write(dir.path().join("data/data.json"), old).unwrap();
        let (data, warn) = s.load_data();
        assert!(warn.is_none());
        let r = &data.remote_devices[0];
        assert_eq!((r.id.as_str(), r.serialno.as_str()), ("192.168.1.20:5555", "R58M1234"));
        assert!(r.auto_connect, "a phone remembered before the switch existed is reconnected");
        assert!(r.endpoints.is_empty() && r.last_seen == 0);
    }

    #[test]
    fn peeking_never_moves_a_damaged_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert!(s.peek_data().is_none() && s.peek_settings().is_none());
        fs::create_dir_all(dir.path().join("data")).unwrap();
        fs::write(dir.path().join("data/data.json"), "{ not json").unwrap();
        assert!(s.peek_data().is_none());
        assert!(dir.path().join("data/data.json").exists());
        s.save_data(&AppData::default()).unwrap();
        assert_eq!(s.peek_data(), Some(AppData::default()));
    }

    #[test]
    fn unwritable_location_is_an_error_not_a_panic() {
        let s = Store::at(Path::new("/proc/nonexistent-cfg"), Path::new("/proc/nonexistent-data"));
        assert!(s.save_settings(&Settings::default()).is_err());
    }

    #[test]
    fn history_is_deduplicated_bounded_and_most_recent_first() {
        let mut data = AppData::default();
        for i in 0..15 {
            data.push_history("tag", &format!("t{i}"));
        }
        data.push_history("tag", "t12");
        data.push_history("tag", "  ");
        let list = &data.logcat_history["tag"];
        assert_eq!(list.len(), HISTORY_MAX);
        assert_eq!(list[0], "t12");
        assert_eq!(list.iter().filter(|v| *v == "t12").count(), 1);
    }
}

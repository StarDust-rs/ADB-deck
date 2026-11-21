//! Device overview, font scale, HTTP proxy and root mode.

use super::control::{parse_battery, Battery};
use super::props::{display_name, parse_getprop};
use super::{sh_quote, DeviceAdb};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overview {
    pub name: String,
    pub brand: String,
    pub model: String,
    pub serialno: String,
    pub processor: String,
    pub abi: String,
    pub cpu_count: usize,
    pub kernel: String,
    pub font_scale: f32,
    pub wifi: String,
    pub root: bool,
    pub ip: String,
    pub mac: String,
    pub storage_total: u64,
    pub storage_used: u64,
    pub mem_total: u64,
    pub resolution: String,
    pub physical_resolution: String,
    pub density: String,
    pub physical_density: String,
    pub manufacturer: String,
    pub build_id: String,
    pub build_type: String,
    pub security_patch: String,
    pub selinux: String,
    pub timezone: String,
    pub bootloader_unlocked: Option<bool>,
    pub encrypted: Option<bool>,
    pub uptime_secs: Option<u64>,
    pub battery: Battery,
    pub mem_available: u64,
    pub user_apps: Option<usize>,
    pub total_apps: Option<usize>,
}

const SCRIPTS: [&str; 14] = [
    "getprop",
    "uname -r",
    "settings get system font_scale",
    "dumpsys wifi | grep mWifiInfo",
    "ip addr show wlan0",
    "grep -E '^(MemTotal|MemAvailable|MemFree)' /proc/meminfo",
    "dumpsys diskstats | grep Data-Free",
    "wm size; wm density",
    "id",
    "grep -c '^cpu[0-9]' /proc/stat",
    "cat /proc/uptime",
    "dumpsys battery",
    "echo $(pm list packages -3 | wc -l) $(pm list packages | wc -l)",
    "getenforce",
];

/// Seconds since boot from `/proc/uptime` ("12345.67 98765.43").
pub fn parse_uptime(text: &str) -> Option<u64> {
    let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then_some(secs as u64)
}

/// "3 d 4 h", "5 h 12 min", "7 min": two units at most, like a status line.
pub fn format_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    match (d, h) {
        (0, 0) => format!("{m} min"),
        (0, _) => format!("{h} h {m} min"),
        _ => format!("{d} d {h} h"),
    }
}

/// `(user apps, all apps)` from the two counts the script prints.
pub fn parse_app_counts(text: &str) -> (Option<usize>, Option<usize>) {
    let mut n = text.split_whitespace().map(|w| w.parse::<usize>().ok());
    (n.next().flatten(), n.next().flatten())
}

/// `Some(true)` when the bootloader is unlocked, from the boot properties.
pub fn bootloader_unlocked(props: &std::collections::HashMap<String, String>) -> Option<bool> {
    match props.get("ro.boot.flash.locked").map(String::as_str) {
        Some("1") => return Some(false),
        Some("0") => return Some(true),
        _ => {}
    }
    match props.get("ro.boot.verifiedbootstate").map(String::as_str) {
        Some("orange") => Some(true),
        Some("green" | "yellow") => Some(false),
        _ => None,
    }
}

pub fn parse_kib_field(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let rest = l.trim().strip_prefix(key)?.strip_prefix(':')?;
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

/// (total, available) bytes from /proc/meminfo lines.
pub fn parse_meminfo(text: &str) -> (u64, u64) {
    let total = parse_kib_field(text, "MemTotal").unwrap_or(0) * 1024;
    let free = parse_kib_field(text, "MemAvailable").or_else(|| parse_kib_field(text, "MemFree")).unwrap_or(0) * 1024;
    (total, free)
}

/// (total, free) bytes of /data from `dumpsys diskstats`.
pub fn parse_data_storage(text: &str) -> (u64, u64) {
    let Some(line) = text.lines().find(|l| l.contains("Data-Free:")) else {
        return (0, 0);
    };
    let rest = line.split("Data-Free:").nth(1).unwrap_or("");
    let mut nums = rest.split('/').map(|part| {
        part.trim()
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u64>()
            .ok()
    });
    match (nums.next().flatten(), nums.next().flatten()) {
        (Some(free), Some(total)) => (total * 1024, free * 1024),
        _ => (0, 0),
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Screen {
    pub resolution: String,
    pub physical_resolution: String,
    pub density: String,
    pub physical_density: String,
}

pub fn parse_screen(text: &str) -> Screen {
    let value = |key: &str| {
        text.lines().find_map(|l| l.trim().strip_prefix(key).map(|v| v.trim_start_matches(':').trim().to_string())).unwrap_or_default()
    };
    let physical_resolution = value("Physical size");
    let physical_density = value("Physical density");
    let over_res = value("Override size");
    let over_den = value("Override density");
    Screen {
        resolution: if over_res.is_empty() { physical_resolution.clone() } else { over_res },
        density: if over_den.is_empty() { physical_density.clone() } else { over_den },
        physical_resolution,
        physical_density,
    }
}

pub fn parse_wifi_ssid(text: &str) -> String {
    let Some(rest) = text.split("SSID:").nth(1) else {
        return String::new();
    };
    let ssid = rest.split(',').next().unwrap_or("").trim().trim_matches('"');
    if ssid == "<unknown ssid>" || ssid.is_empty() {
        String::new()
    } else {
        ssid.to_string()
    }
}

/// (ipv4, mac) from `ip addr show <iface>`.
pub fn parse_ip_mac(text: &str) -> (String, String) {
    let mut ip = String::new();
    let mut mac = String::new();
    for line in text.lines().map(str::trim) {
        if ip.is_empty() {
            if let Some(rest) = line.strip_prefix("inet ") {
                ip = rest.split(['/', ' ']).next().unwrap_or("").to_string();
            }
        }
        if mac.is_empty() {
            if let Some(rest) = line.strip_prefix("link/ether ") {
                mac = rest.split_whitespace().next().unwrap_or("").to_string();
            }
        }
    }
    (ip, mac)
}

pub fn is_root_id(id_output: &str) -> bool {
    id_output.contains("uid=0")
}

impl DeviceAdb {
    pub async fn overview(&self) -> Result<Overview> {
        let parts = self.shell_batch(&SCRIPTS).await?;
        let props = parse_getprop(&parts[0]);
        let get = |k: &str| props.get(k).cloned().unwrap_or_default();
        let (storage_total, free) = parse_data_storage(&parts[6]);
        let screen = parse_screen(&parts[7]);
        let (ip, mac) = parse_ip_mac(&parts[4]);
        let (mem_total, mem_available) = parse_meminfo(&parts[5]);
        let (user_apps, total_apps) = parse_app_counts(&parts[12]);
        Ok(Overview {
            name: display_name(&props),
            brand: get("ro.product.brand"),
            model: get("ro.product.model"),
            serialno: get("ro.serialno"),
            processor: get("ro.product.board"),
            abi: get("ro.product.cpu.abi"),
            cpu_count: parts[9].trim().parse().unwrap_or(0),
            kernel: parts[1].clone(),
            // An unset system setting prints "null"; the platform default is 1.0.
            font_scale: parts[2].trim().parse().ok().filter(|v: &f32| *v > 0.0).unwrap_or(1.0),
            wifi: parse_wifi_ssid(&parts[3]),
            root: is_root_id(&parts[8]),
            ip,
            mac,
            storage_total,
            storage_used: storage_total.saturating_sub(free),
            mem_total,
            resolution: screen.resolution,
            physical_resolution: screen.physical_resolution,
            density: screen.density,
            physical_density: screen.physical_density,
            manufacturer: get("ro.product.manufacturer"),
            build_id: get("ro.build.display.id"),
            build_type: get("ro.build.type"),
            security_patch: get("ro.build.version.security_patch"),
            selinux: parts[13].trim().to_string(),
            timezone: get("persist.sys.timezone"),
            bootloader_unlocked: bootloader_unlocked(&props),
            encrypted: props.get("ro.crypto.state").map(|v| v == "encrypted"),
            uptime_secs: parse_uptime(&parts[10]),
            battery: parse_battery(&parts[11]),
            mem_available,
            user_apps,
            total_apps,
        })
    }

    pub async fn set_font_scale(&self, scale: f32) -> Result<()> {
        if !(0.5..=4.0).contains(&scale) {
            return Err(Error::Invalid("font scale must be between 0.5 and 4.0".into()));
        }
        self.shell_ok(&format!("settings put system font_scale {scale:.2}")).await.map(|_| ())
    }

    pub async fn http_proxy(&self) -> Result<String> {
        let value = self.shell("settings get global http_proxy").await?;
        Ok(normalize_proxy(&value))
    }

    /// Sets (`Some`) or clears (`None`) the global HTTP proxy.
    pub async fn set_http_proxy(&self, proxy: Option<&str>) -> Result<()> {
        let script = match proxy {
            Some(p) => {
                validate_proxy(p)?;
                format!("settings put global http_proxy {}", sh_quote(p))
            }
            None => "settings put global http_proxy :0".to_string(),
        };
        self.shell_ok(&script).await.map(|_| ())
    }

    /// Restarts adbd with root privileges. Fails if the build forbids it.
    pub async fn enable_root(&self) -> Result<()> {
        let id = self.shell("id").await?;
        if is_root_id(&id) {
            return Ok(());
        }
        let out = self.exec(["root"]).await?;
        let text = format!("{}{}", out.stdout_text(), out.stderr);
        if !out.success() || text.contains("cannot run as root") || text.contains("not permitted") {
            return Err(Error::Invalid(format!("Root mode is not available on this device ({})", text.trim())));
        }
        Ok(())
    }
}

pub fn normalize_proxy(raw: &str) -> String {
    let v = raw.trim();
    if v == "null" || v == ":0" {
        String::new()
    } else {
        v.to_string()
    }
}

/// Accepts `host:port` with a DNS-name or IPv4 host.
pub fn validate_proxy(proxy: &str) -> Result<()> {
    let bad = || Error::Invalid("Enter the proxy as host:port, for example 192.168.1.5:8888".into());
    let (host, port) = proxy.trim().rsplit_once(':').ok_or_else(bad)?;
    let port_ok = port.parse::<u16>().map(|p| p > 0).unwrap_or(false);
    let host_ok =
        !host.is_empty() && host.len() <= 253 && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
    if port_ok && host_ok {
        Ok(())
    } else {
        Err(bad())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_prefers_available_over_free() {
        let text = "MemTotal:        7864320 kB\nMemFree:          100000 kB\nMemAvailable:    3000000 kB\n";
        assert_eq!(parse_meminfo(text), (7864320 * 1024, 3000000 * 1024));
        let old = "MemTotal: 1000 kB\nMemFree: 400 kB\n";
        assert_eq!(parse_meminfo(old), (1000 * 1024, 400 * 1024));
        assert_eq!(parse_meminfo("nothing"), (0, 0));
    }

    #[test]
    fn data_storage_from_diskstats() {
        let text = "Data-Free: 53284688K / 110592000K total = 48% free\n";
        assert_eq!(parse_data_storage(text), (110592000 * 1024, 53284688 * 1024));
        assert_eq!(parse_data_storage("junk"), (0, 0));
    }

    #[test]
    fn screen_override_wins_but_physical_is_kept() {
        let s = parse_screen("Physical size: 1080x2400\nOverride size: 720x1600\nPhysical density: 420\n");
        assert_eq!(s.resolution, "720x1600");
        assert_eq!(s.physical_resolution, "1080x2400");
        assert_eq!(s.density, "420");
        let plain = parse_screen("Physical size: 1080x2400\nPhysical density: 420\n");
        assert_eq!(plain.resolution, "1080x2400");
    }

    #[test]
    fn wifi_ssid_variants() {
        assert_eq!(parse_wifi_ssid("mWifiInfo SSID: \"Home Net\", BSSID: aa:bb, MAC: x"), "Home Net");
        assert_eq!(parse_wifi_ssid("mWifiInfo SSID: <unknown ssid>, BSSID: x"), "");
        assert_eq!(parse_wifi_ssid(""), "");
    }

    #[test]
    fn ip_and_mac_from_ip_addr() {
        let text = "24: wlan0: <BROADCAST> mtu 1500\n    link/ether 5a:1c:2b:3d:4e:5f brd ff:ff:ff:ff:ff:ff\n    inet 192.168.1.42/24 brd 192.168.1.255 scope global wlan0\n";
        assert_eq!(parse_ip_mac(text), ("192.168.1.42".into(), "5a:1c:2b:3d:4e:5f".into()));
        assert_eq!(parse_ip_mac("Device wlan0 does not exist."), (String::new(), String::new()));
    }

    #[test]
    fn proxy_normalisation_and_validation() {
        assert_eq!(normalize_proxy("null"), "");
        assert_eq!(normalize_proxy(":0"), "");
        assert_eq!(normalize_proxy("10.0.0.2:8888\n"), "10.0.0.2:8888");
        assert!(validate_proxy("10.0.0.2:8888").is_ok());
        assert!(validate_proxy("proxy.corp-net.example:3128").is_ok());
        for bad in ["", "host", "host:0", "host:99999", "a b:80", "h;reboot:80", "$(id):80", ":80"] {
            assert!(validate_proxy(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn root_detection() {
        assert!(is_root_id("uid=0(root) gid=0(root)"));
        assert!(!is_root_id("uid=2000(shell) gid=2000(shell)"));
    }

    #[test]
    fn uptime_and_counts_parse_and_read_naturally() {
        assert_eq!(parse_uptime("12345.67 98765.43\n"), Some(12_345));
        assert_eq!(parse_uptime(""), None);
        assert_eq!(parse_uptime("-1 2"), None);
        assert_eq!(format_uptime(420), "7 min");
        assert_eq!(format_uptime(5 * 3600 + 12 * 60), "5 h 12 min");
        assert_eq!(format_uptime(3 * 86_400 + 4 * 3600 + 59), "3 d 4 h");
        assert_eq!(parse_app_counts("12 98\n"), (Some(12), Some(98)));
        assert_eq!(parse_app_counts("x"), (None, None));
    }

    #[test]
    fn bootloader_state_comes_from_whichever_property_exists() {
        let props = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        assert_eq!(bootloader_unlocked(&props(&[("ro.boot.flash.locked", "1")])), Some(false));
        assert_eq!(bootloader_unlocked(&props(&[("ro.boot.flash.locked", "0")])), Some(true));
        assert_eq!(bootloader_unlocked(&props(&[("ro.boot.verifiedbootstate", "orange")])), Some(true));
        assert_eq!(bootloader_unlocked(&props(&[("ro.boot.verifiedbootstate", "green")])), Some(false));
        assert_eq!(bootloader_unlocked(&props(&[])), None);
    }
}

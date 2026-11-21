//! Installed packages: listing, details and lifecycle actions.

use std::collections::HashMap;
use std::time::Duration;

use super::perf::{parse_top_activity, TopActivity};
use super::{sh_quote, DeviceAdb};
use crate::error::{Error, Result};
use crate::process::{run, CommandSpec};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackageInfo {
    pub name: String,
    pub version_name: String,
    pub version_code: u64,
    pub min_sdk: Option<u32>,
    pub target_sdk: Option<u32>,
    pub first_install: String,
    pub last_update: String,
    pub system: bool,
    pub enabled: bool,
    pub apk_path: String,
    pub app_size: u64,
    pub data_size: u64,
    pub cache_size: u64,
}

impl PackageInfo {
    pub fn storage_total(&self) -> u64 {
        self.app_size + self.data_size + self.cache_size
    }
}

/// Valid Android package / process identifiers only; everything else is
/// rejected before it can reach a device command line.
pub fn validate_package(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 255
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if ok {
        Ok(name)
    } else {
        Err(Error::Invalid(format!("'{name}' is not a valid package name")))
    }
}

fn tokens_of<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
}

/// Parses the `Packages:` section of `dumpsys package packages`.
pub fn parse_package_dump(text: &str, user: u32) -> Vec<PackageInfo> {
    let user_marker = format!("User {user}:");
    let mut packages: Vec<PackageInfo> = Vec::new();
    let mut current: Option<PackageInfo> = None;
    let mut in_packages = false;

    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if indent == 0 {
            if let Some(p) = current.take() {
                packages.push(p);
            }
            in_packages = trimmed == "Packages:";
            continue;
        }
        if !in_packages {
            continue;
        }
        if indent == 2 {
            if let Some(p) = current.take() {
                packages.push(p);
            }
            if let Some(rest) = trimmed.strip_prefix("Package [") {
                if let Some((name, _)) = rest.split_once(']') {
                    current = Some(PackageInfo { name: name.to_string(), enabled: true, ..Default::default() });
                }
            }
            continue;
        }
        let Some(p) = current.as_mut() else { continue };
        if let Some(v) = trimmed.strip_prefix("versionName=") {
            p.version_name = v.to_string();
        } else if trimmed.starts_with("versionCode=") {
            p.version_code = tokens_of(trimmed, "versionCode").and_then(|v| v.parse().ok()).unwrap_or(0);
            p.min_sdk = tokens_of(trimmed, "minSdk").and_then(|v| v.parse().ok());
            p.target_sdk = tokens_of(trimmed, "targetSdk").and_then(|v| v.parse().ok());
        } else if let Some(v) = trimmed.strip_prefix("firstInstallTime=") {
            p.first_install = v.to_string();
        } else if let Some(v) = trimmed.strip_prefix("lastUpdateTime=") {
            p.last_update = v.to_string();
        } else if trimmed.starts_with("flags=[") {
            p.system = trimmed.split_whitespace().any(|t| t == "SYSTEM");
        } else if trimmed.starts_with(&user_marker) {
            if let Some(state) = tokens_of(trimmed, "enabled").and_then(|v| v.parse::<u8>().ok()) {
                p.enabled = state == 0 || state == 1;
            }
        }
    }
    if let Some(p) = current {
        packages.push(p);
    }
    packages
}

/// `package:/data/app/x/base.apk=com.x` lines -> name -> apk path.
pub fn parse_package_paths(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("package:")?;
            let (path, name) = rest.rsplit_once('=')?;
            Some((name.to_string(), path.to_string()))
        })
        .collect()
}

/// name -> (app, data, cache) bytes from `dumpsys diskstats`.
pub fn parse_diskstats(text: &str) -> HashMap<String, (u64, u64, u64)> {
    let array = |prefix: &str| -> Vec<serde_json::Value> {
        text.lines().find_map(|l| l.trim().strip_prefix(prefix)).and_then(|r| serde_json::from_str(r.trim()).ok()).unwrap_or_default()
    };
    let names = array("Package Names:");
    let (app, data, cache) = (array("App Sizes:"), array("App Data Sizes:"), array("Cache Sizes:"));
    let num = |v: &[serde_json::Value], i: usize| v.get(i).and_then(|x| x.as_u64()).unwrap_or(0);
    names.iter().enumerate().filter_map(|(i, n)| Some((n.as_str()?.to_string(), (num(&app, i), num(&data, i), num(&cache, i))))).collect()
}

/// How a permission is held, which decides whether adb can change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermKind {
    /// Only listed in the manifest (no grant state known).
    Requested,
    /// Granted at install time; fixed.
    Install,
    /// Runtime ("dangerous") permission: `pm grant` / `pm revoke` apply.
    Runtime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permission {
    pub name: String,
    pub granted: Option<bool>,
    pub kind: PermKind,
}

/// Permissions of a single package from `dumpsys package <name>`.
pub fn parse_permissions(text: &str) -> Vec<Permission> {
    let mut order: Vec<String> = Vec::new();
    let mut granted: HashMap<String, bool> = HashMap::new();
    let mut kinds: HashMap<String, PermKind> = HashMap::new();
    let mut section: Option<(usize, bool, PermKind)> = None; // (indent, has grant info, kind)

    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some((hdr_indent, with_grants, kind)) = section {
            if indent > hdr_indent {
                let name = t.split([':', ' ']).next().unwrap_or("");
                if name.contains('.') && !name.starts_with("gids") {
                    if !order.iter().any(|n| n == name) {
                        order.push(name.to_string());
                    }
                    let known = kinds.entry(name.to_string()).or_insert(kind);
                    *known = (*known).max(kind);
                    if with_grants {
                        if let Some(g) = tokens_of(t, "granted") {
                            granted.insert(name.to_string(), g.trim_end_matches(',') == "true");
                        }
                    }
                }
                continue;
            }
            section = None;
        }
        match t {
            "requested permissions:" => section = Some((indent, false, PermKind::Requested)),
            "install permissions:" => section = Some((indent, true, PermKind::Install)),
            "runtime permissions:" => section = Some((indent, true, PermKind::Runtime)),
            _ => {}
        }
    }
    order.into_iter().map(|name| Permission { granted: granted.get(&name).copied(), kind: kinds[&name], name }).collect()
}

/// Reads the outcome of `pm`/`am` commands that report in text.
fn expect_success(text: &str, what: &str) -> Result<()> {
    let t = text.trim();
    // Silent success is normal for `am`; pm/monkey report in words.
    if t.is_empty() || t.starts_with("Success") || t.contains("new state:") || t.contains("Events injected") {
        Ok(())
    } else {
        Err(Error::Invalid(format!("{what} failed: {t}")))
    }
}

impl DeviceAdb {
    pub async fn current_user(&self) -> u32 {
        self.shell("am get-current-user").await.ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0)
    }

    /// One round trip: package dump, APK paths and storage statistics.
    pub async fn list_packages(&self) -> Result<Vec<PackageInfo>> {
        let user = self.current_user().await;
        let script = format!(
            "dumpsys package packages\necho '@@adbdeck-separator@@'\npm list packages -f --user {user}\necho '@@adbdeck-separator@@'\ndumpsys diskstats"
        );
        let out = self.shell_large(&script, Duration::from_secs(120), 128 * 1024 * 1024).await?;
        let text = out.stdout_text().replace("\r\n", "\n");
        let parts = super::split_batch(&text, 3);
        let mut packages = parse_package_dump(&parts[0], user);
        if packages.is_empty() {
            return Err(Error::parse("package list", "the device returned no packages"));
        }
        let paths = parse_package_paths(&parts[1]);
        let sizes = parse_diskstats(&parts[2]);
        for p in &mut packages {
            p.apk_path = paths.get(&p.name).cloned().unwrap_or_default();
            if let Some((a, d, c)) = sizes.get(&p.name) {
                (p.app_size, p.data_size, p.cache_size) = (*a, *d, *c);
            }
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    pub async fn package_permissions(&self, package: &str) -> Result<Vec<Permission>> {
        let package = validate_package(package)?;
        let text = self.shell_output(&format!("dumpsys package {}", sh_quote(package)), Duration::from_secs(30)).await?;
        Ok(parse_permissions(&text.stdout_text()))
    }

    pub async fn apk_size(&self, path: &str) -> Option<u64> {
        if path.is_empty() {
            return None;
        }
        self.shell(&format!("stat -c %s {}", sh_quote(path))).await.ok()?.trim().parse().ok()
    }

    pub async fn foreground(&self) -> Result<TopActivity> {
        Ok(parse_top_activity(&self.shell("dumpsys activity top | grep ACTIVITY").await?))
    }

    /// Foreground activity and HTTP proxy in a single round trip.
    pub async fn status_snapshot(&self) -> Result<(TopActivity, String)> {
        let parts = self.shell_batch(&["dumpsys activity top | grep ACTIVITY", "settings get global http_proxy"]).await?;
        Ok((parse_top_activity(&parts[0]), super::overview::normalize_proxy(&parts[1])))
    }

    pub async fn install_apk(&self, apk: &std::path::Path) -> Result<()> {
        if !apk.is_file() {
            return Err(Error::Invalid(format!("{} is not a file", apk.display())));
        }
        let spec: CommandSpec = self.spec().arg("install").arg("-r").arg(apk).timeout(Duration::from_secs(300));
        let out = self.exec_spec(spec).await?;
        let text = format!("{}{}", out.stdout_text(), out.stderr);
        if out.success() && text.contains("Success") {
            Ok(())
        } else {
            Err(Error::Invalid(install_failure(&text)))
        }
    }

    pub async fn uninstall(&self, package: &str) -> Result<()> {
        let package = validate_package(package)?;
        let out = self.exec(["uninstall", package]).await?;
        expect_success(&format!("{}{}", out.stdout_text(), out.stderr), "Uninstall")
    }

    pub async fn force_stop(&self, package: &str) -> Result<()> {
        let package = validate_package(package)?;
        self.shell_ok(&format!("am force-stop {}", sh_quote(package))).await.map(|_| ())
    }

    pub async fn clear_data(&self, package: &str) -> Result<()> {
        let package = validate_package(package)?;
        let text = self.shell(&format!("pm clear {}", sh_quote(package))).await?;
        expect_success(&text, "Clearing data")
    }

    pub async fn set_package_enabled(&self, package: &str, enabled: bool) -> Result<()> {
        let package = validate_package(package)?;
        let q = sh_quote(package);
        let script = if enabled { format!("pm enable {q}") } else { format!("pm disable-user {q}") };
        let text = self.shell(&script).await?;
        expect_success(&text, if enabled { "Enabling" } else { "Disabling" })
    }

    pub async fn launch(&self, package: &str) -> Result<()> {
        let package = validate_package(package)?;
        let text = self.shell(&format!("monkey -p {} -c android.intent.category.LAUNCHER 1", sh_quote(package))).await?;
        if text.contains("No activities found") {
            Err(Error::Invalid("This app has no launchable activity".into()))
        } else {
            Ok(())
        }
    }

    pub async fn screenshot_png(&self) -> Result<Vec<u8>> {
        let out = self.exec_out(&["screencap", "-p"], 64 * 1024 * 1024, Duration::from_secs(30)).await?;
        if out.stdout.starts_with(b"\x89PNG") {
            Ok(out.stdout)
        } else {
            Err(Error::parse("screencap", "the device did not return a PNG image"))
        }
    }

    pub async fn pull_to(&self, remote: &str, local: &std::path::Path) -> Result<()> {
        let spec = self.spec().arg("pull").arg(remote).arg(local).timeout(Duration::from_secs(300));
        let out = run(&spec).await?;
        if out.success() {
            Ok(())
        } else {
            Err(Error::Invalid(out.failure_message()))
        }
    }
}

fn install_failure(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("Failure") || l.contains("INSTALL_") || l.starts_with("adb:"))
        .unwrap_or_else(|| text.trim());
    let hint = if line.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
        " The installed app was signed with a different key; uninstall it first."
    } else if line.contains("INSTALL_FAILED_VERSION_DOWNGRADE") {
        " A newer version is already installed."
    } else if line.contains("INSTALL_FAILED_INSUFFICIENT_STORAGE") {
        " The device is out of storage."
    } else {
        ""
    };
    format!("Installation failed: {line}.{hint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r"Activity Resolver Table:
  junk
Packages:
  Package [com.example.app] (1a2b3c):
    userId=10123
    codePath=/data/app/~~x/com.example.app-y
    versionCode=42 minSdk=24 targetSdk=34
    versionName=1.2.3
    flags=[ HAS_CODE ALLOW_CLEAR_USER_DATA ]
    firstInstallTime=2023-01-01 10:00:00
    lastUpdateTime=2023-06-01 11:30:00
    User 0: ceDataInode=123 installed=true hidden=false enabled=0 instant=false
  Package [com.android.settings] (4d5e6f):
    versionCode=34 targetSdk=34
    versionName=14
    flags=[ SYSTEM HAS_CODE ]
    User 0: ceDataInode=1 installed=true enabled=2 instant=false

Hidden system packages:
  Package [com.hidden.one] (0a0b0c):
    versionName=9
";

    #[test]
    fn parses_package_sections_only_from_the_packages_block() {
        let p = parse_package_dump(DUMP, 0);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].name, "com.example.app");
        assert_eq!(p[0].version_name, "1.2.3");
        assert_eq!(p[0].version_code, 42);
        assert_eq!((p[0].min_sdk, p[0].target_sdk), (Some(24), Some(34)));
        assert_eq!(p[0].first_install, "2023-01-01 10:00:00");
        assert!(p[0].enabled && !p[0].system);
        assert!(p[1].system && !p[1].enabled);
        assert_eq!(p[1].min_sdk, None);
    }

    #[test]
    fn other_users_state_is_ignored() {
        let p = parse_package_dump(DUMP, 10);
        assert!(p[1].enabled, "no User 10 entry keeps the default");
    }

    #[test]
    fn empty_dump_yields_nothing() {
        assert!(parse_package_dump("", 0).is_empty());
        assert!(parse_package_dump("Error: Can't find service: package", 0).is_empty());
    }

    #[test]
    fn package_paths_handle_equals_in_paths() {
        let m = parse_package_paths("package:/data/app/~~a==/com.x-b==/base.apk=com.x\npackage:/system/app/S.apk=com.s\n");
        assert_eq!(m["com.x"], "/data/app/~~a==/com.x-b==/base.apk");
        assert_eq!(m["com.s"], "/system/app/S.apk");
    }

    #[test]
    fn diskstats_arrays_are_zipped_by_index() {
        let text = "Package Names: [\"com.a\",\"com.b\"]\nApp Sizes: [100,200]\nApp Data Sizes: [10,20]\nCache Sizes: [1,2]\nOther: x\n";
        let m = parse_diskstats(text);
        assert_eq!(m["com.b"], (200, 20, 2));
        assert!(parse_diskstats("garbage").is_empty());
    }

    #[test]
    fn permissions_merge_requested_and_grant_state() {
        let text = "    requested permissions:\n      android.permission.INTERNET\n      android.permission.CAMERA\n      com.example.CUSTOM\n    install permissions:\n      android.permission.INTERNET: granted=true\n    User 0: ceDataInode=1\n      runtime permissions:\n        android.permission.CAMERA: granted=false, flags=[ USER_SET ]\n";
        let p = parse_permissions(text);
        let names: Vec<_> = p.iter().map(|x| (x.name.as_str(), x.granted, x.kind)).collect();
        assert_eq!(
            names,
            vec![
                ("android.permission.INTERNET", Some(true), PermKind::Install),
                ("android.permission.CAMERA", Some(false), PermKind::Runtime),
                ("com.example.CUSTOM", None, PermKind::Requested)
            ]
        );
    }

    #[test]
    fn package_names_are_validated() {
        assert!(validate_package("com.example.app_1").is_ok());
        assert!(validate_package("android").is_ok());
        for bad in ["", "1abc", "a b", "a;b", "a$(id)", "../x", "-r"] {
            assert!(validate_package(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn install_errors_are_explained() {
        let msg =
            install_failure("Performing Streamed Install\nadb: failed to install a.apk: Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: x]\n");
        assert!(msg.contains("different key"));
        assert!(install_failure("Failure [INSTALL_FAILED_INSUFFICIENT_STORAGE]").contains("storage"));
    }

    #[test]
    fn pm_result_interpretation() {
        assert!(expect_success("Success", "x").is_ok());
        assert!(expect_success("Package a new state: disabled-user", "x").is_ok());
        assert!(expect_success("Error: java.lang.IllegalArgumentException", "x").is_err());
    }
}

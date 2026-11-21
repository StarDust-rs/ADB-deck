//! Per-app management that `adb shell` is allowed to do without root:
//! runtime permissions, special access (app ops), cache clearing, background
//! limits, suspending, removing for the current user, and compiler filters.
//!
//! Every value that ends up on the device command line is validated or quoted;
//! the parsers are pure and tested with output in the formats of AOSP's shell
//! commands (`appops`, `am get-standby-bucket`, `dumpsys deviceidle`).

use std::time::Duration;

use super::apps::{parse_permissions, validate_package, Permission};
use super::{sh_quote, DeviceAdb};
use crate::error::{Error, Result};

/// Android permission names: dotted identifiers.
pub fn validate_permission(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 255
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if ok {
        Ok(name)
    } else {
        Err(Error::Invalid(format!("'{name}' is not a valid permission name")))
    }
}

/// App-op names are upper-case identifiers (`SYSTEM_ALERT_WINDOW`).
pub fn validate_op(name: &str) -> Result<&str> {
    let ok = !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(name)
    } else {
        Err(Error::Invalid(format!("'{name}' is not a valid app operation")))
    }
}

/// Special access that is not an ordinary runtime permission, with the label
/// Android's own settings screens use.
pub const SPECIAL_OPS: &[(&str, &str, &str)] = &[
    ("SYSTEM_ALERT_WINDOW", "Display over other apps", "Draw on top of other apps"),
    ("WRITE_SETTINGS", "Modify system settings", "Change system settings such as brightness and ringtones"),
    ("MANAGE_EXTERNAL_STORAGE", "All files access", "Read and write all files on shared storage"),
    ("REQUEST_INSTALL_PACKAGES", "Install unknown apps", "Install other apps from this one"),
    ("GET_USAGE_STATS", "Usage access", "See which apps are used and for how long"),
    ("SCHEDULE_EXACT_ALARM", "Alarms and reminders", "Schedule exact alarms"),
    ("RUN_ANY_IN_BACKGROUND", "Background activity", "Keep running when not in use"),
    ("PICTURE_IN_PICTURE", "Picture in picture", "Show a floating video window"),
    ("USE_FULL_SCREEN_INTENT", "Full-screen notifications", "Take over the screen for calls and alarms"),
    ("TURN_SCREEN_ON", "Turn the screen on", "Wake the display"),
];

/// Permissions that apps are commonly granted through adb because settings
/// offers no switch for them.
pub const ADB_ONLY_PERMISSIONS: &[(&str, &str)] = &[
    ("android.permission.WRITE_SECURE_SETTINGS", "Change secure system settings"),
    ("android.permission.READ_LOGS", "Read the system log"),
    ("android.permission.DUMP", "Read system service dumps"),
    ("android.permission.PACKAGE_USAGE_STATS", "Usage statistics"),
    ("android.permission.READ_PHONE_STATE", "Read phone state"),
    ("android.permission.CHANGE_CONFIGURATION", "Change system configuration"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpMode {
    Allow,
    Ignore,
    Deny,
    Default,
    Foreground,
    Other(String),
}

impl OpMode {
    pub fn parse(word: &str) -> OpMode {
        match word.trim().to_ascii_lowercase().as_str() {
            "allow" => OpMode::Allow,
            "ignore" => OpMode::Ignore,
            "deny" => OpMode::Deny,
            "default" => OpMode::Default,
            "foreground" => OpMode::Foreground,
            other => OpMode::Other(other.to_string()),
        }
    }

    /// The word `appops set` takes; `None` for modes that cannot be set.
    pub fn arg(&self) -> Option<&'static str> {
        Some(match self {
            OpMode::Allow => "allow",
            OpMode::Ignore => "ignore",
            OpMode::Deny => "deny",
            OpMode::Default => "default",
            OpMode::Foreground => "foreground",
            OpMode::Other(_) => return None,
        })
    }

    pub fn label(&self) -> String {
        match self {
            OpMode::Allow => "Allowed".into(),
            OpMode::Ignore => "Ignored".into(),
            OpMode::Deny => "Denied".into(),
            OpMode::Default => "Default".into(),
            OpMode::Foreground => "Only while in use".into(),
            OpMode::Other(o) => o.clone(),
        }
    }
}

/// Parses `appops get <package>`: lines like `SYSTEM_ALERT_WINDOW: allow; time=+3d ago`
/// and `Uid mode: RUN_IN_BACKGROUND: allow`.
pub fn parse_app_ops(text: &str) -> Vec<(String, OpMode)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim().strip_prefix("Uid mode:").map_or(line.trim(), str::trim);
            let (name, rest) = line.split_once(':')?;
            let name = name.trim();
            let valid = !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            let mode = rest.trim().split([';', ' ']).next().unwrap_or("");
            (valid && !mode.is_empty()).then(|| (name.to_string(), OpMode::parse(mode)))
        })
        .collect()
}

/// App standby buckets, from most to least permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Active,
    WorkingSet,
    Frequent,
    Rare,
    Restricted,
}

impl Bucket {
    pub const ALL: [Bucket; 5] = [Bucket::Active, Bucket::WorkingSet, Bucket::Frequent, Bucket::Rare, Bucket::Restricted];

    /// The name `am set-standby-bucket` takes.
    pub fn arg(self) -> &'static str {
        match self {
            Bucket::Active => "active",
            Bucket::WorkingSet => "working_set",
            Bucket::Frequent => "frequent",
            Bucket::Rare => "rare",
            Bucket::Restricted => "restricted",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Bucket::Active => "Active",
            Bucket::WorkingSet => "Working set",
            Bucket::Frequent => "Frequent",
            Bucket::Rare => "Rare",
            Bucket::Restricted => "Restricted",
        }
    }

    /// Parses the answer of `am get-standby-bucket`: the bucket number or its name.
    pub fn parse(text: &str) -> Option<Bucket> {
        let t = text.trim().to_ascii_lowercase();
        Some(match t.as_str() {
            "10" | "active" => Bucket::Active,
            "20" | "working_set" => Bucket::WorkingSet,
            "30" | "frequent" => Bucket::Frequent,
            "40" | "rare" => Bucket::Rare,
            "45" | "restricted" => Bucket::Restricted,
            _ => return None,
        })
    }
}

/// True when `package` is on the battery-optimisation allow list, from `dumpsys deviceidle whitelist`
/// (lines like `user,com.example,10123`).
pub fn parse_battery_exempt(text: &str, package: &str) -> bool {
    text.lines().any(|l| {
        let mut f = l.trim().split(',');
        matches!((f.next(), f.next()), (Some("user" | "system" | "system-excidle"), Some(p)) if p == package)
    })
}

/// State flags from the `User N:` line of `dumpsys package`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserState {
    pub hidden: bool,
    pub suspended: bool,
    pub stopped: bool,
}

pub fn parse_user_state(text: &str) -> UserState {
    let flag = |line: &str, key: &str| line.split_whitespace().any(|t| t.strip_prefix(key) == Some("=true"));
    text.lines()
        .find(|l| l.trim_start().starts_with("User ") && l.contains("installed="))
        .map(|l| UserState { hidden: flag(l, "hidden"), suspended: flag(l, "suspended"), stopped: flag(l, "stopped") })
        .unwrap_or_default()
}

/// Everything the permission and advanced tabs show, read in one round trip.
#[derive(Debug, Clone, PartialEq)]
pub struct AppDetails {
    pub package: String,
    pub permissions: Vec<Permission>,
    pub ops: Vec<(String, OpMode)>,
    pub bucket: Option<Bucket>,
    pub battery_exempt: bool,
    pub state: UserState,
}

impl AppDetails {
    pub fn op_mode(&self, op: &str) -> OpMode {
        self.ops.iter().find(|(n, _)| n == op).map_or(OpMode::Default, |(_, m)| m.clone())
    }

    pub fn permission(&self, name: &str) -> Option<&Permission> {
        self.permissions.iter().find(|p| p.name == name)
    }
}

/// Which caches an attempt emptied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClearedCache {
    pub internal: bool,
    pub external: bool,
}

/// Reads the verdict of `pm`/`cmd package` commands that answer in words.
fn pm_verdict(text: &str, what: &str) -> Result<()> {
    let t = text.trim();
    let failed =
        t.contains("Exception") || t.starts_with("Error") || t.contains("Failure") || t.contains("Unknown") || t.contains("not found");
    if failed {
        Err(Error::Invalid(format!("{what} failed: {}", t.lines().next().unwrap_or(t))))
    } else {
        Ok(())
    }
}

impl DeviceAdb {
    /// Reads permissions, special access, standby bucket, battery exemption and
    /// state flags of one app.
    pub async fn app_details(&self, package: &str) -> Result<AppDetails> {
        let package = validate_package(package)?;
        let q = sh_quote(package);
        let parts = self
            .shell_batch(&[
                &format!("dumpsys package {q}"),
                &format!("appops get {q} 2>&1"),
                &format!("am get-standby-bucket {q} 2>&1"),
                "dumpsys deviceidle whitelist 2>&1",
            ])
            .await?;
        Ok(AppDetails {
            package: package.to_string(),
            permissions: parse_permissions(&parts[0]),
            ops: parse_app_ops(&parts[1]),
            bucket: Bucket::parse(&parts[2]),
            battery_exempt: parse_battery_exempt(&parts[3], package),
            state: parse_user_state(&parts[0]),
        })
    }

    /// Grants or revokes a runtime permission.
    pub async fn set_permission(&self, package: &str, permission: &str, grant: bool) -> Result<()> {
        let (package, permission) = (validate_package(package)?, validate_permission(permission)?);
        let verb = if grant { "grant" } else { "revoke" };
        let out = self.shell_output(&format!("pm {verb} {} {}", sh_quote(package), sh_quote(permission)), Duration::from_secs(20)).await?;
        let text = format!("{}{}", out.stdout_text(), out.stderr);
        if out.success() && text.trim().is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid(permission_failure(&text, permission, grant)))
        }
    }

    pub async fn set_app_op(&self, package: &str, op: &str, mode: &OpMode) -> Result<()> {
        let (package, op) = (validate_package(package)?, validate_op(op)?);
        let Some(arg) = mode.arg() else { return Err(Error::Invalid("that mode cannot be set".into())) };
        let text = self.shell(&format!("appops set {} {op} {arg} 2>&1", sh_quote(package))).await?;
        pm_verdict(&text, "Changing the permission")
    }

    pub async fn set_standby_bucket(&self, package: &str, bucket: Bucket) -> Result<()> {
        let package = validate_package(package)?;
        let text = self.shell(&format!("am set-standby-bucket {} {} 2>&1", sh_quote(package), bucket.arg())).await?;
        pm_verdict(&text, "Changing the standby bucket")
    }

    /// Adds or removes the app from the battery-optimisation allow list.
    pub async fn set_battery_exempt(&self, package: &str, exempt: bool) -> Result<()> {
        let package = validate_package(package)?;
        let sign = if exempt { '+' } else { '-' };
        let text = self.shell(&format!("cmd deviceidle whitelist {sign}{} 2>&1", sh_quote(package))).await?;
        pm_verdict(&text, "Changing battery optimisation")
    }

    pub async fn set_suspended(&self, package: &str, suspended: bool) -> Result<()> {
        let package = validate_package(package)?;
        let verb = if suspended { "suspend" } else { "unsuspend" };
        let text = self.shell(&format!("pm {verb} {} 2>&1", sh_quote(package))).await?;
        pm_verdict(&text, "Suspending")
    }

    pub async fn set_hidden(&self, package: &str, hidden: bool) -> Result<()> {
        let package = validate_package(package)?;
        let verb = if hidden { "hide" } else { "unhide" };
        let text = self.shell(&format!("pm {verb} {} 2>&1", sh_quote(package))).await?;
        pm_verdict(&text, "Hiding")
    }

    /// Removes the app for the current user but keeps its data and the APK on
    /// the system partition, so [`install_existing`](Self::install_existing) can bring it back.
    pub async fn remove_for_user(&self, package: &str) -> Result<()> {
        let package = validate_package(package)?;
        let user = self.current_user().await;
        let text = self.shell(&format!("pm uninstall -k --user {user} {} 2>&1", sh_quote(package))).await?;
        if text.trim().starts_with("Success") {
            Ok(())
        } else {
            Err(Error::Invalid(format!("Removing the app failed: {}", text.trim())))
        }
    }

    /// Brings back an app that was removed for the current user.
    pub async fn install_existing(&self, package: &str) -> Result<()> {
        let package = validate_package(package)?;
        let user = self.current_user().await;
        let text = self.shell(&format!("cmd package install-existing --user {user} {} 2>&1", sh_quote(package))).await?;
        if text.contains("installed for user") || text.trim().starts_with("Success") {
            Ok(())
        } else {
            Err(Error::Invalid(format!("Restoring the app failed: {}", text.trim())))
        }
    }

    /// Recompiles the app (`speed-profile` is Android's usual, `speed` the
    /// most aggressive useful filter) or resets it to the system default.
    pub async fn compile(&self, package: &str, filter: Option<&str>) -> Result<()> {
        let package = validate_package(package)?;
        let q = sh_quote(package);
        let script = match filter {
            Some(f) if f.chars().all(|c| c.is_ascii_lowercase() || c == '-') && !f.is_empty() => {
                format!("cmd package compile -m {f} -f {q} 2>&1")
            }
            Some(f) => return Err(Error::Invalid(format!("'{f}' is not a compiler filter"))),
            None => format!("cmd package compile --reset {q} 2>&1"),
        };
        let out = self.shell_output(&script, Duration::from_secs(300)).await?;
        let text = out.stdout_text();
        if text.contains("Success") {
            Ok(())
        } else {
            Err(Error::Invalid(format!("Compiling failed: {}", text.trim())))
        }
    }

    pub async fn kill_background(&self, package: &str) -> Result<()> {
        let package = validate_package(package)?;
        self.shell_ok(&format!("am kill {}", sh_quote(package))).await.map(|_| ())
    }

    /// Opens the app's system settings page on the phone.
    pub async fn open_app_settings(&self, package: &str) -> Result<()> {
        let package = validate_package(package)?;
        let text = self.shell(&format!("am start -a android.settings.APPLICATION_DETAILS_SETTINGS -d package:{package} 2>&1")).await?;
        pm_verdict(&text, "Opening the settings page")
    }

    /// Empties the cache of one app by the strongest route that works:
    /// `pm clear --cache-only` (Android 14+), `run-as` (debuggable apps), and
    /// the app's cache folder on shared storage.
    pub async fn clear_app_cache(&self, package: &str) -> Result<ClearedCache> {
        let package = validate_package(package)?;
        let q = sh_quote(package);
        let user = self.current_user().await;
        let mut done = ClearedCache::default();

        let sdk: u32 = self.shell("getprop ro.build.version.sdk").await.ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
        if sdk >= 34 {
            let text = self.shell(&format!("pm clear --cache-only --user {user} {q} 2>&1")).await?;
            if text.trim().starts_with("Success") {
                return Ok(ClearedCache { internal: true, external: true });
            }
        }
        let out = self.shell_output(&format!("run-as {q} sh -c 'rm -rf cache/* code_cache/*' 2>&1"), Duration::from_secs(15)).await?;
        done.internal = out.success() && out.stdout_text().trim().is_empty();

        let dir = sh_quote(&format!("/storage/emulated/{user}/Android/data/{package}/cache"));
        let script = format!(
            "d={dir}; if [ ! -d \"$d\" ]; then echo nodir; elif ls \"$d\" >/dev/null 2>&1; then rm -rf \"$d\"/* 2>/dev/null; \
             if [ -z \"$(ls -A \"$d\" 2>/dev/null)\" ]; then echo cleared; else echo partial; fi; else echo denied; fi"
        );
        let verdict = self.shell(&script).await?;
        done.external = matches!(verdict.trim(), "cleared" | "nodir");
        if done.internal || verdict.trim() == "cleared" {
            Ok(done)
        } else {
            Err(Error::Invalid(
                "adb cannot reach this app's private cache before Android 14 unless the app is debuggable. \
                 Use \"Clear all caches\" to free the cache of every app at once."
                    .into(),
            ))
        }
    }

    /// Frees the cache of every app (`pm trim-caches` with a target larger than the storage).
    pub async fn trim_all_caches(&self) -> Result<()> {
        let out = self.shell_output("pm trim-caches 4096G 2>&1", Duration::from_secs(120)).await?;
        pm_verdict(&out.stdout_text(), "Clearing caches")
    }
}

/// Explains why `pm grant` / `pm revoke` refused.
pub fn permission_failure(text: &str, permission: &str, grant: bool) -> String {
    let t = text.trim();
    let short = permission.rsplit('.').next().unwrap_or(permission);
    if t.contains("not a changeable permission type") {
        format!("{short} is fixed at install time and cannot be changed")
    } else if t.contains("has not requested permission") {
        format!("This app does not declare {short}")
    } else if t.contains("Unknown permission") {
        format!("The phone does not know the permission {short}")
    } else if t.contains("SecurityException") && t.contains("grant") {
        format!("The phone does not allow adb to {} {short}", if grant { "grant" } else { "revoke" })
    } else if t.is_empty() {
        format!("Changing {short} failed")
    } else {
        format!("Changing {short} failed: {}", t.lines().next().unwrap_or(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated_before_reaching_a_shell() {
        assert!(validate_permission("android.permission.CAMERA").is_ok());
        for bad in ["", "a b", "a;rm", "$(id)", "a/b", "1a"] {
            assert!(validate_permission(bad).is_err(), "{bad}");
        }
        assert!(validate_op("SYSTEM_ALERT_WINDOW").is_ok());
        for bad in ["", "lower", "A B", "A;B", "A$"] {
            assert!(validate_op(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn app_ops_parse_modes_and_ignore_noise() {
        let text = "SYSTEM_ALERT_WINDOW: allow; time=+3d2h ago\nWRITE_SETTINGS: deny\nUid mode: RUN_IN_BACKGROUND: allow\nCOARSE_LOCATION: foreground; time=+1m ago\nNo operations.\nOP_X: default\n";
        let ops = parse_app_ops(text);
        assert_eq!(
            ops,
            vec![
                ("SYSTEM_ALERT_WINDOW".to_string(), OpMode::Allow),
                ("WRITE_SETTINGS".to_string(), OpMode::Deny),
                ("RUN_IN_BACKGROUND".to_string(), OpMode::Allow),
                ("COARSE_LOCATION".to_string(), OpMode::Foreground),
                ("OP_X".to_string(), OpMode::Default),
            ]
        );
        assert!(parse_app_ops("").is_empty());
        assert!(parse_app_ops("Error: package not found").is_empty(), "prose is not an operation");
    }

    #[test]
    fn op_modes_round_trip_to_their_arguments() {
        for m in [OpMode::Allow, OpMode::Ignore, OpMode::Deny, OpMode::Default, OpMode::Foreground] {
            assert_eq!(OpMode::parse(m.arg().unwrap()), m);
        }
        assert!(OpMode::Other("errored".into()).arg().is_none());
    }

    #[test]
    fn standby_buckets_parse_numbers_and_names() {
        assert_eq!(Bucket::parse("10\n"), Some(Bucket::Active));
        assert_eq!(Bucket::parse("45"), Some(Bucket::Restricted));
        assert_eq!(Bucket::parse("working_set"), Some(Bucket::WorkingSet));
        assert_eq!(Bucket::parse("50"), None);
        assert_eq!(Bucket::parse("Exception occurred"), None);
        for b in Bucket::ALL {
            assert_eq!(Bucket::parse(b.arg()), Some(b));
        }
    }

    #[test]
    fn battery_exemption_matches_whole_package_names() {
        let text = "system-excidle,com.android.shell,2000\nuser,com.example.app,10123\nsystem,android,1000\n";
        assert!(parse_battery_exempt(text, "com.example.app"));
        assert!(parse_battery_exempt(text, "com.android.shell"));
        assert!(!parse_battery_exempt(text, "com.example"), "a prefix is not a match");
        assert!(!parse_battery_exempt("", "com.example.app"));
    }

    #[test]
    fn user_state_flags_come_from_the_user_line() {
        let text = "  Packages:\n    User 0: ceDataInode=1234 deviceProtectedDataInode=0 installed=true hidden=false suspended=true distractionFlags=0 stopped=true notLaunched=false enabled=0\n";
        assert_eq!(parse_user_state(text), UserState { hidden: false, suspended: true, stopped: true });
        assert_eq!(parse_user_state("nothing here"), UserState::default());
    }

    #[test]
    fn pm_verdicts_catch_errors_and_pass_success() {
        assert!(pm_verdict("", "x").is_ok());
        assert!(pm_verdict("Package a.b new suspended state: true", "x").is_ok());
        assert!(pm_verdict("Exception occurred while executing 'hide': java.lang.SecurityException", "x").is_err());
        assert!(pm_verdict("Error: Unknown command", "x").is_err());
    }

    #[test]
    fn permission_failures_explain_themselves() {
        let fixed = permission_failure("Operation not allowed: java.lang.SecurityException: Permission android.permission.INTERNET requested by a is not a changeable permission type", "android.permission.INTERNET", true);
        assert!(fixed.contains("INTERNET is fixed at install time"));
        assert!(permission_failure(
            "java.lang.SecurityException: Package a has not requested permission android.permission.CAMERA",
            "android.permission.CAMERA",
            true
        )
        .contains("does not declare CAMERA"));
        assert!(permission_failure("", "android.permission.CAMERA", false).contains("CAMERA"));
    }

    #[test]
    fn details_lookups_default_sensibly() {
        let d = AppDetails {
            package: "a.b".into(),
            permissions: vec![],
            ops: vec![("WRITE_SETTINGS".into(), OpMode::Allow)],
            bucket: None,
            battery_exempt: false,
            state: UserState::default(),
        };
        assert_eq!(d.op_mode("WRITE_SETTINGS"), OpMode::Allow);
        assert_eq!(d.op_mode("SYSTEM_ALERT_WINDOW"), OpMode::Default);
    }
}

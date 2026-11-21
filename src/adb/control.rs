//! Device controls: radios, display, developer toggles, battery and doze
//! simulation, power. Everything `adb shell` may do without root.
//!
//! A [`Command`] is a closed set of typed actions; [`Command::script`] is the
//! only place that turns one into shell text, and it only ever formats numbers
//! and fixed words, so nothing the user types reaches the device command line
//! unchecked.

use std::time::Duration;

use super::overview::parse_screen;
use super::DeviceAdb;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DarkMode {
    Auto,
    Light,
    Dark,
}

impl DarkMode {
    pub const ALL: [DarkMode; 3] = [DarkMode::Auto, DarkMode::Light, DarkMode::Dark];

    fn arg(self) -> &'static str {
        match self {
            DarkMode::Auto => "auto",
            DarkMode::Light => "no",
            DarkMode::Dark => "yes",
        }
    }

    /// Parses `cmd uimode night` ("Night mode: yes").
    pub fn parse(text: &str) -> Option<DarkMode> {
        let word = text.rsplit(':').next()?.trim();
        DarkMode::ALL.into_iter().find(|m| m.arg() == word)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Auto,
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

impl Rotation {
    pub const ALL: [Rotation; 5] = [Rotation::Auto, Rotation::Deg0, Rotation::Deg90, Rotation::Deg180, Rotation::Deg270];

    pub fn label(self) -> &'static str {
        match self {
            Rotation::Auto => "Auto",
            Rotation::Deg0 => "0°",
            Rotation::Deg90 => "90°",
            Rotation::Deg180 => "180°",
            Rotation::Deg270 => "270°",
        }
    }

    fn quarter_turns(self) -> Option<u8> {
        match self {
            Rotation::Auto => None,
            Rotation::Deg0 => Some(0),
            Rotation::Deg90 => Some(1),
            Rotation::Deg180 => Some(2),
            Rotation::Deg270 => Some(3),
        }
    }

    /// From the `accelerometer_rotation` and `user_rotation` settings.
    pub fn from_settings(accelerometer: &str, user: &str) -> Rotation {
        if accelerometer.trim() != "0" {
            return Rotation::Auto;
        }
        match user.trim() {
            "1" => Rotation::Deg90,
            "2" => Rotation::Deg180,
            "3" => Rotation::Deg270,
            _ => Rotation::Deg0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebootTarget {
    System,
    Recovery,
    Bootloader,
    PowerOff,
}

impl RebootTarget {
    pub fn label(self) -> &'static str {
        match self {
            RebootTarget::System => "Reboot",
            RebootTarget::Recovery => "Reboot to recovery",
            RebootTarget::Bootloader => "Reboot to bootloader",
            RebootTarget::PowerOff => "Power off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBar {
    Notifications,
    QuickSettings,
    Collapse,
}

/// One thing the user can ask the device to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Wifi(bool),
    MobileData(bool),
    Bluetooth(bool),
    Nfc(bool),
    Airplane(bool),
    StayAwake(bool),
    ShowTouches(bool),
    PointerLocation(bool),
    DarkMode(DarkMode),
    Rotation(Rotation),
    /// Window, transition and animator scale together; 0 turns animations off.
    AnimationScale(f32),
    /// 0–255; also switches automatic brightness off.
    Brightness(u8),
    AutoBrightness(bool),
    ScreenTimeoutSecs(u32),
    /// `None` restores the panel's own size.
    Size(Option<(u32, u32)>),
    /// `None` restores the panel's own density.
    Density(Option<u32>),
    /// Pretend the battery is at this level and unplugged.
    BatteryLevel(u8),
    BatteryReset,
    /// Forces the device into (`true`) or out of deep doze.
    Doze(bool),
    StatusBar(StatusBar),
    Wake,
    Sleep,
    Reboot(RebootTarget),
}

fn onoff(on: bool, yes: &'static str, no: &'static str) -> &'static str {
    if on {
        yes
    } else {
        no
    }
}

impl Command {
    /// The text shown after a command succeeded.
    pub fn done(&self) -> String {
        let state = |on: &bool| onoff(*on, "on", "off");
        match self {
            Command::Wifi(on) => format!("Wi-Fi turned {}", state(on)),
            Command::MobileData(on) => format!("Mobile data turned {}", state(on)),
            Command::Bluetooth(on) => format!("Bluetooth turned {}", state(on)),
            Command::Nfc(on) => format!("NFC turned {}", state(on)),
            Command::Airplane(on) => format!("Airplane mode turned {}", state(on)),
            Command::StayAwake(on) => format!("Stay awake while charging turned {}", state(on)),
            Command::ShowTouches(on) => format!("Show touches turned {}", state(on)),
            Command::PointerLocation(on) => format!("Pointer location turned {}", state(on)),
            Command::DarkMode(m) => format!(
                "Appearance set to {}",
                match m {
                    DarkMode::Auto => "automatic",
                    DarkMode::Light => "light",
                    DarkMode::Dark => "dark",
                }
            ),
            Command::Rotation(r) => format!("Rotation set to {}", r.label()),
            Command::AnimationScale(s) if *s == 0.0 => "Animations turned off".into(),
            Command::AnimationScale(s) => format!("Animation scale set to {s}×"),
            Command::Brightness(b) => format!("Brightness set to {}%", u32::from(*b) * 100 / 255),
            Command::AutoBrightness(on) => format!("Adaptive brightness turned {}", state(on)),
            Command::ScreenTimeoutSecs(s) => format!("Screen timeout set to {s} s"),
            Command::Size(Some((w, h))) => format!("Screen size set to {w}×{h}"),
            Command::Size(None) => "Screen size reset".into(),
            Command::Density(Some(d)) => format!("Screen density set to {d} dpi"),
            Command::Density(None) => "Screen density reset".into(),
            Command::BatteryLevel(l) => format!("Battery simulated at {l}%"),
            Command::BatteryReset => "Battery reporting is back to normal".into(),
            Command::Doze(true) => "Device forced into doze".into(),
            Command::Doze(false) => "Device left doze".into(),
            Command::StatusBar(_) => "Status bar updated".into(),
            Command::Wake => "Screen woken".into(),
            Command::Sleep => "Screen turned off".into(),
            Command::Reboot(t) => format!("{} requested", t.label()),
        }
    }

    /// Commands that interrupt what the user is doing on the phone.
    pub fn needs_confirmation(&self) -> bool {
        matches!(self, Command::Reboot(_))
    }

    /// The shell script for this command, after range checks.
    pub fn script(&self) -> Result<String> {
        let range = |name: &str, value: u32, lo: u32, hi: u32| {
            if (lo..=hi).contains(&value) {
                Ok(value)
            } else {
                Err(Error::Invalid(format!("{name} must be between {lo} and {hi}")))
            }
        };
        Ok(match self {
            Command::Wifi(on) => format!("svc wifi {}", onoff(*on, "enable", "disable")),
            Command::MobileData(on) => format!("svc data {}", onoff(*on, "enable", "disable")),
            Command::Bluetooth(on) => format!("svc bluetooth {}", onoff(*on, "enable", "disable")),
            Command::Nfc(on) => format!("svc nfc {}", onoff(*on, "enable", "disable")),
            Command::Airplane(on) => format!("cmd connectivity airplane-mode {}", onoff(*on, "enable", "disable")),
            Command::StayAwake(on) => format!("svc power stayon {}", onoff(*on, "true", "false")),
            Command::ShowTouches(on) => format!("settings put system show_touches {}", u8::from(*on)),
            Command::PointerLocation(on) => format!("settings put system pointer_location {}", u8::from(*on)),
            Command::DarkMode(m) => format!("cmd uimode night {}", m.arg()),
            Command::Rotation(r) => match r.quarter_turns() {
                None => "settings put system accelerometer_rotation 1".into(),
                Some(n) => format!("settings put system accelerometer_rotation 0; settings put system user_rotation {n}"),
            },
            Command::AnimationScale(s) => {
                if !(0.0..=10.0).contains(s) {
                    return Err(Error::Invalid("animation scale must be between 0 and 10".into()));
                }
                ["window_animation_scale", "transition_animation_scale", "animator_duration_scale"]
                    .iter()
                    .map(|k| format!("settings put global {k} {s}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            }
            Command::Brightness(b) => format!("settings put system screen_brightness_mode 0; settings put system screen_brightness {b}"),
            Command::AutoBrightness(on) => format!("settings put system screen_brightness_mode {}", u8::from(*on)),
            Command::ScreenTimeoutSecs(s) => {
                format!("settings put system screen_off_timeout {}", range("Screen timeout", *s, 5, 86_400)? * 1000)
            }
            Command::Size(Some((w, h))) => format!("wm size {}x{}", range("Width", *w, 200, 10_000)?, range("Height", *h, 200, 10_000)?),
            Command::Size(None) => "wm size reset".into(),
            Command::Density(Some(d)) => format!("wm density {}", range("Density", *d, 72, 1000)?),
            Command::Density(None) => "wm density reset".into(),
            Command::BatteryLevel(l) => {
                format!("dumpsys battery unplug; dumpsys battery set level {}", range("Battery level", u32::from(*l), 0, 100)?)
            }
            Command::BatteryReset => "dumpsys battery reset".into(),
            Command::Doze(true) => "dumpsys deviceidle force-idle".into(),
            Command::Doze(false) => "dumpsys deviceidle unforce".into(),
            Command::StatusBar(StatusBar::Notifications) => "cmd statusbar expand-notifications".into(),
            Command::StatusBar(StatusBar::QuickSettings) => "cmd statusbar expand-settings".into(),
            Command::StatusBar(StatusBar::Collapse) => "cmd statusbar collapse".into(),
            Command::Wake => "input keyevent KEYCODE_WAKEUP".into(),
            Command::Sleep => "input keyevent KEYCODE_SLEEP".into(),
            Command::Reboot(_) => return Err(Error::Invalid("rebooting is a host command, not a shell script".into())),
        })
    }
}

/// What the controls panel shows; every field is `None` when the phone did
/// not answer in a recognisable way, so the panel can say "unknown".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Controls {
    pub wifi: Option<bool>,
    pub mobile_data: Option<bool>,
    pub bluetooth: Option<bool>,
    pub airplane: Option<bool>,
    pub stay_awake: Option<bool>,
    pub show_touches: Option<bool>,
    pub pointer_location: Option<bool>,
    pub dark_mode: Option<DarkMode>,
    pub rotation: Option<Rotation>,
    pub animation_scale: Option<f32>,
    pub brightness: Option<u8>,
    pub auto_brightness: Option<bool>,
    pub screen_timeout_secs: Option<u32>,
    pub battery: Battery,
    pub size: String,
    pub density: String,
    pub physical_size: String,
    pub physical_density: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Battery {
    pub level: Option<u8>,
    pub charging: bool,
    pub temperature_tenths: Option<i32>,
}

/// `settings get` prints `null` for unset keys; `1`/`0` are the booleans.
pub fn parse_flag(text: &str) -> Option<bool> {
    match text.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

pub fn parse_number<T: std::str::FromStr>(text: &str) -> Option<T> {
    text.trim().parse().ok()
}

/// From `dumpsys battery`; powered-by lines tell whether it is charging.
pub fn parse_battery(text: &str) -> Battery {
    let field = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.trim().split_once(':')?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
    };
    Battery {
        level: field("level").and_then(|v| v.parse().ok()),
        charging: ["AC powered", "USB powered", "Wireless powered"].iter().any(|k| field(k).as_deref() == Some("true")),
        temperature_tenths: field("temperature").and_then(|v| v.parse().ok()),
    }
}

const READS: [&str; 15] = [
    "settings get global airplane_mode_on",
    "settings get global mobile_data",
    "settings get global wifi_on",
    "settings get global bluetooth_on",
    "settings get global stay_on_while_plugged_in",
    "settings get system show_touches",
    "settings get system pointer_location",
    "cmd uimode night",
    "settings get global window_animation_scale",
    "settings get system screen_brightness",
    "settings get system screen_brightness_mode",
    "settings get system screen_off_timeout",
    "dumpsys battery",
    "wm size; wm density",
    "echo $(settings get system accelerometer_rotation) $(settings get system user_rotation)",
];

impl DeviceAdb {
    pub async fn controls(&self) -> Result<Controls> {
        let p = self.shell_batch(&READS).await?;
        let screen = parse_screen(&p[13]);
        let rotation = p[14].split_whitespace().collect::<Vec<_>>();
        Ok(Controls {
            airplane: parse_flag(&p[0]),
            mobile_data: parse_flag(&p[1]),
            wifi: parse_flag(&p[2]),
            bluetooth: parse_flag(&p[3]),
            // The setting is a bitmask of power sources (AC 1, USB 2, wireless 4); any bit means "stay on".
            stay_awake: parse_number::<u32>(&p[4]).map(|v| v != 0),
            show_touches: parse_flag(&p[5]),
            pointer_location: parse_flag(&p[6]),
            dark_mode: DarkMode::parse(&p[7]),
            animation_scale: parse_number(&p[8]),
            brightness: parse_number(&p[9]),
            auto_brightness: parse_flag(&p[10]),
            screen_timeout_secs: parse_number::<u32>(&p[11]).map(|ms| ms / 1000),
            battery: parse_battery(&p[12]),
            rotation: (rotation.len() == 2).then(|| Rotation::from_settings(rotation[0], rotation[1])),
            size: screen.resolution,
            density: screen.density,
            physical_size: screen.physical_resolution,
            physical_density: screen.physical_density,
        })
    }

    /// Carries out one command and reads the device's verdict.
    pub async fn apply(&self, command: &Command) -> Result<String> {
        if let Command::Reboot(target) = command {
            return self.reboot(*target).await.map(|()| command.done());
        }
        let out = self.shell_output(&command.script()?, Duration::from_secs(20)).await?;
        let text = format!("{}{}", out.stdout_text(), out.stderr);
        let t = text.trim();
        if !out.success() || t.contains("Exception") || t.starts_with("Error") || t.contains("Unknown command") || t.contains("not found") {
            let first = t.lines().next().unwrap_or("the phone refused");
            return Err(Error::Invalid(format!("{}: {first}", command.done().replace(" turned", " could not be turned"))));
        }
        Ok(command.done())
    }

    async fn reboot(&self, target: RebootTarget) -> Result<()> {
        let args: &[&str] = match target {
            RebootTarget::System => &["reboot"],
            RebootTarget::Recovery => &["reboot", "recovery"],
            RebootTarget::Bootloader => &["reboot", "bootloader"],
            // `adb reboot` has no power-off; the shell command does.
            RebootTarget::PowerOff => &["shell", "reboot", "-p"],
        };
        let out = self.exec(args.iter().copied()).await?;
        if out.success() {
            Ok(())
        } else {
            Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_become_the_expected_commands() {
        let cases = [
            (Command::Wifi(true), "svc wifi enable"),
            (Command::MobileData(false), "svc data disable"),
            (Command::Bluetooth(true), "svc bluetooth enable"),
            (Command::Airplane(true), "cmd connectivity airplane-mode enable"),
            (Command::StayAwake(true), "svc power stayon true"),
            (Command::ShowTouches(false), "settings put system show_touches 0"),
            (Command::DarkMode(DarkMode::Dark), "cmd uimode night yes"),
            (Command::DarkMode(DarkMode::Light), "cmd uimode night no"),
            (Command::Doze(true), "dumpsys deviceidle force-idle"),
            (Command::Size(None), "wm size reset"),
            (Command::Density(Some(420)), "wm density 420"),
        ];
        for (command, script) in cases {
            assert_eq!(command.script().unwrap(), script, "{command:?}");
        }
    }

    #[test]
    fn multi_step_commands_run_in_the_right_order() {
        assert_eq!(
            Command::Rotation(Rotation::Deg90).script().unwrap(),
            "settings put system accelerometer_rotation 0; settings put system user_rotation 1"
        );
        assert_eq!(Command::Rotation(Rotation::Auto).script().unwrap(), "settings put system accelerometer_rotation 1");
        assert_eq!(Command::BatteryLevel(15).script().unwrap(), "dumpsys battery unplug; dumpsys battery set level 15");
        let anim = Command::AnimationScale(0.5).script().unwrap();
        assert_eq!(anim.matches("settings put global").count(), 3);
        assert!(anim.contains("window_animation_scale 0.5") && anim.contains("animator_duration_scale 0.5"));
        assert!(Command::Brightness(200).script().unwrap().starts_with("settings put system screen_brightness_mode 0;"));
    }

    #[test]
    fn out_of_range_values_never_become_a_script() {
        for bad in [
            Command::BatteryLevel(101),
            Command::AnimationScale(11.0),
            Command::AnimationScale(f32::NAN),
            Command::ScreenTimeoutSecs(1),
            Command::Size(Some((10, 800))),
            Command::Size(Some((1080, 99_999))),
            Command::Density(Some(5)),
            Command::Density(Some(100_000)),
        ] {
            assert!(bad.script().is_err(), "{bad:?}");
        }
        assert_eq!(Command::ScreenTimeoutSecs(30).script().unwrap(), "settings put system screen_off_timeout 30000");
    }

    #[test]
    fn only_interrupting_commands_ask_first() {
        assert!(Command::Reboot(RebootTarget::Recovery).needs_confirmation());
        assert!(!Command::Wifi(false).needs_confirmation());
        assert!(Command::Reboot(RebootTarget::System).script().is_err(), "reboot is not a shell script");
    }

    #[test]
    fn readings_parse_the_formats_android_prints() {
        assert_eq!(parse_flag("1\n"), Some(true));
        assert_eq!(parse_flag("0"), Some(false));
        assert_eq!(parse_flag("null"), None);
        assert_eq!(DarkMode::parse("Night mode: yes"), Some(DarkMode::Dark));
        assert_eq!(DarkMode::parse("Night mode: auto"), Some(DarkMode::Auto));
        assert_eq!(DarkMode::parse("Night mode: no"), Some(DarkMode::Light));
        assert_eq!(DarkMode::parse("Unknown command"), None);
        assert_eq!(Rotation::from_settings("1", "3"), Rotation::Auto);
        assert_eq!(Rotation::from_settings("0", "3"), Rotation::Deg270);
        assert_eq!(Rotation::from_settings("0", "null"), Rotation::Deg0);
        let b =
            parse_battery("Current Battery Service state:\n  AC powered: false\n  USB powered: true\n  level: 87\n  temperature: 251\n");
        assert_eq!((b.level, b.charging, b.temperature_tenths), (Some(87), true, Some(251)));
        let b = parse_battery("  AC powered: false\n  USB powered: false\n  Wireless powered: false\n  level: 5\n");
        assert!(!b.charging);
    }
}

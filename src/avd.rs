//! Android Virtual Devices through the SDK `emulator` binary.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::adb::Adb;
use crate::error::{Error, Result};
use crate::process::{run_checked, CommandSpec};

/// AVD names are file-name-like; anything else is refused so it can never
/// be taken for an emulator option.
pub fn validate_avd_name(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with(['-', '.'])
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ' '));
    if ok {
        Ok(name)
    } else {
        Err(Error::Invalid(format!("'{name}' is not a valid virtual device name")))
    }
}

pub fn parse_avd_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        // The emulator prints notices such as "INFO | ..." on the same stream.
        .filter(|l| !l.is_empty() && validate_avd_name(l).is_ok())
        .map(str::to_string)
        .collect()
}

pub async fn list_avds(emulator: &Path) -> Result<Vec<String>> {
    let spec = CommandSpec::new(emulator).arg("-list-avds").timeout(Duration::from_secs(20));
    Ok(parse_avd_list(&run_checked(&spec).await?.stdout_text()))
}

pub fn start_args(name: &str, cold_boot: bool) -> Result<Vec<String>> {
    let name = validate_avd_name(name)?;
    let mut args = vec!["-avd".to_string(), name.to_string()];
    if cold_boot {
        args.push("-no-snapshot-load".to_string());
    }
    Ok(args)
}

/// Starts an emulator. It is deliberately not tied to this app: closing
/// ADB Deck must not shut down a virtual device the user may be using.
pub fn start_avd(emulator: &Path, name: &str, cold_boot: bool) -> Result<()> {
    let args = start_args(name, cold_boot)?;
    let mut cmd = std::process::Command::new(emulator);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|source| Error::Spawn { program: "emulator".into(), source })?;
    // Reap it in the background so it never lingers as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Shuts a running emulator down with `adb emu kill`.
pub async fn stop_emulator(adb: &Adb, serial: &str) -> Result<()> {
    if !serial.starts_with("emulator-") {
        return Err(Error::Invalid(format!("{serial} is not an emulator")));
    }
    let out = adb.device(serial).exec(["emu", "kill"]).await?;
    if out.success() {
        Ok(())
    } else {
        Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avd_list_ignores_emulator_notices() {
        let text = "INFO    | Storing crashdata in: /tmp/x, detection is enabled\nPixel_7_API_34\nTV_API_33\n\n";
        assert_eq!(parse_avd_list(text), ["Pixel_7_API_34", "TV_API_33"]);
    }

    #[test]
    fn names_that_look_like_options_are_rejected() {
        for bad in ["", "-wipe-data", "a;b", "a/b", ".hidden"] {
            assert!(validate_avd_name(bad).is_err(), "{bad}");
            assert!(start_args(bad, false).is_err());
        }
        assert_eq!(start_args("Pixel_7", true).unwrap(), ["-avd", "Pixel_7", "-no-snapshot-load"]);
    }

    #[tokio::test]
    async fn only_emulators_can_be_stopped() {
        let adb = Adb::new("/nonexistent/adb");
        assert!(matches!(stop_emulator(&adb, "28131FDH").await, Err(Error::Invalid(_))));
    }
}

//! Adapter around the `adb` executable. All device interaction goes through
//! [`Adb`] / [`DeviceAdb`]; submodules hold feature-specific commands and the
//! pure parsers for their output.

pub mod apps;
pub mod connect;
pub mod control;
pub mod devices;
pub mod discover;
pub mod files;
pub mod known;
pub mod layout;
pub mod logcat;
pub mod manage;
pub mod overview;
pub mod perf;
pub mod ports;
pub mod procs;
pub mod props;
pub mod remote_keys;
pub mod tcpdump;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::process::{self, CommandSpec, Output};
use crate::retry::{self, Backoff};

/// Marker printed between the results of a batched shell script.
const SEPARATOR: &str = "@@adbdeck-separator@@";
const SHELL_TIMEOUT: Duration = Duration::from_secs(20);

/// Quotes a value for the device's POSIX shell. `adb shell` joins its
/// arguments into one command line, so every dynamic value must pass here.
pub fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Handle to the adb executable (not bound to a device).
#[derive(Debug, Clone)]
pub struct Adb {
    bin: Arc<PathBuf>,
}

impl Adb {
    pub fn new(bin: impl Into<PathBuf>) -> Self {
        Self { bin: Arc::new(bin.into()) }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.bin
    }

    pub fn spec(&self) -> CommandSpec {
        CommandSpec::new(self.bin.as_ref().clone())
    }

    /// Runs an adb command that is not tied to a device.
    pub async fn global<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let spec = self.spec().args(args);
        process::run(&spec).await
    }

    pub fn device(&self, serial: &str) -> DeviceAdb {
        DeviceAdb { adb: self.clone(), serial: serial.to_string() }
    }

    /// Restarts the adb server (kill, then start).
    pub async fn restart_server(&self) -> Result<()> {
        self.global(["kill-server"]).await?;
        let out = self.global(["start-server"]).await?;
        if out.success() {
            Ok(())
        } else {
            Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() })
        }
    }

    /// Starts the adb server if it is not running; harmless when it already is.
    pub async fn start_server(&self) -> Result<()> {
        let out = self.global(["start-server"]).await?;
        if out.success() {
            Ok(())
        } else {
            Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() })
        }
    }

    pub async fn kill_server(&self) {
        // Best effort at shutdown; nothing useful can be done on failure.
        let _ = self.global(["kill-server"]).await;
    }
}

/// An adb handle bound to one device serial.
#[derive(Debug, Clone)]
pub struct DeviceAdb {
    adb: Adb,
    serial: String,
}

/// Turns adb's own failures (as opposed to failures of the remote command)
/// into typed errors.
pub fn classify_failure(serial: &str, out: &Output) -> Option<Error> {
    if out.success() {
        return None;
    }
    let gone = out.stderr.lines().any(|line| {
        let line = line.trim().to_ascii_lowercase();
        (line.starts_with("adb:") || line.starts_with("error:"))
            && (line.contains("not found")
                || line.contains("no devices/emulators found")
                || line.contains("device offline")
                || line.contains("error: closed")
                || line.ends_with("closed")
                || line.contains("more than one device")
                || line.contains("failed to get feature set"))
    });
    gone.then(|| Error::DeviceGone(serial.to_string()))
}

impl DeviceAdb {
    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn adb(&self) -> &Adb {
        &self.adb
    }

    pub fn spec(&self) -> CommandSpec {
        self.adb.spec().arg("-s").arg(self.serial.as_str())
    }

    /// Runs `adb -s <serial> <args...>`; the exit status is returned as data
    /// unless adb itself reports the device missing.
    pub async fn exec<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.exec_spec(self.spec().args(args)).await
    }

    pub async fn exec_spec(&self, spec: CommandSpec) -> Result<Output> {
        let backoff = Backoff::QUICK;
        let mut attempt = 1;
        loop {
            let out = process::run(&spec).await?;
            // adb's own hiccups (server restarting, link re-enumerating) happen before
            // the command reaches the phone, so trying again cannot repeat its effect.
            if !out.success() && attempt < backoff.attempts && retry::is_adb_hiccup(&out.stderr) {
                tracing::debug!(serial = %self.serial, attempt, "adb hiccup, retrying: {}", out.stderr.trim());
                if retry::is_server_down(&out.stderr) {
                    let _ = self.adb.start_server().await;
                }
                tokio::time::sleep(backoff.delay(attempt)).await;
                attempt += 1;
                continue;
            }
            if let Some(err) = classify_failure(&self.serial, &out) {
                return Err(err);
            }
            // Out of tries: this is adb's failure, not the command's, so it must not look like empty output.
            if !out.success() && retry::is_adb_hiccup(&out.stderr) {
                return Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() });
            }
            return Ok(out);
        }
    }

    /// Runs a trusted script on the device and returns trimmed stdout. The
    /// remote exit status is deliberately ignored: many probes (grep, ls)
    /// exit non-zero for "nothing found".
    pub async fn shell(&self, script: &str) -> Result<String> {
        let out = self.shell_output(script, SHELL_TIMEOUT).await?;
        Ok(normalize(&out.stdout_text()))
    }

    pub async fn shell_output(&self, script: &str, timeout: Duration) -> Result<Output> {
        self.exec_spec(self.spec().arg("shell").arg(script).timeout(timeout)).await
    }

    /// For commands with very large output (full package dumps).
    pub async fn shell_large(&self, script: &str, timeout: Duration, max_stdout: usize) -> Result<Output> {
        self.exec_spec(self.spec().arg("shell").arg(script).timeout(timeout).max_stdout(max_stdout)).await
    }

    /// Like [`shell`](Self::shell) but a non-zero remote status is an error
    /// carrying the device's message.
    pub async fn shell_ok(&self, script: &str) -> Result<String> {
        let out = self.shell_output(script, SHELL_TIMEOUT).await?;
        if !out.success() {
            return Err(Error::Exit { program: "adb shell".into(), code: out.code, message: normalize(&out.failure_message()) });
        }
        Ok(normalize(&out.stdout_text()))
    }

    /// Runs several trusted scripts in one adb invocation and splits the
    /// output per script.
    pub async fn shell_batch(&self, scripts: &[&str]) -> Result<Vec<String>> {
        let joined = scripts.join(&format!("\necho '{SEPARATOR}'\n"));
        let out = self.shell(&joined).await?;
        Ok(split_batch(&out, scripts.len()))
    }

    /// `adb exec-out`: binary-safe stdout, used for screenshots and file
    /// contents.
    pub async fn exec_out(&self, command: &[&str], max_bytes: usize, timeout: Duration) -> Result<Output> {
        let spec = self.spec().arg("exec-out").args(command.iter().map(|s| s.to_string())).max_stdout(max_bytes).timeout(timeout);
        self.exec_spec(spec).await
    }
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").trim().to_string()
}

pub fn split_batch(output: &str, expected: usize) -> Vec<String> {
    let mut parts: Vec<String> = output.split(SEPARATOR).map(|p| p.trim().to_string()).collect();
    parts.resize(expected, String::new());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(code: i32, stderr: &str) -> Output {
        Output { code: Some(code), stderr: stderr.into(), ..Output::default() }
    }

    #[test]
    fn quoting_neutralises_shell_metacharacters() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote("$(reboot); `id`"), "'$(reboot); `id`'");
    }

    #[test]
    fn adb_level_failures_become_device_gone() {
        for msg in [
            "adb: device 'abc' not found\n",
            "adb: no devices/emulators found\n",
            "error: device offline\n",
            "error: closed\n",
            "adb: error: failed to get feature set: device 'x' not found\n",
        ] {
            let err = classify_failure("abc", &out(1, msg)).unwrap_or_else(|| panic!("{msg}"));
            assert!(err.is_device_gone(), "{msg}");
        }
    }

    #[test]
    fn remote_command_failures_are_not_device_gone() {
        assert!(classify_failure("abc", &out(1, "ls: /nope: No such file or directory\n")).is_none());
        assert!(classify_failure("abc", &out(0, "adb: device 'x' not found")).is_none());
        // Remote text that merely contains "not found" must not be mistaken.
        assert!(classify_failure("abc", &out(127, "sh: tcpdump: not found\n")).is_none());
    }

    #[test]
    fn batch_output_is_split_and_padded() {
        let text = format!("one\n{SEPARATOR}\ntwo\n");
        assert_eq!(split_batch(&text, 3), vec!["one", "two", ""]);
    }
}

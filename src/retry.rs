//! Retrying operations that fail for transient reasons.
//!
//! adb is a client of a background server and a USB or Wi-Fi link, so single
//! commands fail now and then for reasons that vanish a moment later (the
//! server is restarting, the device is re-enumerating, `adb` lost a race).
//! [`retry`] runs an operation again with exponentially growing, capped pauses,
//! and only for errors the caller considers transient.

use std::future::Future;
use std::time::Duration;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    /// Total tries, including the first.
    pub attempts: u32,
    /// Pause before the second try; doubles each time.
    pub first: Duration,
    /// No pause is longer than this.
    pub cap: Duration,
}

impl Backoff {
    /// Short bursts for commands the user is waiting on.
    pub const QUICK: Backoff = Backoff { attempts: 3, first: Duration::from_millis(250), cap: Duration::from_secs(1) };
    /// For connections that may take a few seconds to come back.
    pub const PATIENT: Backoff = Backoff { attempts: 6, first: Duration::from_millis(500), cap: Duration::from_secs(8) };
    /// Win back a Wi-Fi device that dropped: about five minutes of trying, spaced out.
    pub const RECONNECT: Backoff = Backoff { attempts: 12, first: Duration::from_secs(2), cap: Duration::from_secs(30) };

    /// The pause that follows failed try number `attempt` (1-based).
    pub fn delay(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(16);
        self.first.saturating_mul(1u32 << shift).min(self.cap)
    }
}

/// Failures worth another try: a process that timed out or could not be
/// reached, or an adb server that is not up yet. A vanished device, bad input
/// and a command that ran and failed are final.
pub fn is_transient(e: &Error) -> bool {
    match e {
        Error::Timeout { .. } | Error::Spawn { .. } => true,
        Error::Exit { message, .. } => {
            let m = message.to_ascii_lowercase();
            [
                "server didn't ack",
                "cannot connect to daemon",
                "connection reset",
                "device offline",
                "protocol fault",
                "closed",
                "daemon not running",
                "try again",
            ]
            .iter()
            .any(|needle| m.contains(needle))
        }
        Error::Io { source, .. } => matches!(
            source.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::Interrupted
        ),
        _ => false,
    }
}

/// adb's own transient complaints, as opposed to a command that ran and
/// failed. They are raised before the command reaches the phone, so repeating
/// the command is safe even when it changes something.
pub fn is_adb_hiccup(stderr: &str) -> bool {
    let m = stderr.to_ascii_lowercase();
    [
        "server didn't ack",
        "cannot connect to daemon",
        "daemon not running",
        "failed to check server version",
        "protocol fault",
        "couldn't read status",
        "device still authorizing",
        "device offline",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

/// True when the adb server itself is not running (as opposed to a busy device).
pub fn is_server_down(stderr: &str) -> bool {
    let m = stderr.to_ascii_lowercase();
    ["cannot connect to daemon", "daemon not running", "server didn't ack", "failed to check server version"].iter().any(|n| m.contains(n))
}

/// Runs `op` until it succeeds, fails with a non-transient error, or the tries
/// run out; the last error is returned. `on_retry` is told which try failed and
/// why, before the pause (for status lines and logs).
pub async fn retry<T, F, Fut>(backoff: Backoff, mut on_retry: impl FnMut(u32, &Error), mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 1;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < backoff.attempts && is_transient(&e) => {
                on_retry(attempt, &e);
                tokio::time::sleep(backoff.delay(attempt)).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn timeout() -> Error {
        Error::Timeout { program: "adb".into(), secs: 1 }
    }

    #[test]
    fn delays_double_and_are_capped() {
        let b = Backoff { attempts: 9, first: Duration::from_millis(100), cap: Duration::from_millis(700) };
        let delays: Vec<u64> = (1..=5).map(|a| b.delay(a).as_millis() as u64).collect();
        assert_eq!(delays, [100, 200, 400, 700, 700]);
        assert_eq!(b.delay(1000), Duration::from_millis(700), "huge attempt numbers do not overflow");
    }

    #[test]
    fn only_transient_failures_are_retried() {
        assert!(is_transient(&timeout()));
        assert!(is_transient(&Error::Exit {
            program: "adb".into(),
            code: Some(1),
            message: "error: protocol fault (couldn't read status)".into()
        }));
        assert!(is_transient(&Error::Exit {
            program: "adb".into(),
            code: Some(1),
            message: "cannot connect to daemon at tcp:5037".into()
        }));
        assert!(!is_transient(&Error::DeviceGone("x".into())));
        assert!(!is_transient(&Error::Invalid("bad".into())));
        assert!(!is_transient(&Error::Exit { program: "adb".into(), code: Some(1), message: "Failure [INSTALL_FAILED_OLDER_SDK]".into() }));
    }

    #[test]
    fn adb_hiccups_are_told_from_failed_commands() {
        assert!(is_adb_hiccup("error: protocol fault (couldn't read status): Connection reset by peer"));
        assert!(is_adb_hiccup("adb: device offline"));
        assert!(is_adb_hiccup("* daemon not running; starting now at tcp:5037\nadb server didn't ACK"));
        assert!(!is_adb_hiccup("Failure [INSTALL_FAILED_OLDER_SDK]"));
        assert!(!is_adb_hiccup("adb: device 'X' not found"));
        assert!(!is_adb_hiccup(""));
        assert!(is_server_down("cannot connect to daemon at tcp:5037: Connection refused"));
        assert!(!is_server_down("adb: device offline"));
    }

    #[tokio::test(start_paused = true)]
    async fn retries_until_success_and_reports_each_failure() {
        let tries = AtomicU32::new(0);
        let mut reported = Vec::new();
        let out = retry(
            Backoff::QUICK,
            |n, _| reported.push(n),
            || async {
                if tries.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(timeout())
                } else {
                    Ok(7)
                }
            },
        )
        .await
        .unwrap();
        assert_eq!((out, tries.load(Ordering::SeqCst), reported), (7, 3, vec![1, 2]));
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_the_last_attempt_with_the_last_error() {
        let tries = AtomicU32::new(0);
        let err = retry(
            Backoff::QUICK,
            |_, _| {},
            || async {
                tries.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(timeout())
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Timeout { .. }));
        assert_eq!(tries.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_final_error_is_not_retried() {
        let tries = AtomicU32::new(0);
        let err = retry(
            Backoff::PATIENT,
            |_, _| {},
            || async {
                tries.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(Error::DeviceGone("abc".into()))
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::DeviceGone(_)));
        assert_eq!(tries.load(Ordering::SeqCst), 1);
    }
}

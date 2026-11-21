//! The windowless background service, started with `adbdeck --daemon` (by the
//! autostart entry at login, or by hand). It runs the same device watcher as the
//! window: remembered phones are connected, dropped ones are won back, and phones
//! on the network or at their old addresses (Wi-Fi, USB tethering, hotspot) are
//! found, with no window open. The adb server it starts keeps the connections, so
//! the window finds them in place when it is opened later.
//!
//! The service only reads the saved data. The window is the one writer, so the two
//! processes never overwrite each other; the service re-reads the data on every
//! network scan, which is how a phone the user disconnects in the window stays
//! disconnected.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::adb::devices::{watch_devices, Registry, Reload, WatchEvent, WatchOptions};
use crate::adb::Adb;
use crate::error::{Error, Result};
use crate::store::{Settings, Store};
use crate::tools::{detect_all, LookupEnv, Tool};

/// How often the service checks that it is still wanted.
const STILL_WANTED_EVERY: Duration = Duration::from_secs(30);

/// The right to be the one background service of this user; released when dropped.
#[derive(Debug)]
pub struct Instance {
    path: PathBuf,
}

impl Instance {
    /// `Ok(None)`: another service is running. A pid file left by a process that
    /// no longer exists (crash, power loss) is replaced.
    pub fn acquire(path: &Path) -> Result<Option<Instance>> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| Error::io(format!("creating {}", dir.display()), e))?;
        }
        for _ in 0..2 {
            match fs::OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id()).map_err(|e| Error::io(format!("writing {}", path.display()), e))?;
                    return Ok(Some(Instance { path: path.to_path_buf() }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if holder_alive(path) {
                        return Ok(None);
                    }
                    let _ = fs::remove_file(path);
                }
                Err(e) => return Err(Error::io(format!("creating {}", path.display()), e)),
            }
        }
        Ok(None)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The recorded process still exists and has this program's name, which guards
/// against a recycled process id.
fn holder_alive(path: &Path) -> bool {
    let Some(pid) = fs::read_to_string(path).ok().and_then(|t| t.trim().parse::<u32>().ok()) else { return false };
    let name = |who: &str| fs::read_to_string(format!("/proc/{who}/comm")).ok().map(|n| n.trim().to_string());
    match (name(&pid.to_string()), name("self")) {
        (Some(holder), Some(me)) => holder == me,
        _ => false,
    }
}

/// False once the user switched off automatic reconnection or the background service.
/// An unreadable settings file does not stop a running service.
fn wanted(settings: Option<Settings>) -> bool {
    settings.is_none_or(|s| s.auto_reconnect && s.background_service)
}

/// Runs until it is switched off in Settings, its event channel closes or the process is stopped.
pub async fn run(store: Store) -> Result<()> {
    let settings = store.peek_settings().unwrap_or_default();
    if !wanted(Some(settings.clone())) {
        // Not a failure: a service manager must not restart what the user switched off.
        tracing::info!("automatic reconnection or the background service is switched off in Settings");
        return Ok(());
    }
    let adb_path = detect_all(&settings, &LookupEnv::from_process())
        .into_iter()
        .find(|s| s.tool == Tool::Adb)
        .and_then(|s| s.path)
        .ok_or_else(|| Tool::Adb.missing_error())?;
    let adb = Adb::new(adb_path);
    // The adb server keeps the connections, so it has to be up before anything else.
    adb.start_server().await?;

    let data = store.peek_data().unwrap_or_default();
    let registry = Registry::from_data(&data);
    let remembered: Vec<String> = data.remote_devices.iter().filter(|r| r.auto_connect).map(|r| r.id.clone()).collect();
    let reload_store = store.clone();
    let reload: Reload = Arc::new(move || Registry::from_data(&reload_store.peek_data().unwrap_or_default()));
    let options = WatchOptions { remembered, known: registry.known, reload: Some(reload), ..WatchOptions::default() };
    let (tx, mut rx) = mpsc::channel(8);
    let watcher = tokio::spawn(watch_devices(adb, tx, options));

    let mut check = tokio::time::interval(STILL_WANTED_EVERY);
    let mut online = usize::MAX;
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Some(WatchEvent::Devices(list)) => {
                    let now = list.iter().filter(|d| d.is_online()).count();
                    if now != online {
                        online = now;
                        tracing::info!(online = now, "devices changed");
                    }
                }
                Some(WatchEvent::Failed(e)) => tracing::warn!(error = %e, "adb is not answering"),
                Some(WatchEvent::Reconnecting { id, attempt, of }) => tracing::info!(%id, attempt, of, "reconnecting"),
                Some(WatchEvent::Restored(id)) => tracing::info!(%id, "reconnected"),
                Some(WatchEvent::Found(id)) => tracing::info!(%id, "connected to a remembered phone"),
                Some(WatchEvent::GaveUp(id)) => tracing::warn!(%id, "stopped retrying the old address; still looking on the network"),
                None => break,
            },
            _ = check.tick() => {
                if !wanted(store.peek_settings()) {
                    tracing::info!("switched off in Settings; stopping");
                    break;
                }
            }
        }
    }
    watcher.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_service_is_refused_until_the_first_one_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run/adbdeck-daemon.pid");
        let first = Instance::acquire(&path).unwrap();
        assert!(first.is_some());
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), std::process::id().to_string());
        // The recorded pid is this very process, which is alive and has our name.
        assert!(Instance::acquire(&path).unwrap().is_none(), "one service per user");
        drop(first);
        assert!(!path.exists(), "the pid file goes away with the service");
        assert!(Instance::acquire(&path).unwrap().is_some());
    }

    #[test]
    fn a_pid_file_of_a_dead_process_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adbdeck-daemon.pid");
        fs::write(&path, "4294967\n").unwrap();
        assert!(!holder_alive(&path));
        let taken = Instance::acquire(&path).unwrap();
        assert!(taken.is_some(), "a stale file must not block the service for good");
        assert_eq!(fs::read_to_string(&path).unwrap().trim(), std::process::id().to_string());
    }

    #[test]
    fn garbage_in_the_pid_file_counts_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adbdeck-daemon.pid");
        fs::write(&path, "not a pid").unwrap();
        assert!(!holder_alive(&path));
        assert!(!holder_alive(&dir.path().join("missing.pid")));
    }

    #[test]
    fn the_service_stops_when_switched_off_and_keeps_running_when_unsure() {
        assert!(wanted(None), "an unreadable settings file does not stop it");
        assert!(wanted(Some(Settings::default())));
        assert!(!wanted(Some(Settings { auto_reconnect: false, ..Settings::default() })));
        assert!(!wanted(Some(Settings { background_service: false, ..Settings::default() })));
    }
}

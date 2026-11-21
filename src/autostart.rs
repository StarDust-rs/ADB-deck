//! Starting the background service when the user logs in (XDG autostart, which
//! KDE Plasma, GNOME and most other desktops honour). The entry runs
//! `adbdeck --daemon`: no window, only the device watcher (see `daemon`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::paths;
use crate::store::write_atomic;

const FILE_NAME: &str = "adbdeck-daemon.desktop";

/// The `Exec` value. The program path is always quoted; characters that the
/// desktop entry syntax treats specially inside quotes are refused rather than escaped.
fn exec_line(exe: &Path) -> Result<String> {
    let text = exe.to_str().ok_or_else(|| Error::Invalid("the program path is not valid UTF-8".into()))?;
    if text.chars().any(|c| c.is_control() || matches!(c, '"' | '`' | '$' | '\\' | '%')) {
        return Err(Error::Invalid(format!("{text} cannot be used in an autostart entry")));
    }
    Ok(format!("\"{text}\" --daemon"))
}

/// The desktop entry that starts the background service from `exe`.
pub fn entry(exe: &Path) -> Result<String> {
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=ADB Deck background service\n\
         Comment=Reconnects remembered Android devices without opening ADB Deck\n\
         Exec={}\n\
         Icon=adbdeck\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_line(exe)?
    ))
}

pub fn entry_path() -> Option<PathBuf> {
    paths::autostart_dir().map(|d| d.join(FILE_NAME))
}

/// Makes the autostart entry match `enabled`. Idempotent: nothing is written when the
/// entry is already right, and the program path is refreshed after a move or upgrade.
pub fn sync(enabled: bool, exe: &Path) -> Result<()> {
    let path = entry_path().ok_or_else(|| Error::Invalid("cannot determine the autostart directory".into()))?;
    sync_at(&path, enabled, exe)
}

fn sync_at(path: &Path, enabled: bool, exe: &Path) -> Result<()> {
    if !enabled {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(format!("removing {}", path.display()), e)),
        };
    }
    let wanted = entry(exe)?;
    if fs::read_to_string(path).is_ok_and(|have| have == wanted) {
        return Ok(());
    }
    write_atomic(path, wanted.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_starts_the_daemon_from_the_given_program() {
        let text = entry(Path::new("/usr/bin/adbdeck")).unwrap();
        assert!(text.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(text.contains("\nExec=\"/usr/bin/adbdeck\" --daemon\n"));
        assert!(text.contains("\nNoDisplay=true\n"), "it must not show up in the application menu");
    }

    #[test]
    fn paths_with_spaces_are_quoted_and_unsafe_ones_refused() {
        assert!(entry(Path::new("/opt/my apps/adbdeck")).unwrap().contains("Exec=\"/opt/my apps/adbdeck\" --daemon"));
        for bad in ["/opt/a\"b/adbdeck", "/opt/$HOME/adbdeck", "/opt/100%/adbdeck", "/opt/a`b/adbdeck", "/opt/a\\b/adbdeck"] {
            assert!(entry(Path::new(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn syncing_creates_refreshes_and_removes_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autostart/adbdeck-daemon.desktop");
        sync_at(&path, false, Path::new("/usr/bin/adbdeck")).unwrap();
        assert!(!path.exists(), "removing what is not there is fine");

        sync_at(&path, true, Path::new("/usr/bin/adbdeck")).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("/usr/bin/adbdeck"));

        sync_at(&path, true, Path::new("/opt/adbdeck/adbdeck")).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("/opt/adbdeck/adbdeck") && !text.contains("/usr/bin"), "a moved program is picked up");

        sync_at(&path, false, Path::new("/opt/adbdeck/adbdeck")).unwrap();
        assert!(!path.exists());
    }
}

//! XDG directory resolution. All locations can be overridden through the
//! standard `XDG_*` variables, which the tests and the demo mode rely on.

use std::path::PathBuf;

use directories::{BaseDirs, ProjectDirs};

pub const APP_DIR: &str = "adbdeck";

fn dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", APP_DIR)
}

pub fn config_dir() -> Option<PathBuf> {
    dirs().map(|d| d.config_dir().to_path_buf())
}

pub fn data_dir() -> Option<PathBuf> {
    dirs().map(|d| d.data_dir().to_path_buf())
}

pub fn cache_dir() -> Option<PathBuf> {
    dirs().map(|d| d.cache_dir().to_path_buf())
}

/// Per-user temporary area for files pulled from a device for preview/open.
pub fn preview_cache_dir() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("preview"))
}

/// Where decoded app icons are kept between runs.
pub fn icon_cache_dir() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("icons"))
}

/// `$XDG_CONFIG_HOME/autostart`: desktop entries started when the user logs in.
pub fn autostart_dir() -> Option<PathBuf> {
    BaseDirs::new().map(|d| d.config_dir().join("autostart"))
}

/// Where the background service records its process id (one instance per user).
/// Prefers `$XDG_RUNTIME_DIR`, which is private to the user and cleared at logout.
pub fn daemon_pid_file() -> PathBuf {
    let dir = BaseDirs::new()
        .and_then(|d| d.runtime_dir().map(PathBuf::from))
        .unwrap_or_else(|| cache_dir().unwrap_or_else(std::env::temp_dir));
    dir.join("adbdeck-daemon.pid")
}

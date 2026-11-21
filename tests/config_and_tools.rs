//! Configuration failures and external-tool supervision through the public API.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use adbdeck::error::Error;
use adbdeck::store::{Settings, Store, ThemeChoice};

#[test]
fn damaged_config_files_fall_back_to_defaults_and_are_kept_aside() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(&dir.path().join("cfg"), &dir.path().join("data"));
    fs::create_dir_all(dir.path().join("cfg")).unwrap();
    fs::create_dir_all(dir.path().join("data")).unwrap();
    fs::write(dir.path().join("cfg/settings.toml"), "theme = [[[ not toml").unwrap();
    fs::write(dir.path().join("data/data.json"), "{ truncated").unwrap();

    let (settings, warn) = store.load_settings();
    assert_eq!(settings, Settings::default());
    assert!(warn.is_some_and(|w| w.contains("settings")), "the user is told");
    assert!(dir.path().join("cfg/settings.bad").exists(), "the damaged file is preserved");

    let (data, warn) = store.load_data();
    assert_eq!(data, Default::default());
    assert!(warn.is_some());

    // The application keeps working: new settings can be saved again.
    store.save_settings(&Settings { theme: ThemeChoice::Dark, ..Default::default() }).unwrap();
    assert_eq!(store.load_settings().0.theme, ThemeChoice::Dark);
}

#[test]
fn an_unwritable_config_location_is_an_error_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
    let store = Store::at(&locked.join("cfg"), &locked.join("data"));
    let running_as_root = fs::write(locked.join("probe"), b"x").is_ok();
    let result = store.save_settings(&Settings::default());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    if !running_as_root {
        assert!(matches!(result, Err(Error::Io { .. })));
    }
}

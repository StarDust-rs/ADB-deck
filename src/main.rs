//! ADB Deck entry point.

mod ui;

use std::path::PathBuf;
use std::sync::Arc;

use adbdeck::store::Store;
use adbdeck::{daemon, paths};

const HELP: &str = "ADB Deck - a desktop companion for Android Debug Bridge\n\n\
Usage: adbdeck [OPTIONS] [APK...]\n\n\
Options:\n  -d, --daemon     Run without a window and keep remembered devices connected\n  -h, --help       Show this help\n  -V, --version    Show the version\n\n\
APK files given on the command line are queued for installation on the selected device.";

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "adbdeck=info".into()))
        .with_writer(std::io::stderr)
        .init();

    let mut apks: Vec<PathBuf> = Vec::new();
    let mut background = false;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("-d" | "--daemon") => background = true,
            Some("-h" | "--help") => {
                println!("{HELP}");
                return std::process::ExitCode::SUCCESS;
            }
            Some("-V" | "--version") => {
                println!("adbdeck {}", env!("CARGO_PKG_VERSION"));
                return std::process::ExitCode::SUCCESS;
            }
            Some(s) if s.starts_with('-') => {
                eprintln!("adbdeck: unknown option '{s}'\n\n{HELP}");
                return std::process::ExitCode::from(2);
            }
            _ => {
                let p = PathBuf::from(&arg);
                if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("apk")) {
                    apks.push(std::fs::canonicalize(&p).unwrap_or(p));
                } else {
                    eprintln!("adbdeck: ignoring '{}' (not an .apk file)", p.display());
                }
            }
        }
    }

    let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(3).enable_all().thread_name("adbdeck-worker").build() {
        Ok(rt) => Arc::new(rt),
        Err(e) => {
            eprintln!("adbdeck: cannot start the background runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if background {
        return run_daemon(&rt);
    }

    let mut startup_notes = Vec::new();
    let (store, settings, data) = match Store::default_location() {
        Ok(store) => {
            let (settings, w1) = store.load_settings();
            let (data, w2) = store.load_data();
            startup_notes.extend(w1);
            startup_notes.extend(w2);
            (store, settings, data)
        }
        Err(e) => {
            // Run with defaults; saving will report the problem.
            startup_notes.push(e.to_string());
            (
                Store::at(&std::env::temp_dir().join("adbdeck-unsaved"), &std::env::temp_dir().join("adbdeck-unsaved")),
                Default::default(),
                Default::default(),
            )
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([820.0, 520.0])
            .with_app_id("adbdeck")
            .with_title("ADB Deck"),
        ..Default::default()
    };
    let rt_for_app = rt.clone();
    let result = eframe::run_native(
        "ADB Deck",
        options,
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, rt_for_app, store, settings, data, startup_notes, apks)))),
    );
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("adbdeck: the window could not be shown: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// `adbdeck --daemon`: the windowless background service (see `adbdeck::daemon`).
fn run_daemon(rt: &tokio::runtime::Runtime) -> std::process::ExitCode {
    let store = match Store::default_location() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("adbdeck: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    // Held until this function returns; a second service would only repeat the first one's work.
    let _instance = match daemon::Instance::acquire(&paths::daemon_pid_file()) {
        Ok(Some(instance)) => instance,
        Ok(None) => {
            tracing::info!("the background service is already running");
            return std::process::ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("adbdeck: cannot start the background service: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match rt.block_on(daemon::run(store)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("adbdeck: the background service stopped: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

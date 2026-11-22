//! Integration tests: the real adapters against `tests/fixtures/fake-adb`,
//! a script that simulates one Android device on the host.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use adbdeck::adb::devices::{watch_devices, DeviceState, Registry, Reload, Suppressed, WatchEvent, WatchOptions};
use adbdeck::adb::files::{Direction, TransferEvent};
use adbdeck::adb::Adb;
use adbdeck::error::Error;
use adbdeck::retry::Backoff;
use adbdeck::tools::{detect_all, locate, LookupEnv, Tool};
use tempfile::TempDir;
use tokio::sync::mpsc;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

struct Fake {
    dir: TempDir,
    adb: Adb,
}

impl Fake {
    /// `exports` are extra shell lines such as `export FAKE_ADB_MODE=exit1`.
    fn new(exports: &str) -> Fake {
        let dir = tempfile::tempdir().expect("tempdir");
        let wrapper = dir.path().join("adb");
        let script = format!(
            "#!/bin/sh\nexport FAKE_ADB_HOME='{}'\n{exports}\nexec '{}' \"$@\"\n",
            dir.path().join("home").display(),
            fixture_dir().join("fake-adb").display()
        );
        fs::write(&wrapper, script).expect("write wrapper");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("chmod");
        Fake { adb: Adb::new(&wrapper), dir }
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("home/state")
    }

    fn sdcard(&self) -> PathBuf {
        self.dir.path().join("home/root/sdcard")
    }
}

const SERIAL: &str = "FAKE0001";

#[tokio::test]
async fn lists_and_describes_the_device() {
    let f = Fake::new("");
    let devices = f.adb.snapshot().await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, SERIAL);
    assert_eq!(devices[0].state, DeviceState::Online);
    assert_eq!(devices[0].name, "Google Pixel 7");
    assert_eq!(devices[0].android_version, "14");
    assert_eq!(devices[0].sdk_version, "34");
}

#[tokio::test]
async fn reads_overview_apps_processes_and_performance() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);

    let o = dev.overview().await.unwrap();
    assert_eq!(o.model, "Pixel 7");
    assert_eq!(o.wifi, "HomeNet");
    assert_eq!(o.ip, "192.168.1.42");
    assert_eq!(o.resolution, "360x720");
    assert!(!o.root);
    assert_eq!(o.font_scale, 1.0, "an unset font scale is the default");
    assert_eq!((o.security_patch.as_str(), o.build_id.as_str(), o.selinux.as_str()), ("2024-10-05", "AP2A.240905.003", "Enforcing"));
    assert_eq!((o.bootloader_unlocked, o.encrypted), (Some(false), Some(true)));
    assert_eq!((o.user_apps, o.total_apps), (Some(2), Some(3)));
    assert_eq!(o.battery.level, Some(87));
    assert!(o.uptime_secs.is_some() && o.mem_available > 0);

    let apps = dev.list_packages().await.unwrap();
    assert_eq!(apps.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["com.android.settings", "com.example.app", "com.example.game"]);
    let app = apps.iter().find(|p| p.name == "com.example.app").unwrap();
    assert_eq!(app.version_name, "1.2.3");
    assert_eq!(app.app_size, 52_428_800);
    assert!(app.apk_path.ends_with("base.apk"));
    assert!(apps.iter().find(|p| p.name == "com.android.settings").unwrap().system);

    let perms = dev.package_permissions("com.example.app").await.unwrap();
    assert!(perms.iter().any(|p| p.name.ends_with("CAMERA") && p.granted == Some(false)));

    let procs = dev.processes().await.unwrap();
    assert!(procs.iter().any(|p| p.name == "com.example.app" && p.package.as_deref() == Some("com.example.app")));

    let sample = dev.perf_sample().await.unwrap();
    assert_eq!(sample.battery.level, 87);
    assert_eq!(sample.top.package, "com.example.app");
    assert!(!sample.cpus.is_empty());
}

#[tokio::test]
async fn app_actions_reach_the_device() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    dev.force_stop("com.example.app").await.unwrap();
    assert_eq!(fs::read_to_string(f.state().join("stopped.log")).unwrap().trim(), "com.example.app");
    dev.launch("com.example.app").await.unwrap();
    dev.clear_data("com.example.app").await.unwrap();
    dev.uninstall("com.example.app").await.unwrap();
    assert!(dev.force_stop("bad name; reboot").await.is_err(), "package names are validated");
    dev.press(adbdeck::adb::remote_keys::Key::HOME).await.unwrap();
    assert_eq!(fs::read_to_string(f.state().join("input.log")).unwrap().trim(), "keyevent 3");
}

#[tokio::test]
async fn install_reports_success_and_failure() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    let good = f.dir.path().join("good.apk");
    let bad = f.dir.path().join("bad.apk");
    fs::write(&good, b"apk").unwrap();
    fs::write(&bad, b"apk").unwrap();
    dev.install_apk(&good).await.unwrap();
    let err = dev.install_apk(&bad).await.unwrap_err().to_string();
    assert!(err.contains("signed with a different key"), "{err}");
    assert!(dev.install_apk(&f.dir.path().join("missing.apk")).await.is_err());
}

#[tokio::test]
async fn file_operations_round_trip() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    let list = dev.list_dir("/sdcard", false).await.unwrap();
    assert!(list.iter().any(|e| e.name == "DCIM" && e.is_dir));

    dev.make_dir("/sdcard/Made", false).await.unwrap();
    assert!(f.sdcard().join("Made").is_dir());
    dev.move_path("/sdcard/Made", "/sdcard/Moved", false).await.unwrap();
    assert!(f.sdcard().join("Moved").is_dir() && !f.sdcard().join("Made").exists());
    dev.delete_path("/sdcard/Moved", true, false).await.unwrap();
    assert!(!f.sdcard().join("Moved").exists());

    let docs = dev.list_dir("/sdcard/Documents", false).await.unwrap();
    assert_eq!(docs[0].name, "notes.txt");
    assert_eq!(docs[0].size, Some(18));

    // Transfers go through a pseudo-terminal and report progress.
    let local = f.dir.path().join("pulled.txt");
    let (tx, mut rx) = mpsc::unbounded_channel();
    dev.transfer(Direction::Download, "/sdcard/Documents/notes.txt", local.to_str().unwrap(), tx).await.unwrap();
    assert_eq!(fs::read_to_string(&local).unwrap(), "Fake device notes\n");
    let mut last = 0;
    while let Ok(TransferEvent::Progress { percent, .. }) = rx.try_recv() {
        last = percent;
    }
    assert_eq!(last, 100);

    let (tx, _rx) = mpsc::unbounded_channel();
    dev.transfer(Direction::Upload, local.to_str().unwrap(), "/sdcard/Download/up.txt", tx).await.unwrap();
    assert!(f.sdcard().join("Download/up.txt").is_file());

    let (tx, _rx) = mpsc::unbounded_channel();
    let err = dev.transfer(Direction::Download, "/sdcard/nope", local.to_str().unwrap(), tx).await.unwrap_err();
    assert!(err.to_string().contains("No such file"), "{err}");
}

#[tokio::test]
async fn a_failed_run_as_pull_keeps_the_existing_local_file() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    let local = f.dir.path().join("export.bin");
    fs::write(&local, "precious").unwrap();
    // The fake adb cannot run `run-as`, so the pull fails after the target was chosen.
    let err = dev.pull_runas_file("com.example.app", "/data/data/com.example.app/files/x", &local).await;
    assert!(err.is_err());
    assert_eq!(fs::read_to_string(&local).unwrap(), "precious");
    assert!(!f.dir.path().join("export.bin.part").exists(), "partial file must be removed");
}

#[tokio::test]
async fn screenshot_layout_and_capture() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    let png = dev.screenshot_png().await.unwrap();
    assert!(png.starts_with(b"\x89PNG"));

    let xml = dev.dump_hierarchy().await.unwrap();
    let tree = adbdeck::adb::layout::UiTree::parse(&xml).unwrap();
    let hit = tree.hit_test(100, 150).unwrap();
    assert_eq!(tree.nodes[hit].attr("text"), "Welcome");
    assert_eq!(tree.xpath(hit), "//*[@resource-id=\"com.example.app:id/card\"]");

    let capture = dev.start_capture(7, "port 443").await.unwrap();
    let remote = capture.remote_path.clone();
    tokio::time::sleep(Duration::from_millis(300)).await;
    dev.stop_capture(&capture).await.unwrap();
    assert_eq!(dev.remote_size(&remote).await, Some(24));
    let local = f.dir.path().join("c.pcap");
    dev.pull_to(&remote, &local).await.unwrap();
    assert_eq!(adbdeck::adb::tcpdump::count_pcap_packets(&local).unwrap(), 0);
    dev.remove_remote(&remote).await;
    assert_eq!(dev.remote_size(&remote).await, None);
}

#[tokio::test]
async fn ports_connect_and_pairing() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    dev.forward("8080", "tcp:9090").await.unwrap();
    dev.reverse("tcp:7000", "7001").await.unwrap();
    assert_eq!(dev.list_forwards().await.unwrap()[0].remote, "tcp:9090");
    assert_eq!(dev.list_reverses().await.unwrap()[0].local, "tcp:7001");
    dev.remove_forward_endpoint("tcp:8080").await.unwrap();
    assert!(dev.list_forwards().await.unwrap().is_empty());
    assert!(dev.forward("80; reboot", "tcp:1").await.is_err());

    f.adb.connect("192.168.1.9", Some(5555)).await.unwrap();
    f.adb.pair("192.168.1.9", 37000, "123456").await.unwrap();
    assert!(f.adb.connect("-bad", None).await.is_err(), "hosts cannot look like options");
    assert!(f.adb.pair("192.168.1.9", 37000, "12").await.is_err());
}

#[tokio::test]
async fn logcat_streams_and_stops_when_dropped() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    let mut stream = dev.start_logcat().await.unwrap();
    let line = tokio::time::timeout(Duration::from_secs(5), stream.lines.recv()).await.unwrap().unwrap();
    let entry = adbdeck::adb::logcat::parse_threadtime(&line).unwrap();
    assert_eq!(entry.tag, "DemoTag");
    drop(stream);
    // The adb child must not outlive its stream.
    let pid_gone = wait_for(|| !process_running("fake-adb", f.dir.path()), Duration::from_secs(3)).await;
    assert!(pid_gone, "logcat child is still running after the stream was dropped");
}

// ---- failure handling ----------------------------------------------------

#[tokio::test]
async fn missing_tools_are_reported_with_install_guidance() {
    let env = LookupEnv::default();
    assert_eq!(locate(Tool::Adb, "", &env), None);
    let statuses = detect_all(&Default::default(), &env);
    assert!(statuses.iter().all(|s| s.path.is_none()));
    let msg = Tool::Adb.missing_error().to_string();
    assert!(msg.contains("dnf install android-tools"), "{msg}");
    // A configured but unusable path is not replaced by another tool.
    let env = LookupEnv { path: vec![fixture_dir()], ..Default::default() };
    assert_eq!(locate(Tool::Adb, "/nonexistent/adb", &env), None);
}

#[tokio::test]
async fn spawn_failure_is_a_typed_error() {
    let adb = Adb::new("/nonexistent/adb");
    assert!(matches!(adb.snapshot().await, Err(Error::Spawn { .. })));
    let dir = tempfile::tempdir().unwrap();
    let not_exec = dir.path().join("adb");
    fs::write(&not_exec, "#!/bin/sh\n").unwrap();
    assert!(matches!(Adb::new(&not_exec).snapshot().await, Err(Error::Spawn { .. })));
}

#[tokio::test]
async fn nonzero_exit_and_invalid_output_are_errors() {
    let f = Fake::new("export FAKE_ADB_MODE=exit1");
    match f.adb.list_devices().await {
        Err(Error::Exit { code: Some(1), message, .. }) => assert!(message.contains("simulated failure"), "{message}"),
        other => panic!("unexpected {other:?}"),
    }
    let f = Fake::new("export FAKE_ADB_MODE=garbage");
    assert!(matches!(f.adb.list_devices().await, Err(Error::Parse { .. })));
}

#[tokio::test]
async fn a_vanished_device_is_reported_as_disconnected() {
    let f = Fake::new("export FAKE_ADB_DEVICES=''");
    let err = f.adb.device(SERIAL).shell("id").await.unwrap_err();
    assert!(err.is_device_gone(), "{err:?}");
    assert!(f.adb.device(SERIAL).list_dir("/sdcard", false).await.unwrap_err().is_device_gone());
    let f = Fake::new("export FAKE_ADB_DEVICES='FAKE0001\tunauthorized'");
    let devices = f.adb.snapshot().await.unwrap();
    assert_eq!(devices[0].state, DeviceState::Unauthorized);
    assert!(f.adb.device(SERIAL).shell("id").await.unwrap_err().is_device_gone());
}

#[tokio::test]
async fn a_hung_command_times_out_and_is_killed() {
    let f = Fake::new("export FAKE_ADB_MODE=timeout");
    let started = Instant::now();
    let err = f.adb.device(SERIAL).shell_output("id", Duration::from_millis(500)).await.unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    let gone = wait_for(|| !process_running("fake-adb", f.dir.path()), Duration::from_secs(3)).await;
    assert!(gone, "the timed-out child must be killed");
}

#[tokio::test]
async fn cancelling_a_job_kills_its_process() {
    use adbdeck::jobs::Spawner;
    use std::sync::Arc;
    let f = Fake::new("export FAKE_ADB_MODE=timeout");
    let sp = Spawner::new(tokio::runtime::Handle::current(), Arc::new(|| {}));
    let dev = f.adb.device(SERIAL);
    let job = sp.spawn(async move { dev.shell_output("id", Duration::from_secs(300)).await.map(|_| ()) });
    assert!(wait_for(|| process_running("fake-adb", f.dir.path()), Duration::from_secs(3)).await, "process never started");
    drop(job);
    assert!(wait_for(|| !process_running("fake-adb", f.dir.path()), Duration::from_secs(3)).await, "process survived cancellation");
}

#[tokio::test]
async fn watcher_follows_the_device_list_and_reconnects() {
    let f = Fake::new("export FAKE_ADB_TRACK_ENDS=1");
    let (tx, mut rx) = mpsc::channel(8);
    let task = tokio::spawn(watch_devices(f.adb.clone(), tx, WatchOptions::default()));
    let first = next_devices(&mut rx).await;
    assert_eq!(first[0].id, SERIAL);
    // track-devices ends right away in this setup; the watcher must come back
    // with a fresh snapshot instead of giving up.
    let again = next_devices(&mut rx).await;
    assert_eq!(again[0].id, SERIAL);
    task.abort();
}

const PHONE: &str = "192.168.1.20:5555";

fn listing(wifi: Option<&str>) -> String {
    let mut text = "FAKE0001\tdevice\tusb:1-1 product:panther model:Pixel_7\n".to_string();
    if let Some(state) = wifi {
        text.push_str(&format!("{PHONE}\t{state}\tproduct:panther model:Pixel_7\n"));
    }
    text
}

/// Next device list, skipping progress events.
async fn next_devices(rx: &mut mpsc::Receiver<WatchEvent>) -> Vec<adbdeck::adb::devices::Device> {
    loop {
        match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await.expect("watcher went quiet").expect("watcher ended") {
            WatchEvent::Devices(d) => return d,
            WatchEvent::Failed(e) => panic!("unexpected failure: {e}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_dropped_wifi_phone_is_reconnected_by_itself() {
    let f = Fake::new("export FAKE_ADB_TRACK_ENDS=1");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("devices"), listing(Some("device"))).unwrap();
    let (tx, mut rx) = mpsc::channel(32);
    let fast = Backoff { attempts: 5, first: Duration::from_millis(100), cap: Duration::from_millis(200) };
    let task = tokio::spawn(watch_devices(f.adb.clone(), tx, WatchOptions { backoff: fast, ..WatchOptions::default() }));
    assert!(next_devices(&mut rx).await.iter().any(|d| d.id == PHONE && d.is_online()));

    // The Wi-Fi link drops; connecting again brings it back.
    fs::write(home.join("on-connect"), listing(Some("device"))).unwrap();
    fs::write(home.join("devices"), listing(None)).unwrap();
    let mut events = Vec::new();
    let restored = tokio::time::timeout(Duration::from_secs(25), async {
        while let Some(e) = rx.recv().await {
            let done = matches!(&e, WatchEvent::Restored(id) if id == PHONE);
            events.push(e);
            if done {
                return true;
            }
        }
        false
    })
    .await;
    assert_eq!(restored, Ok(true), "never restored: {events:?}");
    assert!(events.iter().any(|e| matches!(e, WatchEvent::Reconnecting { id, attempt: 1, .. } if id == PHONE)), "{events:?}");
    task.abort();
}

#[tokio::test]
async fn a_phone_the_user_disconnected_stays_disconnected() {
    let f = Fake::new("export FAKE_ADB_TRACK_ENDS=1");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("devices"), listing(Some("device"))).unwrap();
    let suppressed = Suppressed::default();
    let (tx, mut rx) = mpsc::channel(32);
    let fast = Backoff { attempts: 5, first: Duration::from_millis(50), cap: Duration::from_millis(100) };
    let task = tokio::spawn(watch_devices(
        f.adb.clone(),
        tx,
        WatchOptions { backoff: fast, suppressed: suppressed.clone(), ..WatchOptions::default() },
    ));
    assert!(next_devices(&mut rx).await.iter().any(|d| d.id == PHONE));
    suppressed.lock().unwrap().insert(PHONE.to_string());
    fs::write(home.join("devices"), listing(None)).unwrap();
    let mut events = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(6), async {
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
    })
    .await;
    assert!(!events.iter().any(|e| matches!(e, WatchEvent::Reconnecting { .. })), "{events:?}");
    assert!(
        events.iter().any(|e| matches!(e, WatchEvent::Devices(d) if d.iter().all(|d| d.id != PHONE))),
        "the drop itself is still reported"
    );
    task.abort();
}

/// What `adb mdns services` lists for a phone with Wireless debugging on.
const MDNS_ROW: &str = "adb-FAKE9999-AbCdEf\t_adb-tls-connect._tcp.\t192.168.1.20:5555\n";

fn scanning() -> WatchOptions {
    WatchOptions { scan_every: Duration::from_millis(100), ..WatchOptions::default() }
}

/// How many times the watcher ran `adb connect`.
fn connect_calls(f: &Fake) -> usize {
    fs::read_to_string(f.dir.path().join("home/adb.log")).unwrap_or_default().lines().filter(|l| l.starts_with("connect ")).count()
}

#[tokio::test]
async fn a_phone_advertising_on_the_network_is_connected_without_being_remembered() {
    let f = Fake::new("export FAKE_ADB_LOG=\"$FAKE_ADB_HOME/adb.log\"");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("devices"), listing(None)).unwrap();
    fs::write(home.join("mdns"), MDNS_ROW).unwrap();
    fs::write(home.join("on-connect"), listing(Some("device"))).unwrap();
    let (tx, mut rx) = mpsc::channel(32);
    let task = tokio::spawn(watch_devices(f.adb.clone(), tx, scanning()));

    let mut events = Vec::new();
    let mut online = false;
    while !online {
        let e = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await.expect("watcher went quiet").expect("watcher ended");
        online = matches!(&e, WatchEvent::Devices(d) if d.iter().any(|d| d.id == PHONE && d.is_online()));
        events.push(e);
    }
    assert!(events.iter().any(|e| matches!(e, WatchEvent::Found(id) if id == PHONE)), "{events:?}");
    assert_eq!(connect_calls(&f), 1, "connected once, then the phone showed up in the list");
    task.abort();
}

#[tokio::test]
async fn a_phone_that_does_not_come_up_is_not_hammered() {
    let f = Fake::new("export FAKE_ADB_LOG=\"$FAKE_ADB_HOME/adb.log\"");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("devices"), listing(None)).unwrap();
    fs::write(home.join("mdns"), MDNS_ROW).unwrap();
    // No on-connect file: the connect "works" but the phone never appears.
    let (tx, mut rx) = mpsc::channel(256);
    let task = tokio::spawn(watch_devices(f.adb.clone(), tx, scanning()));
    let _ = tokio::time::timeout(Duration::from_millis(1500), async { while rx.recv().await.is_some() {} }).await;
    assert_eq!(connect_calls(&f), 1, "about 15 scans ran, one connect");
    task.abort();
}

#[tokio::test]
async fn a_phone_the_user_disconnected_is_not_found_again_on_a_new_port() {
    let f = Fake::new("export FAKE_ADB_LOG=\"$FAKE_ADB_HOME/adb.log\"");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("devices"), listing(None)).unwrap();
    fs::write(home.join("mdns"), MDNS_ROW).unwrap();
    fs::write(home.join("on-connect"), listing(Some("device"))).unwrap();
    // The user disconnected this phone while it listened on another port.
    let suppressed = Suppressed::default();
    suppressed.lock().unwrap().insert("192.168.1.20:40555".to_string());
    let (tx, mut rx) = mpsc::channel(256);
    let task = tokio::spawn(watch_devices(f.adb.clone(), tx, WatchOptions { suppressed, ..scanning() }));
    let mut events = Vec::new();
    let _ = tokio::time::timeout(Duration::from_millis(1500), async {
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
    })
    .await;
    assert_eq!(connect_calls(&f), 0);
    assert!(!events.iter().any(|e| matches!(e, WatchEvent::Found(_))), "{events:?}");
    task.abort();
}

/// A watcher fed by a reload function, as the background service runs it.
async fn reloaded_watcher_events(f: &Fake, registry: Registry) -> Vec<WatchEvent> {
    let reload: Reload = std::sync::Arc::new(move || registry.clone());
    let (tx, mut rx) = mpsc::channel(256);
    let task = tokio::spawn(watch_devices(f.adb.clone(), tx, WatchOptions { reload: Some(reload), ..scanning() }));
    let mut events = Vec::new();
    let _ = tokio::time::timeout(Duration::from_millis(1500), async {
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
    })
    .await;
    task.abort();
    events
}

#[tokio::test]
async fn the_background_service_leaves_a_phone_alone_that_the_saved_data_blocks() {
    let f = Fake::new("export FAKE_ADB_LOG=\"$FAKE_ADB_HOME/adb.log\"");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("devices"), listing(None)).unwrap();
    fs::write(home.join("mdns"), MDNS_ROW).unwrap();
    fs::write(home.join("on-connect"), listing(Some("device"))).unwrap();
    // Saved by the window when the user pressed Disconnect, on another port than the one advertised now.
    let registry = Registry { blocked: ["192.168.1.20:40555".to_string()].into(), ..Registry::default() };
    let events = reloaded_watcher_events(&f, registry).await;
    assert_eq!(connect_calls(&f), 0);
    assert!(!events.iter().any(|e| matches!(e, WatchEvent::Found(_))), "{events:?}");
}

#[tokio::test]
async fn the_background_service_connects_a_phone_the_saved_data_does_not_block() {
    let f = Fake::new("export FAKE_ADB_LOG=\"$FAKE_ADB_HOME/adb.log\"");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("devices"), listing(None)).unwrap();
    fs::write(home.join("mdns"), MDNS_ROW).unwrap();
    fs::write(home.join("on-connect"), listing(Some("device"))).unwrap();
    let events = reloaded_watcher_events(&f, Registry::default()).await;
    assert!(events.iter().any(|e| matches!(e, WatchEvent::Found(id) if id == PHONE)), "{events:?}");
}

#[tokio::test]
async fn an_offline_entry_does_not_stop_a_phone_from_being_connected_again() {
    let f = Fake::new("export FAKE_ADB_LOG=\"$FAKE_ADB_HOME/adb.log\"");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    // The old transport is still listed, but dead.
    fs::write(home.join("devices"), listing(Some("offline"))).unwrap();
    fs::write(home.join("mdns"), MDNS_ROW).unwrap();
    fs::write(home.join("on-connect"), listing(Some("device"))).unwrap();
    let (tx, mut rx) = mpsc::channel(256);
    let task = tokio::spawn(watch_devices(f.adb.clone(), tx, scanning()));
    let mut online = false;
    while !online {
        let e = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await.expect("watcher went quiet").expect("watcher ended");
        online = matches!(&e, WatchEvent::Devices(d) if d.iter().any(|d| d.id == PHONE && d.is_online()));
    }
    assert!(connect_calls(&f) >= 1);
    task.abort();
}

#[tokio::test]
async fn switching_auto_reconnect_off_also_stops_the_network_scan() {
    let f = Fake::new("export FAKE_ADB_LOG=\"$FAKE_ADB_HOME/adb.log\"");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("devices"), listing(None)).unwrap();
    fs::write(home.join("mdns"), MDNS_ROW).unwrap();
    let (tx, mut rx) = mpsc::channel(256);
    let task = tokio::spawn(watch_devices(f.adb.clone(), tx, WatchOptions { auto_reconnect: false, ..scanning() }));
    let _ = tokio::time::timeout(Duration::from_millis(1000), async { while rx.recv().await.is_some() {} }).await;
    let log = fs::read_to_string(home.join("adb.log")).unwrap_or_default();
    assert!(!log.lines().any(|l| l.starts_with("mdns") || l.starts_with("connect")), "{log}");
    task.abort();
}

#[tokio::test]
async fn remembered_wifi_phones_are_connected_at_start() {
    let f = Fake::new("");
    let home = f.dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("devices"), listing(None)).unwrap();
    fs::write(home.join("on-connect"), listing(Some("device"))).unwrap();
    let (tx, mut rx) = mpsc::channel(32);
    let options = WatchOptions {
        remembered: vec![PHONE.into()],
        backoff: Backoff { attempts: 3, first: Duration::from_millis(50), cap: Duration::from_millis(50) },
        ..WatchOptions::default()
    };
    let task = tokio::spawn(watch_devices(f.adb.clone(), tx, options));
    let mut found = false;
    for _ in 0..6 {
        found |= next_devices(&mut rx).await.iter().any(|d| d.id == PHONE && d.is_online());
        if found {
            break;
        }
    }
    assert!(found, "the remembered phone never came up");
    task.abort();
}

#[tokio::test]
async fn adb_hiccups_are_retried_and_a_dead_link_still_fails() {
    // Two protocol faults, then success: the caller never notices.
    let f = Fake::new("export FAKE_ADB_FLAKY=2");
    let out = f.adb.device(SERIAL).shell("echo hello").await.unwrap();
    assert_eq!(out, "hello");
    assert_eq!(fs::read_to_string(f.state().join("flaky")).unwrap().trim(), "2");

    // A link that never recovers gives up after the bounded number of tries.
    let f = Fake::new("export FAKE_ADB_FLAKY=99");
    let started = Instant::now();
    assert!(f.adb.device(SERIAL).shell("echo hello").await.is_err());
    assert_eq!(fs::read_to_string(f.state().join("flaky")).unwrap().trim(), "3", "three tries, no more");
    assert!(started.elapsed() < Duration::from_secs(10));

    // A command that ran and failed is not repeated.
    let f = Fake::new("");
    let _ = f.adb.device(SERIAL).shell_ok("exit 3").await;
    assert!(!f.state().join("flaky").exists());
}

#[test]
fn shutdown_during_an_active_operation_does_not_hang_or_leak() {
    let f = Fake::new("export FAKE_ADB_MODE=timeout");
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let dev = f.adb.device(SERIAL);
    let handle = rt.spawn(async move { dev.shell_output("id", Duration::from_secs(300)).await });
    std::thread::sleep(Duration::from_millis(500));
    assert!(process_running_sync("fake-adb", f.dir.path()));
    let started = Instant::now();
    rt.shutdown_timeout(Duration::from_secs(3));
    assert!(started.elapsed() < Duration::from_secs(4), "runtime shutdown blocked");
    drop(handle);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!process_running_sync("fake-adb", f.dir.path()), "child survived the runtime shutdown");
}

// ---- helpers ---------------------------------------------------------------

/// True while a live process carries this fixture's `FAKE_ADB_HOME` in its
/// environment (the wrapper exports it, so adb and its children all do).
fn process_running_sync(_needle: &str, dir: &Path) -> bool {
    let marker = format!("FAKE_ADB_HOME={}", dir.join("home").display());
    let Ok(entries) = fs::read_dir("/proc") else { return false };
    entries.filter_map(Result::ok).any(|e| {
        let Ok(env) = fs::read(e.path().join("environ")) else { return false };
        String::from_utf8_lossy(&env).split('\0').any(|v| v == marker)
    })
}

fn process_running(needle: &str, dir: &Path) -> bool {
    process_running_sync(needle, dir)
}

async fn wait_for(mut cond: impl FnMut() -> bool, limit: Duration) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cond()
}

// ---- embedded screen mirroring ----------------------------------------------------------------

/// Polls `f` until it returns a value; panics after `secs` seconds.
async fn eventually<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn mirror_options(scid: u32) -> adbdeck::mirror::ServerOptions {
    adbdeck::mirror::ServerOptions {
        scid,
        bit_rate: 8_000_000,
        max_size: 0,
        max_fps: 0,
        control: true,
        stay_awake: true,
        show_touches: false,
        power_off_on_close: false,
        clipboard_autosync: true,
    }
}

/// The test video as the server would send it: one configuration packet, then one packet per picture.
fn video_packets() -> Vec<(bool, Vec<u8>)> {
    let stream = fs::read(fixture_dir().join("test-320x240.h264")).unwrap();
    let mut out = Vec::new();
    let mut config = Vec::new();
    for nal in openh264::nal_units(&stream) {
        let kind = nal.iter().position(|&b| b == 1).map(|i| nal[i + 1] & 0x1f).unwrap_or(0);
        if kind == 7 || kind == 8 {
            config.extend_from_slice(nal);
        } else {
            if !config.is_empty() {
                out.push((true, std::mem::take(&mut config)));
            }
            out.push((false, nal.to_vec()));
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_mirror_streams_video_and_carries_input_both_ways() {
    use adbdeck::mirror::proto::{PacketHeader, CODEC_H264};
    use adbdeck::mirror::{session::SERVER_JAR, Control, Event, KeyAction, Session};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    let f = Fake::new("");
    let opts = mirror_options(0x00ab_c123);
    let mut session = Session::start(
        f.adb.device(SERIAL),
        opts,
        &f.dir.path().join("scratch"),
        &tokio::runtime::Handle::current(),
        std::sync::Arc::new(|| {}),
    );

    // The server jar reaches the device unchanged, and the reverse tunnel names the session's socket.
    let reverses = f.state().join("reverses");
    let port: u16 = eventually(10, "the reverse tunnel", || {
        fs::read_to_string(&reverses).ok()?.lines().find_map(|l| l.strip_prefix("localabstract:scrcpy_00abc123 tcp:")?.trim().parse().ok())
    })
    .await;
    assert_eq!(fs::read(f.dir.path().join("home/root/tmp/adbdeck/scrcpy-server.jar")).unwrap(), SERVER_JAR);

    // Play the phone: connect video, then control, as the server does.
    let mut video = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut control = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut name = [0u8; 64];
    name[..7].copy_from_slice(b"Pixel 7");
    video.write_all(&name).await.unwrap();
    let mut codec = Vec::new();
    codec.extend(CODEC_H264.to_be_bytes());
    codec.extend(320u32.to_be_bytes());
    codec.extend(240u32.to_be_bytes());
    video.write_all(&codec).await.unwrap();
    let started = eventually(10, "the Started event", || session.poll_event()).await;
    assert_eq!(started, Event::Started { device_name: "Pixel 7".into(), width: 320, height: 240 });

    let packets = video_packets();
    for (i, (config, data)) in packets.iter().enumerate() {
        let mut head = Vec::new();
        let flags = if *config {
            1u64 << 63
        } else if i == 1 {
            1u64 << 62
        } else {
            i as u64 * 100_000
        };
        head.extend(flags.to_be_bytes());
        head.extend((data.len() as u32).to_be_bytes());
        assert_eq!(head.len(), PacketHeader::LEN);
        video.write_all(&head).await.unwrap();
        video.write_all(data).await.unwrap();
    }
    let frame = eventually(10, "a decoded picture", || session.take_frame()).await;
    assert_eq!((frame.width, frame.height, frame.rgba.len()), (320, 240, 320 * 240 * 4));

    // Input goes to the control socket in the server's wire format.
    session.send(Control::Key { action: KeyAction::Down, keycode: 3, repeat: 0, meta: 0 });
    let mut key = [0u8; 14];
    tokio::time::timeout(Duration::from_secs(5), control.read_exact(&mut key)).await.expect("the key arrives").unwrap();
    assert_eq!(key, [0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0]);

    // A clipboard message from the device is surfaced.
    control.write_all(&[0, 0, 0, 0, 2, b'h', b'i']).await.unwrap();
    let clip = eventually(10, "the clipboard event", || session.poll_event()).await;
    assert_eq!(clip, Event::Clipboard("hi".into()));

    // Stopping ends the session cleanly and removes the tunnel.
    session.stop();
    let ended = eventually(10, "the Ended event", || session.poll_event()).await;
    assert_eq!(ended, Event::Ended(None));
    eventually(10, "the tunnel to be removed", || {
        (!fs::read_to_string(&reverses).unwrap_or_default().contains("scrcpy_00abc123")).then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_dies_on_start_is_reported_with_its_message() {
    use adbdeck::mirror::{Event, Session};
    let f = Fake::new("export FAKE_SERVER_CRASH=1");
    let start = Instant::now();
    let mut session = Session::start(
        f.adb.device(SERIAL),
        mirror_options(0x0000_0042),
        &f.dir.path().join("scratch"),
        &tokio::runtime::Handle::current(),
        std::sync::Arc::new(|| {}),
    );
    let ended = eventually(15, "the session to fail", || session.poll_event()).await;
    let Event::Ended(Some(why)) = ended else { panic!("expected a failure, got {ended:?}") };
    assert!(why.contains("stopped before it connected") && why.contains("class com.genymobile.scrcpy.Server not found"), "{why}");
    assert!(start.elapsed() < Duration::from_secs(8), "a dead server is noticed long before the connect time-out");
}

// ---- app management ------------------------------------------------------------------------

fn log_lines(path: PathBuf) -> Vec<String> {
    fs::read_to_string(path).unwrap_or_default().lines().map(str::to_string).collect()
}

#[tokio::test]
async fn app_management_operations_reach_the_device_and_read_back() {
    use adbdeck::adb::manage::{Bucket, OpMode};
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    let pkg = "com.example.app";
    let pm_log = || log_lines(f.state().join("pm.log"));

    // Reading: runtime permission state, special access, bucket and exemption in one call.
    let d = dev.app_details(pkg).await.unwrap();
    let camera = d.permission("android.permission.CAMERA").unwrap();
    assert_eq!((camera.granted, camera.kind), (Some(false), adbdeck::adb::apps::PermKind::Runtime));
    assert_eq!(d.op_mode("SYSTEM_ALERT_WINDOW"), OpMode::Deny);
    assert_eq!(d.bucket, Some(Bucket::Active));
    assert!(!d.battery_exempt);

    // Permissions: granting reaches pm, and a fixed permission explains itself.
    dev.set_permission(pkg, "android.permission.CAMERA", true).await.unwrap();
    assert_eq!(pm_log(), vec!["grant com.example.app android.permission.CAMERA"]);
    let err = dev.set_permission(pkg, "android.permission.INTERNET", false).await.unwrap_err().to_string();
    assert!(err.contains("INTERNET is fixed at install time"), "{err}");

    // Special access, bucket and battery exemption change and read back.
    dev.set_app_op(pkg, "SYSTEM_ALERT_WINDOW", &OpMode::Allow).await.unwrap();
    dev.set_standby_bucket(pkg, Bucket::Rare).await.unwrap();
    dev.set_battery_exempt(pkg, true).await.unwrap();
    let d = dev.app_details(pkg).await.unwrap();
    assert_eq!((d.op_mode("SYSTEM_ALERT_WINDOW"), d.bucket, d.battery_exempt), (OpMode::Allow, Some(Bucket::Rare), true));
    dev.set_battery_exempt(pkg, false).await.unwrap();
    assert!(!dev.app_details(pkg).await.unwrap().battery_exempt);
}

#[tokio::test]
async fn app_lifecycle_operations_reach_the_device() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    let pkg = "com.example.app";
    let pm_log = || log_lines(f.state().join("pm.log"));

    // Lifecycle: suspend, hide, remove for the user and bring back, compile, kill.
    dev.set_suspended(pkg, true).await.unwrap();
    dev.set_hidden(pkg, true).await.unwrap();
    dev.remove_for_user(pkg).await.unwrap();
    dev.install_existing(pkg).await.unwrap();
    dev.compile(pkg, Some("speed-profile")).await.unwrap();
    dev.compile(pkg, None).await.unwrap();
    dev.kill_background(pkg).await.unwrap();
    dev.open_app_settings(pkg).await.unwrap();
    assert_eq!(
        pm_log()[..],
        [
            "suspend com.example.app",
            "hide com.example.app",
            "uninstall -k --user 0 com.example.app",
            "package install-existing --user 0 com.example.app",
        ]
    );
    assert_eq!(
        log_lines(f.state().join("compile.log")),
        ["package compile -m speed-profile -f com.example.app", "package compile --reset com.example.app"]
    );
    assert_eq!(log_lines(f.state().join("killed.log")), [pkg]);
    assert!(log_lines(f.state().join("started.log"))[0].contains("APPLICATION_DETAILS_SETTINGS"));
}

#[tokio::test]
async fn unvalidated_names_never_reach_the_device() {
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    let pkg = "com.example.app";
    let pm_log = || log_lines(f.state().join("pm.log"));

    // Nothing unvalidated reaches the device.
    let before = pm_log().len();
    for bad in ["a;reboot", "$(id)", "a b", ""] {
        assert!(matches!(dev.set_permission(bad, "android.permission.CAMERA", true).await, Err(Error::Invalid(_))), "{bad}");
        assert!(matches!(dev.set_permission(pkg, bad, true).await, Err(Error::Invalid(_))), "{bad}");
        assert!(matches!(dev.set_suspended(bad, true).await, Err(Error::Invalid(_))), "{bad}");
    }
    assert!(dev.compile(pkg, Some("speed; reboot")).await.is_err());
    assert_eq!(pm_log().len(), before);
}

#[tokio::test]
async fn clearing_a_cache_uses_the_best_route_the_device_offers() {
    let pkg = "com.example.app";
    // Android 14: one command clears the private and external caches.
    let f = Fake::new("");
    let cleared = f.adb.device(SERIAL).clear_app_cache(pkg).await.unwrap();
    assert!(cleared.internal && cleared.external);
    assert_eq!(log_lines(f.state().join("pm.log")), ["clear --cache-only --user 0 com.example.app"]);

    // Older Android, not debuggable: only the external cache can be emptied.
    let f = Fake::new("export FAKE_SDK=30");
    f.adb.device(SERIAL).current_user().await; // the fake creates its storage on first use
    let cache = f.sdcard().join("Android/data/com.example.app/cache");
    fs::create_dir_all(cache.join("sub")).unwrap();
    fs::write(cache.join("a.tmp"), b"x").unwrap();
    fs::write(cache.join("sub/b.tmp"), b"y").unwrap();
    let cleared = f.adb.device(SERIAL).clear_app_cache(pkg).await.unwrap();
    assert!(!cleared.internal && cleared.external);
    assert!(cache.is_dir() && fs::read_dir(&cache).unwrap().next().is_none(), "the folder stays, its contents go");
    assert!(log_lines(f.state().join("pm.log")).is_empty(), "no pm clear before Android 14");

    // Older Android and no external cache either: an honest error that points to the system-wide option.
    let f = Fake::new("export FAKE_SDK=30");
    let err = f.adb.device(SERIAL).clear_app_cache(pkg).await.unwrap_err().to_string();
    assert!(err.contains("Android 14") && err.contains("Clear all caches"), "{err}");
    assert!(f.adb.device(SERIAL).clear_app_cache("a;b").await.is_err());
}

#[tokio::test]
async fn all_caches_are_cleared_with_one_trim_command() {
    let f = Fake::new("");
    f.adb.device(SERIAL).trim_all_caches().await.unwrap();
    assert_eq!(log_lines(f.state().join("pm.log")), ["trim-caches 4096G"]);
}

// ---- device controls -----------------------------------------------------------------------

#[tokio::test]
async fn controls_read_back_what_was_applied() {
    use adbdeck::adb::control::{Command, DarkMode, Rotation};
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    let before = dev.controls().await.unwrap();
    assert_eq!((before.wifi, before.show_touches, before.dark_mode), (None, None, Some(DarkMode::Auto)), "unset settings read as unknown");
    assert_eq!((before.battery.level, before.battery.charging), (Some(87), true));
    assert_eq!((before.size.as_str(), before.density.as_str()), ("360x720", "320"));

    for c in [
        Command::ShowTouches(true),
        Command::DarkMode(DarkMode::Dark),
        Command::Rotation(Rotation::Deg90),
        Command::AnimationScale(0.5),
        Command::Brightness(128),
        Command::ScreenTimeoutSecs(45),
        Command::Size(Some((1080, 2400))),
        Command::Density(Some(420)),
    ] {
        dev.apply(&c).await.unwrap_or_else(|e| panic!("{c:?}: {e}"));
    }
    let after = dev.controls().await.unwrap();
    assert_eq!(after.show_touches, Some(true));
    assert_eq!(after.dark_mode, Some(DarkMode::Dark));
    assert_eq!(after.rotation, Some(Rotation::Deg90));
    assert_eq!(after.animation_scale, Some(0.5));
    assert_eq!((after.brightness, after.auto_brightness), (Some(128), Some(false)));
    assert_eq!(after.screen_timeout_secs, Some(45));
    assert_eq!((after.size.as_str(), after.density.as_str()), ("1080x2400", "420"));
    assert_eq!(after.physical_size, "360x720", "the panel's own size stays visible");

    dev.apply(&Command::Size(None)).await.unwrap();
    assert_eq!(dev.controls().await.unwrap().size, "360x720");
}

#[tokio::test]
async fn service_commands_and_reboots_reach_the_device() {
    use adbdeck::adb::control::{Command, RebootTarget, StatusBar};
    let f = Fake::new("");
    let dev = f.adb.device(SERIAL);
    for c in [
        Command::Wifi(false),
        Command::MobileData(true),
        Command::Airplane(true),
        Command::StayAwake(true),
        Command::BatteryLevel(15),
        Command::BatteryReset,
        Command::Doze(true),
        Command::Doze(false),
        Command::StatusBar(StatusBar::QuickSettings),
        Command::Reboot(RebootTarget::Recovery),
        Command::Reboot(RebootTarget::PowerOff),
    ] {
        dev.apply(&c).await.unwrap_or_else(|e| panic!("{c:?}: {e}"));
    }
    assert_eq!(
        log_lines(f.state().join("control.log")),
        [
            "svc wifi disable",
            "svc data enable",
            "cmd connectivity airplane-mode enable",
            "svc power stayon true",
            "dumpsys battery unplug",
            "dumpsys battery set level 15",
            "dumpsys battery reset",
            "dumpsys deviceidle force-idle",
            "dumpsys deviceidle unforce",
            "cmd statusbar expand-settings",
            "adb reboot recovery",
            "shell reboot -p",
        ]
    );
    let err = dev.apply(&Command::BatteryLevel(200)).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "range errors are reported before anything is sent");
}

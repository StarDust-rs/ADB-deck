//! The Play Store icon client, run against a stand-in `curl` that plays the metadata service.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use adbdeck::appicon::{cache_get, cache_put_found, decode_icon, fetch_batch, Entry, Options, PlayStore, Request};

const PACKAGE: &str = "com.example.app";
const ICON_URL: &str = "https://play-lh.googleusercontent.com/iconA";
const ONLINE: Options = Options { network: true, retry_failed: false };

struct Service {
    dir: tempfile::TempDir,
}

impl Service {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["listing", "icons"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn cache(&self) -> PathBuf {
        self.root().join("cache")
    }

    /// A `curl` that serves this service's files; `fail` makes it fail like an offline curl.
    fn store(&self, fail: bool) -> PlayStore {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-curl");
        let wrapper = self.root().join(if fail { "curl-offline" } else { "curl" });
        let failing = if fail { "export FAKE_CURL_FAIL=1\n" } else { "" };
        fs::write(&wrapper, format!("#!/bin/sh\nexport FAKE_CURL_DIR={}\n{failing}exec {} \"$@\"\n", self.root().display(), fixture.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        }
        PlayStore::with_curl(wrapper)
    }

    fn listing(&self, package: &str, json: &str) {
        fs::write(self.root().join("listing").join(format!("{package}.json")), json).unwrap();
    }

    fn icon(&self, name: &str, rgba: [u8; 4]) {
        let img = image::RgbaImage::from_pixel(72, 72, image::Rgba(rgba));
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();
        fs::write(self.root().join("icons").join(name), png.into_inner()).unwrap();
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.root().join("calls.log")).map(|t| t.lines().map(String::from).collect()).unwrap_or_default()
    }
}

fn ask(packages: &[&str]) -> Vec<Request> {
    packages.iter().map(|p| Request { package: p.to_string() }).collect()
}

fn listing_json(name: &str, logo: &str) -> String {
    format!(r#"{{"packageID":"{PACKAGE}","name":"{name}","logo":"{logo}","rating":"4.6"}}"#)
}

#[tokio::test]
async fn an_app_on_the_play_store_gets_its_icon_and_title_and_is_then_answered_from_the_cache() {
    let svc = Service::new();
    svc.listing(PACKAGE, &listing_json("Example App", ICON_URL));
    svc.icon("iconA", [200, 30, 30, 255]);
    let store = svc.store(false);

    let batch = fetch_batch(&store, &ask(&[PACKAGE]), &svc.cache(), ONLINE).await;
    assert!(batch.failure.is_none());
    let app = batch.done.iter().find(|d| d.package == PACKAGE).expect("answered");
    assert_eq!(app.entry.label, "Example App");
    assert_eq!(&app.entry.icon.as_ref().expect("it has an icon").rgba[..4], &[200, 30, 30, 255]);
    assert_eq!(svc.calls(), ["https://play.rajkumaar.co.in/json?id=com.example.app", ICON_URL]);

    let again = fetch_batch(&store, &ask(&[PACKAGE]), &svc.cache(), ONLINE).await;
    assert_eq!(again.done.len(), 1);
    assert_eq!(svc.calls().len(), 2, "the cache answered, nothing was requested");
    let kept = cache_get(&svc.cache().join("play"), PACKAGE, SystemTime::now()).unwrap();
    assert!(kept.fresh && matches!(kept.entry, Entry::Found(_)));
}

#[tokio::test]
async fn an_app_the_service_does_not_know_is_a_definite_answer_and_is_not_asked_again() {
    let svc = Service::new();
    let store = svc.store(false);
    let batch = fetch_batch(&store, &ask(&["com.android.shell"]), &svc.cache(), ONLINE).await;
    let app = &batch.done[0];
    assert!(app.entry.icon.is_none() && app.entry.label.is_empty());
    assert!(batch.failure.is_none());
    fetch_batch(&store, &ask(&["com.android.shell"]), &svc.cache(), ONLINE).await;
    assert_eq!(svc.calls().len(), 1, "a day passes before it is asked about again");
}

#[tokio::test]
async fn a_service_error_is_a_failure_that_is_not_hammered_but_can_be_retried() {
    let svc = Service::new();
    fs::write(svc.root().join("listing").join(format!("{PACKAGE}.status")), "503").unwrap();
    let store = svc.store(false);

    let batch = fetch_batch(&store, &ask(&[PACKAGE]), &svc.cache(), ONLINE).await;
    assert!(batch.done.is_empty(), "no answer, so the letter badge stays and nothing is cached as final");
    assert!(batch.failure.as_deref().is_some_and(|f| f.contains("503")), "{:?}", batch.failure);

    fetch_batch(&store, &ask(&[PACKAGE]), &svc.cache(), ONLINE).await;
    assert_eq!(svc.calls().len(), 1, "it waits ten minutes after a failure");
    fetch_batch(&store, &ask(&[PACKAGE]), &svc.cache(), Options { network: true, retry_failed: true }).await;
    assert_eq!(svc.calls().len(), 2, "Retry asks at once");
}

#[tokio::test]
async fn without_a_network_curl_failure_is_reported_and_the_old_icon_is_still_shown() {
    let svc = Service::new();
    // An icon kept for 40 days.
    svc.icon("kept", [1, 2, 3, 255]);
    let icon = decode_icon(&fs::read(svc.root().join("icons/kept")).unwrap()).unwrap();
    let dir = svc.cache().join("play");
    cache_put_found(&dir, PACKAGE, &icon, "Kept Title").unwrap();
    let label = fs::OpenOptions::new().write(true).open(dir.join(format!("{PACKAGE}.label"))).unwrap();
    label.set_modified(SystemTime::now() - Duration::from_secs(40 * 24 * 3600)).unwrap();

    let store = svc.store(true);
    let batch = fetch_batch(&store, &ask(&[PACKAGE, "com.other.app"]), &svc.cache(), ONLINE).await;
    assert_eq!(batch.done.len(), 1, "the kept icon is shown, the unknown app has to wait");
    assert_eq!((batch.done[0].package.as_str(), batch.done[0].entry.label.as_str()), (PACKAGE, "Kept Title"));
    assert!(batch.failure.as_deref().is_some_and(|f| f.contains("curl")), "{:?}", batch.failure);
}

#[tokio::test]
async fn with_the_network_off_only_the_cache_is_used() {
    let svc = Service::new();
    svc.listing(PACKAGE, &listing_json("Example App", ICON_URL));
    svc.icon("iconA", [5, 5, 5, 255]);
    let store = svc.store(false);
    let off = Options { network: false, retry_failed: false };

    let batch = fetch_batch(&store, &ask(&[PACKAGE]), &svc.cache(), off).await;
    assert!(batch.done.is_empty() && batch.failure.is_none());
    assert!(svc.calls().is_empty(), "nothing was requested");

    fetch_batch(&store, &ask(&[PACKAGE]), &svc.cache(), ONLINE).await;
    let batch = fetch_batch(&store, &ask(&[PACKAGE]), &svc.cache(), off).await;
    assert_eq!(batch.done.len(), 1, "what was fetched earlier is still shown");
}

#[tokio::test]
async fn an_icon_address_outside_googles_image_server_is_never_requested() {
    let svc = Service::new();
    svc.listing(PACKAGE, &listing_json("Example App", "https://evil.example/iconA"));
    let store = svc.store(false);
    let batch = fetch_batch(&store, &ask(&[PACKAGE]), &svc.cache(), ONLINE).await;
    assert!(batch.done[0].entry.icon.is_none());
    assert_eq!(svc.calls().len(), 1, "only the listing was requested");
}

#[tokio::test]
async fn names_that_are_not_package_names_never_reach_the_network() {
    let svc = Service::new();
    let store = svc.store(false);
    let batch = fetch_batch(&store, &ask(&["../etc/passwd", "a b", "x?id=y"]), &svc.cache(), ONLINE).await;
    assert!(batch.done.is_empty() && svc.calls().is_empty());
}

#[tokio::test]
async fn several_apps_are_asked_about_together_and_each_gets_its_own_answer() {
    let svc = Service::new();
    for (package, name, logo, rgba) in [("com.a.one", "One", "iconOne", [10, 0, 0, 255]), ("com.b.two", "Two", "iconTwo", [0, 10, 0, 255])] {
        svc.listing(package, &listing_json(name, &format!("https://play-lh.googleusercontent.com/{logo}")));
        svc.icon(logo, rgba);
    }
    let store = svc.store(false);
    let batch = fetch_batch(&store, &ask(&["com.a.one", "com.b.two", "com.c.three"]), &svc.cache(), ONLINE).await;
    let find = |p: &str| batch.done.iter().find(|d| d.package == p).unwrap_or_else(|| panic!("{p} missing"));
    assert_eq!(find("com.a.one").entry.label, "One");
    assert_eq!(&find("com.b.two").entry.icon.as_ref().unwrap().rgba[..4], &[0, 10, 0, 255]);
    assert!(find("com.c.three").entry.icon.is_none());
}

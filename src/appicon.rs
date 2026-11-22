//! App icons and names for the app list, from the Play Store.
//!
//! Android has no command that returns an app's icon. ADB Deck asks the open source
//! Play Store metadata API (<https://play.rajkumaar.co.in>, MIT, source at
//! `github.com/rajkumaar23/playstore-api`) for the store listing of a package: its title
//! and the address of its icon on Google's image server, and then downloads that icon.
//! Only package names leave the computer. Apps the Play Store does not have (system apps,
//! sideloaded builds) get a letter badge.
//!
//! The requests are made by the system's `curl`: it uses the system's trusted
//! certificates and crypto policy, and the program has no TLS stack of its own.
//!
//! Results are kept in a host cache, one set of files per package. A found icon is
//! kept for 30 days, an app the service does not know for one day, and a failed lookup
//! (offline, rate limited, server error) is not asked again for ten minutes. A kept icon
//! that is out of date is still shown when the refresh fails.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use image::imageops::FilterType;
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::error::{Error, Result};
use crate::process::{run, CommandSpec};

/// The service's JSON endpoint: `GET <API_URL>?id=<package>`.
pub const API_URL: &str = "https://play.rajkumaar.co.in/json";
/// Icons are only downloaded from Google's image server, whatever the service answers.
const ICON_URL_PREFIX: &str = "https://play-lh.googleusercontent.com/";
const USER_AGENT: &str = concat!("ADB-Deck/", env!("CARGO_PKG_VERSION"));

/// Edge length of the cached, resized icon.
pub const ICON_SIZE: u32 = 96;

const FOUND_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
const MISSING_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
const FAILED_MAX_AGE: Duration = Duration::from_secs(10 * 60);
/// The service is a free, community-run one: at most this many requests at a time.
const MAX_PARALLEL: usize = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_LISTING_BYTES: usize = 1024 * 1024;
const MAX_ICON_BYTES: usize = 4 * 1024 * 1024;
/// The service accepts package names of up to this many characters.
const MAX_PACKAGE_LEN: usize = 200;

/// An icon decoded to straight RGBA pixels, `ICON_SIZE` square.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub rgba: Vec<u8>,
}

impl Icon {
    pub fn size(&self) -> [usize; 2] {
        [ICON_SIZE as usize; 2]
    }

    /// Pixels of the icon scaled to `edge` square, for crisp display at small sizes.
    pub fn resized(&self, edge: u32) -> Vec<u8> {
        match image::RgbaImage::from_raw(ICON_SIZE, ICON_SIZE, self.rgba.clone()) {
            Some(img) => image::imageops::resize(&img, edge, edge, FilterType::Triangle).into_raw(),
            None => vec![0; (edge * edge * 4) as usize],
        }
    }
}

/// Decodes and resizes an image to a square `ICON_SIZE` icon.
pub fn decode_icon(bytes: &[u8]) -> Result<Icon> {
    let img = image::load_from_memory(bytes).map_err(|e| Error::parse("icon", e.to_string()))?;
    let img = img.resize_to_fill(ICON_SIZE, ICON_SIZE, FilterType::Triangle);
    Ok(Icon { rgba: img.to_rgba8().into_raw() })
}

fn io(context: &str, e: std::io::Error) -> Error {
    Error::io(context.to_string(), e)
}

/// What is known about one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached {
    /// `None`: the app has no icon to show.
    pub icon: Option<Icon>,
    /// The store title; empty when there is none.
    pub label: String,
}

/// Whether a package name can be sent to the service: letters, digits, `.`, `_` and `-`,
/// starting with a letter, up to 200 characters.
pub fn is_api_package(name: &str) -> bool {
    name.len() <= MAX_PACKAGE_LEN
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// An icon address the service returned is only followed when it is an `https` address
/// on Google's image server made of plain URL characters.
fn is_icon_url(url: &str) -> bool {
    url.len() <= 512
        && url.strip_prefix(ICON_URL_PREFIX).is_some_and(|rest| {
            !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '/' | '=' | '%' | '+'))
        })
}

/// The part of a store listing that is used.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listing {
    name: String,
    logo: String,
}

/// `Ok(None)`: a valid answer for an app without a usable icon address.
fn parse_listing(body: &[u8]) -> std::result::Result<Option<Listing>, serde_json::Error> {
    let json: Value = serde_json::from_slice(body)?;
    let text = |key: &str| json.get(key).and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let logo = text("logo");
    Ok(is_icon_url(&logo).then(|| Listing { name: text("name"), logo }))
}

// ---- the disk cache -----------------------------------------------------------------

/// What the cache holds for one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Found(Cached),
    /// The service does not know the app.
    Missing,
    /// The last lookup failed.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub entry: Entry,
    /// False when the entry is old enough to be asked about again.
    pub fresh: bool,
}

fn age(path: &Path, now: SystemTime) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    // A file from the future (clock change) counts as new.
    Some(now.duration_since(modified).unwrap_or(Duration::ZERO))
}

/// Looks `package` up in the cache directory; `None` when nothing usable is kept.
pub fn cache_get(dir: &Path, package: &str, now: SystemTime) -> Option<Lookup> {
    let file = |ext: &str| dir.join(format!("{package}.{ext}"));
    let failed_recently = age(&file("failed"), now).is_some_and(|a| a < FAILED_MAX_AGE);

    if let (Ok(label), Ok(img)) = (std::fs::read_to_string(file("label")), image::open(file("png"))) {
        let img = img.to_rgba8();
        if img.width() == ICON_SIZE && img.height() == ICON_SIZE {
            let fresh = age(&file("label"), now).is_some_and(|a| a < FOUND_MAX_AGE) || failed_recently;
            return Some(Lookup { entry: Entry::Found(Cached { icon: Some(Icon { rgba: img.into_raw() }), label }), fresh });
        }
    }
    if let Some(a) = age(&file("none"), now) {
        return Some(Lookup { entry: Entry::Missing, fresh: a < MISSING_MAX_AGE || failed_recently });
    }
    failed_recently.then_some(Lookup { entry: Entry::Failed, fresh: true })
}

fn remove(path: PathBuf) {
    let _ = std::fs::remove_file(path);
}

/// Remembers an icon and title. Failing to write only costs a second lookup later.
pub fn cache_put_found(dir: &Path, package: &str, icon: &Icon, label: &str) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| io("creating the icon cache", e))?;
    let img = image::RgbaImage::from_raw(ICON_SIZE, ICON_SIZE, icon.rgba.clone()).ok_or_else(|| Error::parse("icon", "wrong pixel count"))?;
    // Written beside the final name and renamed, so a crash never leaves half a PNG.
    let tmp = dir.join(format!("{package}.png.part"));
    img.save_with_format(&tmp, image::ImageFormat::Png).map_err(|e| Error::parse("icon", e.to_string()))?;
    std::fs::rename(&tmp, dir.join(format!("{package}.png"))).map_err(|e| io("saving the icon", e))?;
    remove(dir.join(format!("{package}.none")));
    remove(dir.join(format!("{package}.failed")));
    // Last, because a lookup only counts when this file exists.
    std::fs::write(dir.join(format!("{package}.label")), label).map_err(|e| io("saving the app name", e))
}

/// Remembers that the service does not know the app.
pub fn cache_put_missing(dir: &Path, package: &str) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| io("creating the icon cache", e))?;
    for ext in ["png", "label", "failed"] {
        remove(dir.join(format!("{package}.{ext}")));
    }
    std::fs::write(dir.join(format!("{package}.none")), b"").map_err(|e| io("saving the icon marker", e))
}

/// Remembers that a lookup failed, so it is not repeated at once. An icon already kept stays.
pub fn cache_put_failed(dir: &Path, package: &str) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| io("creating the icon cache", e))?;
    std::fs::write(dir.join(format!("{package}.failed")), b"").map_err(|e| io("saving the failure marker", e))
}

// ---- the service -------------------------------------------------------------------

/// A client of the Play Store metadata service.
#[derive(Debug, Clone)]
pub struct PlayStore {
    curl: PathBuf,
}

impl Default for PlayStore {
    fn default() -> Self {
        Self::with_curl("curl")
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

/// Splits what `curl --write-out '\n%{http_code}'` printed into the body and the status.
fn split_reply(stdout: &[u8]) -> Option<(u16, &[u8])> {
    let newline = stdout.iter().rposition(|b| *b == b'\n')?;
    let status = std::str::from_utf8(&stdout[newline + 1..]).ok()?.trim().parse::<u16>().ok()?;
    Some((status, &stdout[..newline]))
}

impl PlayStore {
    /// Uses the given `curl` program (a stand-in in tests).
    pub fn with_curl(curl: impl Into<PathBuf>) -> Self {
        Self { curl: curl.into() }
    }

    async fn get(&self, url: &str, accept: &str, max_body: usize) -> Result<Reply> {
        let mut args: Vec<String> = ["--silent", "--show-error", "--location", "--max-redirs", "3", "--proto", "=https", "--proto-redir", "=https"]
            .into_iter()
            .map(String::from)
            .collect();
        args.extend([
            "--connect-timeout".into(),
            "8".into(),
            "--max-time".into(),
            REQUEST_TIMEOUT.as_secs().to_string(),
            "--max-filesize".into(),
            max_body.to_string(),
            "--user-agent".into(),
            USER_AGENT.into(),
            "--header".into(),
            format!("Accept: {accept}"),
            "--write-out".into(),
            "\n%{http_code}".into(),
            "--".into(),
            url.into(),
        ]);
        let spec = CommandSpec::new(self.curl.clone()).args(args).timeout(REQUEST_TIMEOUT + Duration::from_secs(5)).max_stdout(max_body + 16);
        let out = run(&spec).await?;
        if !out.success() {
            return Err(Error::Exit { program: "curl".into(), code: out.code, message: out.failure_message() });
        }
        if out.truncated {
            return Err(Error::Invalid("the reply was larger than expected".into()));
        }
        let (status, body) = split_reply(&out.stdout).ok_or_else(|| Error::parse("curl", "no status in the reply"))?;
        Ok(Reply { status, body: body.to_vec() })
    }

    /// Looks one package up: its listing, then its icon.
    async fn lookup(&self, package: &str) -> Outcome {
        let listing = match self.get(&format!("{API_URL}?id={package}"), "application/json", MAX_LISTING_BYTES).await {
            Ok(reply) => reply,
            Err(e) => return Outcome::Failed(e.to_string()),
        };
        match listing.status {
            200 => {}
            // 400 is a name the service refuses, 404 an app that is not on the Play Store
            400 | 404 => return Outcome::Missing,
            other => return Outcome::Failed(format!("the icon service answered HTTP {other}")),
        }
        let listing = match parse_listing(&listing.body) {
            Ok(Some(listing)) => listing,
            Ok(None) => return Outcome::Missing,
            Err(_) => return Outcome::Failed("the icon service sent something that is not JSON".into()),
        };
        let icon = match self.get(&listing.logo, "image/png,image/jpeg;q=0.9", MAX_ICON_BYTES).await {
            Ok(reply) if reply.status == 200 => reply,
            Ok(reply) => return Outcome::Failed(format!("the icon was not available (HTTP {})", reply.status)),
            Err(e) => return Outcome::Failed(e.to_string()),
        };
        match decode_icon(&icon.body) {
            Ok(icon) => Outcome::Found(Cached { icon: Some(icon), label: listing.name }),
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }
}

enum Outcome {
    Found(Cached),
    Missing,
    Failed(String),
}

/// One app whose icon is wanted.
#[derive(Debug, Clone)]
pub struct Request {
    pub package: String,
}

/// One app that was resolved, from the cache or from the service.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub package: String,
    pub entry: Cached,
}

/// Result of looking up a batch of apps.
#[derive(Debug, Default)]
pub struct Batch {
    pub done: Vec<Fetched>,
    /// Why the service could not be used, when no lookup in the batch got an answer.
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// False: only what the cache holds is used, nothing is asked on the network.
    pub network: bool,
    /// Ask again about lookups that failed recently, instead of waiting out the ten minutes.
    pub retry_failed: bool,
}

fn answer(entry: &Entry) -> Option<Cached> {
    match entry {
        Entry::Found(cached) => Some(cached.clone()),
        Entry::Missing => Some(Cached { icon: None, label: String::new() }),
        Entry::Failed => None,
    }
}

/// Resolves `requests`: the cache first, then the service for what is missing or old.
/// Definite answers (including "not on the Play Store") are cached. Apps that could not be
/// answered because of a network or service problem are absent from the result unless an
/// older answer is kept, so a later run asks again.
pub async fn fetch_batch(store: &PlayStore, requests: &[Request], cache: &Path, options: Options) -> Batch {
    let dir = cache.join("play");
    let now = SystemTime::now();
    let mut batch = Batch::default();
    let mut to_ask: Vec<(String, Option<Cached>)> = Vec::new();

    for req in requests.iter().filter(|r| is_api_package(&r.package)) {
        let (d, p) = (dir.clone(), req.package.clone());
        let kept = tokio::task::spawn_blocking(move || cache_get(&d, &p, now)).await.ok().flatten();
        let stale = kept.as_ref().and_then(|k| answer(&k.entry));
        let up_to_date = kept.as_ref().is_some_and(|k| k.fresh && !(options.retry_failed && k.entry == Entry::Failed));
        if up_to_date || !options.network {
            if let Some(entry) = stale {
                batch.done.push(Fetched { package: req.package.clone(), entry });
            }
        } else {
            to_ask.push((req.package.clone(), stale));
        }
    }
    if to_ask.is_empty() {
        return batch;
    }

    let gate = Arc::new(Semaphore::new(MAX_PARALLEL));
    let mut lookups = JoinSet::new();
    for (package, stale) in to_ask {
        let (store, gate) = (store.clone(), gate.clone());
        lookups.spawn(async move {
            let _permit = gate.acquire_owned().await.ok();
            let outcome = store.lookup(&package).await;
            (package, stale, outcome)
        });
    }

    let (mut answered, mut failures) = (0usize, Vec::new());
    while let Some(joined) = lookups.join_next().await {
        let Ok((package, stale, outcome)) = joined else { continue };
        let d = dir.clone();
        let p = package.clone();
        match outcome {
            Outcome::Found(cached) => {
                answered += 1;
                let stored = cached.clone();
                let _ = tokio::task::spawn_blocking(move || match &stored.icon {
                    Some(icon) => cache_put_found(&d, &p, icon, &stored.label),
                    None => Ok(()),
                })
                .await;
                batch.done.push(Fetched { package, entry: cached });
            }
            Outcome::Missing => {
                answered += 1;
                let _ = tokio::task::spawn_blocking(move || cache_put_missing(&d, &p)).await;
                batch.done.push(Fetched { package, entry: Cached { icon: None, label: String::new() } });
            }
            Outcome::Failed(why) => {
                tracing::debug!(%package, %why, "app icon lookup failed");
                failures.push(why);
                let _ = tokio::task::spawn_blocking(move || cache_put_failed(&d, &p)).await;
                if let Some(entry) = stale {
                    batch.done.push(Fetched { package, entry });
                }
            }
        }
    }
    if answered == 0 {
        batch.failure = failures.into_iter().next();
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn icon(rgba: [u8; 4]) -> Icon {
        decode_icon(&png(30, 30, rgba)).unwrap()
    }

    #[test]
    fn icons_of_any_size_become_square_and_fixed_size() {
        for (w, h) in [(48, 48), (192, 96), (512, 512)] {
            let icon = decode_icon(&png(w, h, [9, 8, 7, 255])).unwrap();
            assert_eq!(icon.rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
            assert_eq!(&icon.rgba[..4], &[9, 8, 7, 255]);
        }
        assert_eq!(decode_icon(&png(48, 48, [1, 2, 3, 255])).unwrap().resized(48).len(), 48 * 48 * 4);
    }

    #[test]
    fn corrupt_images_are_errors_not_panics() {
        assert!(decode_icon(b"not an image").is_err());
        assert!(decode_icon(b"\x89PNG but truncated").is_err());
        assert!(decode_icon(b"").is_err());
    }

    #[test]
    fn only_names_the_service_accepts_are_sent() {
        assert!(is_api_package("com.google.android.youtube"));
        assert!(is_api_package("a.b_c-d"));
        for bad in ["", "1abc", "../evil", "a b", "a/b", "a?b=c", "a\"b", "a;b", &"a".repeat(201)] {
            assert!(!is_api_package(bad), "{bad:?}");
        }
        assert!(is_api_package(&"a".repeat(200)));
    }

    #[test]
    fn icon_addresses_must_be_plain_https_on_googles_image_server() {
        assert!(is_icon_url("https://play-lh.googleusercontent.com/Fvled-zLfL8ER0EBNIk-FnunJCcH2u_T6rdITclOFdU2jpEopdMstZOHP-PtuhP_5coZ"));
        assert!(is_icon_url("https://play-lh.googleusercontent.com/abc=s128-rw"));
        for bad in [
            "http://play-lh.googleusercontent.com/abc",
            "https://evil.example/abc",
            "https://play-lh.googleusercontent.com.evil.example/abc",
            "https://play-lh.googleusercontent.com@evil.example/abc",
            "https://play-lh.googleusercontent.com/",
            "https://play-lh.googleusercontent.com/a b",
            "https://play-lh.googleusercontent.com/a\nHost: x",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!is_icon_url(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_listing_gives_title_and_icon_address() {
        let ok = br#"{"packageID":"com.dd.doordash","name":" DoorDash - Food Delivery ","logo":"https://play-lh.googleusercontent.com/abc123","rating":"4.6"}"#;
        assert_eq!(
            parse_listing(ok).unwrap(),
            Some(Listing { name: "DoorDash - Food Delivery".into(), logo: "https://play-lh.googleusercontent.com/abc123".into() })
        );
        assert_eq!(parse_listing(br#"{"name":"X"}"#).unwrap(), None, "no icon address");
        assert_eq!(parse_listing(br#"{"name":"X","logo":"https://evil.example/x"}"#).unwrap(), None, "an address that is not Google's");
        assert!(parse_listing(b"<html>Bad gateway</html>").is_err());
    }

    #[test]
    fn the_status_is_split_from_the_body_even_for_binary_bodies() {
        assert_eq!(split_reply(b"{\"a\":1}\n200"), Some((200, &b"{\"a\":1}"[..])));
        assert_eq!(split_reply(b"\x89PNG\r\n\x1a\n\x00\xff\n404"), Some((404, &b"\x89PNG\r\n\x1a\n\x00\xff"[..])));
        assert_eq!(split_reply(b"\n200"), Some((200, &b""[..])));
        assert_eq!(split_reply(b"no status"), None);
        assert_eq!(split_reply(b"body\nnot-a-number"), None);
    }

    #[test]
    fn the_cache_round_trips_icons_and_markers() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        assert!(cache_get(dir.path(), "a.b", now).is_none());

        cache_put_found(dir.path(), "a.b", &icon([9, 8, 7, 255]), "Äpp Bee").unwrap();
        let hit = cache_get(dir.path(), "a.b", now).unwrap();
        assert!(hit.fresh);
        assert_eq!(hit.entry, Entry::Found(Cached { icon: Some(icon([9, 8, 7, 255])), label: "Äpp Bee".into() }));

        cache_put_missing(dir.path(), "c.d").unwrap();
        assert_eq!(cache_get(dir.path(), "c.d", now), Some(Lookup { entry: Entry::Missing, fresh: true }));

        cache_put_failed(dir.path(), "e.f").unwrap();
        assert_eq!(cache_get(dir.path(), "e.f", now), Some(Lookup { entry: Entry::Failed, fresh: true }));
    }

    #[test]
    fn entries_age_out_at_their_own_pace() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        cache_put_found(dir.path(), "a.b", &icon([1, 2, 3, 255]), "A").unwrap();
        cache_put_missing(dir.path(), "c.d").unwrap();
        cache_put_failed(dir.path(), "e.f").unwrap();
        let later = |d: Duration| now + d;
        let fresh = |p: &str, t: SystemTime| cache_get(dir.path(), p, t).map(|l| l.fresh);

        assert_eq!(fresh("a.b", later(Duration::from_secs(29 * 24 * 3600))), Some(true));
        assert_eq!(fresh("a.b", later(Duration::from_secs(31 * 24 * 3600))), Some(false), "old, but still there to show");
        assert_eq!(fresh("c.d", later(Duration::from_secs(23 * 3600))), Some(true));
        assert_eq!(fresh("c.d", later(Duration::from_secs(25 * 3600))), Some(false));
        assert_eq!(fresh("e.f", later(Duration::from_secs(9 * 60))), Some(true));
        assert_eq!(fresh("e.f", later(Duration::from_secs(11 * 60))), None, "a failure is forgotten after ten minutes");
    }

    #[test]
    fn a_recent_failure_keeps_an_old_icon_without_asking_again() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        cache_put_found(dir.path(), "a.b", &icon([1, 2, 3, 255]), "A").unwrap();
        // Make the kept icon 40 days old.
        let label = std::fs::OpenOptions::new().write(true).open(dir.path().join("a.b.label")).unwrap();
        label.set_modified(now - Duration::from_secs(40 * 24 * 3600)).unwrap();
        assert_eq!(cache_get(dir.path(), "a.b", now).map(|l| l.fresh), Some(false), "old enough to refresh");

        cache_put_failed(dir.path(), "a.b").unwrap();
        let hit = cache_get(dir.path(), "a.b", now).unwrap();
        assert!(hit.fresh, "the refresh just failed, so it waits");
        assert!(matches!(hit.entry, Entry::Found(_)), "and the old icon is still shown");
    }

    #[test]
    fn a_new_answer_replaces_the_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        cache_put_found(dir.path(), "a.b", &icon([1, 2, 3, 255]), "A").unwrap();
        cache_put_missing(dir.path(), "a.b").unwrap();
        assert_eq!(cache_get(dir.path(), "a.b", now).map(|l| l.entry), Some(Entry::Missing));
        cache_put_found(dir.path(), "a.b", &icon([4, 5, 6, 255]), "B").unwrap();
        assert!(matches!(cache_get(dir.path(), "a.b", now).map(|l| l.entry), Some(Entry::Found(c)) if c.label == "B"));
    }

    #[test]
    fn an_entry_without_its_label_file_is_not_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        cache_put_found(dir.path(), "a.b", &icon([1, 2, 3, 255]), "x").unwrap();
        std::fs::remove_file(dir.path().join("a.b.label")).unwrap();
        assert!(cache_get(dir.path(), "a.b", SystemTime::now()).is_none(), "a half-written entry is looked up again");
    }
}

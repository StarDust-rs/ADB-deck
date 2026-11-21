//! Launcher icons and labels for the app list.
//!
//! Android has no command that returns an app's icon, so they come from the
//! ADBPlus helper running on the phone (see [`crate::aya`]): it renders each
//! icon with Android's own resource code, which also covers adaptive and vector
//! icons. Results are kept in a host cache keyed by package and version, so a
//! device is only asked about apps it has not been asked about before.

use std::path::Path;

use image::imageops::FilterType;
use tokio::sync::Mutex;

use crate::adb::apps::validate_package;
use crate::adb::DeviceAdb;
use crate::aya::Helper;
use crate::error::{Error, Result};

/// Edge length of the cached, resized icon.
pub const ICON_SIZE: u32 = 96;

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

/// What the disk cache knows about one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached {
    /// `None`: the app was inspected and has no icon.
    pub icon: Option<Icon>,
    /// The display name; empty when the app has none.
    pub label: String,
}

/// Looks `stem` up in the cache directory; `None` when it was never stored.
pub fn cache_get(dir: &Path, stem: &str) -> Option<Cached> {
    let label = std::fs::read_to_string(dir.join(format!("{stem}.label"))).ok()?;
    if dir.join(format!("{stem}.none")).is_file() {
        return Some(Cached { icon: None, label });
    }
    let img = image::open(dir.join(format!("{stem}.png"))).ok()?.to_rgba8();
    (img.width() == ICON_SIZE && img.height() == ICON_SIZE).then(|| Cached { icon: Some(Icon { rgba: img.into_raw() }), label })
}

/// Remembers the outcome for `stem`. Failing to write only costs a second lookup later.
pub fn cache_put(dir: &Path, stem: &str, entry: &Cached) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| io("creating the icon cache", e))?;
    match &entry.icon {
        Some(icon) => {
            let img = image::RgbaImage::from_raw(ICON_SIZE, ICON_SIZE, icon.rgba.clone())
                .ok_or_else(|| Error::parse("icon", "wrong pixel count"))?;
            // Written beside the final name and renamed, so a crash never leaves half a PNG.
            let tmp = dir.join(format!("{stem}.png.part"));
            img.save_with_format(&tmp, image::ImageFormat::Png).map_err(|e| Error::parse("icon", e.to_string()))?;
            std::fs::rename(&tmp, dir.join(format!("{stem}.png"))).map_err(|e| io("saving the icon", e))?;
        }
        None => std::fs::write(dir.join(format!("{stem}.none")), b"").map_err(|e| io("saving the icon marker", e))?,
    }
    // Last, because a lookup only counts when this file exists.
    std::fs::write(dir.join(format!("{stem}.label")), &entry.label).map_err(|e| io("saving the app name", e))
}

/// One app whose icon is wanted.
#[derive(Debug, Clone)]
pub struct Request {
    pub package: String,
    pub version_code: u64,
}

impl Request {
    fn stem(&self) -> Option<String> {
        validate_package(&self.package).ok().map(|p| format!("{p}-{}", self.version_code))
    }
}

/// One app that was resolved, from the cache or from the device.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub package: String,
    pub entry: Cached,
}

/// The helper of one device, started on first use.
#[derive(Default)]
pub enum HelperState {
    #[default]
    Idle,
    Ready(Helper),
    /// Starting failed; the text says why.
    Failed(String),
}

/// Result of looking up a batch of apps.
#[derive(Debug, Default)]
pub struct Batch {
    pub done: Vec<Fetched>,
    /// Why the helper could not be used, if that is why some apps are missing from `done`.
    pub failure: Option<String>,
}

/// Resolves `requests`: cache first, then one round trip to the helper for all
/// that are missing. Definite answers (including "no icon") are cached; apps
/// that could not be answered because of a device or transfer problem are
/// simply absent from the result, so a later run asks again.
pub async fn fetch_batch(helper: &Mutex<HelperState>, dev: &DeviceAdb, requests: &[Request], cache: &Path) -> Batch {
    let mut batch = Batch::default();
    let mut missing: Vec<(&Request, String)> = Vec::new();
    for req in requests {
        let Some(stem) = req.stem() else { continue };
        let (dir, key) = (cache.to_path_buf(), stem.clone());
        match tokio::task::spawn_blocking(move || cache_get(&dir, &key)).await.ok().flatten() {
            Some(entry) => batch.done.push(Fetched { package: req.package.clone(), entry }),
            None => missing.push((req, stem)),
        }
    }
    if missing.is_empty() {
        return batch;
    }

    let mut state = helper.lock().await;
    if matches!(*state, HelperState::Idle) {
        *state = match Helper::start(dev.clone(), &cache.join("helper")).await {
            Ok(h) => HelperState::Ready(h),
            Err(e) => HelperState::Failed(e.to_string()),
        };
    }
    let helper = match &mut *state {
        HelperState::Ready(h) => h,
        HelperState::Failed(why) => {
            batch.failure = Some(why.clone());
            return batch;
        }
        HelperState::Idle => return batch,
    };

    let names: Vec<String> = missing.iter().map(|(r, _)| r.package.clone()).collect();
    let infos = match helper.package_infos(&names).await {
        Ok(infos) => infos,
        Err(e) => {
            // The connection is suspect; the next batch starts from scratch.
            batch.failure = Some(e.to_string());
            if let HelperState::Ready(h) = std::mem::take(&mut *state) {
                h.shutdown().await;
            }
            return batch;
        }
    };
    let HelperState::Ready(helper) = &*state else { return batch };

    for (req, stem) in missing {
        let Some(info) = infos.iter().find(|i| i.package == req.package) else { continue };
        let label = if info.label == info.package { String::new() } else { info.label.clone() };
        let icon = if info.icon.is_empty() {
            None
        } else {
            match helper.read_icon(&info.icon).await {
                // An icon that cannot be decoded will not decode next time either.
                Ok(bytes) => decode_icon(&bytes).ok(),
                Err(_) => continue,
            }
        };
        let entry = Cached { icon, label };
        let (dir, key, stored) = (cache.to_path_buf(), stem, entry.clone());
        let _ = tokio::task::spawn_blocking(move || cache_put(&dir, &key, &stored)).await;
        batch.done.push(Fetched { package: req.package.clone(), entry });
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
    fn the_cache_round_trips_icons_and_negative_results() {
        let dir = tempfile::tempdir().unwrap();
        assert!(cache_get(dir.path(), "a.b-1").is_none());
        let icon = decode_icon(&png(30, 30, [9, 8, 7, 255])).unwrap();
        let with_icon = Cached { icon: Some(icon), label: "Äpp Bee".into() };
        let without = Cached { icon: None, label: String::new() };
        cache_put(dir.path(), "a.b-1", &with_icon).unwrap();
        cache_put(dir.path(), "c.d-2", &without).unwrap();
        assert_eq!(cache_get(dir.path(), "a.b-1"), Some(with_icon));
        assert_eq!(cache_get(dir.path(), "c.d-2"), Some(without));
        // A different version of the same app is a different entry.
        assert!(cache_get(dir.path(), "a.b-2").is_none());
    }

    #[test]
    fn an_entry_without_its_label_file_is_not_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        cache_put(dir.path(), "a.b-1", &Cached { icon: None, label: "x".into() }).unwrap();
        std::fs::remove_file(dir.path().join("a.b-1.label")).unwrap();
        assert!(cache_get(dir.path(), "a.b-1").is_none(), "a half-written entry is looked up again");
    }

    #[test]
    fn requests_for_invalid_package_names_have_no_cache_key() {
        assert!(Request { package: "../evil".into(), version_code: 1 }.stem().is_none());
        assert_eq!(Request { package: "a.b".into(), version_code: 7 }.stem().as_deref(), Some("a.b-7"));
    }
}

//! App icons and names for the Apps panel: looked up in batches from the Play Store
//! (see [`adbdeck::appicon`]), only for the rows that are on screen.

use std::collections::{HashMap, HashSet, VecDeque};

use egui::{ColorImage, Context, TextureHandle, TextureOptions, Ui};

use adbdeck::adb::apps::PackageInfo;
use adbdeck::appicon::{self, Batch, Options, PlayStore, Request};
use adbdeck::jobs::Slot;
use adbdeck::paths;

use super::Env;

/// Edge of the small variant used in list rows.
const SMALL: u32 = 48;
/// Apps asked about in one batch.
const BATCH: usize = 9;

struct Ready {
    large: TextureHandle,
    small: TextureHandle,
}

#[derive(Default)]
pub struct AppIcons {
    /// `None` means the app has no icon to show (letter badge instead).
    known: HashMap<String, Option<Ready>>,
    labels: HashMap<String, String>,
    queue: VecDeque<Request>,
    queued: HashSet<String>,
    store: PlayStore,
    job: Slot<(Vec<String>, Batch)>,
    failure: Option<String>,
    /// Whether the last batch was allowed to use the network (the setting).
    network: bool,
    /// Set by "Retry": the next batch asks again even about lookups that failed a moment ago.
    retry_failed: bool,
}

fn texture(ctx: &Context, name: &str, edge: u32, rgba: &[u8]) -> TextureHandle {
    let image = ColorImage::from_rgba_unmultiplied([edge as usize; 2], rgba);
    ctx.load_texture(format!("app-icon:{name}:{edge}"), image, TextureOptions::LINEAR)
}

impl AppIcons {
    /// Call once per frame before the rows ask for their icons.
    pub fn begin_frame(&mut self, ctx: &Context) {
        // Only what was asked for in the previous frame is still on screen.
        self.queue.clear();
        self.queued.clear();
        if let Some(Ok((asked, batch))) = self.job.poll() {
            for fetched in batch.done {
                let ready = fetched.entry.icon.map(|icon| Ready {
                    small: texture(ctx, &fetched.package, SMALL, &icon.resized(SMALL)),
                    large: texture(ctx, &fetched.package, appicon::ICON_SIZE, &icon.rgba),
                });
                if !fetched.entry.label.is_empty() {
                    self.labels.insert(fetched.package.clone(), fetched.entry.label);
                }
                self.known.insert(fetched.package, ready);
            }
            // Apps nobody could answer keep their letter badge until Reload.
            for name in asked {
                self.known.entry(name).or_insert(None);
            }
            if batch.failure.is_some() {
                tracing::warn!("app icons unavailable: {}", batch.failure.as_deref().unwrap_or_default());
                self.failure = batch.failure;
            }
        }
    }

    /// Notes that the icon of `pkg` is wanted this frame.
    pub fn want(&mut self, pkg: &PackageInfo) {
        if !self.known.contains_key(&pkg.name) && self.queued.insert(pkg.name.clone()) {
            self.queue.push_back(Request { package: pkg.name.clone() });
        }
    }

    /// Call once per frame after the rows: starts the next batch when idle.
    pub fn pump(&mut self, env: &Env) {
        let network = env.settings.fetch_app_icons;
        if network != self.network {
            // Switching the setting on or off changes what the apps without an icon can get.
            self.network = network;
            self.known.retain(|_, v| v.is_some());
        }
        if self.job.busy() || self.queue.is_empty() {
            return;
        }
        let Some(dir) = paths::icon_cache_dir() else {
            self.known.extend(self.queue.drain(..).map(|r| (r.package, None)));
            return;
        };
        let requests: Vec<Request> = self.queue.drain(..BATCH.min(self.queue.len())).collect();
        let asked: Vec<String> = requests.iter().map(|r| r.package.clone()).collect();
        let options = Options { network, retry_failed: std::mem::take(&mut self.retry_failed) };
        let store = self.store.clone();
        self.job.set(env.sp.spawn(async move {
            let batch = appicon::fetch_batch(&store, &requests, &dir, options).await;
            Ok((asked, batch))
        }));
    }

    /// Lets apps whose lookup failed be tried again (after a reload or "Retry").
    pub fn retry_missing(&mut self) {
        self.known.retain(|_, v| v.is_some());
        self.failure = None;
        self.retry_failed = true;
    }

    /// The app's store title, when there is one.
    pub fn label(&self, name: &str) -> Option<&str> {
        self.labels.get(name).map(String::as_str)
    }

    /// Why icons are unavailable, if they are.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Draws the icon at `size` and returns `true`, or draws nothing and returns `false`.
    pub fn show(&self, ui: &mut Ui, name: &str, size: f32) -> bool {
        let Some(Some(ready)) = self.known.get(name) else { return false };
        let tex = if size > SMALL as f32 / 1.5 { &ready.large } else { &ready.small };
        ui.add(egui::Image::new(egui::load::SizedTexture::new(tex.id(), egui::vec2(size, size))).corner_radius(size / 4.5));
        true
    }
}

//! The Permissions and Advanced tabs of the app details panel.
//!
//! The tabs only draw and report what the user asked for as a [`Request`];
//! [`Request::run`] is the one place that turns a request into device commands,
//! so the panel code stays free of async plumbing.

use egui::{RichText, Ui};

use adbdeck::adb::apps::{PermKind, Permission};
use adbdeck::adb::manage::{AppDetails, Bucket, OpMode, ADB_ONLY_PERMISSIONS, SPECIAL_OPS};
use adbdeck::adb::DeviceAdb;
use adbdeck::error::Result;
use adbdeck::text::contains_ci;

use super::{kit, theme};

/// What the user asked to change on the selected app.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Permission {
        name: String,
        grant: bool,
    },
    AllPermissions {
        names: Vec<String>,
        grant: bool,
    },
    Op {
        op: &'static str,
        label: &'static str,
        mode: OpMode,
    },
    Bucket(Bucket),
    Battery(bool),
    Suspend(bool),
    Hide(bool),
    Compile(Option<&'static str>),
    Kill,
    Settings,
    ClearCache,
    /// Needs the user's confirmation before it is run.
    RemoveForUser,
}

impl Request {
    /// True when the app list (sizes, enabled state) may look different afterwards.
    pub fn changes_list(&self) -> bool {
        matches!(self, Request::ClearCache | Request::RemoveForUser | Request::Suspend(_) | Request::Hide(_) | Request::Compile(_))
    }

    /// Carries the request out; the result is the message to show.
    pub async fn run(self, dev: DeviceAdb, pkg: String) -> Result<String> {
        match self {
            Request::Permission { name, grant } => {
                dev.set_permission(&pkg, &name, grant).await?;
                Ok(format!("{} {}", pretty(&name), if grant { "granted" } else { "revoked" }))
            }
            Request::AllPermissions { names, grant } => {
                let (mut done, mut first_error) = (0usize, None);
                for name in &names {
                    match dev.set_permission(&pkg, name, grant).await {
                        Ok(()) => done += 1,
                        Err(e) => first_error = first_error.or(Some(e)),
                    }
                }
                match first_error {
                    Some(e) if done == 0 => Err(e),
                    Some(_) => Ok(format!("{} {done} of {} permissions", if grant { "Granted" } else { "Revoked" }, names.len())),
                    None => Ok(format!("{} {done} permissions", if grant { "Granted" } else { "Revoked" })),
                }
            }
            Request::Op { op, label, mode } => {
                dev.set_app_op(&pkg, op, &mode).await?;
                Ok(format!("{label}: {}", mode.label().to_lowercase()))
            }
            Request::Bucket(b) => {
                dev.set_standby_bucket(&pkg, b).await?;
                Ok(format!("Standby bucket set to {}", b.label().to_lowercase()))
            }
            Request::Battery(on) => {
                dev.set_battery_exempt(&pkg, on).await?;
                Ok(if on { "Battery optimisation turned off for this app" } else { "Battery optimisation turned back on" }.into())
            }
            Request::Suspend(on) => {
                dev.set_suspended(&pkg, on).await?;
                Ok(if on { "App suspended" } else { "App resumed" }.into())
            }
            Request::Hide(on) => {
                dev.set_hidden(&pkg, on).await?;
                Ok(if on { "App hidden" } else { "App visible again" }.into())
            }
            Request::Compile(filter) => {
                dev.compile(&pkg, filter).await?;
                Ok(filter.map_or("Compiler filter reset".to_string(), |f| format!("Recompiled with {f}")))
            }
            Request::Kill => {
                dev.kill_background(&pkg).await?;
                Ok("Background processes stopped".into())
            }
            Request::Settings => {
                dev.open_app_settings(&pkg).await?;
                Ok("Opened the app's settings on the phone".into())
            }
            Request::ClearCache => {
                let c = dev.clear_app_cache(&pkg).await?;
                Ok(match (c.internal, c.external) {
                    (true, true) => "Cache cleared".into(),
                    (false, true) => "Cleared the shared-storage cache (the private cache needs Android 14 or a debuggable app)".into(),
                    _ => "Cleared the private cache".into(),
                })
            }
            Request::RemoveForUser => {
                dev.remove_for_user(&pkg).await?;
                Ok(format!("Removed {pkg} for this user; use Restore to bring it back"))
            }
        }
    }
}

/// `android.permission.READ_CONTACTS` → "Read contacts".
pub fn pretty(permission: &str) -> String {
    let short = permission.rsplit('.').next().unwrap_or(permission);
    let mut s = short.replace('_', " ").to_lowercase();
    if let Some(first) = s.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    s
}

fn adb_only(name: &str) -> Option<&'static str> {
    ADB_ONLY_PERMISSIONS.iter().find(|(n, _)| *n == name).map(|(_, hint)| *hint)
}

fn section(ui: &mut Ui, title: &str, detail: &str) {
    let p = theme::pal(ui);
    ui.add_space(8.0);
    ui.label(kit::semibold(title).size(13.5));
    if !detail.is_empty() {
        ui.add(egui::Label::new(RichText::new(detail).size(12.0).color(p.text3)).wrap());
    }
    ui.add_space(4.0);
}

// ---- permissions ------------------------------------------------------------------------

pub struct PermissionsTab<'a> {
    pub details: &'a AppDetails,
    pub filter: &'a mut String,
    pub busy: bool,
}

impl PermissionsTab<'_> {
    pub fn show(self, ui: &mut Ui) -> Option<Request> {
        let Self { details, filter, busy } = self;
        let p = theme::pal(ui);
        let mut request = None;
        let width = ui.available_width();
        kit::search(ui, filter, "Filter permissions", width);
        let needle = filter.as_str();
        let shown = |perm: &&Permission| contains_ci(&perm.name, needle) || contains_ci(&pretty(&perm.name), needle);

        let adjustable: Vec<&Permission> =
            details.permissions.iter().filter(|perm| perm.kind == PermKind::Runtime || adb_only(&perm.name).is_some()).collect();
        let fixed: Vec<&Permission> = details.permissions.iter().filter(|perm| !adjustable.contains(perm)).collect();

        section(ui, "Permissions you can change", "Granting or revoking applies at once; the app may need a restart to notice.");
        let visible: Vec<&Permission> = adjustable.iter().copied().filter(shown).collect();
        if adjustable.is_empty() {
            ui.label(RichText::new("This app asks for no changeable permissions.").color(p.text3));
        } else {
            ui.horizontal(|ui| {
                let grantable: Vec<String> = visible.iter().filter(|x| x.granted != Some(true)).map(|x| x.name.clone()).collect();
                let revocable: Vec<String> = visible.iter().filter(|x| x.granted == Some(true)).map(|x| x.name.clone()).collect();
                if kit::Btn::new("Grant all").icon(kit::ph::CHECK_CIRCLE).enabled(!busy && !grantable.is_empty()).show(ui).clicked() {
                    request = Some(Request::AllPermissions { names: grantable, grant: true });
                }
                if kit::Btn::new("Revoke all").icon(kit::ph::PROHIBIT).enabled(!busy && !revocable.is_empty()).show(ui).clicked() {
                    request = Some(Request::AllPermissions { names: revocable, grant: false });
                }
            });
            ui.add_space(4.0);
        }
        for perm in visible {
            let mut on = perm.granted == Some(true);
            let hint = adb_only(&perm.name).map_or_else(|| perm.name.clone(), |h| format!("{h}\n{}", perm.name));
            kit::setting_row(ui, &pretty(&perm.name), &hint, |ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    if kit::toggle(ui, &mut on, "").changed() {
                        request = Some(Request::Permission { name: perm.name.clone(), grant: on });
                    }
                });
            });
        }

        section(ui, "Special access", "Switches Android keeps on separate settings screens.");
        for (op, label, hint) in SPECIAL_OPS {
            if !contains_ci(label, needle) && !contains_ci(op, needle) {
                continue;
            }
            let current = details.op_mode(op);
            let mut chosen = current.clone();
            kit::setting_row(ui, label, hint, |ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    egui::ComboBox::from_id_salt(("op", *op)).width(112.0).selected_text(current.label()).show_ui(ui, |ui| {
                        for mode in [OpMode::Allow, OpMode::Ignore, OpMode::Deny, OpMode::Default] {
                            ui.selectable_value(&mut chosen, mode.clone(), mode.label());
                        }
                    });
                });
            });
            if chosen != current {
                request = Some(Request::Op { op, label, mode: chosen });
            }
        }

        let fixed: Vec<&Permission> = fixed.into_iter().filter(shown).collect();
        if !fixed.is_empty() {
            ui.add_space(8.0);
            egui::CollapsingHeader::new(kit::semibold(format!("Fixed at install time ({})", fixed.len())).size(13.0))
                .id_salt("fixed-perms")
                .show(ui, |ui| {
                    for perm in fixed {
                        ui.horizontal(|ui| {
                            match perm.granted {
                                Some(true) => kit::dot(ui, kit::Kind::Success),
                                Some(false) => kit::dot(ui, kit::Kind::Neutral),
                                None => {
                                    ui.add_space(8.0);
                                }
                            }
                            ui.add(egui::Label::new(RichText::new(&perm.name).monospace().size(11.0).color(p.text2)).truncate())
                                .on_hover_text(&perm.name);
                        });
                    }
                });
        }
        request
    }
}

// ---- advanced ---------------------------------------------------------------------------

pub struct AdvancedTab<'a> {
    pub details: &'a AppDetails,
    pub cache_size: String,
    pub busy: bool,
}

impl AdvancedTab<'_> {
    pub fn show(self, ui: &mut Ui) -> Option<Request> {
        let Self { details, cache_size, busy } = self;
        let p = theme::pal(ui);
        let mut request = None;
        let w = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).floor();

        section(ui, "Storage", "");
        kit::setting_row(ui, &format!("Cache {cache_size}"), "Temporary files the app can rebuild", |ui| {
            if kit::Btn::new("Clear cache").icon(kit::ph::BROOM).enabled(!busy).show(ui).clicked() {
                request = Some(Request::ClearCache);
            }
        });

        section(ui, "Background activity", "Limit what the app may do while you are not using it.");
        let mut bucket = details.bucket;
        kit::setting_row(ui, "Standby bucket", "How often Android lets the app run jobs and alarms", |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                egui::ComboBox::from_id_salt("bucket").width(112.0).selected_text(bucket.map_or("Unknown", Bucket::label)).show_ui(
                    ui,
                    |ui| {
                        for b in Bucket::ALL {
                            ui.selectable_value(&mut bucket, Some(b), b.label());
                        }
                    },
                );
            });
        });
        if bucket != details.bucket {
            request = bucket.map(Request::Bucket);
        }
        let mut exempt = details.battery_exempt;
        kit::setting_row(ui, "Ignore battery optimisation", "Lets the app run in the background without Doze limits", |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if kit::toggle(ui, &mut exempt, "").changed() {
                    request = Some(Request::Battery(exempt));
                }
            });
        });
        ui.horizontal(|ui| {
            if kit::Btn::new("Stop")
                .icon(kit::ph::STOP)
                .min_width(w)
                .enabled(!busy)
                .tip("Stops the app's cached background processes")
                .show(ui)
                .clicked()
            {
                request = Some(Request::Kill);
            }
            if kit::Btn::new("Settings")
                .icon(kit::ph::GEAR_SIX)
                .min_width(w)
                .enabled(!busy)
                .tip("Opens the app's page in Android settings")
                .show(ui)
                .clicked()
            {
                request = Some(Request::Settings);
            }
        });

        section(ui, "Availability", "");
        let mut suspended = details.state.suspended;
        kit::setting_row(ui, "Suspended", "Greys out the icon and blocks launching until resumed", |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if kit::toggle(ui, &mut suspended, "").changed() {
                    request = Some(Request::Suspend(suspended));
                }
            });
        });
        let mut hidden = details.state.hidden;
        kit::setting_row(ui, "Hidden", "Behaves as if uninstalled but keeps the data", |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if kit::toggle(ui, &mut hidden, "").changed() {
                    request = Some(Request::Hide(hidden));
                }
            });
        });

        section(ui, "Performance", "Recompile the app ahead of time for faster starts.");
        ui.horizontal_wrapped(|ui| {
            for (label, filter, tip) in [
                ("speed-profile", Some("speed-profile"), "Android's usual choice; compiles what the app actually uses"),
                ("speed", Some("speed"), "Compiles everything; fastest, uses more storage"),
                ("Reset", None, "Back to the system default"),
            ] {
                if kit::Btn::new(label).enabled(!busy).tip(tip).show(ui).clicked() {
                    request = Some(Request::Compile(filter));
                }
            }
        });

        section(ui, "Remove", "");
        ui.add(
            egui::Label::new(
                RichText::new("Removes the app for you but keeps its data, and system apps stay on the phone. Bring it back with Restore in the toolbar.")
                    .size(12.0)
                    .color(p.text3),
            )
            .wrap(),
        );
        ui.add_space(4.0);
        if kit::Btn::new("Remove for this user").icon(kit::ph::TRASH).danger().enabled(!busy).show(ui).clicked() {
            request = Some(Request::RemoveForUser);
        }
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_read_like_settings_labels() {
        assert_eq!(pretty("android.permission.READ_CONTACTS"), "Read contacts");
        assert_eq!(pretty("CAMERA"), "Camera");
        assert_eq!(pretty(""), "");
    }

    #[test]
    fn only_list_changing_requests_ask_for_a_reload() {
        assert!(Request::ClearCache.changes_list());
        assert!(Request::Suspend(true).changes_list());
        assert!(!Request::Permission { name: "x".into(), grant: true }.changes_list());
        assert!(!Request::Kill.changes_list());
    }
}

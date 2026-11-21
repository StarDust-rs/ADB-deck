//! APK signing with saved keystore profiles.

use std::path::PathBuf;

use egui::{RichText, Ui};

use adbdeck::jobs::Slot;
use adbdeck::signing::{self, SignRequest};
use adbdeck::store::{SigningProfile, SigningScheme};
use adbdeck::tools::Tool;

use super::widgets::{self, Dialog};
use super::{kit, theme, Env};

#[derive(Clone, Copy)]
enum Pick {
    Apk,
    Keystore,
    Output,
    VerifyApk,
}

pub struct Signing {
    selected: Option<String>,
    input: String,
    output: String,
    keystore: String,
    alias: String,
    scheme: SigningScheme,
    store_pass: String,
    key_pass: String,
    profile_name: String,
    save_password: bool,
    dialog: Slot<Vec<PathBuf>>,
    pick: Option<Pick>,
    job: Slot<String>,
    verify_job: Slot<String>,
    verify_text: Option<String>,
    secrets: Slot<Option<(String, String)>>,
    secret_write: Slot<()>,
}

impl Default for Signing {
    fn default() -> Self {
        Self {
            selected: None,
            input: String::new(),
            output: String::new(),
            keystore: String::new(),
            alias: String::new(),
            scheme: SigningScheme::V1V2,
            store_pass: String::new(),
            key_pass: String::new(),
            profile_name: String::new(),
            save_password: false,
            dialog: Slot::default(),
            pick: None,
            job: Slot::default(),
            verify_job: Slot::default(),
            verify_text: None,
            secrets: Slot::default(),
            secret_write: Slot::default(),
        }
    }
}

impl Signing {
    fn browse(&mut self, env: &Env, pick: Pick) {
        self.pick = Some(pick);
        let dialog = match pick {
            Pick::Apk | Pick::VerifyApk => {
                Dialog::OpenFile { title: "Choose an APK".into(), filter: Some(("Android packages".into(), vec!["apk".into()])) }
            }
            Pick::Keystore => Dialog::OpenFile { title: "Choose a keystore".into(), filter: None },
            Pick::Output => Dialog::Save {
                title: "Save signed APK".into(),
                name: signing::default_output(std::path::Path::new(&self.input))
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "signed.apk".into()),
            },
        };
        self.dialog.set(widgets::dialog(env.sp, dialog));
    }

    fn apply_profile(&mut self, p: &SigningProfile, env: &Env) {
        self.selected = Some(p.id.clone());
        self.keystore = p.keystore_path.clone();
        self.alias = p.key_alias.clone();
        self.scheme = p.scheme;
        self.profile_name = p.name.clone();
        self.store_pass.clear();
        self.key_pass.clear();
        let id = p.id.clone();
        self.secrets.set(env.sp.spawn(async move { signing::load_passwords(id).await }));
    }

    pub fn ui(&mut self, ui: &mut Ui, env: &mut Env) {
        let Some(apksigner) = env.tools.apksigner.clone() else {
            kit::tool_missing(
                ui,
                kit::ph::SEAL_CHECK,
                "apksigner was not found",
                "apksigner is part of the Android SDK build-tools and needs a Java runtime.",
                Tool::Apksigner.install_hint(),
            );
            return;
        };
        if let Some(r) = self.dialog.poll() {
            if let (Some(path), Some(pick)) = (env.notes.report("File dialog", r).and_then(|p| p.into_iter().next()), self.pick.take()) {
                let s = path.to_string_lossy().into_owned();
                match pick {
                    Pick::Apk => {
                        self.output = signing::default_output(&path).to_string_lossy().into_owned();
                        self.input = s;
                    }
                    Pick::Keystore => self.keystore = s,
                    Pick::Output => self.output = s,
                    Pick::VerifyApk => {
                        let a = apksigner.clone();
                        self.verify_text = None;
                        self.verify_job.set(env.sp.spawn(async move { signing::verify(&a, &path).await }));
                    }
                }
            }
        }
        if let Some(r) = self.job.poll() {
            if let Some(m) = env.notes.report("Signing", r) {
                env.notes.info(m);
            }
        }
        if let Some(r) = self.verify_job.poll() {
            self.verify_text = Some(match r {
                Ok(t) => t,
                Err(e) => format!("Verification failed: {e}"),
            });
        }
        if let Some(r) = self.secrets.poll() {
            match r {
                Ok(Some((s, k))) => {
                    self.store_pass = s;
                    self.key_pass = k;
                    self.save_password = true;
                }
                Ok(None) => self.save_password = false,
                Err(e) => env.notes.error("Secret service", &e),
            }
        }
        if let Some(r) = self.secret_write.poll() {
            env.notes.report("Secret service", r);
        }

        let p = theme::pal(ui);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            kit::card_titled(ui, "Signing profiles", |ui| {
                let profiles = env.data.signing_profiles.clone();
                if profiles.is_empty() {
                    ui.label(RichText::new("No saved profiles yet. Fill in the form below and save it as a profile.").color(p.text3));
                }
                ui.horizontal_wrapped(|ui| {
                    for pr in &profiles {
                        let on = self.selected.as_deref() == Some(pr.id.as_str());
                        if kit::Btn::new(&pr.name).icon(kit::ph::KEY).selected(on).show(ui).clicked() {
                            self.apply_profile(pr, env);
                        }
                    }
                });
            });
            ui.add_space(12.0);

            kit::card_titled(ui, "Sign an APK", |ui| {
                let browse = |ui: &mut Ui| kit::Btn::new("").icon(kit::ph::FOLDER_OPEN).tip("Browse…").show(ui).clicked();
                egui::Grid::new("sign-form").num_columns(3).min_col_width(120.0).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label(RichText::new("APK to sign").color(p.text2));
                    kit::text_field(ui, &mut self.input, "app.apk", 420.0);
                    if browse(ui) && !self.dialog.busy() {
                        self.browse(env, Pick::Apk);
                    }
                    ui.end_row();
                    ui.label(RichText::new("Signed APK").color(p.text2));
                    kit::text_field(ui, &mut self.output, "app-signed.apk", 420.0);
                    if browse(ui) && !self.dialog.busy() {
                        self.browse(env, Pick::Output);
                    }
                    ui.end_row();
                    ui.label(RichText::new("Keystore").color(p.text2));
                    kit::text_field(ui, &mut self.keystore, "release.jks", 420.0);
                    if browse(ui) && !self.dialog.busy() {
                        self.browse(env, Pick::Keystore);
                    }
                    ui.end_row();
                    ui.label(RichText::new("Key alias").color(p.text2));
                    kit::text_field(ui, &mut self.alias, "alias", 200.0);
                    ui.end_row();
                    ui.label(RichText::new("Keystore password").color(p.text2));
                    kit::password_field(ui, &mut self.store_pass, "", 200.0);
                    ui.end_row();
                    ui.label(RichText::new("Key password").color(p.text2));
                    kit::password_field(ui, &mut self.key_pass, "same as keystore", 200.0);
                    ui.end_row();
                    ui.label(RichText::new("Signature scheme").color(p.text2));
                    let options: Vec<(SigningScheme, &str)> = SigningScheme::ALL.iter().map(|s| (*s, s.label())).collect();
                    kit::segmented(ui, &mut self.scheme, &options);
                    ui.end_row();
                });
                ui.add_space(10.0);
                let ready = !self.input.trim().is_empty()
                    && !self.output.trim().is_empty()
                    && !self.keystore.trim().is_empty()
                    && !self.alias.trim().is_empty();
                ui.horizontal(|ui| {
                    if kit::Btn::new("Sign APK").icon(kit::ph::SEAL_CHECK).primary().enabled(ready && !self.job.busy()).show(ui).clicked() {
                        let (a, input, output, ks, alias) = (
                            apksigner.clone(),
                            PathBuf::from(&self.input),
                            PathBuf::from(&self.output),
                            PathBuf::from(&self.keystore),
                            self.alias.clone(),
                        );
                        let (sp, kp, scheme) = (self.store_pass.clone(), self.key_pass.clone(), self.scheme);
                        self.job.set(env.sp.spawn(async move {
                            let req = SignRequest {
                                input: &input,
                                output: &output,
                                keystore: &ks,
                                alias: &alias,
                                store_password: &sp,
                                key_password: &kp,
                                scheme,
                            };
                            signing::sign(&a, &req).await?;
                            Ok(format!("Signed {}", output.display()))
                        }));
                    }
                    if self.job.busy() {
                        ui.add(egui::Spinner::new().size(16.0));
                    }
                });
                ui.add_space(10.0);
                kit::hsep(ui);
                ui.add_space(8.0);
                ui.label(kit::semibold("Save this setup as a profile").size(13.0));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    kit::text_field(ui, &mut self.profile_name, "Profile name", 200.0);
                    kit::toggle(ui, &mut self.save_password, "Store passwords in the secret service");
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if kit::Btn::new("Save profile").icon(kit::ph::FLOPPY_DISK).enabled(!self.profile_name.trim().is_empty() && !self.keystore.is_empty()).show(ui).clicked() {
                        let existing = self
                            .selected
                            .clone()
                            .filter(|id| env.data.signing_profiles.iter().any(|p| &p.id == id && p.name == self.profile_name.trim()));
                        let id = existing.unwrap_or_else(|| {
                            format!("p{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0))
                        });
                        let profile = SigningProfile {
                            id: id.clone(),
                            name: self.profile_name.trim().to_string(),
                            keystore_path: self.keystore.clone(),
                            key_alias: self.alias.clone(),
                            scheme: self.scheme,
                        };
                        match env.data.signing_profiles.iter_mut().find(|p| p.id == id) {
                            Some(p) => *p = profile,
                            None => env.data.signing_profiles.push(profile),
                        }
                        *env.data_dirty = true;
                        self.selected = Some(id.clone());
                        let (s, k, save) = (self.store_pass.clone(), self.key_pass.clone(), self.save_password);
                        self.secret_write.set(env.sp.spawn(async move {
                            if save {
                                signing::store_passwords(id, s, k).await
                            } else {
                                signing::delete_passwords(id).await
                            }
                        }));
                    }
                    if kit::Btn::new("Delete profile").icon(kit::ph::TRASH).danger().enabled(self.selected.is_some()).show(ui).clicked() {
                        if let Some(id) = self.selected.take() {
                            env.data.signing_profiles.retain(|p| p.id != id);
                            *env.data_dirty = true;
                            self.secret_write.set(env.sp.spawn(async move { signing::delete_passwords(id).await }));
                        }
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Passwords are only kept if you switch this on, and then only in the secret service (KWallet or GNOME Keyring), never in ADB Deck's files.")
                        .size(12.0)
                        .color(p.text3),
                );
            });
            ui.add_space(12.0);

            kit::card_titled(ui, "Verify an APK", |ui| {
                if kit::Btn::new("Choose APK to verify").icon(kit::ph::FOLDER_OPEN).enabled(!self.verify_job.busy() && !self.dialog.busy()).show(ui).clicked() {
                    self.browse(env, Pick::VerifyApk);
                }
                if self.verify_job.busy() {
                    ui.add_space(6.0);
                    kit::busy(ui, "Verifying…");
                }
                if let Some(t) = &self.verify_text {
                    ui.add_space(8.0);
                    kit::code_block(ui, t);
                }
            });
        });
    }
}

//! APK signing and verification with the Android SDK's `apksigner`, plus
//! optional password storage in the desktop secret service (Secret Service
//! API: KWallet or GNOME Keyring).
//!
//! Passwords reach apksigner through environment variables (`env:NAME`), so
//! they never show up in the process list.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::process::{run, CommandSpec};
use crate::store::SigningScheme;

const KS_PASS: &str = "ADBDECK_KS_PASS";
const KEY_PASS: &str = "ADBDECK_KEY_PASS";
const SERVICE: &str = "adbdeck";

pub struct SignRequest<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub keystore: &'a Path,
    pub alias: &'a str,
    pub store_password: &'a str,
    /// Empty means "same as the keystore password".
    pub key_password: &'a str,
    pub scheme: SigningScheme,
}

fn require_file(path: &Path, what: &str) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(Error::Invalid(format!("{what} not found: {}", path.display())))
    }
}

/// apksigner arguments (without secrets). Pure, for unit tests.
pub fn sign_args(req: &SignRequest) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = vec!["sign".into()];
    a.push("--ks".into());
    a.push(req.keystore.into());
    a.push("--ks-key-alias".into());
    a.push(req.alias.into());
    a.push("--ks-pass".into());
    a.push(format!("env:{KS_PASS}").into());
    if !req.key_password.is_empty() {
        a.push("--key-pass".into());
        a.push(format!("env:{KEY_PASS}").into());
    }
    a.push(format!("--v1-signing-enabled={}", req.scheme.v1()).into());
    a.push(format!("--v2-signing-enabled={}", req.scheme.v2()).into());
    a.push("--out".into());
    a.push(req.output.into());
    a.push(req.input.into());
    a
}

pub async fn sign(apksigner: &Path, req: &SignRequest<'_>) -> Result<()> {
    require_file(req.input, "APK")?;
    require_file(req.keystore, "Keystore")?;
    if req.alias.trim().is_empty() {
        return Err(Error::Invalid("Enter the key alias".into()));
    }
    if req.input == req.output {
        return Err(Error::Invalid("Choose a different output file than the input APK".into()));
    }
    let spec = CommandSpec::new(apksigner)
        .args(sign_args(req))
        .env(KS_PASS, req.store_password)
        .env(KEY_PASS, req.key_password)
        .timeout(Duration::from_secs(120));
    let out = run(&spec).await?;
    if out.success() {
        Ok(())
    } else {
        // apksigner never echoes the passwords; its message is safe to show.
        Err(Error::Exit { program: "apksigner".into(), code: out.code, message: out.failure_message() })
    }
}

pub async fn verify(apksigner: &Path, apk: &Path) -> Result<String> {
    require_file(apk, "APK")?;
    let spec = CommandSpec::new(apksigner).args(["verify", "--verbose", "--print-certs"]).arg(apk).timeout(Duration::from_secs(60));
    let out = run(&spec).await?;
    let text = format!("{}{}", out.stdout_text(), out.stderr);
    if out.success() {
        Ok(text.trim().to_string())
    } else {
        Err(Error::Exit { program: "apksigner".into(), code: out.code, message: text.trim().to_string() })
    }
}

/// `app-release.apk` -> `app-release-signed.apk`, next to the input.
pub fn default_output(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("app");
    input.with_file_name(format!("{stem}-signed.apk"))
}

// ---- secret storage ----------------------------------------------------

fn keyring_error(e: keyring::Error) -> Error {
    Error::Invalid(format!("The desktop secret service is not available ({e}). Passwords are not stored; enter them each time."))
}

fn entry(profile_id: &str, which: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, &format!("{profile_id}:{which}")).map_err(keyring_error)
}

/// Stores the passwords of a profile. Runs on a blocking thread because the
/// secret service client is synchronous.
pub async fn store_passwords(profile_id: String, store_pass: String, key_pass: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        entry(&profile_id, "store")?.set_password(&store_pass).map_err(keyring_error)?;
        entry(&profile_id, "key")?.set_password(&key_pass).map_err(keyring_error)
    })
    .await
    .map_err(|_| Error::Cancelled)?
}

/// `(keystore password, key password)`, or `None` if nothing is stored.
pub async fn load_passwords(profile_id: String) -> Result<Option<(String, String)>> {
    tokio::task::spawn_blocking(move || {
        let get = |which: &str| -> Result<Option<String>> {
            match entry(&profile_id, which)?.get_password() {
                Ok(p) => Ok(Some(p)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(keyring_error(e)),
            }
        };
        Ok(get("store")?.map(|s| (s, get("key").ok().flatten().unwrap_or_default())))
    })
    .await
    .map_err(|_| Error::Cancelled)?
}

pub async fn delete_passwords(profile_id: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        for which in ["store", "key"] {
            match entry(&profile_id, which)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(keyring_error(e)),
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| Error::Cancelled)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::testutil::write_exe;

    fn req<'a>(dir: &'a Path, apk: &'a Path, ks: &'a Path, out: &'a Path) -> SignRequest<'a> {
        let _ = dir;
        SignRequest {
            input: apk,
            output: out,
            keystore: ks,
            alias: "key0",
            store_password: "s3cret",
            key_password: "",
            scheme: SigningScheme::V1V2,
        }
    }

    #[test]
    fn passwords_never_appear_in_arguments() {
        let (a, k, o) = (PathBuf::from("a.apk"), PathBuf::from("k.jks"), PathBuf::from("o.apk"));
        let mut r = req(Path::new("."), &a, &k, &o);
        r.key_password = "other";
        let args: Vec<String> = sign_args(&r).iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert!(!args.iter().any(|x| x.contains("s3cret") || x.contains("other")));
        assert!(args.contains(&format!("env:{KS_PASS}")));
        assert!(args.contains(&format!("env:{KEY_PASS}")));
        assert!(args.contains(&"--v1-signing-enabled=true".to_string()));
        assert_eq!(args.last().unwrap(), "a.apk");
    }

    #[test]
    fn schemes_map_to_flags() {
        let (a, k, o) = (PathBuf::from("a"), PathBuf::from("k"), PathBuf::from("o"));
        let mut r = req(Path::new("."), &a, &k, &o);
        r.scheme = SigningScheme::V2;
        let args: Vec<String> = sign_args(&r).iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&"--v1-signing-enabled=false".to_string()));
        assert!(args.contains(&"--v2-signing-enabled=true".to_string()));
    }

    #[test]
    fn default_output_sits_next_to_the_input() {
        assert_eq!(default_output(Path::new("/x/app-release.apk")), PathBuf::from("/x/app-release-signed.apk"));
    }

    #[tokio::test]
    async fn signing_passes_secrets_through_the_environment() {
        let d = tempfile::tempdir().unwrap();
        let tool = write_exe(
            &d.path().join("apksigner"),
            "[ \"$ADBDECK_KS_PASS\" = s3cret ] || { echo 'bad password' >&2; exit 2; }\nfor last; do :; done\ncp \"$last\" \"$OUT\" 2>/dev/null\nexit 0\n",
        );
        let (apk, ks, out) = (d.path().join("a.apk"), d.path().join("k.jks"), d.path().join("a-signed.apk"));
        std::fs::write(&apk, b"apk").unwrap();
        std::fs::write(&ks, b"ks").unwrap();
        sign(&tool, &req(d.path(), &apk, &ks, &out)).await.unwrap();
    }

    #[tokio::test]
    async fn tool_failure_is_reported_with_its_message() {
        let d = tempfile::tempdir().unwrap();
        let tool = write_exe(&d.path().join("apksigner"), "echo 'Failed to load signer' >&2\nexit 1\n");
        let (apk, ks, out) = (d.path().join("a.apk"), d.path().join("k.jks"), d.path().join("o.apk"));
        std::fs::write(&apk, b"apk").unwrap();
        std::fs::write(&ks, b"ks").unwrap();
        let err = sign(&tool, &req(d.path(), &apk, &ks, &out)).await.unwrap_err();
        assert!(err.to_string().contains("Failed to load signer"), "{err}");
    }

    #[tokio::test]
    async fn missing_inputs_are_checked_before_spawning() {
        let d = tempfile::tempdir().unwrap();
        let (apk, ks, out) = (d.path().join("none.apk"), d.path().join("k.jks"), d.path().join("o.apk"));
        let err = sign(Path::new("/nonexistent"), &req(d.path(), &apk, &ks, &out)).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)));
    }
}

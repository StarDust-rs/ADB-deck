//! Hand-off to the desktop: open files, folders and links with `xdg-open`.

use std::path::Path;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::process::{run, CommandSpec};

async fn xdg_open(target: &str) -> Result<()> {
    let spec = CommandSpec::new("xdg-open").arg(target).timeout(Duration::from_secs(15));
    let out = run(&spec).await.map_err(|e| match e {
        Error::Spawn { .. } => Error::Invalid("xdg-open is not installed (package xdg-utils)".into()),
        other => other,
    })?;
    if out.success() {
        Ok(())
    } else {
        Err(Error::Invalid(format!("xdg-open failed: {}", out.failure_message())))
    }
}

pub async fn open_path(path: &Path) -> Result<()> {
    let text = path.to_str().ok_or_else(|| Error::Invalid("path is not valid UTF-8".into()))?;
    if text.starts_with('-') {
        // A leading dash would be read as an option by xdg-open.
        return xdg_open(&format!("./{text}")).await;
    }
    xdg_open(text).await
}

/// Opens the folder that contains `path`.
pub async fn reveal(path: &Path) -> Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => open_path(dir).await,
        _ => open_path(Path::new(".")).await,
    }
}

/// Only http(s) links are handed to the desktop.
pub async fn open_url(url: &str) -> Result<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(Error::Invalid("only http and https links can be opened".into()));
    }
    xdg_open(url).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_non_http_urls() {
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "--version", ""] {
            assert!(open_url(bad).await.is_err(), "{bad}");
        }
    }
}

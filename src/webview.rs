//! Inspectable WebViews and Chrome tabs on a device. Android exposes them as
//! abstract unix sockets named `webview_devtools_remote_<pid>` or
//! `chrome_devtools_remote`. We forward one to a local port and read the
//! standard DevTools `/json` listing from it.

use std::collections::BTreeSet;
use std::time::Duration;

use serde::Deserialize;

use crate::adb::DeviceAdb;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DevtoolsSocket {
    pub name: String,
    /// Process id, when the socket name carries one.
    pub pid: Option<u32>,
}

/// Parses `/proc/net/unix` text.
pub fn parse_sockets(text: &str) -> Vec<DevtoolsSocket> {
    let names: BTreeSet<DevtoolsSocket> = text
        .split_whitespace()
        .filter_map(|tok| tok.strip_prefix('@'))
        .filter(|name| name.ends_with("_devtools_remote") || name.contains("_devtools_remote_"))
        .map(|name| DevtoolsSocket { name: name.to_string(), pid: name.rsplit_once('_').and_then(|(_, p)| p.parse().ok()) })
        .collect();
    names.into_iter().collect()
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub struct Page {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default, rename = "devtoolsFrontendUrl")]
    pub frontend_url: String,
}

pub fn parse_pages(json: &str) -> Result<Vec<Page>> {
    serde_json::from_str(json).map_err(|e| Error::parse("devtools /json", e.to_string()))
}

/// Makes the frontend link absolute and local (`/devtools/...` -> localhost).
pub fn frontend_link(page: &Page, port: u16) -> Option<String> {
    let f = page.frontend_url.trim();
    if f.is_empty() {
        None
    } else if f.starts_with("http://") || f.starts_with("https://") {
        Some(f.to_string())
    } else if f.starts_with('/') {
        Some(format!("http://127.0.0.1:{port}{f}"))
    } else {
        None
    }
}

pub struct Inspectable {
    pub socket: DevtoolsSocket,
    pub package: Option<String>,
    pub port: u16,
    pub pages: Vec<Page>,
}

impl DeviceAdb {
    pub async fn devtools_sockets(&self) -> Result<Vec<DevtoolsSocket>> {
        Ok(parse_sockets(&self.shell("cat /proc/net/unix").await?))
    }

    /// Forwards `localabstract:<socket>` to a free local port.
    pub async fn forward_devtools(&self, socket: &str) -> Result<u16> {
        if !socket.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(Error::Invalid(format!("unexpected socket name '{socket}'")));
        }
        let remote = format!("localabstract:{socket}");
        let out = self.exec(["forward", "tcp:0", remote.as_str()]).await?;
        if !out.success() {
            return Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() });
        }
        out.stdout_text()
            .trim()
            .parse::<u16>()
            .map_err(|_| Error::parse("adb forward", format!("expected a port number, got '{}'", out.stdout_text().trim())))
    }

    pub async fn remove_forward(&self, port: u16) {
        let local = format!("tcp:{port}");
        // Best effort: a stale forward is harmless and ends with the adb server.
        let _ = self.exec(["forward", "--remove", local.as_str()]).await;
    }

    /// Lists the pages behind one DevTools socket.
    pub async fn inspect(&self, socket: DevtoolsSocket) -> Result<Inspectable> {
        let port = self.forward_devtools(&socket.name).await?;
        let pages = fetch_pages(port).await;
        let package = match socket.pid {
            Some(pid) => self
                .shell(&format!("cat /proc/{pid}/cmdline"))
                .await
                .ok()
                .map(|s| s.trim_matches('\0').trim().to_string())
                .filter(|s| !s.is_empty()),
            None => None,
        };
        match pages {
            Ok(pages) => Ok(Inspectable { socket, package, port, pages }),
            Err(e) => {
                self.remove_forward(port).await;
                Err(e)
            }
        }
    }
}

async fn fetch_pages(port: u16) -> Result<Vec<Page>> {
    let body = tokio::task::spawn_blocking(move || -> Result<String> {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).build();
        let url = format!("http://127.0.0.1:{port}/json");
        agent
            .get(&url)
            .call()
            .map_err(|e| Error::Invalid(format!("The WebView did not answer on port {port}: {e}")))?
            .into_string()
            .map_err(|e| Error::Io { context: "reading DevTools listing".into(), source: e })
    })
    .await
    .map_err(|_| Error::Cancelled)??;
    parse_pages(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_devtools_sockets_in_proc_net_unix() {
        let text = "Num RefCount Protocol Flags Type St Inode Path\n\
            0000: 00000002 00000000 00010000 0001 01 1234 @webview_devtools_remote_4321\n\
            0000: 00000002 00000000 00010000 0001 01 1235 @chrome_devtools_remote\n\
            0000: 00000002 00000000 00010000 0001 01 1236 @something_else\n\
            0000: 00000002 00000000 00010000 0001 01 1237 /dev/socket/zygote\n\
            0000: 00000002 00000000 00010000 0001 01 1238 @webview_devtools_remote_4321\n";
        let s = parse_sockets(text);
        assert_eq!(s.len(), 2);
        assert!(s.iter().any(|x| x.name == "chrome_devtools_remote" && x.pid.is_none()));
        assert!(s.iter().any(|x| x.name == "webview_devtools_remote_4321" && x.pid == Some(4321)));
    }

    #[test]
    fn parses_page_listing_with_missing_fields() {
        let json = r#"[{"id":"A","title":"Home","type":"page","url":"https://x.test/","devtoolsFrontendUrl":"/devtools/inspector.html?ws=127.0.0.1:1/devtools/page/A"},{"id":"B"}]"#;
        let pages = parse_pages(json).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].kind, "page");
        assert_eq!(pages[1].title, "");
        assert_eq!(frontend_link(&pages[0], 9222).unwrap(), "http://127.0.0.1:9222/devtools/inspector.html?ws=127.0.0.1:1/devtools/page/A");
        assert!(frontend_link(&pages[1], 9222).is_none());
    }

    #[test]
    fn invalid_json_is_a_parse_error() {
        assert!(matches!(parse_pages("<html>"), Err(Error::Parse { .. })));
    }
}

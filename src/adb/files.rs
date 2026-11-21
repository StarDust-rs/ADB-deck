//! Device file browsing and manipulation.
//!
//! Directory listings come from the device's `ls`; transfers use
//! `adb push/pull` on a pseudo-terminal so progress is reported. Paths under
//! `/data/data/<package>` are reachable without root through `run-as` for
//! debuggable apps.

use std::path::Path;
use std::time::Duration;

use tokio::sync::mpsc;

use super::{sh_quote, DeviceAdb};
use crate::error::{Error, Result};
use crate::process::{run, CommandSpec};
use crate::pty::Pty;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: Option<u64>,
    pub modified: String,
    pub mode: String,
}

fn is_date(token: &str) -> bool {
    let b = token.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Splits off the next whitespace-delimited token, returning (token, rest).
fn next_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// Parses one `ls -l` line from toybox or the older toolbox.
pub fn parse_ls_line(line: &str) -> Option<FileEntry> {
    let mut before: Vec<&str> = Vec::new();
    let mut rest = line.trim_end();
    let date = loop {
        let (tok, r) = next_token(rest)?;
        rest = r;
        if is_date(tok) {
            break tok;
        }
        before.push(tok);
        if before.len() > 8 {
            return None;
        }
    };
    let (time, name_part) = next_token(rest)?;
    let name_full = name_part.trim_start();
    if name_full.is_empty() {
        return None;
    }
    let mode = *before.first()?;
    if mode.len() < 10 || !mode.is_ascii() {
        return None;
    }
    let kind = mode.as_bytes()[0];
    let is_link = kind == b'l';
    let name = if is_link { name_full.split(" -> ").next().unwrap_or(name_full) } else { name_full };
    if name == "." || name == ".." {
        return None;
    }
    // Device nodes print "major, minor" where a size would be.
    let is_node = before.len() >= 2 && before[before.len() - 2].ends_with(',');
    let size =
        (before.len() >= 4 && !is_node).then(|| before.last().and_then(|t| t.parse::<u64>().ok())).flatten().filter(|_| kind != b'd');
    Some(FileEntry {
        name: name.to_string(),
        is_dir: kind == b'd',
        is_link,
        size,
        modified: format!("{date} {time}"),
        mode: mode.to_string(),
    })
}

pub fn parse_ls(text: &str) -> Vec<FileEntry> {
    text.lines().filter_map(parse_ls_line).collect()
}

/// Cleans a device path: absolute, no `.`/`..`/empty segments.
pub fn normalize_path(input: &str) -> Result<String> {
    let t = input.trim();
    if !t.starts_with('/') || t.contains('\0') {
        return Err(Error::Invalid(format!("'{t}' is not an absolute device path")));
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in t.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

pub fn parent(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", _)) | None => "/".to_string(),
        Some((p, _)) => p.to_string(),
    }
}

/// A file name that is safe to create: one segment, not `.`/`..`.
pub fn validate_name(name: &str) -> Result<&str> {
    let n = name.trim();
    if n.is_empty() || n == "." || n == ".." || n.contains('/') || n.contains('\0') {
        return Err(Error::Invalid("Enter a plain file or folder name without slashes".into()));
    }
    Ok(n)
}

/// How a path must be reached on a device without root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    Direct,
    /// Through `run-as <package>`.
    RunAs(String),
}

pub fn access_for(path: &str, rooted: bool) -> Access {
    if rooted {
        return Access::Direct;
    }
    match path.strip_prefix("/data/data/").and_then(|r| r.split('/').next()) {
        Some(pkg) if !pkg.is_empty() => Access::RunAs(pkg.to_string()),
        _ => Access::Direct,
    }
}

fn wrap(access: &Access, script: String) -> String {
    match access {
        Access::Direct => script,
        Access::RunAs(pkg) => format!("run-as {} {}", sh_quote(pkg), script),
    }
}

/// Without root, parts of /data are not listable; the legacy-style virtual
/// entries let the user reach app data and APK locations.
fn virtual_dir(path: &str) -> Option<Vec<&'static str>> {
    match path {
        "/data" => Some(vec!["app", "data", "local"]),
        "/data/local" => Some(vec!["tmp"]),
        _ => None,
    }
}

fn dir_entry(name: &str) -> FileEntry {
    FileEntry { name: name.to_string(), is_dir: true, is_link: false, size: None, modified: String::new(), mode: "drwx------".into() }
}

impl DeviceAdb {
    pub async fn is_root(&self) -> Result<bool> {
        Ok(super::overview::is_root_id(&self.shell("id").await?))
    }

    pub async fn list_dir(&self, path: &str, rooted: bool) -> Result<Vec<FileEntry>> {
        let path = normalize_path(path)?;
        if !rooted {
            if let Some(names) = virtual_dir(&path) {
                return Ok(names.into_iter().map(dir_entry).collect());
            }
            if path == "/data/data" {
                return self.list_packages_as_dirs().await;
            }
        }
        let access = access_for(&path, rooted);
        let script = wrap(&access, format!("ls -lA {}/", sh_quote(&path)));
        let out = self.shell_output(&script, Duration::from_secs(30)).await?;
        let text = out.stdout_text().replace("\r\n", "\n");
        let mut entries = parse_ls(&text);
        if entries.is_empty() && !out.success() {
            return Err(Error::Invalid(device_message(&out.failure_message())));
        }
        self.resolve_links(&path, &access, &mut entries).await;
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(entries)
    }

    async fn list_packages_as_dirs(&self) -> Result<Vec<FileEntry>> {
        let text = self.shell("pm list packages").await?;
        let mut names: Vec<&str> = text.lines().filter_map(|l| l.trim().strip_prefix("package:")).collect();
        names.sort_unstable();
        Ok(names.into_iter().map(dir_entry).collect())
    }

    /// Symlinks pointing at directories (like /sdcard) are shown as folders.
    async fn resolve_links(&self, dir: &str, access: &Access, entries: &mut [FileEntry]) {
        let links: Vec<usize> = entries.iter().enumerate().filter(|(_, e)| e.is_link).map(|(i, _)| i).take(200).collect();
        if links.is_empty() {
            return;
        }
        let mut script = String::new();
        for &i in &links {
            let p = sh_quote(&join(dir, &entries[i].name));
            script.push_str(&format!("if [ -d {p} ]; then echo d; else echo f; fi\n"));
        }
        let Ok(text) = self.shell(&wrap(access, format!("sh -c {}", sh_quote(&script)))).await else {
            return;
        };
        for (&i, answer) in links.iter().zip(text.lines()) {
            entries[i].is_dir = answer.trim() == "d";
        }
    }

    pub async fn stat_path(&self, path: &str, rooted: bool) -> Result<FileEntry> {
        let path = normalize_path(path)?;
        let access = access_for(&path, rooted);
        let out = self.shell_output(&wrap(&access, format!("ls -ldL {}", sh_quote(&path))), Duration::from_secs(15)).await?;
        parse_ls(&out.stdout_text()).into_iter().next().ok_or_else(|| Error::Invalid(device_message(&out.failure_message())))
    }

    pub async fn make_dir(&self, path: &str, rooted: bool) -> Result<()> {
        let path = normalize_path(path)?;
        let access = access_for(&path, rooted);
        self.shell_ok(&wrap(&access, format!("mkdir -p {}", sh_quote(&path)))).await.map(|_| ())
    }

    pub async fn delete_path(&self, path: &str, directory: bool, rooted: bool) -> Result<()> {
        let path = normalize_path(path)?;
        if path.matches('/').count() < 2 {
            return Err(Error::Invalid("Refusing to delete a top-level folder".into()));
        }
        let access = access_for(&path, rooted);
        let cmd = if directory { "rm -rf" } else { "rm -f" };
        self.shell_ok(&wrap(&access, format!("{cmd} {}", sh_quote(&path)))).await.map(|_| ())
    }

    pub async fn move_path(&self, from: &str, to: &str, rooted: bool) -> Result<()> {
        let (from, to) = (normalize_path(from)?, normalize_path(to)?);
        let access = match (access_for(&from, rooted), access_for(&to, rooted)) {
            (Access::Direct, other) | (other, Access::Direct) => other,
            (a, _) => a,
        };
        self.shell_ok(&wrap(&access, format!("mv {} {}", sh_quote(&from), sh_quote(&to)))).await.map(|_| ())
    }

    /// Package whose files `path` belongs to, when `run-as` is required.
    pub fn runas_package(path: &str, rooted: bool) -> Option<String> {
        match access_for(path, rooted) {
            Access::RunAs(p) => Some(p),
            Access::Direct => None,
        }
    }
}

fn device_message(raw: &str) -> String {
    let m = raw.trim();
    if m.contains("Permission denied") {
        "Permission denied on the device. Some folders need root access.".into()
    } else if m.contains("No such file") {
        "That folder does not exist on the device.".into()
    } else if m.contains("not debuggable") || m.contains("is not an app") || m.contains("Unknown package") {
        "This app is not debuggable, so its private files cannot be accessed without root.".into()
    } else if m.is_empty() {
        "The device returned no listing.".into()
    } else {
        m.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransferEvent {
    Progress { percent: u8, current: String },
}

/// Extracts the last `[ nn%] name` progress marker from adb's output.
pub fn parse_progress(chunk: &str) -> Option<(u8, String)> {
    let mut found = None;
    for part in chunk.split(['\r', '\n']) {
        let part = part.trim_start();
        let Some(rest) = part.strip_prefix('[') else { continue };
        let Some((pct, name)) = rest.split_once("%]") else { continue };
        if let Ok(p) = pct.trim().parse::<u8>() {
            found = Some((p.min(100), name.trim().to_string()));
        }
    }
    found
}

impl DeviceAdb {
    /// `adb pull`/`push` with progress. Dropping the future kills adb.
    pub async fn transfer(
        &self,
        direction: Direction,
        source: &str,
        dest: &str,
        events: mpsc::UnboundedSender<TransferEvent>,
    ) -> Result<()> {
        let verb = if direction == Direction::Upload { "push" } else { "pull" };
        let args = ["-s", self.serial(), verb, source, dest].map(std::ffi::OsString::from).to_vec();
        let mut pty = Pty::spawn(self.adb().path(), &args, 24, 200)?;
        let mut tail = String::new();
        while let Some(chunk) = pty.output.recv().await {
            let text = String::from_utf8_lossy(&chunk);
            if let Some((percent, current)) = parse_progress(&text) {
                let _ = events.send(TransferEvent::Progress { percent, current });
            }
            tail.push_str(&text);
            if tail.len() > 8192 {
                let cut = tail.len() - 4096;
                let cut = (cut..tail.len()).find(|i| tail.is_char_boundary(*i)).unwrap_or(0);
                tail.drain(..cut);
            }
        }
        if pty.wait().await == 0 {
            Ok(())
        } else {
            Err(Error::Exit { program: "adb".into(), code: None, message: transfer_failure(&tail) })
        }
    }

    /// Copies a file out of an app's private storage via `run-as`.
    pub async fn pull_runas_file(&self, package: &str, source: &str, dest: &Path) -> Result<()> {
        // Stream into a sibling file and rename on success, so a failed or
        // cancelled pull never truncates a file that already exists at `dest`.
        let mut part_name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        part_name.push(".part");
        let part = dest.with_file_name(part_name);
        let guard = PartialFile(Some(part.clone()));
        let file = std::fs::File::create(&part).map_err(|e| Error::io(format!("creating {}", part.display()), e))?;
        let mut spec = self
            .spec()
            .arg("exec-out")
            .arg(format!("run-as {} cat {}", sh_quote(package), sh_quote(source)))
            .timeout(Duration::from_secs(600));
        spec.max_stdout = 0;
        let mut cmd = tokio::process::Command::new(&spec.program);
        cmd.args(&spec.args).kill_on_drop(true).stdout(file).stderr(std::process::Stdio::piped());
        let output = tokio::time::timeout(spec.timeout, cmd.output())
            .await
            .map_err(|_| Error::Timeout { program: "adb".into(), secs: spec.timeout.as_secs() })?
            .map_err(|e| Error::Spawn { program: "adb".into(), source: e })?;
        if !output.status.success() {
            return Err(Error::Invalid(device_message(&String::from_utf8_lossy(&output.stderr))));
        }
        std::fs::rename(&part, dest).map_err(|e| Error::io(format!("saving {}", dest.display()), e))?;
        guard.disarm();
        Ok(())
    }

    /// Writes a local file into an app's private storage via `run-as`.
    pub async fn push_runas_file(&self, package: &str, source: &Path, dest: &str) -> Result<()> {
        let data = tokio::fs::read(source).await.map_err(|e| Error::io(format!("reading {}", source.display()), e))?;
        let script = format!("run-as {} sh -c {}", sh_quote(package), sh_quote(&format!("cat > {}", sh_quote(dest))));
        let spec: CommandSpec = self.spec().arg("shell").arg(script).stdin(data).timeout(Duration::from_secs(600));
        let out = run(&spec).await?;
        if out.success() {
            Ok(())
        } else {
            Err(Error::Invalid(device_message(&out.failure_message())))
        }
    }
}

/// Removes a partially written file unless the write was committed. Dropping
/// covers errors, timeouts and cancellation of the owning task.
struct PartialFile(Option<std::path::PathBuf>);

impl PartialFile {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn transfer_failure(tail: &str) -> String {
    let line = tail
        .split(['\r', '\n'])
        .map(str::trim)
        .rev()
        .find(|l| l.starts_with("adb:") || l.starts_with("error:") || l.contains("remote object") || l.contains("failed"))
        .unwrap_or("adb reported an error while copying");
    line.trim_start_matches("adb: ").trim_start_matches("error: ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modern_toybox_listing() {
        let text = "total 40\n\
            drwxrwx--x  5 root sdcard_rw 4096 2023-05-01 10:12 Android\n\
            -rw-rw----  1 root sdcard_rw 12345 2023-05-02 08:00 my photo (1).jpg\n\
            lrwxrwxrwx  1 root root        21 2023-01-01 00:00 sdcard -> /storage/self/primary\n\
            crw-rw-rw-  1 root root   1,   3 2023-01-01 00:00 null\n";
        let e = parse_ls(text);
        assert_eq!(e.len(), 4);
        assert!(e[0].is_dir && e[0].size.is_none());
        assert_eq!(e[1].name, "my photo (1).jpg");
        assert_eq!(e[1].size, Some(12345));
        assert_eq!(e[1].modified, "2023-05-02 08:00");
        assert!(e[2].is_link && e[2].name == "sdcard");
        assert_eq!(e[3].size, None, "device nodes have no size");
    }

    #[test]
    fn parses_older_toolbox_listing() {
        let e = parse_ls("drwxrwx--x root     sdcard_rw          2014-01-01 12:00 Android\n-rw-r--r-- root     root         4096 2014-01-01 12:00 a.txt\n");
        assert_eq!(e.len(), 2);
        assert!(e[0].is_dir);
        assert_eq!(e[1].size, Some(4096));
    }

    #[test]
    fn numeric_group_on_an_old_directory_is_not_a_size() {
        let e = parse_ls("drwxrwx--x root 1028 2014-01-01 12:00 data\n");
        assert_eq!(e[0].size, None);
    }

    #[test]
    fn ignores_errors_headers_and_dot_entries() {
        assert!(parse_ls("ls: /x: Permission denied\ntotal 0\n").is_empty());
        assert!(parse_ls("drwxr-xr-x 2 root root 4096 2023-01-01 00:00 .\n").is_empty());
    }

    #[test]
    fn path_helpers() {
        assert_eq!(normalize_path("/sdcard//DCIM/./../Download/").unwrap(), "/sdcard/Download");
        assert_eq!(normalize_path("/../..").unwrap(), "/");
        assert!(normalize_path("sdcard").is_err());
        assert_eq!(join("/sdcard/", "a"), "/sdcard/a");
        assert_eq!(join("/", "a"), "/a");
        assert_eq!(parent("/sdcard/DCIM"), "/sdcard");
        assert_eq!(parent("/sdcard"), "/");
        assert_eq!(parent("/"), "/");
    }

    #[test]
    fn names_are_validated() {
        assert!(validate_name(" ok.txt ").is_ok());
        for bad in ["", " ", ".", "..", "a/b", "a\0b"] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn runas_is_only_used_for_app_data_without_root() {
        assert_eq!(access_for("/data/data/com.foo/files", false), Access::RunAs("com.foo".into()));
        assert_eq!(access_for("/data/data/com.foo/files", true), Access::Direct);
        assert_eq!(access_for("/sdcard/x", false), Access::Direct);
        assert_eq!(access_for("/data/data", false), Access::Direct);
        assert_eq!(access_for("/data/data/", false), Access::Direct);
    }

    #[test]
    fn progress_markers_are_extracted() {
        assert_eq!(parse_progress("\r[  0%] /sdcard/a.bin\r[ 45%] /sdcard/a.bin"), Some((45, "/sdcard/a.bin".into())));
        assert_eq!(parse_progress("[100%] x\n/sdcard/a.bin: 1 file pulled"), Some((100, "x".into())));
        assert_eq!(parse_progress("/sdcard/a.bin: 1 file pulled"), None);
    }

    #[test]
    fn transfer_failures_pick_the_adb_error_line() {
        let tail = "[ 0%] x\r\nadb: error: failed to stat remote object '/nope': No such file or directory\r\n";
        assert!(transfer_failure(tail).contains("No such file"));
        assert!(transfer_failure("").contains("error while copying"));
    }

    #[test]
    fn device_messages_are_friendly() {
        assert!(device_message("ls: /data: Permission denied").contains("root"));
        assert!(device_message("ls: /x: No such file or directory").contains("does not exist"));
        assert!(
            device_message("run-as: package not debuggable: com.x").contains("debuggable")
                || device_message("run-as: package not debuggable: com.x").contains("run-as")
        );
    }
}

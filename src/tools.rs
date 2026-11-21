//! Discovery of external executables and the guidance shown when one is
//! missing. Nothing is downloaded or bundled.

use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Adb,
    Apksigner,
    Emulator,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Adb, Tool::Apksigner, Tool::Emulator];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Adb => "adb",
            Tool::Apksigner => "apksigner",
            Tool::Emulator => "emulator",
        }
    }

    pub fn required(self) -> bool {
        matches!(self, Tool::Adb)
    }

    /// What breaks without the tool, so the user can judge if it matters.
    pub fn needed_for(self) -> &'static str {
        match self {
            Tool::Adb => "everything that talks to a device",
            Tool::Apksigner => "APK signing (also needs Java)",
            Tool::Emulator => "starting Android virtual devices",
        }
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Tool::Adb => "On Fedora run: sudo dnf install android-tools",
            Tool::Apksigner => {
                "Install the Android SDK build-tools (sdkmanager \"build-tools;<version>\") and a \
                 Java runtime (sudo dnf install java-latest-openjdk-headless), then set the \
                 apksigner path in Settings if it is not on PATH."
            }
            Tool::Emulator => {
                "Install the Android SDK emulator package and set ANDROID_HOME, or put the \
                 emulator directory on PATH."
            }
        }
    }

    pub fn missing_error(self) -> Error {
        Error::ToolMissing { tool: self.name(), hint: self.install_hint().to_string() }
    }
}

/// Environment the lookups depend on, injectable for tests.
#[derive(Debug, Clone, Default)]
pub struct LookupEnv {
    pub path: Vec<PathBuf>,
    pub android_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl LookupEnv {
    pub fn from_process() -> Self {
        let path = env::var_os("PATH").map(|p| env::split_paths(&p).collect()).unwrap_or_default();
        let android_home = env::var_os("ANDROID_HOME").or_else(|| env::var_os("ANDROID_SDK_ROOT")).map(PathBuf::from);
        Self { path, android_home, home: env::var_os("HOME").map(PathBuf::from) }
    }

    fn sdk_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(h) = &self.android_home {
            roots.push(h.clone());
        }
        if let Some(h) = &self.home {
            roots.push(h.join("Android/Sdk"));
        }
        roots
    }
}

pub fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

fn in_path(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(name)).find(|p| is_executable_file(p))
}

/// Newest build-tools directory containing `apksigner`.
fn sdk_apksigner(root: &Path) -> Option<PathBuf> {
    let mut versions: Vec<PathBuf> = std::fs::read_dir(root.join("build-tools")).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    versions.sort();
    versions.into_iter().rev().map(|d| d.join("apksigner")).find(|p| is_executable_file(p))
}

/// Resolves a tool. An explicit user path always wins; if it is unusable the
/// tool is reported missing rather than silently substituting another one.
pub fn locate(tool: Tool, configured: &str, env: &LookupEnv) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if p.components().count() == 1 {
            return in_path(configured, &env.path);
        }
        return is_executable_file(&p).then_some(p);
    }
    if let Some(p) = in_path(tool.name(), &env.path) {
        return Some(p);
    }
    for root in env.sdk_roots() {
        let found = match tool {
            Tool::Adb => Some(root.join("platform-tools/adb")),
            Tool::Emulator => Some(root.join("emulator/emulator")),
            Tool::Apksigner => sdk_apksigner(&root),
        };
        if let Some(p) = found.filter(|p| is_executable_file(p)) {
            return Some(p);
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct ToolStatus {
    pub tool: Tool,
    pub path: Option<PathBuf>,
}

pub fn detect_all(settings: &crate::store::Settings, env: &LookupEnv) -> Vec<ToolStatus> {
    Tool::ALL
        .iter()
        .map(|&tool| {
            let configured = match tool {
                Tool::Adb => settings.adb_path.as_str(),
                Tool::Apksigner => settings.apksigner_path.as_str(),
                Tool::Emulator => "",
            };
            ToolStatus { tool, path: locate(tool, configured, env) }
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    pub fn write_exe(path: &Path, body: &str) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let script = if body.starts_with("#!") { body.to_string() } else { format!("#!/bin/sh\n{body}") };
        fs::write(path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::write_exe;
    use super::*;

    #[test]
    fn finds_tool_on_path_and_ignores_non_executables() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("adb"), "x").unwrap(); // not executable
        let env = LookupEnv { path: vec![bin.clone()], ..Default::default() };
        assert_eq!(locate(Tool::Adb, "", &env), None);
        write_exe(&bin.join("adb"), "#!/bin/sh\n");
        assert_eq!(locate(Tool::Adb, "", &env), Some(bin.join("adb")));
    }

    #[test]
    fn explicit_path_wins_and_bad_explicit_path_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let on_path = write_exe(&dir.path().join("bin/adb"), "#!/bin/sh\n");
        let custom = write_exe(&dir.path().join("custom/my-adb"), "#!/bin/sh\n");
        let env = LookupEnv { path: vec![dir.path().join("bin")], ..Default::default() };
        assert_eq!(locate(Tool::Adb, custom.to_str().unwrap(), &env), Some(custom));
        assert_eq!(locate(Tool::Adb, "/does/not/exist", &env), None);
        assert_eq!(locate(Tool::Adb, "adb", &env), Some(on_path));
    }

    #[test]
    fn falls_back_to_the_android_sdk_layout() {
        let dir = tempfile::tempdir().unwrap();
        let sdk = dir.path().join("sdk");
        let adb = write_exe(&sdk.join("platform-tools/adb"), "#!/bin/sh\n");
        write_exe(&sdk.join("build-tools/33.0.0/apksigner"), "#!/bin/sh\n");
        let newest = write_exe(&sdk.join("build-tools/34.0.0/apksigner"), "#!/bin/sh\n");
        let env = LookupEnv { android_home: Some(sdk), ..Default::default() };
        assert_eq!(locate(Tool::Adb, "", &env), Some(adb));
        assert_eq!(locate(Tool::Apksigner, "", &env), Some(newest));
    }

    #[test]
    fn missing_tool_error_explains_how_to_fix_it() {
        let msg = Tool::Adb.missing_error().to_string();
        assert!(msg.contains("adb was not found"));
        assert!(msg.contains("dnf install android-tools"));
        assert!(Tool::Adb.required() && !Tool::Apksigner.required());
    }
}

//! Error type shared by all backend modules.

use std::io;

use thiserror::Error;

/// Failures that cross module boundaries. Variants are chosen so the UI can
/// tell an expected operational problem (missing tool, device gone) from a
/// parsing defect.
#[derive(Debug, Error)]
pub enum Error {
    #[error("{tool} was not found. {hint}")]
    ToolMissing { tool: &'static str, hint: String },

    #[error("could not start {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },

    #[error("{program} did not finish within {secs}s")]
    Timeout { program: String, secs: u64 },

    #[error("{program} failed{}: {message}", code.map(|c| format!(" (exit {c})")).unwrap_or_default())]
    Exit { program: String, code: Option<i32>, message: String },

    #[error("unexpected output from {context}: {detail}")]
    Parse { context: &'static str, detail: String },

    #[error("device {0} is no longer connected")]
    DeviceGone(String),

    #[error("{0}")]
    Invalid(String),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },

    #[error("operation was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Error::Io { context: context.into(), source }
    }

    pub fn parse(context: &'static str, detail: impl Into<String>) -> Self {
        Error::Parse { context, detail: detail.into() }
    }

    /// True when the failure means the target device vanished or is unusable,
    /// so panels should drop their state instead of showing a raw error.
    pub fn is_device_gone(&self) -> bool {
        matches!(self, Error::DeviceGone(_))
    }
}

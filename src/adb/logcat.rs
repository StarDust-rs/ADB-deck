//! Log streaming and filtering.

use std::collections::HashMap;
use std::time::Duration;

use super::DeviceAdb;
use crate::error::Result;
use crate::process::{spawn_lines, LineStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Verbose = 2,
    Debug = 3,
    Info = 4,
    Warn = 5,
    Error = 6,
    Fatal = 7,
}

impl Level {
    /// Levels offered as a minimum-priority filter.
    pub const FILTERABLE: [Level; 5] = [Level::Verbose, Level::Debug, Level::Info, Level::Warn, Level::Error];

    pub fn from_letter(c: char) -> Option<Level> {
        Some(match c {
            'V' => Level::Verbose,
            'D' => Level::Debug,
            'I' => Level::Info,
            'W' => Level::Warn,
            'E' => Level::Error,
            'F' | 'A' => Level::Fatal,
            _ => return None,
        })
    }

    pub fn letter(self) -> char {
        match self {
            Level::Verbose => 'V',
            Level::Debug => 'D',
            Level::Info => 'I',
            Level::Warn => 'W',
            Level::Error => 'E',
            Level::Fatal => 'F',
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Verbose => "VERBOSE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARNING",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub date: String,
    pub time: String,
    pub pid: u32,
    pub tid: u32,
    pub level: Level,
    pub tag: String,
    pub message: String,
}

impl LogEntry {
    /// The line as saved to a file.
    pub fn to_line(&self) -> String {
        format!("{} {} {:>5} {:>5} {} {}: {}", self.date, self.time, self.pid, self.tid, self.level.letter(), self.tag, self.message)
    }
}

fn token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// Parses `logcat -v threadtime`: `MM-DD HH:MM:SS.mmm PID TID L TAG: msg`.
pub fn parse_threadtime(line: &str) -> Option<LogEntry> {
    let (date, rest) = token(line)?;
    let (time, rest) = token(rest)?;
    let (pid, rest) = token(rest)?;
    let (tid, rest) = token(rest)?;
    let (level, rest) = token(rest)?;
    if date.len() != 5 || date.as_bytes()[2] != b'-' || !time.contains(':') {
        return None;
    }
    let mut chars = level.chars();
    let level = Level::from_letter(chars.next()?).filter(|_| chars.next().is_none())?;
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let (tag, message) = match rest.find(": ") {
        Some(i) => (&rest[..i], &rest[i + 2..]),
        None => (rest.trim_end_matches(':'), ""),
    };
    Some(LogEntry {
        date: date.into(),
        time: time.into(),
        pid: pid.parse().ok()?,
        tid: tid.parse().ok()?,
        level,
        tag: tag.trim_end().to_string(),
        message: message.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    pub min_level: Level,
    pub package: String,
    pub tag: String,
    pub keyword: String,
}

impl Default for LogFilter {
    fn default() -> Self {
        Self { min_level: Level::Verbose, package: String::new(), tag: String::new(), keyword: String::new() }
    }
}

use crate::text::contains_ci;

impl LogFilter {
    pub fn matches(&self, entry: &LogEntry, package: Option<&str>) -> bool {
        entry.level >= self.min_level
            && (self.package.is_empty() || package.is_some_and(|p| contains_ci(p, &self.package)))
            && contains_ci(&entry.tag, &self.tag)
            && (self.keyword.is_empty() || contains_ci(&entry.message, &self.keyword) || contains_ci(&entry.tag, &self.keyword))
    }
}

/// `ps -A -o PID,ARGS` -> pid -> process name.
pub fn parse_ps(text: &str) -> HashMap<u32, String> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let (pid, rest) = token(l)?;
            let name = rest.split_whitespace().next()?;
            Some((pid.parse().ok()?, name.to_string()))
        })
        .collect()
}

impl DeviceAdb {
    /// Clears the device log buffer and streams new entries as raw lines.
    pub async fn start_logcat(&self) -> Result<LineStream> {
        // A failed clear (restricted buffers) must not prevent streaming.
        let _ = self.exec(["logcat", "-c"]).await;
        let spec = self.spec().args(["logcat", "-v", "threadtime"]).timeout(Duration::from_secs(u32::MAX as u64));
        spawn_lines(&spec, 2048, false)
    }

    pub async fn pid_names(&self) -> Result<HashMap<u32, String>> {
        Ok(parse_ps(&self.shell("ps -A -o PID,ARGS").await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_threadtime_lines() {
        let e = parse_threadtime("10-28 22:45:01.123  1234  5678 I ActivityManager: Start proc: com.x").unwrap();
        assert_eq!((e.date.as_str(), e.time.as_str()), ("10-28", "22:45:01.123"));
        assert_eq!((e.pid, e.tid, e.level), (1234, 5678, Level::Info));
        assert_eq!(e.tag, "ActivityManager");
        assert_eq!(e.message, "Start proc: com.x");
    }

    #[test]
    fn tag_padding_and_colons_in_messages() {
        let e = parse_threadtime("01-02 03:04:05.678   10   11 W Tag     : a: b: c").unwrap();
        assert_eq!(e.tag, "Tag");
        assert_eq!(e.message, "a: b: c");
        let empty = parse_threadtime("01-02 03:04:05.678   10   11 D Tag:").unwrap();
        assert_eq!((empty.tag.as_str(), empty.message.as_str()), ("Tag", ""));
    }

    #[test]
    fn rejects_non_entries() {
        for l in ["--------- beginning of main", "", "garbage in", "01-02 03:04:05.678 x y I T: m", "01-02 03:04:05.678 1 2 Q T: m"] {
            assert!(parse_threadtime(l).is_none(), "{l:?}");
        }
    }

    #[test]
    fn saved_line_round_trips() {
        let e = parse_threadtime("10-28 22:45:01.123  1234  5678 E Tag: boom").unwrap();
        assert_eq!(parse_threadtime(&e.to_line()).unwrap(), e);
    }

    #[test]
    fn filter_combines_level_package_tag_and_keyword() {
        let e = parse_threadtime("10-28 22:45:01.123  1234  5678 W NetTag: Connection Lost").unwrap();
        let f = LogFilter { min_level: Level::Warn, package: "example".into(), tag: "nettag".into(), keyword: "lost".into() };
        assert!(f.matches(&e, Some("com.example.app")));
        assert!(!f.matches(&e, Some("com.other")));
        assert!(!f.matches(&e, None));
        let strict = LogFilter { min_level: Level::Error, ..LogFilter::default() };
        assert!(!strict.matches(&e, None));
        assert!(LogFilter::default().matches(&e, None));
    }

    #[test]
    fn ps_output_maps_pids() {
        let m = parse_ps("  PID ARGS\n 1234 com.example.app\n  987 /system/bin/surfaceflinger\nbad\n");
        assert_eq!(m[&1234], "com.example.app");
        assert_eq!(m[&987], "/system/bin/surfaceflinger");
        assert_eq!(m.len(), 2);
    }
}

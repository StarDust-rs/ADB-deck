//! Process execution with explicit arguments, timeouts, bounded output and
//! cancellation-by-drop. Nothing here ever builds a shell string.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::{Error, Result};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_STDOUT: usize = 8 * 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const MAX_LINE: usize = 64 * 1024;

/// A fully specified child process invocation.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    pub max_stdout: usize,
    /// Bytes piped to the child's stdin, then stdin is closed.
    pub stdin: Option<Vec<u8>>,
}

impl CommandSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            timeout: DEFAULT_TIMEOUT,
            max_stdout: DEFAULT_MAX_STDOUT,
            stdin: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn max_stdout(mut self, max: usize) -> Self {
        self.max_stdout = max;
        self
    }

    pub fn stdin(mut self, data: Vec<u8>) -> Self {
        self.stdin = Some(data);
        self
    }

    pub fn program_name(&self) -> String {
        self.program.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.program.display().to_string())
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        cmd
    }

    fn spawn_error(&self, source: std::io::Error) -> Error {
        Error::Spawn { program: self.program.display().to_string(), source }
    }
}

/// Captured result of a finished process.
#[derive(Debug, Clone, Default)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
    /// Set when stdout exceeded the configured cap and the tail was dropped.
    pub truncated: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Best human-readable failure text: stderr first, then stdout.
    pub fn failure_message(&self) -> String {
        let err = self.stderr.trim();
        if !err.is_empty() {
            return err.to_string();
        }
        let out = self.stdout_text();
        let out = out.trim();
        if out.is_empty() {
            "no output".to_string()
        } else {
            out.to_string()
        }
    }
}

/// Reads a stream to the end, keeping at most `cap` bytes. The remainder is
/// drained so the child never blocks on a full pipe.
async fn read_capped<R: AsyncRead + Unpin>(mut reader: R, cap: usize) -> (Vec<u8>, bool) {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(kept.len());
                if room >= n {
                    kept.extend_from_slice(&buf[..n]);
                } else {
                    kept.extend_from_slice(&buf[..room]);
                    truncated = true;
                }
            }
        }
    }
    (kept, truncated)
}

/// Runs a process to completion. A non-zero exit is returned as data; use
/// [`run_checked`] when it should be an error. Dropping the returned future
/// kills the child.
pub async fn run(spec: &CommandSpec) -> Result<Output> {
    let mut cmd = spec.command();
    if spec.stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| spec.spawn_error(e))?;

    if let (Some(data), Some(mut stdin)) = (spec.stdin.clone(), child.stdin.take()) {
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            // A child that exits early closes the pipe; that is not our error.
            let _ = stdin.write_all(&data).await;
            let _ = stdin.shutdown().await;
        });
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let max_stdout = spec.max_stdout;

    let work = async {
        let out_task = async {
            match stdout {
                Some(s) => read_capped(s, max_stdout).await,
                None => (Vec::new(), false),
            }
        };
        let err_task = async {
            match stderr {
                Some(s) => read_capped(s, MAX_STDERR).await.0,
                None => Vec::new(),
            }
        };
        let ((stdout, truncated), stderr) = tokio::join!(out_task, err_task);
        let status = child.wait().await;
        (stdout, truncated, stderr, status)
    };

    match tokio::time::timeout(spec.timeout, work).await {
        Ok((stdout, truncated, stderr, status)) => {
            let status = status.map_err(|e| spec.spawn_error(e))?;
            Ok(Output { code: status.code(), stdout, stderr: String::from_utf8_lossy(&stderr).into_owned(), truncated })
        }
        Err(_) => {
            // `work` borrowed `child`; it is dropped with the future, and
            // kill_on_drop terminates the process.
            Err(Error::Timeout { program: spec.program_name(), secs: spec.timeout.as_secs() })
        }
    }
}

/// Like [`run`] but maps a non-zero exit status to [`Error::Exit`].
pub async fn run_checked(spec: &CommandSpec) -> Result<Output> {
    let out = run(spec).await?;
    if out.success() {
        Ok(out)
    } else {
        Err(Error::Exit { program: spec.program_name(), code: out.code, message: out.failure_message() })
    }
}

/// Line-oriented output of a long-running child. Dropping the stream aborts
/// the reader task, which drops the child and kills it.
pub struct LineStream {
    pub lines: mpsc::Receiver<String>,
    task: JoinHandle<()>,
}

impl LineStream {
    /// True once the child has exited and all lines were delivered.
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for LineStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts `spec` and forwards stdout lines (stderr lines too when
/// `merge_stderr`) through a bounded channel. The final exit status is
/// reported as a last pseudo line only through `on_exit`.
pub fn spawn_lines(spec: &CommandSpec, capacity: usize, merge_stderr: bool) -> Result<LineStream> {
    let mut child = spec.command().spawn().map_err(|e| spec.spawn_error(e))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = mpsc::channel(capacity);

    let task = tokio::spawn(async move {
        let out = forward_lines(stdout, tx.clone());
        let err = async {
            if merge_stderr {
                forward_lines(stderr, tx.clone()).await;
            }
        };
        tokio::join!(out, err);
        drop(tx);
        // Keep the child alive until the pipes close, then reap it.
        reap(child).await;
    });

    Ok(LineStream { lines: rx, task })
}

async fn reap(mut child: Child) {
    let _ = child.wait().await;
}

async fn forward_lines<R: AsyncRead + Unpin>(reader: Option<R>, tx: mpsc::Sender<String>) {
    let Some(mut reader) = reader else { return };
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\n' {
                if !send_line(&tx, &mut pending).await {
                    return;
                }
            } else if pending.len() < MAX_LINE {
                pending.push(b);
            }
        }
    }
    if !pending.is_empty() {
        send_line(&tx, &mut pending).await;
    }
}

/// Returns false when the receiver is gone and reading should stop.
async fn send_line(tx: &mpsc::Sender<String>, pending: &mut Vec<u8>) -> bool {
    if pending.last() == Some(&b'\r') {
        pending.pop();
    }
    let line = String::from_utf8_lossy(pending).into_owned();
    pending.clear();
    tx.send(line).await.is_ok()
}

/// Asks a process we started to exit (SIGTERM), without using a shell.
pub fn request_terminate(pid: u32) {
    let _ = std::process::Command::new("kill").arg("-TERM").arg(pid.to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> CommandSpec {
        CommandSpec::new("/bin/sh").arg("-c").arg(script)
    }

    #[tokio::test]
    async fn captures_stdout_stderr_and_status() {
        let out = run(&sh("echo out; echo err >&2; exit 3")).await.unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout_text(), "out\n");
        assert_eq!(out.stderr, "err\n");
    }

    #[tokio::test]
    async fn checked_run_reports_exit_with_stderr() {
        let err = run_checked(&sh("echo boom >&2; exit 2")).await.unwrap_err();
        match err {
            Error::Exit { code, message, .. } => {
                assert_eq!(code, Some(2));
                assert_eq!(message, "boom");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_program_is_spawn_error() {
        let err = run(&CommandSpec::new("/nonexistent/adb-x")).await.unwrap_err();
        assert!(matches!(err, Error::Spawn { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn timeout_kills_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("alive");
        let script = format!("sleep 1; touch {}", marker.display());
        let spec = sh(&script).timeout(Duration::from_millis(100));
        let err = run(&spec).await.unwrap_err();
        assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!marker.exists(), "child kept running after timeout");
    }

    #[tokio::test]
    async fn stdout_is_capped_without_blocking_the_child() {
        let spec = sh("head -c 200000 /dev/zero").max_stdout(1000);
        let out = run(&spec).await.unwrap();
        assert_eq!(out.stdout.len(), 1000);
        assert!(out.truncated);
        assert!(out.success());
    }

    #[tokio::test]
    async fn stdin_is_delivered_and_closed() {
        let out = run(&sh("cat").stdin(b"hello".to_vec())).await.unwrap();
        assert_eq!(out.stdout_text(), "hello");
    }

    #[tokio::test]
    async fn arguments_are_not_interpreted_by_a_shell() {
        let spec = CommandSpec::new("/bin/echo").arg("$(touch /tmp/never); `id`");
        let out = run(&spec).await.unwrap();
        assert_eq!(out.stdout_text(), "$(touch /tmp/never); `id`\n");
    }

    #[tokio::test]
    async fn line_stream_delivers_lines_and_kills_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        let script = format!("echo one; echo two; sleep 1; touch {}", marker.display());
        let mut stream = spawn_lines(&sh(&script), 16, false).unwrap();
        assert_eq!(stream.lines.recv().await.as_deref(), Some("one"));
        assert_eq!(stream.lines.recv().await.as_deref(), Some("two"));
        drop(stream);
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert!(!marker.exists(), "child survived stream drop");
    }

    #[tokio::test]
    async fn line_stream_ends_when_child_exits() {
        let mut stream = spawn_lines(&sh("printf 'a\\r\\nb'"), 16, false).unwrap();
        assert_eq!(stream.lines.recv().await.as_deref(), Some("a"));
        assert_eq!(stream.lines.recv().await.as_deref(), Some("b"));
        assert_eq!(stream.lines.recv().await, None);
    }
}

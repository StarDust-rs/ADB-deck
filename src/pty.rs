//! Pseudo-terminal child processes. adb only prints transfer progress and
//! only behaves like a terminal program when attached to a TTY, so
//! interactive shells and `adb push/pull` run through here.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tokio::sync::mpsc;

use crate::error::{Error, Result};

pub struct Pty {
    pub output: mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

fn size(rows: u16, cols: u16) -> PtySize {
    PtySize { rows: rows.max(1), cols: cols.max(1), pixel_width: 0, pixel_height: 0 }
}

impl Pty {
    pub fn spawn(program: &Path, args: &[OsString], rows: u16, cols: u16) -> Result<Pty> {
        let pair = native_pty_system().openpty(size(rows, cols)).map_err(|e| Error::Invalid(format!("cannot open a terminal: {e}")))?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| Error::Spawn { program: program.display().to_string(), source: std::io::Error::other(e.to_string()) })?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| Error::Invalid(format!("cannot read from the terminal: {e}")))?;
        let writer = pair.master.take_writer().map_err(|e| Error::Invalid(format!("cannot write to the terminal: {e}")))?;

        let (tx, rx) = mpsc::channel(256);
        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.blocking_send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(|e| Error::io("starting the terminal reader", e))?;

        Ok(Pty { output: rx, writer, master: pair.master, child })
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes).and_then(|_| self.writer.flush()).map_err(|e| Error::io("writing to the terminal", e))
    }

    pub fn resize(&self, rows: u16, cols: u16) {
        // A failed resize only affects line wrapping; nothing to recover.
        let _ = self.master.resize(size(rows, cols));
    }

    /// Exit code if the child has finished (signal deaths report 1).
    pub fn try_exit_code(&mut self) -> Option<u32> {
        self.child.try_wait().ok().flatten().map(|s| s.exit_code())
    }

    /// Waits for the child to exit.
    pub async fn wait(&mut self) -> u32 {
        loop {
            if let Some(code) = self.try_exit_code() {
                return code;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn collect(pty: &mut Pty) -> String {
        let mut text = Vec::new();
        while let Some(chunk) = pty.output.recv().await {
            text.extend(chunk);
        }
        String::from_utf8_lossy(&text).into_owned()
    }

    #[tokio::test]
    async fn child_sees_a_terminal_and_exit_code_is_reported() {
        let args = vec!["-c".into(), "[ -t 1 ] && echo tty; exit 7".into()];
        let mut pty = Pty::spawn(Path::new("/bin/sh"), &args, 24, 80).unwrap();
        let text = collect(&mut pty).await;
        assert!(text.contains("tty"), "{text:?}");
        assert_eq!(pty.wait().await, 7);
    }

    #[tokio::test]
    async fn input_reaches_the_child_and_resize_is_accepted() {
        let args = vec!["-c".into(), "read line; echo got:$line".into()];
        let mut pty = Pty::spawn(Path::new("/bin/sh"), &args, 24, 80).unwrap();
        pty.resize(40, 100);
        pty.write(b"hello\n").unwrap();
        let text = collect(&mut pty).await;
        assert!(text.contains("got:hello"), "{text:?}");
    }

    #[tokio::test]
    async fn dropping_kills_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late");
        let args = vec!["-c".into(), format!("sleep 1; touch {}", marker.display()).into()];
        let pty = Pty::spawn(Path::new("/bin/sh"), &args, 24, 80).unwrap();
        drop(pty);
        tokio::time::sleep(Duration::from_millis(1400)).await;
        assert!(!marker.exists());
    }

    #[test]
    fn missing_program_is_an_error() {
        assert!(Pty::spawn(Path::new("/nonexistent/prog"), &[], 24, 80).is_err());
    }
}

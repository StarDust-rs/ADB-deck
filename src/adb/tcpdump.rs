//! On-device packet capture with `tcpdump`.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use super::{sh_quote, DeviceAdb};
use crate::error::{Error, Result};
use crate::process::{spawn_lines, LineStream};

const REMOTE_DIR: &str = "/data/local/tmp";

/// Counts packets in a pcap file; a truncated last record is ignored.
pub fn count_pcap_packets(path: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(path).map_err(|e| Error::io(format!("opening {}", path.display()), e))?;
    let len = file.metadata().map_err(|e| Error::io("reading capture size", e))?.len();
    let mut header = [0u8; 24];
    file.read_exact(&mut header).map_err(|_| Error::parse("capture file", "file is too short to be a pcap"))?;
    let big_endian = match u32::from_le_bytes([header[0], header[1], header[2], header[3]]) {
        0xa1b2_c3d4 | 0xa1b2_3c4d => false,
        0xd4c3_b2a1 | 0x4d3c_b2a1 => true,
        other => return Err(Error::parse("capture file", format!("unknown pcap magic {other:#x}"))),
    };
    let mut pos = 24u64;
    let mut count = 0u64;
    let mut rec = [0u8; 16];
    while pos + 16 <= len {
        file.read_exact(&mut rec).map_err(|e| Error::io("reading capture", e))?;
        let raw = [rec[8], rec[9], rec[10], rec[11]];
        let incl = u64::from(if big_endian { u32::from_be_bytes(raw) } else { u32::from_le_bytes(raw) });
        if pos + 16 + incl > len {
            break;
        }
        pos += 16 + incl;
        count += 1;
        file.seek(SeekFrom::Start(pos)).map_err(|e| Error::io("reading capture", e))?;
    }
    Ok(count)
}

/// Splits a user BPF expression into shell-quoted words.
pub fn quote_filter(filter: &str) -> Result<String> {
    let f = filter.trim();
    if f.len() > 512 || f.chars().any(char::is_control) {
        return Err(Error::Invalid("The capture filter is too long or contains control characters".into()));
    }
    Ok(f.split_whitespace().map(sh_quote).collect::<Vec<_>>().join(" "))
}

pub fn remote_file_name(id: u64) -> String {
    format!("{REMOTE_DIR}/adbdeck_capture_{id}.pcap")
}

/// A running capture. Dropping it ends the adb session; call
/// [`DeviceAdb::stop_capture`] first to stop tcpdump on the device.
pub struct Capture {
    pub pid: u32,
    pub remote_path: String,
    pub output: LineStream,
}

impl DeviceAdb {
    pub async fn start_capture(&self, id: u64, filter: &str) -> Result<Capture> {
        let remote_path = remote_file_name(id);
        let filter = quote_filter(filter)?;
        let inner = format!("echo $$; exec tcpdump -i any -p -s 0 -w {} {filter}", sh_quote(&remote_path));
        let script = format!("sh -c {}", sh_quote(&inner));
        let spec = self.spec().arg("shell").arg(script).timeout(Duration::from_secs(u32::MAX as u64));
        let mut output = spawn_lines(&spec, 64, true)?;

        let first = tokio::time::timeout(Duration::from_secs(10), output.lines.recv()).await;
        let pid = match first {
            Ok(Some(line)) => line.trim().parse::<u32>().map_err(|_| capture_error(&line))?,
            _ => return Err(Error::Invalid("tcpdump did not start on the device".into())),
        };
        // tcpdump reports a missing binary or denied permission right away.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let mut messages = Vec::new();
        while let Ok(line) = output.lines.try_recv() {
            messages.push(line);
        }
        if let Some(bad) = messages.iter().find(|l| is_start_failure(l)) {
            return Err(capture_error(bad));
        }
        if output.is_finished() {
            return Err(capture_error(&messages.join("\n")));
        }
        Ok(Capture { pid, remote_path, output })
    }

    /// Asks tcpdump to finish (SIGINT) so the file is complete.
    pub async fn stop_capture(&self, capture: &Capture) -> Result<()> {
        self.shell(&format!("kill -INT {}", capture.pid)).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    pub async fn remote_size(&self, path: &str) -> Option<u64> {
        self.shell(&format!("stat -c %s {}", sh_quote(path))).await.ok()?.trim().parse().ok()
    }

    pub async fn remove_remote(&self, path: &str) {
        // Cleanup is best effort; a leftover file in /data/local/tmp is harmless.
        let _ = self.shell(&format!("rm -f {}", sh_quote(path))).await;
    }
}

fn is_start_failure(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("not found") || l.contains("permission denied") || l.contains("operation not permitted") || l.contains("no such device")
}

fn capture_error(detail: &str) -> Error {
    let d = detail.trim();
    let l = d.to_lowercase();
    if l.contains("not found") {
        Error::Invalid("tcpdump is not installed on this device. Packet capture needs a rootable device with tcpdump.".into())
    } else if l.contains("permission denied") || l.contains("not permitted") {
        Error::Invalid("tcpdump was denied permission. Enable root mode first and try again.".into())
    } else if d.is_empty() {
        Error::Invalid("tcpdump stopped immediately".into())
    } else {
        Error::Invalid(format!("tcpdump failed: {d}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pcap(big: bool, packets: &[usize], truncate_by: usize) -> Vec<u8> {
        let mut v = Vec::new();
        let w32 = |v: &mut Vec<u8>, n: u32| v.extend(if big { n.to_be_bytes() } else { n.to_le_bytes() });
        v.extend(if big { 0xd4c3b2a1u32.to_le_bytes() } else { 0xa1b2c3d4u32.to_le_bytes() });
        v.extend([0u8; 20]);
        for &len in packets {
            w32(&mut v, 0);
            w32(&mut v, 0);
            w32(&mut v, len as u32);
            w32(&mut v, len as u32);
            v.extend(vec![0u8; len]);
        }
        v.truncate(v.len() - truncate_by);
        v
    }

    fn write(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f
    }

    #[test]
    fn counts_packets_in_both_byte_orders() {
        assert_eq!(count_pcap_packets(write(&pcap(false, &[60, 1500, 0, 42], 0)).path()).unwrap(), 4);
        assert_eq!(count_pcap_packets(write(&pcap(true, &[60, 100], 0)).path()).unwrap(), 2);
        assert_eq!(count_pcap_packets(write(&pcap(false, &[], 0)).path()).unwrap(), 0);
    }

    #[test]
    fn a_truncated_last_record_is_not_counted() {
        assert_eq!(count_pcap_packets(write(&pcap(false, &[60, 100], 10)).path()).unwrap(), 1);
    }

    #[test]
    fn bad_files_are_errors() {
        assert!(count_pcap_packets(write(b"short").path()).is_err());
        assert!(count_pcap_packets(write(&[0u8; 64]).path()).is_err());
        assert!(count_pcap_packets(Path::new("/nonexistent/file.pcap")).is_err());
    }

    #[test]
    fn filters_are_quoted_per_word() {
        assert_eq!(quote_filter("port 80 and host 1.2.3.4").unwrap(), "'port' '80' 'and' 'host' '1.2.3.4'");
        assert_eq!(quote_filter("  ").unwrap(), "");
        assert_eq!(quote_filter("a'; reboot; '").unwrap(), "'a'\\'';' 'reboot;' ''\\'''");
        assert!(quote_filter("a\nb").is_err());
        assert!(quote_filter(&"x".repeat(600)).is_err());
    }

    #[test]
    fn start_failures_are_explained() {
        assert!(capture_error("/system/bin/sh: tcpdump: not found").to_string().contains("not installed"));
        assert!(capture_error("tcpdump: any: You don't have permission to capture. Permission denied").to_string().contains("root"));
        assert!(is_start_failure("sh: tcpdump: inaccessible or not found"));
        assert!(!is_start_failure("tcpdump: listening on any"));
    }
}

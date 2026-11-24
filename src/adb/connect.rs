//! Wi-Fi debugging: connect, disconnect, pair, switch a USB device to TCP/IP.

use std::time::Duration;

use super::overview::parse_ip_mac;
use super::Adb;
use crate::error::{Error, Result};
use crate::process::run;

fn valid_host(host: &str) -> bool {
    host.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && host.len() <= 253
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// Validates and joins a host with an optional port.
pub fn endpoint(host: &str, port: Option<u16>) -> Result<String> {
    let host = host.trim();
    if !valid_host(host) {
        return Err(Error::Invalid("Enter a valid IP address or host name".into()));
    }
    Ok(match port {
        Some(p) if p > 0 => format!("{host}:{p}"),
        Some(_) => return Err(Error::Invalid("The port must be between 1 and 65535".into())),
        None => host.to_string(),
    })
}

pub fn parse_port(text: &str) -> Result<Option<u16>> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    t.parse::<u16>().ok().filter(|p| *p > 0).map(Some).ok_or_else(|| Error::Invalid("The port must be between 1 and 65535".into()))
}

/// adb reports connect failures on stdout with exit status 0.
fn connection_succeeded(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    (t.contains("connected to") || t.contains("already connected")) && !t.contains("failed")
}

/// `Successfully paired to 192.168.1.20:41999 [guid=adb-R58M1234-AbCdEf]`: the guid.
fn parse_guid(text: &str) -> Option<String> {
    let rest = text.split_once("[guid=")?.1;
    let guid = rest.split_once(']')?.0.trim();
    (!guid.is_empty()).then(|| guid.to_string())
}

impl Adb {
    pub async fn connect(&self, host: &str, port: Option<u16>) -> Result<()> {
        let target = endpoint(host, port)?;
        let out = self.global(["connect", target.as_str()]).await?;
        let text = format!("{}{}", out.stdout_text(), out.stderr);
        if connection_succeeded(&text) {
            Ok(())
        } else {
            Err(Error::Invalid(text.trim().to_string()))
        }
    }

    pub async fn disconnect(&self, host: &str, port: Option<u16>) -> Result<()> {
        let target = endpoint(host, port)?;
        let out = self.global(["disconnect", target.as_str()]).await?;
        if out.success() {
            Ok(())
        } else {
            Err(Error::Invalid(out.failure_message()))
        }
    }

    /// Pairs with a device showing a Wi-Fi pairing code. The code goes to
    /// stdin so it never appears in the process list.
    pub async fn pair(&self, host: &str, port: u16, code: &str) -> Result<()> {
        let code = code.trim();
        if code.len() < 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::Invalid("The pairing code is six digits".into()));
        }
        self.pair_with_secret(host, port, code).await.map(|_| ())
    }

    /// Pairs with a password instead of the six-digit code (a QR code carries one). Returns the
    /// phone's guid, which names its `_adb-tls-connect` service, when adb printed it.
    pub(super) async fn pair_with_secret(&self, host: &str, port: u16, secret: &str) -> Result<Option<String>> {
        let target = endpoint(host, Some(port))?;
        if secret.is_empty() || !secret.chars().all(|c| c.is_ascii_graphic()) {
            return Err(Error::Invalid("The pairing password is empty or has unusual characters".into()));
        }
        let spec = self.spec().arg("pair").arg(target).stdin(format!("{secret}\n").into_bytes()).timeout(Duration::from_secs(30));
        let out = run(&spec).await?;
        let text = format!("{}{}", out.stdout_text(), out.stderr);
        if text.contains("Successfully paired") {
            Ok(parse_guid(&text))
        } else {
            Err(Error::Invalid(format!("Pairing failed: {}", text.trim())))
        }
    }

    /// Switches a USB-connected device to TCP/IP and connects to it.
    pub async fn enable_wireless(&self, serial: &str) -> Result<String> {
        let dev = self.device(serial);
        let wlan = dev.shell("ip addr show wlan0").await?;
        let (ip, _) = parse_ip_mac(&wlan);
        if ip.is_empty() {
            return Err(Error::Invalid("The device has no Wi-Fi address. Connect it to a Wi-Fi network first.".into()));
        }
        let out = dev.exec(["tcpip", "5555"]).await?;
        if !out.success() {
            return Err(Error::Invalid(out.failure_message()));
        }
        // adbd restarts; give it a moment before connecting.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        self.connect(&ip, Some(5555)).await?;
        Ok(format!("{ip}:5555"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_validation() {
        assert_eq!(endpoint(" 192.168.1.5 ", Some(5555)).unwrap(), "192.168.1.5:5555");
        assert_eq!(endpoint("phone.local", None).unwrap(), "phone.local");
        for bad in ["", "a b", "x;y", "-o", "-host", "host:80"] {
            assert!(endpoint(bad, None).is_err(), "{bad}");
        }
        assert!(endpoint("host", Some(0)).is_err());
    }

    #[test]
    fn port_parsing() {
        assert_eq!(parse_port("").unwrap(), None);
        assert_eq!(parse_port(" 5555 ").unwrap(), Some(5555));
        assert!(parse_port("0").is_err());
        assert!(parse_port("70000").is_err());
        assert!(parse_port("abc").is_err());
    }

    #[test]
    fn the_guid_is_read_from_the_pairing_output() {
        assert_eq!(parse_guid("Successfully paired to 192.168.1.20:41999 [guid=adb-R58M1234-AbCdEf]\n").as_deref(), Some("adb-R58M1234-AbCdEf"));
        assert_eq!(parse_guid("Successfully paired to 192.168.1.20:41999\n"), None);
        assert_eq!(parse_guid("[guid=]"), None);
    }

    #[test]
    fn connect_output_interpretation() {
        assert!(connection_succeeded("connected to 192.168.1.5:5555\n"));
        assert!(connection_succeeded("already connected to 192.168.1.5:5555"));
        assert!(!connection_succeeded("failed to connect to '192.168.1.5:5555': Connection refused"));
        assert!(!connection_succeeded("cannot connect to 1.2.3.4:5555: timed out"));
    }
}

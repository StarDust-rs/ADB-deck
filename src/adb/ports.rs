//! Port forwarding and reversing.

use super::DeviceAdb;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
    /// Host-side endpoint (`tcp:8080`).
    pub local: String,
    /// Device-side endpoint.
    pub remote: String,
}

/// Bare port numbers become `tcp:<n>`; other socket specs pass through.
pub fn normalize_endpoint(input: &str) -> Result<String> {
    let t = input.trim();
    if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
        return match t.parse::<u16>() {
            Ok(p) if p > 0 => Ok(format!("tcp:{p}")),
            _ => Err(Error::Invalid(format!("{t} is not a valid port"))),
        };
    }
    let valid = t.split_once(':').is_some_and(|(kind, rest)| {
        !kind.is_empty()
            && kind.chars().all(|c| c.is_ascii_lowercase())
            && !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || "_./-@".contains(c))
    });
    if valid {
        Ok(t.to_string())
    } else {
        Err(Error::Invalid(format!("'{t}' is not a valid endpoint. Use a port number or a spec such as tcp:8080 or localabstract:name")))
    }
}

/// `adb forward --list`: `<serial> <local> <remote>` per line, all devices.
pub fn parse_forward_list(text: &str, serial: &str) -> Vec<PortMapping> {
    text.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            (p.len() == 3 && p[0] == serial).then(|| PortMapping { local: p[1].into(), remote: p[2].into() })
        })
        .collect()
}

/// `adb reverse --list`: `<name> <device endpoint> <host endpoint>`.
pub fn parse_reverse_list(text: &str) -> Vec<PortMapping> {
    text.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            (p.len() == 3).then(|| PortMapping { remote: p[1].into(), local: p[2].into() })
        })
        .collect()
}

impl DeviceAdb {
    pub async fn list_forwards(&self) -> Result<Vec<PortMapping>> {
        let out = self.adb().global(["forward", "--list"]).await?;
        Ok(parse_forward_list(&out.stdout_text(), self.serial()))
    }

    pub async fn list_reverses(&self) -> Result<Vec<PortMapping>> {
        let out = self.exec(["reverse", "--list"]).await?;
        Ok(parse_reverse_list(&out.stdout_text()))
    }

    pub async fn forward(&self, local: &str, remote: &str) -> Result<()> {
        let (l, r) = (normalize_endpoint(local)?, normalize_endpoint(remote)?);
        self.checked(["forward", l.as_str(), r.as_str()]).await
    }

    /// `adb forward tcp:0 <remote>`: lets adb pick a free host port and returns it.
    pub async fn forward_dynamic(&self, remote: &str) -> Result<u16> {
        let r = normalize_endpoint(remote)?;
        let out = self.exec(["forward", "tcp:0", r.as_str()]).await?;
        if !out.success() {
            return Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() });
        }
        out.stdout_text()
            .trim()
            .parse::<u16>()
            .map_err(|_| Error::parse("adb forward", format!("no port in {:?}", out.stdout_text().trim())))
    }

    /// Makes `remote` (device side) connect to `local` (host side).
    pub async fn reverse(&self, remote: &str, local: &str) -> Result<()> {
        let (r, l) = (normalize_endpoint(remote)?, normalize_endpoint(local)?);
        self.checked(["reverse", r.as_str(), l.as_str()]).await
    }

    pub async fn remove_forward_endpoint(&self, local: &str) -> Result<()> {
        let l = normalize_endpoint(local)?;
        self.checked(["forward", "--remove", l.as_str()]).await
    }

    pub async fn remove_reverse(&self, remote: &str) -> Result<()> {
        let r = normalize_endpoint(remote)?;
        self.checked(["reverse", "--remove", r.as_str()]).await
    }

    async fn checked<const N: usize>(&self, args: [&str; N]) -> Result<()> {
        let out = self.exec(args).await?;
        if out.success() {
            Ok(())
        } else {
            Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_normalised_and_validated() {
        assert_eq!(normalize_endpoint(" 8080 ").unwrap(), "tcp:8080");
        assert_eq!(normalize_endpoint("tcp:9000").unwrap(), "tcp:9000");
        assert_eq!(normalize_endpoint("localabstract:chrome_devtools_remote").unwrap(), "localabstract:chrome_devtools_remote");
        for bad in ["", "0", "70000", "tcp", "tcp:", "TCP:80", "tcp:80;reboot", "a b"] {
            assert!(normalize_endpoint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn lists_are_filtered_and_oriented() {
        let f = parse_forward_list("abc tcp:1 tcp:2\nother tcp:3 tcp:4\nbad line\n", "abc");
        assert_eq!(f, vec![PortMapping { local: "tcp:1".into(), remote: "tcp:2".into() }]);
        let r = parse_reverse_list("UsbFfs tcp:8081 tcp:9091\n");
        assert_eq!(r, vec![PortMapping { local: "tcp:9091".into(), remote: "tcp:8081".into() }]);
    }
}

//! Finding phones on the local network that have Wireless debugging on.
//!
//! Android 11+ advertises `_adb-tls-connect._tcp` over mDNS, with a port that
//! changes whenever Wireless debugging is switched off and on, so a stored
//! `ip:port` goes stale. adb's own mDNS listener reports what is advertised
//! right now (`adb mdns services`). Only the TLS service is used: a phone
//! advertises it only for hosts it was paired with, so connecting never asks
//! the user to approve anything on the phone.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::time::Instant;

use super::devices::is_remote_id;
use super::Adb;
use crate::error::{Error, Result};
use crate::process::run;

const MDNS_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_SERVICE: &str = "_adb-tls-connect._tcp";
const PAIRING_SERVICE: &str = "_adb-tls-pairing._tcp";

/// A phone offering a connection right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// mDNS instance name, `adb-<serialno>-<suffix>`.
    pub instance: String,
    /// `ip:port` to hand to `adb connect`.
    pub endpoint: String,
}

impl Discovered {
    /// The phone's serial number as embedded in the instance name.
    fn serialno(&self) -> Option<&str> {
        self.instance.strip_prefix("adb-")?.rsplit_once('-').map(|(serial, _)| serial)
    }
}

/// Reads `adb mdns services`: tab- or space-separated `instance type ip:port`
/// rows after a header. Rows for other services, IPv6 addresses and anything
/// that does not parse are skipped.
fn parse_services(text: &str, service: &str) -> Vec<Discovered> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (instance, kind, endpoint) = (parts.next()?, parts.next()?, parts.next()?);
            (kind.trim_end_matches('.') == service && is_remote_id(endpoint))
                .then(|| Discovered { instance: instance.to_string(), endpoint: endpoint.to_string() })
        })
        .collect()
}

/// The phones offering a connection (`_adb-tls-connect`).
pub fn parse_mdns_services(text: &str) -> Vec<Discovered> {
    parse_services(text, CONNECT_SERVICE)
}

/// The phones waiting to be paired (`_adb-tls-pairing`). After a QR code was scanned the
/// instance name is the one the code carried.
pub fn parse_pairing_services(text: &str) -> Vec<Discovered> {
    parse_services(text, PAIRING_SERVICE)
}

impl Adb {
    /// What adb's mDNS listener currently sees, as printed by `adb mdns services`.
    pub(super) async fn mdns_services(&self) -> Result<String> {
        // Right after boot adb's mDNS backend can take long to answer; the scan must not wait for it.
        let out = run(&self.spec().args(["mdns", "services"]).timeout(MDNS_TIMEOUT)).await?;
        if !out.success() {
            return Err(Error::Exit { program: "adb".into(), code: out.code, message: out.failure_message() });
        }
        Ok(out.stdout_text())
    }

    /// Phones that currently advertise a Wireless debugging connection.
    pub async fn discover(&self) -> Result<Vec<Discovered>> {
        Ok(parse_mdns_services(&self.mdns_services().await?))
    }
}

/// Decides which discovered phones to connect to. Pure: the caller supplies the
/// device list, the clock and the devices the user disconnected on purpose.
pub struct Discoverer {
    cooldown: Duration,
    last_try: HashMap<String, Instant>,
}

impl Discoverer {
    /// `cooldown`: how long to leave a phone alone after a connect attempt, so one
    /// that refuses (Wireless debugging off for this computer) is not hammered.
    pub fn new(cooldown: Duration) -> Self {
        Self { cooldown, last_try: HashMap::new() }
    }

    /// The phones to connect to now. `listed` holds the serials from `adb devices`;
    /// `blocked` the ids the user disconnected (matched by host, because a
    /// restarted Wireless debugging comes back on a new port).
    pub fn plan(&mut self, found: &[Discovered], listed: &[String], blocked: &HashSet<String>, now: Instant) -> Vec<Discovered> {
        // A phone that stopped advertising is tried at once when it returns.
        self.last_try.retain(|endpoint, _| found.iter().any(|d| &d.endpoint == endpoint));
        let mut plan = Vec::new();
        for d in found {
            let host = d.endpoint.rsplit_once(':').map_or("", |(h, _)| h);
            let connected = listed.iter().any(|id| {
                id == &d.endpoint
                    || id.strip_prefix(d.instance.as_str()).is_some_and(|rest| rest.starts_with('.'))
                    || Some(id.as_str()) == d.serialno()
            });
            let disconnected_by_user = blocked.iter().any(|b| b.rsplit_once(':').is_some_and(|(h, _)| h == host));
            let waiting = self.last_try.get(&d.endpoint).is_some_and(|t| now.duration_since(*t) < self.cooldown);
            if connected || disconnected_by_user || waiting {
                continue;
            }
            self.last_try.insert(d.endpoint.clone(), now);
            plan.push(d.clone());
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "List of discovered mdns services\n\
        adb-R58M1234-AbCdEf\t_adb-tls-connect._tcp.\t192.168.1.20:37123\n\
        adb-R58M1234-AbCdEf\t_adb-tls-pairing._tcp.\t192.168.1.20:41999\n\
        adb-OTHER99-xYz123 _adb-tls-connect._tcp 192.168.1.31:40001\n\
        adb-V6-QrStUv\t_adb-tls-connect._tcp.\t[fe80::1]:40002\n\
        garbage line\n";

    fn found(instance: &str, endpoint: &str) -> Discovered {
        Discovered { instance: instance.into(), endpoint: endpoint.into() }
    }

    #[test]
    fn only_ipv4_connect_services_are_read() {
        assert_eq!(
            parse_mdns_services(OUT),
            vec![found("adb-R58M1234-AbCdEf", "192.168.1.20:37123"), found("adb-OTHER99-xYz123", "192.168.1.31:40001")]
        );
        assert!(parse_mdns_services("").is_empty());
        assert!(parse_mdns_services("ERROR: mdns discovery unavailable\n").is_empty());
    }

    #[test]
    fn only_ipv4_pairing_services_are_read() {
        assert_eq!(parse_pairing_services(OUT), vec![found("adb-R58M1234-AbCdEf", "192.168.1.20:41999")]);
        let qr = "adbdeck-k3x9q2ab\t_adb-tls-pairing._tcp.\t192.168.1.20:44111\n";
        assert_eq!(parse_pairing_services(qr), vec![found("adbdeck-k3x9q2ab", "192.168.1.20:44111")]);
        assert!(parse_pairing_services("").is_empty());
    }

    #[test]
    fn serial_is_taken_from_the_instance_name() {
        assert_eq!(found("adb-R58M1234-AbCdEf", "1.1.1.1:1").serialno(), Some("R58M1234"));
        assert_eq!(found("adb-A-B-xyz", "1.1.1.1:1").serialno(), Some("A-B"));
        assert_eq!(found("printer", "1.1.1.1:1").serialno(), None);
    }

    fn phone() -> Discovered {
        found("adb-R58M1234-AbCdEf", "192.168.1.20:37123")
    }

    fn plan(d: &mut Discoverer, listed: &[&str], blocked: &[&str], now: Instant) -> Vec<Discovered> {
        let listed: Vec<String> = listed.iter().map(|s| s.to_string()).collect();
        let blocked = blocked.iter().map(|s| s.to_string()).collect();
        d.plan(&[phone()], &listed, &blocked, now)
    }

    #[test]
    fn a_new_phone_is_connected_once_per_cooldown() {
        let t0 = Instant::now();
        let mut d = Discoverer::new(Duration::from_secs(60));
        assert_eq!(plan(&mut d, &[], &[], t0), vec![phone()]);
        assert!(plan(&mut d, &[], &[], t0 + Duration::from_secs(59)).is_empty(), "still cooling down");
        assert_eq!(plan(&mut d, &[], &[], t0 + Duration::from_secs(60)), vec![phone()]);
    }

    #[test]
    fn a_phone_that_went_away_is_tried_at_once_when_it_returns() {
        let t0 = Instant::now();
        let mut d = Discoverer::new(Duration::from_secs(60));
        assert_eq!(plan(&mut d, &[], &[], t0).len(), 1);
        assert!(d.plan(&[], &[], &HashSet::new(), t0 + Duration::from_secs(5)).is_empty());
        assert_eq!(plan(&mut d, &[], &[], t0 + Duration::from_secs(10)).len(), 1);
    }

    #[test]
    fn phones_that_are_already_connected_are_left_alone() {
        let t0 = Instant::now();
        let mut d = Discoverer::new(Duration::from_secs(60));
        assert!(plan(&mut d, &["192.168.1.20:37123"], &[], t0).is_empty(), "same ip:port");
        assert!(plan(&mut d, &["adb-R58M1234-AbCdEf._adb-tls-connect._tcp"], &[], t0).is_empty(), "adb connected it itself");
        assert!(plan(&mut d, &["R58M1234"], &[], t0).is_empty(), "the same phone is plugged in");
        assert_eq!(plan(&mut d, &["192.168.1.20:5555"], &[], t0).len(), 1, "an old port does not count");
    }

    #[test]
    fn a_phone_the_user_disconnected_is_not_found_again_on_a_new_port() {
        let t0 = Instant::now();
        let mut d = Discoverer::new(Duration::from_secs(60));
        assert!(plan(&mut d, &[], &["192.168.1.20:40555"], t0).is_empty());
        assert_eq!(plan(&mut d, &[], &["192.168.1.99:40555"], t0).len(), 1, "another host is unaffected");
    }
}

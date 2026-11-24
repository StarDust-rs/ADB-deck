//! Connecting a phone by scanning a QR code (Android 11+: Developer options › Wireless
//! debugging › Pair device with QR code).
//!
//! The computer shows `WIFI:T:ADB;S:<name>;P:<password>;;`. Once the phone has read it, it
//! advertises `_adb-tls-pairing._tcp` under the instance name `<name>` and accepts `adb pair`
//! with `<password>`. So the flow is: wait for that service, pair, wait for the phone's
//! `_adb-tls-connect._tcp` service, connect. Nothing is advertised by this computer.

use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::{sleep, timeout, Instant};

use super::discover::{parse_mdns_services, parse_pairing_services, Discovered, Discoverer};
use super::Adb;
use crate::error::{Error, Result};

const NAME_PREFIX: &str = "adbdeck-";
const NAME_LEN: usize = 8;
const SECRET_LEN: usize = 12;
const LOWER_ALNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const PAIR_ATTEMPTS: u32 = 3;

/// One QR code: the service name the phone will advertise and the password it will accept.
/// Both are random and used for a single attempt.
#[derive(Clone)]
pub struct QrSession {
    name: String,
    secret: String,
}

// The password is a credential while the code is on screen, so it stays out of logs.
impl fmt::Debug for QrSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QrSession").field("name", &self.name).finish_non_exhaustive()
    }
}

impl QrSession {
    pub fn new() -> Result<Self> {
        Ok(Self { name: format!("{NAME_PREFIX}{}", random_string(NAME_LEN, LOWER_ALNUM)?), secret: random_string(SECRET_LEN, ALNUM)? })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The text to encode in the QR code. Both parts use only letters and digits, so nothing
    /// in it needs the escaping the `WIFI:` format asks for.
    pub fn payload(&self) -> String {
        format!("WIFI:T:ADB;S:{};P:{};;", self.name, self.secret)
    }
}

/// Random text from `alphabet`, read from the kernel. There is no weaker fallback: a guessable
/// password would defeat the pairing.
fn random_string(len: usize, alphabet: &[u8]) -> Result<String> {
    let mut urandom = File::open("/dev/urandom").map_err(|e| Error::io("Cannot read random bytes for the QR code", e))?;
    // Bytes at or above this would favour the first characters of the alphabet.
    let limit = 256 - 256 % alphabet.len();
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 32];
    while out.len() < len {
        urandom.read_exact(&mut buf).map_err(|e| Error::io("Cannot read random bytes for the QR code", e))?;
        let want = len - out.len();
        out.extend(
            buf.iter().filter(|b| usize::from(**b) < limit).take(want).map(|b| char::from(alphabet[usize::from(*b) % alphabet.len()])),
        );
    }
    Ok(out)
}

/// Where a QR connection is, for the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrStage {
    /// The code is shown; nothing has scanned it yet.
    Waiting,
    Pairing,
    Connecting,
}

#[derive(Debug, Clone, Copy)]
pub struct QrTiming {
    /// How long the code stays valid.
    pub lifetime: Duration,
    /// Pause between looks at the network.
    pub poll: Duration,
    /// How long a paired phone gets to offer its connection.
    pub connect_wait: Duration,
}

impl Default for QrTiming {
    fn default() -> Self {
        Self { lifetime: Duration::from_secs(180), poll: Duration::from_secs(1), connect_wait: Duration::from_secs(30) }
    }
}

/// The pairing service that belongs to this code (instance names are not case sensitive).
fn waiting_for<'a>(found: &'a [Discovered], session: &QrSession) -> Option<&'a Discovered> {
    found.iter().find(|d| d.instance.eq_ignore_ascii_case(session.name()))
}

fn split_endpoint(endpoint: &str) -> Option<(&str, u16)> {
    let (host, port) = endpoint.rsplit_once(':')?;
    Some((host, port.parse().ok()?))
}

/// True when a connect service is the paired phone's: it is named by the guid adb printed, or it
/// sits at the address that was paired.
fn offered_by(d: &Discovered, host: &str, guid: Option<&str>) -> bool {
    guid.is_some_and(|g| d.instance == g) || split_endpoint(&d.endpoint).is_some_and(|(h, _)| h == host)
}

impl Adb {
    /// Pairs with the phone that scans `session`'s code and connects to it; returns the `ip:port`
    /// it was connected at. Gives up when the code's lifetime is over. Dropping the future (the
    /// user cancels) stops everything, including a running `adb` child.
    pub async fn connect_with_qr(&self, session: &QrSession, timing: QrTiming, stage: &watch::Sender<QrStage>) -> Result<String> {
        stage.send_replace(QrStage::Waiting);
        let phone = self.wait_for_scan(session, timing).await?;
        let (host, port) = split_endpoint(&phone.endpoint).ok_or_else(|| Error::parse("adb mdns services", phone.endpoint.clone()))?;

        stage.send_replace(QrStage::Pairing);
        let guid = self.pair_retrying(host, port, &session.secret, timing.poll).await?;

        stage.send_replace(QrStage::Connecting);
        self.connect_paired(host, guid.as_deref(), timing).await
    }

    async fn wait_for_scan(&self, session: &QrSession, timing: QrTiming) -> Result<Discovered> {
        let mut last_error = None;
        let scan = async {
            loop {
                match self.mdns_services().await {
                    Ok(text) => {
                        if let Some(d) = waiting_for(&parse_pairing_services(&text), session) {
                            return d.clone();
                        }
                    }
                    // A hiccup of adb's mDNS listener must not end the wait.
                    Err(e) => last_error = Some(e.to_string()),
                }
                sleep(timing.poll).await;
            }
        };
        match timeout(timing.lifetime, scan).await {
            Ok(phone) => Ok(phone),
            Err(_) => Err(Error::Invalid(with_cause(
                "The code was not scanned in time. Check that the phone is on the same network and that Wireless debugging is on, then show a new code.",
                last_error,
            ))),
        }
    }

    /// `adb pair` can fail once for network reasons while the phone is still waiting.
    async fn pair_retrying(&self, host: &str, port: u16, secret: &str, pause: Duration) -> Result<Option<String>> {
        let mut attempt = 1;
        loop {
            match self.pair_with_secret(host, port, secret).await {
                Err(_) if attempt < PAIR_ATTEMPTS => {
                    attempt += 1;
                    sleep(pause).await;
                }
                result => return result,
            }
        }
    }

    /// Waits for the paired phone's connection service and connects to it.
    async fn connect_paired(&self, host: &str, guid: Option<&str>, timing: QrTiming) -> Result<String> {
        let mut last_error = None;
        let attempts = async {
            loop {
                match self.try_connect_paired(host, guid).await {
                    Ok(Some(endpoint)) => return endpoint,
                    // Not advertised yet; the phone needs a moment after pairing.
                    Ok(None) => {}
                    Err(e) => last_error = Some(e.to_string()),
                }
                sleep(timing.poll).await;
            }
        };
        match timeout(timing.connect_wait, attempts).await {
            Ok(endpoint) => Ok(endpoint),
            Err(_) => Err(Error::Invalid(with_cause(
                "The phone is paired, but it did not offer a connection in time. Use Connect over Wi-Fi with the address shown under Wireless debugging.",
                last_error,
            ))),
        }
    }

    async fn try_connect_paired(&self, host: &str, guid: Option<&str>) -> Result<Option<String>> {
        let offered: Vec<Discovered> =
            parse_mdns_services(&self.mdns_services().await?).into_iter().filter(|d| offered_by(d, host, guid)).collect();
        let Some(first) = offered.first() else { return Ok(None) };
        let listed: Vec<String> = self.list_devices().await?.into_iter().map(|d| d.serial).collect();
        // The planner knows when adb already has the phone (it connects paired phones itself).
        match Discoverer::new(Duration::ZERO).plan(&offered, &listed, &HashSet::new(), Instant::now()).first() {
            Some(d) => {
                self.connect_id(&d.endpoint).await?;
                Ok(Some(d.endpoint.clone()))
            }
            None => Ok(Some(first.endpoint.clone())),
        }
    }
}

fn with_cause(message: &str, cause: Option<String>) -> String {
    match cause {
        Some(c) => format!("{message} (adb: {c})"),
        None => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(instance: &str, endpoint: &str) -> Discovered {
        Discovered { instance: instance.into(), endpoint: endpoint.into() }
    }

    #[test]
    fn the_payload_is_what_android_reads() {
        let s = QrSession { name: "adbdeck-k3x9q2ab".into(), secret: "Zq81LmTt04Ab".into() };
        assert_eq!(s.payload(), "WIFI:T:ADB;S:adbdeck-k3x9q2ab;P:Zq81LmTt04Ab;;");
    }

    #[test]
    fn every_code_is_new_and_uses_plain_characters() {
        let (a, b) = (QrSession::new().unwrap(), QrSession::new().unwrap());
        assert_ne!((&a.name, &a.secret), (&b.name, &b.secret));
        assert!(a.name.starts_with(NAME_PREFIX) && a.name.len() == NAME_PREFIX.len() + NAME_LEN);
        assert!(a.name.bytes().all(|c| c == b'-' || c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_eq!(a.secret.len(), SECRET_LEN);
        assert!(a.secret.bytes().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn the_password_stays_out_of_debug_output() {
        let s = QrSession { name: "adbdeck-k3x9q2ab".into(), secret: "Zq81LmTt04Ab".into() };
        let text = format!("{s:?}");
        assert!(text.contains("adbdeck-k3x9q2ab") && !text.contains("Zq81LmTt04Ab"), "{text}");
    }

    #[test]
    fn random_text_has_the_length_and_alphabet_asked_for() {
        let digits = random_string(200, b"0123456789").unwrap();
        assert_eq!(digits.len(), 200);
        assert!(digits.bytes().all(|c| c.is_ascii_digit()));
        assert!(random_string(0, ALNUM).unwrap().is_empty());
    }

    #[test]
    fn only_the_phone_that_scanned_this_code_is_paired() {
        let s = QrSession { name: "adbdeck-k3x9q2ab".into(), secret: "x".into() };
        let rows = [
            found("adb-R58M1234-AbCdEf", "192.168.1.20:41999"),
            found("adbdeck-zzzzzzzz", "192.168.1.31:40001"),
            found("ADBDECK-K3X9Q2AB", "192.168.1.40:44111"),
        ];
        assert_eq!(waiting_for(&rows, &s).map(|d| d.endpoint.as_str()), Some("192.168.1.40:44111"));
        assert!(waiting_for(&rows[..2], &s).is_none());
    }

    #[test]
    fn a_connect_service_belongs_to_the_paired_phone_by_guid_or_address() {
        let same_address = found("adb-R58M1234-AbCdEf", "192.168.1.20:37123");
        let other = found("adb-OTHER99-xYz123", "192.168.1.31:40001");
        assert!(offered_by(&same_address, "192.168.1.20", None));
        assert!(!offered_by(&other, "192.168.1.20", None));
        assert!(offered_by(&other, "192.168.1.20", Some("adb-OTHER99-xYz123")), "named by the guid, whatever its address");
        assert!(!offered_by(&other, "192.168.1.2", None), "a host that only starts the same is another host");
    }

    #[test]
    fn endpoints_split_into_host_and_port() {
        assert_eq!(split_endpoint("192.168.1.20:41999"), Some(("192.168.1.20", 41999)));
        assert_eq!(split_endpoint("192.168.1.20"), None);
        assert_eq!(split_endpoint("192.168.1.20:99999"), None);
    }
}

//! Finding a phone that was connected before, without relying on mDNS alone.
//!
//! mDNS (see `discover`) only works while the phone advertises Wireless
//! debugging. A phone in `adb tcpip` mode, one reached over USB tethering or
//! behind a network that drops multicast is found here instead: the addresses it
//! was reached at before, the standard tcpip port on those hosts, and the
//! gateways of this computer's networks (a tethered or hotspot phone is the
//! gateway). Candidates are checked with a plain TCP connect first, so an address
//! that is not there never costs an `adb connect` timeout, and a connection is kept
//! only when the phone's serial number is the one that was remembered.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use tokio::time::Instant;

use super::devices::is_remote_id;
use super::Adb;
use crate::error::Result;
use crate::store::RemoteDevice;

/// The port `adb tcpip` uses unless it is told another one.
pub const DEFAULT_TCPIP_PORT: u16 = 5555;
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);
/// Bounds the work per phone and round, whatever the network looks like.
const MAX_CANDIDATES: usize = 24;
/// `RTF_GATEWAY` in `/proc/net/route`: the route goes through a gateway.
const ROUTE_VIA_GATEWAY: u32 = 0x2;

/// What is needed to look for one remembered phone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KnownDevice {
    /// `ro.serialno`; empty when it was never read.
    pub serialno: String,
    /// `ip:port` it was reached at, most recent first.
    pub endpoints: Vec<String>,
}

impl From<&RemoteDevice> for KnownDevice {
    fn from(r: &RemoteDevice) -> Self {
        let mut endpoints = r.endpoints.clone();
        // Records from before endpoints were kept only have `id`.
        if !r.id.is_empty() && !endpoints.contains(&r.id) {
            endpoints.insert(0, r.id.clone());
        }
        Self { serialno: r.serialno.clone(), endpoints }
    }
}

impl KnownDevice {
    /// A stable name for cooldown bookkeeping.
    fn key(&self) -> Option<&str> {
        if !self.serialno.is_empty() {
            Some(self.serialno.as_str())
        } else {
            self.endpoints.first().map(String::as_str)
        }
    }

    fn hosts(&self) -> Vec<&str> {
        let mut hosts: Vec<&str> = Vec::new();
        for (host, _) in self.endpoints.iter().filter_map(|e| e.rsplit_once(':')) {
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
        hosts
    }
}

/// Gateway addresses from `/proc/net/route`. The address is the raw
/// little-endian word the kernel prints in hex (`812AA8C0` is 192.168.42.129).
pub fn parse_gateways(route_table: &str) -> Vec<String> {
    let mut gateways: Vec<String> = Vec::new();
    for line in route_table.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let (Ok(gateway), Ok(flags)) = (u32::from_str_radix(cols[2], 16), u32::from_str_radix(cols[3], 16)) else { continue };
        if flags & ROUTE_VIA_GATEWAY == 0 || gateway == 0 {
            continue;
        }
        let ip = Ipv4Addr::from(gateway.to_le_bytes()).to_string();
        if !gateways.contains(&ip) {
            gateways.push(ip);
        }
    }
    gateways
}

/// The gateways of this computer's networks right now; empty where the table cannot be read.
pub fn local_gateways() -> Vec<String> {
    std::fs::read_to_string("/proc/net/route").map(|t| parse_gateways(&t)).unwrap_or_default()
}

/// Where to look for a phone, most likely first: the endpoints it was reached at,
/// its hosts on the standard tcpip port, then the gateways (which is where a
/// tethered or hotspot phone sits) on every port the phone has used.
pub fn candidates(known: &KnownDevice, gateways: &[String]) -> Vec<String> {
    let mut ports: Vec<u16> = vec![DEFAULT_TCPIP_PORT];
    for (_, port) in known.endpoints.iter().filter_map(|e| e.rsplit_once(':')) {
        if let Ok(p) = port.parse::<u16>() {
            if !ports.contains(&p) {
                ports.push(p);
            }
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut add = |endpoint: String| {
        if is_remote_id(&endpoint) && !out.contains(&endpoint) {
            out.push(endpoint);
        }
    };
    for endpoint in &known.endpoints {
        add(endpoint.clone());
    }
    for host in known.hosts() {
        add(format!("{host}:{DEFAULT_TCPIP_PORT}"));
    }
    for gateway in gateways {
        for port in &ports {
            add(format!("{gateway}:{port}"));
        }
    }
    out.truncate(MAX_CANDIDATES);
    out
}

fn accepts_connections(endpoint: &str) -> bool {
    match endpoint.parse::<SocketAddr>() {
        Ok(addr) => TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok(),
        Err(_) => false,
    }
}

/// The endpoints that accept a TCP connection right now, in the order given. They
/// are tried in parallel, so a round takes about one timeout however many there are.
pub async fn reachable(endpoints: Vec<String>) -> Vec<String> {
    tokio::task::spawn_blocking(move || {
        std::thread::scope(|scope| {
            let probes: Vec<_> = endpoints.iter().map(|e| scope.spawn(move || accepts_connections(e))).collect();
            probes
                .into_iter()
                .zip(endpoints.iter())
                .filter_map(|(probe, endpoint)| probe.join().unwrap_or(false).then(|| endpoint.clone()))
                .collect::<Vec<String>>()
        })
    })
    .await
    .unwrap_or_default()
}

impl Adb {
    /// Connects to `known` at the first candidate address that answers and turns out
    /// to be the same phone. `Ok(None)`: nothing answered or nothing matched.
    pub async fn reconnect_known(&self, known: &KnownDevice, gateways: &[String]) -> Result<Option<String>> {
        for endpoint in reachable(candidates(known, gateways)).await {
            if let Err(e) = self.connect_id(&endpoint).await {
                tracing::debug!(%endpoint, error = %e, "a known address answered but adb could not connect");
                continue;
            }
            // A phone that is waiting for this computer to be approved cannot tell its serial number yet.
            let serial = self.device(&endpoint).shell("getprop ro.serialno").await.unwrap_or_default();
            if known.serialno.is_empty() || serial.is_empty() || serial == known.serialno {
                return Ok(Some(endpoint));
            }
            tracing::debug!(%endpoint, "another phone answers at a remembered address");
        }
        Ok(None)
    }
}

/// True when `blocked` (ids the user disconnected on purpose) holds one of the
/// phone's hosts. By host, because Wireless debugging comes back on a new port.
fn is_blocked(known: &KnownDevice, blocked: &HashSet<String>) -> bool {
    let hosts = known.hosts();
    blocked.iter().any(|b| b.rsplit_once(':').is_some_and(|(h, _)| hosts.contains(&h)))
}

/// Decides which remembered phones to look for now. Pure: the caller supplies the
/// clock and who is connected, so the schedule is testable without waiting.
pub struct Prober {
    cooldown: Duration,
    last_try: HashMap<String, Instant>,
}

impl Prober {
    /// `cooldown`: how long to leave a phone alone after a round that did not bring it online.
    pub fn new(cooldown: Duration) -> Self {
        Self { cooldown, last_try: HashMap::new() }
    }

    pub fn plan<'a>(
        &mut self,
        known: &'a [KnownDevice],
        is_connected: impl Fn(&KnownDevice) -> bool,
        blocked: &HashSet<String>,
        now: Instant,
    ) -> Vec<&'a KnownDevice> {
        // A phone that was forgotten, or connected since, starts afresh.
        self.last_try.retain(|key, _| known.iter().any(|k| k.key() == Some(key.as_str()) && !is_connected(k)));
        let mut plan = Vec::new();
        for k in known {
            let Some(key) = k.key() else { continue };
            let waiting = self.last_try.get(key).is_some_and(|t| now.duration_since(*t) < self.cooldown);
            if is_connected(k) || is_blocked(k, blocked) || waiting {
                continue;
            }
            self.last_try.insert(key.to_string(), now);
            plan.push(k);
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTES: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        enp3s0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
        enp0s20f0u2\t00000000\t812AA8C0\t0003\t0\t0\t101\t00000000\t0\t0\t0\n\
        enp3s0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
        wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n";

    fn phone(serial: &str, endpoints: &[&str]) -> KnownDevice {
        KnownDevice { serialno: serial.into(), endpoints: endpoints.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn gateways_are_read_from_the_route_table_once_each() {
        assert_eq!(parse_gateways(ROUTES), vec!["192.168.1.1", "192.168.42.129"]);
        assert!(parse_gateways("").is_empty());
        assert!(parse_gateways("Iface\tDestination\tGateway\tFlags\nbroken line\n").is_empty());
    }

    #[test]
    fn candidates_start_with_what_worked_before() {
        let k = phone("R58M1234", &["192.168.1.20:37123", "192.168.1.20:5555"]);
        let gateways = vec!["192.168.42.129".to_string()];
        assert_eq!(
            candidates(&k, &gateways),
            vec![
                "192.168.1.20:37123",
                "192.168.1.20:5555",
                "192.168.42.129:5555",
                "192.168.42.129:37123",
            ]
        );
    }

    #[test]
    fn candidates_ignore_junk_and_stay_bounded() {
        assert!(candidates(&phone("X", &["not-an-endpoint"]), &[]).is_empty());
        let gateways: Vec<String> = (1..=40).map(|i| format!("10.0.0.{i}")).collect();
        assert_eq!(candidates(&phone("X", &["10.9.9.9:5555"]), &gateways).len(), MAX_CANDIDATES);
    }

    #[test]
    fn old_records_without_endpoints_still_give_an_address() {
        let r = RemoteDevice { id: "192.168.1.20:5555".into(), serialno: "R58M1234".into(), ..RemoteDevice::default() };
        assert_eq!(KnownDevice::from(&r), phone("R58M1234", &["192.168.1.20:5555"]));
    }

    #[test]
    fn a_phone_is_looked_for_once_per_cooldown_until_it_is_connected() {
        let t0 = Instant::now();
        let mut p = Prober::new(Duration::from_secs(30));
        let known = vec![phone("A", &["192.168.1.20:5555"]), phone("B", &["192.168.1.31:5555"])];
        let none = HashSet::new();
        let b_online = |k: &KnownDevice| k.serialno == "B";
        let plan = p.plan(&known, b_online, &none, t0);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].serialno, "A", "connected phones are left alone");
        assert!(p.plan(&known, b_online, &none, t0 + Duration::from_secs(29)).is_empty(), "still cooling down");
        assert_eq!(p.plan(&known, b_online, &none, t0 + Duration::from_secs(30)).len(), 1);
    }

    #[test]
    fn a_phone_that_dropped_is_looked_for_again_at_once() {
        let t0 = Instant::now();
        let mut p = Prober::new(Duration::from_secs(30));
        let known = vec![phone("A", &["192.168.1.20:5555"])];
        let none = HashSet::new();
        assert_eq!(p.plan(&known, |_| false, &none, t0).len(), 1);
        assert!(p.plan(&known, |_| true, &none, t0 + Duration::from_secs(1)).is_empty());
        assert_eq!(p.plan(&known, |_| false, &none, t0 + Duration::from_secs(2)).len(), 1, "it was connected in between");
    }

    #[test]
    fn a_phone_the_user_disconnected_is_not_looked_for() {
        let mut p = Prober::new(Duration::from_secs(30));
        let known = vec![phone("A", &["192.168.1.20:37123"])];
        let blocked: HashSet<String> = ["192.168.1.20:40555".to_string()].into();
        assert!(p.plan(&known, |_| false, &blocked, Instant::now()).is_empty(), "same host, new port");
        let other: HashSet<String> = ["192.168.1.99:40555".to_string()].into();
        assert_eq!(p.plan(&known, |_| false, &other, Instant::now()).len(), 1);
    }

    #[tokio::test]
    async fn only_endpoints_that_accept_a_connection_are_reachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap().to_string();
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().to_string()
        };
        assert_eq!(reachable(vec![closed, open.clone(), "not an endpoint".into()]).await, vec![open]);
    }
}

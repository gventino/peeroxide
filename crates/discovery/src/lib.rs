//! LAN peer discovery over mDNS/DNS-SD.
//!
//! A peer announces itself only while broadcasting. Announcements are untrusted input: they are
//! validated, capped in number, and only point at a fingerprint that the transport later verifies.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

pub const SERVICE_TYPE: &str = "_p2pss._udp.local.";
pub const MAX_PEERS: usize = 64;
const TXT_VERSION: &str = "1";
const MAX_NAME_BYTES: usize = 63;

/// A broadcaster seen on the network. `fingerprint` is 64 lowercase hex characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub fingerprint: String,
    pub name: String,
    /// IPv4 addresses, most likely to be reachable first.
    pub addrs: Vec<SocketAddr>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("mDNS error: {0}")]
    Mdns(#[from] mdns_sd::Error),
    #[error("invalid fingerprint")]
    InvalidFingerprint,
}

pub struct Discovery {
    daemon: ServiceDaemon,
    own_fingerprint: String,
    announced: Option<String>,
}

impl Discovery {
    pub fn new(own_fingerprint: &str) -> Result<Self, DiscoveryError> {
        let own_fingerprint =
            normalize_fingerprint(own_fingerprint).ok_or(DiscoveryError::InvalidFingerprint)?;
        Ok(Self {
            daemon: ServiceDaemon::new()?,
            own_fingerprint,
            announced: None,
        })
    }

    /// Announces this peer as a broadcaster reachable on `port`, replacing any earlier announcement.
    pub fn announce(&mut self, name: &str, port: u16) -> Result<(), DiscoveryError> {
        self.withdraw();
        // The fingerprint prefix makes instance names unique even when display names collide.
        let instance = &self.own_fingerprint[..16];
        let host = format!("p2pss-{instance}.local.");
        let name = sanitize_name(name);
        let props = [
            ("v", TXT_VERSION),
            ("name", name.as_str()),
            ("fp", self.own_fingerprint.as_str()),
        ];
        let info = ServiceInfo::new(SERVICE_TYPE, instance, &host, "", port, &props[..])?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        self.daemon.register(info)?;
        tracing::info!(%fullname, port, "announced on mDNS");
        self.announced = Some(fullname);
        Ok(())
    }

    /// Withdraws the announcement (sends an mDNS goodbye so peers drop us right away).
    pub fn withdraw(&mut self) {
        if let Some(fullname) = self.announced.take()
            && let Ok(status) = self.daemon.unregister(&fullname)
        {
            let _ = status.recv_timeout(Duration::from_millis(500));
            tracing::info!(%fullname, "withdrew mDNS announcement");
        }
    }

    /// Watches the network; `on_update` receives the full, validated peer list on every change.
    pub fn browse(
        &self,
        on_update: impl Fn(Vec<Peer>) + Send + 'static,
    ) -> Result<(), DiscoveryError> {
        let events = self.daemon.browse(SERVICE_TYPE)?;
        let mut table = PeerTable::new(self.own_fingerprint.clone());
        std::thread::Builder::new()
            .name("discovery".into())
            .spawn(move || {
                while let Ok(event) = events.recv() {
                    let changed = match event {
                        ServiceEvent::ServiceResolved(s) => {
                            let txt = |k| s.txt_properties.get_property_val_str(k);
                            let peer = validate(
                                txt("v"),
                                txt("name"),
                                txt("fp"),
                                s.addresses.iter().map(|a| a.to_ip_addr()),
                                s.port,
                            );
                            match peer {
                                Ok(peer) => table.upsert(&s.fullname, peer),
                                Err(why) => {
                                    tracing::debug!(fullname = %s.fullname, why, "ignored announcement");
                                    table.remove(&s.fullname)
                                }
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => table.remove(&fullname),
                        _ => false,
                    };
                    if changed {
                        on_update(table.snapshot());
                    }
                }
            })
            .map_err(|e| DiscoveryError::Mdns(mdns_sd::Error::Msg(e.to_string())))?;
        Ok(())
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.withdraw();
        if let Ok(status) = self.daemon.shutdown() {
            let _ = status.recv_timeout(Duration::from_millis(500));
        }
    }
}

struct PeerTable {
    own_fingerprint: String,
    peers: HashMap<String, Peer>,
    warned_full: bool,
}

impl PeerTable {
    fn new(own_fingerprint: String) -> Self {
        Self {
            own_fingerprint,
            peers: HashMap::new(),
            warned_full: false,
        }
    }

    fn upsert(&mut self, fullname: &str, peer: Peer) -> bool {
        if peer.fingerprint == self.own_fingerprint {
            return false;
        }
        if !self.peers.contains_key(fullname) && self.peers.len() >= MAX_PEERS {
            if !std::mem::replace(&mut self.warned_full, true) {
                tracing::warn!("more than {MAX_PEERS} peers announced; ignoring the rest");
            }
            return false;
        }
        self.peers.insert(fullname.to_owned(), peer.clone()) != Some(peer)
    }

    fn remove(&mut self, fullname: &str) -> bool {
        self.peers.remove(fullname).is_some()
    }

    fn snapshot(&self) -> Vec<Peer> {
        let mut peers: Vec<Peer> = self.peers.values().cloned().collect();
        peers.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.fingerprint.cmp(&b.fingerprint))
        });
        peers
    }
}

fn validate(
    version: Option<&str>,
    name: Option<&str>,
    fingerprint: Option<&str>,
    ips: impl IntoIterator<Item = IpAddr>,
    port: u16,
) -> Result<Peer, &'static str> {
    if version != Some(TXT_VERSION) {
        return Err("unsupported version");
    }
    let fingerprint = fingerprint
        .and_then(normalize_fingerprint)
        .ok_or("invalid fingerprint")?;
    if port == 0 {
        return Err("invalid port");
    }
    let mut v4: Vec<Ipv4Addr> = ips
        .into_iter()
        .filter_map(|ip| match ip {
            IpAddr::V4(a) if !a.is_unspecified() && !a.is_multicast() && !a.is_broadcast() => {
                Some(a)
            }
            _ => None,
        })
        .collect();
    v4.sort_by_key(|a| (reachability_rank(a), *a));
    v4.dedup();
    if v4.is_empty() {
        return Err("no IPv4 address");
    }
    Ok(Peer {
        fingerprint,
        name: sanitize_name(name.unwrap_or_default()),
        addrs: v4
            .into_iter()
            .map(|a| SocketAddr::from((a, port)))
            .collect(),
    })
}

/// Private LAN addresses first; link-local and loopback last.
fn reachability_rank(a: &Ipv4Addr) -> u8 {
    if a.is_private() {
        0
    } else if a.is_link_local() {
        2
    } else if a.is_loopback() {
        3
    } else {
        1
    }
}

fn normalize_fingerprint(s: &str) -> Option<String> {
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

fn sanitize_name(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    let mut end = clean.len().min(MAX_NAME_BYTES);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    match clean[..end].trim_end() {
        "" => "unnamed".into(),
        s => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "3f9a07c2aa11bb22cc33dd44ee55ff66778899aabbccddeeff00112233445566";

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn peer(fp: &str, name: &str) -> Peer {
        validate(Some("1"), Some(name), Some(fp), [ip("192.168.0.2")], 5000).unwrap()
    }

    #[test]
    fn accepts_valid_announcement_and_orders_addresses() {
        let p = validate(
            Some("1"),
            Some("Ana's PC"),
            Some(&FP.to_uppercase()),
            [
                ip("127.0.0.1"),
                ip("fe80::1"),
                ip("169.254.3.4"),
                ip("192.168.0.20"),
                ip("172.20.0.5"),
                ip("192.168.0.20"),
            ],
            50123,
        )
        .unwrap();
        assert_eq!(p.fingerprint, FP);
        assert_eq!(p.name, "Ana's PC");
        let addrs: Vec<String> = p.addrs.iter().map(ToString::to_string).collect();
        assert_eq!(
            addrs,
            [
                "172.20.0.5:50123",
                "192.168.0.20:50123",
                "169.254.3.4:50123",
                "127.0.0.1:50123"
            ]
        );
    }

    #[test]
    fn rejects_malformed_announcements() {
        let ok_ip = [ip("192.168.0.2")];
        assert!(validate(Some("2"), Some("x"), Some(FP), ok_ip, 1).is_err());
        assert!(validate(None, Some("x"), Some(FP), ok_ip, 1).is_err());
        assert!(validate(Some("1"), Some("x"), Some("abc"), ok_ip, 1).is_err());
        assert!(validate(Some("1"), Some("x"), Some(&"g".repeat(64)), ok_ip, 1).is_err());
        assert!(validate(Some("1"), Some("x"), None, ok_ip, 1).is_err());
        assert!(validate(Some("1"), Some("x"), Some(FP), ok_ip, 0).is_err());
        assert!(validate(Some("1"), Some("x"), Some(FP), [ip("::1")], 1).is_err());
        assert!(validate(Some("1"), Some("x"), Some(FP), [ip("0.0.0.0")], 1).is_err());
    }

    #[test]
    fn sanitizes_display_names() {
        assert_eq!(sanitize_name("  Ana\u{7}\n PC  "), "Ana PC");
        assert_eq!(sanitize_name("\u{1b}[31m"), "[31m");
        assert_eq!(sanitize_name("   "), "unnamed");
        let long = "é".repeat(40);
        let s = sanitize_name(&long);
        assert!(s.len() <= MAX_NAME_BYTES);
        assert!(s.chars().all(|c| c == 'é'));
    }

    #[test]
    fn table_filters_self_caps_size_and_reports_changes() {
        let own = FP.to_string();
        let mut t = PeerTable::new(own.clone());
        assert!(!t.upsert("self", peer(&own, "me")));

        let other = |i: usize| format!("{i:064x}");
        for i in 0..MAX_PEERS {
            assert!(t.upsert(&format!("p{i}"), peer(&other(i), "x")));
        }
        assert!(!t.upsert("overflow", peer(&other(999), "x")));
        assert_eq!(t.snapshot().len(), MAX_PEERS);

        assert!(
            !t.upsert("p0", peer(&other(0), "x")),
            "unchanged is not a change"
        );
        assert!(t.upsert("p0", peer(&other(0), "renamed")));
        assert!(t.remove("p0"));
        assert!(!t.remove("p0"));
        assert!(t.upsert("overflow", peer(&other(999), "x")), "room again");
    }

    #[test]
    fn snapshot_is_sorted_by_name() {
        let mut t = PeerTable::new(FP.into());
        t.upsert("b", peer(&format!("{:064x}", 2), "bob"));
        t.upsert("a", peer(&format!("{:064x}", 1), "Alice"));
        let names: Vec<String> = t.snapshot().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Alice", "bob"]);
    }

    /// Real mDNS on this machine: one daemon announces, another discovers, then the goodbye.
    #[test]
    fn announce_is_discovered_and_withdraw_removes_it() {
        let (tx, rx) = std::sync::mpsc::channel();
        let viewer = Discovery::new(&format!("{:064x}", 1)).unwrap();
        viewer.browse(move |peers| drop(tx.send(peers))).unwrap();

        let mut broadcaster = Discovery::new(FP).unwrap();
        broadcaster.announce("Test broadcaster", 45678).unwrap();

        let wait = |pred: &dyn Fn(&[Peer]) -> bool| loop {
            let peers = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("no discovery update");
            if pred(&peers) {
                return peers;
            }
        };
        let peers = wait(&|p| p.iter().any(|p| p.fingerprint == FP));
        let found = peers.iter().find(|p| p.fingerprint == FP).unwrap();
        assert_eq!(found.name, "Test broadcaster");
        assert!(found.addrs.iter().all(|a| a.port() == 45678 && a.is_ipv4()));

        broadcaster.withdraw();
        wait(&|p| p.iter().all(|p| p.fingerprint != FP));
    }
}

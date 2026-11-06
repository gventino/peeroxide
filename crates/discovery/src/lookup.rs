//! Unicast lookups, for networks that don't carry multicast (so mDNS can't cross them), such as
//! a Tailscale or Headscale tailnet.
//!
//! While broadcasting, a peer runs a [`Responder`] on UDP [`LOOKUP_PORT`]: to the question "are
//! you broadcasting?" it answers what its mDNS announcement says (name, fingerprint and the
//! broadcast port). A viewer's [`Lookup`] asks the addresses of its saved contacts and, when
//! Tailscale runs on this computer, every online device of the tailnet, every few seconds.
//!
//! An answer is as untrusted as an mDNS announcement: it is validated and only points at a
//! fingerprint that the transport verifies when watching. A question is padded so an answer is
//! never larger than the question that caused it, so the responder can't be used to amplify
//! traffic towards a forged source address.

use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::{
    MAX_NAME_BYTES, MAX_PEERS, Peer, merge, normalize_fingerprint, peer_from, sanitize_name,
};

/// The UDP port every broadcaster answers lookups on. On the wire: never change it.
pub const LOOKUP_PORT: u16 = 47123;

const QUESTION_MAGIC: &[u8; 8] = b"PXLOOKUP";
const ANSWER_MAGIC: &[u8; 8] = b"PXANSWER";
const WIRE_VERSION: u8 = 1;
/// Longer than any answer: magic, version, port, 64 hex digits, name length and name.
const QUESTION_LEN: usize = 160;
const ANSWER_MAX: usize = 8 + 1 + 2 + 64 + 1 + MAX_NAME_BYTES;
const _: () = assert!(ANSWER_MAX <= QUESTION_LEN);

/// How often the responder checks whether it should stop.
const POLL: Duration = Duration::from_millis(100);
/// Addresses asked per round: saved contacts' and the tailnet's.
const MAX_TARGETS: usize = 128;

#[derive(Clone, Copy, Debug)]
struct Timing {
    /// Between the start of one round of questions and the next.
    round: Duration,
    /// How long a round waits for answers.
    wait: Duration,
    /// When the addresses that haven't answered are asked again, within a round.
    resend: Duration,
    /// How long the tailnet's device list is reused before asking Tailscale again.
    tailnet: Duration,
    /// Rounds a found broadcaster may miss before it leaves the list.
    misses: u8,
}

const TIMING: Timing = Timing {
    round: Duration::from_secs(5),
    wait: Duration::from_millis(1500),
    resend: Duration::from_millis(500),
    tailnet: Duration::from_secs(30),
    misses: 2,
};

fn question() -> [u8; QUESTION_LEN] {
    let mut q = [0u8; QUESTION_LEN];
    q[..8].copy_from_slice(QUESTION_MAGIC);
    q[8] = WIRE_VERSION;
    q
}

fn is_question(buf: &[u8]) -> bool {
    buf.len() >= QUESTION_LEN && buf.starts_with(QUESTION_MAGIC) && buf[8] == WIRE_VERSION
}

fn answer(fingerprint: &str, name: &str, port: u16) -> Vec<u8> {
    let name = sanitize_name(name);
    let mut a = Vec::with_capacity(ANSWER_MAX);
    a.extend_from_slice(ANSWER_MAGIC);
    a.push(WIRE_VERSION);
    a.extend_from_slice(&port.to_le_bytes());
    a.extend_from_slice(fingerprint.as_bytes());
    a.push(name.len() as u8);
    a.extend_from_slice(name.as_bytes());
    a
}

#[derive(Debug, PartialEq, Eq)]
struct Answer {
    port: u16,
    fingerprint: String,
    name: String,
}

fn parse_answer(buf: &[u8]) -> Option<Answer> {
    let rest = buf.strip_prefix(ANSWER_MAGIC)?;
    let (&version, rest) = rest.split_first()?;
    if version != WIRE_VERSION {
        return None;
    }
    let (port, rest) = rest.split_first_chunk::<2>()?;
    let (fingerprint, rest) = rest.split_first_chunk::<64>()?;
    let (&len, name) = rest.split_first()?;
    let name = name.get(..usize::from(len))?;
    Some(Answer {
        port: u16::from_le_bytes(*port),
        fingerprint: std::str::from_utf8(fingerprint).ok()?.to_owned(),
        name: String::from_utf8_lossy(name).into_owned(),
    })
}

fn quiet(kind: ErrorKind) -> bool {
    // Windows reports an ICMP "port unreachable" for an earlier datagram as a reset.
    matches!(
        kind,
        ErrorKind::WouldBlock
            | ErrorKind::TimedOut
            | ErrorKind::ConnectionReset
            | ErrorKind::Interrupted
    )
}

/// Answers lookups while broadcasting. Dropping it stops answering.
pub struct Responder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    port: u16,
}

impl Responder {
    /// Answers on [`LOOKUP_PORT`] that this peer broadcasts on `port` as `name`. Fails when the
    /// port is taken, e.g. by another Peeroxide broadcasting on this computer.
    pub fn start(fingerprint: &str, name: &str, port: u16) -> anyhow::Result<Self> {
        Self::bind(LOOKUP_PORT, fingerprint, name, port)
    }

    fn bind(lookup_port: u16, fingerprint: &str, name: &str, port: u16) -> anyhow::Result<Self> {
        let fingerprint = normalize_fingerprint(fingerprint).context("invalid fingerprint")?;
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, lookup_port))
            .with_context(|| format!("UDP port {lookup_port} is taken"))?;
        socket.set_read_timeout(Some(POLL))?;
        let bound = socket.local_addr()?.port();
        let reply = answer(&fingerprint, name, port);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("lookup-responder".into())
            .spawn({
                let stop = stop.clone();
                move || respond(&socket, &reply, &stop)
            })
            .context("could not start the lookup responder")?;
        tracing::info!(port = bound, "answering lookups");
        Ok(Self {
            stop,
            thread: Some(thread),
            port: bound,
        })
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        tracing::info!(port = self.port, "stopped answering lookups");
    }
}

fn respond(socket: &UdpSocket, reply: &[u8], stop: &AtomicBool) {
    let mut buf = [0u8; 512];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buf) {
            Ok((n, from)) if is_question(&buf[..n]) => {
                // Every viewer asks every few seconds: too frequent for the session log.
                tracing::trace!(%from, "answered a lookup");
                let _ = socket.send_to(reply, from);
            }
            Ok(_) => {}
            Err(e) if quiet(e.kind()) => {}
            Err(e) => {
                tracing::debug!("lookup responder: {e}");
                std::thread::sleep(POLL);
            }
        }
    }
}

/// Asks every target at once (twice for those slow to answer) and returns the valid answers.
fn ask(targets: &[SocketAddr], timing: &Timing) -> std::io::Result<Vec<(IpAddr, Answer)>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    let q = question();
    let pending: HashSet<SocketAddr> = targets.iter().copied().collect();
    let mut answered = HashSet::new();
    let mut answers = Vec::new();
    for t in &pending {
        let _ = socket.send_to(&q, t);
    }
    let start = Instant::now();
    let mut resent = false;
    let mut buf = [0u8; 512];
    while answered.len() < pending.len() {
        let elapsed = start.elapsed();
        if elapsed >= timing.wait {
            break;
        }
        if !resent && elapsed >= timing.resend {
            for t in pending.difference(&answered) {
                let _ = socket.send_to(&q, t);
            }
            resent = true;
        }
        let next = if resent { timing.wait } else { timing.resend };
        let wait = next.saturating_sub(elapsed).max(Duration::from_millis(1));
        socket.set_read_timeout(Some(wait))?;
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                if pending.contains(&from)
                    && !answered.contains(&from)
                    && let Some(a) = parse_answer(&buf[..n])
                {
                    answered.insert(from);
                    answers.push((from.ip(), a));
                }
            }
            Err(e) if quiet(e.kind()) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(answers)
}

/// The IPv4 addresses worth asking, in order, without repeats.
fn targets(contacts: &[IpAddr], tailnet: &[Ipv4Addr], port: u16) -> Vec<SocketAddr> {
    let mut seen = HashSet::new();
    contacts
        .iter()
        .filter_map(|ip| match ip {
            IpAddr::V4(a) => Some(*a),
            IpAddr::V6(_) => None,
        })
        .chain(tailnet.iter().copied())
        .filter(|a| !a.is_unspecified() && !a.is_multicast() && !a.is_broadcast())
        .filter(|a| seen.insert(*a))
        .take(MAX_TARGETS)
        .map(|a| SocketAddr::from((a, port)))
        .collect()
}

/// The peers in a round's answers, one per fingerprint, without this peer.
fn peers_in(answers: Vec<(IpAddr, Answer)>, own_fingerprint: &str) -> Vec<Peer> {
    let mut peers = Vec::new();
    for (ip, a) in answers {
        match peer_from(Some(&a.name), Some(&a.fingerprint), [ip], a.port) {
            Ok(p) if p.fingerprint != own_fingerprint => peers = merge(&peers, &[p]),
            Ok(_) => {}
            Err(why) => tracing::debug!(%ip, %why, "ignored lookup answer"),
        }
    }
    peers
}

enum Command {
    Contacts(Vec<IpAddr>),
    Tailscale(bool),
}

/// Looks for broadcasters by asking known addresses, on a thread of its own. Dropping it stops.
pub struct Lookup {
    commands: mpsc::Sender<Command>,
}

impl Lookup {
    /// `on_update` receives the full list of broadcasters found on every change. With
    /// `tailscale`, the tailnet's online devices are asked too (see [`Lookup::set_tailscale`]).
    pub fn start(
        own_fingerprint: &str,
        tailscale: bool,
        on_update: impl Fn(Vec<Peer>) + Send + 'static,
    ) -> anyhow::Result<Self> {
        Self::start_with(own_fingerprint, tailscale, LOOKUP_PORT, TIMING, on_update)
    }

    fn start_with(
        own_fingerprint: &str,
        tailscale: bool,
        port: u16,
        timing: Timing,
        on_update: impl Fn(Vec<Peer>) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let own = normalize_fingerprint(own_fingerprint).context("invalid fingerprint")?;
        let (commands, rx) = mpsc::channel();
        let mut state = State {
            own,
            port,
            timing,
            contacts: Vec::new(),
            tailscale,
            tailnet: Vec::new(),
            tailnet_checked: None,
            tailnet_error: None,
            found: HashMap::new(),
            shown: Vec::new(),
        };
        std::thread::Builder::new()
            .name("lookup".into())
            .spawn(move || state.run(&rx, &on_update))
            .context("could not start the lookup thread")?;
        Ok(Self { commands })
    }

    /// The addresses of saved contacts, asked whether they are broadcasting.
    pub fn set_contacts(&self, ips: impl IntoIterator<Item = IpAddr>) {
        let _ = self
            .commands
            .send(Command::Contacts(ips.into_iter().collect()));
    }

    /// Whether to ask the online devices of the tailnet this computer is on. They come from the
    /// Tailscale client (`tailscale status`), also when the control server is Headscale.
    pub fn set_tailscale(&self, enabled: bool) {
        let _ = self.commands.send(Command::Tailscale(enabled));
    }
}

struct State {
    own: String,
    port: u16,
    timing: Timing,
    contacts: Vec<IpAddr>,
    tailscale: bool,
    tailnet: Vec<Ipv4Addr>,
    tailnet_checked: Option<Instant>,
    /// Why the tailnet couldn't be listed last time.
    tailnet_error: Option<String>,
    /// By fingerprint, with the number of rounds missed in a row.
    found: HashMap<String, (Peer, u8)>,
    /// What `on_update` was last given.
    shown: Vec<Peer>,
}

impl State {
    fn run(&mut self, rx: &mpsc::Receiver<Command>, on_update: &dyn Fn(Vec<Peer>)) {
        loop {
            let next = Instant::now() + self.timing.round;
            self.round(on_update);
            // Until the next round; a change asks again right away.
            loop {
                let left = next.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(Command::Contacts(ips)) => {
                        if ips != self.contacts {
                            self.contacts = ips;
                            break;
                        }
                    }
                    Ok(Command::Tailscale(on)) => {
                        if on != self.tailscale {
                            tracing::info!(enabled = on, "Tailscale lookups");
                            self.tailscale = on;
                            self.tailnet.clear();
                            self.tailnet_checked = None;
                            self.tailnet_error = None;
                            self.found.clear();
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }

    fn round(&mut self, on_update: &dyn Fn(Vec<Peer>)) {
        self.refresh_tailnet();
        let targets = targets(&self.contacts, &self.tailnet, self.port);
        let answers = if targets.is_empty() {
            Vec::new()
        } else {
            ask(&targets, &self.timing).unwrap_or_else(|e| {
                tracing::debug!("lookup round failed: {e}");
                Vec::new()
            })
        };
        let peers = peers_in(answers, &self.own);

        for (peer, misses) in self.found.values_mut() {
            if !peers.iter().any(|p| p.fingerprint == peer.fingerprint) {
                *misses += 1;
            }
        }
        self.found.retain(|_, (peer, misses)| {
            let keep = *misses < self.timing.misses;
            if !keep {
                tracing::info!(name = %peer.name, "no longer answers lookups");
            }
            keep
        });
        for peer in peers {
            if !self.found.contains_key(&peer.fingerprint) {
                if self.found.len() >= MAX_PEERS {
                    continue;
                }
                tracing::info!(name = %peer.name, addrs = ?peer.addrs, "found by lookup");
            }
            self.found.insert(peer.fingerprint.clone(), (peer, 0));
        }

        let all: Vec<Peer> = self.found.values().map(|(p, _)| p.clone()).collect();
        let shown = merge(&all, &[]);
        if shown != self.shown {
            self.shown = shown.clone();
            on_update(shown);
        }
    }

    fn refresh_tailnet(&mut self) {
        if !self.tailscale
            || self
                .tailnet_checked
                .is_some_and(|t| t.elapsed() < self.timing.tailnet)
        {
            return;
        }
        self.tailnet_checked = Some(Instant::now());
        // Logged when it changes only: this runs every few seconds for the whole session.
        match crate::tailscale::online_peers() {
            Ok(ips) => {
                if ips != self.tailnet || self.tailnet_error.is_some() {
                    tracing::info!(devices = ips.len(), "tailnet devices to ask");
                }
                self.tailnet = ips;
                self.tailnet_error = None;
            }
            Err(e) => {
                let e = format!("{e:#}");
                if self.tailnet_error.as_ref() != Some(&e) {
                    tracing::info!("no tailnet to ask: {e}");
                }
                self.tailnet.clear();
                self.tailnet_error = Some(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "3f9a07c2aa11bb22cc33dd44ee55ff66778899aabbccddeeff00112233445566";
    const OWN: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    const FAST: Timing = Timing {
        round: Duration::from_millis(200),
        wait: Duration::from_millis(300),
        resend: Duration::from_millis(100),
        tailnet: Duration::from_secs(30),
        misses: 2,
    };

    fn localhost() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    #[test]
    fn answers_roundtrip_and_never_outgrow_the_question() {
        let a = answer(FP, "Ana's PC", 50123);
        assert!(a.len() <= QUESTION_LEN);
        assert_eq!(
            parse_answer(&a),
            Some(Answer {
                port: 50123,
                fingerprint: FP.into(),
                name: "Ana's PC".into(),
            })
        );
        let long = answer(FP, &"é".repeat(200), 1);
        assert!(long.len() <= QUESTION_LEN);
        assert!(parse_answer(&long).unwrap().name.len() <= MAX_NAME_BYTES);
    }

    #[test]
    fn rejects_malformed_packets() {
        let a = answer(FP, "Ana", 50123);
        assert!(parse_answer(&a[..a.len() - 1]).is_none(), "truncated name");
        assert!(parse_answer(&a[..20]).is_none(), "truncated fingerprint");
        assert!(parse_answer(b"PXANSWER").is_none());
        let mut other_version = a.clone();
        other_version[8] = 2;
        assert!(parse_answer(&other_version).is_none());
        assert!(parse_answer(&question()).is_none());

        assert!(is_question(&question()));
        assert!(!is_question(&question()[..QUESTION_LEN - 1]), "too short");
        assert!(!is_question(&a));
    }

    #[test]
    fn invalid_answers_are_dropped_and_own_is_skipped() {
        let answer = |fp: &str, port| Answer {
            port,
            fingerprint: fp.into(),
            name: "x".into(),
        };
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let peers = peers_in(
            vec![
                (ip("100.64.0.2"), answer(FP, 50123)),
                (ip("192.168.0.9"), answer(FP, 50123)),
                (ip("100.64.0.3"), answer(OWN, 50000)),
                (ip("100.64.0.4"), answer(&"g".repeat(64), 50000)),
                (ip("100.64.0.5"), answer(FP, 0)),
            ],
            OWN,
        );
        assert_eq!(peers.len(), 1);
        let addrs: Vec<String> = peers[0].addrs.iter().map(ToString::to_string).collect();
        assert_eq!(addrs, ["100.64.0.2:50123", "192.168.0.9:50123"]);
    }

    #[test]
    fn targets_are_ipv4_unique_and_capped() {
        let contacts: Vec<IpAddr> = ["100.64.0.2", "::1", "0.0.0.0", "224.0.0.251", "100.64.0.2"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        let tailnet: Vec<Ipv4Addr> = (1..=200).map(|i| Ipv4Addr::new(100, 64, 1, i)).collect();
        let t = targets(&contacts, &tailnet, 7);
        assert_eq!(t[0], "100.64.0.2:7".parse().unwrap());
        assert_eq!(t[1], "100.64.1.1:7".parse().unwrap());
        assert_eq!(t.len(), MAX_TARGETS);
    }

    /// Real sockets on this machine; one target has nothing listening (on Windows its ICMP
    /// error surfaces as a reset that must not end the round).
    #[test]
    fn asks_and_is_answered_despite_a_closed_port() {
        let responder = Responder::bind(0, FP, "Test broadcaster", 45678).unwrap();
        let closed = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap();
        let open = SocketAddr::from((Ipv4Addr::LOCALHOST, responder.port));
        let answers = ask(&[closed, open], &FAST).unwrap();
        assert_eq!(
            answers,
            [(
                localhost(),
                Answer {
                    port: 45678,
                    fingerprint: FP.into(),
                    name: "Test broadcaster".into(),
                }
            )]
        );
    }

    #[test]
    fn lookup_finds_a_contact_and_drops_it_after_it_stops() {
        let responder = Responder::bind(0, FP, "Test broadcaster", 45678).unwrap();
        let (tx, rx) = mpsc::channel();
        let lookup =
            Lookup::start_with(OWN, false, responder.port, FAST, move |p| drop(tx.send(p)))
                .unwrap();
        lookup.set_contacts([localhost()]);

        let found = rx.recv_timeout(Duration::from_secs(5)).expect("no update");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].fingerprint, FP);
        assert_eq!(found[0].name, "Test broadcaster");
        assert_eq!(
            found[0].addrs,
            [SocketAddr::from((Ipv4Addr::LOCALHOST, 45678))]
        );

        drop(responder);
        let gone = rx.recv_timeout(Duration::from_secs(5)).expect("no update");
        assert!(gone.is_empty());
    }
}

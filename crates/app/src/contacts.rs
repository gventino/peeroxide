//! Broadcasters you have watched before, remembered per profile so they can be reached again
//! without discovery, and so a different ID behind a familiar name can be flagged (TOFU).

use std::net::SocketAddr;
use std::path::Path;

use anyhow::Context;
use peeroxide_net::Fingerprint;
use serde::{Deserialize, Serialize};

const FILE: &str = "contacts.toml";
pub const MAX_CONTACTS: usize = 50;
const MAX_ADDRS: usize = 8;
const MAX_NAME_CHARS: usize = 64;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    /// 64 lowercase hex characters.
    pub fingerprint: String,
    pub name: String,
    /// Most recently working address first.
    pub addrs: Vec<SocketAddr>,
    /// Unix seconds of the last successful connection.
    pub last_seen: u64,
}

/// Newest first.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Contacts {
    #[serde(rename = "contact")]
    list: Vec<Contact>,
}

impl Contacts {
    /// Missing or unreadable files give an empty list; malformed entries are dropped.
    pub fn load(dir: &Path) -> Self {
        let mut contacts: Self = std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        contacts
            .list
            .retain(|c| Fingerprint::from_hex(&c.fingerprint).is_some() && !c.addrs.is_empty());
        for c in &mut contacts.list {
            c.fingerprint.make_ascii_lowercase();
        }
        contacts
            .list
            .sort_by_key(|c| std::cmp::Reverse(c.last_seen));
        contacts.list.truncate(MAX_CONTACTS);
        contacts
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let text = toml::to_string_pretty(self).context("serializing the contacts")?;
        let path = dir.join(FILE);
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    pub fn list(&self) -> &[Contact] {
        &self.list
    }

    pub fn get(&self, fingerprint_hex: &str) -> Option<&Contact> {
        self.list.iter().find(|c| c.fingerprint == fingerprint_hex)
    }

    /// Records a successful connection: `working` first, then the other known addresses.
    pub fn remember(
        &mut self,
        fingerprint: &Fingerprint,
        name: &str,
        working: SocketAddr,
        known: &[SocketAddr],
        now: u64,
    ) {
        let hex = fingerprint.to_hex();
        let previous = self
            .list
            .iter()
            .position(|c| c.fingerprint == hex)
            .map(|i| self.list.remove(i));
        let mut addrs = vec![working];
        let older = previous.iter().flat_map(|c| c.addrs.iter());
        for a in known.iter().chain(older) {
            if !addrs.contains(a) && addrs.len() < MAX_ADDRS {
                addrs.push(*a);
            }
        }
        self.list.insert(
            0,
            Contact {
                fingerprint: hex,
                name: sanitize_name(name),
                addrs,
                last_seen: now,
            },
        );
        self.list.truncate(MAX_CONTACTS);
    }

    pub fn remove(&mut self, fingerprint: &str) {
        self.list.retain(|c| c.fingerprint != fingerprint);
    }

    /// A saved contact with the same name as `name` but a different ID, if any.
    pub fn name_conflict(&self, name: &str, fingerprint: &str) -> Option<&Contact> {
        let name = name.trim();
        self.list
            .iter()
            .find(|c| c.fingerprint != fingerprint && c.name.trim().eq_ignore_ascii_case(name))
    }
}

fn sanitize_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    match clean.trim() {
        "" => "unnamed".into(),
        s => s.to_owned(),
    }
}

/// "today", "yesterday" or "N days ago" for a unix timestamp.
pub fn ago(now: u64, then: u64) -> String {
    match now.saturating_sub(then) / 86_400 {
        0 => "today".into(),
        1 => "yesterday".into(),
        n => format!("{n} days ago"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(n: u8) -> Fingerprint {
        Fingerprint::of(&[n])
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn remembers_newest_first_and_merges_addresses() {
        let mut c = Contacts::default();
        c.remember(&fp(1), "Alice", addr("26.0.0.1:5000"), &[], 10);
        c.remember(&fp(2), "Bob", addr("26.0.0.2:5000"), &[], 20);
        assert_eq!(c.list()[0].name, "Bob");

        c.remember(
            &fp(1),
            "Alice B.",
            addr("192.168.0.5:5000"),
            &[addr("26.0.0.1:5000")],
            30,
        );
        let alice = &c.list()[0];
        assert_eq!(alice.name, "Alice B.");
        assert_eq!(alice.last_seen, 30);
        assert_eq!(
            alice.addrs,
            [addr("192.168.0.5:5000"), addr("26.0.0.1:5000")]
        );
        assert_eq!(c.list().len(), 2);
        assert!(c.get(&fp(1).to_hex()).is_some());
    }

    #[test]
    fn caps_list_and_address_count() {
        let mut c = Contacts::default();
        for i in 0..=MAX_CONTACTS as u8 {
            c.remember(&fp(i), "x", addr("26.0.0.1:1"), &[], u64::from(i));
        }
        assert_eq!(c.list().len(), MAX_CONTACTS);
        assert!(c.get(&fp(0).to_hex()).is_none(), "oldest dropped");

        let many: Vec<SocketAddr> = (1..20)
            .map(|p| SocketAddr::from(([10, 0, 0, 1], p)))
            .collect();
        c.remember(&fp(99), "y", addr("26.0.0.9:9"), &many, 100);
        assert_eq!(c.get(&fp(99).to_hex()).unwrap().addrs.len(), MAX_ADDRS);
    }

    #[test]
    fn saves_loads_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Contacts::load(dir.path()), Contacts::default());

        let mut c = Contacts::default();
        c.remember(&fp(1), "Alice", addr("26.0.0.1:5000"), &[], 10);
        c.remember(&fp(2), "Bob", addr("26.0.0.2:5000"), &[], 20);
        c.save(dir.path()).unwrap();
        let mut loaded = Contacts::load(dir.path());
        assert_eq!(loaded, c);

        loaded.remove(&fp(2).to_hex());
        assert_eq!(loaded.list().len(), 1);
        assert_eq!(loaded.list()[0].name, "Alice");
    }

    #[test]
    fn tolerates_corrupt_files_and_drops_bad_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "not = [valid").unwrap();
        assert_eq!(Contacts::load(dir.path()), Contacts::default());

        let good = fp(1).to_hex().to_uppercase();
        std::fs::write(
            dir.path().join(FILE),
            format!(
                "[[contact]]\nfingerprint = \"{good}\"\nname = \"ok\"\naddrs = [\"26.0.0.1:1\"]\nlast_seen = 1\n\
                 [[contact]]\nfingerprint = \"zz\"\nname = \"bad\"\naddrs = [\"26.0.0.1:1\"]\nlast_seen = 2\n\
                 [[contact]]\nfingerprint = \"{good}\"\nname = \"no addrs\"\naddrs = []\nlast_seen = 3\n"
            ),
        )
        .unwrap();
        let loaded = Contacts::load(dir.path());
        assert_eq!(loaded.list().len(), 1);
        assert_eq!(loaded.list()[0].fingerprint, fp(1).to_hex());
    }

    #[test]
    fn flags_same_name_with_different_id() {
        let mut c = Contacts::default();
        c.remember(&fp(1), "Alice", addr("26.0.0.1:5000"), &[], 10);
        let alice = fp(1).to_hex();
        let impostor = fp(2).to_hex();
        assert!(c.name_conflict(" alice ", &impostor).is_some());
        assert!(c.name_conflict("Alice", &alice).is_none());
        assert!(c.name_conflict("Bob", &impostor).is_none());
    }

    #[test]
    fn sanitizes_names_from_the_network() {
        let mut c = Contacts::default();
        c.remember(&fp(1), "\u{1b}[31mEve\n", addr("26.0.0.1:1"), &[], 1);
        assert_eq!(c.list()[0].name, "[31mEve");
        c.remember(&fp(2), "   ", addr("26.0.0.1:1"), &[], 2);
        assert_eq!(c.list()[0].name, "unnamed");
    }

    #[test]
    fn describes_age_in_days() {
        assert_eq!(ago(1000, 1000), "today");
        assert_eq!(ago(86_400 * 3, 86_400 * 2), "yesterday");
        assert_eq!(ago(86_400 * 10, 0), "10 days ago");
        assert_eq!(ago(0, 100), "today");
    }
}

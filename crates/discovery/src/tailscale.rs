//! The devices of the tailnet this computer is on, from the Tailscale client's
//! `tailscale status --json`. The client is the same with a Headscale control server.

use std::collections::HashMap;
use std::io::Read;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, ensure};
use serde::Deserialize;

const TIMEOUT: Duration = Duration::from_secs(3);
/// Far above the status of any tailnet a group of friends shares.
const MAX_OUTPUT: u64 = 8 << 20;

/// The IPv4 addresses of the tailnet's online devices, this computer left out.
pub(crate) fn online_peers() -> anyhow::Result<Vec<Ipv4Addr>> {
    parse_online_peers(&status_json()?)
}

/// Where the command may be: on the PATH, or where the official installers put it.
fn programs() -> Vec<PathBuf> {
    #[allow(unused_mut)]
    let mut programs = vec![PathBuf::from("tailscale")];
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("ProgramFiles") {
        programs.push(PathBuf::from(dir).join("Tailscale").join("tailscale.exe"));
    }
    #[cfg(target_os = "macos")]
    programs.push(PathBuf::from(
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ));
    programs
}

fn spawn(program: &PathBuf) -> std::io::Result<Child> {
    let mut cmd = Command::new(program);
    cmd.args(["status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // The app has no console; without this, every call would flash one.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

fn status_json() -> anyhow::Result<Vec<u8>> {
    let mut child = programs()
        .iter()
        .find_map(|p| match spawn(p) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            other => Some(other),
        })
        .context("Tailscale is not installed")?
        .context("could not run Tailscale")?;
    // Read while it runs, so a long status can't fill the pipe and stall it.
    let mut stdout = child.stdout.take().context("no output from Tailscale")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(MAX_OUTPUT).read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Tailscale did not answer in {TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = reader
        .join()
        .map_err(|_| anyhow::anyhow!("reading Tailscale's output failed"))?;
    ensure!(status.success(), "Tailscale is not running ({status})");
    Ok(out)
}

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Peer", default)]
    peer: Option<HashMap<String, PeerStatus>>,
}

#[derive(Deserialize)]
struct PeerStatus {
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Option<Vec<String>>,
    #[serde(rename = "Online", default)]
    online: bool,
}

fn parse_online_peers(json: &[u8]) -> anyhow::Result<Vec<Ipv4Addr>> {
    let status: Status = serde_json::from_slice(json).context("unreadable Tailscale status")?;
    ensure!(
        status.backend_state == "Running",
        "Tailscale is not connected ({})",
        status.backend_state
    );
    let peers: Vec<PeerStatus> = status.peer.unwrap_or_default().into_values().collect();
    // A control server that doesn't report presence leaves every device offline: ask them all.
    let any_online = peers.iter().any(|p| p.online);
    let mut ips: Vec<Ipv4Addr> = peers
        .into_iter()
        .filter(|p| p.online || !any_online)
        .flat_map(|p| p.tailscale_ips.unwrap_or_default())
        .filter_map(|ip| ip.parse().ok())
        .collect();
    ips.sort();
    ips.dedup();
    Ok(ips)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like `tailscale status --json`, trimmed to the fields read and a few others.
    const STATUS: &str = r#"{
      "Version": "1.90.6-t",
      "BackendState": "Running",
      "TailscaleIPs": ["100.64.0.1", "fd7a:115c:a1e0::1"],
      "Self": {"HostName": "desktop", "TailscaleIPs": ["100.64.0.1", "fd7a:115c:a1e0::1"], "Online": true},
      "Peer": {
        "nodekey:aa": {"HostName": "laptop", "TailscaleIPs": ["100.64.0.3", "fd7a:115c:a1e0::3"], "Online": true, "OS": "windows"},
        "nodekey:bb": {"HostName": "phone", "TailscaleIPs": ["100.64.0.2"], "Online": false},
        "nodekey:cc": {"HostName": "nas", "TailscaleIPs": ["100.64.0.4"], "Online": true}
      },
      "MagicDNSSuffix": "tail.example"
    }"#;

    fn ips(list: &[&str]) -> Vec<Ipv4Addr> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn lists_online_peers_ipv4_only_and_not_self() {
        assert_eq!(
            parse_online_peers(STATUS.as_bytes()).unwrap(),
            ips(&["100.64.0.3", "100.64.0.4"])
        );
    }

    #[test]
    fn asks_everyone_when_no_presence_is_reported() {
        let json = r#"{"BackendState": "Running", "Peer": {
            "a": {"TailscaleIPs": ["100.64.0.3"]},
            "b": {"TailscaleIPs": ["100.64.0.2"], "Online": false}
        }}"#;
        assert_eq!(
            parse_online_peers(json.as_bytes()).unwrap(),
            ips(&["100.64.0.2", "100.64.0.3"])
        );
    }

    #[test]
    fn no_peers_and_not_connected() {
        let alone = r#"{"BackendState": "Running", "Peer": null}"#;
        assert!(parse_online_peers(alone.as_bytes()).unwrap().is_empty());
        let stopped = r#"{"BackendState": "Stopped", "Peer": {}}"#;
        assert!(parse_online_peers(stopped.as_bytes()).is_err());
        let login = r#"{"BackendState": "NeedsLogin"}"#;
        assert!(parse_online_peers(login.as_bytes()).is_err());
        assert!(parse_online_peers(b"not json").is_err());
    }
}

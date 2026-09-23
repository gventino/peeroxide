# P2P Screen Share

Peer-to-peer screen sharing for a local network, written in Rust. Anyone on the LAN can broadcast a monitor or a single window, several people can broadcast at the same time, and each viewer watches one stream at a time. There is no server: peers find each other with mDNS and stream directly over encrypted QUIC.

**Status: MVP.** Video only; audio is planned (FR-14). Verified on Windows 11. macOS and Linux code paths exist but have not been run yet.

Design documents: [functional requirements](functional-requirements.md) · [non-functional requirements](non-functional-requirements.md) · [use cases](use-cases.md) · [abuse cases (STRIDE)](abuse-cases.md)

## Using it

1. **Broadcast:** pick a source (a monitor, a window, or the built-in test pattern) and a quality preset, then press **Start broadcasting**. Capture and encoding only run while at least one person is watching.
   - Presets: **1080p · 30 fps** (~8 Mbps), **720p · 30 fps** (~4 Mbps), and **Internet / VPN · 720p · 20 fps** (~2 Mbps) for Radmin VPN, Hamachi and other links with limited upload.
   - Each broadcast reuses the same UDP port, so a connect string you shared keeps working.
2. **Watch:** broadcasters on your network appear under **Broadcasting on this network**. Click one to watch it; click another to switch. You only ever watch one stream at a time.
3. **Saved:** everyone you have watched is remembered (★). When they aren't showing up in the list, e.g. discovery doesn't reach them, they appear under **Saved**. Click to connect at their last address, or 🗑 to forget them.
4. **Check who you are watching:** every peer has an ID such as `7268-E22A`, shown next to its name. It is derived from that peer's certificate, and the connection is refused if the broadcaster can't prove it owns that ID.
   - If two broadcasters share a name, a ⚠ appears; ask the person you expect for their ID (shown in the top bar of their app).
   - A red ⚠ means someone is using the name of a saved contact with a *different* ID: a reinstall, or an impersonation attempt.

Your display name can be changed with the ✏ button next to it (not while broadcasting). Name and quality preset are remembered.

## Building

Requirements:

- Rust stable (edition 2024; tested with 1.97).
- A C/C++ compiler, because OpenH264 is built from source:
  - Windows: Visual Studio 2022 Build Tools with the "Desktop development with C++" workload.
  - macOS: Xcode Command Line Tools.
  - Linux: `build-essential`, plus PipeWire and D-Bus development packages for screen capture (`libpipewire-0.3-dev`, `libdbus-1-dev`, `libclang-dev`) and the usual eframe dependencies (`libxkbcommon-dev`, `libwayland-dev`, `libgl1-mesa-dev`).
- Optional: [NASM](https://www.nasm.us/) on `PATH`. OpenH264 then builds its SIMD assembly and encodes noticeably faster. Without it, it silently falls back to plain C, which is what the numbers below were measured with.

```sh
cargo build --release
./target/release/p2pss        # p2pss.exe on Windows
```

The result is a single self-contained executable.

### Command-line options

| Option | Purpose |
|---|---|
| `--name <NAME>` | Name shown to others (defaults to the saved name, then the computer name). |
| `--profile <NAME>` | Separate identity, settings and logs. Lets you run several instances on one machine. |
| `--broadcast <SOURCE>` | Start broadcasting immediately: `test`, `monitor`, or part of a window title. |
| `--watch <NAME>` | Watch the first discovered broadcaster whose name contains `NAME`. |
| `--connect <IP:PORT#FINGERPRINT>` | Watch a broadcaster directly, bypassing discovery. The broadcaster's **Copy connect string** button produces this. |

Try it on one machine:

```sh
p2pss --profile a --name Alice --broadcast test
p2pss --profile b --name Bob --watch alice
```

### Network requirements

- Peers must be on the same subnet (mDNS does not cross routers). Discovery uses UDP port 5353 (multicast); video uses one random UDP port per broadcaster.
- Virtual LANs such as Hamachi or Radmin VPN work like a LAN: the broadcaster is reachable on the adapter's address (25.x / 26.x). Whether discovery works depends on the VPN forwarding multicast. When it doesn't, use the connect string once; the broadcaster is saved from then on. Over the internet, use the **Internet / VPN** preset.
- **Windows firewall:** the first run triggers a Windows Defender Firewall prompt. Allow the app on every network type you will use. Virtual LAN adapters (and many home networks) are classified *Public*, so tick **Public** too; otherwise discovery and incoming connections are blocked.
- If discovery is blocked (e.g. guest Wi-Fi with client isolation, some VPNs), the broadcaster uses **Copy connect string**, picks the network adapter the viewer shares with them, and the viewer pastes the string into **Connect manually**.

### Distributing a Windows build

`.cargo/config.toml` links the C runtime statically, and release builds use the Windows GUI subsystem (no console window), so `target/release/p2pss.exe` runs on any Windows 10 (2004+) or 11 PC with no extra installs. The binary isn't code-signed, so SmartScreen shows "Windows protected your PC" on first run (More info → Run anyway).

## Architecture

| Crate | Responsibility |
|---|---|
| `crates/capture` | Enumerate and capture monitors/windows as BGRA frames. Windows: Windows Graphics Capture via `windows-capture`. macOS/Linux: `scap` (ScreenCaptureKit / PipeWire portal). Includes a synthetic test pattern. |
| `crates/codec` | Fixed-size canvas (scale + letterbox) and H.264 encode/decode with OpenH264, behind `VideoEncoder`/`VideoDecoder` traits so hardware encoders can be added later. |
| `crates/net` | Peer identity, fingerprint-pinned TLS 1.3 over QUIC (`quinn`), wire protocol, `BroadcastServer`, `ViewerClient`. |
| `crates/discovery` | mDNS announce/browse (`mdns-sd`) with validation of untrusted announcements. |
| `crates/app` | `p2pss` binary: egui UI, controller, capture→encode and decode→display pipelines, viewer state machine, settings, logging. |

```
Broadcaster: capture ─▶ latest-frame slot ─▶ encoder thread (canvas → I420 → H.264)
             ─▶ broadcast channel ─▶ one task per viewer ─▶ QUIC unidirectional stream
Viewer:      QUIC stream ─▶ bounded queue ─▶ decoder thread (H.264 → RGBA) ─▶ latest-frame slot ─▶ GPU texture
```

- The canvas size is fixed when capture starts, so resizing a shared window letterboxes instead of changing the stream resolution.
- A viewer that falls behind skips ahead to the next keyframe instead of accumulating delay. Keyframes are produced on demand: when a viewer joins, lags, or reports a decode error.
- Protocol: viewers open a control stream (`Hello` → `Welcome`, then `RequestKeyframe`); the broadcaster opens a video stream carrying `[seq, capture time, keyframe flag, length] + H.264 Annex-B`. Why a session ended (stopped, source closed, busy, version mismatch) travels as a QUIC application close code.

## Security

Mapping to [abuse-cases.md](abuse-cases.md):

| Abuse case | MVP mitigation |
|---|---|
| AC-01 Rogue broadcaster impersonation | Each peer's ID is the SHA-256 of its long-lived certificate. The viewer pins the fingerprint from the announcement and aborts the TLS handshake on mismatch ("Identity check failed"). Duplicate names are flagged in the UI, and a saved contact's name showing up with a different ID is flagged as a possible impersonation (trust on first use). |
| AC-03 Stream tampering · AC-05 Eavesdropping | All traffic is QUIC with TLS 1.3 (AEAD); tampered packets are dropped and nothing is sent in the clear. |
| AC-04 Repudiation | Daily-rotated session logs record broadcasts, viewers (name and address), watch sessions and end reasons. |
| AC-06 Enumeration | Peers are only announced while broadcasting. |
| AC-07 UDP flood / oversized messages | Hard size limits on control messages (64 KiB) and frames (8 MiB) checked before allocation, 8-viewer cap, handshake timeouts; QUIC discards unauthenticated packets cheaply. |
| AC-08 Fake announcement flood | Announcements are validated (version, 64-hex fingerprint, port, IPv4 addresses, sanitized name) and capped at 64 peers. |
| AC-10 Capture beyond the selection | Windows are captured through the OS window-capture API, never by cropping a desktop capture. |

Not yet addressed: AC-02 (viewers are not authenticated; any peer on the LAN can watch a broadcast) and AC-09 (the H.264 decoder is C code running in-process; sandboxing and fuzzing are future work). Treat broadcasts as visible to everyone on the network.

## Performance

Measured on the development machine (Windows 11, 12-thread desktop CPU, OpenH264 built without NASM), both ends on the same machine:

| Scenario | Result |
|---|---|
| 1080p30 monitor broadcast | Broadcaster 3.9 % total CPU (47 % of one core); viewer 2.3 % |
| Encode time, 1080p | ~9–10 ms typical desktop; 22 ms worst case (full-screen scrolling text) |
| Encode time, 720p | ~5 ms typical; 11 ms worst case |
| Capture → decoded frame | 5 ms (test pattern), 15–18 ms (monitor), ~40 ms (window) |
| Discovery → watching | ~1.3 s after launch |
| Broadcaster stops → viewer notified | ~2 ms (graceful) · ~6 s (crash, QUIC idle timeout) |

Glass-to-glass latency also includes display refresh (one or two frames). To measure it, show a millisecond stopwatch on the broadcaster and put the viewer next to it on the same screen.

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs formatting, clippy and the tests on Windows, macOS and Linux. It hasn't run yet because the repository is local only; it's the quickest way to compile-check the macOS/Linux code once the repo is hosted.

Automated tests (59) cover:
- the wire protocol, including malformed and oversized input;
- identity persistence and fingerprint rejection;
- real QUIC sessions on localhost: ordering, keyframe-first, stop reasons, viewer cap, version mismatch, lagging viewers, switching, unreachable peers, which address answered;
- announcement validation and the peer-table cap, plus a real mDNS round trip;
- saved contacts (merge, cap, corrupt files, ID-change detection) and the sticky broadcast port;
- the viewer state machine against the use-case diagram;
- H.264 round trips, canvas letterboxing, and the Internet preset holding its budget on scrolling text without dropping frames.

Developer tools: `cargo run --release -p p2pss-capture --example probe` (list sources, measure capture rate) and `cargo run --release -p p2pss-codec --example bench [source|test|scroll] [seconds] [720|1080|internet]`.

### Manual checklist

- [x] A broadcasts a monitor; B discovers it via mDNS and watches at ~30 fps.
- [x] A shares a window; resizing letterboxes it, minimizing holds the last frame, closing it shows "The shared window was closed" on B.
- [x] B leaves → A pauses capture.
- [x] A is killed → B returns to the list (~6 s); A closed normally → B is told immediately and A disappears from the list.
- [x] Two different machines over Radmin VPN, with discovery and pasted connect strings.
- [x] Connecting once by connect string saves the broadcaster; they keep the same port after a restart; a different identity using their name is flagged.
- [ ] Reconnecting from **Saved** by clicking, and switching broadcasters by clicking (both covered by automated tests; click once by hand).
- [ ] Renaming yourself with ✏ persists across restarts.
- [ ] A session over Radmin VPN with the **Internet / VPN** preset while scrolling or playing video.
- [ ] macOS and Linux (see below).

## Known limitations

- **macOS and Linux are untested.** On macOS, grant Screen Recording permission (System Settings → Privacy & Security) and restart the app. On Linux/Wayland the source is chosen in the system's screen-share dialog.
- IPv4 only.
- Software encoding only (OpenH264). Hardware encoders (NVENC, Quick Sync, VideoToolbox) are a planned backend for the encoder trait.
- No audio yet.
- The window list may include a few invisible system windows.
- OpenH264 built from source is not covered by Cisco's patent license, which only applies to Cisco's prebuilt binary. That's fine for personal LAN use; distribution would need the prebuilt library, which the `openh264` crate can load.

## Where data is kept

In the platform's application-data directory; on Windows, `%APPDATA%\P2P Screen Share\data`. `--profile x` uses `profiles\x` inside it. The path is printed on startup (`starting … dir=…`).

- `identity.cert.der`, `identity.key.der`: this peer's identity. Deleting them creates a new ID.
- `settings.toml`: display name, quality preset and broadcast port.
- `contacts.toml`: saved broadcasters (ID, name, last working addresses).
- `logs/session.log.YYYY-MM-DD`: session log.

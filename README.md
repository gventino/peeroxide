<p align="center">
  <img src="assets/icon.png" alt="Peeroxide logo: a rusty pixel-art pier linking screens" width="160">
  <br>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-2024_edition-orange?logo=rust&amp;logoColor=white" alt="Rust 2024 edition"></a>
</p>

# Peeroxide

Peer-to-peer screen sharing for a local network, written in Rust. Anyone on the LAN can broadcast a monitor or a single window, several people can broadcast at the same time, and each viewer watches one stream at a time. There is no server: peers find each other with mDNS and stream directly over encrypted QUIC.

**Status: MVP.** Video, plus optional audio. Sharing audio needs a Windows broadcaster (Windows 10 2004+ or 11); playback is built for every platform. Verified on Windows 11. macOS and Linux code paths exist but have not been run yet.

Target platforms: Windows 10 and 11, macOS, and Linux on both X11 and Wayland. Windows comes first; macOS and Linux follow in 0.7 (see the [roadmap](docs/roadmap.md)).

Each release so far can't talk to the one before, so everyone needs to update together. 0.4 added audio and can't talk to 0.3 (both sides show "incompatible version"). 0.3 can't talk to 0.2 or earlier (released as "P2P Screen Share"), because the protocol and discovery names changed with the rename.

Design documents: [functional requirements](docs/functional-requirements.md) · [non-functional requirements](docs/non-functional-requirements.md) · [use cases](docs/use-cases.md) · [abuse cases (STRIDE)](docs/abuse-cases.md) · [roadmap](docs/roadmap.md) · [releasing](docs/releasing.md)

## Using it

1. **Broadcast:** pick a source (a monitor, a window, or the built-in test pattern) and a quality preset, then press **Start broadcasting**. Capture and encoding only run while at least one person is watching.
   - Presets: **1080p · 30 fps** (~8 Mbps), **720p · 30 fps** (~4 Mbps), and **Internet / VPN · 720p · 20 fps** (~2 Mbps) for Radmin VPN, Hamachi and other links with limited upload.
   - Each broadcast reuses the same UDP port, so a connect string you shared keeps working.
   - **Share audio** (off by default, remembered) adds sound to the broadcast. It says exactly what it captures: for a window, only that app's sound; for a monitor, all sound on the computer except Peeroxide itself (so broadcasting while watching someone never feeds their stream back). The microphone is never captured. The choice is fixed for the broadcast; stop and start again to change it.
2. **Watch:** broadcasters on your network appear under **Broadcasting on this network**. Click one to watch it; click another to switch. You only ever watch one stream at a time.
   - If the broadcaster shares audio, a 🔊 mute button and a volume slider appear under the stream's name. They only affect what you hear, and are remembered. Audio plays in sync with the video.
3. **Saved:** everyone you have watched is remembered (★). When they aren't showing up in the list, e.g. discovery doesn't reach them, they appear under **Saved**. Click to connect at their last address, or 🗑 to forget them.
4. **Check who you are watching:** every peer has an ID such as `7268-E22A`, shown next to its name. It is derived from that peer's certificate, and the connection is refused if the broadcaster can't prove it owns that ID.
   - If two broadcasters share a name, a ⚠ appears; ask the person you expect for their ID (shown in the top bar of their app).
   - A red ⚠ means someone is using the name of a saved contact with a *different* ID: a reinstall, or an impersonation attempt.

Your display name can be changed with the ✏ button next to it (not while broadcasting). Name, quality preset, the audio choice, and volume/mute are remembered.

## Updates

Every time it opens, Peeroxide checks for a newer version, like Discord or Steam do. If there is one, it downloads it (with a progress bar and a **Skip** button to open right away instead), checks it, replaces itself in the same folder, and restarts. "Updated to X" then shows in the top bar.

- **Only genuine updates are installed.** Each release is signed with a key that never leaves the maintainer's computer. The app checks that signature, and that the package is exactly the version it claims to be, before touching anything (AC-12). A package that fails the check is thrown away, and the app says so.
- **It never gets in the way.** Offline, or GitHub unreachable: the app opens within about 5 seconds with a quiet note. Any error leaves the installed version as it was.
- **Same folder, same permissions.** The firewall permission you gave keeps working after an update.
- **A folder it can't write to** (such as Program Files): a note offers the download page instead.
- **Only Windows packages are published for now.** On macOS and Linux the app just opens.
- **To turn it off:** start with `--no-update`, or set the environment variable `PEEROXIDE_NO_UPDATE=1` (`0`, `false`, `off` or `no` leave the check on). Development builds (`cargo run`) never update themselves.
- **Privacy:** the check tells GitHub your IP address and the app version, nothing else (AC-13).
- The first version with the updater has to be installed by hand once; later ones arrive by themselves.

How releases are signed and published: [docs/releasing.md](docs/releasing.md).

## Building

Requirements:

- Rust stable (edition 2024; tested with 1.97).
- A C/C++ compiler, because OpenH264 is built from source:
  - Windows: Visual Studio 2022 Build Tools with the "Desktop development with C++" workload.
  - macOS: Xcode Command Line Tools.
  - Linux: `build-essential`, plus PipeWire and D-Bus development packages for screen capture (`libpipewire-0.3-dev`, `libdbus-1-dev`, `libclang-dev`), ALSA for audio playback (`libasound2-dev`), and the usual eframe dependencies (`libxkbcommon-dev`, `libwayland-dev`, `libgl1-mesa-dev`).
- Optional: [NASM](https://www.nasm.us/) on `PATH`. OpenH264 then builds its SIMD assembly and encodes noticeably faster. Without it, it silently falls back to plain C, which is what the numbers below were measured with.

```sh
cargo build --release
./target/release/peeroxide        # peeroxide.exe on Windows
```

The result is a single self-contained executable.

### Common tasks

The [`Justfile`](Justfile) wraps the usual commands; install [just](https://just.systems) with `cargo install just` (or `winget install Casey.Just`) and run `just` to list them. The main ones:

| Command | Does |
|---|---|
| `just run <options>` | Run the app, e.g. `just run --profile a --broadcast test --share-audio`. |
| `just demo` | Two instances on this machine: Alice broadcasts the test pattern with sound, Bob watches. |
| `just check` | What CI checks: formatting, clippy (warnings as errors) and the tests. |
| `just package [label]` | Windows: release build plus quickstart, zipped into `dist/peeroxide-<version>[-<label>]-windows-x64.zip` and signed for the updater (`.minisig`). |
| `just release-keygen` | One time: create the release signing key (see [releasing](docs/releasing.md)). |
| `just publish <notes.md>` | Publish the packaged release, zip and signature, as a GitHub pre-release. |
| `just serve-release <zip>` | Pretend to be GitHub on this PC, to test the self-update without publishing anything. |
| `just probe-audio`, `just probe-capture`, `just bench` | The developer tools listed under Testing. |

### Command-line options

| Option | Purpose |
|---|---|
| `--name <NAME>` | Name shown to others (defaults to the saved name, then the computer name). |
| `--no-update` | Don't check for updates at start (see [Updates](#updates)). |
| `--profile <NAME>` | Separate identity, settings and logs. Lets you run several instances on one machine. |
| `--broadcast <SOURCE>` | Start broadcasting immediately: `test`, `monitor`, or part of a window title. |
| `--share-audio` | With `--broadcast`: turn on **Share audio** (remembered, like the checkbox). With `test`, the audio is a beep in step with a flashing square. |
| `--watch <NAME>` | Watch the first discovered broadcaster whose name contains `NAME`. |
| `--connect <IP:PORT#FINGERPRINT>` | Watch a broadcaster directly, bypassing discovery. The broadcaster's **Copy connect string** button produces this. |

Try it on one machine:

```sh
peeroxide --profile a --name Alice --broadcast test --share-audio
peeroxide --profile b --name Bob --watch alice
```

Bob sees the test pattern and hears a beep every second while its top-right square flashes.

### Network requirements

- Peers must be on the same subnet (mDNS does not cross routers). Discovery uses UDP port 5353 (multicast); video uses one random UDP port per broadcaster.
- Virtual LANs such as Hamachi or Radmin VPN work like a LAN: the broadcaster is reachable on the adapter's address (25.x / 26.x). Whether discovery works depends on the VPN forwarding multicast. When it doesn't, use the connect string once; the broadcaster is saved from then on. Over the internet, use the **Internet / VPN** preset.
- **Windows firewall:** the first run triggers a Windows Defender Firewall prompt. Allow the app on every network type you will use. Virtual LAN adapters (and many home networks) are classified *Public*, so tick **Public** too; otherwise discovery and incoming connections are blocked.
- If discovery is blocked (e.g. guest Wi-Fi with client isolation, some VPNs), the broadcaster uses **Copy connect string**, picks the network adapter the viewer shares with them, and the viewer pastes the string into **Connect manually**.

### Distributing a Windows build

`.cargo/config.toml` links the C runtime statically, and release builds use the Windows GUI subsystem (no console window), so `target/release/peeroxide.exe` runs on any Windows 10 (2004+) or 11 PC with no extra installs. The exe carries the Peeroxide icon (`assets/`) and version details (Properties → Details, Task Manager), embedded by `crates/app/build.rs`.

**"Windows protected your PC".** The binary isn't code-signed, so Microsoft Defender SmartScreen warns the first time someone runs a downloaded copy. It only warns about files carrying Windows' "downloaded from the internet" mark. Testers can either click More info → Run anyway, or clear the mark first: right-click the zip (or the exe) → Properties → tick **Unblock** → OK. Updates installed by the app itself carry no mark, so the warning only ever appears on the first manual install. Removing it for good needs code signing (see the [roadmap](docs/roadmap.md), 0.8).

## Architecture

| Crate | Responsibility |
|---|---|
| `crates/capture` | Enumerate and capture monitors/windows as BGRA frames. Windows: Windows Graphics Capture via `windows-capture`. macOS/Linux: `scap` (ScreenCaptureKit / PipeWire portal). Includes a synthetic test pattern. |
| `crates/codec` | Fixed-size canvas (scale + letterbox) and H.264 encode/decode with OpenH264, behind `VideoEncoder`/`VideoDecoder` traits so hardware encoders can be added later. Opus audio (pure Rust, `opus-rs`) behind `AudioEncoder`/`AudioDecoder`. |
| `crates/audio` | Audio capture: Windows process loopback via `wasapi` (one app's process tree, or everything except Peeroxide), plus a test tone. Playback via `cpal` with a lock-free ring buffer and volume/mute. The playout scheduler that keeps audio in sync with video. |
| `crates/net` | Peer identity, fingerprint-pinned TLS 1.3 over QUIC (`quinn`), wire protocol, `BroadcastServer`, `ViewerClient`. |
| `crates/discovery` | mDNS announce/browse (`mdns-sd`) with validation of untrusted announcements. |
| `crates/update` | Self-update: GitHub release lookup (`ureq`, rustls + the OS certificate store), download with size caps, minisign signature check, and replacing the running exe (`self-replace`). Also the `release-sign` and `serve-release` tools. |
| `crates/app` | `peeroxide` binary: egui UI, controller, capture→encode and decode→display pipelines (video and audio), viewer state machine, settings, logging. |

```
Broadcaster: capture ─▶ latest-frame slot ─▶ encoder thread (canvas → I420 → H.264)
             ─▶ broadcast channel ─▶ one task per viewer ─▶ QUIC unidirectional stream
             audio capture ─▶ audio encoder thread (20 ms frames → Opus)
             ─▶ broadcast channel ─▶ one task per viewer ─▶ second stream, sent ahead of video
Viewer:      QUIC stream ─▶ bounded queue ─▶ decoder thread (H.264 → RGBA) ─▶ latest-frame slot ─▶ GPU texture
             audio stream ─▶ bounded queue ─▶ audio thread (Opus → PCM → playout) ─▶ ring buffer ─▶ device
```

- The canvas size is fixed when capture starts, so resizing a shared window letterboxes instead of changing the stream resolution.
- A viewer that falls behind skips ahead to the next keyframe instead of accumulating delay. Keyframes are produced on demand: when a viewer joins, lags, or reports a decode error.
- Protocol (version 2): viewers open a control stream (`Hello` → `Welcome`, which says whether audio is shared, then `RequestKeyframe`). The broadcaster opens a video stream carrying `[seq, capture time, keyframe flag, length] + H.264 Annex-B` and, with audio, an audio stream carrying `[seq, capture time, length] + Opus`; each stream starts with a kind byte. Why a session ended (stopped, source closed, busy, version mismatch) travels as a QUIC application close code. In the code, the close codes and stream kinds are enums decoded with strum's `FromRepr`; their numbers never change, because older peers read them.
- Audio/video sync: both streams carry the broadcaster's capture time. The viewer compares `local time − capture time` for the video being shown and for arriving audio; the unknown clock difference cancels out. It then delays audio (never video) to match, on top of a jitter margin of at least 40 ms. Drift and gaps are absorbed with short skips or silences.
- Audio never takes video down: if audio can't be captured, the broadcast goes out video-only with a note; a broken audio stream or missing output device only silences audio.
- Threads: capture, video encode, audio encode, video decode and audio decode each run on their own thread; the network runs on Tokio. Per-frame pixel work that splits well (converting macOS/Linux captures to BGRA, and `windows-capture`'s copy of padded frames) uses rayon, with a pool sized at start: one thread on Windows, two elsewhere. rayon's default of one thread per core spun more CPU than it saved on jobs of a few milliseconds, so scaling the canvas stays on one thread. A paused encoder is woken through a crossbeam channel, and the viewer builds each image on the decoder thread so the UI thread only uploads it.
- Errors: code that can fail returns `anyhow::Result`, with context saying what it was doing (which file, which socket), and errors are logged and shown with `{e:#}` so their causes aren't lost. Typed errors (`thiserror`) remain only where the caller acts on the kind of error: `UpdateError` (which note the updater shows), `CaptureError` (a source that's gone ends the broadcast as "closed", not as a failure) and the wire protocol's `ProtocolError`. The developer tools (`release-sign`, `serve-release`, `probe`, `bench`, `audio-probe`) print the whole cause chain and exit with code 1 when they fail.

## Security

Mapping to [abuse-cases.md](docs/abuse-cases.md):

| Abuse case | MVP mitigation |
|---|---|
| AC-01 Rogue broadcaster impersonation | Each peer's ID is the SHA-256 of its long-lived certificate. The viewer pins the fingerprint from the announcement and aborts the TLS handshake on mismatch ("Identity check failed"). Duplicate names are flagged in the UI, and a saved contact's name showing up with a different ID is flagged as a possible impersonation (trust on first use). |
| AC-03 Stream tampering · AC-05 Eavesdropping | All traffic is QUIC with TLS 1.3 (AEAD); tampered packets are dropped and nothing is sent in the clear. |
| AC-04 Repudiation | Daily-rotated session logs record broadcasts, viewers (name and address), watch sessions and end reasons. |
| AC-06 Enumeration | Peers are only announced while broadcasting. |
| AC-07 UDP flood / oversized messages | Hard size limits on control messages (64 KiB), frames (8 MiB) and audio packets (4 KiB) checked before allocation, 8-viewer cap, handshake timeouts; a broadcaster may open at most two streams to a viewer and viewers none; QUIC discards unauthenticated packets cheaply. |
| AC-08 Fake announcement flood | Announcements are validated (version, 64-hex fingerprint, port, IPv4 addresses, sanitized name) and capped at 64 peers. |
| AC-10 Capture beyond the selection | Windows are captured through the OS window-capture API, never by cropping a desktop capture. A window's audio is captured through the OS per-process loopback API (that app's process tree only), never by filtering the whole system's sound. The microphone is never captured. |
| AC-11 Unintended audio disclosure | Audio is off by default and chosen per broadcast; the UI states exactly what is captured; a window shares only its app's sound; Peeroxide's own playback is excluded; nothing is captured while no one watches. |
| AC-12 Malicious update | Updates must carry a minisign signature from the release key, which is kept offline and never on GitHub. The signed comment must name the exact package, and only strictly newer versions are accepted, so neither an old package nor a downgrade can be slipped in. HTTPS only, with size caps; nothing is extracted before verification, and the package's own paths are never used. |
| AC-13 Update check exposure | One request per start, carrying only the app version; `--no-update` turns it off. |

Not yet addressed: AC-02 (viewers are not authenticated; any peer on the LAN can watch, and hear, a broadcast) and AC-09 (the H.264 decoder is C code running in-process; sandboxing and fuzzing are future work). The Opus decoder is pure Rust on its own thread with panics caught, and survives a 5,000-packet garbage test, but it isn't fuzzed or sandboxed either. Treat broadcasts as visible and audible to everyone on the network.

## Performance

Measured on the development machine (Windows 11, 12-thread desktop CPU, OpenH264 built without NASM), both ends on the same machine:

| Scenario | Result |
|---|---|
| 1080p30 monitor broadcast | Broadcaster 3.9 % total CPU (47 % of one core); viewer 2.3 % |
| Encode time, 1080p | ~9–10 ms typical desktop; 22 ms worst case (full-screen scrolling text) |
| Encode time, 720p | ~5 ms typical; 11 ms worst case |
| Scaling a 1080p screen to the 720p canvas | 3.5 ms per frame (one thread; rayon was faster but cost 2–5× the CPU) |
| Sharing a window, viewer on the same PC (Windows 11 23H2) | Broadcaster 29–36 % of one core; 68–100 % before rayon's pool was sized |
| Capture → decoded frame | 5 ms (test pattern), 15–18 ms (monitor), ~40 ms (window) |
| Discovery → watching | ~1.3 s after launch |
| Broadcaster stops → viewer notified | ~2 ms (graceful) · ~6 s (crash, QUIC idle timeout) |
| Opus, 48 kHz stereo at 128 kbps | 10 s of audio: encode 46 ms, decode 28 ms (under 1 % of one core) |
| Audio vs. video, test pattern on one machine | Audio plays ~64 ms after the video (20 ms Opus frames + 40 ms jitter margin; the test pattern's video path is unusually fast, 6 ms) |

Glass-to-glass latency also includes display refresh (one or two frames). To measure it, show a millisecond stopwatch on the broadcaster and put the viewer next to it on the same screen.

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs formatting, clippy and the tests on Windows, macOS and Linux on every push. It is also the only compile check of the macOS/Linux code so far.

Automated tests (130) cover:
- the wire protocol, including malformed and oversized input, for video and audio, and the fixed values of close codes and stream kinds;
- identity persistence and fingerprint rejection;
- real QUIC sessions on localhost: ordering, keyframe-first, stop reasons, viewer cap, version mismatch (including 0.3 peers), lagging viewers, switching, unreachable peers, which address answered;
- audio over QUIC: in order next to video, absent when not shared, a lagging viewer skipping ahead, and broken or unexpected streams leaving the video running;
- Opus round trips (tone levels and stereo separation at both bitrates, silence, garbage packets, loss concealment);
- the playout scheduler (jitter margin, sync with slower and faster video, the 200 ms cap, drift corrections, gaps), 20 ms framing of captured audio, the resampler, the volume control (lock-free) and the test tone;
- the encoder's wake-up (one sent just before it sleeps isn't lost);
- announcement validation and the peer-table cap, plus a real mDNS round trip;
- the self-update, against a local server standing in for GitHub:
  - choosing the release: newest newer signed one, pre-releases included; drafts, older or equal versions, unsigned packages and other platforms ignored;
  - signatures: valid, tampered, wrong key, replayed under another name, edited signed comment;
  - the package: extracting the exe, rejecting archives with none or several, and paths that try to escape;
  - behaviour: size caps, skip, rate limits, a silent server (5 s), a second instance, insecure URLs;
  - restart options, and the notes shown after a failure;
- saved contacts (merge, cap, corrupt files, ID-change detection), the sticky broadcast port, and audio settings defaults for older settings files;
- the viewer state machine against the use-case diagram;
- the command-line options: clap's own consistency check, contradictory options refused, and the update settings' defaults;
- H.264 round trips, canvas letterboxing, and the Internet preset holding its budget on scrolling text without dropping frames.

Developer tools: `cargo run --release -p peeroxide-capture --example probe` (list sources, measure capture rate), `cargo run --release -p peeroxide-codec --example bench [source|test|scroll] [seconds] [720|1080|internet]` and `cargo run --release -p peeroxide-audio --example audio-probe [system|tone|PID] [seconds] [--play]` (record an audio source to `audio-probe.wav` and check the output device). Each tool, like `release-sign` and `serve-release`, explains its options with `--help` (after `--` with `cargo run`).

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
- [x] Test pattern with **Share audio** on one machine, viewer muted: the viewer receives, decodes and plays the stream (A/V offset ~+64 ms, no underruns, no bad packets); without it, the viewer shows "The broadcaster isn't sharing audio"; audio capture pauses when the viewer leaves.
- [x] System loopback capture (everything except Peeroxide) starts on Windows 11 and delivers 10 ms chunks (audio probe; nothing was playing, so only silence was recorded).
- [ ] Hearing it: the beep matches the flashing square; the volume slider and mute work and are remembered.
- [ ] Sharing a browser window: only the browser's sound is heard, not other apps'.
- [ ] Sharing a monitor while also watching someone: no echo or feedback.
- [x] Two machines on the LAN with audio.
- [ ] Audio over Radmin VPN with the **Internet / VPN** preset.
- [x] Self-update on one PC with a throwaway key and a local server: 0.4.99 updated itself to 0.5.0 in about 2 s and restarted showing "Updated to 0.5.0"; started again, it found nothing newer; a tampered package was rejected and the app opened on its old version; with no server it opened in about 2 s.
- [x] A release build with the real key checks GitHub itself over HTTPS (Windows certificate store) and reports "up to date" in about 0.4 s.
- [ ] Self-update by hand: Skip, a read-only folder, two profiles starting at once, and a real update through GitHub (the release after the first one with the updater).
- [ ] macOS and Linux (see below).

## Known limitations

- **macOS and Linux are untested.** On macOS, grant Screen Recording permission (System Settings → Privacy & Security) and restart the app. On Linux/Wayland the source is chosen in the system's screen-share dialog.
- IPv4 only.
- Software encoding only (OpenH264). Hardware encoders (NVENC, Quick Sync, VideoToolbox) are a planned backend for the encoder trait.
- Audio can only be shared from Windows (10 2004 or later, or 11) for now; macOS and Linux capture is planned for 0.7. Playback is built for all three but has only been tested on Windows. Audio on Windows 10 has not been tested yet either.
- Windows Store (UWP) apps: their windows belong to `ApplicationFrameHost.exe`, so sharing such a window shares none of its sound. Share the monitor instead.
- Audio is chosen before a broadcast starts; there is no mute while live.
- Self-update: Windows only, and the first version with the updater has to be installed by hand. Some antivirus programs distrust apps that replace their own executable; if yours blocks it, download the new version by hand.
- The window list may include a few invisible system windows.
- OpenH264 built from source is not covered by Cisco's patent license, which only applies to Cisco's prebuilt binary. That's fine for personal LAN use; distribution would need the prebuilt library, which the `openh264` crate can load.

## Where data is kept

In the platform's application-data directory; on Windows, `%APPDATA%\Peeroxide\data`. `--profile x` uses `profiles\x` inside it. The path is printed on startup (`starting … dir=…`). Data from versions up to 0.2, when the app was called "P2P Screen Share", is moved there on first run.

- `identity.cert.der`, `identity.key.der`: this peer's identity. Deleting them creates a new ID.
- `settings.toml`: display name, quality preset, broadcast port, whether to share audio, and volume/mute.
- `contacts.toml`: saved broadcasters (ID, name, last working addresses).
- `logs/session.log.YYYY-MM-DD`: session log, including update checks and installs.

While updating, `peeroxide.update.*` files briefly appear next to `peeroxide.exe`; they are removed when it finishes.

## License

Licensed under the [Apache License 2.0](LICENSE). The software is provided "as is", without warranty of any kind.

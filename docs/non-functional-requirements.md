# Non-Functional Requirements

This document lists the non-functional requirements (NFR) for Peeroxide, grouped by quality attribute.

```mermaid
mindmap
  root((Non-Functional<br/>Requirements))
    Performance
      Latency
      Frame rate
      Resource efficiency
    Reliability
      Packet loss tolerance
      Fault isolation
    Portability
      Single codebase
      Self-contained binaries
    Usability
      Zero-config discovery
      Responsive UI
    Security
      Transport confidentiality
      Peer authenticity
    Maintainability
      Modular architecture
```

## Performance

| ID | Requirement | Description |
|----|-------------|-------------|
| NFR-01 | Latency | End-to-end (capture-to-display) latency should stay under approximately 150 ms on a typical wired/Wi-Fi LAN. |
| NFR-02 | Frame Rate | The system should sustain at least 30 FPS at 1080p on typical consumer hardware from the last ~5 years. |
| NFR-03 | Resource Efficiency | The application should use hardware-accelerated encode/decode (e.g., NVENC, Quick Sync, VideoToolbox, VAAPI) when available, to minimize CPU load during capture, encoding, and decoding. |

## Reliability

| ID | Requirement | Description |
|----|-------------|-------------|
| NFR-04 | Packet Loss Tolerance | The video pipeline should degrade gracefully (minor visual artifacts, momentary freeze) rather than crash or hang when UDP packet loss occurs. |
| NFR-05 | Fault Isolation | A failure or crash in one broadcaster/viewer session should not affect other unrelated sessions running on the same peer. |

## Portability

| ID | Requirement | Description |
|----|-------------|-------------|
| NFR-06 | Cross-Platform Codebase | The application shall be built from a single Rust codebase that compiles and runs natively on Windows, macOS, and Linux. |
| NFR-07 | Minimal Runtime Dependencies | The application should be distributable as a self-contained binary per platform, avoiding mandatory external runtime installs (e.g., no separately installed system-wide FFmpeg requirement). |

## Usability

| ID | Requirement | Description |
|----|-------------|-------------|
| NFR-08 | Zero-Configuration Discovery | Users should be able to find and connect to peers without manually entering IP addresses or ports. |
| NFR-09 | Responsive UI | The GUI shall remain responsive (non-blocking) while capturing, encoding, decoding, streaming, or discovering peers. |

## Security

| ID | Requirement | Description |
|----|-------------|-------------|
| NFR-10 | Transport Confidentiality | Video and control traffic should be protected against passive eavesdropping by other devices on the LAN (see `abuse-cases.md`, Information Disclosure). |
| NFR-11 | Peer Authenticity | The system should provide a way for a viewer to verify that a broadcaster's advertised identity has not been spoofed (see `abuse-cases.md`, Spoofing). |

## Maintainability

| ID | Requirement | Description |
|----|-------------|-------------|
| NFR-12 | Modular Architecture | Capture, encoding, networking, discovery, and GUI concerns shall be separated into independent modules/crates to ease independent testing and evolution (e.g., swapping the GUI framework or codec later without touching networking code). |

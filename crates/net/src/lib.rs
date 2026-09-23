//! QUIC transport: broadcaster server, viewer sessions, peer identity and wire protocol.
//!
//! Every peer has a long-lived self-signed certificate; its SHA-256 [`Fingerprint`] is the peer id.
//! Viewers only accept a server whose certificate matches the fingerprint they were told about
//! (via mDNS), so the TLS 1.3 channel is both encrypted and bound to that identity.

mod client;
mod identity;
mod protocol;
mod server;
mod tls;

#[cfg(test)]
mod tests;

pub use client::{SessionEnd, SessionEvent, SessionHandle, SessionId, ViewerClient};
pub use identity::{Fingerprint, Identity};
pub use protocol::{AudioPacket, PROTOCOL_VERSION, VideoFrame};
pub use server::{BroadcastServer, ServerOptions, StopReason};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TLS configuration error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("{0}")]
    Config(String),
}

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use quinn::{Connection, ConnectionError, Endpoint, VarInt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::protocol::{
    ClientMsg, PROTOCOL_VERSION, ServerMsg, VideoFrame, close, read_frame, read_msg, write_msg,
};
use crate::{Fingerprint, NetError, tls};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEnd {
    /// Closed on our side (stopped watching or switched broadcaster).
    Closed,
    BroadcastStopped,
    SourceClosed,
    Busy,
    VersionMismatch,
    /// The peer's certificate does not match the fingerprint it announced.
    IdentityMismatch,
    Unreachable(String),
    ConnectionLost(String),
    ProtocolError(String),
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    Connected { broadcaster_name: String },
    Ended(SessionEnd),
}

/// Opens viewer sessions. One per process is enough. Must be created inside a Tokio runtime.
pub struct ViewerClient {
    endpoint: Endpoint,
    next_id: AtomicU64,
}

/// A running viewer session. Dropping it disconnects.
pub struct SessionHandle {
    id: SessionId,
    keyframes: mpsc::UnboundedSender<()>,
    cancel: Option<oneshot::Sender<()>>,
    _task: JoinHandle<()>,
}

impl SessionHandle {
    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn request_keyframe(&self) {
        let _ = self.keyframes.send(());
    }

    pub fn keyframe_requester(&self) -> impl Fn() + Send + Sync + 'static {
        let tx = self.keyframes.clone();
        move || {
            let _ = tx.send(());
        }
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        if let Some(c) = self.cancel.take() {
            let _ = c.send(());
        }
    }
}

impl ViewerClient {
    pub fn new() -> Result<Self, NetError> {
        let endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))?;
        Ok(Self {
            endpoint,
            next_id: AtomicU64::new(1),
        })
    }

    /// Closes all sessions and waits briefly so broadcasters learn about it immediately.
    pub async fn close(&self) {
        self.endpoint
            .close(VarInt::from_u32(close::VIEWER_LEFT), b"bye");
        let _ = timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }

    /// Connects to the broadcaster at the first reachable address whose certificate matches
    /// `fingerprint`. Frames are handed to `on_frame` in order; `on_event` reports progress.
    pub fn watch(
        &self,
        addrs: Vec<SocketAddr>,
        fingerprint: Fingerprint,
        viewer_name: String,
        on_frame: impl FnMut(VideoFrame) + Send + 'static,
        on_event: impl Fn(SessionId, SessionEvent) + Send + Sync + 'static,
    ) -> SessionHandle {
        let id = SessionId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (keyframes_tx, keyframes_rx) = mpsc::unbounded_channel();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let endpoint = self.endpoint.clone();
        let on_event = Arc::new(on_event);
        let task = tokio::spawn(async move {
            let events = {
                let on_event = on_event.clone();
                move |e| on_event(id, e)
            };
            let end = run(
                endpoint,
                addrs,
                fingerprint,
                viewer_name,
                on_frame,
                &events,
                keyframes_rx,
                cancel_rx,
            )
            .await;
            tracing::info!(?end, %fingerprint, "viewer session ended");
            events(SessionEvent::Ended(end));
        });
        SessionHandle {
            id,
            keyframes: keyframes_tx,
            cancel: Some(cancel_tx),
            _task: task,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    endpoint: Endpoint,
    addrs: Vec<SocketAddr>,
    fingerprint: Fingerprint,
    viewer_name: String,
    mut on_frame: impl FnMut(VideoFrame),
    events: &impl Fn(SessionEvent),
    keyframes: mpsc::UnboundedReceiver<()>,
    mut cancel: oneshot::Receiver<()>,
) -> SessionEnd {
    let conn = tokio::select! {
        r = connect_any(&endpoint, &addrs, fingerprint) => match r {
            Ok(c) => c,
            Err(end) => return end,
        },
        _ = &mut cancel => return SessionEnd::Closed,
    };
    // Closing the connection interrupts whatever the session is waiting on.
    let closer = {
        let conn = conn.clone();
        tokio::spawn(async move {
            let _ = cancel.await;
            conn.close(VarInt::from_u32(close::VIEWER_LEFT), b"bye");
        })
    };
    let end = session(&conn, viewer_name, &mut on_frame, events, keyframes).await;
    closer.abort();
    end
}

async fn connect_any(
    endpoint: &Endpoint,
    addrs: &[SocketAddr],
    fingerprint: Fingerprint,
) -> Result<Connection, SessionEnd> {
    let mut last_error = "no usable address".to_string();
    for addr in addrs.iter().filter(|a| a.is_ipv4()) {
        let (config, mismatch) = tls::client_config(fingerprint)
            .map_err(|e| SessionEnd::ProtocolError(e.to_string()))?;
        let connecting = match endpoint.connect_with(config, *addr, "p2pss.local") {
            Ok(c) => c,
            Err(e) => {
                last_error = format!("{addr}: {e}");
                continue;
            }
        };
        match timeout(CONNECT_TIMEOUT, connecting).await {
            Ok(Ok(conn)) => return Ok(conn),
            Ok(Err(e)) => {
                if mismatch.load(Ordering::Relaxed) {
                    return Err(SessionEnd::IdentityMismatch);
                }
                last_error = format!("{addr}: {e}");
            }
            Err(_) => last_error = format!("{addr}: timed out"),
        }
    }
    Err(SessionEnd::Unreachable(last_error))
}

async fn session(
    conn: &Connection,
    viewer_name: String,
    on_frame: &mut impl FnMut(VideoFrame),
    events: &impl Fn(SessionEvent),
    mut keyframes: mpsc::UnboundedReceiver<()>,
) -> SessionEnd {
    let handshake = async {
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
        write_msg(
            &mut send,
            &ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                viewer_name,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
        let welcome = read_msg::<_, ServerMsg>(&mut recv)
            .await
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((send, welcome))
    };
    let mut send = match timeout(HANDSHAKE_TIMEOUT, handshake).await {
        Ok(Ok((send, Some(ServerMsg::Welcome { broadcaster_name })))) => {
            events(SessionEvent::Connected { broadcaster_name });
            send
        }
        _ => return end_reason(conn).await,
    };

    let requests = tokio::spawn(async move {
        while keyframes.recv().await.is_some() {
            while keyframes.try_recv().is_ok() {}
            if write_msg(&mut send, &ClientMsg::RequestKeyframe)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    if let Ok(Ok(mut video)) = timeout(HANDSHAKE_TIMEOUT, conn.accept_uni()).await {
        while let Ok(Some(frame)) = read_frame(&mut video).await {
            on_frame(frame);
        }
    }
    requests.abort();
    end_reason(conn).await
}

async fn end_reason(conn: &Connection) -> SessionEnd {
    let Ok(reason) = timeout(Duration::from_secs(1), conn.closed()).await else {
        conn.close(VarInt::from_u32(close::PROTOCOL_ERROR), b"protocol");
        return SessionEnd::ProtocolError("stream ended while connected".into());
    };
    match reason {
        ConnectionError::ApplicationClosed(c) => {
            match u32::try_from(c.error_code.into_inner()).unwrap_or(u32::MAX) {
                close::BROADCAST_STOPPED => SessionEnd::BroadcastStopped,
                close::SOURCE_CLOSED => SessionEnd::SourceClosed,
                close::BUSY => SessionEnd::Busy,
                close::VERSION_MISMATCH => SessionEnd::VersionMismatch,
                close::VIEWER_LEFT => SessionEnd::ConnectionLost("closed by broadcaster".into()),
                code => SessionEnd::ProtocolError(format!("closed with code {code}")),
            }
        }
        ConnectionError::LocallyClosed => SessionEnd::Closed,
        ConnectionError::TimedOut => SessionEnd::ConnectionLost("timed out".into()),
        other => SessionEnd::ConnectionLost(other.to_string()),
    }
}

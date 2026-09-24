use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context;
use quinn::{Connection, ConnectionError, Endpoint, RecvStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::protocol::{
    AudioPacket, ClientMsg, CloseCode, PROTOCOL_VERSION, ServerMsg, StreamKind, VideoFrame,
    read_audio, read_frame, read_msg, read_stream_kind, write_msg,
};
use crate::{Fingerprint, tls};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(u64);

/// Why a viewer session ended. `Display` gives the sentence shown to the viewer.
#[derive(Clone, Debug, PartialEq, Eq, strum::Display)]
pub enum SessionEnd {
    /// Closed on our side (stopped watching or switched broadcaster).
    #[strum(to_string = "Stopped watching")]
    Closed,
    #[strum(to_string = "The broadcaster stopped sharing")]
    BroadcastStopped,
    #[strum(to_string = "The shared window was closed")]
    SourceClosed,
    #[strum(to_string = "The broadcaster has reached its viewer limit")]
    Busy,
    #[strum(to_string = "The broadcaster runs an incompatible version")]
    VersionMismatch,
    /// The peer's certificate does not match the fingerprint it announced.
    #[strum(
        to_string = "Identity check failed: the peer's certificate does not match its \
                     announcement (possible impersonation)"
    )]
    IdentityMismatch,
    #[strum(to_string = "Could not reach the broadcaster ({0})")]
    Unreachable(String),
    #[strum(to_string = "Connection lost ({0})")]
    ConnectionLost(String),
    #[strum(to_string = "Protocol error ({0})")]
    ProtocolError(String),
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// `remote` is the broadcaster address that answered (the first reachable one tried).
    /// `audio` says whether the broadcaster shares audio in this broadcast.
    Connected {
        broadcaster_name: String,
        remote: SocketAddr,
        audio: bool,
    },
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
    pub fn new() -> anyhow::Result<Self> {
        let endpoint = Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))
            .context("could not open a UDP socket for watching")?;
        Ok(Self {
            endpoint,
            next_id: AtomicU64::new(1),
        })
    }

    /// Closes all sessions and waits briefly so broadcasters learn about it immediately.
    pub async fn close(&self) {
        self.endpoint.close(CloseCode::ViewerLeft.into(), b"bye");
        let _ = timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }

    /// Connects to the broadcaster at the first reachable address whose certificate matches
    /// `fingerprint`. Frames are handed to `on_frame` in order, audio packets (if the broadcaster
    /// shares audio) to `on_audio` in order; `on_event` reports progress.
    pub fn watch(
        &self,
        addrs: Vec<SocketAddr>,
        fingerprint: Fingerprint,
        viewer_name: String,
        on_frame: impl FnMut(VideoFrame) + Send + 'static,
        on_audio: impl FnMut(AudioPacket) + Send + 'static,
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
                on_audio,
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
    on_audio: impl FnMut(AudioPacket) + Send + 'static,
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
            conn.close(CloseCode::ViewerLeft.into(), b"bye");
        })
    };
    let end = session(
        &conn,
        viewer_name,
        &mut on_frame,
        on_audio,
        events,
        keyframes,
    )
    .await;
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
            .map_err(|e| SessionEnd::ProtocolError(format!("{e:#}")))?;
        let connecting = match endpoint.connect_with(config, *addr, "peeroxide.local") {
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
    on_audio: impl FnMut(AudioPacket) + Send + 'static,
    events: &impl Fn(SessionEvent),
    mut keyframes: mpsc::UnboundedReceiver<()>,
) -> SessionEnd {
    let handshake = async {
        let (mut send, mut recv) = conn.open_bi().await?;
        write_msg(
            &mut send,
            &ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                viewer_name,
            },
        )
        .await?;
        let welcome = read_msg::<_, ServerMsg>(&mut recv).await?;
        anyhow::Ok((send, welcome))
    };
    let mut send = match timeout(HANDSHAKE_TIMEOUT, handshake).await {
        Ok(Ok((
            send,
            Some(ServerMsg::Welcome {
                broadcaster_name,
                audio,
            }),
        ))) => {
            events(SessionEvent::Connected {
                broadcaster_name,
                remote: conn.remote_address(),
                audio,
            });
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

    let (video_tx, video_rx) = oneshot::channel();
    let router = tokio::spawn(route_streams(conn.clone(), video_tx, on_audio));
    if let Ok(Ok(mut video)) = timeout(HANDSHAKE_TIMEOUT, video_rx).await {
        while let Ok(Some(frame)) = read_frame(&mut video).await {
            on_frame(frame);
        }
    }
    router.abort();
    requests.abort();
    end_reason(conn).await
}

/// Aborts a task when dropped, so it can't outlive the task that owns it.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Hands the video stream to the session and plays the audio stream into `on_audio`.
/// Only the video stream decides when a session ends; a broken audio stream just goes quiet,
/// and anything else the broadcaster opens is refused.
async fn route_streams(
    conn: Connection,
    video: oneshot::Sender<RecvStream>,
    on_audio: impl FnMut(AudioPacket) + Send + 'static,
) {
    let mut video = Some(video);
    let mut on_audio = Some(on_audio);
    let mut _audio = None;
    while let Ok(mut stream) = conn.accept_uni().await {
        let kind = timeout(HANDSHAKE_TIMEOUT, read_stream_kind(&mut stream)).await;
        match kind {
            Ok(Ok(Some(StreamKind::Video))) if video.is_some() => {
                if let Some(tx) = video.take() {
                    let _ = tx.send(stream);
                }
            }
            Ok(Ok(Some(StreamKind::Audio))) if on_audio.is_some() => {
                if let Some(mut on_audio) = on_audio.take() {
                    let conn = conn.clone();
                    _audio = Some(AbortOnDrop(tokio::spawn(async move {
                        loop {
                            match read_audio(&mut stream).await {
                                Ok(Some(packet)) => on_audio(packet),
                                Ok(None) => break,
                                // Ending the session also ends this stream; that's no failure.
                                Err(e) if conn.close_reason().is_some() => {
                                    tracing::debug!("audio stream closed: {e}");
                                    break;
                                }
                                Err(e) => {
                                    tracing::warn!("audio stream failed: {e}");
                                    break;
                                }
                            }
                        }
                    })));
                }
            }
            other => {
                tracing::debug!(?other, "refusing an unexpected stream");
                let _ = stream.stop(CloseCode::ProtocolError.into());
            }
        }
    }
}

async fn end_reason(conn: &Connection) -> SessionEnd {
    let Ok(reason) = timeout(Duration::from_secs(1), conn.closed()).await else {
        conn.close(CloseCode::ProtocolError.into(), b"protocol");
        return SessionEnd::ProtocolError("stream ended while connected".into());
    };
    match reason {
        ConnectionError::ApplicationClosed(c) => {
            let code = c.error_code.into_inner();
            match u32::try_from(code).ok().and_then(CloseCode::from_repr) {
                Some(CloseCode::BroadcastStopped) => SessionEnd::BroadcastStopped,
                Some(CloseCode::SourceClosed) => SessionEnd::SourceClosed,
                Some(CloseCode::Busy) => SessionEnd::Busy,
                Some(CloseCode::VersionMismatch) => SessionEnd::VersionMismatch,
                Some(CloseCode::ViewerLeft) => {
                    SessionEnd::ConnectionLost("closed by broadcaster".into())
                }
                Some(CloseCode::ProtocolError) | None => {
                    SessionEnd::ProtocolError(format!("closed with code {code}"))
                }
            }
        }
        ConnectionError::LocallyClosed => SessionEnd::Closed,
        ConnectionError::TimedOut => SessionEnd::ConnectionLost("timed out".into()),
        other => SessionEnd::ConnectionLost(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ends_read_as_sentences() {
        assert_eq!(
            SessionEnd::BroadcastStopped.to_string(),
            "The broadcaster stopped sharing"
        );
        assert_eq!(
            SessionEnd::IdentityMismatch.to_string(),
            "Identity check failed: the peer's certificate does not match its announcement \
             (possible impersonation)"
        );
        assert_eq!(
            SessionEnd::Unreachable("192.168.0.9:5000: timed out".into()).to_string(),
            "Could not reach the broadcaster (192.168.0.9:5000: timed out)"
        );
        assert_eq!(
            SessionEnd::ProtocolError("closed with code 9".into()).to_string(),
            "Protocol error (closed with code 9)"
        );
    }
}

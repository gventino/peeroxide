use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use quinn::{Connection, Endpoint};
use tokio::sync::{broadcast, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::protocol::{
    AudioPacket, ClientMsg, CloseCode, PROTOCOL_VERSION, ServerMsg, StreamKind, VideoFrame,
    read_msg, write_audio, write_frame, write_msg, write_stream_kind,
};
use crate::{Identity, tls};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Frames buffered per viewer before it is considered lagging and skipped ahead to a keyframe.
const FRAME_BUFFER: usize = 30;
/// Audio packets (20 ms each) buffered per viewer; a lagging viewer just skips ahead.
const AUDIO_BUFFER: usize = 50;
/// Audio is sent before video when both are waiting, so a large keyframe never delays it.
const AUDIO_PRIORITY: i32 = 1;
const MAX_NAME_CHARS: usize = 64;

#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub name: String,
    pub max_viewers: usize,
    pub bind: SocketAddr,
    /// Whether this broadcast shares audio (announced to viewers in `Welcome`).
    pub audio: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Stopped,
    SourceClosed,
}

type Callback = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    name: String,
    max_viewers: usize,
    frames: broadcast::Sender<VideoFrame>,
    audio: Option<broadcast::Sender<AudioPacket>>,
    viewers: watch::Sender<usize>,
    on_keyframe: Callback,
}

/// Serves the broadcaster's stream to any number of viewers (up to `max_viewers`).
/// Must be created inside a Tokio runtime.
pub struct BroadcastServer {
    endpoint: Endpoint,
    shared: Arc<Shared>,
    accept_task: JoinHandle<()>,
}

impl BroadcastServer {
    /// `on_keyframe_request` fires whenever a viewer joins, lags behind, or asks for a keyframe.
    pub fn start(
        identity: &Identity,
        options: ServerOptions,
        on_keyframe_request: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let endpoint = Endpoint::server(tls::server_config(identity)?, options.bind)
            .with_context(|| format!("could not listen on {}", options.bind))?;
        let shared = Arc::new(Shared {
            name: options.name,
            max_viewers: options.max_viewers,
            frames: broadcast::channel(FRAME_BUFFER).0,
            audio: options.audio.then(|| broadcast::channel(AUDIO_BUFFER).0),
            viewers: watch::channel(0).0,
            on_keyframe: Arc::new(on_keyframe_request),
        });
        let accept_task = tokio::spawn(accept_loop(endpoint.clone(), shared.clone()));
        Ok(Self {
            endpoint,
            shared,
            accept_task,
        })
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Queues a frame for every connected viewer. Never blocks.
    pub fn publish(&self, frame: VideoFrame) {
        let _ = self.shared.frames.send(frame);
    }

    /// Queues an audio packet for every connected viewer. Never blocks. Ignored if the
    /// broadcast was started without audio.
    pub fn publish_audio(&self, packet: AudioPacket) {
        if let Some(audio) = &self.shared.audio {
            let _ = audio.send(packet);
        }
    }

    pub fn viewers(&self) -> watch::Receiver<usize> {
        self.shared.viewers.subscribe()
    }

    /// Disconnects every viewer with `reason` and waits briefly for the close to be delivered.
    pub async fn stop(&self, reason: StopReason) {
        self.accept_task.abort();
        let code = match reason {
            StopReason::Stopped => CloseCode::BroadcastStopped,
            StopReason::SourceClosed => CloseCode::SourceClosed,
        };
        self.endpoint.close(code.into(), b"broadcast ended");
        let _ = timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }
}

impl Drop for BroadcastServer {
    fn drop(&mut self) {
        self.accept_task.abort();
        self.endpoint
            .close(CloseCode::BroadcastStopped.into(), b"broadcast ended");
    }
}

async fn accept_loop(endpoint: Endpoint, shared: Arc<Shared>) {
    while let Some(incoming) = endpoint.accept().await {
        let shared = shared.clone();
        tokio::spawn(async move {
            let conn = match timeout(HANDSHAKE_TIMEOUT, incoming).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    tracing::debug!("incoming connection failed: {e}");
                    return;
                }
                Err(_) => {
                    tracing::debug!("incoming connection timed out");
                    return;
                }
            };
            let remote = conn.remote_address();
            match serve_viewer(&shared, &conn).await {
                Ok(()) => tracing::info!(%remote, "viewer disconnected"),
                Err(e) => tracing::info!(%remote, "viewer session ended: {e:#}"),
            }
        });
    }
}

/// Decrements the viewer count when a viewer's session ends, however it ends.
struct ViewerSlot(Arc<Shared>);

impl Drop for ViewerSlot {
    fn drop(&mut self) {
        self.0.viewers.send_modify(|n| *n = n.saturating_sub(1));
    }
}

async fn serve_viewer(shared: &Arc<Shared>, conn: &Connection) -> anyhow::Result<()> {
    let (mut ctrl_send, mut ctrl_recv) = timeout(HANDSHAKE_TIMEOUT, conn.accept_bi())
        .await
        .context("no control stream")?
        .context("control stream")?;
    let hello = timeout(HANDSHAKE_TIMEOUT, read_msg::<_, ClientMsg>(&mut ctrl_recv))
        .await
        .context("no hello")?;
    let (version, viewer_name) = match hello {
        Ok(Some(ClientMsg::Hello {
            version,
            viewer_name,
        })) => (version, sanitize(&viewer_name)),
        other => {
            conn.close(CloseCode::ProtocolError.into(), b"expected hello");
            bail!("bad hello: {other:?}");
        }
    };
    if version != PROTOCOL_VERSION {
        conn.close(CloseCode::VersionMismatch.into(), b"version");
        bail!("{viewer_name} speaks protocol v{version}");
    }
    let admitted = shared.viewers.send_if_modified(|n| {
        let ok = *n < shared.max_viewers;
        if ok {
            *n += 1;
        }
        ok
    });
    if !admitted {
        conn.close(CloseCode::Busy.into(), b"too many viewers");
        bail!("{viewer_name} rejected: viewer limit reached");
    }
    let _slot = ViewerSlot(shared.clone());
    tracing::info!(viewer = %viewer_name, remote = %conn.remote_address(), "viewer connected");

    write_msg(
        &mut ctrl_send,
        &ServerMsg::Welcome {
            broadcaster_name: shared.name.clone(),
            audio: shared.audio.is_some(),
        },
    )
    .await
    .context("sending welcome")?;

    // Control messages are read on their own task: stream reads are not cancellation-safe.
    let (left_tx, mut left) = oneshot::channel::<()>();
    let on_keyframe = shared.on_keyframe.clone();
    let control_conn = conn.clone();
    let control_task = tokio::spawn(async move {
        loop {
            match read_msg::<_, ClientMsg>(&mut ctrl_recv).await {
                Ok(Some(ClientMsg::RequestKeyframe)) => on_keyframe(),
                Ok(Some(ClientMsg::Hello { .. })) => {
                    control_conn.close(CloseCode::ProtocolError.into(), b"repeated hello");
                    break;
                }
                Ok(None) | Err(_) => break,
            }
        }
        let _ = left_tx.send(());
    });

    let mut frames = shared.frames.subscribe();
    (shared.on_keyframe)();
    // Audio problems never end the session: the forwarder just stops.
    let audio_task = shared
        .audio
        .as_ref()
        .map(|audio| tokio::spawn(forward_audio(conn.clone(), audio.subscribe())));
    let mut video = conn.open_uni().await.context("opening the video stream")?;
    write_stream_kind(&mut video, StreamKind::Video)
        .await
        .context("opening the video stream")?;
    let mut waiting_for_keyframe = true;
    let result = loop {
        tokio::select! {
            received = frames.recv() => match received {
                Ok(frame) => {
                    if waiting_for_keyframe {
                        if !frame.keyframe {
                            continue;
                        }
                        waiting_for_keyframe = false;
                    }
                    if let Err(e) = write_frame(&mut video, &frame).await {
                        break Err(anyhow::Error::from(e).context("sending video"));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "viewer lagging; skipping to next keyframe");
                    waiting_for_keyframe = true;
                    (shared.on_keyframe)();
                }
                Err(broadcast::error::RecvError::Closed) => break Ok(()),
            },
            _ = &mut left => break Ok(()),
        }
    };
    control_task.abort();
    if let Some(t) = audio_task {
        t.abort();
    }
    result
}

async fn forward_audio(conn: Connection, mut packets: broadcast::Receiver<AudioPacket>) {
    let opened = async {
        let mut stream = conn.open_uni().await?;
        let _ = stream.set_priority(AUDIO_PRIORITY);
        write_stream_kind(&mut stream, StreamKind::Audio).await?;
        anyhow::Ok(stream)
    };
    let mut stream = match opened.await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("could not open the audio stream: {e:#}");
            return;
        }
    };
    loop {
        match packets.recv().await {
            Ok(packet) => {
                if let Err(e) = write_audio(&mut stream, &packet).await {
                    tracing::debug!("audio stream ended: {e}");
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::debug!(skipped, "viewer lagging; skipping audio");
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn sanitize(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    if clean.trim().is_empty() {
        "anonymous".into()
    } else {
        clean
    }
}

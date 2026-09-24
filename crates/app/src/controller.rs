//! Owns the networking runtime and wires capture/encode → server and client → decode.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Context;
use eframe::egui;
use peeroxide_audio::{AudioSource, OutputControl};
use peeroxide_capture::Source;
use peeroxide_codec::Preset;
use peeroxide_discovery::{Discovery, Peer};
use peeroxide_net::{
    BroadcastServer, Fingerprint, Identity, NetError, ServerOptions, SessionEvent, SessionHandle,
    SessionId, StopReason, ViewerClient,
};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use crate::audio_decoder::{AudioReceiver, AudioReceiverStats};
use crate::audio_encoder::{AudioPipeline, audio_source};
use crate::decoder::{DecoderPipeline, DecoderStats, VideoSlot};
use crate::encoder::{EncoderControl, EncoderEnd, EncoderPipeline};

pub const MAX_VIEWERS: usize = 8;

/// Binds the broadcast server to `preferred_port`, falling back to a random port if it is taken.
/// Must be called inside the Tokio runtime.
fn start_server(
    identity: &Identity,
    name: &str,
    preferred_port: Option<u16>,
    audio: bool,
    on_keyframe_request: impl Fn() + Send + Sync + Clone + 'static,
) -> Result<BroadcastServer, NetError> {
    let options = |port| ServerOptions {
        name: name.to_owned(),
        max_viewers: MAX_VIEWERS,
        bind: SocketAddr::from(([0, 0, 0, 0], port)),
        audio,
    };
    if let Some(port) = preferred_port.filter(|p| *p != 0) {
        match BroadcastServer::start(identity, options(port), on_keyframe_request.clone()) {
            Ok(server) => return Ok(server),
            Err(e) => tracing::warn!("port {port} unavailable ({e:#}); using a random port"),
        }
    }
    BroadcastServer::start(identity, options(0), on_keyframe_request)
}

/// The audio to capture for `source`, if this machine can capture it.
fn checked_audio_source(source: &Source) -> anyhow::Result<AudioSource> {
    let audio = audio_source(source).context("could not tell which app owns this window")?;
    peeroxide_audio::check(&audio)?;
    Ok(audio)
}

/// Usable IPv4 addresses per network adapter (LAN, and virtual LANs such as Hamachi or Radmin).
pub fn local_ipv4s() -> Vec<(String, Ipv4Addr)> {
    let mut out: Vec<(String, Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.is_oper_up() && !i.is_loopback() && !i.is_link_local())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) => Some((i.name, ip)),
            IpAddr::V6(_) => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

pub enum Event {
    ViewerCount(usize),
    /// The encoder stopped on its own (source closed or capture failed) for broadcast `generation`.
    BroadcastEnded {
        generation: u64,
        end: EncoderEnd,
    },
    /// Audio capture failed during broadcast `generation`; video goes on.
    AudioEnded {
        generation: u64,
        error: String,
    },
    Session(SessionId, SessionEvent),
    Peers(Vec<Peer>),
}

#[derive(Clone, Debug)]
pub struct PeerTarget {
    pub fingerprint: Fingerprint,
    pub name: String,
    pub addrs: Vec<SocketAddr>,
}

impl PeerTarget {
    pub fn from_peer(peer: &Peer) -> Option<Self> {
        Some(Self {
            fingerprint: Fingerprint::from_hex(&peer.fingerprint)?,
            name: peer.name.clone(),
            addrs: peer.addrs.clone(),
        })
    }
}

pub struct Broadcast {
    pub generation: u64,
    pub port: u16,
    pub source_name: String,
    pub encoder: EncoderPipeline,
    /// Present while the broadcast shares audio.
    pub audio: Option<AudioPipeline>,
    /// Why audio was asked for but isn't shared.
    pub audio_note: Option<String>,
    server: Arc<BroadcastServer>,
    viewers_task: JoinHandle<()>,
}

pub struct Watching {
    _session: SessionHandle,
    pub decoder_stats: Arc<Mutex<DecoderStats>>,
    pub audio_stats: Arc<Mutex<AudioReceiverStats>>,
}

pub struct Controller {
    rt: Runtime,
    identity: Identity,
    display_name: String,
    client: ViewerClient,
    ctx: egui::Context,
    events_tx: Sender<Event>,
    pub events: Receiver<Event>,
    pub video: Arc<VideoSlot>,
    /// Volume and mute of what we watch; kept across sessions and broadcasters.
    pub output: Arc<OutputControl>,
    pub broadcast: Option<Broadcast>,
    pub watching: Option<Watching>,
    discovery: Option<Discovery>,
    pub discovery_error: Option<String>,
    generation: u64,
}

impl Controller {
    pub fn new(
        identity: Identity,
        display_name: String,
        ctx: egui::Context,
    ) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("net")
            .enable_all()
            .build()
            .context("could not start the network runtime")?;
        let client = {
            let _guard = rt.enter();
            ViewerClient::new()?
        };
        let (events_tx, events) = channel();
        let mut ctrl = Self {
            rt,
            identity,
            display_name,
            client,
            ctx,
            events_tx,
            events,
            video: Arc::new(VideoSlot::default()),
            output: OutputControl::new(1.0, false),
            broadcast: None,
            watching: None,
            discovery: None,
            discovery_error: None,
            generation: 0,
        };
        ctrl.start_discovery();
        Ok(ctrl)
    }

    fn start_discovery(&mut self) {
        let emit = self.emitter();
        let started = Discovery::new(&self.identity.fingerprint().to_hex()).and_then(|d| {
            d.browse(move |peers| emit(Event::Peers(peers)))?;
            Ok(d)
        });
        match started {
            Ok(d) => self.discovery = Some(d),
            Err(e) => {
                tracing::warn!("discovery unavailable: {e:#}");
                self.discovery_error = Some(format!("Discovery unavailable: {e:#}"));
            }
        }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.identity.fingerprint()
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Takes effect for the next broadcast and the next watch session.
    pub fn set_display_name(&mut self, name: String) {
        tracing::info!(%name, "display name changed");
        self.display_name = name;
    }

    fn emitter(&self) -> impl Fn(Event) + Send + Sync + Clone + 'static {
        let tx = self.events_tx.clone();
        let ctx = self.ctx.clone();
        move |e| {
            let _ = tx.send(e);
            ctx.request_repaint();
        }
    }

    /// Starts broadcasting on `preferred_port` when it is free (a random port otherwise) and
    /// returns the port actually used. With `share_audio`, the source's audio is shared too if
    /// it can be captured; otherwise the broadcast is video-only and `audio_note` says why.
    pub fn start_broadcast(
        &mut self,
        source: Source,
        preset: Preset,
        preferred_port: Option<u16>,
        share_audio: bool,
    ) -> anyhow::Result<u16> {
        self.stop_broadcast(StopReason::Stopped);
        let _guard = self.rt.enter();
        self.generation += 1;
        let generation = self.generation;

        let (audio_source, audio_note) = if share_audio {
            match checked_audio_source(&source) {
                Ok(a) => (Some(a), None),
                Err(e) => {
                    tracing::warn!("audio unavailable: {e:#}");
                    (None, Some(format!("Sharing video only: {e:#}")))
                }
            }
        } else {
            (None, None)
        };

        // Capture/encode stay paused until the first viewer arrives.
        let control = EncoderControl::new(false);
        let audio_control = EncoderControl::new(false);
        let server = Arc::new(start_server(
            &self.identity,
            &self.display_name,
            preferred_port,
            audio_source.is_some(),
            {
                let control = control.clone();
                move || control.request_keyframe()
            },
        )?);
        let port = server.local_addr()?.port();
        let emit = self.emitter();

        let viewers_task = self.rt.spawn({
            let mut viewers = server.viewers();
            let control = control.clone();
            let audio_control = audio_control.clone();
            let emit = emit.clone();
            async move {
                loop {
                    let n = *viewers.borrow_and_update();
                    control.set_active(n > 0);
                    audio_control.set_active(n > 0);
                    emit(Event::ViewerCount(n));
                    if viewers.changed().await.is_err() {
                        break;
                    }
                }
            }
        });

        let audio = audio_source
            .map(|audio_source| {
                let emit = emit.clone();
                AudioPipeline::start(
                    audio_control,
                    audio_source,
                    preset.audio_bitrate_bps,
                    {
                        let server = server.clone();
                        move |packet| server.publish_audio(packet)
                    },
                    move |error| emit(Event::AudioEnded { generation, error }),
                )
            })
            .transpose()?;
        let encoder = EncoderPipeline::start(
            control,
            source.clone(),
            preset,
            {
                let server = server.clone();
                move |frame| server.publish(frame)
            },
            move |end| emit(Event::BroadcastEnded { generation, end }),
        )?;

        tracing::info!(
            source = %source.name,
            audio = ?audio_source,
            "broadcast started; dev connect string: --connect 127.0.0.1:{port}#{}",
            self.identity.fingerprint().to_hex()
        );
        let reachable: Vec<String> = local_ipv4s()
            .into_iter()
            .map(|(adapter, ip)| format!("{adapter} {ip}:{port}"))
            .collect();
        tracing::info!("reachable at: {}", reachable.join(", "));
        if let Some(d) = &mut self.discovery
            && let Err(e) = d.announce(&self.display_name, port)
        {
            tracing::warn!("could not announce on mDNS: {e:#}");
        }
        self.broadcast = Some(Broadcast {
            generation,
            port,
            source_name: source.name,
            encoder,
            audio,
            audio_note,
            server,
            viewers_task,
        });
        Ok(port)
    }

    pub fn stop_broadcast(&mut self, reason: StopReason) {
        let Some(b) = self.broadcast.take() else {
            return;
        };
        if let Some(d) = &mut self.discovery {
            d.withdraw();
        }
        // Joins the encoder threads, which also releases their handles on the server.
        drop(b.encoder);
        drop(b.audio);
        b.viewers_task.abort();
        let server = b.server;
        self.rt.spawn(async move { server.stop(reason).await });
        tracing::info!(?reason, "broadcast stopped");
    }

    /// Starts watching `target`, replacing any current session: there is never more than one.
    pub fn watch(&mut self, target: PeerTarget) -> SessionId {
        self.stop_watching();
        let _guard = self.rt.enter();

        // The decoder asks for keyframes through the session, which is created after it.
        let requester: Arc<OnceLock<Box<dyn Fn() + Send + Sync>>> = Arc::new(OnceLock::new());
        let need_keyframe: Arc<dyn Fn() + Send + Sync> = {
            let requester = requester.clone();
            Arc::new(move || {
                if let Some(request) = requester.get() {
                    request();
                }
            })
        };
        let repaint: Arc<dyn Fn() + Send + Sync> = {
            let ctx = self.ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let mut decoder = DecoderPipeline::start(self.video.clone(), repaint, need_keyframe);
        let decoder_stats = decoder.stats.clone();
        // Both pipelines live inside the session's callbacks and end with it.
        let audio = AudioReceiver::start(self.output.clone(), decoder.video_offset.clone());
        let audio_stats = audio.stats.clone();

        let emit = self.emitter();
        tracing::info!(peer = %target.name, fingerprint = %target.fingerprint, addrs = ?target.addrs, "watching");
        let session = self.client.watch(
            target.addrs,
            target.fingerprint,
            self.display_name.clone(),
            move |frame| decoder.push(frame),
            move |packet| audio.push(packet),
            move |id, event| emit(Event::Session(id, event)),
        );
        let _ = requester.set(Box::new(session.keyframe_requester()));
        let id = session.id();
        self.watching = Some(Watching {
            _session: session,
            decoder_stats,
            audio_stats,
        });
        id
    }

    pub fn stop_watching(&mut self) {
        if self.watching.take().is_some() {
            tracing::info!("stopped watching");
        }
        self.video.take();
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.watching = None;
        if let Some(b) = self.broadcast.take() {
            drop(b.encoder);
            drop(b.audio);
            b.viewers_task.abort();
            self.rt.block_on(b.server.stop(StopReason::Stopped));
        }
        self.rt.block_on(self.client.close());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_reuses_the_preferred_port_or_falls_back() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let id = Identity::generate().unwrap();
        let free = std::net::UdpSocket::bind("0.0.0.0:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        let first = start_server(&id, "t", Some(free), false, || {}).unwrap();
        assert_eq!(first.local_addr().unwrap().port(), free);

        let taken = start_server(&id, "t", Some(free), false, || {}).unwrap();
        assert_ne!(taken.local_addr().unwrap().port(), free);

        let random = start_server(&id, "t", None, false, || {}).unwrap();
        assert_ne!(random.local_addr().unwrap().port(), 0);
    }
}

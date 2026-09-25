//! Viewer side: Opus packets → PCM, played in sync with the video at the viewer's volume.
//!
//! Runs on its own thread; any failure here (bad packet, no output device) only affects audio.

use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use peeroxide_audio::{AudioOutput, OutputControl, Playout, PlayoutStats};
use peeroxide_codec::opus::{CHANNELS, FRAME_SAMPLES};
use peeroxide_codec::{AudioDecoder, OpusDecoder};
use peeroxide_net::AudioPacket;

use crate::decoder::VideoOffset;
use crate::encoder::wall_clock_us;
use crate::stats::Meter;

/// About a second of packets; more means the thread is stuck, and new ones are dropped.
const QUEUE: usize = 50;
/// Wait before trying to open the output device again after it failed or was missing.
const REOPEN_AFTER: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct AudioReceiverStats {
    pub meter: Meter,
    pub playout: PlayoutStats,
    pub underruns: u64,
    pub bad_packets: u64,
    /// Set while no output device can be opened.
    pub output_error: Option<String>,
}

/// Dropping it ends the thread (after the packets already queued).
pub struct AudioReceiver {
    tx: Option<SyncSender<(AudioPacket, u64)>>,
    pub stats: Arc<Mutex<AudioReceiverStats>>,
    thread: Option<JoinHandle<()>>,
}

impl AudioReceiver {
    pub fn start(control: Arc<OutputControl>, video_offset: Arc<VideoOffset>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<(AudioPacket, u64)>(QUEUE);
        let stats = Arc::new(Mutex::new(AudioReceiverStats::default()));
        let thread = std::thread::Builder::new()
            .name("audio-decoder".into())
            .spawn({
                let stats = stats.clone();
                move || {
                    let mut decoder = match OpusDecoder::new() {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::error!("audio decoder init failed: {e:#}");
                            return;
                        }
                    };
                    let mut player = Player {
                        control,
                        output: None,
                        retry_at: None,
                        playout: Playout::default(),
                    };
                    // Ends when the AudioReceiver drops its sender, after the queued packets.
                    for (packet, arrival_us) in rx {
                        let started = Instant::now();
                        let pcm = match decoder.decode(&packet.data) {
                            Ok(pcm) => pcm,
                            Err(e) => {
                                tracing::debug!(seq = packet.seq, "audio decode: {e:#}");
                                stats.lock().unwrap().bad_packets += 1;
                                continue;
                            }
                        };
                        stats
                            .lock()
                            .unwrap()
                            .meter
                            .record(packet.data.len(), started.elapsed());
                        player.play(
                            &pcm,
                            packet.capture_time_us,
                            arrival_us,
                            video_offset.get(),
                            &stats,
                        );
                    }
                }
            })
            .expect("spawn audio decoder thread");
        Self {
            tx: Some(tx),
            stats,
            thread: Some(thread),
        }
    }

    /// Queues a packet as it arrives from the network. Never blocks.
    pub fn push(&self, packet: AudioPacket) {
        let arrival_us = wall_clock_us(Instant::now());
        if let Some(tx) = &self.tx
            && let Err(TrySendError::Full(_)) = tx.try_send((packet, arrival_us))
        {
            tracing::debug!("audio decoder behind; dropping a packet");
        }
    }
}

impl Drop for AudioReceiver {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Player {
    control: Arc<OutputControl>,
    output: Option<AudioOutput>,
    retry_at: Option<Instant>,
    playout: Playout,
}

impl Player {
    fn play(
        &mut self,
        pcm: &[f32],
        capture_us: u64,
        arrival_us: u64,
        video_offset: Option<i64>,
        stats: &Mutex<AudioReceiverStats>,
    ) {
        self.playout.arrived(capture_us, arrival_us);
        if self.output.as_ref().is_some_and(AudioOutput::failed) {
            tracing::info!("audio output failed; reopening");
            self.output = None;
        }
        if self.output.is_none() && self.retry_at.is_none_or(|t| Instant::now() >= t) {
            // Opened on the first packet, so nothing is held while a broadcast has no audio.
            match AudioOutput::open(self.control.clone()) {
                Ok(o) => {
                    self.output = Some(o);
                    self.retry_at = None;
                    stats.lock().unwrap().output_error = None;
                }
                Err(e) => {
                    tracing::warn!("audio output unavailable: {e:#}");
                    self.retry_at = Some(Instant::now() + REOPEN_AFTER);
                    stats.lock().unwrap().output_error = Some(format!("{e:#}"));
                }
            }
        }
        let Some(out) = &mut self.output else {
            return;
        };
        self.playout.set_video_offset(video_offset);
        let placed = self.playout.place(
            capture_us,
            FRAME_SAMPLES,
            wall_clock_us(Instant::now()),
            out.queued().as_micros() as u64,
        );
        if placed.pad > 0 {
            out.push_silence(placed.pad);
        }
        if placed.skip < FRAME_SAMPLES {
            out.push(&pcm[placed.skip * CHANNELS..]);
        }
        let mut s = stats.lock().unwrap();
        s.playout = self.playout.stats();
        s.underruns = out.underruns();
    }
}

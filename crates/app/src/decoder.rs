//! Viewer side: H.264 packets → RGBA frames in a [`VideoSlot`] for the GUI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use p2pss_codec::{DecodedFrame, H264Decoder, VideoDecoder};

use crate::stats::Meter;
use p2pss_net::VideoFrame;

const QUEUE: usize = 8;

/// Newest decoded frame, taken by the GUI on its next repaint.
#[derive(Default)]
pub struct VideoSlot(Mutex<Option<DecodedFrame>>);

impl VideoSlot {
    pub fn put(&self, frame: DecodedFrame) {
        *self.0.lock().unwrap() = Some(frame);
    }

    pub fn take(&self) -> Option<DecodedFrame> {
        self.0.lock().unwrap().take()
    }
}

#[derive(Default)]
pub struct DecoderStats {
    pub meter: Meter,
    pub dropped: u64,
    pub keyframe_requests: u64,
    /// Capture-to-decoded latency, only meaningful when both ends share a clock (same machine).
    pub latency_ms: Option<f32>,
}

type Callback = Arc<dyn Fn() + Send + Sync>;

pub struct DecoderPipeline {
    tx: Option<SyncSender<VideoFrame>>,
    waiting_for_keyframe: bool,
    resync: Arc<AtomicBool>,
    need_keyframe: Callback,
    pub stats: Arc<Mutex<DecoderStats>>,
    thread: Option<JoinHandle<()>>,
}

impl DecoderPipeline {
    pub fn start(output: Arc<VideoSlot>, on_frame: Callback, need_keyframe: Callback) -> Self {
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(QUEUE);
        let stats = Arc::new(Mutex::new(DecoderStats::default()));
        let resync = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("decoder".into())
            .spawn({
                let stats = stats.clone();
                let resync = resync.clone();
                let need_keyframe = need_keyframe.clone();
                move || {
                    let mut decoder = match H264Decoder::new() {
                        Ok(d) => d,
                        Err(e) => {
                            tracing::error!("decoder init failed: {e}");
                            return;
                        }
                    };
                    for packet in rx {
                        let started = Instant::now();
                        match decoder.decode(&packet.data) {
                            Ok(Some(frame)) => {
                                let mut s = stats.lock().unwrap();
                                s.meter.record(packet.data.len(), started.elapsed());
                                s.latency_ms = latency_ms(packet.capture_time_us);
                                drop(s);
                                output.put(frame);
                                on_frame();
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!(seq = packet.seq, "decode error: {e}");
                                resync.store(true, Ordering::Relaxed);
                                stats.lock().unwrap().keyframe_requests += 1;
                                need_keyframe();
                            }
                        }
                    }
                }
            })
            .expect("spawn decoder thread");
        Self {
            tx: Some(tx),
            waiting_for_keyframe: true,
            resync,
            need_keyframe,
            stats,
            thread: Some(thread),
        }
    }

    /// Queues a packet. Never blocks: on overflow or after a decode error, drops until the next keyframe.
    pub fn push(&mut self, packet: VideoFrame) {
        if self.resync.swap(false, Ordering::Relaxed) {
            self.waiting_for_keyframe = true;
        }
        if self.waiting_for_keyframe {
            if !packet.keyframe {
                self.stats.lock().unwrap().dropped += 1;
                return;
            }
            self.waiting_for_keyframe = false;
        }
        let Some(tx) = &self.tx else { return };
        if let Err(TrySendError::Full(_)) = tx.try_send(packet) {
            self.waiting_for_keyframe = true;
            let mut s = self.stats.lock().unwrap();
            s.dropped += 1;
            s.keyframe_requests += 1;
            drop(s);
            (self.need_keyframe)();
        }
    }
}

impl Drop for DecoderPipeline {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn latency_ms(capture_time_us: u64) -> Option<f32> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let d = now.checked_sub(capture_time_us)?;
    (d < Duration::from_secs(5).as_micros() as u64).then_some(d as f32 / 1000.0)
}

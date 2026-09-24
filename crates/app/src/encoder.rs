//! Broadcaster side: capture → fixed canvas → H.264, paced to the preset frame rate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use peeroxide_capture::{CaptureError, CaptureOptions, CaptureStream, CloseReason, Next, Source};
use peeroxide_codec::{Canvas, H264Encoder, Preset, VideoEncoder, canvas_size};
use peeroxide_net::VideoFrame;

use crate::stats::Meter;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncoderEnd {
    Stopped,
    SourceClosed,
    Failed(String),
}

/// Shared knobs between the pipeline thread and its users.
#[derive(Default)]
pub struct EncoderControl {
    active: AtomicBool,
    keyframe: AtomicBool,
    stop: AtomicBool,
    wake: (Mutex<()>, Condvar),
}

impl EncoderControl {
    pub fn new(active: bool) -> Arc<Self> {
        let c = Self::default();
        c.active.store(active, Ordering::Relaxed);
        Arc::new(c)
    }

    /// Capture and encoding only run while active (i.e. someone is watching).
    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
        self.wake.1.notify_all();
    }

    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Asks the pipeline thread to end and wakes it.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.wake.1.notify_all();
    }

    pub(crate) fn sleep(&self, d: Duration) {
        let guard = self.wake.0.lock().unwrap();
        let _ = self.wake.1.wait_timeout(guard, d).unwrap();
    }
}

#[derive(Default)]
pub struct EncoderStats {
    pub meter: Meter,
    pub canvas: Option<(u32, u32)>,
}

pub struct EncoderPipeline {
    control: Arc<EncoderControl>,
    pub stats: Arc<Mutex<EncoderStats>>,
    thread: Option<JoinHandle<()>>,
}

impl EncoderPipeline {
    pub fn start(
        control: Arc<EncoderControl>,
        source: Source,
        preset: Preset,
        on_packet: impl FnMut(VideoFrame) + Send + 'static,
        on_end: impl FnOnce(EncoderEnd) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let stats = Arc::new(Mutex::new(EncoderStats::default()));
        let thread = std::thread::Builder::new()
            .name("encoder".into())
            .spawn({
                let control = control.clone();
                let stats = stats.clone();
                move || {
                    let end = run(&control, &source, &preset, on_packet, &stats)
                        .unwrap_or_else(|e| EncoderEnd::Failed(format!("{e:#}")));
                    tracing::info!(?end, source = %source.name, "encoder pipeline ended");
                    on_end(end);
                }
            })
            .context("could not start the encoder thread")?;
        Ok(Self {
            control,
            stats,
            thread: Some(thread),
        })
    }
}

impl Drop for EncoderPipeline {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Relaxed);
        self.control.wake.1.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Session {
    capture: CaptureStream,
    canvas: Option<Canvas>,
    encoder: H264Encoder,
}

/// Returns how the pipeline ended on its own; an error means it failed.
fn run(
    control: &EncoderControl,
    source: &Source,
    preset: &Preset,
    mut on_packet: impl FnMut(VideoFrame),
    stats: &Mutex<EncoderStats>,
) -> anyhow::Result<EncoderEnd> {
    // Paced from when a frame is taken (not when encoding ends) so encode time doesn't accumulate
    // as drift; the slack absorbs jitter in the source's own cadence.
    let pace = Duration::from_secs_f64(1.0 / f64::from(preset.fps.max(1)))
        .saturating_sub(Duration::from_millis(1));
    let mut session: Option<Session> = None;
    let mut seq = 0u64;
    let mut last_take = Instant::now() - pace;

    loop {
        if control.stop.load(Ordering::Relaxed) {
            return Ok(EncoderEnd::Stopped);
        }
        if !control.active.load(Ordering::Relaxed) {
            if session.take().is_some() {
                tracing::debug!("no viewers: capture paused");
                stats.lock().unwrap().canvas = None;
            }
            control.sleep(Duration::from_millis(250));
            continue;
        }

        let s = match &mut session {
            Some(s) => s,
            None => {
                let capture = match peeroxide_capture::start(
                    source,
                    CaptureOptions {
                        fps: preset.fps,
                        show_cursor: true,
                    },
                ) {
                    Ok(c) => c,
                    Err(CaptureError::SourceNotFound) => return Ok(EncoderEnd::SourceClosed),
                    Err(e) => return Err(e.into()),
                };
                let encoder = H264Encoder::new(preset)?;
                tracing::debug!("capture started");
                session.insert(Session {
                    capture,
                    canvas: None,
                    encoder,
                })
            }
        };

        if let Some(wait) = (last_take + pace).checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }

        let (captured_at, fresh) = match s.capture.next(Duration::from_millis(100)) {
            Next::Frame(frame) => {
                last_take = Instant::now();
                let canvas = s.canvas.get_or_insert_with(|| {
                    let (w, h) = canvas_size(frame.width, frame.height, preset);
                    stats.lock().unwrap().canvas = Some((w, h));
                    Canvas::new(w, h)
                });
                canvas.draw(&frame.data, frame.width, frame.height)?;
                (frame.captured_at, true)
            }
            Next::Timeout => (Instant::now(), false),
            Next::Closed(CloseReason::SourceClosed) => return Ok(EncoderEnd::SourceClosed),
            Next::Closed(CloseReason::Failed(e)) => bail!(e),
        };

        let want_keyframe = control.keyframe.swap(false, Ordering::Relaxed);
        // A static screen produces no new frames; re-encode the last picture so a joining viewer gets an IDR.
        let Some(canvas) = s.canvas.as_ref().filter(|_| fresh || want_keyframe) else {
            if want_keyframe {
                control.request_keyframe();
            }
            continue;
        };
        if want_keyframe {
            s.encoder.request_keyframe();
        }

        let started = Instant::now();
        let encoded = s
            .encoder
            .encode(canvas.bgra(), canvas.width(), canvas.height())?;
        let Some(encoded) = encoded else { continue };
        stats
            .lock()
            .unwrap()
            .meter
            .record(encoded.data.len(), started.elapsed());

        on_packet(VideoFrame {
            seq,
            keyframe: encoded.keyframe,
            capture_time_us: wall_clock_us(captured_at),
            data: encoded.data.into(),
        });
        seq += 1;
    }
}

pub fn wall_clock_us(at: Instant) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.saturating_sub(at.elapsed()).as_micros() as u64
}

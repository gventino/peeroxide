//! Broadcaster side: audio capture → 20 ms frames → Opus, only while someone is watching.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use peeroxide_audio::{AudioCapture, AudioChunk, AudioSource, Next, start_capture};
use peeroxide_capture::{Source, SourceKind};
use peeroxide_codec::opus::{CHANNELS, FRAME_LEN, SAMPLE_RATE};
use peeroxide_codec::{AudioEncoder, OpusEncoder};
use peeroxide_net::AudioPacket;

use crate::encoder::{EncoderControl, wall_clock_us};
use crate::stats::Meter;

/// A pause this long between chunks is a gap in the audio (nothing played), not jitter.
const GAP: Duration = Duration::from_millis(30);

/// What sharing `source`'s audio captures, or `None` if it can't be told (a window whose
/// owning process is unknown).
pub fn audio_source(source: &Source) -> Option<AudioSource> {
    match source.kind {
        SourceKind::Monitor => Some(AudioSource::System {
            exclude_pid: std::process::id(),
        }),
        SourceKind::Window => source
            .process_id()
            .map(|pid| AudioSource::Application { pid }),
        SourceKind::TestPattern => Some(AudioSource::TestTone),
    }
}

/// How the UI describes what `audio_source` captures.
pub fn describe(source: &AudioSource) -> &'static str {
    match source {
        AudioSource::System { .. } => "all sound on this computer, except Peeroxide",
        AudioSource::Application { .. } => "only sound from this window's app",
        AudioSource::TestTone => "a test tone, beeping with the flashing square",
    }
}

#[derive(Default)]
pub struct AudioStats {
    pub meter: Meter,
}

pub struct AudioPipeline {
    control: Arc<EncoderControl>,
    pub source: AudioSource,
    pub stats: Arc<Mutex<AudioStats>>,
    thread: Option<JoinHandle<()>>,
}

impl AudioPipeline {
    /// `on_end` reports why audio stopped on its own; video is not affected.
    pub fn start(
        control: Arc<EncoderControl>,
        source: AudioSource,
        bitrate_bps: u32,
        on_packet: impl FnMut(AudioPacket) + Send + 'static,
        on_end: impl FnOnce(String) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let stats = Arc::new(Mutex::new(AudioStats::default()));
        let thread = std::thread::Builder::new()
            .name("audio-encoder".into())
            .spawn({
                let control = control.clone();
                let stats = stats.clone();
                move || match run(&control, &source, bitrate_bps, on_packet, &stats) {
                    Ok(()) => tracing::info!(?source, "audio pipeline ended"),
                    Err(e) => {
                        tracing::warn!(?source, "audio pipeline failed: {e:#}");
                        on_end(format!("{e:#}"));
                    }
                }
            })
            .context("could not start the audio encoder thread")?;
        Ok(Self {
            control,
            source,
            stats,
            thread: Some(thread),
        })
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.control.stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Session {
    capture: AudioCapture,
    encoder: OpusEncoder,
    framer: Framer,
}

fn run(
    control: &EncoderControl,
    source: &AudioSource,
    bitrate_bps: u32,
    mut on_packet: impl FnMut(AudioPacket),
    stats: &Mutex<AudioStats>,
) -> anyhow::Result<()> {
    let mut session: Option<Session> = None;
    let mut seq = 0u64;
    loop {
        if control.is_stopped() {
            return Ok(());
        }
        if !control.is_active() {
            if session.take().is_some() {
                tracing::debug!("no viewers: audio capture paused");
            }
            control.sleep(Duration::from_millis(250));
            continue;
        }
        let s = match &mut session {
            Some(s) => s,
            None => {
                let capture = start_capture(source)?;
                let encoder = OpusEncoder::new(bitrate_bps)?;
                tracing::debug!("audio capture started");
                session.insert(Session {
                    capture,
                    encoder,
                    framer: Framer::default(),
                })
            }
        };
        match s.capture.next(Duration::from_millis(100)) {
            Next::Chunk(chunk) => s.framer.push(chunk),
            // Nothing is playing: send what's left of the last sound instead of holding it.
            Next::Timeout => s.framer.flush(),
            Next::Failed(e) => return Err(e),
        }
        while let Some((pcm, captured_at)) = s.framer.pop() {
            let started = Instant::now();
            let data = s.encoder.encode(&pcm)?;
            stats
                .lock()
                .unwrap()
                .meter
                .record(data.len(), started.elapsed());
            on_packet(AudioPacket {
                seq,
                capture_time_us: wall_clock_us(captured_at),
                data: data.into(),
            });
            seq += 1;
        }
    }
}

fn duration_of(samples: usize) -> Duration {
    Duration::from_secs_f64((samples / CHANNELS) as f64 / f64::from(SAMPLE_RATE))
}

/// Cuts captured chunks of any size into 20 ms frames stamped with their first sample's
/// capture time. Stamps follow each new chunk's own time, so they don't drift from the clock.
#[derive(Default)]
struct Framer {
    buf: Vec<f32>,
    /// Capture time of `buf[0]`.
    start: Option<Instant>,
    ready: VecDeque<(Vec<f32>, Instant)>,
}

impl Framer {
    fn push(&mut self, chunk: AudioChunk) {
        if let Some(start) = self.start
            && chunk.captured_at > start + duration_of(self.buf.len()) + GAP
        {
            self.flush();
        }
        self.start = Some(
            chunk
                .captured_at
                .checked_sub(duration_of(self.buf.len()))
                .unwrap_or(chunk.captured_at),
        );
        self.buf.extend_from_slice(&chunk.samples);
        while self.buf.len() >= FRAME_LEN {
            let start = self.start.unwrap_or_else(Instant::now);
            let frame: Vec<f32> = self.buf.drain(..FRAME_LEN).collect();
            self.ready.push_back((frame, start));
            self.start = Some(start + duration_of(FRAME_LEN));
        }
        if self.buf.is_empty() {
            self.start = None;
        }
    }

    /// Pads what's buffered with silence into a last frame.
    fn flush(&mut self) {
        if let Some(start) = self.start.take()
            && !self.buf.is_empty()
        {
            let mut frame = std::mem::take(&mut self.buf);
            frame.resize(FRAME_LEN, 0.0);
            self.ready.push_back((frame, start));
        }
        self.buf.clear();
    }

    fn pop(&mut self) -> Option<(Vec<f32>, Instant)> {
        self.ready.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(at: Instant, frames: usize, value: f32) -> AudioChunk {
        AudioChunk {
            samples: vec![value; frames * 2],
            captured_at: at,
        }
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn continuous_chunks_become_stamped_20_ms_frames() {
        let t0 = Instant::now();
        let mut f = Framer::default();
        for i in 0..5 {
            f.push(chunk(t0 + 10 * i * MS, 480, i as f32));
        }
        let (a, at_a) = f.pop().unwrap();
        let (b, at_b) = f.pop().unwrap();
        assert!(f.pop().is_none(), "the last 10 ms wait for more");
        assert_eq!(a.len(), FRAME_LEN);
        assert_eq!((a[0], a[FRAME_LEN - 1]), (0.0, 1.0));
        assert_eq!((b[0], b[FRAME_LEN - 1]), (2.0, 3.0));
        assert_eq!(at_a, t0);
        assert_eq!(at_b, t0 + 20 * MS);
    }

    #[test]
    fn stamps_follow_the_capture_clock_instead_of_drifting() {
        let t0 = Instant::now();
        let mut f = Framer::default();
        f.push(chunk(t0, 480, 0.0));
        // The next chunk turns out to have been captured 3 ms later than a perfect clock says.
        f.push(chunk(t0 + 13 * MS, 480, 0.0));
        let (_, at) = f.pop().unwrap();
        assert_eq!(at, t0 + 3 * MS);
    }

    #[test]
    fn a_gap_flushes_the_partial_frame_padded_with_silence() {
        let t0 = Instant::now();
        let mut f = Framer::default();
        f.push(chunk(t0, 480, 0.5));
        f.push(chunk(t0 + 500 * MS, 960, 0.7));
        let (tail, at) = f.pop().unwrap();
        assert_eq!(at, t0);
        assert_eq!((tail[0], tail[FRAME_LEN - 1]), (0.5, 0.0));
        let (next, at) = f.pop().unwrap();
        assert_eq!(at, t0 + 500 * MS);
        assert!(next.iter().all(|s| *s == 0.7));
    }

    #[test]
    fn flushing_when_idle_releases_the_tail_once() {
        let t0 = Instant::now();
        let mut f = Framer::default();
        f.push(chunk(t0, 100, 0.1));
        f.flush();
        assert!(f.pop().is_some());
        f.flush();
        assert!(f.pop().is_none());
    }
}

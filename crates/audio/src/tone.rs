//! Test tone: a 440 Hz beep during the first 100 ms of every wall-clock second, in step with the
//! test pattern's flashing square, so audio/video sync can be checked by eye and ear.

use std::f32::consts::TAU;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{AudioChunk, AudioError, CHANNELS, SAMPLE_RATE, Sink};

const CHUNK_FRAMES: usize = 480;
const FREQ: f32 = 440.0;
const AMPLITUDE: f32 = 0.3;
const BEEP: Duration = Duration::from_millis(100);

fn unix_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// Renders `frames` stereo frames starting at wall-clock time `start_us`.
pub(crate) fn render(start_us: u64, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * CHANNELS);
    for i in 0..frames {
        let t_us = start_us + i as u64 * 1_000_000 / u64::from(SAMPLE_RATE);
        let into_second = t_us % 1_000_000;
        let v = if into_second < BEEP.as_micros() as u64 {
            let t = into_second as f32 / 1e6;
            AMPLITUDE * (TAU * FREQ * t).sin()
        } else {
            0.0
        };
        out.extend([v; CHANNELS]);
    }
    out
}

pub(crate) fn start(sink: Sink) -> Result<JoinHandle<()>, AudioError> {
    std::thread::Builder::new()
        .name("audio-tone".into())
        .spawn(move || {
            let chunk = Duration::from_secs_f64(CHUNK_FRAMES as f64 / f64::from(SAMPLE_RATE));
            // Like a real capture, a chunk is delivered once its last sample has been "played".
            let mut start = Instant::now();
            let mut start_us = unix_us();
            while !sink.stopped() {
                let end = start + chunk;
                if let Some(wait) = end.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                } else if Instant::now() - end > Duration::from_millis(200) {
                    // Fell far behind (suspended?): restart from now instead of catching up.
                    start = Instant::now();
                    start_us = unix_us();
                    continue;
                }
                sink.chunk(AudioChunk {
                    samples: render(start_us, CHUNK_FRAMES),
                    captured_at: start,
                });
                start = end;
                start_us += chunk.as_micros() as u64;
            }
        })
        .map_err(|e| AudioError::Backend(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beeps_only_at_the_start_of_each_second() {
        let second = 1_700_000_000_000_000;
        let beep = render(second + 10_000, 480);
        assert!(beep.iter().any(|s| s.abs() > 0.2));
        assert!(
            beep.chunks_exact(2).all(|f| f[0] == f[1]),
            "same on both channels"
        );
        let quiet = render(second + 150_000, 480);
        assert!(quiet.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn delivers_paced_chunks_until_stopped() {
        let capture = crate::start_capture(&crate::AudioSource::TestTone).unwrap();
        let started = Instant::now();
        let mut frames = 0;
        while frames < SAMPLE_RATE as usize / 5 {
            match capture.next(Duration::from_secs(1)) {
                crate::Next::Chunk(c) => frames += c.samples.len() / CHANNELS,
                _ => panic!("tone stopped"),
            }
        }
        // 200 ms of audio can't arrive much faster than real time.
        assert!(started.elapsed() >= Duration::from_millis(150));
        drop(capture);
    }
}

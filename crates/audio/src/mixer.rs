//! Puts several captures on one timeline, by capture time, and adds them up.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::{AudioChunk, CHANNELS, SAMPLE_RATE};

/// Released in blocks of 10 ms, like the audio engine delivers.
pub(crate) const BLOCK: usize = 480;
/// How long a block waits for every capture's samples before it's released.
pub(crate) const MARGIN: Duration = Duration::from_millis(40);
/// A chunk this close to where its capture left off continues it seamlessly: the capture
/// times of consecutive chunks jitter by a few milliseconds.
const SNAP: Duration = Duration::from_millis(20);

const NANOS: u128 = 1_000_000_000;

/// Whole frames in `d`, rounded; integer math so times and frames convert back exactly.
fn frames(d: Duration) -> i64 {
    ((d.as_nanos() * u128::from(SAMPLE_RATE) + NANOS / 2) / NANOS) as i64
}

pub(crate) struct Mixer {
    /// The capture time of frame 0.
    origin: Instant,
    /// The first frame not released yet.
    released: i64,
    /// Summed samples from `released` on, interleaved stereo.
    samples: VecDeque<f32>,
    /// Per block from `released` on: whether any capture wrote to it.
    written: VecDeque<bool>,
    /// Per capture: the frame where its next chunk continues.
    next: HashMap<u32, i64>,
}

impl Mixer {
    pub(crate) fn new(start: Instant) -> Self {
        // Frame 0 a second earlier, so chunks captured just before `start` still have a place.
        let origin = start.checked_sub(Duration::from_secs(1)).unwrap_or(start);
        Self {
            origin,
            released: frames(start - origin),
            samples: VecDeque::new(),
            written: VecDeque::new(),
            next: HashMap::new(),
        }
    }

    fn frame_of(&self, at: Instant) -> i64 {
        match at.checked_duration_since(self.origin) {
            Some(d) => frames(d),
            None => -frames(self.origin - at),
        }
    }

    fn time_of(&self, frame: i64) -> Instant {
        let nanos = frame.max(0) as u128 * NANOS / u128::from(SAMPLE_RATE);
        self.origin + Duration::from_nanos(nanos as u64)
    }

    /// Adds a chunk from capture `stream`. Whatever falls before the released point is dropped.
    pub(crate) fn push(&mut self, stream: u32, chunk: &AudioChunk) {
        let count = chunk.samples.len() / CHANNELS;
        let mut at = self.frame_of(chunk.captured_at);
        if let Some(&next) = self.next.get(&stream)
            && (at - next).abs() <= frames(SNAP)
        {
            at = next;
        }
        self.next.insert(stream, at + count as i64);

        let skip = (self.released - at).clamp(0, count as i64) as usize;
        if skip == count {
            return;
        }
        let start = (at + skip as i64 - self.released) as usize;
        let end = start + count - skip;
        if self.samples.len() < end * CHANNELS {
            self.samples.resize(end * CHANNELS, 0.0);
        }
        if self.written.len() < end.div_ceil(BLOCK) {
            self.written.resize(end.div_ceil(BLOCK), false);
        }
        for (i, s) in chunk.samples[skip * CHANNELS..].iter().enumerate() {
            self.samples[start * CHANNELS + i] += s;
        }
        for block in start / BLOCK..end.div_ceil(BLOCK) {
            self.written[block] = true;
        }
    }

    /// Forgets a capture that ended, so a new one with the same id starts fresh.
    pub(crate) fn remove(&mut self, stream: u32) {
        self.next.remove(&stream);
    }

    /// The next block once it is [`MARGIN`] old. Blocks no capture wrote to are skipped,
    /// which leaves a gap, like a silent source in a single capture.
    pub(crate) fn pop(&mut self, now: Instant) -> Option<AudioChunk> {
        loop {
            let end = self.released + BLOCK as i64;
            if self.time_of(end) + MARGIN > now {
                return None;
            }
            let Some(written) = self.written.pop_front() else {
                // Nothing buffered: move straight to the newest block that could be released.
                let ready = self.frame_of(now.checked_sub(MARGIN)?) - BLOCK as i64;
                if ready > self.released {
                    let whole = (ready - self.released) / BLOCK as i64 * BLOCK as i64;
                    self.released += whole;
                }
                return None;
            };
            let take = (BLOCK * CHANNELS).min(self.samples.len());
            let mut samples: Vec<f32> = self.samples.drain(..take).collect();
            samples.resize(BLOCK * CHANNELS, 0.0);
            let captured_at = self.time_of(self.released);
            self.released = end;
            if written {
                for s in &mut samples {
                    *s = s.clamp(-1.0, 1.0);
                }
                return Some(AudioChunk {
                    samples,
                    captured_at,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn chunk(at: Instant, count: usize, value: f32) -> AudioChunk {
        AudioChunk {
            samples: vec![value; count * CHANNELS],
            captured_at: at,
        }
    }

    fn all(mixer: &mut Mixer, now: Instant) -> Vec<AudioChunk> {
        std::iter::from_fn(|| mixer.pop(now)).collect()
    }

    #[test]
    fn captures_at_the_same_time_are_added_up() {
        let t0 = Instant::now();
        let mut m = Mixer::new(t0);
        m.push(1, &chunk(t0, 480, 0.25));
        m.push(2, &chunk(t0, 480, 0.5));
        let out = all(&mut m, t0 + 50 * MS);
        assert_eq!(out.len(), 1);
        assert!(out[0].samples.iter().all(|&s| s == 0.75));
        assert_eq!(out[0].captured_at, t0);
    }

    #[test]
    fn a_block_waits_for_the_margin() {
        let t0 = Instant::now();
        let mut m = Mixer::new(t0);
        m.push(1, &chunk(t0, 480, 0.1));
        assert!(m.pop(t0 + 49 * MS).is_none());
        assert!(m.pop(t0 + 50 * MS).is_some());
    }

    #[test]
    fn jitter_in_capture_times_is_absorbed_without_gaps_or_overlaps() {
        let t0 = Instant::now();
        let mut m = Mixer::new(t0);
        m.push(1, &chunk(t0, 480, 0.2));
        m.push(1, &chunk(t0 + 13 * MS, 480, 0.2));
        m.push(1, &chunk(t0 + 17 * MS, 480, 0.2));
        let out = all(&mut m, t0 + 100 * MS);
        assert_eq!(out.len(), 3);
        for (i, c) in out.iter().enumerate() {
            assert!(c.samples.iter().all(|&s| s == 0.2), "block {i}");
            assert_eq!(c.captured_at, t0 + 10 * i as u32 * MS);
        }
    }

    #[test]
    fn silence_between_sounds_is_a_gap_and_later_sound_keeps_its_time() {
        let t0 = Instant::now();
        let mut m = Mixer::new(t0);
        m.push(1, &chunk(t0, 480, 0.3));
        assert_eq!(all(&mut m, t0 + 300 * MS).len(), 1);
        m.push(1, &chunk(t0 + 500 * MS, 480, 0.4));
        let out = all(&mut m, t0 + 600 * MS);
        assert_eq!(out.len(), 1, "nothing is sent for the silent blocks");
        assert_eq!(out[0].captured_at, t0 + 500 * MS);
        assert!(out[0].samples.iter().all(|&s| s == 0.4));
    }

    #[test]
    fn a_late_chunk_only_adds_what_is_not_released_yet() {
        let t0 = Instant::now();
        let mut m = Mixer::new(t0);
        m.push(1, &chunk(t0, 960, 0.1));
        assert_eq!(all(&mut m, t0 + 50 * MS).len(), 1);
        m.push(2, &chunk(t0, 960, 0.2));
        let out = all(&mut m, t0 + 100 * MS);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].captured_at, t0 + 10 * MS);
        assert!(out[0].samples.iter().all(|&s| (s - 0.3).abs() < 1e-6));
    }

    #[test]
    fn loud_sums_are_clamped() {
        let t0 = Instant::now();
        let mut m = Mixer::new(t0);
        m.push(1, &chunk(t0, 480, 0.8));
        m.push(2, &chunk(t0, 480, 0.8));
        m.push(3, &chunk(t0, 480, -3.0));
        m.push(3, &chunk(t0 + 10 * MS, 480, -3.0));
        let out = all(&mut m, t0 + 100 * MS);
        assert!(out[0].samples.iter().all(|&s| s == -1.0));
        assert!(out[1].samples.iter().all(|&s| s == -1.0));
        let mut m = Mixer::new(t0);
        m.push(1, &chunk(t0, 480, 0.8));
        m.push(2, &chunk(t0, 480, 0.8));
        assert!(
            all(&mut m, t0 + 100 * MS)[0]
                .samples
                .iter()
                .all(|&s| s == 1.0)
        );
    }

    #[test]
    fn a_partial_block_is_padded_with_silence() {
        let t0 = Instant::now();
        let mut m = Mixer::new(t0);
        m.push(1, &chunk(t0, 100, 0.5));
        let out = all(&mut m, t0 + 50 * MS);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].samples.len(), BLOCK * CHANNELS);
        assert_eq!(out[0].samples[100 * CHANNELS - 1], 0.5);
        assert_eq!(out[0].samples[100 * CHANNELS], 0.0);
    }

    #[test]
    fn a_long_idle_time_costs_nothing() {
        let t0 = Instant::now();
        let mut m = Mixer::new(t0);
        assert!(m.pop(t0 + Duration::from_secs(3600)).is_none());
        m.push(1, &chunk(t0 + Duration::from_secs(3600), 480, 0.1));
        let out = all(&mut m, t0 + Duration::from_secs(3601));
        assert_eq!(out.len(), 1);
    }
}

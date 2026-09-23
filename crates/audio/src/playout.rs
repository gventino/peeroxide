//! Decides where each received audio chunk goes in the output queue so that it plays in sync
//! with the video, with enough buffer to ride out network jitter.
//!
//! Every time is in microseconds. Capture times come from the broadcaster's clock, arrival and
//! "now" from ours; the difference between the two clocks is unknown, but it is the same for
//! audio and video, so comparing their offsets (`local time − capture time`) cancels it out.
//! Only audio is ever delayed to match video, never the other way around.

use std::collections::VecDeque;

use crate::SAMPLE_RATE;

/// Arrivals considered when estimating the network delay and its jitter.
const WINDOW_US: i64 = 3_000_000;
/// Jitter margin above the fastest recent arrival: at least this…
const MIN_MARGIN_US: i64 = 40_000;
/// …and at most this.
const MAX_MARGIN_US: i64 = 150_000;
/// Audio waits for slow video by at most this much beyond the fastest arrival.
const MAX_SYNC_US: i64 = 200_000;
/// Drift below this is left alone, so corrections (a short skip or gap) stay rare.
const TOLERANCE_US: i64 = 20_000;
/// Longest silence inserted in one go.
const MAX_PAD_US: i64 = 500_000;

/// Where to put a chunk in the output queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    /// Frames of silence to queue before the chunk (it would play too early).
    pub pad: usize,
    /// Frames to drop from the start of the chunk (it would play too late).
    pub skip: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayoutStats {
    /// Delay added on top of the fastest recent network delay, for jitter and sync.
    pub buffer_ms: f32,
    /// How much later than the matching video the audio plays (negative: earlier).
    pub av_offset_ms: Option<f32>,
}

#[derive(Default)]
pub struct Playout {
    /// (arrival, arrival − capture) for the last [`WINDOW_US`].
    arrivals: VecDeque<(i64, i64)>,
    video_offset: Option<i64>,
    stats: PlayoutStats,
}

fn frames(us: i64) -> usize {
    (us.max(0) as u64 * u64::from(SAMPLE_RATE) / 1_000_000) as usize
}

fn us(frames: usize) -> i64 {
    (frames as u64 * 1_000_000 / u64::from(SAMPLE_RATE)) as i64
}

impl Playout {
    /// Records that a packet captured at `capture_us` arrived at `arrival_us`.
    pub fn arrived(&mut self, capture_us: u64, arrival_us: u64) {
        let (capture, arrival) = (capture_us as i64, arrival_us as i64);
        while self
            .arrivals
            .front()
            .is_some_and(|(at, _)| arrival - at > WINDOW_US)
        {
            self.arrivals.pop_front();
        }
        self.arrivals.push_back((arrival, arrival - capture));
    }

    /// `local time − capture time` of the video frames being shown, if any.
    pub fn set_video_offset(&mut self, offset_us: Option<i64>) {
        self.video_offset = offset_us;
    }

    pub fn stats(&self) -> PlayoutStats {
        self.stats
    }

    /// `local play time − capture time` the audio should have. `None` before any arrival.
    fn target_offset(&self) -> Option<i64> {
        let fastest = self.arrivals.iter().map(|(_, o)| *o).min()?;
        let slowest = self.arrivals.iter().map(|(_, o)| *o).max()?;
        let margin = (slowest - fastest + 10_000).clamp(MIN_MARGIN_US, MAX_MARGIN_US);
        let own = fastest + margin;
        let target = match self.video_offset {
            Some(video) => video.clamp(own, own.max(fastest + MAX_SYNC_US)),
            None => own,
        };
        Some(target)
    }

    /// Places a chunk of `len` frames captured at `capture_us`, given that anything queued now
    /// starts playing at `now_us + queued_us`.
    pub fn place(&mut self, capture_us: u64, len: usize, now_us: u64, queued_us: u64) -> Placement {
        let capture = capture_us as i64;
        let starts = (now_us + queued_us) as i64;
        let target = self
            .target_offset()
            .unwrap_or(now_us as i64 - capture + MIN_MARGIN_US);
        let fastest = self
            .arrivals
            .iter()
            .map(|(_, o)| *o)
            .min()
            .unwrap_or(target);
        let late = starts - (capture + target);
        let placement = if late > TOLERANCE_US {
            Placement {
                pad: 0,
                skip: frames(late).min(len),
            }
        } else if late < -TOLERANCE_US {
            Placement {
                pad: frames((-late).min(MAX_PAD_US)),
                skip: 0,
            }
        } else {
            Placement::default()
        };
        let offset = starts + us(placement.pad) - us(placement.skip) - capture;
        self.stats = PlayoutStats {
            buffer_ms: (offset - fastest) as f32 / 1000.0,
            av_offset_ms: self.video_offset.map(|v| (offset - v) as f32 / 1000.0),
        };
        placement
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;
    /// Our clock runs this far ahead of the broadcaster's; it must never matter.
    const SKEW: u64 = 3_600_000_000;
    const CHUNK: usize = 960;

    /// A steady stream: captured every 20 ms, arriving `net` ms later.
    fn steady(p: &mut Playout, from: u64, count: u64, net: u64) {
        for i in 0..count {
            let capture = from + i * 20 * MS;
            p.arrived(capture, capture + SKEW + net * MS);
        }
    }

    #[test]
    fn first_chunk_is_buffered_by_the_jitter_margin() {
        let mut p = Playout::default();
        p.arrived(0, SKEW + 5 * MS);
        // Nothing queued: it would start right away, 5 ms after capture; wanted at 5 + 40.
        let placed = p.place(0, CHUNK, SKEW + 5 * MS, 0);
        assert_eq!(placed.skip, 0);
        assert_eq!(placed.pad, frames(40_000));
        assert_eq!(p.stats().buffer_ms, 40.0);
    }

    #[test]
    fn small_drift_is_left_alone_and_large_drift_corrected() {
        let mut p = Playout::default();
        steady(&mut p, 0, 50, 5);
        let capture = 49 * 20 * MS;
        let arrival = capture + SKEW + 5 * MS;
        // Target: 45 ms after capture. 10 ms early or late: fine.
        for queued in [30 * MS, 50 * MS] {
            assert_eq!(
                p.place(capture, CHUNK, arrival, queued),
                Placement::default()
            );
        }
        // 60 ms late: drop 60 ms from the start of a 100 ms chunk…
        assert_eq!(
            p.place(capture, 5 * CHUNK, arrival, 100 * MS),
            Placement {
                pad: 0,
                skip: frames(60_000)
            }
        );
        // …or all of a 20 ms one.
        assert_eq!(p.place(capture, CHUNK, arrival, 100 * MS).skip, CHUNK);
        // 30 ms early: insert 30 ms of silence.
        assert_eq!(
            p.place(capture, CHUNK, arrival, 10 * MS).pad,
            frames(30_000)
        );
    }

    #[test]
    fn audio_waits_for_slower_video() {
        let mut p = Playout::default();
        steady(&mut p, 0, 50, 5);
        p.set_video_offset(Some((SKEW + 120 * MS) as i64));
        let capture = 49 * 20 * MS;
        let now = capture + SKEW + 5 * MS;
        let placed = p.place(capture, CHUNK, now, 0);
        assert_eq!(placed.pad, frames(115_000));
        let stats = p.stats();
        assert_eq!(stats.av_offset_ms, Some(0.0));
        assert_eq!(stats.buffer_ms, 115.0);
    }

    #[test]
    fn audio_never_waits_more_than_the_sync_cap() {
        let mut p = Playout::default();
        steady(&mut p, 0, 50, 5);
        p.set_video_offset(Some((SKEW + 2_000 * MS) as i64));
        let capture = 49 * 20 * MS;
        let placed = p.place(capture, CHUNK, capture + SKEW + 5 * MS, 0);
        assert_eq!(placed.pad, frames(200_000));
    }

    #[test]
    fn faster_video_does_not_pull_audio_below_its_margin() {
        let mut p = Playout::default();
        steady(&mut p, 0, 50, 5);
        p.set_video_offset(Some((SKEW + MS) as i64));
        let capture = 49 * 20 * MS;
        let placed = p.place(capture, CHUNK, capture + SKEW + 5 * MS, 0);
        assert_eq!(placed.pad, frames(40_000));
        assert_eq!(p.stats().av_offset_ms, Some(44.0));
    }

    #[test]
    fn jitter_widens_the_margin_and_old_arrivals_are_forgotten() {
        let mut p = Playout::default();
        steady(&mut p, 0, 50, 5);
        p.arrived(50 * 20 * MS, 50 * 20 * MS + SKEW + 105 * MS);
        // Spread of 100 ms: margin 110 ms above the fastest arrival.
        let capture = 51 * 20 * MS;
        let placed = p.place(capture, CHUNK, capture + SKEW + 5 * MS, 0);
        assert_eq!(placed.pad, frames(110_000));

        // Four seconds later only recent, steady arrivals count: back to 40 ms.
        steady(&mut p, 250 * 20 * MS, 50, 5);
        let capture = 299 * 20 * MS;
        let placed = p.place(capture, CHUNK, capture + SKEW + 5 * MS, 0);
        assert_eq!(placed.pad, frames(40_000));
    }

    #[test]
    fn a_gap_in_the_stream_is_rebuffered() {
        let mut p = Playout::default();
        steady(&mut p, 0, 50, 5);
        // Silence on the broadcaster (nothing sent for 2 s); the queue ran dry.
        steady(&mut p, 150 * 20 * MS, 1, 5);
        let capture = 150 * 20 * MS;
        let placed = p.place(capture, CHUNK, capture + SKEW + 5 * MS, 10 * MS);
        assert_eq!(placed.pad, frames(30_000));
    }
}

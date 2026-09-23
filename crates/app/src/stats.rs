use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(1);

/// Per-second rates for a stream of work items (frames).
pub struct Meter {
    window_start: Instant,
    frames: u32,
    bytes: u64,
    busy: Duration,
    snapshot: Rates,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rates {
    pub fps: f32,
    pub kbps: f32,
    pub avg_ms: f32,
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            window_start: Instant::now(),
            frames: 0,
            bytes: 0,
            busy: Duration::ZERO,
            snapshot: Rates::default(),
        }
    }
}

impl Meter {
    pub fn record(&mut self, bytes: usize, busy: Duration) {
        self.roll();
        self.frames += 1;
        self.bytes += bytes as u64;
        self.busy += busy;
    }

    pub fn rates(&mut self) -> Rates {
        self.roll();
        self.snapshot
    }

    fn roll(&mut self) {
        let elapsed = self.window_start.elapsed();
        if elapsed < WINDOW {
            return;
        }
        let secs = elapsed.as_secs_f32();
        self.snapshot = Rates {
            fps: self.frames as f32 / secs,
            kbps: self.bytes as f32 * 8.0 / secs / 1000.0,
            avg_ms: if self.frames == 0 {
                0.0
            } else {
                self.busy.as_secs_f32() * 1000.0 / self.frames as f32
            },
        };
        self.window_start = Instant::now();
        self.frames = 0;
        self.bytes = 0;
        self.busy = Duration::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_rates_over_window_and_decays_to_zero() {
        let mut m = Meter {
            window_start: Instant::now() - WINDOW,
            ..Default::default()
        };
        m.frames = 30;
        m.bytes = 125_000;
        m.busy = Duration::from_millis(300);
        let r = m.rates();
        assert!((r.fps - 30.0).abs() < 1.0);
        assert!((r.kbps - 1000.0).abs() < 30.0);
        assert!((r.avg_ms - 10.0).abs() < 0.01);

        m.window_start = Instant::now() - WINDOW;
        assert_eq!(m.rates().fps, 0.0);
    }
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::Frame;
use crate::slot::FrameSlot;

pub(crate) const WIDTH: u32 = 1280;
pub(crate) const HEIGHT: u32 = 720;
const BOX: u32 = 120;
const BARS: [[u8; 4]; 8] = [
    [255, 255, 255, 255],
    [0, 255, 255, 255],
    [255, 255, 0, 255],
    [0, 255, 0, 255],
    [255, 0, 255, 255],
    [0, 0, 255, 255],
    [255, 0, 0, 255],
    [0, 0, 0, 255],
];

pub(crate) struct Guard {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub(crate) fn start(slot: Arc<FrameSlot>, fps: u32) -> Guard {
    let stop = Arc::new(AtomicBool::new(false));
    let interval = Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));
    let thread = std::thread::Builder::new()
        .name("capture-test-pattern".into())
        .spawn({
            let stop = stop.clone();
            move || {
                let background = background();
                let mut n: u64 = 0;
                let mut next_at = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    slot.put(render(&background, n));
                    n += 1;
                    next_at += interval;
                    if let Some(d) = next_at.checked_duration_since(Instant::now()) {
                        std::thread::sleep(d);
                    } else {
                        next_at = Instant::now();
                    }
                }
            }
        })
        .expect("spawn test pattern thread");
    Guard {
        stop,
        thread: Some(thread),
    }
}

fn background() -> Vec<u8> {
    let mut data = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    let bar_w = WIDTH / BARS.len() as u32;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = ((y * WIDTH + x) * 4) as usize;
            let px = if y < HEIGHT * 2 / 3 {
                BARS[((x / bar_w) as usize).min(BARS.len() - 1)]
            } else {
                let v = (x * 255 / (WIDTH - 1)) as u8;
                [v, v, v, 255]
            };
            data[i..i + 4].copy_from_slice(&px);
        }
    }
    data
}

/// Renders frame `n`: static bars plus a bouncing box and a 32-bit frame counter strip.
pub(crate) fn render(background: &[u8], n: u64) -> Frame {
    let mut data = background.to_vec();
    let span_x = u64::from(WIDTH - BOX);
    let span_y = u64::from(HEIGHT * 2 / 3 - BOX);
    let bx = bounce(n * 7, span_x) as u32;
    let by = bounce(n * 5, span_y) as u32;
    fill(&mut data, bx, by, BOX, BOX, [40, 40, 40, 255]);

    let cell = WIDTH / 32;
    for bit in 0..32 {
        let on = (n >> (31 - bit)) & 1 == 1;
        let c = if on {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 255]
        };
        fill(&mut data, bit * cell, HEIGHT - 40, cell - 2, 30, c);
    }

    Frame {
        width: WIDTH,
        height: HEIGHT,
        data,
        captured_at: Instant::now(),
    }
}

fn bounce(pos: u64, span: u64) -> u64 {
    let p = pos % (2 * span);
    if p < span { p } else { 2 * span - p }
}

fn fill(data: &mut [u8], x: u32, y: u32, w: u32, h: u32, px: [u8; 4]) {
    for row in y..(y + h).min(HEIGHT) {
        let start = ((row * WIDTH + x) * 4) as usize;
        let end = ((row * WIDTH + (x + w).min(WIDTH)) * 4) as usize;
        for chunk in data[start..end].chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_differ_and_have_expected_size() {
        let bg = background();
        let a = render(&bg, 0);
        let b = render(&bg, 1);
        assert_eq!(a.data.len(), (WIDTH * HEIGHT * 4) as usize);
        assert_ne!(a.data, b.data);
    }
}

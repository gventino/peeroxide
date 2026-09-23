use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::{CloseReason, Frame, Next};

/// Single-slot mailbox: producers overwrite, the consumer always gets the newest frame.
#[derive(Default)]
pub(crate) struct FrameSlot {
    state: Mutex<State>,
    cv: Condvar,
}

#[derive(Default)]
struct State {
    frame: Option<Frame>,
    closed: Option<CloseReason>,
}

impl FrameSlot {
    pub(crate) fn put(&self, frame: Frame) {
        let mut s = self.state.lock().unwrap();
        if s.closed.is_none() {
            s.frame = Some(frame);
            self.cv.notify_one();
        }
    }

    pub(crate) fn close(&self, reason: CloseReason) {
        let mut s = self.state.lock().unwrap();
        s.closed.get_or_insert(reason);
        self.cv.notify_all();
    }

    pub(crate) fn next(&self, timeout: Duration) -> Next {
        let deadline = Instant::now() + timeout;
        let mut s = self.state.lock().unwrap();
        loop {
            if let Some(f) = s.frame.take() {
                return Next::Frame(f);
            }
            if let Some(r) = &s.closed {
                return Next::Closed(r.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Next::Timeout;
            }
            s = self.cv.wait_timeout(s, deadline - now).unwrap().0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32) -> Frame {
        Frame {
            width: w,
            height: 1,
            data: vec![0; w as usize * 4],
            captured_at: Instant::now(),
        }
    }

    #[test]
    fn keeps_only_newest_frame() {
        let slot = FrameSlot::default();
        slot.put(frame(1));
        slot.put(frame(2));
        assert!(matches!(slot.next(Duration::ZERO), Next::Frame(f) if f.width == 2));
        assert!(matches!(slot.next(Duration::from_millis(5)), Next::Timeout));
    }

    #[test]
    fn delivers_pending_frame_before_close() {
        let slot = FrameSlot::default();
        slot.put(frame(1));
        slot.close(CloseReason::SourceClosed);
        slot.put(frame(2));
        assert!(matches!(slot.next(Duration::ZERO), Next::Frame(f) if f.width == 1));
        assert!(matches!(
            slot.next(Duration::ZERO),
            Next::Closed(CloseReason::SourceClosed)
        ));
    }
}

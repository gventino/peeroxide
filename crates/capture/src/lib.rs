//! Screen capture: enumerate monitors/windows and stream BGRA frames from one of them.

#[cfg(any(not(windows), test))]
mod pixels;
mod slot;
mod test_pattern;

#[cfg(not(windows))]
mod scap_backend;
#[cfg(windows)]
mod windows;

use std::sync::Arc;
use std::time::{Duration, Instant};

use slot::FrameSlot;

/// One captured frame: tightly packed BGRA8, `stride == width * 4`.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub captured_at: Instant,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Monitor,
    Window,
    TestPattern,
}

/// Something that can be captured. Obtain one from [`list_sources`] or [`Source::test_pattern`].
#[derive(Clone, Debug)]
pub struct Source {
    pub kind: SourceKind,
    pub name: String,
    target: Target,
}

#[derive(Clone, Debug)]
enum Target {
    TestPattern,
    #[cfg(windows)]
    Monitor(usize),
    #[cfg(windows)]
    Window(usize),
    #[cfg(not(windows))]
    Scap(Option<scap::Target>),
}

impl Source {
    pub fn test_pattern() -> Self {
        Self {
            kind: SourceKind::TestPattern,
            name: "Test pattern".into(),
            target: Target::TestPattern,
        }
    }

    /// Stable identifier, usable to re-select the same source after a refresh.
    pub fn id(&self) -> String {
        match &self.target {
            Target::TestPattern => "test-pattern".into(),
            #[cfg(windows)]
            Target::Monitor(h) => format!("monitor:{h:x}"),
            #[cfg(windows)]
            Target::Window(h) => format!("window:{h:x}"),
            #[cfg(not(windows))]
            Target::Scap(None) => "portal".into(),
            #[cfg(not(windows))]
            Target::Scap(Some(scap::Target::Display(d))) => format!("display:{}", d.id),
            #[cfg(not(windows))]
            Target::Scap(Some(scap::Target::Window(w))) => format!("window:{}", w.id),
        }
    }

    /// The process that owns a window source; its sound is what sharing the window's audio
    /// captures. `None` for monitors, the test pattern, or when the platform doesn't say.
    pub fn process_id(&self) -> Option<u32> {
        #[cfg(windows)]
        if let Target::Window(h) = &self.target {
            return windows::process_id(*h);
        }
        None
    }
}

impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CaptureOptions {
    pub fps: u32,
    pub show_cursor: bool,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            fps: 30,
            show_cursor: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("screen capture is not supported on this system")]
    Unsupported,
    #[error(
        "screen recording permission was not granted (on macOS: System Settings → Privacy & \
         Security → Screen Recording, then restart the app)"
    )]
    PermissionDenied,
    #[error("capture source is no longer available")]
    SourceNotFound,
    #[error("capture backend error: {0}")]
    Backend(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// The captured window was closed (or the monitor disconnected).
    SourceClosed,
    Failed(String),
}

pub enum Next {
    Frame(Frame),
    Timeout,
    Closed(CloseReason),
}

/// A running capture. Dropping it stops the capture.
pub struct CaptureStream {
    slot: Arc<FrameSlot>,
    _guard: Box<dyn Send>,
}

impl CaptureStream {
    /// Waits up to `timeout` for the most recent frame not yet returned; older ones are dropped.
    pub fn next(&self, timeout: Duration) -> Next {
        self.slot.next(timeout)
    }
}

/// Lists capturable monitors and windows. The test pattern is not included.
pub fn list_sources() -> Result<Vec<Source>, CaptureError> {
    #[cfg(windows)]
    return windows::list_sources();
    #[cfg(not(windows))]
    return scap_backend::list_sources();
}

/// One frame of `source`, at most 320 px wide, then the capture stops.
///
/// The test pattern is drawn in-process. Everything else waits up to 400 ms for the first
/// frame of a capture started at 1 fps. A window that cannot be captured (exclusive fullscreen,
/// a closed window) returns an error; the caller shows the name without a picture. So does the
/// Wayland portal source, whose capture would open the portal's dialog just for a picture.
pub fn snapshot(source: &Source) -> Result<Frame, CaptureError> {
    #[cfg(not(windows))]
    if matches!(source.target, Target::Scap(None)) {
        return Err(CaptureError::Backend(
            "the portal source is chosen when the broadcast starts".to_string(),
        ));
    }
    let frame = if source.kind == SourceKind::TestPattern {
        test_pattern::still()
    } else {
        let stream = start(
            source,
            CaptureOptions {
                fps: 1,
                show_cursor: false,
            },
        )?;
        match stream.next(Duration::from_millis(400)) {
            Next::Frame(frame) => frame,
            Next::Timeout => {
                return Err(CaptureError::Backend(
                    "no frame arrived in time".to_string(),
                ));
            }
            Next::Closed(CloseReason::SourceClosed) => return Err(CaptureError::SourceNotFound),
            Next::Closed(CloseReason::Failed(error)) => return Err(CaptureError::Backend(error)),
        }
    };
    downscale_bgra(frame.width, frame.height, &frame.data, 320)
        .ok_or_else(|| CaptureError::Backend("empty frame".to_string()))
}

/// Shrinks tightly packed BGRA so the width is at most `max_width`, keeping the aspect ratio.
/// A frame that is already that narrow is copied as it is.
pub fn downscale_bgra(width: u32, height: u32, bgra: &[u8], max_width: u32) -> Option<Frame> {
    let pixels = (width as usize).checked_mul(height as usize)?;
    let bytes = pixels.checked_mul(4)?;
    if width == 0 || height == 0 || max_width == 0 || bgra.len() < bytes {
        return None;
    }
    let bgra = &bgra[..bytes];
    if width <= max_width {
        return Some(Frame {
            width,
            height,
            data: bgra.to_vec(),
            captured_at: Instant::now(),
        });
    }
    let dst_w = max_width;
    let dst_h = ((u64::from(height) * u64::from(dst_w)) / u64::from(width)).max(1) as u32;
    let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize) * 4];
    for dy in 0..dst_h {
        let y0 = u64::from(dy) * u64::from(height) / u64::from(dst_h);
        let y1 = ((u64::from(dy) + 1) * u64::from(height) / u64::from(dst_h))
            .max(y0 + 1)
            .min(u64::from(height));
        for dx in 0..dst_w {
            let x0 = u64::from(dx) * u64::from(width) / u64::from(dst_w);
            let x1 = ((u64::from(dx) + 1) * u64::from(width) / u64::from(dst_w))
                .max(x0 + 1)
                .min(u64::from(width));
            let mut acc = [0u32; 4];
            let mut count = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y as u32 * width + x as u32) * 4) as usize;
                    acc[0] += u32::from(bgra[i]);
                    acc[1] += u32::from(bgra[i + 1]);
                    acc[2] += u32::from(bgra[i + 2]);
                    acc[3] += u32::from(bgra[i + 3]);
                    count += 1;
                }
            }
            let o = ((dy * dst_w + dx) * 4) as usize;
            out[o] = (acc[0] / count) as u8;
            out[o + 1] = (acc[1] / count) as u8;
            out[o + 2] = (acc[2] / count) as u8;
            out[o + 3] = (acc[3] / count) as u8;
        }
    }
    Some(Frame {
        width: dst_w,
        height: dst_h,
        data: out,
        captured_at: Instant::now(),
    })
}

pub fn start(source: &Source, options: CaptureOptions) -> Result<CaptureStream, CaptureError> {
    let slot = Arc::new(FrameSlot::default());
    let guard: Box<dyn Send> = match &source.target {
        Target::TestPattern => Box::new(test_pattern::start(slot.clone(), options.fps)),
        #[cfg(windows)]
        Target::Monitor(h) => windows::start_monitor(*h, options, slot.clone())?,
        #[cfg(windows)]
        Target::Window(h) => windows::start_window(*h, options, slot.clone())?,
        #[cfg(not(windows))]
        Target::Scap(t) => scap_backend::start(t.clone(), options, slot.clone())?,
    };
    Ok(CaptureStream {
        slot,
        _guard: guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_averages_a_solid_block() {
        let bgra = vec![
            0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255,
        ];
        let small = downscale_bgra(2, 2, &bgra, 1).unwrap();
        assert_eq!((small.width, small.height), (1, 1));
        assert_eq!(&small.data, &[0, 0, 255, 255]);
    }

    #[test]
    fn downscale_leaves_a_narrow_frame_alone() {
        let bgra = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let small = downscale_bgra(2, 1, &bgra, 4).unwrap();
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.data, bgra);
    }

    #[test]
    fn downscale_rejects_a_short_buffer() {
        assert!(downscale_bgra(2, 2, &[0, 0, 0, 0], 1).is_none());
    }

    #[test]
    fn test_pattern_snapshot_is_a_still() {
        let frame = snapshot(&Source::test_pattern()).unwrap();
        assert!(frame.width <= 320);
        assert!(frame.height > 0);
        assert_eq!(frame.data.len(), (frame.width * frame.height * 4) as usize);
    }
}

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

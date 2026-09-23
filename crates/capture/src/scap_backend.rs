//! macOS (ScreenCaptureKit) and Linux (PipeWire portal) capture via `scap`.
//! Not runtime-verified yet: only Windows is available for testing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use scap::capturer::{Capturer, Options};
use scap::frame::{Frame as ScapFrame, FrameType};

use crate::pixels::{self, Layout};
use crate::slot::FrameSlot;
use crate::{CaptureError, CaptureOptions, CloseReason, Frame, Source, SourceKind, Target};

pub(crate) fn list_sources() -> Result<Vec<Source>, CaptureError> {
    if !scap::is_supported() {
        return Err(CaptureError::Unsupported);
    }
    let targets = scap::get_all_targets();
    if targets.is_empty() {
        // Linux/Wayland: the source is chosen in the xdg-desktop-portal dialog when capture starts.
        return Ok(vec![Source {
            kind: SourceKind::Monitor,
            name: "Choose via system dialog".into(),
            target: Target::Scap(None),
        }]);
    }
    Ok(targets
        .into_iter()
        .filter_map(|t| {
            let (kind, name) = match &t {
                scap::Target::Display(d) => (SourceKind::Monitor, d.title.clone()),
                scap::Target::Window(w) if !w.title.trim().is_empty() => {
                    (SourceKind::Window, w.title.clone())
                }
                scap::Target::Window(_) => return None,
            };
            Some(Source {
                kind,
                name,
                target: Target::Scap(Some(t)),
            })
        })
        .collect())
}

struct Guard {
    stop: Arc<AtomicBool>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        // The capture thread may be blocked waiting for a frame; it stops on its next wake-up.
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub(crate) fn start(
    target: Option<scap::Target>,
    options: CaptureOptions,
    slot: Arc<FrameSlot>,
) -> Result<Box<dyn Send>, CaptureError> {
    if !scap::has_permission() && !scap::request_permission() {
        return Err(CaptureError::PermissionDenied);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("capture-scap".into())
        .spawn({
            let stop = stop.clone();
            move || run(target, options, slot, stop, ready_tx)
        })
        .map_err(|e| CaptureError::Backend(e.to_string()))?;
    ready_rx
        .recv()
        .map_err(|_| CaptureError::Backend("capture thread exited".into()))??;
    Ok(Box::new(Guard { stop }))
}

fn run(
    target: Option<scap::Target>,
    options: CaptureOptions,
    slot: Arc<FrameSlot>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), CaptureError>>,
) {
    let mut capturer = match Capturer::build(Options {
        fps: options.fps,
        show_cursor: options.show_cursor,
        show_highlight: false,
        target,
        output_type: FrameType::BGRAFrame,
        ..Default::default()
    }) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(CaptureError::Backend(e.to_string())));
            return;
        }
    };
    capturer.start_capture();
    let _ = ready.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        match capturer.get_next_frame() {
            Ok(frame) => {
                if let Some(f) = to_bgra(frame) {
                    slot.put(f);
                }
            }
            Err(_) => {
                slot.close(CloseReason::SourceClosed);
                break;
            }
        }
    }
    capturer.stop_capture();
}

fn to_bgra(frame: ScapFrame) -> Option<Frame> {
    let (w, h, data, layout) = match frame {
        ScapFrame::BGRA(f) => (f.width, f.height, f.data, Layout::Bgrx),
        ScapFrame::BGRx(f) => (f.width, f.height, f.data, Layout::Bgrx),
        ScapFrame::BGR0(f) => (f.width, f.height, f.data, Layout::Bgrx),
        ScapFrame::RGBx(f) => (f.width, f.height, f.data, Layout::Rgbx),
        ScapFrame::XBGR(f) => (f.width, f.height, f.data, Layout::Xbgr),
        ScapFrame::RGB(f) => (f.width, f.height, f.data, Layout::Rgb),
        ScapFrame::YUVFrame(_) => return None,
    };
    let (width, height) = (u32::try_from(w).ok()?, u32::try_from(h).ok()?);
    Some(Frame {
        width,
        height,
        data: pixels::to_bgra(width, height, &data, layout)?,
        captured_at: Instant::now(),
    })
}

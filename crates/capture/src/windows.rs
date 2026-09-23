use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as WcFrame;
use windows_capture::graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl};
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    GraphicsCaptureItemType, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window;

use crate::slot::FrameSlot;
use crate::{CaptureError, CaptureOptions, CloseReason, Frame, Source, SourceKind, Target};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

struct Handler {
    slot: Arc<FrameSlot>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = Arc<FrameSlot>;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { slot: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut WcFrame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let mut buffer = frame.buffer()?;
        let (width, height) = (buffer.width(), buffer.height());
        if width == 0 || height == 0 {
            return Ok(());
        }
        let data = if buffer.has_padding() {
            let mut packed = Vec::new();
            let _ = buffer.as_nopadding_buffer(&mut packed);
            packed.truncate((width * height * 4) as usize);
            packed
        } else {
            buffer.as_raw_buffer().to_vec()
        };
        self.slot.put(Frame {
            width,
            height,
            data,
            captured_at: Instant::now(),
        });
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        self.slot.close(CloseReason::SourceClosed);
        Ok(())
    }
}

struct Guard {
    control: Option<CaptureControl<Handler, BoxError>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(c) = self.control.take() {
            let _ = c.stop();
        }
    }
}

fn backend_err(e: impl std::fmt::Display) -> CaptureError {
    CaptureError::Backend(e.to_string())
}

pub(crate) fn list_sources() -> Result<Vec<Source>, CaptureError> {
    if !GraphicsCaptureApi::is_supported().unwrap_or(false) {
        return Err(CaptureError::Unsupported);
    }
    let mut sources = Vec::new();

    let primary = Monitor::primary().ok();
    for (i, m) in Monitor::enumerate()
        .map_err(backend_err)?
        .into_iter()
        .enumerate()
    {
        let size = match (m.width(), m.height()) {
            (Ok(w), Ok(h)) => format!(" ({w}x{h})"),
            _ => String::new(),
        };
        let tag = if Some(m) == primary { ", primary" } else { "" };
        let label = m.device_string().unwrap_or_default();
        let name = if label.is_empty() {
            format!("Monitor {}{size}{tag}", i + 1)
        } else {
            format!("Monitor {}: {label}{size}{tag}", i + 1)
        };
        sources.push(Source {
            kind: SourceKind::Monitor,
            name,
            target: Target::Monitor(m.as_raw_hmonitor() as usize),
        });
    }

    let own_pid = std::process::id();
    for w in Window::enumerate().map_err(backend_err)? {
        if !w.is_valid() || w.process_id().ok() == Some(own_pid) {
            continue;
        }
        let Ok(title) = w.title() else { continue };
        let title = title.trim();
        if title.is_empty() {
            continue;
        }
        let name = match w.process_name() {
            Ok(p) if !p.is_empty() => format!("{title} [{p}]"),
            _ => title.to_string(),
        };
        sources.push(Source {
            kind: SourceKind::Window,
            name,
            target: Target::Window(w.as_raw_hwnd() as usize),
        });
    }
    Ok(sources)
}

pub(crate) fn start_monitor(
    handle: usize,
    options: CaptureOptions,
    slot: Arc<FrameSlot>,
) -> Result<Box<dyn Send>, CaptureError> {
    let monitor = Monitor::from_raw_hmonitor(handle as *mut std::ffi::c_void);
    if !Monitor::enumerate()
        .map_err(backend_err)?
        .contains(&monitor)
    {
        return Err(CaptureError::SourceNotFound);
    }
    start_item(monitor, options, slot)
}

pub(crate) fn start_window(
    handle: usize,
    options: CaptureOptions,
    slot: Arc<FrameSlot>,
) -> Result<Box<dyn Send>, CaptureError> {
    let window = Window::from_raw_hwnd(handle as *mut std::ffi::c_void);
    if !window.is_valid() {
        return Err(CaptureError::SourceNotFound);
    }
    start_item(window, options, slot)
}

fn start_item<T>(
    item: T,
    options: CaptureOptions,
    slot: Arc<FrameSlot>,
) -> Result<Box<dyn Send>, CaptureError>
where
    T: TryInto<GraphicsCaptureItemType> + Send + 'static,
{
    let cursor = if GraphicsCaptureApi::is_cursor_settings_supported().unwrap_or(false) {
        if options.show_cursor {
            CursorCaptureSettings::WithCursor
        } else {
            CursorCaptureSettings::WithoutCursor
        }
    } else {
        CursorCaptureSettings::Default
    };
    let border = if GraphicsCaptureApi::is_border_settings_supported().unwrap_or(false) {
        DrawBorderSettings::WithoutBorder
    } else {
        DrawBorderSettings::Default
    };
    let interval = if GraphicsCaptureApi::is_minimum_update_interval_supported().unwrap_or(false) {
        MinimumUpdateIntervalSettings::Custom(Duration::from_secs_f64(
            1.0 / f64::from(options.fps.max(1)),
        ))
    } else {
        MinimumUpdateIntervalSettings::Default
    };

    let settings = Settings::new(
        item,
        cursor,
        border,
        SecondaryWindowSettings::Default,
        interval,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        slot,
    );
    let control = Handler::start_free_threaded(settings).map_err(backend_err)?;
    Ok(Box::new(Guard {
        control: Some(control),
    }))
}

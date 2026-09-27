//! The mini player: while watching, minimizing Peeroxide shows the stream in a small window that
//! stays on top, like Discord's picture-in-picture.
//!
//! It is a *deferred* viewport: eframe repaints a minimized window at most every 100 ms, and an
//! immediate viewport is drawn inside its parent's pass, so it would only reach 10 fps. A
//! deferred one has its own callback and repaints at the stream's rate. It shares the main
//! window's video texture, so switching between the two never shows a black frame.
//!
//! While the main window is minimized, eframe runs no ui pass for it (only `App::logic`), and no
//! window can be created then. So the mini player's window exists, hidden, for as long as a
//! stream is showing, and `App::logic` reveals it ([`reveal`]) as soon as the main window is
//! minimized. From then on it is visible, so eframe runs the main window's passes again (about
//! every 100 ms), which keep it registered and hide it once the main window comes back.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, Pos2, Rect, ResizeDirection, RichText, Sense, Vec2, ViewportBuilder,
    ViewportCommand, ViewportId, pos2, vec2,
};
use peeroxide_audio::OutputControl;

use crate::decoder::VideoSlot;
use crate::fullscreen::controls_visible;
use crate::video::VideoView;

pub const DEFAULT_SIZE: Vec2 = vec2(400.0, 225.0);
pub const MIN_SIZE: Vec2 = vec2(240.0, 135.0);
/// From the monitor's right edge, and from its bottom edge (clearing the taskbar).
const MARGIN: Vec2 = vec2(16.0, 64.0);
const BAR_HEIGHT: f32 = 30.0;
/// How often the frame count goes to the log.
const LOG_EVERY: Duration = Duration::from_secs(5);

pub fn id() -> ViewportId {
    ViewportId::from_hash_of("peeroxide-mini-player")
}

/// The mini player is up while a stream is showing and the main window is minimized.
pub fn wanted(streaming: bool, minimized: bool) -> bool {
    streaming && minimized
}

/// Where the mini player opens: where it was left, if that is entirely on `monitor` (points,
/// from the top left) and not too small; otherwise the bottom-right corner.
pub fn placement(monitor: Vec2, saved: Option<Rect>) -> Rect {
    let screen = Rect::from_min_size(Pos2::ZERO, monitor);
    if let Some(rect) = saved
        && rect.width() >= MIN_SIZE.x
        && rect.height() >= MIN_SIZE.y
        && screen.contains_rect(rect)
    {
        return rect;
    }
    let size = DEFAULT_SIZE.min(monitor);
    let min = (monitor - size - MARGIN).max(Vec2::ZERO);
    Rect::from_min_size(min.to_pos2(), size)
}

/// What the main window must do for the mini player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// 🗙 (or Alt+F4): stop watching.
    Stop,
    /// The mute button: remember the new state.
    Muted(bool),
}

/// Everything the mini player's callback uses. It runs outside the main window's pass, so it
/// only reaches the app through this.
pub struct Shared {
    video: Arc<Mutex<VideoView>>,
    slot: Arc<VideoSlot>,
    output: Arc<OutputControl>,
    title: Mutex<String>,
    audio: AtomicBool,
    actions: Mutex<Vec<Action>>,
    /// The window's last outer rect, to remember.
    rect: Mutex<Option<Rect>>,
    /// When the mini player last showed up, for the controls' grace period.
    opened: Mutex<Instant>,
    frames: AtomicU32,
    counting_since: Mutex<Instant>,
}

impl Shared {
    pub fn new(
        video: Arc<Mutex<VideoView>>,
        slot: Arc<VideoSlot>,
        output: Arc<OutputControl>,
    ) -> Arc<Self> {
        Arc::new(Self {
            video,
            slot,
            output,
            title: Mutex::default(),
            audio: AtomicBool::new(false),
            actions: Mutex::default(),
            rect: Mutex::default(),
            opened: Mutex::new(Instant::now()),
            frames: AtomicU32::new(0),
            counting_since: Mutex::new(Instant::now()),
        })
    }

    /// Who is being watched, and whether they share sound (for the mute button).
    pub fn set_stream(&self, broadcaster_name: &str, audio: bool) {
        *self.title.lock().unwrap() = broadcaster_name.to_string();
        self.audio.store(audio, Ordering::Relaxed);
    }

    pub fn take_actions(&self) -> Vec<Action> {
        std::mem::take(&mut self.actions.lock().unwrap())
    }

    pub fn last_rect(&self) -> Option<Rect> {
        *self.rect.lock().unwrap()
    }

    fn act(&self, ctx: &egui::Context, action: Action) {
        self.actions.lock().unwrap().push(action);
        // The main window handles it in its next pass (it runs at least every 100 ms).
        ctx.request_repaint_of(ViewportId::ROOT);
    }
}

/// Keeps the mini player's window for this pass of the main window: created at `rect`, and
/// shown or hidden.
pub fn show(ctx: &egui::Context, shared: &Arc<Shared>, rect: Rect, visible: bool) {
    let title = format!("Peeroxide — {}", shared.title.lock().unwrap());
    let builder = ViewportBuilder::default()
        .with_title(title)
        .with_position(rect.min)
        .with_inner_size(rect.size())
        .with_min_inner_size(MIN_SIZE)
        .with_decorations(false)
        .with_resizable(true)
        .with_always_on_top()
        .with_taskbar(false)
        .with_active(false)
        .with_visible(visible);
    let shared = shared.clone();
    ctx.show_viewport_deferred(id(), builder, move |ui, _class| mini_ui(ui, &shared));
}

/// Shows the (hidden) mini player from `App::logic`, while the main window is minimized.
pub fn reveal(ctx: &egui::Context) {
    ctx.send_viewport_cmd_to(id(), ViewportCommand::Visible(true));
}

/// Marks the mini player as just opened (for the controls' grace period and the frame log).
pub fn opened(shared: &Shared) {
    *shared.opened.lock().unwrap() = Instant::now();
    *shared.counting_since.lock().unwrap() = Instant::now();
    shared.frames.store(0, Ordering::Relaxed);
}

fn mini_ui(ui: &mut egui::Ui, shared: &Shared) {
    let ctx = ui.ctx().clone();
    let (outer, close_requested) =
        ctx.input(|i| (i.viewport().outer_rect, i.viewport().close_requested()));
    if outer.is_some() {
        *shared.rect.lock().unwrap() = outer;
    }
    if close_requested {
        shared.act(&ctx, Action::Stop);
    }
    count_frame(shared);

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(Color32::BLACK))
        .show(ui, |ui| {
            let video = shared.video.lock().unwrap().ui(ui, &shared.slot, "");
            let drag = ui.interact(video.rect, video.id.with("drag"), Sense::click_and_drag());
            if drag.drag_started() {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }
            if video.double_clicked() || drag.double_clicked() {
                restore(&ctx);
            }

            // Shown while the pointer moves over the player (and for a moment after it opens).
            let since_opened = shared.opened.lock().unwrap().elapsed().as_secs_f32();
            let bar = Rect::from_min_max(
                video.rect.min,
                pos2(video.rect.max.x, video.rect.min.y + BAR_HEIGHT),
            );
            let (since_moved, on_bar) = ui.input(|i| match i.pointer.hover_pos() {
                Some(p) => (i.pointer.time_since_last_movement(), bar.contains(p)),
                None => (f32::INFINITY, false),
            });
            if controls_visible(since_moved, since_opened, on_bar) {
                controls(ui, &ctx, shared, bar);
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        });
}

/// The bar on top of the video: resize grip, name, mute, back to Peeroxide, stop.
fn controls(ui: &mut egui::Ui, ctx: &egui::Context, shared: &Shared, bar: Rect) {
    ui.painter()
        .rect_filled(bar, 0.0, Color32::from_black_alpha(190));
    let mut bar_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(bar.shrink2(vec2(8.0, 3.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    bar_ui.style_mut().visuals = egui::Visuals::dark();
    // The top-left corner resizes (the mini player opens in the bottom-right one).
    let grip = bar_ui
        .add(egui::Label::new("⬉").sense(Sense::drag()))
        .on_hover_cursor(egui::CursorIcon::ResizeNorthWest)
        .on_hover_text("Drag to resize");
    if grip.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(ResizeDirection::NorthWest));
    }
    let title = shared.title.lock().unwrap().clone();
    bar_ui.add(egui::Label::new(RichText::new(title).strong()).truncate());
    bar_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui.button("🗙").on_hover_text("Stop watching").clicked() {
            shared.act(ctx, Action::Stop);
        }
        if ui
            .button("🗖")
            .on_hover_text("Back to Peeroxide (or double-click the video)")
            .clicked()
        {
            restore(ctx);
        }
        if shared.audio.load(Ordering::Relaxed) {
            let muted = shared.output.muted();
            let icon = if muted { "🔇" } else { "🔊" };
            if ui
                .button(icon)
                .on_hover_text(if muted { "Unmute" } else { "Mute" })
                .clicked()
            {
                shared.output.set_muted(!muted);
                shared.act(ctx, Action::Muted(!muted));
            }
        }
    });
}

/// Brings the main window back, which closes the mini player.
fn restore(ctx: &egui::Context) {
    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
}

fn count_frame(shared: &Shared) {
    let frames = shared.frames.fetch_add(1, Ordering::Relaxed) + 1;
    let mut since = shared.counting_since.lock().unwrap();
    let elapsed = since.elapsed();
    if elapsed >= LOG_EVERY {
        tracing::debug!(
            fps = format!("{:.1}", f64::from(frames) / elapsed.as_secs_f64()),
            "mini player repaints"
        );
        *since = Instant::now();
        shared.frames.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_while_streaming_and_minimized() {
        assert!(wanted(true, true));
        assert!(!wanted(true, false), "the main window shows the stream");
        assert!(!wanted(false, true), "nothing to show");
        assert!(!wanted(false, false));
    }

    #[test]
    fn opens_in_the_bottom_right_corner_by_default() {
        let r = placement(vec2(1920.0, 1080.0), None);
        assert_eq!(r, Rect::from_min_size(pos2(1504.0, 791.0), DEFAULT_SIZE));
        let r = placement(vec2(1280.0, 720.0), None);
        assert_eq!(r.max, pos2(1264.0, 656.0));
    }

    #[test]
    fn a_small_monitor_gets_a_smaller_player_that_still_fits() {
        let r = placement(vec2(300.0, 200.0), None);
        assert_eq!(r.min, Pos2::ZERO);
        assert_eq!(r.size(), vec2(300.0, 200.0));
    }

    #[test]
    fn it_reopens_where_it_was_left_if_that_is_still_on_screen() {
        let monitor = vec2(1920.0, 1080.0);
        let left = Rect::from_min_size(pos2(40.0, 60.0), vec2(640.0, 360.0));
        assert_eq!(placement(monitor, Some(left)), left);

        let off_screen = Rect::from_min_size(pos2(1800.0, 60.0), vec2(640.0, 360.0));
        assert_eq!(
            placement(monitor, Some(off_screen)),
            placement(monitor, None)
        );
        let too_small = Rect::from_min_size(pos2(40.0, 60.0), vec2(100.0, 60.0));
        assert_eq!(
            placement(monitor, Some(too_small)),
            placement(monitor, None)
        );
    }
}

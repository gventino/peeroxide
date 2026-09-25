//! When the watched stream is shown fullscreen, and when its controls are visible there.

/// Controls stay up this long after the pointer last moved, in seconds.
pub const SHOW_CONTROLS_FOR: f32 = 2.0;
/// Controls also stay up this long after entering fullscreen, so a viewer who pressed F11 sees
/// how to leave.
pub const SHOW_CONTROLS_ON_ENTER: f32 = 3.0;

/// The fullscreen state to ask for, given the current one, whether a stream is showing, and this
/// frame's requests: `toggle` (F11, a double-click on the video, the Fullscreen button) and `exit`
/// (Esc, the exit button).
pub fn wanted(fullscreen: bool, streaming: bool, toggle: bool, exit: bool) -> bool {
    if !streaming || exit {
        // Also when the stream ends: the viewer needs the normal window to see why.
        return false;
    }
    fullscreen != toggle
}

/// Whether the fullscreen control bar (and the cursor) is shown. Times are in seconds.
pub fn controls_visible(
    since_pointer_moved: f32,
    since_entered: f32,
    pointer_on_bar: bool,
) -> bool {
    pointer_on_bar
        || since_pointer_moved < SHOW_CONTROLS_FOR
        || since_entered < SHOW_CONTROLS_ON_ENTER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_enters_and_leaves_only_while_streaming() {
        assert!(wanted(false, true, true, false), "F11 while streaming");
        assert!(!wanted(true, true, true, false), "F11 again");
        assert!(!wanted(false, false, true, false), "nothing to show yet");
        assert!(wanted(true, true, false, false), "stays fullscreen");
        assert!(!wanted(false, true, false, false), "stays windowed");
    }

    #[test]
    fn exit_and_the_stream_ending_leave_fullscreen() {
        assert!(!wanted(true, true, false, true), "Esc");
        assert!(!wanted(true, true, true, true), "Esc wins over a toggle");
        assert!(
            !wanted(false, true, false, true),
            "Esc while windowed changes nothing"
        );
        assert!(!wanted(true, false, false, false), "the stream ended");
    }

    #[test]
    fn controls_hide_after_both_grace_periods_but_not_on_the_bar() {
        assert!(controls_visible(0.5, 10.0, false), "pointer just moved");
        assert!(controls_visible(10.0, 1.0, false), "just entered");
        assert!(!controls_visible(2.5, 3.5, false), "idle");
        assert!(controls_visible(30.0, 30.0, true), "pointer on the bar");
    }
}

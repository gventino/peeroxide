//! The apps playing sound, listed on a background thread for the "Mute apps" checklist, and only
//! while the checklist is on screen.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, Sender, bounded};
use peeroxide_audio::{AudioApp, audio_apps};

/// How often the list is refreshed while it's shown.
const REFRESH: Duration = Duration::from_secs(2);
/// Not asked for in this long: the checklist isn't on screen any more.
const HIDDEN: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Shared {
    apps: Mutex<Vec<AudioApp>>,
    /// When the list was last asked for.
    shown: Mutex<Option<Instant>>,
    stop: AtomicBool,
}

impl Shared {
    fn recently_shown(&self) -> bool {
        self.shown
            .lock()
            .unwrap()
            .is_some_and(|at| at.elapsed() < HIDDEN)
    }
}

pub struct AppList {
    shared: Arc<Shared>,
    /// Holds at most one pending wake-up (like the encoder's), for an immediate refresh.
    wake: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl AppList {
    /// `on_update` runs after each refresh (to repaint).
    pub fn start(on_update: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        let (wake, woken): (Sender<()>, Receiver<()>) = bounded(1);
        let thread = std::thread::Builder::new()
            .name("audio-apps".into())
            .spawn({
                let shared = shared.clone();
                move || {
                    while !shared.stop.load(Ordering::Relaxed) {
                        if shared.recently_shown() {
                            match audio_apps() {
                                Ok(apps) => {
                                    *shared.apps.lock().unwrap() = apps;
                                    on_update();
                                }
                                Err(e) => tracing::debug!("could not list audio apps: {e:#}"),
                            }
                        }
                        let _ = woken.recv_timeout(REFRESH);
                    }
                }
            })
            .ok();
        Self {
            shared,
            wake,
            thread,
        }
    }

    /// The latest list; keeps it refreshing while it's being shown. The first call after a
    /// while asks for a refresh right away.
    pub fn apps(&self) -> Vec<AudioApp> {
        let newly_shown = !self.shared.recently_shown();
        *self.shared.shown.lock().unwrap() = Some(Instant::now());
        if newly_shown {
            let _ = self.wake.try_send(());
        }
        self.shared.apps.lock().unwrap().clone()
    }
}

impl Drop for AppList {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.try_send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

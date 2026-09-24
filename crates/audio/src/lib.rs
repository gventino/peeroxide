//! Audio I/O: capture what the shared source plays, and play a received stream.
//!
//! PCM is always 48 kHz interleaved stereo f32 at this crate's boundary.
//!
//! Capture is enforced by the OS API itself (abuse case AC-10): a shared window's audio comes
//! from that application's process tree only, and a shared screen's from everything except
//! Peeroxide. The microphone is never captured.

#[cfg(windows)]
mod loopback;
mod output;
mod playout;
mod tone;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::anyhow;

pub use output::{AudioOutput, OutputControl};
pub use playout::{Placement, Playout, PlayoutStats};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

/// What to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioSource {
    /// Everything the computer plays, except this process tree (Peeroxide itself, so a peer
    /// that also watches someone never re-broadcasts their stream).
    System { exclude_pid: u32 },
    /// Only what this process and its children play (the shared window's application).
    Application { pid: u32 },
    /// A 440 Hz beep during the first 100 ms of every wall-clock second, for testing.
    TestTone,
}

/// Interleaved stereo samples; `captured_at` is when the first one was captured.
#[derive(Clone, Debug)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub captured_at: Instant,
}

/// Whether `source` can be captured on this platform at all (see [`check`] for this machine).
pub fn is_supported(source: &AudioSource) -> bool {
    match source {
        AudioSource::TestTone => true,
        AudioSource::System { .. } | AudioSource::Application { .. } => cfg!(windows),
    }
}

/// Checks that `source` can be captured on this machine by opening it and closing it again.
pub fn check(source: &AudioSource) -> anyhow::Result<()> {
    match source {
        AudioSource::TestTone => Ok(()),
        #[cfg(windows)]
        AudioSource::System { .. } | AudioSource::Application { .. } => loopback::check(*source),
        #[cfg(not(windows))]
        AudioSource::System { .. } | AudioSource::Application { .. } => {
            anyhow::bail!("audio sharing isn't available on this system yet")
        }
    }
}

pub enum Next {
    Chunk(AudioChunk),
    /// Nothing new yet. Loopback capture may deliver nothing at all while the source is silent.
    Timeout,
    Failed(anyhow::Error),
}

/// Chunks buffered between the capture thread and its reader before new ones are dropped.
const QUEUE: usize = 64;

/// A running capture. Dropping it stops the capture thread.
pub struct AudioCapture {
    rx: Receiver<anyhow::Result<AudioChunk>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioCapture {
    /// Waits up to `timeout` for the next chunk.
    pub fn next(&self, timeout: Duration) -> Next {
        match self.rx.recv_timeout(timeout) {
            Ok(Ok(chunk)) => Next::Chunk(chunk),
            Ok(Err(e)) => Next::Failed(e),
            Err(RecvTimeoutError::Timeout) => Next::Timeout,
            Err(RecvTimeoutError::Disconnected) => Next::Failed(anyhow!("audio capture stopped")),
        }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Where a capture thread delivers its chunks. Never blocks: if the reader falls behind, chunks
/// are dropped.
pub(crate) struct Sink {
    tx: SyncSender<anyhow::Result<AudioChunk>>,
    pub(crate) stop: Arc<AtomicBool>,
}

impl Sink {
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub(crate) fn chunk(&self, chunk: AudioChunk) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(Ok(chunk)) {
            tracing::debug!("audio reader behind; dropping a chunk");
        }
    }

    pub(crate) fn fail(&self, error: anyhow::Error) {
        let _ = self.tx.try_send(Err(error));
    }
}

/// Starts capturing `source` on its own thread.
pub fn start_capture(source: &AudioSource) -> anyhow::Result<AudioCapture> {
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    let stop = Arc::new(AtomicBool::new(false));
    let sink = Sink {
        tx,
        stop: stop.clone(),
    };
    let thread = match source {
        AudioSource::TestTone => tone::start(sink)?,
        #[cfg(windows)]
        AudioSource::System { .. } | AudioSource::Application { .. } => {
            loopback::start(*source, sink)?
        }
        #[cfg(not(windows))]
        AudioSource::System { .. } | AudioSource::Application { .. } => {
            anyhow::bail!("audio sharing isn't available on this system yet");
        }
    };
    Ok(AudioCapture {
        rx,
        stop,
        thread: Some(thread),
    })
}

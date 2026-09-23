//! Windows process loopback (Windows 10 2004+): captures what one process tree plays, or
//! everything except one process tree, straight from the audio engine.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wasapi::{
    AudioCaptureClient, AudioClient, Direction, Handle, SampleType, StreamMode, WaveFormat,
    initialize_mta,
};

use crate::{AudioChunk, AudioError, AudioSource, CHANNELS, SAMPLE_RATE, Sink};

const BYTES_PER_FRAME: usize = CHANNELS * 4;
/// Engine buffer; the engine delivers about every 10 ms regardless.
const BUFFER_HNS: i64 = 200_000;
/// How often the thread checks for a stop request while the source is silent.
const WAKE_MS: u32 = 100;

fn backend(context: &str, e: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(format!("{context}: {e}"))
}

struct Opened {
    client: AudioClient,
    capture: AudioCaptureClient,
    event: Handle,
}

/// Must run on a thread that has initialized COM.
fn open(source: AudioSource) -> Result<Opened, AudioError> {
    let (pid, include_tree) = match source {
        AudioSource::Application { pid } => (pid, true),
        AudioSource::System { exclude_pid } => (exclude_pid, false),
        AudioSource::TestTone => unreachable!("not a loopback source"),
    };
    let mut client =
        AudioClient::new_application_loopback_client(pid, include_tree).map_err(|e| {
            backend(
                "could not start audio capture (needs Windows 10 2004 or later)",
                e,
            )
        })?;
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        SAMPLE_RATE as usize,
        CHANNELS,
        None,
    );
    client
        .initialize_client(
            &format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: BUFFER_HNS,
            },
        )
        .map_err(|e| backend("could not configure audio capture", e))?;
    let event = client
        .set_get_eventhandle()
        .map_err(|e| backend("audio capture event", e))?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|e| backend("audio capture client", e))?;
    Ok(Opened {
        client,
        capture,
        event,
    })
}

/// Opens and closes the capture on a fresh COM thread, so the caller's apartment doesn't matter.
pub(crate) fn check(source: AudioSource) -> Result<(), AudioError> {
    std::thread::Builder::new()
        .name("audio-check".into())
        .spawn(move || {
            let _ = initialize_mta();
            open(source).map(drop)
        })
        .map_err(|e| backend("thread", e))?
        .join()
        .unwrap_or_else(|_| Err(AudioError::Backend("audio check panicked".into())))
}

pub(crate) fn start(source: AudioSource, sink: Sink) -> Result<JoinHandle<()>, AudioError> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("audio-capture".into())
        .spawn(move || {
            let _ = initialize_mta();
            let opened = match open(source).and_then(|o| {
                o.client
                    .start_stream()
                    .map_err(|e| backend("could not start audio capture", e))?;
                Ok(o)
            }) {
                Ok(o) => o,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            if let Err(e) = run(&opened, &sink) {
                tracing::warn!("audio capture failed: {e}");
                sink.fail(e);
            }
            let _ = opened.client.stop_stream();
        })
        .map_err(|e| backend("thread", e))?;
    ready_rx
        .recv()
        .unwrap_or_else(|_| Err(AudioError::Backend("audio capture thread exited".into())))?;
    Ok(thread)
}

fn run(o: &Opened, sink: &Sink) -> Result<(), String> {
    let mut bytes = Vec::new();
    while !sink.stopped() {
        // May time out (harmlessly) while the source is silent: the engine can deliver nothing.
        if o.event.wait_for_event(WAKE_MS).is_err() {
            continue;
        }
        loop {
            let frames = o
                .capture
                .get_next_packet_size()
                .map_err(|e| e.to_string())?
                .unwrap_or(0) as usize;
            if frames == 0 {
                break;
            }
            bytes.resize(frames * BYTES_PER_FRAME, 0);
            let (read, info) = o
                .capture
                .read_from_device(&mut bytes)
                .map_err(|e| e.to_string())?;
            let read = read as usize;
            if read == 0 {
                break;
            }
            // The newest sample was captured just now; the first one a buffer's length ago.
            let captured_at = Instant::now()
                .checked_sub(Duration::from_secs_f64(
                    read as f64 / f64::from(SAMPLE_RATE),
                ))
                .unwrap_or_else(Instant::now);
            let samples = if info.flags.silent {
                vec![0.0; read * CHANNELS]
            } else {
                bytes[..read * BYTES_PER_FRAME]
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect()
            };
            sink.chunk(AudioChunk {
                samples,
                captured_at,
            });
        }
    }
    Ok(())
}

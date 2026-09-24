//! Windows process loopback (Windows 10 2004+): captures what one process tree plays, or
//! everything except one process tree, straight from the audio engine.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use wasapi::{
    AudioCaptureClient, AudioClient, Direction, Handle, SampleType, StreamMode, WaveFormat,
    initialize_mta,
};

use crate::{AudioChunk, AudioSource, CHANNELS, SAMPLE_RATE, Sink};

const BYTES_PER_FRAME: usize = CHANNELS * 4;
/// Engine buffer; the engine delivers about every 10 ms regardless.
const BUFFER_HNS: i64 = 200_000;
/// How often the thread checks for a stop request while the source is silent.
const WAKE_MS: u32 = 100;

struct Opened {
    client: AudioClient,
    capture: AudioCaptureClient,
    event: Handle,
}

/// Must run on a thread that has initialized COM.
fn open(source: AudioSource) -> anyhow::Result<Opened> {
    let (pid, include_tree) = match source {
        AudioSource::Application { pid } => (pid, true),
        AudioSource::System { exclude_pid } => (exclude_pid, false),
        AudioSource::TestTone => unreachable!("not a loopback source"),
    };
    let mut client = AudioClient::new_application_loopback_client(pid, include_tree)
        .context("could not start audio capture (needs Windows 10 2004 or later)")?;
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
        .context("could not configure audio capture")?;
    let event = client
        .set_get_eventhandle()
        .context("audio capture event")?;
    let capture = client
        .get_audiocaptureclient()
        .context("audio capture client")?;
    Ok(Opened {
        client,
        capture,
        event,
    })
}

/// Opens and closes the capture on a fresh COM thread, so the caller's apartment doesn't matter.
pub(crate) fn check(source: AudioSource) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("audio-check".into())
        .spawn(move || {
            let _ = initialize_mta();
            open(source).map(drop)
        })
        .context("could not start the audio check thread")?
        .join()
        .unwrap_or_else(|_| Err(anyhow!("audio check panicked")))
}

pub(crate) fn start(source: AudioSource, sink: Sink) -> anyhow::Result<JoinHandle<()>> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("audio-capture".into())
        .spawn(move || {
            let _ = initialize_mta();
            let opened = match open(source).and_then(|o| {
                o.client
                    .start_stream()
                    .context("could not start audio capture")?;
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
                tracing::warn!("audio capture failed: {e:#}");
                sink.fail(e);
            }
            let _ = opened.client.stop_stream();
        })
        .context("could not start the audio capture thread")?;
    ready_rx
        .recv()
        .unwrap_or_else(|_| Err(anyhow!("audio capture thread exited")))?;
    Ok(thread)
}

fn run(o: &Opened, sink: &Sink) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    while !sink.stopped() {
        // May time out (harmlessly) while the source is silent: the engine can deliver nothing.
        if o.event.wait_for_event(WAKE_MS).is_err() {
            continue;
        }
        loop {
            let frames = o.capture.get_next_packet_size()?.unwrap_or(0) as usize;
            if frames == 0 {
                break;
            }
            bytes.resize(frames * BYTES_PER_FRAME, 0);
            let (read, info) = o.capture.read_from_device(&mut bytes)?;
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

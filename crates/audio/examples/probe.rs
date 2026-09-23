//! Records a few seconds of an audio source into a WAV file and checks the output device.
//! Usage: cargo run -p peeroxide-audio --example probe [system|tone|PID] [seconds] [--play]
//!
//! * `system`: everything this computer plays except this probe; `PID`: only that process tree.
//! * Writes `audio-probe.wav` (16-bit stereo, 48 kHz) and prints the peak level every second.
//! * Opens the default output muted to report its rate and latency; with `--play` it plays the
//!   recording back audibly instead.

use std::io::Write;
use std::time::{Duration, Instant};

use peeroxide_audio::{
    AudioOutput, AudioSource, CHANNELS, Next, OutputControl, SAMPLE_RATE, check, start_capture,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let play = args.iter().any(|a| a == "--play");
    let mut positional = args.iter().filter(|a| !a.starts_with("--"));
    let source = match positional.next().map(String::as_str) {
        None | Some("system") => AudioSource::System {
            exclude_pid: std::process::id(),
        },
        Some("tone") => AudioSource::TestTone,
        Some(pid) => AudioSource::Application {
            pid: pid.parse().expect("system, tone or a process id"),
        },
    };
    let secs: u64 = positional.next().and_then(|a| a.parse().ok()).unwrap_or(3);

    match check(&source) {
        Ok(()) => println!("{source:?}: available"),
        Err(e) => {
            println!("{source:?}: {e}");
            return;
        }
    }
    let capture = start_capture(&source).expect("start capture");
    println!("recording {secs}s…");
    let started = Instant::now();
    let (mut samples, mut chunks, mut peak) = (Vec::new(), 0u32, 0.0f32);
    let mut next_report = Duration::from_secs(1);
    while started.elapsed() < Duration::from_secs(secs) {
        match capture.next(Duration::from_millis(100)) {
            Next::Chunk(c) => {
                chunks += 1;
                peak = c.samples.iter().fold(peak, |p, s| p.max(s.abs()));
                samples.extend(c.samples);
            }
            Next::Timeout => {}
            Next::Failed(e) => {
                println!("capture failed: {e}");
                break;
            }
        }
        if started.elapsed() >= next_report {
            println!(
                "{:>2}s: {chunks} chunks, peak {:.3} ({})",
                next_report.as_secs(),
                peak,
                if peak == 0.0 { "silent" } else { "sound" }
            );
            next_report += Duration::from_secs(1);
            (chunks, peak) = (0, 0.0);
        }
    }
    drop(capture);
    let seconds = samples.len() as f64 / CHANNELS as f64 / f64::from(SAMPLE_RATE);
    println!("captured {seconds:.2}s of audio (some systems deliver nothing while silent)");
    write_wav("audio-probe.wav", &samples).expect("write wav");
    println!("wrote audio-probe.wav");

    let control = OutputControl::new(1.0, !play);
    let mut output = match AudioOutput::open(control) {
        Ok(o) => o,
        Err(e) => {
            println!("output: {e}");
            return;
        }
    };
    let frames = if play {
        samples.len() / CHANNELS
    } else {
        SAMPLE_RATE as usize
    };
    let mut fed = 0;
    let deadline = Instant::now() + Duration::from_secs_f64(frames as f64 / 48_000.0 + 1.0);
    while Instant::now() < deadline {
        // Keep ~100 ms queued, like the viewer does.
        if output.queued() < Duration::from_millis(100) && fed < frames {
            let n = 960.min(frames - fed);
            if play {
                output.push(&samples[fed * CHANNELS..(fed + n) * CHANNELS]);
            } else {
                output.push_silence(n);
            }
            fed += n;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!(
        "output ok: queued {:?} at the end, {} underruns{}",
        output.queued(),
        output.underruns(),
        if output.failed() {
            ", device error"
        } else {
            ""
        }
    );
}

fn write_wav(path: &str, samples: &[f32]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&(CHANNELS as u16).to_le_bytes())?;
    f.write_all(&SAMPLE_RATE.to_le_bytes())?;
    f.write_all(&(SAMPLE_RATE * CHANNELS as u32 * 2).to_le_bytes())?;
    f.write_all(&(CHANNELS as u16 * 2).to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?;
    }
    f.flush()
}

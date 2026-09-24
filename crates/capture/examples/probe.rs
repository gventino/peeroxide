//! Lists capture sources and measures the delivered frame rate of one of them.
//! Usage: cargo run -p peeroxide-capture --example probe [source-index|title-substring] [seconds]

use std::time::{Duration, Instant};

use anyhow::Context;
use peeroxide_capture::{CaptureOptions, Next, list_sources, start};

fn main() -> anyhow::Result<()> {
    let sources = list_sources().context("listing sources")?;
    for (i, s) in sources.iter().enumerate() {
        println!("[{i}] {:?} {}", s.kind, s.name);
    }
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap_or_else(|| "0".into());
    let secs: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(3);
    let index = which
        .parse::<usize>()
        .ok()
        .or_else(|| sources.iter().position(|s| s.name.contains(&which)))
        .with_context(|| format!("no source matches {which}"))?;
    let source = sources
        .get(index)
        .with_context(|| format!("no source {index}"))?;
    println!("capturing [{index}] {} for {secs}s", source.name);

    let stream = start(source, CaptureOptions::default()).context("starting the capture")?;
    let started = Instant::now();
    let mut frames = 0u32;
    let mut size = None;
    while started.elapsed() < Duration::from_secs(secs) {
        match stream.next(Duration::from_millis(200)) {
            Next::Frame(f) => {
                frames += 1;
                if size != Some((f.width, f.height)) {
                    size = Some((f.width, f.height));
                    println!("{} size {}x{}", clock(), f.width, f.height);
                }
            }
            Next::Timeout => {}
            Next::Closed(r) => {
                println!("{} closed: {r:?}", clock());
                break;
            }
        }
    }
    let fps = f64::from(frames) / started.elapsed().as_secs_f64();
    println!("frames={frames} fps={fps:.1}");
    Ok(())
}

/// Seconds within the minute, local wall clock (matches PowerShell's ss.fff).
fn clock() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("{:02}.{:03}", (ms / 1000) % 60, ms % 1000)
}

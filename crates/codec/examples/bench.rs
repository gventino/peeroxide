//! Measures capture → canvas → H.264 encode → decode on a real source. Run it with `--help` for
//! the options.

use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, ValueEnum};
use peeroxide_capture::{CaptureOptions, Next, Source, list_sources, start};
use peeroxide_codec::{
    Canvas, H264Decoder, H264Encoder, Preset, VideoDecoder, VideoEncoder, canvas_size,
};

/// Measures capture → canvas → H.264 encode → decode, on a real source or a synthetic one.
#[derive(Parser)]
struct Cli {
    /// What to encode: a source's number in the capture list, `test` for the test pattern, or
    /// `scroll` for a synthetic full-screen page scrolling by (the worst case).
    #[arg(default_value = "0", value_parser = parse_input)]
    input: Input,
    /// How long to run, in seconds.
    #[arg(default_value_t = 5)]
    seconds: u64,
    /// The quality preset.
    #[arg(value_enum, default_value_t = Quality::P1080)]
    quality: Quality,
}

#[derive(Clone, Copy)]
enum Input {
    Source(usize),
    Test,
    Scroll,
}

fn parse_input(s: &str) -> anyhow::Result<Input> {
    Ok(match s {
        "test" => Input::Test,
        "scroll" => Input::Scroll,
        n => Input::Source(
            n.parse()
                .context("expected a source number, test or scroll")?,
        ),
    })
}

#[derive(Clone, Copy, ValueEnum)]
enum Quality {
    /// 720p, 30 fps
    #[value(name = "720")]
    P720,
    /// 1080p, 30 fps
    #[value(name = "1080")]
    P1080,
    /// 720p, 20 fps, for links with limited upload
    Internet,
}

impl Quality {
    fn preset(self) -> Preset {
        match self {
            Self::P720 => Preset::P720,
            Self::P1080 => Preset::P1080,
            Self::Internet => Preset::INTERNET,
        }
    }
}

#[derive(Default)]
struct Stage(Duration, u32);

impl Stage {
    fn add(&mut self, d: Duration) {
        self.0 += d;
        self.1 += 1;
    }
    fn avg_ms(&self) -> f64 {
        self.0.as_secs_f64() * 1000.0 / f64::from(self.1.max(1))
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let (secs, preset) = (cli.seconds, cli.quality.preset());
    let source = match cli.input {
        Input::Scroll => return scroll_stress(secs, &preset),
        Input::Test => Source::test_pattern(),
        Input::Source(index) => list_sources()
            .context("listing sources")?
            .get(index)
            .with_context(|| format!("no source {index}"))?
            .clone(),
    };
    println!("source: {} | preset: {}", source.name, preset.name);

    let stream = start(&source, CaptureOptions::default()).context("starting the capture")?;
    let interval = Duration::from_secs_f64(1.0 / f64::from(preset.fps));
    let mut canvas: Option<Canvas> = None;
    let mut enc = H264Encoder::new(&preset)?;
    let mut dec = H264Decoder::new()?;
    let (mut t_canvas, mut t_enc, mut t_dec) =
        (Stage::default(), Stage::default(), Stage::default());
    let (mut bytes, mut keyframes) = (0usize, 0u32);
    let started = Instant::now();
    let mut next_at = Instant::now();

    while started.elapsed() < Duration::from_secs(secs) {
        let frame = match stream.next(Duration::from_millis(500)) {
            Next::Frame(f) => f,
            Next::Timeout => continue,
            Next::Closed(r) => {
                println!("closed: {r:?}");
                break;
            }
        };
        let c = canvas.get_or_insert_with(|| {
            let (w, h) = canvas_size(frame.width, frame.height, &preset);
            println!("canvas {w}x{h} (source {}x{})", frame.width, frame.height);
            Canvas::new(w, h)
        });

        let t = Instant::now();
        c.draw(&frame.data, frame.width, frame.height)?;
        t_canvas.add(t.elapsed());

        let t = Instant::now();
        let encoded = enc.encode(c.bgra(), c.width(), c.height())?;
        t_enc.add(t.elapsed());

        if let Some(e) = encoded {
            bytes += e.data.len();
            keyframes += u32::from(e.keyframe);
            let t = Instant::now();
            dec.decode(&e.data)?;
            t_dec.add(t.elapsed());
        }

        next_at += interval;
        match next_at.checked_duration_since(Instant::now()) {
            Some(d) => std::thread::sleep(d),
            None => next_at = Instant::now(),
        }
    }

    report(started, &t_canvas, &t_enc, &t_dec, bytes, keyframes);
    Ok(())
}

/// Worst case for screen sharing: a full-screen, high-detail page scrolling 6 px per frame.
fn scroll_stress(secs: u64, preset: &Preset) -> anyhow::Result<()> {
    let (w, h) = (preset.max_width, preset.max_height);
    let page_h = h * 4;
    let mut seed = 0x2545_f491_u32;
    let mut page = vec![255u8; (w * page_h * 4) as usize];
    for line in 0..page_h / 18 {
        let mut x = 20;
        while x + 12 < w - 20 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let glyph_w = 4 + seed % 9;
            for gy in 0..11 {
                for gx in 0..glyph_w {
                    if (seed >> ((gx + gy) % 31)) & 1 == 1 {
                        let i = (((line * 18 + gy + 3) * w + x + gx) * 4) as usize;
                        page[i..i + 3].copy_from_slice(&[20, 20, 20]);
                    }
                }
            }
            x += glyph_w + 2 + u32::from(seed.is_multiple_of(7)) * 8;
        }
    }
    println!("synthetic scrolling page {w}x{h}");
    let mut enc = H264Encoder::new(preset)?;
    let mut dec = H264Decoder::new()?;
    let (mut t_enc, mut t_dec) = (Stage::default(), Stage::default());
    let (mut bytes, mut keyframes) = (0usize, 0u32);
    let started = Instant::now();
    let interval = Duration::from_secs_f64(1.0 / f64::from(preset.fps));
    let mut next_at = Instant::now();
    let mut n = 0u32;
    while started.elapsed() < Duration::from_secs(secs) {
        let off = ((n * 6) % (page_h - h)) as usize * w as usize * 4;
        let frame = &page[off..off + (w * h * 4) as usize];
        let t = Instant::now();
        let e = enc.encode(frame, w, h)?;
        t_enc.add(t.elapsed());
        if let Some(e) = e {
            bytes += e.data.len();
            keyframes += u32::from(e.keyframe);
            let t = Instant::now();
            dec.decode(&e.data)?;
            t_dec.add(t.elapsed());
        }
        n += 1;
        next_at += interval;
        match next_at.checked_duration_since(Instant::now()) {
            Some(d) => std::thread::sleep(d),
            None => next_at = Instant::now(),
        }
    }
    report(started, &Stage::default(), &t_enc, &t_dec, bytes, keyframes);
    let delivered = t_dec.1;
    println!(
        "delivered {delivered} of {n} frames ({:.1} fps after rate control)",
        f64::from(delivered) / started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn report(
    started: Instant,
    t_canvas: &Stage,
    t_enc: &Stage,
    t_dec: &Stage,
    bytes: usize,
    keyframes: u32,
) {
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "frames={} fps={:.1} canvas={:.2}ms encode={:.2}ms decode={:.2}ms bitrate={:.0}kbps keyframes={}",
        t_enc.1,
        f64::from(t_enc.1) / elapsed,
        t_canvas.avg_ms(),
        t_enc.avg_ms(),
        t_dec.avg_ms(),
        bytes as f64 * 8.0 / elapsed / 1000.0,
        keyframes
    );
}

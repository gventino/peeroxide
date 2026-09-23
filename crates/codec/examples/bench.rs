//! Measures capture → canvas → H.264 encode → decode on a real source.
//! Usage: cargo run --release -p p2pss-codec --example bench [source-index|test] [seconds] [720|1080]

use std::time::{Duration, Instant};

use p2pss_capture::{CaptureOptions, Next, Source, list_sources, start};
use p2pss_codec::{
    Canvas, H264Decoder, H264Encoder, Preset, VideoDecoder, VideoEncoder, canvas_size,
};

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

fn main() {
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap_or_else(|| "0".into());
    let secs: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(5);
    let preset = match args.next().as_deref() {
        Some("720") => Preset::P720,
        Some("internet") => Preset::INTERNET,
        _ => Preset::P1080,
    };
    if which == "scroll" {
        return scroll_stress(secs, &preset);
    }
    let source = if which == "test" {
        Source::test_pattern()
    } else {
        list_sources().unwrap()[which.parse::<usize>().unwrap()].clone()
    };
    println!("source: {} | preset: {}", source.name, preset.name);

    let stream = start(&source, CaptureOptions::default()).unwrap();
    let interval = Duration::from_secs_f64(1.0 / f64::from(preset.fps));
    let mut canvas: Option<Canvas> = None;
    let mut enc = H264Encoder::new(&preset).unwrap();
    let mut dec = H264Decoder::new().unwrap();
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
        c.draw(&frame.data, frame.width, frame.height).unwrap();
        t_canvas.add(t.elapsed());

        let t = Instant::now();
        let encoded = enc.encode(c.bgra(), c.width(), c.height()).unwrap();
        t_enc.add(t.elapsed());

        if let Some(e) = encoded {
            bytes += e.data.len();
            keyframes += u32::from(e.keyframe);
            let t = Instant::now();
            dec.decode(&e.data).unwrap();
            t_dec.add(t.elapsed());
        }

        next_at += interval;
        match next_at.checked_duration_since(Instant::now()) {
            Some(d) => std::thread::sleep(d),
            None => next_at = Instant::now(),
        }
    }

    report(started, &t_canvas, &t_enc, &t_dec, bytes, keyframes);
}

/// Worst case for screen sharing: a full-screen, high-detail page scrolling 6 px per frame.
fn scroll_stress(secs: u64, preset: &Preset) {
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
    let mut enc = H264Encoder::new(preset).unwrap();
    let mut dec = H264Decoder::new().unwrap();
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
        let e = enc.encode(frame, w, h).unwrap();
        t_enc.add(t.elapsed());
        if let Some(e) = e {
            bytes += e.data.len();
            keyframes += u32::from(e.keyframe);
            let t = Instant::now();
            dec.decode(&e.data).unwrap();
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

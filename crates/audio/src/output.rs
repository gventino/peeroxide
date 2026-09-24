//! Plays 48 kHz stereo through the default output device, at the viewer's volume.
//!
//! The device callback only pops samples from a lock-free ring buffer and applies the gain: it
//! never locks or allocates. Whoever pushes (the viewer's audio thread) decides timing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::{CHANNELS, SAMPLE_RATE};

/// Per-sample smoothing of gain changes (about 5 ms), so moving the slider doesn't click.
const GAIN_SMOOTHING: f32 = 0.005;

/// Volume and mute, set by the UI and read by the device callback without locking.
pub struct OutputControl {
    volume: AtomicU32,
    muted: AtomicBool,
}

impl OutputControl {
    /// `volume` is 0.0–1.0.
    pub fn new(volume: f32, muted: bool) -> Arc<Self> {
        let c = Self {
            volume: AtomicU32::new(0),
            muted: AtomicBool::new(muted),
        };
        c.set_volume(volume);
        Arc::new(c)
    }

    pub fn set_volume(&self, volume: f32) {
        let v = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Amplitude factor. Squared, so the slider feels even across its range.
    fn gain(&self) -> f32 {
        if self.muted() {
            0.0
        } else {
            self.volume().powi(2)
        }
    }
}

#[derive(Default)]
struct Shared {
    latency_us: AtomicU64,
    underruns: AtomicU64,
    failed: AtomicBool,
}

/// An open output device. Dropping it stops playback.
pub struct AudioOutput {
    _stream: cpal::Stream,
    producer: Producer<f32>,
    rate: u32,
    resampler: Option<Linear>,
    shared: Arc<Shared>,
    scratch: Vec<f32>,
}

impl AudioOutput {
    /// Opens the default output device, at 48 kHz if it takes it (no resampling), otherwise at
    /// its own rate.
    pub fn open(control: Arc<OutputControl>) -> anyhow::Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("no audio output device")?;
        let default = device.default_output_config().context("audio output")?;
        let mut rates = vec![SAMPLE_RATE];
        if default.sample_rate() != SAMPLE_RATE {
            rates.push(default.sample_rate());
        }
        let mut last_error = anyhow!("no output configuration to try");
        for rate in rates {
            let config = StreamConfig {
                channels: default.channels(),
                sample_rate: rate,
                buffer_size: cpal::BufferSize::Default,
            };
            let (producer, consumer) = RingBuffer::new(rate as usize * CHANNELS);
            let shared = Arc::new(Shared::default());
            let built = match default.sample_format() {
                SampleFormat::F32 => build::<f32>(&device, config, consumer, &control, &shared),
                SampleFormat::I16 => build::<i16>(&device, config, consumer, &control, &shared),
                SampleFormat::I32 => build::<i32>(&device, config, consumer, &control, &shared),
                SampleFormat::U16 => build::<u16>(&device, config, consumer, &control, &shared),
                other => bail!("unsupported output sample format {other}"),
            };
            match built.and_then(|s| s.play().map(|()| s)) {
                Ok(stream) => {
                    tracing::info!(
                        rate,
                        channels = default.channels(),
                        format = %default.sample_format(),
                        "audio output open"
                    );
                    return Ok(Self {
                        _stream: stream,
                        producer,
                        rate,
                        resampler: (rate != SAMPLE_RATE).then(|| Linear::new(rate)),
                        shared,
                        scratch: Vec::new(),
                    });
                }
                Err(e) => last_error = e.into(),
            }
        }
        Err(last_error.context("audio output"))
    }

    /// How long until something queued now starts playing (queued samples plus the device's own
    /// buffering).
    pub fn queued(&self) -> Duration {
        let samples = self.producer.buffer().capacity() - self.producer.slots();
        let frames = (samples / CHANNELS) as f64;
        Duration::from_secs_f64(frames / f64::from(self.rate))
            + Duration::from_micros(self.shared.latency_us.load(Ordering::Relaxed))
    }

    /// Queues interleaved 48 kHz stereo samples. Whatever doesn't fit is dropped.
    pub fn push(&mut self, samples: &[f32]) {
        let out = match &mut self.resampler {
            Some(r) => {
                self.scratch.clear();
                r.process(samples, &mut self.scratch);
                &self.scratch[..]
            }
            None => samples,
        };
        for s in out {
            if self.producer.push(*s).is_err() {
                break;
            }
        }
    }

    pub fn push_silence(&mut self, frames: usize) {
        self.push(&vec![0.0; frames * CHANNELS]);
    }

    /// Times playback ran dry in the middle of audio.
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// The device reported an error (e.g. it was unplugged); reopen to recover.
    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut consumer: Consumer<f32>,
    control: &Arc<OutputControl>,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let control = control.clone();
    let (data_shared, error_shared) = (shared.clone(), shared.clone());
    let mut gain = control.gain();
    let mut playing = false;
    device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            let ts = info.timestamp();
            if let Some(latency) = ts.playback.checked_duration_since(ts.callback) {
                data_shared
                    .latency_us
                    .store(latency.as_micros() as u64, Ordering::Relaxed);
            }
            let target = control.gain();
            let mut ran_dry = false;
            for frame in data.chunks_mut(channels) {
                let (l, r) = if consumer.slots() >= CHANNELS {
                    (consumer.pop().unwrap_or(0.0), consumer.pop().unwrap_or(0.0))
                } else {
                    ran_dry = true;
                    (0.0, 0.0)
                };
                gain += (target - gain) * GAIN_SMOOTHING;
                let (l, r) = (l * gain, r * gain);
                match frame {
                    [mono] => *mono = T::from_sample((l + r) * 0.5),
                    [left, right, rest @ ..] => {
                        *left = T::from_sample(l);
                        *right = T::from_sample(r);
                        for s in rest {
                            *s = T::EQUILIBRIUM;
                        }
                    }
                    [] => {}
                }
            }
            // Running dry after playing is an underrun; staying dry is just silence.
            if ran_dry && playing {
                data_shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
            playing = !ran_dry;
        },
        move |e| {
            tracing::warn!("audio output error: {e}");
            error_shared.failed.store(true, Ordering::Relaxed);
        },
        None,
    )
}

/// Linear-interpolation resampler from 48 kHz, only used for devices that can't play 48 kHz.
struct Linear {
    /// Input frames per output frame.
    step: f64,
    /// Position of the next output frame; -1.0 is the last frame of the previous input.
    pos: f64,
    prev: [f32; CHANNELS],
}

impl Linear {
    fn new(out_rate: u32) -> Self {
        Self {
            step: f64::from(SAMPLE_RATE) / f64::from(out_rate),
            pos: 0.0,
            prev: [0.0; CHANNELS],
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = input.len() / CHANNELS;
        if n == 0 {
            return;
        }
        let frame = |i: isize| -> [f32; CHANNELS] {
            if i < 0 {
                self.prev
            } else {
                let i = i as usize * CHANNELS;
                [input[i], input[i + 1]]
            }
        };
        while self.pos <= (n - 1) as f64 {
            let i = self.pos.floor() as isize;
            let t = (self.pos - i as f64) as f32;
            let (a, b) = (frame(i), frame(i + 1));
            for c in 0..CHANNELS {
                out.push(a[c] + (b[c] - a[c]) * t);
            }
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = frame(n as isize - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_is_clamped_and_mute_silences() {
        let c = OutputControl::new(2.0, false);
        assert_eq!(c.volume(), 1.0);
        c.set_volume(f32::NAN);
        assert_eq!(c.volume(), 1.0);
        c.set_volume(0.5);
        assert_eq!(c.gain(), 0.25);
        c.set_muted(true);
        assert_eq!(c.gain(), 0.0);
        assert_eq!(c.volume(), 0.5, "mute keeps the volume");
    }

    #[test]
    fn resampler_keeps_duration_and_shape() {
        let mut r = Linear::new(44_100);
        let mut out = Vec::new();
        // One second of a slow ramp, in 20 ms chunks.
        let input: Vec<f32> = (0..48_000)
            .flat_map(|i| [i as f32 / 48_000.0, -(i as f32) / 48_000.0])
            .collect();
        for chunk in input.chunks(960 * CHANNELS) {
            r.process(chunk, &mut out);
        }
        let frames = out.len() / CHANNELS;
        assert!((44_099..=44_101).contains(&frames), "{frames} frames");
        for (i, f) in out.chunks_exact(CHANNELS).enumerate().skip(1) {
            let expected = i as f32 * 48_000.0 / 44_100.0 / 48_000.0;
            assert!(
                (f[0] - expected).abs() < 1e-4,
                "frame {i}: {} vs {expected}",
                f[0]
            );
            assert_eq!(f[1], -f[0]);
        }
    }
}

//! Opus audio: 48 kHz interleaved stereo f32, one 20 ms frame per packet.

use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Context, bail, ensure};
use opus_rs::Application;

use crate::{AudioDecoder, AudioEncoder};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// Samples per channel in one 20 ms frame.
pub const FRAME_SAMPLES: usize = 960;
/// Interleaved samples in one frame.
pub const FRAME_LEN: usize = FRAME_SAMPLES * CHANNELS;
/// Largest single-frame Opus packet (RFC 6716: 1275-byte frame plus the ToC byte).
pub const MAX_PACKET: usize = 1276;

pub struct OpusEncoder {
    inner: opus_rs::OpusEncoder,
    out: Vec<u8>,
}

impl OpusEncoder {
    pub fn new(bitrate_bps: u32) -> anyhow::Result<Self> {
        // opus-rs reports errors as plain `&str`s.
        let mut inner = opus_rs::OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS, Application::Audio)
            .map_err(anyhow::Error::msg)
            .context("could not create the Opus encoder")?;
        inner.bitrate_bps = i32::try_from(bitrate_bps).unwrap_or(i32::MAX);
        Ok(Self {
            inner,
            out: vec![0; MAX_PACKET],
        })
    }
}

impl AudioEncoder for OpusEncoder {
    fn encode(&mut self, pcm: &[f32]) -> anyhow::Result<Vec<u8>> {
        ensure!(
            pcm.len() == FRAME_LEN,
            "invalid frame: expected {FRAME_LEN} samples, got {}",
            pcm.len()
        );
        let n = self
            .inner
            .encode(pcm, FRAME_SAMPLES, &mut self.out)
            .map_err(anyhow::Error::msg)
            .context("Opus encoding")?;
        Ok(self.out[..n].to_vec())
    }
}

pub struct OpusDecoder {
    inner: opus_rs::OpusDecoder,
}

impl OpusDecoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            inner: opus_rs::OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS)
                .map_err(anyhow::Error::msg)
                .context("could not create the Opus decoder")?,
        })
    }
}

impl AudioDecoder for OpusDecoder {
    fn decode(&mut self, packet: &[u8]) -> anyhow::Result<Vec<f32>> {
        ensure!(
            !packet.is_empty() && packet.len() <= MAX_PACKET,
            "invalid packet of {} bytes",
            packet.len()
        );
        let mut pcm = vec![0.0; FRAME_LEN];
        // The packet comes from the network: a decoder bug must cost one packet, not the thread.
        let decoded = catch_unwind(AssertUnwindSafe(|| {
            self.inner.decode(packet, FRAME_SAMPLES, &mut pcm)
        }));
        match decoded {
            Ok(Ok(n)) if n == FRAME_SAMPLES => Ok(pcm),
            Ok(Ok(n)) => bail!("invalid packet: holds {n} samples, expected {FRAME_SAMPLES}"),
            Ok(Err(e)) => Err(anyhow::Error::msg(e).context("Opus decoding")),
            Err(_) => {
                // Its state is unknown after a panic; start over.
                *self = Self::new()?;
                bail!("the Opus decoder panicked")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;
    use std::time::{Duration, Instant};

    use super::*;

    /// Interleaved stereo: `left` and `right` are lists of (frequency, amplitude).
    fn tones(left: &[(f32, f32)], right: &[(f32, f32)], frames: usize) -> Vec<f32> {
        let n = frames * FRAME_SAMPLES;
        let mut pcm = Vec::with_capacity(n * CHANNELS);
        for i in 0..n {
            let t = i as f32 / SAMPLE_RATE as f32;
            let sum = |parts: &[(f32, f32)]| {
                parts
                    .iter()
                    .map(|(f, a)| a * (TAU * f * t).sin())
                    .sum::<f32>()
            };
            pcm.push(sum(left));
            pcm.push(sum(right));
        }
        pcm
    }

    fn roundtrip(pcm: &[f32], bitrate: u32) -> Vec<f32> {
        let mut enc = OpusEncoder::new(bitrate).unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        let mut out = Vec::with_capacity(pcm.len());
        for frame in pcm.chunks_exact(FRAME_LEN) {
            let packet = enc.encode(frame).unwrap();
            assert!(!packet.is_empty() && packet.len() <= MAX_PACKET);
            out.extend(dec.decode(&packet).unwrap());
        }
        out
    }

    /// Power of `freq` in one channel (Goertzel), normalized so a full-scale sine gives 0.25.
    fn power(pcm: &[f32], channel: usize, freq: f32) -> f32 {
        let samples: Vec<f32> = pcm
            .iter()
            .skip(channel)
            .step_by(CHANNELS)
            .copied()
            .collect();
        let w = TAU * freq / SAMPLE_RATE as f32;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for x in &samples {
            let s0 = x + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let n = samples.len() as f32;
        (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (n * n)
    }

    fn rms(pcm: &[f32]) -> f32 {
        (pcm.iter().map(|x| x * x).sum::<f32>() / pcm.len() as f32).sqrt()
    }

    #[test]
    fn stereo_tones_survive_at_every_preset_bitrate() {
        let left = [(440.0, 0.3), (1000.0, 0.2)];
        let right = [(660.0, 0.3), (3000.0, 0.2)];
        let pcm = tones(&left, &right, 100);
        for bitrate in [64_000, 128_000] {
            // Skip the first half second: codec delay and encoder warm-up.
            let out = roundtrip(&pcm, bitrate);
            let (a, b) = (&pcm[48_000 * CHANNELS..], &out[48_000 * CHANNELS..]);
            let level_db = 20.0 * (rms(b) / rms(a)).log10();
            assert!(
                level_db.abs() < 1.5,
                "{bitrate}: level off by {level_db:.1} dB"
            );
            for (ch, own, other) in [(0, &left, &right), (1, &right, &left)] {
                for (f, amp) in own.iter() {
                    let want = amp * amp / 4.0;
                    let got = power(b, ch, *f);
                    assert!(
                        (got / want - 1.0).abs() < 0.25,
                        "{bitrate}: channel {ch} {f} Hz power {got} vs {want}"
                    );
                }
                for (f, amp) in other.iter() {
                    let leak = power(b, ch, *f) / (amp * amp / 4.0);
                    assert!(
                        leak < 0.01,
                        "{bitrate}: {f} Hz leaks into channel {ch} ({leak})"
                    );
                }
            }
        }
    }

    #[test]
    fn silence_and_quiet_passages_stay_stereo_and_decodable() {
        // A stereo decoder rejects mono packets, so the encoder must never switch to mono.
        let mut pcm = vec![0.0; 25 * FRAME_LEN];
        pcm.extend(tones(&[(200.0, 0.001)], &[(300.0, 0.001)], 25));
        pcm.extend(tones(&[(440.0, 0.5)], &[(440.0, 0.5)], 25));
        for bitrate in [64_000, 128_000] {
            let out = roundtrip(&pcm, bitrate);
            assert_eq!(out.len(), pcm.len());
            assert!(rms(&out[..25 * FRAME_LEN]) < 1e-3);
        }
    }

    #[test]
    fn rejects_wrong_frame_sizes() {
        let mut enc = OpusEncoder::new(128_000).unwrap();
        assert!(enc.encode(&[0.0; FRAME_LEN - 2]).is_err());
        assert!(enc.encode(&[0.0; FRAME_LEN * 2]).is_err());
    }

    #[test]
    fn garbage_packets_are_errors_or_noise_never_panics() {
        let mut dec = OpusDecoder::new().unwrap();
        assert!(dec.decode(&[]).is_err());
        assert!(dec.decode(&vec![0xAB; MAX_PACKET + 1]).is_err());
        // Deterministic pseudo-random packets of every size class.
        let mut x: u32 = 0x1234_5678;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        for i in 0..5_000 {
            let len = 1 + (next() as usize % MAX_PACKET);
            let mut packet: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if i % 2 == 0 {
                // Stereo 20 ms CELT ToC, so the payload reaches the entropy decoder.
                packet[0] = 0b1111_1100;
            }
            if let Ok(pcm) = dec.decode(&packet) {
                assert_eq!(pcm.len(), FRAME_LEN);
                assert!(pcm.iter().all(|s| s.is_finite()));
            }
        }
        // Still usable afterwards.
        let mut enc = OpusEncoder::new(128_000).unwrap();
        let packet = enc
            .encode(&tones(&[(440.0, 0.3)], &[(440.0, 0.3)], 1))
            .unwrap();
        assert_eq!(dec.decode(&packet).unwrap().len(), FRAME_LEN);
    }

    #[test]
    fn a_toc_only_packet_conceals_a_lost_frame() {
        let pcm = tones(&[(440.0, 0.3)], &[(440.0, 0.3)], 10);
        let mut enc = OpusEncoder::new(128_000).unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        let mut toc = 0;
        for frame in pcm.chunks_exact(FRAME_LEN) {
            let packet = enc.encode(frame).unwrap();
            toc = packet[0];
            dec.decode(&packet).unwrap();
        }
        assert_eq!(dec.decode(&[toc]).unwrap().len(), FRAME_LEN);
    }

    #[test]
    #[ignore = "timing; run with --ignored --nocapture"]
    fn speed() {
        let pcm = tones(&[(440.0, 0.3), (5000.0, 0.1)], &[(660.0, 0.3)], 500);
        let mut enc = OpusEncoder::new(128_000).unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        let (mut te, mut td, mut bytes) = (Duration::ZERO, Duration::ZERO, 0);
        for frame in pcm.chunks_exact(FRAME_LEN) {
            let t = Instant::now();
            let packet = enc.encode(frame).unwrap();
            te += t.elapsed();
            bytes += packet.len();
            let t = Instant::now();
            dec.decode(&packet).unwrap();
            td += t.elapsed();
        }
        println!(
            "10 s of audio: encode {te:?}, decode {td:?}, {:.0} kbps",
            bytes as f64 * 8.0 / 10.0 / 1000.0
        );
    }
}

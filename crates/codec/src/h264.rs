use std::time::Instant;

use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod,
    RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};

use crate::{CodecError, DecodedFrame, EncodedFrame, Preset, VideoDecoder, VideoEncoder};

fn codec_err(e: impl std::fmt::Display) -> CodecError {
    CodecError::Codec(e.to_string())
}

pub struct H264Encoder {
    inner: Encoder,
    yuv: Option<YUVBuffer>,
    started: Instant,
    initialized: bool,
    want_keyframe: bool,
}

impl H264Encoder {
    pub fn new(preset: &Preset) -> Result<Self, CodecError> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::CameraVideoRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(preset.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(preset.fps as f32))
            .skip_frames(false)
            .scene_change_detect(false)
            .complexity(Complexity::Low)
            .intra_frame_period(IntraFramePeriod::from_num_frames(preset.fps * 10));
        let inner =
            Encoder::with_api_config(OpenH264API::from_source(), config).map_err(codec_err)?;
        Ok(Self {
            inner,
            yuv: None,
            started: Instant::now(),
            initialized: false,
            want_keyframe: false,
        })
    }
}

impl VideoEncoder for H264Encoder {
    fn encode(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Option<EncodedFrame>, CodecError> {
        let (w, h) = (width as usize, height as usize);
        if w % 2 != 0 || h % 2 != 0 || bgra.len() < w * h * 4 {
            return Err(CodecError::InvalidInput(format!(
                "{width}x{height} with {} bytes",
                bgra.len()
            )));
        }
        let yuv = match &mut self.yuv {
            Some(buf) if buf.dimensions() == (w, h) => buf,
            slot => slot.insert(YUVBuffer::new(w, h)),
        };
        yuv.read_bgra8(BgraSliceU8::new(&bgra[..w * h * 4], (w, h)));

        // The encoder initializes lazily on the first frame, which is always an IDR anyway.
        if self.want_keyframe && self.initialized {
            self.inner.force_intra_frame();
        }
        self.want_keyframe = false;

        let ts = openh264::Timestamp::from_millis(self.started.elapsed().as_millis() as u64);
        let bitstream = self.inner.encode_at(&*yuv, ts).map_err(codec_err)?;
        self.initialized = true;

        let keyframe = match bitstream.frame_type() {
            FrameType::IDR => true,
            FrameType::I | FrameType::P | FrameType::IPMixed => false,
            FrameType::Skip | FrameType::Invalid => return Ok(None),
        };
        let data = bitstream.to_vec();
        if data.is_empty() {
            return Ok(None);
        }
        Ok(Some(EncodedFrame { data, keyframe }))
    }

    fn request_keyframe(&mut self) {
        self.want_keyframe = true;
    }
}

pub struct H264Decoder {
    inner: Decoder,
}

impl H264Decoder {
    pub fn new() -> Result<Self, CodecError> {
        let inner = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new())
            .map_err(codec_err)?;
        Ok(Self { inner })
    }
}

impl VideoDecoder for H264Decoder {
    fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>, CodecError> {
        let Some(yuv) = self.inner.decode(data).map_err(codec_err)? else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        let mut rgba = vec![0u8; w * h * 4];
        yuv.write_rgba8(&mut rgba);
        Ok(Some(DecodedFrame {
            width: w as u32,
            height: h as u32,
            rgba,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 320;
    const H: u32 = 180;

    fn picture(n: u32) -> Vec<u8> {
        let mut data = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                let in_box = (x + W - (n * 8) % W) % W < 40 && y > 60 && y < 120;
                let v = if in_box { 250 } else { (x * 255 / W) as u8 };
                data[i..i + 4].copy_from_slice(&[v, (y * 255 / H) as u8, 128, 255]);
            }
        }
        data
    }

    fn psnr(bgra: &[u8], rgba: &[u8]) -> f64 {
        let mut se = 0.0;
        let mut n = 0.0;
        for (s, d) in bgra.chunks_exact(4).zip(rgba.chunks_exact(4)) {
            for (a, b) in [(s[2], d[0]), (s[1], d[1]), (s[0], d[2])] {
                se += (f64::from(a) - f64::from(b)).powi(2);
                n += 1.0;
            }
        }
        10.0 * (255.0f64.powi(2) / (se / n)).log10()
    }

    fn encoder() -> H264Encoder {
        H264Encoder::new(&Preset {
            name: "test",
            max_width: W,
            max_height: H,
            fps: 30,
            bitrate_bps: 2_000_000,
        })
        .unwrap()
    }

    #[test]
    fn roundtrip_preserves_size_and_quality() {
        let mut enc = encoder();
        let mut dec = H264Decoder::new().unwrap();
        let mut last = None;
        for n in 0..15 {
            let src = picture(n);
            let frame = enc.encode(&src, W, H).unwrap().expect("frame");
            if let Some(out) = dec.decode(&frame.data).unwrap() {
                last = Some((src, out));
            }
        }
        let (src, out) = last.expect("decoded at least one frame");
        assert_eq!((out.width, out.height), (W, H));
        let q = psnr(&src, &out.rgba);
        assert!(q > 30.0, "psnr {q:.1} dB");
    }

    #[test]
    fn first_frame_and_requested_frames_are_keyframes() {
        let mut enc = encoder();
        let kinds: Vec<bool> = (0..6)
            .map(|n| {
                if n == 4 {
                    enc.request_keyframe();
                }
                enc.encode(&picture(n), W, H).unwrap().unwrap().keyframe
            })
            .collect();
        assert_eq!(kinds, [true, false, false, false, true, false]);
    }

    #[test]
    fn decoder_can_join_at_a_keyframe() {
        let mut enc = encoder();
        let frames: Vec<EncodedFrame> = (0..8)
            .map(|n| {
                if n == 5 {
                    enc.request_keyframe();
                }
                enc.encode(&picture(n), W, H).unwrap().unwrap()
            })
            .collect();
        let mut dec = H264Decoder::new().unwrap();
        let decoded = frames[5..]
            .iter()
            .filter_map(|f| dec.decode(&f.data).unwrap())
            .count();
        assert_eq!(decoded, 3);
    }

    #[test]
    fn rejects_odd_dimensions() {
        let mut enc = encoder();
        assert!(enc.encode(&[0u8; 4 * 3 * 2], 3, 2).is_err());
    }
}

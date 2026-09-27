use std::time::Instant;

use anyhow::{Context, ensure};
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod,
    RateControlMode, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};

use crate::{Codec, DecodedFrame, EncodedFrame, Preset, VideoDecoder, VideoEncoder};

pub struct H264Encoder {
    inner: Encoder,
    yuv: Option<YUVBuffer>,
    started: Instant,
    initialized: bool,
    want_keyframe: bool,
}

impl H264Encoder {
    pub fn new(preset: &Preset) -> anyhow::Result<Self> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::CameraVideoRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(preset.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(preset.fps as f32))
            .skip_frames(false)
            .scene_change_detect(false)
            .complexity(Complexity::Low)
            .intra_frame_period(IntraFramePeriod::from_num_frames(preset.fps * 10));
        let inner = Encoder::with_api_config(OpenH264API::from_source(), config)
            .context("could not create the H.264 encoder")?;
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
    ) -> anyhow::Result<Option<EncodedFrame>> {
        let now_ms = self.started.elapsed().as_millis() as u64;
        self.encode_at(bgra, width, height, now_ms)
    }

    fn request_keyframe(&mut self) {
        self.want_keyframe = true;
    }

    fn codec(&self) -> Codec {
        Codec::H264
    }

    fn describe(&self) -> String {
        "H.264 · software".into()
    }
}

impl H264Encoder {
    /// Like [`VideoEncoder::encode`] with an explicit timestamp, which rate control uses.
    fn encode_at(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        timestamp_ms: u64,
    ) -> anyhow::Result<Option<EncodedFrame>> {
        let (w, h) = (width as usize, height as usize);
        ensure!(
            w % 2 == 0 && h % 2 == 0 && bgra.len() >= w * h * 4,
            "invalid frame: {width}x{height} with {} bytes",
            bgra.len()
        );
        let yuv = match &mut self.yuv {
            Some(buf) if buf.dimensions() == (w, h) => buf,
            slot => slot.insert(YUVBuffer::new(w, h)),
        };
        yuv.read_bgra8(BgraSliceU8::new(&bgra[..w * h * 4], (w, h)));

        // The encoder initializes lazily on the first frame, which is always an IDR anyway.
        // The request stays pending until an IDR actually comes out.
        if self.want_keyframe && self.initialized {
            self.inner.force_intra_frame();
        }

        let ts = openh264::Timestamp::from_millis(timestamp_ms);
        let bitstream = self.inner.encode_at(&*yuv, ts).context("H.264 encoding")?;
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
        if keyframe {
            self.want_keyframe = false;
        }
        Ok(Some(EncodedFrame { data, keyframe }))
    }
}

pub struct H264Decoder {
    inner: Decoder,
}

impl H264Decoder {
    pub fn new() -> anyhow::Result<Self> {
        let inner = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new())
            .context("could not create the H.264 decoder")?;
        Ok(Self { inner })
    }
}

impl VideoDecoder for H264Decoder {
    fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<DecodedFrame>> {
        let Some(yuv) = self.inner.decode(data).context("H.264 decoding")? else {
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
    use crate::test_util::{H, PAGE_H, PAGE_W, W, picture, psnr, scrolled, text_page};

    fn encoder() -> H264Encoder {
        H264Encoder::new(&Preset {
            id: "test",
            name: "test",
            max_width: W,
            max_height: H,
            fps: 30,
            bitrate_bps: 2_000_000,
            audio_bitrate_bps: 64_000,
        })
        .unwrap()
    }

    /// Encodes `frames` frames at the preset's frame rate; returns (bits per second, frame kinds).
    fn encode_scroll(
        preset: &Preset,
        frames: u32,
        keyframe_at: Option<u32>,
    ) -> (f64, Vec<Option<bool>>) {
        let page = text_page();
        let mut enc = H264Encoder::new(preset).unwrap();
        let interval_ms = 1000 / u64::from(preset.fps);
        let (mut bytes, mut kinds) = (0usize, Vec::new());
        for n in 0..frames {
            if keyframe_at == Some(n) {
                enc.request_keyframe();
            }
            let out = enc
                .encode_at(
                    scrolled(&page, n),
                    PAGE_W,
                    PAGE_H,
                    u64::from(n) * interval_ms,
                )
                .unwrap();
            bytes += out.as_ref().map_or(0, |f| f.data.len());
            kinds.push(out.map(|f| f.keyframe));
        }
        let seconds = f64::from(frames) / f64::from(preset.fps);
        (bytes as f64 * 8.0 / seconds, kinds)
    }

    /// Ten seconds of scrolling text: the budget holds and every frame is still delivered.
    #[test]
    fn internet_budget_holds_on_scrolling_text() {
        let preset = Preset::INTERNET;
        let (bps, kinds) = encode_scroll(&preset, 200, None);
        let budget = f64::from(preset.bitrate_bps);
        assert!(bps < budget * 1.5, "{bps:.0} bps vs budget {budget:.0}");
        assert!(kinds.iter().all(Option::is_some), "no frame may be dropped");
    }

    #[test]
    fn keyframe_request_mid_stream_is_honored_immediately() {
        let (_, kinds) = encode_scroll(&Preset::INTERNET, 30, Some(20));
        assert_eq!(kinds[20], Some(true), "{kinds:?}");
        assert!(kinds[21..].iter().all(|k| *k == Some(false)));
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

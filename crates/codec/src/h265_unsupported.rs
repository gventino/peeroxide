//! No hardware H.265 encoder is wired up on this platform yet (VideoToolbox and VAAPI are planned
//! for 0.8), so broadcasts use H.264. Decoding H.265 works everywhere.

use crate::{Codec, EncodedFrame, Preset, VideoEncoder};

pub enum H265Encoder {}

impl H265Encoder {
    pub fn new(_preset: &Preset) -> anyhow::Result<Self> {
        anyhow::bail!("no hardware H.265 encoder on this platform yet")
    }
}

pub fn hardware_h265_encoder() -> Option<&'static str> {
    None
}

impl VideoEncoder for H265Encoder {
    fn encode(&mut self, _: &[u8], _: u32, _: u32) -> anyhow::Result<Option<EncodedFrame>> {
        match *self {}
    }

    fn request_keyframe(&mut self) {
        match *self {}
    }

    fn codec(&self) -> Codec {
        Codec::H265
    }

    fn describe(&self) -> String {
        match *self {}
    }
}

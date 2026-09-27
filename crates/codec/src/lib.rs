//! Media pipeline pieces: fixed-size canvas (scale + letterbox), video encode/decode (H.265 on
//! the GPU where there is a hardware encoder, H.264 in software otherwise; both decode in
//! software everywhere), and Opus audio encode/decode.

mod canvas;
mod h264;
mod h265;
#[cfg(windows)]
mod h265_mf;
#[cfg(not(windows))]
mod h265_unsupported;
pub mod opus;
#[cfg(test)]
mod test_util;

pub use canvas::{Canvas, FitRect, canvas_size, fit_rect};
pub use h264::{H264Decoder, H264Encoder};
pub use h265::H265Decoder;
#[cfg(windows)]
pub use h265_mf::{H265Encoder, hardware_h265_encoder};
#[cfg(not(windows))]
pub use h265_unsupported::{H265Encoder, hardware_h265_encoder};
pub use opus::{OpusDecoder, OpusEncoder};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    H265,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265",
        }
    }
}

/// An encoder for `preferred`. H.265 needs a hardware encoder; without a working one this
/// falls back to H.264 in software, so a broadcast always starts.
pub fn new_encoder(preset: &Preset, preferred: Codec) -> anyhow::Result<Box<dyn VideoEncoder>> {
    if preferred == Codec::H265 {
        match H265Encoder::new(preset) {
            Ok(encoder) => return Ok(Box::new(encoder)),
            Err(e) => tracing::info!("using H.264: {e:#}"),
        }
    }
    Ok(Box::new(H264Encoder::new(preset)?))
}

pub fn new_decoder(codec: Codec) -> anyhow::Result<Box<dyn VideoDecoder>> {
    Ok(match codec {
        Codec::H264 => Box::new(H264Decoder::new()?),
        Codec::H265 => Box::new(H265Decoder::new()?),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    /// Stable identifier, saved in the settings; never change an existing one.
    pub id: &'static str,
    pub name: &'static str,
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    /// Opus bitrate when the broadcast shares audio.
    pub audio_bitrate_bps: u32,
}

impl Preset {
    pub const P720: Self = Self {
        id: "720p30",
        name: "720p · 30 fps",
        max_width: 1280,
        max_height: 720,
        fps: 30,
        bitrate_bps: 4_000_000,
        audio_bitrate_bps: 128_000,
    };
    pub const P720_60: Self = Self {
        id: "720p60",
        name: "720p · 60 fps",
        max_width: 1280,
        max_height: 720,
        fps: 60,
        bitrate_bps: 6_000_000,
        audio_bitrate_bps: 128_000,
    };
    pub const P1080: Self = Self {
        id: "1080p30",
        name: "1080p · 30 fps",
        max_width: 1920,
        max_height: 1080,
        fps: 30,
        bitrate_bps: 8_000_000,
        audio_bitrate_bps: 128_000,
    };
    pub const P1080_60: Self = Self {
        id: "1080p60",
        name: "1080p · 60 fps",
        max_width: 1920,
        max_height: 1080,
        fps: 60,
        bitrate_bps: 12_000_000,
        audio_bitrate_bps: 128_000,
    };
    /// For links with limited upload, such as a virtual LAN over the internet. 24 fps is the
    /// frame rate of films, series and anime. Rate control raises the quantizer to stay near
    /// the budget; frames are never dropped, because a frame-skipping encoder makes each
    /// surviving frame bigger and spirals down to ~1 fps.
    pub const INTERNET: Self = Self {
        id: "internet",
        name: "Internet / VPN · 720p · 24 fps",
        max_width: 1280,
        max_height: 720,
        fps: 24,
        bitrate_bps: 2_000_000,
        audio_bitrate_bps: 64_000,
    };
    pub const ALL: [Self; 5] = [
        Self::P720,
        Self::P720_60,
        Self::P1080,
        Self::P1080_60,
        Self::INTERNET,
    ];

    /// Finds a preset by its id, or by the display name older versions saved.
    pub fn find(key: &str) -> Option<Self> {
        let legacy = match key {
            "Internet / VPN · 720p · 20 fps" => Some(Self::INTERNET),
            _ => None,
        };
        Self::ALL
            .into_iter()
            .find(|p| p.id == key || p.name == key)
            .or(legacy)
    }
}

impl Default for Preset {
    fn default() -> Self {
        Self::P1080
    }
}

/// One encoded access unit in Annex-B format.
#[derive(Clone, Debug)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// Decoded picture, RGBA8 tightly packed.
#[derive(Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Implemented by the software H.264 encoder and the hardware H.265 one; more hardware encoders
/// (VideoToolbox, VAAPI, ...) can be added behind it.
pub trait VideoEncoder: Send {
    /// Encodes one BGRA picture. Returns `None` if the encoder skipped the frame.
    fn encode(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
    ) -> anyhow::Result<Option<EncodedFrame>>;

    /// The next encoded frame will be an IDR keyframe.
    fn request_keyframe(&mut self);

    fn codec(&self) -> Codec;

    /// For logs and stats, e.g. "H.265 · AMDh265Encoder".
    fn describe(&self) -> String;
}

pub trait VideoDecoder: Send {
    /// Decodes one Annex-B access unit. Returns `None` if no picture is ready yet.
    fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<DecodedFrame>>;
}

/// Encodes one 20 ms frame of 48 kHz interleaved stereo audio ([`opus::FRAME_LEN`] samples).
pub trait AudioEncoder: Send {
    fn encode(&mut self, pcm: &[f32]) -> anyhow::Result<Vec<u8>>;
}

/// Decodes one packet into a 20 ms frame of 48 kHz interleaved stereo audio.
pub trait AudioDecoder: Send {
    fn decode(&mut self, packet: &[u8]) -> anyhow::Result<Vec<f32>>;
}

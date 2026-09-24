//! Media pipeline pieces: fixed-size canvas (scale + letterbox), H.264 encode/decode, and Opus
//! audio encode/decode.

mod canvas;
mod h264;
pub mod opus;

pub use canvas::{Canvas, FitRect, canvas_size, fit_rect};
pub use h264::{H264Decoder, H264Encoder};
pub use opus::{OpusDecoder, OpusEncoder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
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
        name: "720p · 30 fps",
        max_width: 1280,
        max_height: 720,
        fps: 30,
        bitrate_bps: 4_000_000,
        audio_bitrate_bps: 128_000,
    };
    pub const P1080: Self = Self {
        name: "1080p · 30 fps",
        max_width: 1920,
        max_height: 1080,
        fps: 30,
        bitrate_bps: 8_000_000,
        audio_bitrate_bps: 128_000,
    };
    /// For links with limited upload, such as a virtual LAN over the internet. Rate control
    /// raises the quantizer to stay near the budget; frames are never dropped, because a
    /// frame-skipping encoder makes each surviving frame bigger and spirals down to ~1 fps.
    pub const INTERNET: Self = Self {
        name: "Internet / VPN · 720p · 20 fps",
        max_width: 1280,
        max_height: 720,
        fps: 20,
        bitrate_bps: 2_000_000,
        audio_bitrate_bps: 64_000,
    };
    pub const ALL: [Self; 3] = [Self::P720, Self::P1080, Self::INTERNET];
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

/// Hardware encoders (NVENC, VideoToolbox, ...) can be added later behind this trait.
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

//! Video pipeline pieces: fixed-size canvas (scale + letterbox) and H.264 encode/decode.

mod canvas;
mod h264;

pub use canvas::{Canvas, FitRect, canvas_size, fit_rect};
pub use h264::{H264Decoder, H264Encoder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
}

impl Preset {
    pub const P720: Self = Self {
        name: "720p · 30 fps",
        max_width: 1280,
        max_height: 720,
        fps: 30,
        bitrate_bps: 4_000_000,
    };
    pub const P1080: Self = Self {
        name: "1080p · 30 fps",
        max_width: 1920,
        max_height: 1080,
        fps: 30,
        bitrate_bps: 8_000_000,
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

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("invalid frame: {0}")]
    InvalidInput(String),
    #[error("codec error: {0}")]
    Codec(String),
}

/// Hardware encoders (NVENC, VideoToolbox, ...) can be added later behind this trait.
pub trait VideoEncoder: Send {
    /// Encodes one BGRA picture. Returns `None` if the encoder skipped the frame.
    fn encode(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Option<EncodedFrame>, CodecError>;

    /// The next encoded frame will be an IDR keyframe.
    fn request_keyframe(&mut self);
}

pub trait VideoDecoder: Send {
    /// Decodes one Annex-B access unit. Returns `None` if no picture is ready yet.
    fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>, CodecError>;
}

//! Wire format.
//!
//! * Control (bidirectional stream opened by the viewer): length-prefixed postcard messages.
//!   Viewer sends `Hello`, broadcaster answers `Welcome` (which says whether audio is shared),
//!   then the viewer may send `RequestKeyframe`.
//! * Media (unidirectional streams opened by the broadcaster), each starting with one
//!   [`stream_kind`] byte:
//!   * video: fixed 21-byte header + H.264 Annex-B, per frame;
//!   * audio (only when shared): fixed 18-byte header + one Opus packet, per 20 ms.
//! * Session end reasons travel as QUIC application close codes (see [`close`]).

use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Version 2 adds audio. The ALPN stays the same so older peers still reach the version check
/// and report "incompatible version" instead of a handshake failure.
pub const PROTOCOL_VERSION: u16 = 2;
pub(crate) const ALPN: &[u8] = b"peeroxide/1";
pub(crate) const MAX_CONTROL_MSG: usize = 64 * 1024;
pub(crate) const MAX_FRAME: usize = 8 * 1024 * 1024;
/// Far above any 20 ms Opus packet (at most 1276 bytes); checked before allocating.
pub(crate) const MAX_AUDIO_PACKET: usize = 4 * 1024;
const HEADER_LEN: usize = 21;
const AUDIO_HEADER_LEN: usize = 18;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientMsg {
    Hello { version: u16, viewer_name: String },
    RequestKeyframe,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServerMsg {
    Welcome {
        broadcaster_name: String,
        /// Whether an audio stream follows. Fixed for the whole broadcast.
        audio: bool,
    },
}

/// First byte of every unidirectional stream.
pub(crate) mod stream_kind {
    pub const VIDEO: u8 = 0;
    pub const AUDIO: u8 = 1;
}

pub(crate) mod close {
    pub const VIEWER_LEFT: u32 = 0;
    pub const BROADCAST_STOPPED: u32 = 1;
    pub const SOURCE_CLOSED: u32 = 2;
    pub const BUSY: u32 = 3;
    pub const VERSION_MISMATCH: u32 = 4;
    pub const PROTOCOL_ERROR: u32 = 5;
}

/// One encoded access unit as sent on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    pub seq: u64,
    /// Broadcaster wall clock at capture; only comparable to the viewer's clock on the same machine.
    pub capture_time_us: u64,
    pub keyframe: bool,
    pub data: Bytes,
}

/// One encoded 20 ms audio packet as sent on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioPacket {
    pub seq: u64,
    /// Broadcaster wall clock at capture, on the same clock as [`VideoFrame::capture_time_us`].
    pub capture_time_us: u64,
    pub data: Bytes,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProtocolError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed message: {0}")]
    Malformed(#[from] postcard::Error),
    #[error("message of {0} bytes exceeds the limit")]
    TooLarge(usize),
    #[error("stream ended mid-message")]
    Truncated,
}

pub(crate) async fn write_msg<W, T>(w: &mut W, msg: &T) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = postcard::to_stdvec(msg)?;
    if body.len() > MAX_CONTROL_MSG {
        return Err(ProtocolError::TooLarge(body.len()));
    }
    w.write_all(&(body.len() as u32).to_le_bytes()).await?;
    w.write_all(&body).await?;
    Ok(())
}

/// Reads one message; `Ok(None)` on a clean end of stream between messages.
pub(crate) async fn read_msg<R, T>(r: &mut R) -> Result<Option<T>, ProtocolError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    if !read_exact_or_eof(r, &mut len).await? {
        return Ok(None);
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_CONTROL_MSG {
        return Err(ProtocolError::TooLarge(len));
    }
    let mut body = vec![0u8; len];
    if !read_exact_or_eof(r, &mut body).await? && len > 0 {
        return Err(ProtocolError::Truncated);
    }
    Ok(Some(postcard::from_bytes(&body)?))
}

pub(crate) async fn write_frame<W>(w: &mut W, f: &VideoFrame) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    if f.data.len() > MAX_FRAME {
        return Err(ProtocolError::TooLarge(f.data.len()));
    }
    let mut header = [0u8; HEADER_LEN];
    header[0..8].copy_from_slice(&f.seq.to_le_bytes());
    header[8..16].copy_from_slice(&f.capture_time_us.to_le_bytes());
    header[16] = u8::from(f.keyframe);
    header[17..21].copy_from_slice(&(f.data.len() as u32).to_le_bytes());
    w.write_all(&header).await?;
    w.write_all(&f.data).await?;
    Ok(())
}

pub(crate) async fn read_frame<R>(r: &mut R) -> Result<Option<VideoFrame>, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; HEADER_LEN];
    if !read_exact_or_eof(r, &mut header).await? {
        return Ok(None);
    }
    let len = u32::from_le_bytes(header[17..21].try_into().unwrap()) as usize;
    if len > MAX_FRAME {
        return Err(ProtocolError::TooLarge(len));
    }
    let mut data = vec![0u8; len];
    if !read_exact_or_eof(r, &mut data).await? && len > 0 {
        return Err(ProtocolError::Truncated);
    }
    Ok(Some(VideoFrame {
        seq: u64::from_le_bytes(header[0..8].try_into().unwrap()),
        capture_time_us: u64::from_le_bytes(header[8..16].try_into().unwrap()),
        keyframe: header[16] != 0,
        data: data.into(),
    }))
}

pub(crate) async fn write_stream_kind<W>(w: &mut W, kind: u8) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    w.write_all(&[kind]).await?;
    Ok(())
}

/// `Ok(None)` if the stream ended before saying what it carries.
pub(crate) async fn read_stream_kind<R>(r: &mut R) -> Result<Option<u8>, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    let mut kind = [0u8; 1];
    Ok(read_exact_or_eof(r, &mut kind).await?.then_some(kind[0]))
}

pub(crate) async fn write_audio<W>(w: &mut W, p: &AudioPacket) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
{
    if p.data.len() > MAX_AUDIO_PACKET {
        return Err(ProtocolError::TooLarge(p.data.len()));
    }
    let mut header = [0u8; AUDIO_HEADER_LEN];
    header[0..8].copy_from_slice(&p.seq.to_le_bytes());
    header[8..16].copy_from_slice(&p.capture_time_us.to_le_bytes());
    header[16..18].copy_from_slice(&(p.data.len() as u16).to_le_bytes());
    w.write_all(&header).await?;
    w.write_all(&p.data).await?;
    Ok(())
}

pub(crate) async fn read_audio<R>(r: &mut R) -> Result<Option<AudioPacket>, ProtocolError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; AUDIO_HEADER_LEN];
    if !read_exact_or_eof(r, &mut header).await? {
        return Ok(None);
    }
    let len = usize::from(u16::from_le_bytes(header[16..18].try_into().unwrap()));
    if len > MAX_AUDIO_PACKET {
        return Err(ProtocolError::TooLarge(len));
    }
    let mut data = vec![0u8; len];
    if !read_exact_or_eof(r, &mut data).await? && len > 0 {
        return Err(ProtocolError::Truncated);
    }
    Ok(Some(AudioPacket {
        seq: u64::from_le_bytes(header[0..8].try_into().unwrap()),
        capture_time_us: u64::from_le_bytes(header[8..16].try_into().unwrap()),
        data: data.into(),
    }))
}

/// Fills `buf`; returns `false` if the stream ended before the first byte.
async fn read_exact_or_eof<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut [u8],
) -> Result<bool, ProtocolError> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = r.read(&mut buf[filled..]).await?;
        if n == 0 {
            return if filled == 0 {
                Ok(false)
            } else {
                Err(ProtocolError::Truncated)
            };
        }
        filled += n;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn control_messages_roundtrip_and_end_cleanly() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let hello = ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            viewer_name: "Ana".into(),
        };
        write_msg(&mut a, &hello).await.unwrap();
        write_msg(&mut a, &ClientMsg::RequestKeyframe)
            .await
            .unwrap();
        drop(a);
        assert_eq!(read_msg::<_, ClientMsg>(&mut b).await.unwrap(), Some(hello));
        assert_eq!(
            read_msg::<_, ClientMsg>(&mut b).await.unwrap(),
            Some(ClientMsg::RequestKeyframe)
        );
        assert_eq!(read_msg::<_, ClientMsg>(&mut b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn oversized_control_message_is_rejected_before_allocating() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(u32::MAX).to_le_bytes()).await.unwrap();
        assert!(matches!(
            read_msg::<_, ClientMsg>(&mut b).await,
            Err(ProtocolError::TooLarge(_))
        ));
    }

    #[tokio::test]
    async fn garbage_control_message_is_malformed() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&3u32.to_le_bytes()).await.unwrap();
        a.write_all(&[0xff, 0xff, 0xff]).await.unwrap();
        assert!(matches!(
            read_msg::<_, ClientMsg>(&mut b).await,
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn frames_roundtrip_and_truncation_is_detected() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let f = VideoFrame {
            seq: 42,
            capture_time_us: 123_456,
            keyframe: true,
            data: Bytes::from_static(&[0, 0, 0, 1, 0x65, 1, 2, 3]),
        };
        write_frame(&mut a, &f).await.unwrap();
        assert_eq!(read_frame(&mut b).await.unwrap(), Some(f));

        a.write_all(&[0u8; 10]).await.unwrap();
        drop(a);
        assert!(matches!(
            read_frame(&mut b).await,
            Err(ProtocolError::Truncated)
        ));
    }

    #[tokio::test]
    async fn oversized_frame_header_is_rejected() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let mut header = [0u8; HEADER_LEN];
        header[17..21].copy_from_slice(&(MAX_FRAME as u32 + 1).to_le_bytes());
        a.write_all(&header).await.unwrap();
        assert!(matches!(
            read_frame(&mut b).await,
            Err(ProtocolError::TooLarge(_))
        ));
    }

    #[tokio::test]
    async fn audio_packets_roundtrip_after_the_stream_kind() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let p = AudioPacket {
            seq: 7,
            capture_time_us: 987_654,
            data: Bytes::from(vec![0xFC; 300]),
        };
        write_stream_kind(&mut a, stream_kind::AUDIO).await.unwrap();
        write_audio(&mut a, &p).await.unwrap();
        drop(a);
        assert_eq!(
            read_stream_kind(&mut b).await.unwrap(),
            Some(stream_kind::AUDIO)
        );
        assert_eq!(read_audio(&mut b).await.unwrap(), Some(p));
        assert_eq!(read_audio(&mut b).await.unwrap(), None);
        assert_eq!(read_stream_kind(&mut b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn oversized_audio_is_rejected_on_both_ends() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let big = AudioPacket {
            seq: 0,
            capture_time_us: 0,
            data: Bytes::from(vec![0; MAX_AUDIO_PACKET + 1]),
        };
        assert!(matches!(
            write_audio(&mut a, &big).await,
            Err(ProtocolError::TooLarge(_))
        ));
        let mut header = [0u8; AUDIO_HEADER_LEN];
        header[16..18].copy_from_slice(&u16::MAX.to_le_bytes());
        a.write_all(&header).await.unwrap();
        assert!(matches!(
            read_audio(&mut b).await,
            Err(ProtocolError::TooLarge(_))
        ));
    }

    #[tokio::test]
    async fn truncated_audio_is_detected() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        let mut header = [0u8; AUDIO_HEADER_LEN];
        header[16..18].copy_from_slice(&100u16.to_le_bytes());
        a.write_all(&header).await.unwrap();
        a.write_all(&[1; 10]).await.unwrap();
        drop(a);
        assert!(matches!(
            read_audio(&mut b).await,
            Err(ProtocolError::Truncated)
        ));
    }
}

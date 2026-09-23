//! Wire format.
//!
//! * Control (bidirectional stream opened by the viewer): length-prefixed postcard messages.
//!   Viewer sends `Hello`, broadcaster answers `Welcome`, then the viewer may send `RequestKeyframe`.
//! * Video (unidirectional stream opened by the broadcaster): fixed 21-byte header + H.264 Annex-B.
//! * Session end reasons travel as QUIC application close codes (see [`close`]).

use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u16 = 1;
pub(crate) const ALPN: &[u8] = b"peeroxide/1";
pub(crate) const MAX_CONTROL_MSG: usize = 64 * 1024;
pub(crate) const MAX_FRAME: usize = 8 * 1024 * 1024;
const HEADER_LEN: usize = 21;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientMsg {
    Hello { version: u16, viewer_name: String },
    RequestKeyframe,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServerMsg {
    Welcome { broadcaster_name: String },
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
}

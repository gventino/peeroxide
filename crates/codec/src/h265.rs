//! H.265 decoding with libde265 (see `crates/de265-sys`), available on every platform. Encoding
//! needs a hardware encoder (see `h265_mf.rs`).
#![allow(unsafe_code)]

use std::ffi::{CStr, c_int};
use std::ptr::NonNull;

use anyhow::{Context, bail, ensure};
use peeroxide_de265_sys as de265;
use yuv::{YuvPlanarImage, YuvRange, YuvStandardMatrix};

use crate::{DecodedFrame, VideoDecoder};

/// `DE265_ERROR_IMAGE_BUFFER_FULL`: pictures must be taken out before decoding continues.
const IMAGE_BUFFER_FULL: de265::de265_error = 9;

pub struct H265Decoder {
    ctx: NonNull<de265::de265_decoder_context>,
}

// SAFETY: the context is only ever used through `&mut self`, so by one thread at a time, which is
// all libde265 requires (it has no thread affinity).
unsafe impl Send for H265Decoder {}

impl H265Decoder {
    pub fn new() -> anyhow::Result<Self> {
        // SAFETY: no preconditions; a null result is handled.
        let ctx = unsafe { de265::de265_new_decoder() };
        let ctx = NonNull::new(ctx).context("could not create the H.265 decoder")?;
        Ok(Self { ctx })
    }

    /// Converts the next output picture, if any. The picture is only valid until the next
    /// libde265 call, so it is copied out right away.
    fn next_picture(&mut self) -> anyhow::Result<Option<DecodedFrame>> {
        // SAFETY: the context is valid; the returned picture is used before any other call.
        unsafe {
            let img = de265::de265_get_next_picture(self.ctx.as_ptr());
            if img.is_null() {
                return Ok(None);
            }
            to_rgba(img).map(Some)
        }
    }
}

impl Drop for H265Decoder {
    fn drop(&mut self) {
        // SAFETY: the context came from `de265_new_decoder` and is freed exactly once.
        unsafe {
            de265::de265_free_decoder(self.ctx.as_ptr());
        }
    }
}

impl VideoDecoder for H265Decoder {
    /// `data` is one complete access unit, so its picture comes out right away.
    fn decode(&mut self, data: &[u8]) -> anyhow::Result<Option<DecodedFrame>> {
        let len = c_int::try_from(data.len()).context("H.265 access unit too large")?;
        let ctx = self.ctx.as_ptr();
        // SAFETY: `data` outlives the call (libde265 copies it); the context is valid.
        let pushed = unsafe {
            let err =
                de265::de265_push_data(ctx, data.as_ptr().cast(), len, 0, std::ptr::null_mut());
            de265::de265_push_end_of_frame(ctx);
            err
        };
        check(pushed)?;

        let mut newest = None;
        loop {
            let mut more: c_int = 0;
            // SAFETY: the context is valid and `more` is a valid out pointer.
            let err = unsafe { de265::de265_decode(ctx, &mut more) };
            if let Some(frame) = self.next_picture()? {
                newest = Some(frame);
            }
            match err {
                de265::DE265_ERROR_WAITING_FOR_INPUT_DATA => break,
                IMAGE_BUFFER_FULL => continue,
                _ => {
                    if let Err(e) = check(err) {
                        // Start clean: the next keyframe carries everything needed.
                        // SAFETY: the context is valid.
                        unsafe { de265::de265_reset(ctx) };
                        return Err(e);
                    }
                }
            }
            if more == 0 {
                break;
            }
        }
        while let Some(frame) = self.next_picture()? {
            newest = Some(frame);
        }
        Ok(newest)
    }
}

fn check(err: de265::de265_error) -> anyhow::Result<()> {
    // SAFETY: `de265_isOK` and `de265_get_error_text` accept any value; the text is static.
    unsafe {
        if de265::de265_isOK(err) != 0 {
            return Ok(());
        }
        let text = CStr::from_ptr(de265::de265_get_error_text(err));
        bail!("H.265 decoding: {} ({err})", text.to_string_lossy())
    }
}

/// One plane of `img` as a slice of exactly the bytes the conversion reads.
///
/// # Safety
/// `img` must be a valid picture that stays alive while the slice is used.
unsafe fn plane<'a>(
    img: *const de265::de265_image,
    channel: c_int,
) -> anyhow::Result<(&'a [u8], u32, u32, u32)> {
    // SAFETY: the caller guarantees `img` is valid.
    unsafe {
        let width = de265::de265_get_image_width(img, channel);
        let height = de265::de265_get_image_height(img, channel);
        let mut stride: c_int = 0;
        let data = de265::de265_get_image_plane(img, channel, &mut stride);
        ensure!(
            !data.is_null() && width > 0 && height > 0 && stride >= width,
            "H.265 picture without a usable plane {channel}"
        );
        let (w, h, s) = (width as usize, height as usize, stride as usize);
        // Rows are `stride` apart; the last one only needs `width` bytes.
        let slice = std::slice::from_raw_parts(data, s * (h - 1) + w);
        Ok((slice, width as u32, height as u32, stride as u32))
    }
}

/// # Safety
/// `img` must be a valid picture that stays alive during the call.
unsafe fn to_rgba(img: *const de265::de265_image) -> anyhow::Result<DecodedFrame> {
    // SAFETY: the caller guarantees `img` is valid.
    unsafe {
        ensure!(
            de265::de265_get_chroma_format(img) == de265::DE265_CHROMA_420,
            "unsupported H.265 chroma format (only 4:2:0 is)"
        );
        ensure!(
            (0..3).all(|c| de265::de265_get_bits_per_pixel(img, c) == 8),
            "unsupported H.265 bit depth (only 8 bits is)"
        );
        let (y, width, height, y_stride) = plane(img, 0)?;
        let (u, _, _, u_stride) = plane(img, 1)?;
        let (v, _, _, v_stride) = plane(img, 2)?;
        let planar = YuvPlanarImage {
            y_plane: y,
            y_stride,
            u_plane: u,
            u_stride,
            v_plane: v,
            v_stride,
            width,
            height,
        };
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        // BT.601 limited range, like the H.264 path and the H.265 encoder's conversion.
        yuv::yuv420_to_rgba(
            &planar,
            &mut rgba,
            width * 4,
            YuvRange::Limited,
            YuvStandardMatrix::Bt601,
        )
        .context("converting the H.265 picture")?;
        Ok(DecodedFrame {
            width,
            height,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{H, W, picture, psnr};

    /// 30 frames of `picture(n)` from the AMD hardware encoder, with keyframes at 0 and 20.
    /// Each access unit: 4-byte little-endian length, a keyframe byte, then Annex-B data.
    /// Regenerate with the ignored test `h265_mf::tests::write_h265_fixture`.
    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/pattern-320x180.aus");

    fn access_units() -> Vec<(bool, &'static [u8])> {
        let mut rest = FIXTURE;
        let mut units = Vec::new();
        while !rest.is_empty() {
            let len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
            units.push((rest[4] == 1, &rest[5..5 + len]));
            rest = &rest[5 + len..];
        }
        units
    }

    #[test]
    fn each_access_unit_decodes_at_once_with_good_quality() {
        let units = access_units();
        assert_eq!(units.len(), 30);
        let mut dec = H265Decoder::new().unwrap();
        for (n, (_, data)) in units.iter().enumerate() {
            let out = dec
                .decode(data)
                .unwrap()
                .expect("a picture per access unit, no delay");
            assert_eq!((out.width, out.height), (W, H));
            let q = psnr(&picture(n as u32), &out.rgba);
            assert!(q > 30.0, "frame {n}: psnr {q:.1} dB");
        }
    }

    #[test]
    fn decoding_can_start_at_the_second_keyframe() {
        let units = access_units();
        let keyframes: Vec<usize> = (0..units.len()).filter(|&i| units[i].0).collect();
        assert_eq!(keyframes, [0, 20]);
        let mut dec = H265Decoder::new().unwrap();
        for (n, (_, data)) in units.iter().enumerate().skip(20) {
            let out = dec.decode(data).unwrap().expect("picture");
            assert!(psnr(&picture(n as u32), &out.rgba) > 30.0, "frame {n}");
        }
    }

    /// Like a broken or hostile stream from the network: errors or nothing, never a crash, and
    /// the decoder recovers at the next keyframe.
    #[test]
    fn garbage_is_survived_and_the_next_keyframe_recovers() {
        let units = access_units();
        let mut dec = H265Decoder::new().unwrap();
        let mut seed = 0x9e37_79b9_u32;
        for len in [0usize, 1, 3, 7, 100, 5000, 70_000] {
            let mut junk: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            let _ = dec.decode(&junk);
            // Start codes followed by junk reach the NAL parsers.
            junk.splice(0..0, [0, 0, 0, 1]);
            let _ = dec.decode(&junk);
        }
        // A cut-off keyframe, then the real one.
        let _ = dec.decode(&units[20].1[..units[20].1.len() / 2]);
        let out = units[20..]
            .iter()
            .filter_map(|(_, data)| dec.decode(data).ok().flatten())
            .last()
            .expect("pictures after the keyframe");
        assert!(psnr(&picture(29), &out.rgba) > 30.0);
    }
}

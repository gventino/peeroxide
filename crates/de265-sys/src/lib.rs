//! Hand-written bindings to the part of libde265's C API (`vendor/libde265/de265.h`) that
//! Peeroxide uses. The library itself is compiled from `vendor/` by `build.rs`.
#![allow(non_camel_case_types, unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

/// `de265_error`: 0 is OK, 1–999 are errors, 1000 and up are warnings.
pub type de265_error = c_int;
pub const DE265_OK: de265_error = 0;
pub const DE265_ERROR_WAITING_FOR_INPUT_DATA: de265_error = 13;

/// `de265_chroma`
pub type de265_chroma = c_int;
pub const DE265_CHROMA_420: de265_chroma = 1;

pub type de265_PTS = i64;

#[repr(C)]
pub struct de265_image {
    _private: [u8; 0],
}

#[repr(C)]
pub struct de265_decoder_context {
    _private: [u8; 0],
}

unsafe extern "C" {
    pub fn de265_get_version() -> *const c_char;
    pub fn de265_get_error_text(err: de265_error) -> *const c_char;
    /// True for `DE265_OK` and warnings.
    pub fn de265_isOK(err: de265_error) -> c_int;

    pub fn de265_new_decoder() -> *mut de265_decoder_context;
    pub fn de265_start_worker_threads(
        ctx: *mut de265_decoder_context,
        number_of_threads: c_int,
    ) -> de265_error;
    pub fn de265_free_decoder(ctx: *mut de265_decoder_context) -> de265_error;

    /// Annex-B bytes with start codes; only queues them.
    pub fn de265_push_data(
        ctx: *mut de265_decoder_context,
        data: *const c_void,
        length: c_int,
        pts: de265_PTS,
        user_data: *mut c_void,
    ) -> de265_error;
    /// The data pushed so far ends a picture, so it can be decoded without waiting for more.
    pub fn de265_push_end_of_frame(ctx: *mut de265_decoder_context);
    pub fn de265_decode(ctx: *mut de265_decoder_context, more: *mut c_int) -> de265_error;
    pub fn de265_reset(ctx: *mut de265_decoder_context);

    /// Valid only until the next call into the decoder.
    pub fn de265_get_next_picture(ctx: *mut de265_decoder_context) -> *const de265_image;

    pub fn de265_get_image_width(img: *const de265_image, channel: c_int) -> c_int;
    pub fn de265_get_image_height(img: *const de265_image, channel: c_int) -> c_int;
    pub fn de265_get_chroma_format(img: *const de265_image) -> de265_chroma;
    pub fn de265_get_bits_per_pixel(img: *const de265_image, channel: c_int) -> c_int;
    pub fn de265_get_image_plane(
        img: *const de265_image,
        channel: c_int,
        out_stride: *mut c_int,
    ) -> *const u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_reports_the_vendored_version() {
        // SAFETY: returns a pointer to a static NUL-terminated string.
        let version = unsafe { std::ffi::CStr::from_ptr(de265_get_version()) };
        assert_eq!(version.to_str().unwrap(), "1.1.3");
    }

    #[test]
    fn decoder_starts_and_rejects_nothing_pushed() {
        // SAFETY: the context is created, used on this thread only, and freed once.
        unsafe {
            let ctx = de265_new_decoder();
            assert!(!ctx.is_null());
            let mut more = 0;
            let err = de265_decode(ctx, &mut more);
            assert_eq!(err, DE265_ERROR_WAITING_FOR_INPUT_DATA);
            assert!(de265_get_next_picture(ctx).is_null());
            assert_eq!(de265_free_decoder(ctx), DE265_OK);
        }
    }
}

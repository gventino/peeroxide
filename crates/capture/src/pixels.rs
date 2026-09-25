//! Converts the pixel layouts delivered by the scap backends into tightly packed BGRA.

use rayon::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// B, G, R, (ignored) — also covers BGRA.
    Bgrx,
    /// R, G, B, (ignored)
    Rgbx,
    /// (ignored), B, G, R
    Xbgr,
    /// R, G, B, three bytes per pixel.
    Rgb,
}

impl Layout {
    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            _ => 4,
        }
    }
}

/// `data` may carry per-row padding (PipeWire buffers do); the stride is derived from its length.
/// Rows are converted in parallel on rayon's pool.
pub(crate) fn to_bgra(width: u32, height: u32, data: &[u8], layout: Layout) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    let bpp = layout.bytes_per_pixel();
    let row = w * bpp;
    if w == 0 || h == 0 || data.len() < row * h {
        return None;
    }
    let stride = data.len() / h;
    let mut out = vec![0u8; w * h * 4];
    out.par_chunks_exact_mut(w * 4)
        .zip(data.par_chunks(stride))
        .for_each(|(dst, src)| {
            for (px, bgra) in src[..row].chunks_exact(bpp).zip(dst.chunks_exact_mut(4)) {
                let (r, g, b) = match layout {
                    Layout::Bgrx => (px[2], px[1], px[0]),
                    Layout::Rgbx | Layout::Rgb => (px[0], px[1], px[2]),
                    Layout::Xbgr => (px[3], px[2], px[1]),
                };
                bgra.copy_from_slice(&[b, g, r, 255]);
            }
        });
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two pixels: pure red, then pure blue, expected as BGRA.
    const EXPECTED: [u8; 8] = [0, 0, 255, 255, 255, 0, 0, 255];

    #[test]
    fn converts_every_layout() {
        let cases: [(Layout, &[u8]); 4] = [
            (Layout::Bgrx, &[0, 0, 255, 9, 255, 0, 0, 9]),
            (Layout::Rgbx, &[255, 0, 0, 9, 0, 0, 255, 9]),
            (Layout::Xbgr, &[9, 0, 0, 255, 9, 255, 0, 0]),
            (Layout::Rgb, &[255, 0, 0, 0, 0, 255]),
        ];
        for (layout, data) in cases {
            assert_eq!(to_bgra(2, 1, data, layout).unwrap(), EXPECTED, "{layout:?}");
        }
    }

    #[test]
    fn skips_row_padding() {
        let mut data = vec![0u8; 2 * 12];
        data[0..8].copy_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        data[12..20].copy_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
        let out = to_bgra(2, 2, &data, Layout::Bgrx).unwrap();
        assert_eq!(&out[0..8], &EXPECTED);
        assert_eq!(&out[8..16], &EXPECTED);
    }

    #[test]
    fn rejects_short_or_empty_buffers() {
        assert_eq!(to_bgra(2, 2, &[0; 15], Layout::Bgrx), None);
        assert_eq!(to_bgra(0, 2, &[0; 16], Layout::Bgrx), None);
    }

    /// A 1080p conversion per layout, paced at 30 fps like a capture.
    #[test]
    #[ignore = "timing; run with --release --ignored --nocapture"]
    fn conversion_speed() {
        use std::time::{Duration, Instant};
        const FRAMES: u32 = 60;
        for layout in [Layout::Bgrx, Layout::Rgb] {
            let data: Vec<u8> = (0..1920 * 1080 * layout.bytes_per_pixel())
                .map(|i| (i % 251) as u8)
                .collect();
            let mut busy = Duration::ZERO;
            for _ in 0..FRAMES {
                let started = Instant::now();
                assert!(to_bgra(1920, 1080, &data, layout).is_some());
                busy += started.elapsed();
                std::thread::sleep(Duration::from_millis(33));
            }
            let ms = busy.as_secs_f64() * 1000.0 / f64::from(FRAMES);
            println!("{layout:?} 1920x1080: {ms:.2} ms per frame");
        }
    }
}

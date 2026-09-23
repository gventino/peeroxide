//! Converts the pixel layouts delivered by the scap backends into tightly packed BGRA.

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
pub(crate) fn to_bgra(width: u32, height: u32, data: &[u8], layout: Layout) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    let bpp = layout.bytes_per_pixel();
    let row = w * bpp;
    if w == 0 || h == 0 || data.len() < row * h {
        return None;
    }
    let stride = data.len() / h;
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let src = &data[y * stride..y * stride + row];
        for px in src.chunks_exact(bpp) {
            let (r, g, b) = match layout {
                Layout::Bgrx => (px[2], px[1], px[0]),
                Layout::Rgbx | Layout::Rgb => (px[0], px[1], px[2]),
                Layout::Xbgr => (px[3], px[2], px[1]),
            };
            out.extend_from_slice(&[b, g, r, 255]);
        }
    }
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
}

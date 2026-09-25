use anyhow::{Context, ensure};
use fast_image_resize::images::{CroppedImageMut, Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::Preset;

/// Output size for a source: fit inside the preset box, never upscale, even dimensions.
pub fn canvas_size(src_w: u32, src_h: u32, preset: &Preset) -> (u32, u32) {
    let scale = f64::min(
        1.0,
        f64::min(
            f64::from(preset.max_width) / f64::from(src_w.max(1)),
            f64::from(preset.max_height) / f64::from(src_h.max(1)),
        ),
    );
    let even = |v: f64| ((v.round() as u32) & !1).max(16);
    (
        even(f64::from(src_w) * scale),
        even(f64::from(src_h) * scale),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FitRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Largest aspect-preserving rectangle for `src` centered inside `dst`.
pub fn fit_rect(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> FitRect {
    let scale = f64::min(
        f64::from(dst_w) / f64::from(src_w.max(1)),
        f64::from(dst_h) / f64::from(src_h.max(1)),
    );
    let width = ((f64::from(src_w) * scale).round() as u32).clamp(1, dst_w);
    let height = ((f64::from(src_h) * scale).round() as u32).clamp(1, dst_h);
    FitRect {
        x: (dst_w - width) / 2,
        y: (dst_h - height) / 2,
        width,
        height,
    }
}

/// Fixed-size BGRA picture the encoder always sees; sources of any size are letterboxed into it.
pub struct Canvas {
    image: Image<'static>,
    resizer: Resizer,
    last_fit: Option<FitRect>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            image: Image::new(width, height, PixelType::U8x4),
            resizer: Resizer::new(),
            last_fit: None,
        }
    }

    pub fn width(&self) -> u32 {
        self.image.width()
    }

    pub fn height(&self) -> u32 {
        self.image.height()
    }

    pub fn bgra(&self) -> &[u8] {
        self.image.buffer()
    }

    pub fn draw(&mut self, bgra: &[u8], width: u32, height: u32) -> anyhow::Result<()> {
        let (cw, ch) = (self.width(), self.height());
        let fit = fit_rect(width, height, cw, ch);
        if self.last_fit != Some(fit) {
            self.image.buffer_mut().fill(0);
            self.last_fit = Some(fit);
        }

        if fit.width == width && fit.height == height {
            let row = width as usize * 4;
            ensure!(
                bgra.len() >= row * height as usize,
                "invalid frame: buffer too small"
            );
            let canvas_row = cw as usize * 4;
            let dst = self.image.buffer_mut();
            for (y, src) in bgra.chunks_exact(row).take(height as usize).enumerate() {
                let start = (fit.y as usize + y) * canvas_row + fit.x as usize * 4;
                dst[start..start + row].copy_from_slice(src);
            }
            return Ok(());
        }

        let src = ImageRef::new(width, height, bgra, PixelType::U8x4).context("invalid frame")?;
        let mut dst = CroppedImageMut::new(&mut self.image, fit.x, fit.y, fit.width, fit.height)
            .context("invalid frame")?;
        let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
        self.resizer
            .resize(&src, &mut dst, &options)
            .context("scaling the frame")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_size_caps_and_keeps_even() {
        let p = Preset::P1080;
        assert_eq!(canvas_size(1920, 1080, &p), (1920, 1080));
        assert_eq!(canvas_size(3840, 2160, &p), (1920, 1080));
        assert_eq!(canvas_size(1601, 987, &p), (1600, 986));
        assert_eq!(canvas_size(2560, 1600, &p), (1728, 1080));
        assert_eq!(canvas_size(800, 1600, &p), (540, 1080));
        assert_eq!(canvas_size(5, 3, &p), (16, 16));
    }

    #[test]
    fn fit_rect_letterboxes_and_pillarboxes() {
        assert_eq!(
            fit_rect(720, 720, 1280, 720),
            FitRect {
                x: 280,
                y: 0,
                width: 720,
                height: 720
            }
        );
        assert_eq!(
            fit_rect(1280, 360, 1280, 720),
            FitRect {
                x: 0,
                y: 180,
                width: 1280,
                height: 360
            }
        );
    }

    #[test]
    fn draw_centers_smaller_source_and_blacks_out_borders() {
        let mut canvas = Canvas::new(8, 4);
        canvas.draw(&[255u8; 4 * 4 * 4], 4, 4).unwrap();
        let px = |x: usize, y: usize| canvas.bgra()[(y * 8 + x) * 4];
        assert_eq!(px(0, 0), 0);
        assert_eq!(px(2, 1), 255);
        assert_eq!(px(5, 3), 255);
        assert_eq!(px(7, 3), 0);
    }

    #[test]
    fn draw_downscales_larger_source() {
        let mut canvas = Canvas::new(16, 16);
        canvas.draw(&vec![200u8; 64 * 32 * 4], 64, 32).unwrap();
        assert_eq!(canvas.bgra()[0], 0);
        assert_eq!(canvas.bgra()[(8 * 16 + 8) * 4], 200);
    }
}

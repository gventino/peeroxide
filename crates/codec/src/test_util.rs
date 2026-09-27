//! Synthetic pictures shared by the codec tests.

pub const W: u32 = 320;
pub const H: u32 = 180;

pub fn picture(n: u32) -> Vec<u8> {
    let mut data = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let in_box = (x + W - (n * 8) % W) % W < 40 && y > 60 && y < 120;
            let v = if in_box { 250 } else { (x * 255 / W) as u8 };
            data[i..i + 4].copy_from_slice(&[v, (y * 255 / H) as u8, 128, 255]);
        }
    }
    data
}

pub fn psnr(bgra: &[u8], rgba: &[u8]) -> f64 {
    let mut se = 0.0;
    let mut n = 0.0;
    for (s, d) in bgra.chunks_exact(4).zip(rgba.chunks_exact(4)) {
        for (a, b) in [(s[2], d[0]), (s[1], d[1]), (s[0], d[2])] {
            se += (f64::from(a) - f64::from(b)).powi(2);
            n += 1.0;
        }
    }
    10.0 * (255.0f64.powi(2) / (se / n)).log10()
}

pub const PAGE_W: u32 = 1280;
pub const PAGE_H: u32 = 720;

/// A 720p-wide page of pseudo-random "text" (18 px lines), three screens tall.
pub fn text_page() -> Vec<u8> {
    let (w, h) = (PAGE_W, PAGE_H * 3);
    let mut page = vec![255u8; (w * h * 4) as usize];
    let mut seed = 0x2545_f491_u32;
    for line in 0..h / 18 {
        let mut x = 20;
        while x + 12 < w - 20 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let glyph_w = 4 + seed % 9;
            for gy in 0..11 {
                for gx in 0..glyph_w {
                    if (seed >> ((gx + gy) % 31)) & 1 == 1 {
                        let i = (((line * 18 + gy + 3) * w + x + gx) * 4) as usize;
                        page[i..i + 3].copy_from_slice(&[20, 20, 20]);
                    }
                }
            }
            x += glyph_w + 2 + u32::from(seed.is_multiple_of(7)) * 8;
        }
    }
    page
}

/// Frame `n` of the page scrolling 6 px per frame, like a user scrolling a document.
pub fn scrolled(page: &[u8], n: u32) -> &[u8] {
    let row = (PAGE_W * 4) as usize;
    let offset = ((n * 6) % (PAGE_H * 2)) as usize * row;
    &page[offset..offset + row * PAGE_H as usize]
}

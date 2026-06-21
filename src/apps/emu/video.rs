//! Game Boy video scaler.
//!
//! The DMG screen is 160x144; the panel is 240x135. We stretch to the FULL panel
//! (160->240 wide, 144->135 tall) so the whole little screen is used — the image is
//! ~1.5x wider than tall vs the GB's near-square, a deliberate trade (the panel is
//! small enough that black bars hurt more than the mild stretch). Peanut-GB hands
//! us one source scanline at a time via `lcd_draw_line`, so we map each GB line to
//! its panel row and stretch it horizontally into the framebuffer in one pass.

use crate::hal::fb::W;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;

pub const GB_W: usize = 160;
pub const GB_H: usize = 144;

/// Scaled image geometry inside the 240x135 panel (full width, full height).
pub const DST_W: usize = W; // stretch the full 240 px width
pub const DST_H: usize = 135; // GB_H * 15 / 16
pub const X_OFF: usize = (W - DST_W) / 2; // 0

/// Four-shade grey palette, index 0 = lightest (matches Peanut-GB's 2-bit output).
/// A faint green tint nods to the original DMG LCD without hurting readability.
const SHADES: [Rgb565; 4] = [
    Rgb565::new(0x1D, 0x3D, 0x12), // lightest
    Rgb565::new(0x14, 0x2C, 0x0D),
    Rgb565::new(0x0A, 0x16, 0x07),
    Rgb565::new(0x02, 0x05, 0x02), // darkest
];

/// Panel row for a given GB scanline (0..DST_H). 16 GB lines collapse to 15 rows.
#[inline]
pub fn dst_row(gb_line: u8) -> usize {
    (gb_line as usize * 15) / 16
}

/// Blit one GB scanline (160 palette indices in the low 2 bits) into `fb` at the
/// scaled position. `fb` is the raw 240x135 row-major framebuffer. The horizontal
/// downscale (160->150) is a cheap inline `dx*16/15` per pixel.
pub fn draw_line(fb: &mut [Rgb565], pixels: &[u8; GB_W], gb_line: u8) {
    let y = dst_row(gb_line);
    if y >= DST_H {
        return;
    }
    let base = y * W + X_OFF;
    for dx in 0..DST_W {
        let sx = ((dx * GB_W) / DST_W).min(GB_W - 1);
        let src = pixels[sx] & 0x03;
        fb[base + dx] = SHADES[src as usize];
    }
}

/// Blit one GB/GBC scanline that is already RGB565 (Walnut-CGB resolves the CGB
/// palette for us) into `fb` at the scaled position. Same 160->150 downscale as
/// `draw_line`, but the source pixel is the colour itself.
#[cfg(feature = "emugbc")]
pub fn draw_line_rgb(fb: &mut [Rgb565], pixels: &[u16; GB_W], gb_line: u8) {
    use embedded_graphics::pixelcolor::raw::RawU16;
    let y = dst_row(gb_line);
    if y >= DST_H {
        return;
    }
    let base = y * W + X_OFF;
    for dx in 0..DST_W {
        let sx = ((dx * GB_W) / DST_W).min(GB_W - 1);
        fb[base + dx] = Rgb565::from(RawU16::new(pixels[sx]));
    }
}

/// Paint the side bars (and clear the frame) black before a game starts.
pub fn clear(fb: &mut [Rgb565]) {
    fb.fill(Rgb565::BLACK);
}

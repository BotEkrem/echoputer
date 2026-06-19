//! In-RAM framebuffer. Every screen renders off-screen here, then `main` blits
//! the whole buffer to the panel in one `set_pixels` pass — so there is never a
//! clear-then-draw "black flash", on updates or screen transitions.

use core::convert::Infallible;
use embedded_graphics::{pixelcolor::Rgb565, prelude::*, primitives::Rectangle};

pub const W: usize = 240;
pub const H: usize = 135;

pub struct FrameBuf {
    buf: &'static mut [Rgb565],
}

impl FrameBuf {
    pub fn new(buf: &'static mut [Rgb565]) -> Self {
        Self { buf }
    }

    /// Row-major pixels, for blitting to the display.
    pub fn pixels(&self) -> impl Iterator<Item = Rgb565> + '_ {
        self.buf.iter().copied()
    }
}

impl Dimensions for FrameBuf {
    fn bounding_box(&self) -> Rectangle {
        Rectangle::new(Point::zero(), Size::new(W as u32, H as u32))
    }
}

impl DrawTarget for FrameBuf {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Infallible>
    where
        I: IntoIterator<Item = Pixel<Rgb565>>,
    {
        for Pixel(p, c) in pixels {
            if p.x >= 0 && p.y >= 0 && (p.x as usize) < W && (p.y as usize) < H {
                self.buf[p.y as usize * W + p.x as usize] = c;
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Rgb565) -> Result<(), Infallible> {
        self.buf.fill(color);
        Ok(())
    }
}

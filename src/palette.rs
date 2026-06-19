//! Per-app accent colours: an evenly-spaced HSV hue wheel ("tone by tone").
//!
//! Ported from a JS generator that walks `h` from 0..1 at full saturation/value
//! and emits N vivid colours. We take a 16-slot wheel and give each app the slot
//! at its menu position (slot 0 = the first app, etc.). The result on the 16-bit
//! display: Hacking=red, Synthwave=orange, File Browser=amber, Charge=yellow,
//! Settings=chartreuse — a warm-to-green gradient across the launcher.

use embedded_graphics::pixelcolor::Rgb565;

/// Number of evenly-spaced hues on the wheel.
pub const WHEEL: usize = 16;

/// HSV -> 8-bit RGB. All inputs in `0.0..=1.0`. Faithful port of the JS
/// `hsvToRgb`: `i = floor(h*6)`, then pick the sextant.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let i = libm::floorf(h * 6.0) as i32;
    let f = h * 6.0 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// Pack 8-bit RGB into RGB565 (5/6/5 bits).
fn rgb565(r: u8, g: u8, b: u8) -> Rgb565 {
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}

/// Colour for wheel slot `i` (wraps at [`WHEEL`]): hue `i/WHEEL`, full S and V.
pub fn wheel(i: usize) -> Rgb565 {
    let h = (i % WHEEL) as f32 / WHEEL as f32;
    let (r, g, b) = hsv_to_rgb(h, 1.0, 1.0);
    rgb565(r, g, b)
}

//! Onboard WS2812 RGB LED (GPIO21) via the RMT peripheral.
//!
//! No smart-LED crate is compatible with esp-hal 1.1.1, so we encode the WS2812
//! bitstream by hand into RMT pulse codes. The LED reacts to the music: hue from
//! the current scale, brightness pulsing with the live output level (plus a slow
//! idle breathing so it's always alive).

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::RgbColor;
use esp_hal::gpio::Level;
use esp_hal::rmt::PulseCode;

// RMT base clock 80 MHz, clk_divider = 1 -> 1 tick = 12.5 ns.
const T0H: u16 = 28; // ~0.35 us
const T0L: u16 = 64; // ~0.80 us
const T1H: u16 = 56; // ~0.70 us
const T1L: u16 = 48; // ~0.60 us
const RESET: u16 = 6000; // >50 us latch
const MAX_BRIGHT: f32 = 0.5; // onboard LEDs are bright; cap for comfort

/// 24 data pulses + reset + end marker.
pub const LEN: usize = 26;

/// Encode an RGB colour into WS2812 (GRB-order) RMT pulse codes.
pub fn encode(r: u8, g: u8, b: u8) -> [PulseCode; LEN] {
    let bytes = [g, r, b]; // WS2812 wants green first
    let mut data = [PulseCode::end_marker(); LEN];
    let mut i = 0;
    for byte in bytes {
        for bit in 0..8u8 {
            let one = (byte & (0x80 >> bit)) != 0;
            data[i] = if one {
                PulseCode::new(Level::High, T1H, Level::Low, T1L)
            } else {
                PulseCode::new(Level::High, T0H, Level::Low, T0L)
            };
            i += 1;
        }
    }
    data[24] = PulseCode::new(Level::Low, RESET, Level::Low, RESET);
    data[25] = PulseCode::end_marker();
    data
}

/// LED colour for `mode`, pulsing with `level` (0..1) plus slow idle breathing
/// driven by `phase` (radians).
/// LED colour: the theme `accent` hue with a slow waving brightness (boosted by
/// the audio `level`). `user` is the user brightness 0.0..1.0 (0 = off).
pub fn accent_wave(accent: Rgb565, level: f32, phase: f32, user: f32) -> (u8, u8, u8) {
    // RGB565 channels -> 0..255
    let r = (accent.r() as f32) * (255.0 / 31.0);
    let g = (accent.g() as f32) * (255.0 / 63.0);
    let b = (accent.b() as f32) * (255.0 / 31.0);
    let wave = 0.5 + 0.5 * libm::sinf(phase); // 0..1 smooth wave
    let bright = (0.30 + 0.45 * wave + level * 0.5).min(1.0) * MAX_BRIGHT * user.clamp(0.0, 1.0);
    ((r * bright) as u8, (g * bright) as u8, (b * bright) as u8)
}

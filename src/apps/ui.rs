//! Synthwave app screen — minimal, theme-driven (see [`crate::theme`]).

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};

use crate::apps::scales::{self, Mode};
use crate::theme;

const NOTE_TOP: i32 = 22;
const NOTE_CY: i32 = 44;
const FREQ_Y: i32 = 60;
const LABEL_Y: i32 = 78;
const METER_Y: i32 = 88;
const VOL_Y: i32 = 102;

/// Full screen, call when the synth app opens.
pub fn draw_static(d: &mut impl DrawTarget<Color = Rgb565>, mode: Mode, vol: u8) {
    theme::clear(d);
    theme::topbar(d, "Synthwave");
    theme::badge(d, theme::W - theme::PAD - 68, 3, mode.name(), theme::accent());
    theme::text(d, "LEVEL", theme::PAD, LABEL_Y, theme::BODY_FONT, theme::MUTED);
    draw_note(d, None, mode);
    draw_vu(d, 0.0, 0, mode);
    draw_volume(d, vol, mode);
    theme::hint(d, "keys play    G0 scale    ESC menu");
}

/// Big current note. `None` = idle.
pub fn draw_note(d: &mut impl DrawTarget<Color = Rgb565>, midi: Option<u8>, _mode: Mode) {
    theme::fill(d, 0, NOTE_TOP, theme::W as u32, (LABEL_Y - NOTE_TOP - 2) as u32, theme::BG);
    match midi {
        Some(m) => {
            let mut buf = [0u8; 4];
            let name = scales::note_name(m, &mut buf);
            theme::text_center(d, name, theme::W / 2, NOTE_CY, &FONT_10X20, theme::FG);
            let uw = name.len() as i32 * 11;
            theme::fill(d, theme::W / 2 - uw / 2, NOTE_CY + 13, uw as u32, 2, theme::accent());
            let mut fb = [0u8; 12];
            let s = fmt_hz(scales::midi_to_freq(m) as u32, &mut fb);
            theme::text_center(d, s, theme::W / 2, FREQ_Y + 4, theme::BODY_FONT, theme::MUTED);
        }
        None => {
            theme::text_center(d, "press a key", theme::W / 2, NOTE_CY, theme::BODY_FONT, theme::FAINT);
        }
    }
}

/// Live output level meter.
pub fn draw_vu(d: &mut impl DrawTarget<Color = Rgb565>, level: f32, _voices: usize, _mode: Mode) {
    theme::meter(d, theme::PAD, METER_Y, theme::W - 2 * theme::PAD, 5, level, theme::accent());
}

/// Volume ticks.
pub fn draw_volume(d: &mut impl DrawTarget<Color = Rgb565>, vol: u8, _mode: Mode) {
    theme::fill(d, 0, VOL_Y, theme::W as u32, 10, theme::BG);
    theme::text(d, "VOL", theme::PAD, VOL_Y + 1, theme::BODY_FONT, theme::MUTED);
    theme::ticks(d, theme::PAD + 28, VOL_Y + 1, 10, vol as i32, theme::accent());
}

/// Re-draw the parts that depend on the accent after a scale switch.
pub fn flash_mode(d: &mut impl DrawTarget<Color = Rgb565>, mode: Mode, vol: u8) {
    theme::fill(d, 120, 0, (theme::W - 120) as u32, 16, theme::BG);
    theme::badge(d, theme::W - theme::PAD - 68, 3, mode.name(), theme::accent());
    theme::draw_battery(d, theme::W - theme::PAD, 3);
    draw_note(d, None, mode);
    draw_vu(d, 0.0, 0, mode);
    draw_volume(d, vol, mode);
}

fn fmt_hz(hz: u32, buf: &mut [u8; 12]) -> &str {
    let mut tmp = [0u8; 6];
    let mut n = hz;
    let mut i = 0;
    if n == 0 {
        tmp[i] = b'0';
        i += 1;
    }
    while n > 0 && i < tmp.len() {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    let mut j = 0;
    while i > 0 {
        i -= 1;
        buf[j] = tmp[i];
        j += 1;
    }
    for &b in b" Hz" {
        if j < buf.len() {
            buf[j] = b;
            j += 1;
        }
    }
    core::str::from_utf8(&buf[..j]).unwrap_or("")
}

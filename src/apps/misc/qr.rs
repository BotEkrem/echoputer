//! QR — type some text (a URL, a WiFi string, anything), press ENTER, and get a
//! scannable QR code on screen. Pure offline: no SD, no radio. The matrix is drawn
//! dark-on-white with a quiet-zone border so a phone camera can read it off the panel.
//!
//! The encoder lives in [`crate::apps::misc::qr_encode`] (a small no_std QR generator). Both
//! the input buffer and the generated matrix are heap-backed, so this app's resident
//! footprint is a handful of bytes — it never bloats the (tight) main stack frame.

use alloc::vec::Vec;
use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

use crate::apps::misc::qr_encode::Qr as QrCode;
use crate::hal::keymap;
use crate::i18n::qr;
use crate::{i18n, theme};

const CAP: usize = 160; // max input chars (comfortably inside QR version 10 @ ECC M)
const QUIET: usize = 4; // quiet-zone modules each side (the spec minimum)
const TOPY: i32 = 22; // input row baseline
const AREA_Y: i32 = 36; // QR drawing area top
const AREA_H: i32 = 92; // QR drawing area height budget

pub struct Qr {
    buf: Vec<u8>, // typed input (heap; empty until used)
    code: Option<QrCode>,
    err: bool,   // last encode overflowed
    shift: bool, // "Aa" caps/shift toggle for typed characters
}

impl Qr {
    pub fn new() -> Self {
        Qr {
            buf: Vec::new(),
            code: None,
            err: false,
            shift: false,
        }
    }

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        self.buf.clear();
        self.code = None;
        self.err = false;
        self.shift = false;
        self.draw_all(d);
    }

    /// Free the heap input buffer + QR matrix when leaving the app.
    pub fn exit(&mut self) {
        self.code = None;
        self.buf = Vec::new();
    }

    /// "Aa" toggles caps/shift for typed characters (uppercase + symbols like `:`).
    pub fn toggle_caps(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        self.shift = !self.shift;
        self.draw_input(d);
    }

    pub fn on_key(&mut self, rc: (u8, u8), d: &mut impl DrawTarget<Color = Rgb565>) {
        if rc == crate::K_ENTER {
            self.code = QrCode::encode(&self.buf);
            self.err = self.code.is_none() && !self.buf.is_empty();
            self.draw_all(d);
            return;
        }
        if rc == keymap::K_BKSP {
            if !self.buf.is_empty() {
                self.buf.pop();
                self.draw_input(d);
            }
            return;
        }
        if let Some(b) = keymap::ch_shift(rc.0, rc.1, self.shift) {
            if self.buf.len() < CAP {
                self.buf.push(b);
                self.draw_input(d);
            }
        }
    }

    pub fn tick(&mut self, _d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        false // static screen — all drawing happens on keypress
    }

    /// Redraw just the input row (cheap, on every keystroke).
    fn draw_input(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::fill(d, 0, TOPY - 2, theme::W as u32, 14, theme::BG);
        let text = core::str::from_utf8(&self.buf).unwrap_or("");
        // show the tail if it's longer than the row can hold (~28 chars at BODY font)
        let shown = if text.len() > 28 { &text[text.len() - 28..] } else { text };
        theme::text(d, ">", theme::PAD, TOPY, theme::BODY_FONT, theme::accent());
        theme::text(d, shown, theme::PAD + 10, TOPY, theme::BODY_FONT, theme::FG);
    }

    fn draw_all(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::clear(d);
        theme::topbar(d, "QR");
        self.draw_input(d);
        match self.code.as_ref() {
            Some(code) => self.draw_matrix(d, code),
            None => {
                let msg = if self.err {
                    i18n::t(qr::TOO_LONG)
                } else {
                    i18n::t(qr::TYPE_TO_GENERATE)
                };
                theme::text(d, msg, theme::PAD, AREA_Y + 30, theme::BODY_FONT, theme::MUTED);
            }
        }
        theme::hint(d, i18n::t(qr::HINT));
    }

    /// Draw the QR dark-on-white with a quiet zone, scaled to fit the area.
    fn draw_matrix(&self, d: &mut impl DrawTarget<Color = Rgb565>, code: &QrCode) {
        let size = code.size();
        let total = size + 2 * QUIET;
        // integer pixels-per-module that fits both the height budget and the width
        let scale = ((AREA_H as usize / total).min(theme::W as usize / total)).max(1) as i32;
        let dim = total as i32 * scale;
        let ox = (theme::W as i32 - dim) / 2;
        let oy = AREA_Y + (AREA_H - dim).max(0) / 2;

        // white field (data + quiet zone) for scan contrast
        theme::fill(d, ox, oy, dim as u32, dim as u32, Rgb565::WHITE);
        let q = QUIET as i32;
        for y in 0..size {
            for x in 0..size {
                if code.module(x, y) {
                    let px = ox + (x as i32 + q) * scale;
                    let py = oy + (y as i32 + q) * scale;
                    theme::fill(d, px, py, scale as u32, scale as u32, Rgb565::BLACK);
                }
            }
        }
    }
}

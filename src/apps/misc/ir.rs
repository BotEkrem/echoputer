//! IR remote — fire NEC codes from the onboard IR LED. Transmit only (the Cardputer
//! has no IR receiver, so codes are sent, not learned): pick a preset TV-power code or
//! enter a 32-bit NEC code yourself (look your device's value up in any NEC code list).
//! Aim the top edge of the Cardputer at the device and press ENTER.

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

use crate::hal::ir::{IrTx, PRESETS};
use crate::hal::keymap;
use crate::i18n::ir;
use crate::{i18n, theme};

const CUSTOM: usize = PRESETS.len(); // row index of the "Custom" entry (last row)
const ROWS: usize = PRESETS.len() + 1;
const TOP: i32 = 30;
const ROW_H: i32 = 16;

pub struct Ir {
    tx: IrTx,
    sel: usize,      // 0..PRESETS.len() = a preset; == CUSTOM = the custom-code row
    custom: u32,     // 32-bit NEC code being entered (hex) on the Custom row
    last_sent: bool, // flash a "sent" confirmation after a transmit
}

impl Ir {
    pub fn new(tx: IrTx) -> Self {
        Ir {
            tx,
            sel: 0,
            custom: 0,
            last_sent: false,
        }
    }

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        self.last_sent = false;
        self.draw_all(d);
    }

    /// The IR channel lives inside `IrTx`, owned by this app for its lifetime — there
    /// is no transient heap to release.
    pub fn exit(&mut self) {}

    pub fn on_key(&mut self, rc: (u8, u8), d: &mut impl DrawTarget<Color = Rgb565>) {
        match rc {
            crate::K_UP => {
                if self.sel > 0 {
                    self.sel -= 1;
                    self.last_sent = false;
                    self.draw_all(d);
                }
            }
            crate::K_DOWN => {
                if self.sel < CUSTOM {
                    self.sel += 1;
                    self.last_sent = false;
                    self.draw_all(d);
                }
            }
            crate::K_ENTER => {
                let code = if self.sel == CUSTOM { self.custom } else { PRESETS[self.sel].1 };
                self.tx.send_nec(code);
                self.last_sent = true;
                self.draw_all(d);
            }
            _ => {
                // On the Custom row, edit the hex code: digits 0-9/a-f shift in a nibble,
                // backspace drops one.
                if self.sel == CUSTOM {
                    if rc == keymap::K_BKSP {
                        self.custom >>= 4;
                        self.last_sent = false;
                        self.draw_all(d);
                    } else if let Some(nib) = keymap::ch_shift(rc.0, rc.1, false).and_then(hex_nibble) {
                        self.custom = (self.custom << 4) | nib as u32;
                        self.last_sent = false;
                        self.draw_all(d);
                    }
                }
            }
        }
    }

    pub fn tick(&mut self, _d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        false // static screen — drawing happens on keypress
    }

    fn draw_all(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::clear(d);
        theme::topbar(d, "IR");
        for i in 0..ROWS {
            let y = TOP + i as i32 * ROW_H;
            let selected = i == self.sel;
            let col = if selected { theme::accent() } else { theme::MUTED };
            if selected {
                theme::text(d, ">", theme::PAD, y, theme::TITLE_FONT, theme::accent());
            }
            if i == CUSTOM {
                let mut hb = [0u8; 8];
                hex8(self.custom, &mut hb);
                let hs = core::str::from_utf8(&hb).unwrap_or("00000000");
                theme::text(d, i18n::t(ir::CUSTOM), theme::PAD + 16, y, theme::TITLE_FONT, col);
                theme::text(d, hs, theme::PAD + 86, y + 1, theme::BODY_FONT, if selected { theme::FG } else { theme::FAINT });
            } else {
                theme::text(d, PRESETS[i].0, theme::PAD + 16, y, theme::TITLE_FONT, col);
            }
        }
        // Confirmation + aim hint.
        if self.last_sent {
            theme::text_center(d, i18n::t(ir::SENT), theme::W / 2, 100, theme::BODY_FONT, theme::accent());
        } else {
            theme::text_center(d, i18n::t(ir::AIM_HINT), theme::W / 2, 100, theme::BODY_FONT, theme::FAINT);
        }
        let hint = if self.sel == CUSTOM {
            i18n::t(ir::CUSTOM_HINT)
        } else {
            i18n::t(ir::PICK_HINT)
        };
        theme::hint(d, hint);
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Write `v` as 8 uppercase hex digits into `buf`.
fn hex8(v: u32, buf: &mut [u8; 8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for i in 0..8 {
        buf[i] = HEX[((v >> (28 - i * 4)) & 0xF) as usize];
    }
}

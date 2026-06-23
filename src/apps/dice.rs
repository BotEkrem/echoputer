//! Dice / RNG — two modes, switched with LEFT/RIGHT.
//!
//! DICE mode: pick a preset with UP/DOWN — d4, d6, d8, d10, d12, d20, d100, or a
//! coin (Heads/Tails) — and ENTER rolls it, showing a big centred result.
//!
//! RANGE mode: type a MIN and a MAX (digits + backspace, TAB switches the active
//! field), ENTER returns a uniform integer in [min, max] inclusive.
//!
//! Randomness is the snake-style LCG (state = state*1664525 + 1013904223), seeded
//! from the boot Instant at new() and churned on every key and tick — no RNG
//! crate, no global. Range rolls use rejection sampling to avoid modulo bias.
//! Resident state is tiny (well under 1 KB) and exit() is a no-op.

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};

use esp_hal::time::Instant;

use crate::{hal::keymap, i18n, theme};

/// 'r' key (row1 col4 on the silkscreen) — re-roll convenience in DICE mode.
const K_R: (u8, u8) = (1, 4);
/// TAB key (row1 col0) — switch the active field in RANGE mode.
const K_TAB: (u8, u8) = (1, 0);

/// Vertical centre of the big result readout.
const BIG_CY: i32 = 74;

/// The dice presets, in selection order. A coin is modelled as a 2-sided die but
/// rendered as Heads/Tails instead of a number.
const PRESETS: [u16; 8] = [4, 6, 8, 10, 12, 20, 100, 2];
const COIN_IDX: usize = 7; // the 2-sided entry is the coin

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Dice,
    Range,
}

/// Which RANGE field digits flow into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Min,
    Max,
}

pub struct Dice {
    mode: Mode,
    rng: u32,
    // DICE mode -------------------------------------------------------------
    sel: usize,    // index into PRESETS
    result: u16,   // last roll value (1..=sides); for coin: 1=Heads 2=Tails
    rolled: bool,  // a result is showing
    // RANGE mode ------------------------------------------------------------
    field: Field,
    min: u32,      // typed min (0 when the field is empty)
    max: u32,      // typed max
    range_val: u32, // last range roll result
    range_rolled: bool,
    range_err: bool, // min > max -> show an error instead of a value
}

impl Dice {
    pub fn new() -> Self {
        // Seed from the boot Instant so the first roll isn't deterministic across
        // power cycles; a non-zero seed keeps the LCG from sticking at 0.
        let seed = Instant::now().duration_since_epoch().as_micros() as u32;
        Dice {
            mode: Mode::Dice,
            rng: seed | 1,
            sel: 1, // default to d6
            result: 0,
            rolled: false,
            field: Field::Min,
            min: 1,
            max: 6,
            range_val: 0,
            range_rolled: false,
            range_err: false,
        }
    }

    /// Advance the LCG and return the new state.
    fn rand(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.rng
    }

    /// Uniform integer in 0..n (n > 0) via rejection sampling to dodge modulo bias.
    fn uniform(&mut self, n: u32) -> u32 {
        if n <= 1 {
            return 0;
        }
        // Largest multiple of n that fits in u32; reject the biased tail above it.
        let zone = u32::MAX - (u32::MAX % n);
        loop {
            let r = self.rand();
            if r < zone {
                return r % n;
            }
        }
    }

    /// Sides of the currently selected preset.
    fn sides(&self) -> u16 {
        PRESETS[self.sel]
    }

    fn is_coin(&self) -> bool {
        self.sel == COIN_IDX
    }

    // ---- public interface (called by main.rs) ----

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        // Fresh visit: clear any stale result, churn the RNG, full repaint.
        self.rolled = false;
        self.range_rolled = false;
        self.range_err = false;
        let _ = self.rand();
        self.draw_all(d);
    }

    pub fn on_key(&mut self, rc: (u8, u8), d: &mut impl DrawTarget<Color = Rgb565>) {
        // Stir on every key so timing of presses feeds the randomness.
        let _ = self.rand();

        // Mode switch is global to both modes.
        if rc == crate::K_LEFT || rc == crate::K_RIGHT {
            self.mode = match self.mode {
                Mode::Dice => Mode::Range,
                Mode::Range => Mode::Dice,
            };
            self.draw_all(d);
            return;
        }

        match self.mode {
            Mode::Dice => self.on_key_dice(rc, d),
            Mode::Range => self.on_key_range(rc, d),
        }
    }

    fn on_key_dice(&mut self, rc: (u8, u8), d: &mut impl DrawTarget<Color = Rgb565>) {
        match rc {
            crate::K_UP => {
                self.sel = if self.sel == 0 { PRESETS.len() - 1 } else { self.sel - 1 };
                self.rolled = false;
                self.draw_dice_body(d);
            }
            crate::K_DOWN => {
                self.sel = (self.sel + 1) % PRESETS.len();
                self.rolled = false;
                self.draw_dice_body(d);
            }
            crate::K_ENTER | K_R => {
                let n = self.sides() as u32;
                self.result = (self.uniform(n) + 1) as u16;
                self.rolled = true;
                self.draw_dice_body(d);
            }
            _ => {}
        }
    }

    fn on_key_range(&mut self, rc: (u8, u8), d: &mut impl DrawTarget<Color = Rgb565>) {
        if rc == K_TAB {
            self.field = match self.field {
                Field::Min => Field::Max,
                Field::Max => Field::Min,
            };
            self.draw_range_body(d);
            return;
        }
        if rc == keymap::K_BKSP {
            let f = match self.field {
                Field::Min => &mut self.min,
                Field::Max => &mut self.max,
            };
            *f /= 10;
            self.range_rolled = false;
            self.draw_range_body(d);
            return;
        }
        if rc == crate::K_ENTER {
            if self.min > self.max {
                self.range_err = true;
                self.range_rolled = false;
            } else {
                let span = self.max - self.min + 1; // inclusive, both fit in u32
                self.range_val = self.min + self.uniform(span);
                self.range_rolled = true;
                self.range_err = false;
            }
            self.draw_range_body(d);
            return;
        }
        // Digit entry. Ignore non-digits; cap the field at 6 digits (< 1e6) so the
        // span stays comfortably inside u32 and the readout fits the screen.
        if let Some(b) = keymap::ch_shift(rc.0, rc.1, false) {
            if b.is_ascii_digit() {
                let f = match self.field {
                    Field::Min => &mut self.min,
                    Field::Max => &mut self.max,
                };
                if *f < 100_000 {
                    *f = *f * 10 + (b - b'0') as u32;
                    self.range_rolled = false;
                    self.range_err = false;
                    self.draw_range_body(d);
                }
            }
        }
    }

    pub fn tick(&mut self, _d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        // No animation — just keep the LCG churning so idle time seeds randomness.
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        false
    }

    pub fn exit(&mut self) {
        // No heap held — nothing to free.
    }

    // ---- drawing ----

    fn draw_all(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Dice", "Zar"));
        // Mode label on the row under the topbar.
        self.draw_mode_row(d);
        match self.mode {
            Mode::Dice => self.draw_dice_body(d),
            Mode::Range => self.draw_range_body(d),
        }
    }

    fn draw_mode_row(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::fill(d, 0, theme::TOPBAR_Y + 3, theme::W as u32, 12, theme::BG);
        let label = match self.mode {
            Mode::Dice => i18n::t("DICE", "ZAR"),
            Mode::Range => i18n::t("RANGE", "ARALIK"),
        };
        theme::text(d, label, theme::PAD, theme::TOPBAR_Y + 4, theme::BODY_FONT, theme::MUTED);
        theme::text_right(
            d,
            i18n::t("<> mode", "<> mod"),
            theme::W - theme::PAD,
            theme::TOPBAR_Y + 4,
            theme::BODY_FONT,
            theme::FAINT,
        );
    }

    /// Repaint the DICE content area (preset name + big result), below the mode row.
    fn draw_dice_body(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        // Erase the whole content band (mode row sits above at TOPBAR_Y+3..+15).
        theme::fill(d, 0, theme::TOPBAR_Y + 16, theme::W as u32, (theme::HINT_Y - (theme::TOPBAR_Y + 16) - 2) as u32, theme::BG);

        // Selected die name, e.g. "d20" or "COIN".
        let mut nb = [0u8; 8];
        let name = self.preset_name(&mut nb);
        theme::text_center(d, name, theme::W / 2, 44, theme::BODY_FONT, theme::FG);

        // Big result.
        if self.rolled {
            if self.is_coin() {
                let s = if self.result == 1 {
                    i18n::t("HEADS", "YAZI")
                } else {
                    i18n::t("TAILS", "TURA")
                };
                theme::text_center(d, s, theme::W / 2, BIG_CY, &FONT_10X20, theme::accent());
                let uw = s.len() as i32 * 11;
                theme::fill(d, theme::W / 2 - uw / 2, BIG_CY + 13, uw as u32, 2, theme::accent());
            } else {
                let mut rb = [0u8; 8];
                let s = fmt_u32(self.result as u32, &mut rb);
                theme::text_center(d, s, theme::W / 2, BIG_CY, &FONT_10X20, theme::accent());
                let uw = s.len() as i32 * 11;
                theme::fill(d, theme::W / 2 - uw / 2, BIG_CY + 13, uw as u32, 2, theme::accent());
            }
        } else {
            theme::text_center(d, "--", theme::W / 2, BIG_CY, &FONT_10X20, theme::FAINT);
        }

        theme::hint(d, i18n::t("up/dn pick  Enter roll  <> mode", "yuk/asa sec  Enter at  <> mod"));
    }

    /// "d6", "d100", or "COIN" for the current selection.
    fn preset_name<'a>(&self, buf: &'a mut [u8; 8]) -> &'a str {
        if self.is_coin() {
            return i18n::t("COIN", "PARA");
        }
        buf[0] = b'd';
        let mut tail = [0u8; 8];
        let n = fmt_u32(self.sides() as u32, &mut tail);
        let mut j = 1;
        for &b in n.as_bytes() {
            if j < buf.len() {
                buf[j] = b;
                j += 1;
            }
        }
        core::str::from_utf8(&buf[..j]).unwrap_or("d6")
    }

    /// Repaint the RANGE content: two labelled number fields + big result.
    fn draw_range_body(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::fill(d, 0, theme::TOPBAR_Y + 16, theme::W as u32, (theme::HINT_Y - (theme::TOPBAR_Y + 16) - 2) as u32, theme::BG);

        // Two fields side by side: MIN (left) and MAX (right). The active one
        // gets an accent card; the inactive a neutral one. fy=46 keeps the MIN/MAX
        // labels (drawn at fy-11) clear of the mode row above (which ends ~y31) —
        // at the old fy=36 the "RANGE" label and the "MIN" label collided.
        let fy = 46;
        let fw = 104u32;
        let fh = 26u32;
        let lx = 8;
        let rx = theme::W - 8 - fw as i32;

        let min_active = self.field == Field::Min;
        theme::card(d, lx, fy, fw, fh, if min_active { Some(theme::accent()) } else { None });
        theme::card(d, rx, fy, fw, fh, if !min_active { Some(theme::accent()) } else { None });

        theme::text(d, i18n::t("MIN", "ALT"), lx + 6, fy - 11, theme::BODY_FONT, theme::MUTED);
        theme::text(d, i18n::t("MAX", "UST"), rx + 6, fy - 11, theme::BODY_FONT, theme::MUTED);

        let mut mb = [0u8; 8];
        let ms = fmt_u32(self.min, &mut mb);
        theme::text(d, ms, lx + 8, fy + 4, &FONT_10X20, theme::FG);
        let mut xb = [0u8; 8];
        let xs = fmt_u32(self.max, &mut xb);
        theme::text(d, xs, rx + 8, fy + 4, &FONT_10X20, theme::FG);

        // Result / error band.
        if self.range_err {
            theme::text_center(
                d,
                i18n::t("min > max", "alt > ust"),
                theme::W / 2,
                BIG_CY + 14,
                theme::BODY_FONT,
                theme::DESTRUCTIVE,
            );
        } else if self.range_rolled {
            let mut rb = [0u8; 12];
            let s = fmt_u32(self.range_val, &mut rb);
            theme::text_center(d, s, theme::W / 2, BIG_CY + 14, &FONT_10X20, theme::accent());
            let uw = s.len() as i32 * 11;
            theme::fill(d, theme::W / 2 - uw / 2, BIG_CY + 27, uw as u32, 2, theme::accent());
        } else {
            theme::text_center(d, "--", theme::W / 2, BIG_CY + 14, &FONT_10X20, theme::FAINT);
        }

        theme::hint(d, i18n::t("type digits  Tab field  Enter roll", "rakam yaz  Tab alan  Enter at"));
    }
}

/// `u32` -> decimal into `buf`, returning the slice as &str.
fn fmt_u32(v: u32, buf: &mut [u8]) -> &str {
    let mut tmp = [0u8; 10];
    let mut n = v;
    let mut i = 0;
    if n == 0 {
        tmp[0] = b'0';
        i = 1;
    } else {
        while n > 0 && i < tmp.len() {
            tmp[i] = b'0' + (n % 10) as u8;
            n /= 10;
            i += 1;
        }
    }
    let mut j = 0;
    while i > 0 && j < buf.len() {
        i -= 1;
        buf[j] = tmp[i];
        j += 1;
    }
    core::str::from_utf8(&buf[..j]).unwrap_or("0")
}

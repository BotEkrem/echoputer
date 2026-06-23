//! Unit converter. Four categories — Length, Mass, Temperature, Data — cycled
//! with LEFT/RIGHT. Within a category UP/DOWN moves the selection between three
//! fields: FROM unit, TO unit and the VALUE; typed digits and '.' edit the value
//! while it's selected, and the converted result is recomputed and shown live.
//!
//! Non-temperature categories are modelled as a unit list with a factor-to-base
//! (e.g. Length's base is metres, Data's base is bytes). Temperature is special-
//! cased: everything is converted via Celsius with C/F/K offset math. Self-
//! contained, tiny resident state (a few indices + a short ASCII value buffer);
//! no heap, so exit() is a no-op.

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};

use crate::{i18n, theme};

/// A unit within a (non-temperature) category: a display label + its factor to
/// the category's base unit (value_in_base = value * factor).
struct Unit {
    label: &'static str,
    factor: f32,
}

/// Categories carry an index used to special-case temperature in the math.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cat {
    Length,
    Mass,
    Temp,
    Data,
}

const CATS: [Cat; 4] = [Cat::Length, Cat::Mass, Cat::Temp, Cat::Data];

impl Cat {
    fn name(self) -> &'static str {
        match self {
            Cat::Length => i18n::t("Length", "Uzunluk"),
            Cat::Mass => i18n::t("Mass", "Kutle"),
            Cat::Temp => i18n::t("Temperature", "Sicaklik"),
            Cat::Data => i18n::t("Data", "Veri"),
        }
    }

    /// The unit table for a category. Temperature returns a table of bare labels
    /// (factors unused — converted specially); the others use factor-to-base.
    fn units(self) -> &'static [Unit] {
        match self {
            // base: metre
            Cat::Length => &[
                Unit { label: "m", factor: 1.0 },
                Unit { label: "km", factor: 1000.0 },
                Unit { label: "cm", factor: 0.01 },
                Unit { label: "mm", factor: 0.001 },
                Unit { label: "in", factor: 0.0254 },
                Unit { label: "ft", factor: 0.3048 },
                Unit { label: "mi", factor: 1609.344 },
            ],
            // base: gram
            Cat::Mass => &[
                Unit { label: "g", factor: 1.0 },
                Unit { label: "kg", factor: 1000.0 },
                Unit { label: "lb", factor: 453.59237 },
                Unit { label: "oz", factor: 28.349523 },
            ],
            // labels only; conversion is offset math via Celsius
            Cat::Temp => &[
                Unit { label: "C", factor: 1.0 },
                Unit { label: "F", factor: 1.0 },
                Unit { label: "K", factor: 1.0 },
            ],
            // base: byte
            Cat::Data => &[
                Unit { label: "B", factor: 1.0 },
                Unit { label: "KB", factor: 1024.0 },
                Unit { label: "MB", factor: 1048576.0 },
                Unit { label: "GB", factor: 1073741824.0 },
            ],
        }
    }
}

/// Which of the three editable rows is highlighted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    From,
    To,
    Value,
}

// Layout: three stacked rows under the topbar, then the big result + hint.
const ROW_FROM_Y: i32 = 26;
const ROW_TO_Y: i32 = 50;
const ROW_VAL_Y: i32 = 74;
const ROW_H: u32 = 20;
const LABEL_X: i32 = theme::PAD + 4;
const FIELD_X: i32 = theme::PAD + 56;
const RESULT_CY: i32 = 108;

/// Max characters the user can type into the value field.
const VAL_CAP: usize = 12;

pub struct Convert {
    cat: usize,            // index into CATS
    from: usize,           // index into the current category's unit table
    to: usize,
    field: Field,          // highlighted row
    val: [u8; VAL_CAP],    // ASCII value being edited (digits + at most one '.')
    val_len: usize,
}

impl Convert {
    pub fn new() -> Self {
        Convert {
            cat: 0,
            from: 0,
            to: 1,
            field: Field::Value,
            val: [0; VAL_CAP],
            val_len: 0,
        }
    }

    fn cat_kind(&self) -> Cat {
        CATS[self.cat]
    }

    /// Reset units + value to sensible defaults for the current category.
    fn reset_for_cat(&mut self) {
        self.from = 0;
        self.to = 1; // a distinct second unit if the table has one
        let n = self.cat_kind().units().len();
        if self.to >= n {
            self.to = 0;
        }
        self.set_val_str("1");
    }

    /// Overwrite the value buffer from an ASCII string (defaults / clamping).
    fn set_val_str(&mut self, s: &str) {
        self.val_len = 0;
        for &b in s.as_bytes() {
            if self.val_len < VAL_CAP {
                self.val[self.val_len] = b;
                self.val_len += 1;
            }
        }
    }

    fn val_str(&self) -> &str {
        core::str::from_utf8(&self.val[..self.val_len]).unwrap_or("0")
    }

    /// Parse the value buffer to f32. Empty / lone '.' / '-' parse as 0.
    fn val_f32(&self) -> f32 {
        parse_f32(self.val_str())
    }

    /// Does the buffer already contain a decimal point?
    fn has_dot(&self) -> bool {
        self.val[..self.val_len].contains(&b'.')
    }

    /// The converted result for the current category / units / value.
    fn result(&self) -> f32 {
        let v = self.val_f32();
        match self.cat_kind() {
            Cat::Temp => convert_temp(v, self.from, self.to),
            _ => {
                let units = self.cat_kind().units();
                let base = v * units[self.from].factor;
                base / units[self.to].factor
            }
        }
    }

    // ---- key handling ----

    fn cycle_cat(&mut self, forward: bool) {
        let n = CATS.len();
        if forward {
            self.cat = (self.cat + 1) % n;
        } else {
            self.cat = (self.cat + n - 1) % n;
        }
        self.reset_for_cat();
    }

    fn cycle_field(&mut self, down: bool) {
        self.field = match (self.field, down) {
            (Field::From, true) => Field::To,
            (Field::To, true) => Field::Value,
            (Field::Value, true) => Field::From,
            (Field::From, false) => Field::Value,
            (Field::To, false) => Field::From,
            (Field::Value, false) => Field::To,
        };
    }

    /// On the From/To rows, LEFT/RIGHT cycles the chosen unit instead of the
    /// category. Returns true if a unit was changed.
    fn cycle_unit(&mut self, forward: bool) -> bool {
        let n = self.cat_kind().units().len();
        let slot = match self.field {
            Field::From => &mut self.from,
            Field::To => &mut self.to,
            Field::Value => return false,
        };
        if forward {
            *slot = (*slot + 1) % n;
        } else {
            *slot = (*slot + n - 1) % n;
        }
        true
    }

    fn type_char(&mut self, b: u8) {
        // Only digits and a single decimal point are accepted into the value.
        let ok = b.is_ascii_digit() || (b == b'.' && !self.has_dot());
        if !ok {
            return;
        }
        if self.val_len < VAL_CAP {
            self.val[self.val_len] = b;
            self.val_len += 1;
        }
    }

    fn backspace(&mut self) {
        if self.val_len > 0 {
            self.val_len -= 1;
        }
    }

    // ---- public interface (called by main.rs) ----

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.cat = 0;
        self.field = Field::Value;
        self.reset_for_cat();
        theme::clear(d);
        theme::topbar(d, i18n::t("Convert", "Cevir"));
        self.draw_all(d);
    }

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) {
        match rc {
            crate::K_UP => self.cycle_field(false),
            crate::K_DOWN => self.cycle_field(true),
            crate::K_LEFT => {
                // On a unit row, LEFT/RIGHT picks the unit; otherwise the category.
                if !self.cycle_unit(false) {
                    self.cycle_cat(false);
                }
            }
            crate::K_RIGHT => {
                if !self.cycle_unit(true) {
                    self.cycle_cat(true);
                }
            }
            crate::hal::keymap::K_BKSP => {
                if self.field == Field::Value {
                    self.backspace();
                }
            }
            _ => {
                if self.field == Field::Value {
                    if let Some(b) = crate::hal::keymap::ch_shift(rc.0, rc.1, false) {
                        self.type_char(b);
                    }
                }
            }
        }
        self.draw_all(d);
    }

    /// No animation — everything is event-driven, so tick never draws.
    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, _d: &mut D) -> bool {
        false
    }

    /// No heap allocations to release.
    pub fn exit(&mut self) {}

    // ---- drawing ----

    /// Repaint the whole content area (cheap: a handful of fills + texts).
    fn draw_all<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        // category name, just under the topbar divider, on its own thin band
        theme::fill(d, 0, theme::TOPBAR_Y + 2, theme::W as u32, 10, theme::BG);
        theme::text(d, self.cat_kind().name(), theme::PAD, theme::TOPBAR_Y + 3, theme::BODY_FONT, theme::MUTED);
        // category position on the right of the same band (e.g. 2/4)
        let mut cb = [0u8; 4];
        let cs = fmt_ratio(self.cat as u8 + 1, CATS.len() as u8, &mut cb);
        theme::text_right(d, cs, theme::W - theme::PAD, theme::TOPBAR_Y + 3, theme::BODY_FONT, theme::FAINT);

        let units = self.cat_kind().units();
        self.draw_row(d, ROW_FROM_Y, i18n::t("From", "Birim1"), units[self.from].label, self.field == Field::From);
        self.draw_row(d, ROW_TO_Y, i18n::t("To", "Birim2"), units[self.to].label, self.field == Field::To);
        self.draw_value_row(d);
        self.draw_result(d);
        self.draw_hint(d);
    }

    /// A unit row: a label on the left, a framed unit cell, accent when selected.
    fn draw_row<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, y: i32, label: &str, unit: &str, sel: bool) {
        theme::fill(d, 0, y - 1, theme::W as u32, ROW_H + 2, theme::BG);
        theme::card(d, theme::PAD, y, (theme::W - 2 * theme::PAD) as u32, ROW_H, if sel { Some(theme::accent()) } else { None });
        let lc = if sel { theme::FG } else { theme::MUTED };
        theme::text(d, label, LABEL_X, y + 5, theme::BODY_FONT, theme::FAINT);
        theme::text(d, unit, FIELD_X, y + 4, theme::TITLE_FONT, lc);
        if sel {
            // hint that LEFT/RIGHT cycles the unit here
            theme::text_right(d, "< >", theme::W - theme::PAD - 6, y + 5, theme::BODY_FONT, theme::accent());
        }
    }

    /// The value row: shows the typed number, with a caret when it's selected.
    fn draw_value_row<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let sel = self.field == Field::Value;
        let y = ROW_VAL_Y;
        theme::fill(d, 0, y - 1, theme::W as u32, ROW_H + 2, theme::BG);
        theme::card(d, theme::PAD, y, (theme::W - 2 * theme::PAD) as u32, ROW_H, if sel { Some(theme::accent()) } else { None });
        theme::text(d, i18n::t("Value", "Deger"), LABEL_X, y + 5, theme::BODY_FONT, theme::FAINT);
        let s = if self.val_len == 0 { "0" } else { self.val_str() };
        let col = if sel { theme::FG } else { theme::MUTED };
        theme::text(d, s, FIELD_X, y + 4, theme::TITLE_FONT, col);
        if sel {
            // caret right after the digits (8px per glyph in the title font)
            let glyphs = if self.val_len == 0 { 1 } else { self.val_len };
            let cx = FIELD_X + glyphs as i32 * 8;
            theme::fill(d, cx + 1, y + 4, 2, 13, theme::accent());
        }
    }

    /// The big live result, centred, with the target unit label appended.
    fn draw_result<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::fill(d, 0, RESULT_CY - 12, theme::W as u32, 24, theme::BG);
        let units = self.cat_kind().units();
        let mut buf = [0u8; 24];
        let s = fmt_result(self.result(), units[self.to].label, &mut buf);
        theme::text_center(d, s, theme::W / 2, RESULT_CY, &FONT_10X20, theme::accent());
    }

    fn draw_hint<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let s = match self.field {
            Field::Value => i18n::t("type value  up/dn field  <> category", "deger yaz  yuk/asa alan  <> kategori"),
            _ => i18n::t("<> unit  up/dn field", "<> birim  yuk/asa alan"),
        };
        theme::hint(d, s);
    }
}

/// Convert a temperature `v` from unit index `from` to `to` (0=C, 1=F, 2=K).
/// Goes via Celsius as the pivot.
fn convert_temp(v: f32, from: usize, to: usize) -> f32 {
    let c = match from {
        1 => (v - 32.0) * 5.0 / 9.0, // F -> C
        2 => v - 273.15,             // K -> C
        _ => v,                      // C
    };
    match to {
        1 => c * 9.0 / 5.0 + 32.0, // C -> F
        2 => c + 273.15,           // C -> K
        _ => c,                    // C
    }
}

/// Parse a small unsigned decimal string ("12", "3.5", ".5", "") to f32 without
/// the std float parser. Unknown chars are skipped; a lone '.' / empty -> 0.
fn parse_f32(s: &str) -> f32 {
    let mut int_part: f32 = 0.0;
    let mut frac_part: f32 = 0.0;
    let mut scale: f32 = 1.0;
    let mut seen_dot = false;
    for &b in s.as_bytes() {
        match b {
            b'0'..=b'9' => {
                let d = (b - b'0') as f32;
                if seen_dot {
                    scale *= 0.1;
                    frac_part += d * scale;
                } else {
                    int_part = int_part * 10.0 + d;
                }
            }
            b'.' => seen_dot = true,
            _ => {}
        }
    }
    int_part + frac_part
}

/// "n/d" into `buf` (e.g. "2/4"). Both fit a single digit for our 4 categories.
fn fmt_ratio(n: u8, d: u8, buf: &mut [u8; 4]) -> &str {
    let mut i = 0;
    push_u64(buf, &mut i, n as u64);
    if i < buf.len() {
        buf[i] = b'/';
        i += 1;
    }
    push_u64(buf, &mut i, d as u64);
    core::str::from_utf8(&buf[..i]).unwrap_or("?")
}

/// Format the result number + a trailing unit label, into `buf`. Picks a digit
/// count that stays readable across the wide range of magnitudes we produce
/// (bytes -> GB, mm -> mi), then trims trailing zeros after the point.
fn fmt_result<'a>(v: f32, unit: &str, buf: &'a mut [u8; 24]) -> &'a str {
    let mut i = 0;

    let neg = v < 0.0;
    let mag = if neg { -v } else { v };

    if neg {
        push_byte(buf, &mut i, b'-');
    }

    // Choose decimal places by magnitude: big numbers get fewer, tiny get more.
    let places: u32 = if mag >= 1000.0 {
        2
    } else if mag >= 1.0 {
        3
    } else {
        6
    };

    // Scale, round to the chosen places, split into integer + fractional digits.
    let scale = libm::powf(10.0, places as f32);
    let scaled = libm::roundf(mag * scale);
    // Guard against overflow of the integer accumulation for absurd inputs.
    let scaled = if scaled > 4.0e9 { 4.0e9 } else { scaled };
    let scaled_u = scaled as u64;
    let div = scale as u64;
    let int_v = scaled_u / div;
    let mut frac_v = scaled_u % div;

    push_u64(buf, &mut i, int_v);

    if places > 0 && frac_v > 0 {
        push_byte(buf, &mut i, b'.');
        // Emit exactly `places` fractional digits, MSB first...
        let mut tmp = [0u8; 8];
        let mut p = places as usize;
        let mut k = p;
        while k > 0 {
            k -= 1;
            tmp[k] = b'0' + (frac_v % 10) as u8;
            frac_v /= 10;
        }
        // ...then trim trailing zeros for a clean reading.
        while p > 1 && tmp[p - 1] == b'0' {
            p -= 1;
        }
        for &b in &tmp[..p] {
            push_byte(buf, &mut i, b);
        }
    }

    // trailing space + unit
    push_byte(buf, &mut i, b' ');
    for &b in unit.as_bytes() {
        push_byte(buf, &mut i, b);
    }

    core::str::from_utf8(&buf[..i]).unwrap_or("?")
}

#[inline]
fn push_byte(buf: &mut [u8], i: &mut usize, b: u8) {
    if *i < buf.len() {
        buf[*i] = b;
        *i += 1;
    }
}

/// Append a u64's decimal digits to `buf` at `*i` (most-significant first).
fn push_u64(buf: &mut [u8], i: &mut usize, v: u64) {
    let mut tmp = [0u8; 20];
    let mut n = v;
    let mut c = 0;
    if n == 0 {
        push_byte(buf, i, b'0');
        return;
    }
    while n > 0 && c < tmp.len() {
        tmp[c] = b'0' + (n % 10) as u8;
        n /= 10;
        c += 1;
    }
    while c > 0 {
        c -= 1;
        push_byte(buf, i, tmp[c]);
    }
}
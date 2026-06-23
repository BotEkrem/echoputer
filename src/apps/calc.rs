//! Pocket calculator — immediate-execution model, like a cheap four-function
//! calculator. There is a running `value`, a `pending` operator, and a text
//! `entry` buffer being typed. Digits 0-9 and '.' append to the entry; an
//! operator key (+ - * /) commits the entry (`value = value OP entry`) and
//! latches the new operator; '=' / ENTER folds the entry into the value and
//! shows the result. Backspace trims the last entry char; 'c'/'C' clears all.
//!
//! Divide-by-zero is caught (shows "err" and locks until the next clear/digit)
//! so nothing panics. Integral results print without a trailing ".0".
//!
//! Resident state is tiny: a 15-byte ASCII entry buffer plus a couple of f32s,
//! so the whole struct lives comfortably on main's stack. exit() is a no-op.

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};

use crate::{hal::keymap, i18n, theme};

/// Max characters the user can type into one entry (digits + sign + dot).
const ENTRY_CAP: usize = 15;

/// Vertical centre of the big result readout.
const BIG_CY: i32 = 84;
/// Baseline (top) of the expression line, under the topbar.
const EXPR_Y: i32 = 30;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    fn sym(self) -> &'static str {
        match self {
            Op::Add => "+",
            Op::Sub => "-",
            Op::Mul => "*",
            Op::Div => "/",
        }
    }
    fn apply(self, a: f32, b: f32) -> Option<f32> {
        Some(match self {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            Op::Div => {
                if b == 0.0 {
                    return None;
                }
                a / b
            }
        })
    }
}

pub struct Calc {
    /// The committed running value (left operand of the pending op).
    value: f32,
    /// Operator awaiting its right operand; None until one is typed.
    pending: Option<Op>,
    /// ASCII characters typed for the operand currently being entered.
    entry: [u8; ENTRY_CAP],
    entry_len: usize,
    /// True right after '='/an operator commit: the next digit starts fresh
    /// rather than appending to a now-stale entry.
    fresh: bool,
    /// Divide-by-zero (or other) error latch — clears on 'c' or a new digit.
    error: bool,
}

impl Calc {
    pub fn new() -> Self {
        Calc {
            value: 0.0,
            pending: None,
            entry: [0u8; ENTRY_CAP],
            entry_len: 0,
            fresh: true,
            error: false,
        }
    }

    /// Wipe everything back to a powered-on "0".
    fn clear_all(&mut self) {
        self.value = 0.0;
        self.pending = None;
        self.entry_len = 0;
        self.fresh = true;
        self.error = false;
    }

    /// The entry buffer as a &str (ASCII only, always valid UTF-8).
    fn entry_str(&self) -> &str {
        core::str::from_utf8(&self.entry[..self.entry_len]).unwrap_or("")
    }

    /// Parse the current entry as f32. Empty / lone "-" / lone "." -> 0.0.
    fn entry_value(&self) -> f32 {
        parse_f32(self.entry_str())
    }

    /// Append one ASCII char to the entry, honouring caps and the "fresh" reset.
    fn push_char(&mut self, ch: u8) {
        // Coming off an '=' or operator: start a brand-new operand.
        if self.fresh {
            self.entry_len = 0;
            self.fresh = false;
            self.error = false;
        }
        match ch {
            b'0'..=b'9' => {
                if self.entry_len < ENTRY_CAP {
                    self.entry[self.entry_len] = ch;
                    self.entry_len += 1;
                }
            }
            b'.' => {
                // Only one dot per number; seed a leading 0 if dot comes first.
                if !self.entry_str().as_bytes().contains(&b'.') && self.entry_len < ENTRY_CAP {
                    if self.entry_len == 0 {
                        self.entry[self.entry_len] = b'0';
                        self.entry_len += 1;
                    }
                    if self.entry_len < ENTRY_CAP {
                        self.entry[self.entry_len] = b'.';
                        self.entry_len += 1;
                    }
                }
            }
            _ => {}
        }
    }

    /// Fold the typed entry into `value` using the pending operator. With no
    /// pending op the entry simply *becomes* the value. Returns false on error
    /// (e.g. divide by zero), leaving the error latch set.
    fn commit(&mut self) -> bool {
        let rhs = self.entry_value();
        match self.pending {
            None => {
                // First operand, or a bare number: it is the value.
                self.value = rhs;
                true
            }
            Some(op) => match op.apply(self.value, rhs) {
                Some(r) => {
                    self.value = r;
                    true
                }
                None => {
                    self.error = true;
                    false
                }
            },
        }
    }

    /// Operator key: commit what's typed, then latch the new operator so the
    /// next operand applies to the freshly-computed value.
    fn press_op(&mut self, op: Op) {
        if self.error {
            return; // locked until clear / new digit
        }
        // If nothing was typed since the last op, just swap the operator.
        if self.fresh && self.pending.is_some() {
            self.pending = Some(op);
            return;
        }
        if self.commit() {
            self.pending = Some(op);
            self.entry_len = 0;
            self.fresh = true;
        }
    }

    /// '=' / ENTER: evaluate, show the result, drop the pending operator.
    fn press_equals(&mut self) {
        if self.error {
            return;
        }
        if self.commit() {
            self.pending = None;
            self.entry_len = 0;
            self.fresh = true;
        }
    }

    fn backspace(&mut self) {
        if self.error || self.fresh {
            return;
        }
        if self.entry_len > 0 {
            self.entry_len -= 1;
        }
    }

    // ---- public interface (called by main.rs) ----

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        self.clear_all();
        theme::clear(d);
        theme::topbar(d, i18n::t("Calculator", "Hesap Makinesi"));
        self.draw_body(d);
        theme::hint(
            d,
            i18n::t("0-9 . + - * /  = eval  bksp  c clear", "0-9 . + - * /  = hesapla  bksp  c sil"),
        );
    }

    pub fn on_key(&mut self, rc: (u8, u8), d: &mut impl DrawTarget<Color = Rgb565>) {
        // ENTER acts as '='.
        if rc == crate::K_ENTER {
            self.press_equals();
            self.draw_body(d);
            return;
        }
        if rc == keymap::K_BKSP {
            self.backspace();
            self.draw_body(d);
            return;
        }

        // Everything else routes through the typed-character map.
        if let Some(ch) = keymap::ch_shift(rc.0, rc.1, false) {
            match ch {
                b'0'..=b'9' | b'.' => self.push_char(ch),
                b'+' => self.press_op(Op::Add),
                b'-' => self.press_op(Op::Sub),
                // '*' is reachable shifted; also accept 'x'/'X' as a friendly alias.
                b'*' | b'x' | b'X' => self.press_op(Op::Mul),
                b'/' => self.press_op(Op::Div),
                b'=' => self.press_equals(),
                b'c' | b'C' => self.clear_all(),
                _ => return, // ignore unrelated keys (no redraw)
            }
            self.draw_body(d);
        }
    }

    /// No animation — the display only changes on key presses.
    pub fn tick(&mut self, _d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        false
    }

    /// No heap to release.
    pub fn exit(&mut self) {}

    // ---- drawing ----

    fn draw_body(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        // Erase the whole content band (between topbar and hint), then repaint.
        theme::fill(
            d,
            0,
            theme::TOPBAR_Y + 1,
            theme::W as u32,
            (theme::HINT_Y - theme::TOPBAR_Y - 3) as u32,
            theme::BG,
        );
        self.draw_expr(d);
        self.draw_result(d);
    }

    /// The running expression line: "value OP entry" (whatever applies), in muted
    /// type just under the topbar.
    fn draw_expr(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        let mut buf = [0u8; 48];
        let mut i = 0usize;

        // Left side: the committed running value.
        let mut vb = [0u8; 24];
        let vs = fmt_f32(self.value, &mut vb);
        push_str(&mut buf, &mut i, vs);

        if let Some(op) = self.pending {
            push_str(&mut buf, &mut i, " ");
            push_str(&mut buf, &mut i, op.sym());
            push_str(&mut buf, &mut i, " ");
            // Show the operand-in-progress if any has been typed.
            if !self.fresh && self.entry_len > 0 {
                push_str(&mut buf, &mut i, self.entry_str());
            }
        }

        let s = core::str::from_utf8(&buf[..i]).unwrap_or("");
        theme::text_right(d, s, theme::W - theme::PAD, EXPR_Y, theme::BODY_FONT, theme::MUTED);
    }

    /// The big result/entry readout. Shows the live entry while typing, otherwise
    /// the running value — or "err" when the error latch is set.
    fn draw_result(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        let mut buf = [0u8; 24];
        let (s, col): (&str, Rgb565) = if self.error {
            (i18n::t("err", "hata"), theme::DESTRUCTIVE)
        } else if !self.fresh && self.entry_len > 0 {
            (self.entry_str(), theme::FG)
        } else {
            (fmt_f32(self.value, &mut buf), theme::FG)
        };

        theme::text_right(d, s, theme::W - theme::PAD, BIG_CY, &FONT_10X20, col);

        // Accent underline spanning the readout width (matches the synth/stopwatch look).
        let uw = (s.len() as i32 * 11).clamp(0, theme::W - 2 * theme::PAD);
        theme::fill(
            d,
            theme::W - theme::PAD - uw,
            BIG_CY + 22,
            uw as u32,
            2,
            theme::accent(),
        );
    }
}

/// Append `s`'s bytes to `buf` at `*i`, stopping at the buffer's end.
fn push_str(buf: &mut [u8], i: &mut usize, s: &str) {
    for &b in s.as_bytes() {
        if *i < buf.len() {
            buf[*i] = b;
            *i += 1;
        }
    }
}

/// Parse a small decimal string ("-12.5", ".5", "-", "") to f32 without libm or
/// the (no_std-absent) str::parse float path. Lone "-"/"."/"" parse as 0.
fn parse_f32(s: &str) -> f32 {
    let b = s.as_bytes();
    let mut idx = 0;
    let neg = if !b.is_empty() && b[0] == b'-' {
        idx = 1;
        true
    } else {
        false
    };

    let mut int_part: f32 = 0.0;
    let mut frac_part: f32 = 0.0;
    let mut frac_scale: f32 = 1.0;
    let mut seen_dot = false;

    while idx < b.len() {
        let c = b[idx];
        if c == b'.' {
            seen_dot = true;
        } else if c.is_ascii_digit() {
            let dv = (c - b'0') as f32;
            if seen_dot {
                frac_scale *= 10.0;
                frac_part += dv / frac_scale;
            } else {
                int_part = int_part * 10.0 + dv;
            }
        }
        idx += 1;
    }

    let mag = int_part + frac_part;
    if neg {
        -mag
    } else {
        mag
    }
}

/// Format an f32 for the readout: drops a trailing ".0" for integral values,
/// otherwise prints up to six fractional digits with trailing zeros trimmed.
/// Non-finite values render as "err" here too.
fn fmt_f32(v: f32, buf: &mut [u8; 24]) -> &str {
    // Guard against NaN / inf so the formatter never loops or overflows.
    if !is_finite(v) {
        return "err";
    }

    let mut i = 0usize;
    let mut x = v;
    if x < 0.0 {
        push_byte(buf, &mut i, b'-');
        x = -x;
    }

    // Round to 6 decimal places to absorb float noise (e.g. 0.1+0.2).
    let scaled = round_half_up(x * 1_000_000.0);
    let int_part = scaled / 1_000_000;
    let frac = (scaled % 1_000_000) as u32; // 0..=999_999

    // Integer part, most-significant first.
    {
        // Count digits.
        let mut digits = 1u32;
        let mut t = int_part / 10;
        while t > 0 {
            digits += 1;
            t /= 10;
        }
        let mut scale = 1u64;
        for _ in 1..digits {
            scale = scale.saturating_mul(10);
        }
        while scale > 0 {
            let dgt = (int_part / scale) % 10;
            push_byte(buf, &mut i, b'0' + dgt as u8);
            scale /= 10;
        }
    }

    // Fractional part: only if non-zero. Trim trailing zeros.
    if frac > 0 {
        // Emit exactly 6 digits, then trim trailing '0's by rewinding `i`.
        let dot_pos = i;
        push_byte(buf, &mut i, b'.');
        let mut scale = 100_000u32;
        while scale > 0 {
            let dgt = (frac / scale) % 10;
            push_byte(buf, &mut i, b'0' + dgt as u8);
            scale /= 10;
        }
        // Rewind past trailing zeros (but keep at least one fractional digit).
        while i > dot_pos + 2 && buf[i - 1] == b'0' {
            i -= 1;
        }
    }

    core::str::from_utf8(&buf[..i]).unwrap_or("0")
}

#[inline]
fn push_byte(buf: &mut [u8; 24], i: &mut usize, b: u8) {
    if *i < buf.len() {
        buf[*i] = b;
        *i += 1;
    }
}

/// Round a non-negative f32 to the nearest integer (half away from zero), as u64.
#[inline]
fn round_half_up(x: f32) -> u64 {
    libm::roundf(x) as u64
}

/// Cheap finite check via the IEEE-754 exponent field (exp == 0xFF => inf/NaN).
#[inline]
fn is_finite(v: f32) -> bool {
    let bits = v.to_bits();
    let exp = (bits >> 23) & 0xFF;
    exp != 0xFF
}
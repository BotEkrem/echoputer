//! Stopwatch + countdown timer. No wall clock — the Cardputer ADV has no RTC
//! battery, so time-of-day is intentionally omitted; everything here is relative
//! elapsed time measured from the monotonic [`Instant`] counter.
//!
//! Two modes (LEFT/RIGHT switch): STOPWATCH counts up from 0, TIMER counts down
//! from a target the user nudges in 10 s steps while paused. ENTER runs/pauses;
//! 'r' resets. The big MM:SS.cs readout only repaints when its visible value
//! actually changes, so the idle (paused) screen costs nothing per frame.

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};

use esp_hal::time::Instant;

use crate::{i18n, theme};
use crate::i18n::stopwatch;

/// 'r' key (row1 col4 on the silkscreen) — reset. Documented in the hint line.
const K_RESET: (u8, u8) = (1, 4);

/// Timer adjustment step while paused: +/- 10 s.
const STEP_MS: u64 = 10_000;

/// Vertical centre of the big readout.
const BIG_CY: i32 = 70;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Stopwatch,
    Timer,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Stopwatch => i18n::t(stopwatch::STOPWATCH_UPPER),
            Mode::Timer => i18n::t(stopwatch::TIMER_UPPER),
        }
    }
}

pub struct Stopwatch {
    mode: Mode,
    /// Time folded in from previous run segments, in ms.
    accumulated: u64,
    /// Set while running; None while paused. elapsed = accumulated + start.elapsed().
    start: Option<Instant>,
    /// TIMER target in ms (adjusted in STEP_MS steps while paused).
    target_ms: u64,
    /// TIMER finished its countdown -> DONE state.
    done: bool,
    /// Last whole value painted in the big readout, so tick() only redraws on change.
    last_shown_ms: u64,
}

impl Stopwatch {
    pub fn new() -> Self {
        Stopwatch {
            mode: Mode::Stopwatch,
            accumulated: 0,
            start: None,
            target_ms: 60_000, // sensible default: 1:00 countdown
            done: false,
            last_shown_ms: u64::MAX, // force first paint
        }
    }

    fn running(&self) -> bool {
        self.start.is_some()
    }

    /// Raw elapsed run time in ms (accumulated + the live segment).
    fn elapsed_ms(&self) -> u64 {
        let live = self.start.map(|s| s.elapsed().as_millis()).unwrap_or(0);
        self.accumulated + live
    }

    /// The number the big readout shows, in ms: counts up (stopwatch) or down to
    /// zero (timer). Saturates at 0 so the countdown never wraps.
    fn display_ms(&self) -> u64 {
        match self.mode {
            Mode::Stopwatch => self.elapsed_ms(),
            Mode::Timer => self.target_ms.saturating_sub(self.elapsed_ms()),
        }
    }

    /// Fold the live segment into `accumulated` and stop the clock.
    fn pause(&mut self) {
        if let Some(s) = self.start.take() {
            self.accumulated += s.elapsed().as_millis();
        }
    }

    fn reset(&mut self) {
        self.start = None;
        self.accumulated = 0;
        self.done = false;
    }

    // ---- public interface (called by main.rs) ----

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, i18n::t(stopwatch::STOPWATCH));
        self.last_shown_ms = u64::MAX; // force the value redraw below
        self.draw_mode(d);
        self.draw_value(d);
        self.draw_hint(d);
    }

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) {
        match rc {
            crate::K_LEFT | crate::K_RIGHT => {
                // Switch mode. Pause and reset the run so the two clocks don't
                // share a stale elapsed count.
                self.pause();
                self.reset();
                self.mode = match self.mode {
                    Mode::Stopwatch => Mode::Timer,
                    Mode::Timer => Mode::Stopwatch,
                };
                self.draw_mode(d);
                self.draw_value(d);
                self.draw_hint(d);
            }
            crate::K_ENTER => {
                if self.running() {
                    self.pause();
                } else if !(self.mode == Mode::Timer && (self.done || self.target_ms == 0)) {
                    // Resume/start. Don't start a finished or zero-length timer.
                    self.done = false;
                    self.start = Some(Instant::now());
                }
                self.draw_value(d);
                self.draw_hint(d);
            }
            crate::K_UP | crate::K_DOWN if self.mode == Mode::Timer && !self.running() => {
                if rc == crate::K_UP {
                    self.target_ms = self.target_ms.saturating_add(STEP_MS);
                } else {
                    self.target_ms = self.target_ms.saturating_sub(STEP_MS);
                }
                self.done = false;
                self.draw_value(d);
            }
            K_RESET => {
                self.reset();
                self.draw_value(d);
                self.draw_hint(d);
            }
            _ => {}
        }
    }

    /// Called ~every 40 ms. Returns true ONLY when the framebuffer changed, so a
    /// paused clock costs the main loop nothing.
    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        if !self.running() {
            return false;
        }
        // TIMER: stop the instant the countdown hits zero and show DONE.
        if self.mode == Mode::Timer && self.elapsed_ms() >= self.target_ms {
            self.pause();
            self.done = true;
            self.draw_value(d);
            self.draw_hint(d);
            return true;
        }
        // STOPWATCH / running TIMER: repaint only when the visible cs changed.
        if cs_repr(self.display_ms()) != cs_repr(self.last_shown_ms) {
            self.draw_value(d);
            return true;
        }
        false
    }

    // ---- drawing ----

    /// Mode name (left) under the top bar.
    fn draw_mode<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::fill(d, 0, theme::TOPBAR_Y + 3, theme::W as u32, 12, theme::BG);
        theme::text(d, self.mode.name(), theme::PAD, theme::TOPBAR_Y + 4, theme::BODY_FONT, theme::MUTED);
        // Run/idle state on the right of the same row.
        let (label, col) = if self.done {
            (i18n::t(stopwatch::DONE), theme::accent())
        } else if self.running() {
            (i18n::t(stopwatch::RUNNING), theme::accent())
        } else {
            (i18n::t(stopwatch::PAUSED), theme::FAINT)
        };
        theme::text_right(d, label, theme::W - theme::PAD, theme::TOPBAR_Y + 4, theme::BODY_FONT, col);
    }

    /// The big centred MM:SS.cs readout (plus the live state on its row).
    fn draw_value<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        // Erase the big band and the state row, then repaint both.
        theme::fill(d, 0, BIG_CY - 16, theme::W as u32, 32, theme::BG);
        self.draw_mode(d);

        let ms = self.display_ms();
        self.last_shown_ms = ms;

        let mut buf = [0u8; 12];
        let s = fmt_clock(ms, &mut buf);
        let col = if self.done {
            theme::accent()
        } else if self.mode == Mode::Timer && !self.running() && ms <= 10_000 && ms > 0 {
            // about to expire: nudge attention without driving the LED (main owns it)
            theme::accent()
        } else {
            theme::FG
        };
        theme::text_center(d, s, theme::W / 2, BIG_CY, &FONT_10X20, col);

        // Underline accent, matching the synth note style.
        let uw = s.len() as i32 * 11;
        theme::fill(d, theme::W / 2 - uw / 2, BIG_CY + 13, uw as u32, 2, theme::accent());
    }

    fn draw_hint<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let s = if self.mode == Mode::Timer && !self.running() {
            // paused TIMER: surface the +/-10 s adjust keys
            i18n::t(stopwatch::HINT_TIMER_PAUSED)
        } else {
            i18n::t(stopwatch::HINT_DEFAULT)
        };
        theme::hint(d, s);
    }
}

/// Centisecond bucket of an ms value — used to decide whether the readout's
/// visible text would change (so we don't repaint on sub-cs jitter).
#[inline]
fn cs_repr(ms: u64) -> u64 {
    ms / 10
}

/// Format ms as MM:SS.cs (centiseconds). Minutes are not clamped; a stopwatch
/// past 99 minutes simply shows more digits.
fn fmt_clock(ms: u64, buf: &mut [u8; 12]) -> &str {
    let cs = (ms / 10) % 100;
    let total_s = ms / 1000;
    let secs = total_s % 60;
    let mins = total_s / 60;

    let mut i = 0;
    push_u64(buf, &mut i, mins, 2); // minutes, at least two digits
    push_byte(buf, &mut i, b':');
    push_u64(buf, &mut i, secs, 2);
    push_byte(buf, &mut i, b'.');
    push_u64(buf, &mut i, cs, 2);

    core::str::from_utf8(&buf[..i]).unwrap_or("00:00.00")
}

/// Append `v`'s decimal digits to `buf` at `*i`, zero-padded to at least
/// `min_width` digits (most-significant first).
fn push_u64(buf: &mut [u8; 12], i: &mut usize, v: u64, min_width: usize) {
    // Count digits (at least one, for v == 0).
    let mut digits = 1;
    let mut n = v / 10;
    while n > 0 {
        digits += 1;
        n /= 10;
    }
    for _ in digits..min_width {
        push_byte(buf, i, b'0');
    }
    // Highest power of ten that fits, then walk down emitting each digit.
    let mut scale = 1u64;
    for _ in 1..digits {
        scale *= 10;
    }
    while scale > 0 {
        let d = (v / scale) % 10;
        push_byte(buf, i, b'0' + d as u8);
        scale /= 10;
    }
}

#[inline]
fn push_byte(buf: &mut [u8; 12], i: &mut usize, b: u8) {
    if *i < buf.len() {
        buf[*i] = b;
        *i += 1;
    }
}

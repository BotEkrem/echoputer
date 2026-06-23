//! Conway's Game of Life — a cellular-automaton sim drawn into the framebuffer.
//!
//! An 80x32 toroidal grid of 3x3-px cells fills the band under the topbar. Two
//! bit-packed generations (current + next) plus a "shown" buffer (what's on the
//! panel right now) are heap-`Box`ed — allocated in `enter()`, freed in `exit()` —
//! so nothing big sits on the tight, no-PSRAM main stack frame. `tick()` advances
//! one generation every ~120 ms with wrap-around, then repaints ONLY the cells
//! that differ from `shown`.
//!
//! Controls (on_key): ENTER / space = pause-resume; `s` = single step (paused);
//! `r` = reseed random; arrows move an edit cursor; `t` (or backspace) toggles the
//! cell under the cursor. The G0 button is routed to `help()`, a rules overlay
//! that also pauses the sim.
//!
//! Self-contained: a small LCG (snake-style) supplies the seeding randomness —
//! no RNG crate, no float math.

use alloc::boxed::Box;
use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use esp_hal::time::{Duration, Instant};

use crate::hal::keymap;
use crate::{i18n, theme};

// ---- grid geometry: 3x3-px square cells, integer-fitting the content band ----
const CELL: i32 = 3;
const COLS: usize = 80; // 80 * 3 = 240 px wide (full width)
const ROWS: usize = 32; // 32 * 3 = 96 px tall
const BOARD_TOP: i32 = 22; // just under the topbar divider (TOPBAR_Y = 17)
const BOARD_LEFT: i32 = 0; // 80*3 == 240 == theme::W, so no horizontal offset

const N_CELLS: usize = COLS * ROWS; // 2560
const N_BYTES: usize = N_CELLS.div_ceil(8); // 320 bytes per bit-packed generation

// ---- step cadence ----
const STEP_MS: u64 = 120;

/// A bit-packed COLS*ROWS grid (one bit per cell, row-major). Heap-boxed.
type Grid = [u8; N_BYTES];

#[inline]
fn get(g: &Grid, x: usize, y: usize) -> bool {
    let i = y * COLS + x;
    g[i >> 3] & (1 << (i & 7)) != 0
}

#[inline]
fn set(g: &mut Grid, x: usize, y: usize, on: bool) {
    let i = y * COLS + x;
    let m = 1u8 << (i & 7);
    if on {
        g[i >> 3] |= m;
    } else {
        g[i >> 3] &= !m;
    }
}

pub struct Life {
    cur: Option<Box<Grid>>,   // live generation
    next: Option<Box<Grid>>,  // scratch for the next generation
    shown: Option<Box<Grid>>, // what is currently painted on the panel (for diffing)
    paused: bool,
    showing_help: bool,
    // edit cursor
    cx: usize,
    cy: usize,
    prev_cx: usize, // cursor position last frame, to clear a stale marker
    prev_cy: usize,
    was_paused: bool, // run/pause state to restore after the help overlay
    rng: u32,
    last_step: Instant,
    gen: u32,
}

impl Life {
    pub fn new() -> Self {
        Life {
            cur: None,
            next: None,
            shown: None,
            paused: false,
            showing_help: false,
            cx: COLS / 2,
            cy: ROWS / 2,
            prev_cx: COLS / 2,
            prev_cy: ROWS / 2,
            was_paused: false,
            rng: 0x1357_9BDF, // fixed seed; stirred by time + keys
            last_step: Instant::now(),
            gen: 0,
        }
    }

    /// Advance the LCG and return the new state.
    fn rand(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.rng
    }

    /// Fill `cur` with a pseudo-random pattern (~25-35% alive) and clear `shown`.
    fn reseed(&mut self) {
        // Stir the seed off the wall clock so each reseed differs.
        self.rng ^= Instant::now().duration_since_epoch().as_micros() as u32;
        if let Some(g) = self.cur.as_mut() {
            for b in g.iter_mut() {
                // Two LCG draws per byte; AND of two ~50% fields -> ~25-35% set.
                let a = (self.rng >> 13) as u8;
                self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let c = (self.rng >> 17) as u8;
                self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = a & c;
            }
        }
        if let Some(s) = self.shown.as_mut() {
            s.fill(0xFF); // force a full repaint on the next draw (impossible state)
        }
        self.gen = 0;
    }

    // ---- pixel helpers ----

    fn cell_px(x: usize, y: usize) -> (i32, i32) {
        (BOARD_LEFT + x as i32 * CELL, BOARD_TOP + y as i32 * CELL)
    }

    fn paint_cell<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: usize, y: usize, alive: bool, cursor: bool) {
        let (px, py) = Self::cell_px(x, y);
        let col = if alive {
            theme::accent()
        } else if cursor {
            theme::SURFACE2
        } else {
            theme::BG
        };
        theme::fill(d, px, py, CELL as u32, CELL as u32, col);
    }

    /// Repaint every cell that differs from `shown`, then sync `shown` to `cur`.
    /// Also repaints the cell under the edit cursor (when paused) as a marker.
    fn draw_diff<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        let mark = self.paused; // only show the edit cursor while paused
        let mut changed = false;
        let cur = match self.cur.as_ref() {
            Some(c) => c.as_ref(),
            None => return false,
        };
        for y in 0..ROWS {
            for x in 0..COLS {
                let alive = get(cur, x, y);
                let is_cursor = mark && x == self.cx && y == self.cy;
                let want_alive = alive;
                let shown_alive = {
                    let s = self.shown.as_ref().unwrap();
                    get(s, x, y)
                };
                // Redraw when the alive-state changed, when a cursor marker is
                // needed here, or to clear a stale marker from last frame.
                let needs = want_alive != shown_alive || is_cursor || self.cursor_stale(x, y);
                if needs {
                    Self::paint_cell(d, x, y, alive, is_cursor);
                    changed = true;
                }
            }
        }
        // Sync shown to cur (alive bits only); the cursor marker is transient.
        if changed {
            let cur_copy = *cur; // 320-byte stack copy — cheap and bounded
            let s = self.shown.as_mut().unwrap();
            **s = cur_copy;
        }
        // Remember where the cursor was so we can clear it next frame.
        self.prev_cx = self.cx;
        self.prev_cy = self.cy;
        changed
    }

    /// True if (x,y) was the cursor last frame but isn't now — needs a repaint to
    /// clear the stale marker.
    fn cursor_stale(&self, x: usize, y: usize) -> bool {
        self.paused && x == self.prev_cx && y == self.prev_cy && !(x == self.cx && y == self.cy)
    }

    /// One Life generation with toroidal (wrap-around) neighbour counting.
    fn step_gen(&mut self) {
        let (cur, next) = match (self.cur.as_ref(), self.next.as_mut()) {
            (Some(c), Some(n)) => (c.as_ref(), n.as_mut()),
            _ => return,
        };
        for y in 0..ROWS {
            let yu = if y == 0 { ROWS - 1 } else { y - 1 };
            let yd = if y == ROWS - 1 { 0 } else { y + 1 };
            for x in 0..COLS {
                let xl = if x == 0 { COLS - 1 } else { x - 1 };
                let xr = if x == COLS - 1 { 0 } else { x + 1 };
                let n = get(cur, xl, yu) as u8
                    + get(cur, x, yu) as u8
                    + get(cur, xr, yu) as u8
                    + get(cur, xl, y) as u8
                    + get(cur, xr, y) as u8
                    + get(cur, xl, yd) as u8
                    + get(cur, x, yd) as u8
                    + get(cur, xr, yd) as u8;
                let alive = get(cur, x, y);
                // Live with 2-3 neighbours survives; dead with exactly 3 is born.
                let born = (alive && (n == 2 || n == 3)) || (!alive && n == 3);
                set(next, x, y, born);
            }
        }
        // Swap cur <-> next (just swap the Box pointers).
        core::mem::swap(&mut self.cur, &mut self.next);
        self.gen = self.gen.wrapping_add(1);
    }

    /// Run/pause label + generation counter, drawn in the topbar band (the board
    /// uses the full width below, so there's no room for a status line there).
    fn draw_status<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::fill(d, 96, 3, 110, 13, theme::BG);
        let label = if self.paused {
            i18n::t("PAUSE", "DURDU")
        } else {
            i18n::t("RUN", "CALIS")
        };
        theme::text(d, label, 100, 4, theme::BODY_FONT, theme::accent());
        let mut buf = [0u8; 12];
        let s = fmt_u32(self.gen, &mut buf);
        theme::text_right(d, s, theme::W - 56, 4, theme::BODY_FONT, theme::MUTED);
    }

    fn hint_line<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::hint(
            d,
            i18n::t(
                "enter:pause s:step r:reseed",
                "enter:dur s:adim r:yeniden",
            ),
        );
    }

    /// Full board paint (used on enter and after dismissing help): clear the band,
    /// reset `shown` to all-dead so live cells repaint, then diff-draw.
    fn full_draw<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        theme::fill(d, BOARD_LEFT, BOARD_TOP, (COLS as i32 * CELL) as u32, (ROWS as i32 * CELL) as u32, theme::BG);
        if let Some(s) = self.shown.as_mut() {
            s.fill(0); // background is already cleared, so shown==all-dead matches
        }
        self.prev_cx = self.cx;
        self.prev_cy = self.cy;
        let _ = self.draw_diff(d);
        self.draw_status(d);
    }

    // ---- public interface (called by main.rs) ----

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        // Allocate the three grids on the heap (mirrors chip8.rs boxing its 4 KB).
        self.cur = Some(Box::new([0u8; N_BYTES]));
        self.next = Some(Box::new([0u8; N_BYTES]));
        self.shown = Some(Box::new([0u8; N_BYTES]));
        self.paused = false;
        self.showing_help = false;
        self.cx = COLS / 2;
        self.cy = ROWS / 2;
        self.prev_cx = self.cx;
        self.prev_cy = self.cy;
        self.reseed();
        self.last_step = Instant::now();

        theme::clear(d);
        theme::topbar(d, i18n::t("Life", "Yasam"));
        self.full_draw(d);
        self.hint_line(d);
    }

    /// Free the heap grids when leaving the app.
    pub fn exit(&mut self) {
        self.cur = None;
        self.next = None;
        self.shown = None;
    }

    pub fn on_key(&mut self, rc: (u8, u8), d: &mut impl DrawTarget<Color = Rgb565>) {
        let _ = self.rand(); // stir on every key

        // While the help overlay is up, any key dismisses it.
        if self.showing_help {
            self.help(d);
            return;
        }
        if self.cur.is_none() {
            return;
        }

        // ENTER or space toggles pause/resume.
        if rc == crate::K_ENTER || keymap::ch_shift(rc.0, rc.1, false) == Some(b' ') {
            self.paused = !self.paused;
            self.last_step = Instant::now();
            self.draw_status(d);
            let _ = self.draw_diff(d); // show/hide the edit cursor
            return;
        }

        // Arrow cluster moves the edit cursor (the marker only renders when paused).
        match rc {
            crate::K_UP => {
                self.cy = if self.cy == 0 { ROWS - 1 } else { self.cy - 1 };
                let _ = self.draw_diff(d);
                return;
            }
            crate::K_DOWN => {
                self.cy = if self.cy == ROWS - 1 { 0 } else { self.cy + 1 };
                let _ = self.draw_diff(d);
                return;
            }
            crate::K_LEFT => {
                self.cx = if self.cx == 0 { COLS - 1 } else { self.cx - 1 };
                let _ = self.draw_diff(d);
                return;
            }
            crate::K_RIGHT => {
                self.cx = if self.cx == COLS - 1 { 0 } else { self.cx + 1 };
                let _ = self.draw_diff(d);
                return;
            }
            _ => {}
        }

        // Backspace toggles the cell under the cursor (alongside 't').
        if rc == keymap::K_BKSP {
            self.toggle_under_cursor(d);
            return;
        }

        // Letter commands.
        match keymap::ch_shift(rc.0, rc.1, false) {
            Some(b'r') => {
                self.reseed();
                let _ = self.draw_diff(d);
                self.draw_status(d);
            }
            Some(b's') => {
                // Single step (most useful while paused, but allowed anytime).
                self.step_gen();
                let _ = self.draw_diff(d);
                self.draw_status(d);
            }
            Some(b't') => self.toggle_under_cursor(d),
            _ => {}
        }
    }

    fn toggle_under_cursor(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        if let Some(g) = self.cur.as_mut() {
            let on = get(g, self.cx, self.cy);
            set(g, self.cx, self.cy, !on);
        }
        let _ = self.draw_diff(d);
    }

    pub fn tick(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        // Keep the LCG churning when idle so seeding stays unpredictable.
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);

        if self.showing_help || self.paused || self.cur.is_none() {
            return false;
        }
        if self.last_step.elapsed() < Duration::from_millis(STEP_MS) {
            return false; // not time yet — leave the framebuffer alone
        }
        self.last_step = Instant::now();

        self.step_gen();
        let _ = self.draw_diff(d);
        self.draw_status(d);
        true // we advanced a generation and repainted the status line
    }

    /// G0 button: toggle a rules-explanation overlay. First call draws the rules
    /// over the board and pauses; second call dismisses it and restores state.
    pub fn help(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        if !self.showing_help {
            self.showing_help = true;
            self.was_paused = self.paused;
            self.paused = true;

            // A calm card over the board with the three rules.
            theme::card(d, 8, BOARD_TOP, (theme::W - 16) as u32, 96, Some(theme::accent()));
            theme::text(
                d,
                i18n::t("Game of Life", "Yasam Oyunu"),
                18,
                BOARD_TOP + 6,
                theme::TITLE_FONT,
                theme::FG,
            );
            theme::text(
                d,
                i18n::t("Live cell, 2-3 neighbours:", "Canli, 2-3 komsu:"),
                18,
                BOARD_TOP + 24,
                theme::BODY_FONT,
                theme::MUTED,
            );
            theme::text(
                d,
                i18n::t("  survives.", "  yasar."),
                18,
                BOARD_TOP + 36,
                theme::BODY_FONT,
                theme::FG,
            );
            theme::text(
                d,
                i18n::t("Dead cell, exactly 3:", "Olu, tam 3 komsu:"),
                18,
                BOARD_TOP + 50,
                theme::BODY_FONT,
                theme::MUTED,
            );
            theme::text(
                d,
                i18n::t("  is born.", "  dogar."),
                18,
                BOARD_TOP + 62,
                theme::BODY_FONT,
                theme::FG,
            );
            theme::text(
                d,
                i18n::t("Else: it dies.", "Yoksa: olur."),
                18,
                BOARD_TOP + 76,
                theme::BODY_FONT,
                theme::MUTED,
            );
            theme::hint(d, i18n::t("any key / G0: close", "tus / G0: kapat"));
        } else {
            // Dismiss: redraw the board, restore the prior run/pause state.
            self.showing_help = false;
            self.paused = self.was_paused;
            self.last_step = Instant::now();
            theme::clear(d);
            theme::topbar(d, i18n::t("Life", "Yasam"));
            self.full_draw(d);
            self.hint_line(d);
        }
    }
}

/// `u32` -> decimal, into `buf`. Returns the slice as &str.
fn fmt_u32(v: u32, buf: &mut [u8; 12]) -> &str {
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
    while i > 0 {
        i -= 1;
        buf[j] = tmp[i];
        j += 1;
    }
    core::str::from_utf8(&buf[..j]).unwrap_or("0")
}
//! Tetris — the falling-blocks classic. A 10x16 well sits centred under the
//! topbar; the seven standard tetrominoes drift down one row per gravity step
//! (driven by `tick`), steered with the arrow cluster. Clear full rows for
//! score; speed creeps up with every line. Stack to the ceiling and it's game
//! over — any key then starts a fresh game.
//!
//! Self-contained: a small LCG (advanced on every tick and key) supplies the
//! piece bag — no RNG crate, no float math. Rotation states are precomputed as
//! const 4x4 bit masks; a one-cell wall-kick rescues edge rotations.

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};

use esp_hal::time::{Duration, Instant};

use crate::{i18n, palette, theme};

// ---- well geometry (square cells; the well is an integer number of them) ----
const CELL: i32 = 6;
const COLS: i32 = 10;
const ROWS: i32 = 16;
const WELL_W: i32 = COLS * CELL; // 60
const WELL_H: i32 = ROWS * CELL; // 96
const BOARD_TOP: i32 = 20; // just under the topbar divider (TOPBAR_Y = 17)
// Centre the well horizontally; leave a 1px border all round.
const WELL_LEFT: i32 = (theme::W - WELL_W) / 2; // (240 - 60) / 2 = 90

// ---- gravity cadence: fast floor as lines accumulate ----
const DROP_START_MS: u64 = 500;
const DROP_FLOOR_MS: u64 = 120;

// 7 tetromino types. Indices double as the per-type colour-wheel slot.
const PIECE_COUNT: usize = 7;

// Each tetromino is a set of rotation states; each state is a 16-bit mask over a
// 4x4 grid (bit = row*4 + col, row 0 = top, col 0 = left). I/S/Z keep two
// effective rotations but we store four so indexing is uniform; O is one shape
// repeated. Masks chosen so spawned pieces sit in the top rows.
const PIECES: [[u16; 4]; PIECE_COUNT] = [
    // I
    [0x0F00, 0x2222, 0x00F0, 0x4444],
    // O
    [0x6600, 0x6600, 0x6600, 0x6600],
    // T
    [0x4E00, 0x4640, 0x0E40, 0x4C40],
    // S
    [0x6C00, 0x4620, 0x06C0, 0x8C40],
    // Z
    [0xC600, 0x2640, 0x0C60, 0x4C80],
    // J
    [0x8E00, 0x6440, 0x0E20, 0x44C0],
    // L
    [0x2E00, 0x4460, 0x0E80, 0xC440],
];

/// Cell colour for piece type `t`: spread the seven across the hue wheel so they
/// read distinctly against the neutral well. Slots picked to avoid clustering.
fn piece_color(t: u8) -> Rgb565 {
    // 0:I 1:O 2:T 3:S 4:Z 5:J 6:L mapped onto the 16-slot wheel.
    const SLOT: [usize; PIECE_COUNT] = [8, 4, 11, 6, 0, 13, 2];
    palette::wheel(SLOT[(t as usize) % PIECE_COUNT])
}

pub struct Tetris {
    // The settled stack: 0 = empty, else 1 + piece type (so colour survives).
    well: [[u8; COLS as usize]; ROWS as usize],
    // Active piece.
    cur: u8,    // piece type
    rot: u8,    // rotation state 0..4
    px: i32,    // top-left of the 4x4 box, in cells (may be negative)
    py: i32,
    next: u8,   // queued next piece type
    score: u16,
    lines: u16,
    over: bool,
    rng: u32,
    last_step: Instant,
}

impl Tetris {
    pub fn new() -> Self {
        let mut g = Tetris {
            well: [[0; COLS as usize]; ROWS as usize],
            cur: 0,
            rot: 0,
            px: 0,
            py: 0,
            next: 0,
            score: 0,
            lines: 0,
            over: false,
            rng: 0x2468_ACE1, // fixed seed; advanced on every tick + key
            last_step: Instant::now(),
        };
        g.reset();
        g
    }

    /// Lay out a fresh game: empty well, pick a piece + a queued next one.
    fn reset(&mut self) {
        self.well = [[0; COLS as usize]; ROWS as usize];
        self.score = 0;
        self.lines = 0;
        self.over = false;
        self.next = self.rand_piece();
        self.spawn(); // sets cur/rot/px/py from `next`, refills `next`
        self.last_step = Instant::now();
    }

    /// Advance the LCG and return the new state (state = state*1664525 + 1013904223).
    fn rand(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.rng
    }

    /// Pseudo-random piece type 0..PIECE_COUNT.
    fn rand_piece(&mut self) -> u8 {
        (self.rand() % PIECE_COUNT as u32) as u8
    }

    /// Pull `next` into the active slot, refill `next`, place at the top-centre.
    /// Sets `over` if the spawned piece already collides.
    fn spawn(&mut self) {
        self.cur = self.next;
        self.next = self.rand_piece();
        self.rot = 0;
        self.px = (COLS - 4) / 2; // 4-wide box centred -> 3
        self.py = -1; // start one row above so the empty top mask rows hide it
        // Nudge down to the first row where any filled cell is visible (py such
        // that the lowest mask row sits at/under row 0), then test collision.
        if self.collides(self.cur, self.rot, self.px, self.py) {
            // Try at py 0 in case of the -1 offset clashing.
            if self.collides(self.cur, self.rot, self.px, 0) {
                self.over = true;
            } else {
                self.py = 0;
            }
        }
    }

    /// The 16-bit mask for piece `t` at rotation `r`.
    #[inline]
    fn mask(t: u8, r: u8) -> u16 {
        PIECES[(t as usize) % PIECE_COUNT][(r as usize) % 4]
    }

    /// Would piece `t`@`r` placed with its 4x4 box at (`ox`,`oy`) overlap a wall,
    /// the floor, or a settled cell? Cells above the ceiling (y < 0) are allowed
    /// so a piece can spawn/rotate partly off the top.
    fn collides(&self, t: u8, r: u8, ox: i32, oy: i32) -> bool {
        let m = Self::mask(t, r);
        for i in 0..16 {
            if m & (0x8000 >> i) == 0 {
                continue;
            }
            let cx = ox + (i % 4);
            let cy = oy + (i / 4);
            if cx < 0 || cx >= COLS || cy >= ROWS {
                return true;
            }
            if cy >= 0 && self.well[cy as usize][cx as usize] != 0 {
                return true;
            }
        }
        false
    }

    /// Stamp the active piece into the well (called on lock).
    fn lock(&mut self) {
        let m = Self::mask(self.cur, self.rot);
        for i in 0..16 {
            if m & (0x8000 >> i) == 0 {
                continue;
            }
            let cx = self.px + (i % 4);
            let cy = self.py + (i / 4);
            if cy >= 0 && cy < ROWS && cx >= 0 && cx < COLS {
                self.well[cy as usize][cx as usize] = self.cur + 1;
            }
        }
    }

    /// Remove any full rows, shifting everything above down. Returns the count.
    fn clear_lines(&mut self) -> u32 {
        let mut cleared = 0u32;
        let mut row = ROWS - 1;
        while row >= 0 {
            let full = self.well[row as usize].iter().all(|&c| c != 0);
            if full {
                // Shift every row above `row` down by one.
                let mut r = row;
                while r > 0 {
                    self.well[r as usize] = self.well[(r - 1) as usize];
                    r -= 1;
                }
                self.well[0] = [0; COLS as usize];
                cleared += 1;
                // Re-test the same row index (it now holds what was above).
            } else {
                row -= 1;
            }
        }
        cleared
    }

    /// Current gravity interval: faster with every cleared line, floored.
    fn step_ms(&self) -> u64 {
        DROP_START_MS
            .saturating_sub(self.lines as u64 * 18)
            .max(DROP_FLOOR_MS)
    }

    /// Try to rotate the active piece, with a simple +/-1 cell wall-kick.
    fn try_rotate(&mut self) -> bool {
        let nr = (self.rot + 1) % 4;
        if !self.collides(self.cur, nr, self.px, self.py) {
            self.rot = nr;
            return true;
        }
        // Kick right, then left.
        for dx in [1, -1, 2, -2] {
            if !self.collides(self.cur, nr, self.px + dx, self.py) {
                self.px += dx;
                self.rot = nr;
                return true;
            }
        }
        false
    }

    /// Try to shift the piece by `dx`; returns whether it moved.
    fn try_move(&mut self, dx: i32) -> bool {
        if !self.collides(self.cur, self.rot, self.px + dx, self.py) {
            self.px += dx;
            true
        } else {
            false
        }
    }

    /// Step the piece down one row. Returns false if it landed (couldn't fall).
    fn step_down(&mut self) -> bool {
        if !self.collides(self.cur, self.rot, self.px, self.py + 1) {
            self.py += 1;
            true
        } else {
            false
        }
    }

    /// Land the current piece: lock, clear lines, score, spawn the next. Sets
    /// `over` if the freshly spawned piece collides.
    fn land(&mut self) {
        self.lock();
        let cleared = self.clear_lines();
        if cleared > 0 {
            // Classic-ish scoring: 1->40, 2->100, 3->300, 4->1200.
            let gain: u16 = match cleared {
                1 => 40,
                2 => 100,
                3 => 300,
                _ => 1200,
            };
            self.score = self.score.saturating_add(gain);
            self.lines = self.lines.saturating_add(cleared as u16);
        } else {
            self.score = self.score.saturating_add(1); // small reward per lock
        }
        self.spawn();
    }

    // ---- pixel helpers ----

    #[inline]
    fn cell_x(cx: i32) -> i32 {
        WELL_LEFT + cx * CELL
    }
    #[inline]
    fn cell_y(cy: i32) -> i32 {
        BOARD_TOP + cy * CELL
    }

    /// Paint one well cell, 1px gap so blocks read as a grid.
    fn draw_block<D: DrawTarget<Color = Rgb565>>(d: &mut D, cx: i32, cy: i32, col: Rgb565) {
        theme::fill(
            d,
            Self::cell_x(cx) + 1,
            Self::cell_y(cy) + 1,
            (CELL - 1) as u32,
            (CELL - 1) as u32,
            col,
        );
    }

    /// Score + lines, drawn in the top-bar band between the title and battery.
    fn draw_hud<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        // erase previous values (battery occupies the far right ~52px)
        theme::fill(d, 56, 3, 110, 13, theme::BG);
        let mut sb = [0u8; 8];
        let s = fmt_u16(self.score, &mut sb);
        theme::text(d, i18n::t("SC", "SK"), 58, 4, theme::BODY_FONT, theme::MUTED);
        theme::text(d, s, 58 + 3 * 6, 4, theme::BODY_FONT, theme::accent());
        let mut lb = [0u8; 8];
        let l = fmt_u16(self.lines, &mut lb);
        theme::text(d, i18n::t("LN", "SR"), 120, 4, theme::BODY_FONT, theme::MUTED);
        theme::text(d, l, 120 + 3 * 6, 4, theme::BODY_FONT, theme::FG);
    }

    /// Draw the 1px well border (outside the playable cells).
    fn draw_frame<D: DrawTarget<Color = Rgb565>>(d: &mut D) {
        // top, bottom, left, right hairlines just outside the cell area
        let x0 = WELL_LEFT - 1;
        let y0 = BOARD_TOP - 1;
        let w = (WELL_W + 2) as u32;
        let h = (WELL_H + 2) as u32;
        theme::fill(d, x0, y0, w, 1, theme::BORDER_HI);
        theme::fill(d, x0, y0 + h as i32 - 1, w, 1, theme::BORDER_HI);
        theme::fill(d, x0, y0, 1, h, theme::BORDER_HI);
        theme::fill(d, x0 + w as i32 - 1, y0, 1, h, theme::BORDER_HI);
    }

    /// Full repaint of the well: clear the play area, draw the settled stack,
    /// then the active piece on top.
    fn draw_well<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::fill(d, WELL_LEFT, BOARD_TOP, WELL_W as u32, WELL_H as u32, theme::BG);
        // settled cells
        for cy in 0..ROWS {
            for cx in 0..COLS {
                let v = self.well[cy as usize][cx as usize];
                if v != 0 {
                    Self::draw_block(d, cx, cy, piece_color(v - 1));
                }
            }
        }
        // active piece
        if !self.over {
            let col = piece_color(self.cur);
            let m = Self::mask(self.cur, self.rot);
            for i in 0..16 {
                if m & (0x8000 >> i) == 0 {
                    continue;
                }
                let cx = self.px + (i % 4);
                let cy = self.py + (i / 4);
                if cy >= 0 && cy < ROWS && cx >= 0 && cx < COLS {
                    Self::draw_block(d, cx, cy, col);
                }
            }
        }
    }

    /// Game-over overlay: score + restart prompt, centred on the well.
    fn draw_over<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let cy = BOARD_TOP + WELL_H / 2;
        theme::card(d, 36, cy - 30, (theme::W - 72) as u32, 56, Some(theme::accent()));
        theme::text_center(
            d,
            i18n::t("GAME OVER", "OYUN BITTI"),
            theme::W / 2,
            cy - 12,
            &FONT_10X20,
            theme::FG,
        );
        let mut buf = [0u8; 16];
        let s = fmt_score_line(self.score, &mut buf);
        theme::text_center(d, s, theme::W / 2, cy + 8, theme::BODY_FONT, theme::accent());
        theme::hint(d, i18n::t("any key: play again", "herhangi tus: tekrar"));
    }

    /// Repaint the dynamic surface (well + frame + hud) without touching topbar.
    fn redraw<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        self.draw_well(d);
        Self::draw_frame(d);
        self.draw_hud(d);
    }

    // ---- public interface (called by main.rs) ----

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.reset();
        theme::clear(d);
        theme::topbar(d, i18n::t("Tetris", "Tetris"));
        self.redraw(d);
        theme::hint(d, i18n::t("rot up  drop enter", "don ust  birak enter"));
    }

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) {
        // Stir the RNG on every key so the bag isn't purely tick-timed.
        let _ = self.rand();

        if self.over {
            // Any key restarts a fresh game.
            self.reset();
            theme::clear(d);
            theme::topbar(d, i18n::t("Tetris", "Tetris"));
            self.redraw(d);
            theme::hint(d, i18n::t("rot up  drop enter", "don ust  birak enter"));
            return;
        }

        let mut changed = false;
        match rc {
            crate::K_LEFT => changed = self.try_move(-1),
            crate::K_RIGHT => changed = self.try_move(1),
            crate::K_UP => changed = self.try_rotate(),
            crate::K_DOWN => {
                // Soft drop: one row, reset the gravity clock so it doesn't
                // immediately step again.
                if self.step_down() {
                    self.last_step = Instant::now();
                    changed = true;
                } else {
                    self.land();
                    changed = true;
                }
            }
            crate::K_ENTER => {
                // Hard drop: fall to the bottom, then lock.
                while self.step_down() {}
                self.land();
                changed = true;
            }
            _ => {}
        }

        if changed {
            if self.over {
                self.draw_over(d);
            } else {
                self.redraw(d);
            }
        }
    }

    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        // Keep the LCG churning even when idle so timing seeds the randomness.
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);

        if self.over {
            return false; // frozen until a key restarts
        }
        if self.last_step.elapsed() < Duration::from_millis(self.step_ms()) {
            return false; // not time to fall yet — leave the framebuffer alone
        }
        self.last_step = Instant::now();

        if self.step_down() {
            // The piece moved down a row; repaint the well.
            self.redraw(d);
        } else {
            // Couldn't fall -> land it (lock, clear lines, spawn next).
            self.land();
            if self.over {
                self.draw_over(d);
            } else {
                self.redraw(d);
            }
        }
        true
    }
}

/// `u16` -> decimal, into `buf`. Returns the slice as &str.
fn fmt_u16(v: u16, buf: &mut [u8; 8]) -> &str {
    let mut tmp = [0u8; 5];
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

/// "score N" on the game-over card (ASCII only, bilingual prefix).
fn fmt_score_line(v: u16, buf: &mut [u8; 16]) -> &str {
    let prefix = i18n::t("score ", "skor ").as_bytes();
    let mut j = 0;
    for &b in prefix {
        if j < buf.len() {
            buf[j] = b;
            j += 1;
        }
    }
    let mut nb = [0u8; 8];
    let n = fmt_u16(v, &mut nb);
    for &b in n.as_bytes() {
        if j < buf.len() {
            buf[j] = b;
            j += 1;
        }
    }
    core::str::from_utf8(&buf[..j]).unwrap_or("score 0")
}

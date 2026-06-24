//! Snake — the classic. A grid fills the area under the topbar; the snake walks
//! one cell per ~150 ms step (driven by `tick`), steered with the arrow cluster.
//! Eat the food to grow and score; hit a wall or yourself and it's game over —
//! any key then starts a fresh game. Speed creeps up as the score climbs.
//!
//! Self-contained: a small LCG (advanced on every tick and key) supplies the
//! pseudo-randomness for food placement — no RNG crate, no float trig.

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};

use esp_hal::time::{Duration, Instant};

use crate::{i18n, theme};

// ---- grid geometry (cells are square; the board is an integer number of them) ----
const CELL: i32 = 6;
const BOARD_TOP: i32 = 20; // just under the topbar divider (TOPBAR_Y = 17)
const COLS: i32 = theme::W / CELL; // 240 / 6 = 40
const ROWS: i32 = (theme::HINT_Y - BOARD_TOP) / CELL; // (123 - 20) / 6 = 17
const BOARD_W: i32 = COLS * CELL; // 240
const BOARD_H: i32 = ROWS * CELL; // 102
const BOARD_LEFT: i32 = (theme::W - BOARD_W) / 2; // 0
// The snake can at most fill every cell.
const MAX_LEN: usize = (COLS * ROWS) as usize;

// ---- step cadence: fast-start to a floor as the score grows ----
const STEP_START_MS: u64 = 150;
const STEP_FLOOR_MS: u64 = 70;

// Food colour: a fixed bright red, distinct from the per-app accent (snake) and
// the neutral grayscale surfaces. Full red + a touch of green so it reads warm
// against any accent hue on the 16-bit panel.
const FOOD: Rgb565 = Rgb565::new(31, 10, 6);

// A grid cell as (col, row); both fit a u8 comfortably (max 40 / 17).
type Cell = (u8, u8);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    /// Reject a 180° reversal (you can't fold straight back onto your neck).
    fn opposite(self, other: Dir) -> bool {
        matches!(
            (self, other),
            (Dir::Up, Dir::Down)
                | (Dir::Down, Dir::Up)
                | (Dir::Left, Dir::Right)
                | (Dir::Right, Dir::Left)
        )
    }
}

pub struct Snake {
    // Body cells stored head-first in `body[..len]`; body[0] is the head.
    body: [Cell; MAX_LEN],
    len: usize,
    dir: Dir,
    // Latched steering for the *next* step, so two taps within one step don't
    // let you reverse through your own neck mid-cell.
    next_dir: Dir,
    food: Cell,
    score: u16,
    over: bool,
    rng: u32,
    last_step: Instant,
}

impl Snake {
    pub fn new() -> Self {
        let mut s = Snake {
            body: [(0, 0); MAX_LEN],
            len: 0,
            dir: Dir::Right,
            next_dir: Dir::Right,
            food: (0, 0),
            score: 0,
            over: false,
            rng: 0x1357_9BDF, // fixed seed; advanced on every tick + key
            last_step: Instant::now(),
        };
        s.reset();
        s
    }

    /// Lay out a fresh game: a 3-cell snake mid-board heading right, first food.
    fn reset(&mut self) {
        let cy = (ROWS / 2) as u8;
        let cx = (COLS / 2) as u8;
        // Head first, so body[0] is the rightmost cell.
        self.body[0] = (cx, cy);
        self.body[1] = (cx - 1, cy);
        self.body[2] = (cx - 2, cy);
        self.len = 3;
        self.dir = Dir::Right;
        self.next_dir = Dir::Right;
        self.score = 0;
        self.over = false;
        self.place_food();
        self.last_step = Instant::now();
    }

    /// Advance the LCG and return the new state (state = state*1664525 + 1013904223).
    fn rand(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.rng
    }

    /// Drop food on a pseudo-random free cell. With the board nearly full this
    /// scans forward from a random start for the first empty cell.
    fn place_food(&mut self) {
        let total = (COLS * ROWS) as u32;
        let start = self.rand() % total;
        for off in 0..total {
            let idx = (start + off) % total;
            let c = ((idx % COLS as u32) as u8, (idx / COLS as u32) as u8);
            if !self.occupied(c) {
                self.food = c;
                return;
            }
        }
        // Board completely full (a win, effectively) — leave food on the head.
        self.food = self.body[0];
    }

    /// Is `c` part of the snake body?
    fn occupied(&self, c: Cell) -> bool {
        self.body[..self.len].iter().any(|&b| b == c)
    }

    /// Current step interval: shorten by 4 ms per food eaten, floored.
    fn step_ms(&self) -> u64 {
        STEP_START_MS
            .saturating_sub(self.score as u64 * 4)
            .max(STEP_FLOOR_MS)
    }

    // ---- pixel helpers ----

    fn cell_x(c: Cell) -> i32 {
        BOARD_LEFT + c.0 as i32 * CELL
    }
    fn cell_y(c: Cell) -> i32 {
        BOARD_TOP + c.1 as i32 * CELL
    }

    /// Paint a single cell, leaving a 1px gap so segments read as a chain.
    fn draw_cell<D: DrawTarget<Color = Rgb565>>(d: &mut D, c: Cell, col: Rgb565) {
        theme::fill(d, Self::cell_x(c) + 1, Self::cell_y(c) + 1, (CELL - 1) as u32, (CELL - 1) as u32, col);
    }

    fn clear_cell<D: DrawTarget<Color = Rgb565>>(d: &mut D, c: Cell) {
        theme::fill(d, Self::cell_x(c), Self::cell_y(c), CELL as u32, CELL as u32, theme::BG);
    }

    /// Score, drawn in the top-bar band between the title and the battery
    /// indicator (the board uses the full width below, so there's no room for a
    /// score line down there).
    fn draw_score<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let mut buf = [0u8; 8];
        let s = fmt_u16(self.score, &mut buf);
        // erase previous value (battery occupies the far right ~52px)
        theme::fill(d, 70, 3, 90, 13, theme::BG);
        theme::text(d, i18n::t("SCORE", "SKOR"), 74, 4, theme::BODY_FONT, theme::MUTED);
        theme::text(d, s, 74 + 6 * 6, 4, theme::BODY_FONT, theme::accent());
    }

    /// Full board paint: frame the playfield, draw snake + food + score.
    fn draw_board<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        // playfield background (one fill, then the pieces on top)
        theme::fill(d, BOARD_LEFT, BOARD_TOP, BOARD_W as u32, BOARD_H as u32, theme::BG);
        Self::draw_cell(d, self.food, FOOD);
        for &c in &self.body[..self.len] {
            Self::draw_cell(d, c, theme::accent());
        }
        self.draw_score(d);
    }

    /// Game-over overlay: score + restart prompt, centred on the board.
    fn draw_over<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let cy = BOARD_TOP + BOARD_H / 2;
        // a calm card behind the text so the snake doesn't bleed through
        theme::card(d, 36, cy - 30, (theme::W - 72) as u32, 56, Some(theme::accent()));
        theme::text_center(d, i18n::t("GAME OVER", "OYUN BITTI"), theme::W / 2, cy - 12, &FONT_10X20, theme::FG);
        let mut buf = [0u8; 16];
        let s = fmt_score_line(self.score, &mut buf);
        theme::text_center(d, s, theme::W / 2, cy + 8, theme::BODY_FONT, theme::accent());
        theme::hint(d, i18n::t("any key: play again", "herhangi tus: tekrar"));
    }

    // ---- public interface (called by main.rs) ----

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.reset();
        theme::clear(d);
        theme::topbar(d, i18n::t("Snake", "Snake"));
        self.draw_board(d);
        theme::hint(d, i18n::t("arrows: steer", "oklar: yon ver"));
    }

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) {
        // Stir the RNG on every key so food placement isn't deterministic.
        let _ = self.rand();

        if self.over {
            // Any key restarts a fresh game.
            self.reset();
            theme::clear(d);
            theme::topbar(d, i18n::t("Snake", "Snake"));
            self.draw_board(d);
            theme::hint(d, i18n::t("arrows: steer", "oklar: yon ver"));
            return;
        }

        // Steering only — latch into next_dir, applied at the next step. Reject a
        // direct reversal of the *current* heading.
        let want = match rc {
            crate::K_UP => Some(Dir::Up),
            crate::K_DOWN => Some(Dir::Down),
            crate::K_LEFT => Some(Dir::Left),
            crate::K_RIGHT => Some(Dir::Right),
            _ => None,
        };
        if let Some(dir) = want {
            if !self.dir.opposite(dir) {
                self.next_dir = dir;
            }
        }
        // No redraw here; the visible change happens on the next step in tick().
    }

    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        // Keep the LCG churning even when idle so timing seeds the randomness.
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);

        if self.over {
            return false; // frozen until a key restarts
        }
        if self.last_step.elapsed() < Duration::from_millis(self.step_ms()) {
            return false; // not time to move yet — leave the framebuffer alone
        }
        self.last_step = Instant::now();

        // Commit the latched steering for this step.
        self.dir = self.next_dir;

        // Compute the new head from the current one.
        let head = self.body[0];
        let (hx, hy) = (head.0 as i32, head.1 as i32);
        let (nx, ny) = match self.dir {
            Dir::Up => (hx, hy - 1),
            Dir::Down => (hx, hy + 1),
            Dir::Left => (hx - 1, hy),
            Dir::Right => (hx + 1, hy),
        };

        // Wall collision -> death.
        if nx < 0 || ny < 0 || nx >= COLS || ny >= ROWS {
            self.over = true;
            self.draw_over(d);
            return true;
        }
        let new_head = (nx as u8, ny as u8);

        let ate = new_head == self.food;
        // Self-collision: hitting any body cell except the tail we're about to
        // vacate (only safe to step onto when we're NOT growing).
        let tail = self.body[self.len - 1];
        let hits_self = self.body[..self.len]
            .iter()
            .enumerate()
            .any(|(i, &b)| b == new_head && !(i == self.len - 1 && !ate));
        if hits_self {
            self.over = true;
            self.draw_over(d);
            return true;
        }

        if ate {
            // Grow: shift the whole body back by one and prepend the head.
            if self.len < MAX_LEN {
                self.body.copy_within(0..self.len, 1);
                self.len += 1;
            } else {
                // Board full: shift in place (no growth possible).
                self.body.copy_within(0..self.len - 1, 1);
            }
            self.body[0] = new_head;
            self.score = self.score.saturating_add(1);
            Self::draw_cell(d, new_head, theme::accent());
            self.place_food();
            Self::draw_cell(d, self.food, FOOD);
            self.draw_score(d);
        } else {
            // Move: erase the old tail, shift body back by one, prepend head.
            Self::clear_cell(d, tail);
            self.body.copy_within(0..self.len - 1, 1);
            self.body[0] = new_head;
            Self::draw_cell(d, new_head, theme::accent());
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

/// "score: N" on the game-over card (ASCII only, bilingual prefix).
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

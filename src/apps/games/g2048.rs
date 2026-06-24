//! 2048 — the sliding-tile classic. A 4x4 board of powers of two sits centred
//! under the topbar; the arrow cluster slides every tile in one direction,
//! merging equal neighbours once per move. Each move that changes the board
//! spawns a fresh tile (mostly a '2', sometimes a '4') and adds the merged
//! values to the score. Fill the board with no move left and it's game over —
//! any key then deals a new game.
//!
//! Self-contained and turn-based: a small LCG (advanced on every tick and key)
//! decides spawn position and value — no RNG crate, no floats. `tick` never
//! animates, so it returns false; it only churns the LCG for entropy.

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};

use esp_hal::time::Instant;

use crate::{i18n, palette, theme};

// ---- board geometry ----
const N: usize = 4; // 4x4
const CELL: i32 = 23; // tile side in px
const GAP: i32 = 2; // gap between tiles
const BOARD_PX: i32 = N as i32 * CELL + (N as i32 + 1) * GAP; // 4*23 + 5*2 = 102
const BOARD_LEFT: i32 = (theme::W - BOARD_PX) / 2; // (240-102)/2 = 69
const BOARD_TOP: i32 = 22; // under the topbar divider (TOPBAR_Y = 17)

/// Tile background for an empty slot (a faint raised surface).
const EMPTY_TILE: Rgb565 = theme::SURFACE2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

pub struct G2048 {
    // Exponents: 0 = empty, otherwise the tile value is 1 << exp (so 1 -> 2,
    // 2 -> 4, ... 11 -> 2048). Indexed [row][col].
    grid: [[u8; N]; N],
    score: u32,
    over: bool,
    rng: u32,
    // Held so `new()` matches Snake's shape; `tick` stirs it for entropy.
    last_tick: Instant,
}

impl G2048 {
    pub fn new() -> Self {
        let mut g = G2048 {
            grid: [[0; N]; N],
            score: 0,
            over: false,
            rng: 0x2048_ACE1, // fixed seed; advanced on every tick + key
            last_tick: Instant::now(),
        };
        g.reset();
        g
    }

    /// Lay out a fresh game: clear the board and deal two starting tiles.
    fn reset(&mut self) {
        self.grid = [[0; N]; N];
        self.score = 0;
        self.over = false;
        self.spawn();
        self.spawn();
        self.last_tick = Instant::now();
    }

    /// Advance the LCG and return the new state (state = state*1664525 + 1013904223).
    fn rand(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.rng
    }

    /// Drop a tile on a pseudo-random empty cell: 90% a '2' (exp 1), 10% a '4'
    /// (exp 2). No-op when the board is full.
    fn spawn(&mut self) {
        let mut empties = 0u32;
        for r in 0..N {
            for c in 0..N {
                if self.grid[r][c] == 0 {
                    empties += 1;
                }
            }
        }
        if empties == 0 {
            return;
        }
        let pick = self.rand() % empties; // which empty cell
        let val = if self.rand() % 10 == 0 { 2 } else { 1 }; // 10% -> exp 2 ('4')
        let mut seen = 0u32;
        for r in 0..N {
            for c in 0..N {
                if self.grid[r][c] == 0 {
                    if seen == pick {
                        self.grid[r][c] = val;
                        return;
                    }
                    seen += 1;
                }
            }
        }
    }

    /// Slide + merge a single line toward index 0 (the low end). Returns the new
    /// line and the score gained from merges this line. A tile merges at most
    /// once per move (standard 2048).
    fn slide_line(line: [u8; N]) -> ([u8; N], u32) {
        // Compact non-empty tiles toward the front.
        let mut packed = [0u8; N];
        let mut p = 0;
        for &v in line.iter() {
            if v != 0 {
                packed[p] = v;
                p += 1;
            }
        }
        // Merge equal adjacent pairs once, left to right.
        let mut out = [0u8; N];
        let mut o = 0;
        let mut gained = 0u32;
        let mut i = 0;
        while i < p {
            if i + 1 < p && packed[i] == packed[i + 1] {
                let merged = packed[i] + 1; // exp+1 doubles the value
                out[o] = merged;
                gained += 1u32 << merged; // the resulting tile value
                o += 1;
                i += 2;
            } else {
                out[o] = packed[i];
                o += 1;
                i += 1;
            }
        }
        (out, gained)
    }

    /// Apply a move. Returns true if the board changed. Lines are read in the
    /// move direction so the shared `slide_line` (which packs toward index 0)
    /// works for all four directions.
    fn slide(&mut self, dir: Dir) -> bool {
        let mut changed = false;
        let old = self.grid;
        match dir {
            Dir::Left => {
                for r in 0..N {
                    let (line, g) = Self::slide_line(self.grid[r]);
                    self.grid[r] = line;
                    self.score += g;
                }
            }
            Dir::Right => {
                for r in 0..N {
                    let mut rev = self.grid[r];
                    rev.reverse();
                    let (mut line, g) = Self::slide_line(rev);
                    line.reverse();
                    self.grid[r] = line;
                    self.score += g;
                }
            }
            Dir::Up => {
                for c in 0..N {
                    let col = [self.grid[0][c], self.grid[1][c], self.grid[2][c], self.grid[3][c]];
                    let (line, g) = Self::slide_line(col);
                    for r in 0..N {
                        self.grid[r][c] = line[r];
                    }
                    self.score += g;
                }
            }
            Dir::Down => {
                for c in 0..N {
                    let mut col = [self.grid[0][c], self.grid[1][c], self.grid[2][c], self.grid[3][c]];
                    col.reverse();
                    let (mut line, g) = Self::slide_line(col);
                    line.reverse();
                    for r in 0..N {
                        self.grid[r][c] = line[r];
                    }
                    self.score += g;
                }
            }
        }
        for r in 0..N {
            for c in 0..N {
                if old[r][c] != self.grid[r][c] {
                    changed = true;
                }
            }
        }
        changed
    }

    /// Is any move still possible? (an empty cell, or two equal neighbours).
    fn can_move(&self) -> bool {
        for r in 0..N {
            for c in 0..N {
                if self.grid[r][c] == 0 {
                    return true;
                }
                if c + 1 < N && self.grid[r][c] == self.grid[r][c + 1] {
                    return true;
                }
                if r + 1 < N && self.grid[r][c] == self.grid[r + 1][c] {
                    return true;
                }
            }
        }
        false
    }

    // ---- pixel helpers ----

    fn tile_x(c: usize) -> i32 {
        BOARD_LEFT + GAP + c as i32 * (CELL + GAP)
    }
    fn tile_y(r: usize) -> i32 {
        BOARD_TOP + GAP + r as i32 * (CELL + GAP)
    }

    /// Colour for a tile of the given exponent. Empty -> a faint surface;
    /// otherwise cycle the palette hue wheel by log2 so each value reads as a
    /// distinct tone, warming as the tiles grow.
    fn tile_colour(exp: u8) -> Rgb565 {
        if exp == 0 {
            return EMPTY_TILE;
        }
        // exp 1 ('2') starts a few slots into the wheel; step by 2 hues each
        // doubling so adjacent values contrast strongly.
        palette::wheel((exp as usize + 1) * 2)
    }

    /// Pick a legible text colour from the tile's actual luminance: dark text on
    /// bright fills, light text on dark fills — so the number never washes out
    /// (the bright wheel hues were swallowing the white numbers).
    fn tile_text_colour(exp: u8) -> Rgb565 {
        let c = Self::tile_colour(exp);
        // RGB565 channels (R,B: 0..31, G: 0..63) -> 0..255, then perceived luma.
        let r8 = c.r() as u32 * 255 / 31;
        let g8 = c.g() as u32 * 255 / 63;
        let b8 = c.b() as u32 * 255 / 31;
        let luma = (30 * r8 + 59 * g8 + 11 * b8) / 100; // 0..255
        if luma > 140 {
            theme::BG // dark text on a bright tile
        } else {
            theme::FG // light text on a dark tile
        }
    }

    /// Paint one tile (background + centred number).
    fn draw_tile<D: DrawTarget<Color = Rgb565>>(d: &mut D, r: usize, c: usize, exp: u8) {
        let x = Self::tile_x(c);
        let y = Self::tile_y(r);
        theme::fill(d, x, y, CELL as u32, CELL as u32, Self::tile_colour(exp));
        if exp != 0 {
            let mut buf = [0u8; 6];
            let s = fmt_pow2(exp, &mut buf);
            theme::text_center(
                d,
                s,
                x + CELL / 2,
                y + CELL / 2,
                theme::BODY_FONT,
                Self::tile_text_colour(exp),
            );
        }
    }

    /// Score, drawn in the top-bar band between the title and the battery
    /// indicator (the board fills the area below).
    fn draw_score<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let mut buf = [0u8; 12];
        let s = fmt_u32(self.score, &mut buf);
        // erase previous value (battery occupies the far right ~52px)
        theme::fill(d, 70, 3, 90, 13, theme::BG);
        theme::text(d, i18n::t("SCORE", "SKOR"), 74, 4, theme::BODY_FONT, theme::MUTED);
        theme::text(d, s, 74 + 6 * 6, 4, theme::BODY_FONT, theme::accent());
    }

    /// Full board paint: the board backplate, every tile, the score.
    fn draw_board<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        // board frame: a single fill behind the tiles supplies the gaps' colour
        theme::fill(d, BOARD_LEFT, BOARD_TOP, BOARD_PX as u32, BOARD_PX as u32, theme::BORDER);
        for r in 0..N {
            for c in 0..N {
                Self::draw_tile(d, r, c, self.grid[r][c]);
            }
        }
        self.draw_score(d);
    }

    /// Game-over overlay: score + restart prompt, centred on the board.
    fn draw_over<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let cy = BOARD_TOP + BOARD_PX / 2;
        theme::card(d, 36, cy - 30, (theme::W - 72) as u32, 56, Some(theme::accent()));
        theme::text_center(d, i18n::t("GAME OVER", "OYUN BITTI"), theme::W / 2, cy - 12, &FONT_10X20, theme::FG);
        let mut buf = [0u8; 20];
        let s = fmt_score_line(self.score, &mut buf);
        theme::text_center(d, s, theme::W / 2, cy + 8, theme::BODY_FONT, theme::accent());
        theme::hint(d, i18n::t("any key: play again", "herhangi tus: tekrar"));
    }

    // ---- public interface (called by main.rs) ----

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.reset();
        theme::clear(d);
        theme::topbar(d, i18n::t("2048", "2048"));
        self.draw_board(d);
        theme::hint(d, i18n::t("arrows: slide", "oklar: kaydir"));
    }

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) {
        // Stir the RNG on every key so spawns aren't deterministic.
        let _ = self.rand();

        if self.over {
            // Any key deals a fresh game.
            self.reset();
            theme::clear(d);
            theme::topbar(d, i18n::t("2048", "2048"));
            self.draw_board(d);
            theme::hint(d, i18n::t("arrows: slide", "oklar: kaydir"));
            return;
        }

        let dir = match rc {
            crate::K_UP => Some(Dir::Up),
            crate::K_DOWN => Some(Dir::Down),
            crate::K_LEFT => Some(Dir::Left),
            crate::K_RIGHT => Some(Dir::Right),
            _ => None,
        };
        let Some(dir) = dir else { return };

        if self.slide(dir) {
            self.spawn();
            self.draw_board(d);
            if !self.can_move() {
                self.over = true;
                self.draw_over(d);
            }
        }
    }

    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, _d: &mut D) -> bool {
        // Turn-based: never animates. Keep the LCG churning so spawn timing
        // seeds the randomness, mirroring the reference game.
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        false
    }
}

/// `1 << exp` -> decimal, into `buf`. Returns the slice as &str. exp is small
/// (<= ~16 here), so the value fits a u32 with room to spare.
fn fmt_pow2(exp: u8, buf: &mut [u8; 6]) -> &str {
    let v: u32 = 1u32 << exp;
    let mut nb = [0u8; 12];
    let s = fmt_u32(v, &mut nb);
    let mut j = 0;
    for &b in s.as_bytes() {
        if j < buf.len() {
            buf[j] = b;
            j += 1;
        }
    }
    core::str::from_utf8(&buf[..j]).unwrap_or("?")
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

/// "score N" on the game-over card (ASCII only, bilingual prefix).
fn fmt_score_line(v: u32, buf: &mut [u8; 20]) -> &str {
    let prefix = i18n::t("score ", "skor ").as_bytes();
    let mut j = 0;
    for &b in prefix {
        if j < buf.len() {
            buf[j] = b;
            j += 1;
        }
    }
    let mut nb = [0u8; 12];
    let n = fmt_u32(v, &mut nb);
    for &b in n.as_bytes() {
        if j < buf.len() {
            buf[j] = b;
            j += 1;
        }
    }
    core::str::from_utf8(&buf[..j]).unwrap_or("score 0")
}

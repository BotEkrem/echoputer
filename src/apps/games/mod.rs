//! Games — a sub-launcher that groups the arcade apps. The home menu opens this;
//! picking a game runs it, and G0/Backspace returns to the games list (then a
//! second press pops back to the home menu). Every game implements the same
//! `new/enter/on_key/tick` interface, so this just routes to the active one.

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

pub mod g2048;
pub mod pong;
pub mod snake;
pub mod tetris;

use crate::apps::games::g2048::G2048;
use crate::apps::games::pong::Pong;
use crate::apps::games::snake::Snake;
use crate::apps::games::tetris::Tetris;
use crate::i18n::games;
use crate::{i18n, theme};

/// Display names (language-neutral; arcade titles stay as-is). With the emulator
/// built in, "Game Boy" is the last entry — picking it hands off to the emulator
/// (which owns its own screen, library and save handling).
#[cfg(feature = "emu")]
const GAMES: [&str; 5] = ["Snake", "2048", "Tetris", "Pong", "Game Boy"];
#[cfg(not(feature = "emu"))]
const GAMES: [&str; 4] = ["Snake", "2048", "Tetris", "Pong"];
const TOP: i32 = 30;
const ROW_H: i32 = 20;

pub struct Games {
    sel: usize,
    active: Option<usize>, // None = the list; Some(i) = playing game i
    snake: Snake,
    g2048: G2048,
    tetris: Tetris,
    pong: Pong,
}

impl Games {
    pub fn new() -> Self {
        Games {
            sel: 0,
            active: None,
            snake: Snake::new(),
            g2048: G2048::new(),
            tetris: Tetris::new(),
            pong: Pong::new(),
        }
    }

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.active = None;
        self.draw_list(d);
    }

    /// G0/Backspace: in a game -> back to the list; in the list -> false (pop to
    /// the home menu).
    pub fn back<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        if self.active.is_some() {
            self.active = None;
            self.draw_list(d);
            true
        } else {
            false
        }
    }

    /// Returns true only when the user picked "Game Boy" — the caller then enters
    /// the emulator (it needs the SD volume + key releases the launcher can't give).
    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> bool {
        match self.active {
            None => match rc {
                crate::K_UP => {
                    if self.sel > 0 {
                        self.sel -= 1;
                        self.draw_list(d);
                    }
                }
                crate::K_DOWN => {
                    if self.sel + 1 < GAMES.len() {
                        self.sel += 1;
                        self.draw_list(d);
                    }
                }
                crate::K_ENTER => {
                    #[cfg(feature = "emu")]
                    if self.sel == GAMES.len() - 1 {
                        return true; // hand off to the emulator
                    }
                    self.active = Some(self.sel);
                    self.launch(self.sel, d);
                }
                _ => {}
            },
            Some(0) => self.snake.on_key(rc, d),
            Some(1) => self.g2048.on_key(rc, d),
            Some(2) => self.tetris.on_key(rc, d),
            Some(3) => self.pong.on_key(rc, d),
            _ => {}
        }
        false
    }

    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        match self.active {
            Some(0) => self.snake.tick(d),
            Some(1) => self.g2048.tick(d),
            Some(2) => self.tetris.tick(d),
            Some(3) => self.pong.tick(d),
            _ => false,
        }
    }

    fn launch<D: DrawTarget<Color = Rgb565>>(&mut self, i: usize, d: &mut D) {
        match i {
            0 => self.snake.enter(d),
            1 => self.g2048.enter(d),
            2 => self.tetris.enter(d),
            3 => self.pong.enter(d),
            _ => {}
        }
    }

    fn draw_list<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, i18n::t(games::GAMES));
        for (i, name) in GAMES.iter().enumerate() {
            let y = TOP + i as i32 * ROW_H;
            let selected = i == self.sel;
            let col = if selected { theme::accent() } else { theme::MUTED };
            if selected {
                theme::text(d, ">", theme::PAD, y, theme::TITLE_FONT, theme::accent());
            }
            theme::text(d, name, theme::PAD + 16, y, theme::TITLE_FONT, col);
        }
        theme::hint(d, i18n::t(games::HINT));
    }
}

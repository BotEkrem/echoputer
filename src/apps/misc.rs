//! Misc — a sub-launcher grouping the small extra apps (Chip-8 now; QR + TOTP next).
//! Mirrors the Games launcher: the home menu opens this, picking an item runs it, and
//! G0/Backspace returns to this list (a second press pops back to the home menu).
//! Items that touch the SD card take the volume manager through `on_key`.

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_sdmmc::{BlockDevice, TimeSource, VolumeManager};

use crate::apps::chip8::Chip8;
use crate::{i18n, theme};

const ITEMS: [&str; 1] = ["Chip-8"];
const TOP: i32 = 30;
const ROW_H: i32 = 20;

pub struct Misc {
    sel: usize,
    active: Option<usize>, // None = the list; Some(i) = running item i
    chip8: Chip8,
}

impl Misc {
    pub fn new() -> Self {
        Misc {
            sel: 0,
            active: None,
            chip8: Chip8::new(),
        }
    }

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.active = None;
        self.draw_list(d);
    }

    /// G0/Backspace: in an item -> back to the list (freeing its state); in the list
    /// -> false (the caller pops to the home menu).
    pub fn back<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        if self.active.is_some() {
            self.chip8.exit();
            self.active = None;
            self.draw_list(d);
            true
        } else {
            false
        }
    }

    /// Free any running item's heap state (used when jumping straight home with `).
    pub fn leave(&mut self) {
        self.chip8.exit();
        self.active = None;
    }

    pub fn on_key<D: BlockDevice, T: TimeSource>(
        &mut self,
        rc: (u8, u8),
        sd: &VolumeManager<D, T>,
        d: &mut impl DrawTarget<Color = Rgb565>,
    ) {
        match self.active {
            None => match rc {
                crate::K_UP => {
                    if self.sel > 0 {
                        self.sel -= 1;
                        self.draw_list(d);
                    }
                }
                crate::K_DOWN => {
                    if self.sel + 1 < ITEMS.len() {
                        self.sel += 1;
                        self.draw_list(d);
                    }
                }
                crate::K_ENTER => {
                    self.active = Some(self.sel);
                    match self.sel {
                        0 => self.chip8.enter(sd, d),
                        _ => {}
                    }
                }
                _ => {}
            },
            Some(0) => self.chip8.on_key(rc, d),
            _ => {}
        }
    }

    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        match self.active {
            Some(0) => self.chip8.tick(d),
            _ => false,
        }
    }

    fn draw_list<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Misc", "Diger"));
        for (i, name) in ITEMS.iter().enumerate() {
            let y = TOP + i as i32 * ROW_H;
            let selected = i == self.sel;
            let col = if selected { theme::accent() } else { theme::MUTED };
            if selected {
                theme::text(d, ">", theme::PAD, y, theme::TITLE_FONT, theme::accent());
            }
            theme::text(d, name, theme::PAD + 16, y, theme::TITLE_FONT, col);
        }
        theme::hint(d, i18n::t("UP/DN pick  ENTER open  ` menu", "YUK/AS sec  ENTER ac  ` menu"));
    }
}

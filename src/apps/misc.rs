//! Misc — a sub-launcher grouping the small extra apps. Mirrors the Games launcher:
//! the home menu opens this, picking an item runs it, and G0/Backspace returns to this
//! list (a second press pops to the home menu). Items that touch the SD card take the
//! volume manager through `on_key`; G0 is routed to the active item via [`Misc::g0`]
//! (Conway's Life uses it to toggle its rules overlay), everything else falls back to
//! "leave the item".

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_sdmmc::{BlockDevice, TimeSource, VolumeManager};

use crate::apps::calc::Calc;
use crate::apps::chip8::Chip8;
use crate::apps::convert::Convert;
use crate::apps::dice::Dice;
use crate::apps::life::Life;
use crate::apps::qr::Qr;
use crate::{i18n, theme};

const ITEMS: [&str; 6] = ["Chip-8", "Calc", "Convert", "Dice", "Life", "QR"];
const LIFE: usize = 4; // index of Conway's Life (G0 toggles its rules overlay)
const TOP: i32 = 30;
const ROW_H: i32 = 18;

pub struct Misc {
    sel: usize,
    active: Option<usize>, // None = the list; Some(i) = running item i
    chip8: Chip8,
    calc: Calc,
    convert: Convert,
    dice: Dice,
    life: Life,
    qr: Qr,
}

impl Misc {
    pub fn new() -> Self {
        Misc {
            sel: 0,
            active: None,
            chip8: Chip8::new(),
            calc: Calc::new(),
            convert: Convert::new(),
            dice: Dice::new(),
            life: Life::new(),
            qr: Qr::new(),
        }
    }

    // `inline(never)` on the dispatch methods keeps all six sub-apps' code in Misc's own
    // functions instead of inlining into the monolithic `main` — without it `.text.main`
    // overflows the Xtensa l32r literal-pool reach (~256 KB) and the link fails.
    #[inline(never)]
    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.active = None;
        self.draw_list(d);
    }

    /// Free the heap state of whatever item is running (Chip-8 / Life box buffers).
    fn exit_active(&mut self) {
        match self.active {
            Some(0) => self.chip8.exit(),
            Some(1) => self.calc.exit(),
            Some(2) => self.convert.exit(),
            Some(3) => self.dice.exit(),
            Some(4) => self.life.exit(),
            Some(5) => self.qr.exit(),
            _ => {}
        }
    }

    /// Backspace: in an item -> back to the list (freeing its state); in the list ->
    /// false (the caller pops to the home menu).
    pub fn back<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        if self.active.is_some() {
            self.exit_active();
            self.active = None;
            self.draw_list(d);
            true
        } else {
            false
        }
    }

    /// G0 button: Life toggles its rules overlay (stays in the app); any other running
    /// item leaves to the list; in the list, returns false so the caller goes home.
    pub fn g0<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        match self.active {
            Some(LIFE) => {
                self.life.help(d);
                true
            }
            Some(_) => self.back(d),
            None => false,
        }
    }

    /// Free any running item's heap state (used when jumping straight home with `).
    pub fn leave(&mut self) {
        self.exit_active();
        self.active = None;
    }

    #[inline(never)]
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
                    self.launch(self.sel, sd, d);
                }
                _ => {}
            },
            Some(0) => self.chip8.on_key(rc, d),
            Some(1) => self.calc.on_key(rc, d),
            Some(2) => self.convert.on_key(rc, d),
            Some(3) => self.dice.on_key(rc, d),
            Some(4) => self.life.on_key(rc, d),
            Some(5) => self.qr.on_key(rc, d),
            _ => {}
        }
    }

    #[inline(never)]
    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        match self.active {
            Some(0) => self.chip8.tick(d),
            Some(1) => self.calc.tick(d),
            Some(2) => self.convert.tick(d),
            Some(3) => self.dice.tick(d),
            Some(4) => self.life.tick(d),
            Some(5) => self.qr.tick(d),
            _ => false,
        }
    }

    fn launch<D: BlockDevice, T: TimeSource>(
        &mut self,
        i: usize,
        sd: &VolumeManager<D, T>,
        d: &mut impl DrawTarget<Color = Rgb565>,
    ) {
        match i {
            0 => self.chip8.enter(sd, d), // Chip-8 loads its ROM from the SD card
            1 => self.calc.enter(d),
            2 => self.convert.enter(d),
            3 => self.dice.enter(d),
            4 => self.life.enter(d),
            5 => self.qr.enter(d),
            _ => {}
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

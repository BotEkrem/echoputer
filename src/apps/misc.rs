//! Misc — a sub-launcher grouping the small extra apps. Mirrors the Games launcher:
//! the home menu opens this, picking an item runs it, and G0/Backspace returns to this
//! list (a second press pops to the home menu). Items that touch the SD card take the
//! volume manager through `on_key`.

use embedded_graphics::primitives::{PrimitiveStyle, Triangle};
use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_sdmmc::{BlockDevice, TimeSource, VolumeManager};

use crate::apps::calc::Calc;
use crate::apps::chip8::Chip8;
use crate::apps::convert::Convert;
use crate::apps::dice::Dice;
use crate::apps::qr::Qr;
use crate::{i18n, theme};

const ITEMS: [&str; 5] = ["Chip-8", "Calc", "Convert", "Dice", "QR"];
const TOP: i32 = 28;
const ROW_H: i32 = 18;
const VISIBLE: usize = 5; // list rows that fit between the topbar and the hint line

pub struct Misc {
    sel: usize,
    scroll: usize,         // top item shown when the list is longer than VISIBLE
    active: Option<usize>, // None = the list; Some(i) = running item i
    chip8: Chip8,
    calc: Calc,
    convert: Convert,
    dice: Dice,
    qr: Qr,
}

impl Misc {
    pub fn new() -> Self {
        Misc {
            sel: 0,
            scroll: 0,
            active: None,
            chip8: Chip8::new(),
            calc: Calc::new(),
            convert: Convert::new(),
            dice: Dice::new(),
            qr: Qr::new(),
        }
    }

    // `inline(never)` on the dispatch methods keeps the sub-apps' code in Misc's own
    // functions instead of inlining into the monolithic `main` — without it `.text.main`
    // overflows the Xtensa l32r literal-pool reach (~256 KB) and the link fails.
    #[inline(never)]
    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.active = None;
        self.clamp_scroll();
        self.draw_list(d);
    }

    /// Keep the selected row inside the visible window.
    fn clamp_scroll(&mut self) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + VISIBLE {
            self.scroll = self.sel + 1 - VISIBLE;
        }
    }

    /// Free the heap state of whatever item is running (Chip-8 box buffers, QR matrix).
    fn exit_active(&mut self) {
        match self.active {
            Some(0) => self.chip8.exit(),
            Some(1) => self.calc.exit(),
            Some(2) => self.convert.exit(),
            Some(3) => self.dice.exit(),
            Some(4) => self.qr.exit(),
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

    /// G0 button: same as Backspace here — leave a running item to the list, or pop
    /// to the home menu from the list.
    pub fn g0<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        self.back(d)
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
                        self.clamp_scroll();
                        self.draw_list(d);
                    }
                }
                crate::K_DOWN => {
                    if self.sel + 1 < ITEMS.len() {
                        self.sel += 1;
                        self.clamp_scroll();
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
            Some(4) => self.qr.on_key(rc, d),
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
            Some(4) => self.qr.tick(d),
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
            4 => self.qr.enter(d),
            _ => {}
        }
    }

    fn draw_list<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Misc", "Diger"));
        let n = ITEMS.len();
        // Only the VISIBLE-row window starting at `scroll` (the list can be longer
        // than the screen — without this the last items hid behind the hint line).
        for row in 0..VISIBLE {
            let i = self.scroll + row;
            if i >= n {
                break;
            }
            let y = TOP + row as i32 * ROW_H;
            let selected = i == self.sel;
            let col = if selected { theme::accent() } else { theme::MUTED };
            if selected {
                theme::text(d, ">", theme::PAD, y, theme::TITLE_FONT, theme::accent());
            }
            theme::text(d, ITEMS[i], theme::PAD + 16, y, theme::TITLE_FONT, col);
        }
        // Up/down scroll affordances when there's more above/below the window.
        let st = PrimitiveStyle::with_fill(theme::MUTED);
        if self.scroll > 0 {
            let _ = Triangle::new(Point::new(233, 25), Point::new(239, 25), Point::new(236, 20))
                .into_styled(st)
                .draw(d);
        }
        if self.scroll + VISIBLE < n {
            let yb = TOP + VISIBLE as i32 * ROW_H - 8;
            let _ = Triangle::new(Point::new(233, yb), Point::new(239, yb), Point::new(236, yb + 5))
                .into_styled(st)
                .draw(d);
        }
        theme::hint(d, i18n::t("UP/DN pick  ENTER open  ` menu", "YUK/AS sec  ENTER ac  ` menu"));
    }
}

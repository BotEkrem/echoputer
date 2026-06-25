//! Misc — a sub-launcher grouping the small extra apps. Mirrors the Games launcher:
//! the home menu opens this, picking an item runs it, and G0/Backspace returns to this
//! list (a second press pops to the home menu). Items that touch the SD card take the
//! volume manager through `on_key` / the leave paths; the IMU apps (Level, Steps) take
//! the shared I2C through `tick`.

use embedded_graphics::primitives::{PrimitiveStyle, Triangle};
use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_hal::i2c::I2c;
use embedded_sdmmc::{BlockDevice, TimeSource, VolumeManager};

pub mod calc;
pub mod chip8;
pub mod convert;
pub mod dice;
pub mod ir;
pub mod level;
pub mod qr;
pub mod qr_encode;
pub mod remote;
pub mod stepcount;

use crate::apps::misc::calc::Calc;
use crate::apps::misc::chip8::Chip8;
use crate::apps::misc::convert::Convert;
use crate::apps::misc::dice::Dice;
use crate::apps::misc::ir::Ir;
use crate::apps::misc::level::Level;
use crate::apps::misc::qr::Qr;
use crate::apps::misc::remote::Remote;
use crate::apps::misc::stepcount::StepCount;
use crate::hal::ir::IrTx;
use crate::hal::usb_hid::UsbParts;
use crate::i18n::misc;
use crate::{i18n, theme};

// Item indices are stable (Chip-8=0 .. Steps=7, Keyboard/Mouse=8). The dispatch keys
// off the REMOTE const below for the last item, never a literal.
const ITEMS: [&str; 9] = ["Chip-8", "Calc", "Convert", "Dice", "QR", "IR", "Level", "Steps", "Keyboard/Mouse"];

/// Index of the Remote item (always last).
const REMOTE: usize = ITEMS.len() - 1;

const TOP: i32 = 28;
const ROW_H: i32 = 18;
const VISIBLE: usize = 5; // list rows that fit between the topbar and the hint line

pub struct Misc {
    sel: usize,
    scroll: usize,
    active: Option<usize>,
    chip8: Chip8,
    calc: Calc,
    convert: Convert,
    dice: Dice,
    qr: Qr,
    ir: Ir,
    level: Level,
    stepcount: StepCount,
    remote: Remote,
}

impl Misc {
    pub fn new(ir_tx: IrTx, usb: UsbParts) -> Self {
        Misc {
            sel: 0,
            scroll: 0,
            active: None,
            chip8: Chip8::new(),
            calc: Calc::new(),
            convert: Convert::new(),
            dice: Dice::new(),
            qr: Qr::new(),
            ir: Ir::new(ir_tx),
            level: Level::new(),
            stepcount: StepCount::new(),
            remote: Remote::new(usb),
        }
    }

    #[inline(never)]
    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.active = None;
        self.clamp_scroll();
        self.draw_list(d);
    }

    fn clamp_scroll(&mut self) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + VISIBLE {
            self.scroll = self.sel + 1 - VISIBLE;
        }
    }

    /// Free / finalise whatever item is running. Takes the SD volume so item leave paths
    /// that need it can flush/close (none do today; kept for a uniform signature).
    fn exit_active<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>) {
        let _ = sd;
        match self.active {
            Some(0) => self.chip8.exit(),
            Some(1) => self.calc.exit(),
            Some(2) => self.convert.exit(),
            Some(3) => self.dice.exit(),
            Some(4) => self.qr.exit(),
            Some(5) => self.ir.exit(),
            Some(6) => self.level.exit(),
            Some(7) => self.stepcount.exit(),
            Some(REMOTE) => self.remote.exit(),
            _ => {}
        }
    }

    /// Backspace: in an item -> back to the list (finalising it); in the list -> false.
    pub fn back<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        if self.active.is_some() {
            self.exit_active(sd);
            self.active = None;
            self.draw_list(d);
            true
        } else {
            false
        }
    }

    /// G0 button: in the Remote app it toggles the connection type (USB/Bluetooth)
    /// and is consumed (stay in the app); everywhere else it behaves like Backspace
    /// (back to the list, then a second press pops to the home menu).
    pub fn g0<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        if self.active == Some(REMOTE) {
            self.remote.toggle_conn(d);
            return true;
        }
        self.back(sd, d)
    }

    /// True when an active input item (Calc/Convert/Dice/QR) needs Backspace to
    /// DELETE a character rather than exit — main exempts it from the global back.
    pub fn is_editing(&self) -> bool {
        // Calc/Convert/Dice/QR delete on Backspace; IR deletes a hex nibble only on its
        // Custom row; the Remote USB keyboard sends Backspace to the HOST — all need it
        // routed to on_key, not "go back".
        matches!(self.active, Some(1 | 2 | 3 | 4))
            || (self.active == Some(5) && self.ir.is_editing())
            || self.remote_typing()
    }

    /// "Aa" caps/shift toggle for the active item (only QR consumes it today).
    pub fn toggle_caps(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        if self.active == Some(4) {
            self.qr.toggle_caps(d);
        }
    }

    /// True in the Remote app's USB keyboard mode — main then suppresses key
    /// auto-repeat so a held key sends ONE HID report (no host-side spam).
    pub fn remote_typing(&self) -> bool {
        self.active == Some(REMOTE) && self.remote.is_typing()
    }

    /// Free/finalise any running item (used when jumping straight home with `).
    pub fn leave<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>) {
        self.exit_active(sd);
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
            Some(5) => self.ir.on_key(rc, d),
            Some(6) => self.level.on_key(rc, d),
            Some(7) => self.stepcount.on_key(rc, d),
            Some(REMOTE) => self.remote.on_key(rc, d),
            _ => {}
        }
    }

    /// `i2c` is the shared internal bus; the IMU apps (Level, Steps) read the BMI270
    /// through it here. The others ignore it.
    #[inline(never)]
    pub fn tick<I: I2c, D: DrawTarget<Color = Rgb565>>(&mut self, i2c: &mut I, d: &mut D) -> bool {
        match self.active {
            Some(0) => self.chip8.tick(d),
            Some(1) => self.calc.tick(d),
            Some(2) => self.convert.tick(d),
            Some(3) => self.dice.tick(d),
            Some(4) => self.qr.tick(d),
            Some(5) => self.ir.tick(d),
            Some(6) => self.level.tick(i2c, d),
            Some(7) => self.stepcount.tick(i2c, d),
            Some(REMOTE) => self.remote.tick(d),
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
            0 => self.chip8.enter(sd, d),
            1 => self.calc.enter(d),
            2 => self.convert.enter(d),
            3 => self.dice.enter(d),
            4 => self.qr.enter(d),
            5 => self.ir.enter(d),
            6 => self.level.enter(d),
            7 => self.stepcount.enter(d),
            REMOTE => self.remote.enter(d),
            _ => {}
        }
    }

    fn draw_list<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, i18n::t(misc::MISC));
        let n = ITEMS.len();
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
        theme::hint(d, i18n::t(misc::LIST_HINT));
    }
}

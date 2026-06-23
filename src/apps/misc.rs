//! Misc — a sub-launcher grouping the small extra apps. Mirrors the Games launcher:
//! the home menu opens this, picking an item runs it, and G0/Backspace returns to this
//! list (a second press pops to the home menu). Items that touch the SD card take the
//! volume manager through `on_key` / the leave paths.
//!
//! The Mic recorder is only present off the emugbc (colour) build — its I2S RX DMA
//! buffer would shrink the tight CPU0 stack enough to overflow the Web UI there.

use embedded_graphics::primitives::{PrimitiveStyle, Triangle};
use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_sdmmc::{BlockDevice, TimeSource, VolumeManager};

use crate::apps::calc::Calc;
use crate::apps::chip8::Chip8;
use crate::apps::convert::Convert;
use crate::apps::dice::Dice;
use crate::apps::ir::Ir;
use crate::apps::qr::Qr;
#[cfg(not(feature = "emugbc"))]
use crate::apps::recorder::Recorder;
use crate::hal::ir::IrTx;
use crate::{i18n, theme};

#[cfg(not(feature = "emugbc"))]
const ITEMS: [&str; 7] = ["Chip-8", "Calc", "Convert", "Dice", "QR", "IR", "Mic"];
#[cfg(feature = "emugbc")]
const ITEMS: [&str; 6] = ["Chip-8", "Calc", "Convert", "Dice", "QR", "IR"];

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
    #[cfg(not(feature = "emugbc"))]
    recorder: Recorder,
}

impl Misc {
    pub fn new(ir_tx: IrTx) -> Self {
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
            #[cfg(not(feature = "emugbc"))]
            recorder: Recorder::new(),
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

    /// Free / finalise whatever item is running. The recorder needs the SD volume to
    /// flush + close its WAV, so the leave paths thread it through here.
    fn exit_active<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>) {
        let _ = sd; // used by the recorder arm only (absent on emugbc)
        match self.active {
            Some(0) => self.chip8.exit(),
            Some(1) => self.calc.exit(),
            Some(2) => self.convert.exit(),
            Some(3) => self.dice.exit(),
            Some(4) => self.qr.exit(),
            Some(5) => self.ir.exit(),
            #[cfg(not(feature = "emugbc"))]
            Some(6) => self.recorder.finalize(sd),
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

    /// G0 button: same as Backspace here.
    pub fn g0<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        self.back(sd, d)
    }

    /// Free/finalise any running item (used when jumping straight home with `).
    pub fn leave<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>) {
        self.exit_active(sd);
        self.active = None;
    }

    /// True when the Mic recorder is the active item and is recording — main then pops
    /// fresh I2S RX audio and hands it to [`Misc::mic_feed`].
    #[cfg(not(feature = "emugbc"))]
    pub fn mic_armed(&self) -> bool {
        self.active == Some(6) && self.recorder.is_recording()
    }

    /// Stream a chunk of captured PCM to the recorder (called by main while recording).
    #[cfg(not(feature = "emugbc"))]
    pub fn mic_feed<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>, bytes: &[u8]) {
        self.recorder.feed(sd, bytes);
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
            #[cfg(not(feature = "emugbc"))]
            Some(6) => self.recorder.on_key(rc, sd, d),
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
            Some(5) => self.ir.tick(d),
            #[cfg(not(feature = "emugbc"))]
            Some(6) => self.recorder.tick(d),
            _ => false,
        }
    }

    fn launch<D: BlockDevice, T: TimeSource>(
        &mut self,
        i: usize,
        sd: &VolumeManager<D, T>,
        d: &mut impl DrawTarget<Color = Rgb565>,
    ) {
        let _ = sd; // used by Chip-8 (ROM load); the rest ignore it
        match i {
            0 => self.chip8.enter(sd, d),
            1 => self.calc.enter(d),
            2 => self.convert.enter(d),
            3 => self.dice.enter(d),
            4 => self.qr.enter(d),
            5 => self.ir.enter(d),
            #[cfg(not(feature = "emugbc"))]
            6 => self.recorder.enter(d),
            _ => {}
        }
    }

    fn draw_list<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Misc", "Diger"));
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
        theme::hint(d, i18n::t("UP/DN pick  ENTER open  ` menu", "YUK/AS sec  ENTER ac  ` menu"));
    }
}

//! Settings app — grouped preferences (General / Synthwave / File Browser),
//! persisted to /ECHO/DATA/CONFIG.BIN (best-effort; works fine with no SD card).

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

use crate::config::Config;
use crate::i18n;
use crate::i18n::settings;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Language,
    Led,
    LedBright,
    Intro,
    DispBright,
    SynthScale,
    SynthVol,
    RockChord,
    SortBy,
    ShowHidden,
    ConfirmDel,
}

enum Row {
    Header(fn() -> &'static str),
    Item(Item),
}

const ROWS: &[Row] = &[
    Row::Header(|| i18n::t(settings::GENERAL)),
    Row::Item(Item::Language),
    Row::Item(Item::Led),
    Row::Item(Item::LedBright),
    Row::Item(Item::Intro),
    Row::Item(Item::DispBright),
    Row::Header(|| i18n::t(settings::SYNTHWAVE)),
    Row::Item(Item::SynthScale),
    Row::Item(Item::SynthVol),
    Row::Item(Item::RockChord),
    Row::Header(|| i18n::t(settings::FILE_BROWSER)),
    Row::Item(Item::SortBy),
    Row::Item(Item::ShowHidden),
    Row::Item(Item::ConfirmDel),
];

const TOP: i32 = 22;
const ROW_H: i32 = 11;
const VISIBLE: usize = 9;

const K_UP: (u8, u8) = (2, 11);
const K_DOWN: (u8, u8) = (3, 11);
const K_LEFT: (u8, u8) = (3, 10);
const K_RIGHT: (u8, u8) = (3, 12);
const K_ENTER: (u8, u8) = (2, 13);

pub struct Settings {
    sel: usize, // index into ROWS (always an Item)
    scroll: usize,
}

impl Settings {
    pub fn new() -> Self {
        Settings { sel: 1, scroll: 0 }
    }

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>, cfg: &Config) {
        self.sel = 1;
        self.scroll = 0;
        self.draw(d, cfg, true);
    }

    /// Returns true if a value changed (so the caller can persist + apply it).
    pub fn on_key(&mut self, rc: (u8, u8), cfg: &mut Config, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        match rc {
            K_UP => {
                self.sel = prev_item(self.sel);
                self.fix_scroll();
                self.draw(d, cfg, false);
                false
            }
            K_DOWN => {
                self.sel = next_item(self.sel);
                self.fix_scroll();
                self.draw(d, cfg, false);
                false
            }
            K_LEFT => {
                let full = self.cycle(cfg, false);
                self.draw(d, cfg, full);
                true
            }
            K_RIGHT | K_ENTER => {
                let full = self.cycle(cfg, true);
                self.draw(d, cfg, full);
                true
            }
            _ => false,
        }
    }

    fn fix_scroll(&mut self) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + VISIBLE {
            self.scroll = self.sel + 1 - VISIBLE;
        }
    }

    /// Apply a value change. Returns true if the whole screen must be repainted
    /// (language change: every label, header, title + hint switch at once).
    fn cycle(&self, cfg: &mut Config, fwd: bool) -> bool {
        let it = match &ROWS[self.sel] {
            Row::Item(i) => *i,
            _ => return false,
        };
        match it {
            Item::Language => {
                let n = i18n::COUNT as u8;
                cfg.lang = if fwd { (cfg.lang + 1) % n } else { (cfg.lang + n - 1) % n };
                cfg.apply_lang();
                return true; // re-render every string
            }
            Item::Led => cfg.led_on = !cfg.led_on,
            Item::LedBright => cfg.led_bright = step(cfg.led_bright, fwd, 0, 10),
            Item::Intro => cfg.intro_on = !cfg.intro_on,
            Item::DispBright => cfg.disp_bright = step(cfg.disp_bright, fwd, 1, 10),
            Item::SynthScale => cfg.synth_start = if fwd { cfg.synth_start.next() } else { cfg.synth_start.prev() },
            Item::SynthVol => cfg.synth_vol = step(cfg.synth_vol, fwd, 0, 10),
            Item::RockChord => cfg.rock_chord = !cfg.rock_chord,
            Item::SortBy => cfg.sort_by = if cfg.sort_by == 1 { 0 } else { 1 },
            Item::ShowHidden => cfg.show_hidden = !cfg.show_hidden,
            Item::ConfirmDel => cfg.confirm_delete = !cfg.confirm_delete,
        }
        false
    }

    /// `clear` true only when entering (transition). On updates we skip the
    /// full-screen clear and let each row repaint its own background — no flicker.
    pub fn draw(&self, d: &mut impl DrawTarget<Color = Rgb565>, cfg: &Config, clear: bool) {
        if clear {
            theme::clear(d);
            theme::topbar(d, i18n::t(settings::SETTINGS));
            theme::hint(
                d,
                i18n::t(settings::HINT),
            );
        }

        let end = (self.scroll + VISIBLE).min(ROWS.len());
        for r in self.scroll..end {
            let y = TOP + (r - self.scroll) as i32 * ROW_H;
            // self-clear the row band (erases old highlight/content)
            theme::fill(d, 0, y - 1, theme::W as u32, ROW_H as u32, theme::BG);
            match &ROWS[r] {
                Row::Header(name) => {
                    theme::text(d, name(), theme::PAD, y + 1, theme::BODY_FONT, theme::accent());
                    theme::hline(d, y + 10, theme::BORDER);
                }
                Row::Item(it) => {
                    let selected = r == self.sel;
                    if selected {
                        theme::fill(d, theme::PAD, y - 1, (theme::W - 2 * theme::PAD) as u32, ROW_H as u32, theme::SURFACE2);
                        theme::fill(d, theme::PAD, y - 1, 3, ROW_H as u32, theme::accent());
                    }
                    let lc = if selected { theme::FG } else { theme::MUTED };
                    theme::text(d, item_label(*it), theme::PAD + 9, y + 1, theme::BODY_FONT, lc);
                    let vc = if selected { theme::accent() } else { theme::MUTED };
                    draw_value(d, cfg, *it, theme::W - theme::PAD - 8, y + 1, vc);
                }
            }
        }
        // erase any leftover band when the tail has fewer than VISIBLE rows
        let drawn = end - self.scroll;
        if drawn < VISIBLE {
            let y = TOP + drawn as i32 * ROW_H;
            theme::fill(d, 0, y - 1, theme::W as u32, ((VISIBLE - drawn) as i32 * ROW_H) as u32, theme::BG);
        }
    }
}

fn next_item(from: usize) -> usize {
    let mut i = from;
    while i + 1 < ROWS.len() {
        i += 1;
        if matches!(ROWS[i], Row::Item(_)) {
            return i;
        }
    }
    from
}

fn prev_item(from: usize) -> usize {
    let mut i = from;
    while i > 0 {
        i -= 1;
        if matches!(ROWS[i], Row::Item(_)) {
            return i;
        }
    }
    from
}

fn step(v: u8, fwd: bool, lo: u8, hi: u8) -> u8 {
    if fwd {
        if v >= hi { hi } else { v + 1 }
    } else if v <= lo {
        lo
    } else {
        v - 1
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        i18n::t(settings::ON)
    } else {
        i18n::t(settings::OFF)
    }
}

fn item_label(it: Item) -> &'static str {
    match it {
        Item::Language => i18n::t(settings::LANGUAGE),
        Item::Led => i18n::t(settings::LED),
        Item::LedBright => i18n::t(settings::LED_BRIGHT),
        Item::Intro => i18n::t(settings::BOOT_INTRO),
        Item::DispBright => i18n::t(settings::BRIGHTNESS),
        Item::SynthScale => i18n::t(settings::START_SCALE),
        Item::SynthVol => i18n::t(settings::START_VOLUME),
        Item::RockChord => i18n::t(settings::POWER_CHORD),
        Item::SortBy => i18n::t(settings::SORT_BY),
        Item::ShowHidden => i18n::t(settings::SHOW_HIDDEN),
        Item::ConfirmDel => i18n::t(settings::CONFIRM_DEL),
    }
}

fn draw_value(d: &mut impl DrawTarget<Color = Rgb565>, cfg: &Config, it: Item, xr: i32, y: i32, col: Rgb565) {
    let mut nb = [0u8; 3];
    let s: &str = match it {
        Item::Language => i18n::NAMES[(cfg.lang as usize).min(i18n::COUNT - 1)],
        Item::Led => on_off(cfg.led_on),
        Item::LedBright => fmt_u8(cfg.led_bright, &mut nb),
        Item::Intro => on_off(cfg.intro_on),
        Item::DispBright => fmt_u8(cfg.disp_bright, &mut nb),
        Item::SynthScale => cfg.synth_start.name(),
        Item::SynthVol => fmt_u8(cfg.synth_vol, &mut nb),
        Item::RockChord => on_off(cfg.rock_chord),
        Item::SortBy => {
            if cfg.sort_by == 1 {
                i18n::t(settings::SIZE)
            } else {
                i18n::t(settings::NAME)
            }
        }
        Item::ShowHidden => on_off(cfg.show_hidden),
        Item::ConfirmDel => on_off(cfg.confirm_delete),
    };
    theme::text_right(d, s, xr, y, theme::BODY_FONT, col);
}

fn fmt_u8(v: u8, buf: &mut [u8; 3]) -> &str {
    let mut tmp = [0u8; 3];
    let mut n = v as u16;
    let mut i = 0;
    if n == 0 {
        tmp[0] = b'0';
        i = 1;
    } else {
        while n > 0 && i < 3 {
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

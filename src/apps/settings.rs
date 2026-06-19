//! Settings app — grouped preferences (General / Synthwave / File Browser),
//! persisted to /ECHO/DATA/CONFIG.BIN (best-effort; works fine with no SD card).

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_sdmmc::{BlockDevice, Mode as FileMode, TimeSource, VolumeIdx, VolumeManager};

use crate::apps::scales::Mode;
use crate::i18n;
use crate::theme;

const DIR_APP: &str = "ECHO"; // 8.3 FAT short-name limit (echoputer is 9 chars)
const DIR_DATA: &str = "DATA";
const FILE_CFG: &str = "CONFIG.BIN";
const VERSION: u8 = 3;

/// User configuration shared across the firmware.
pub struct Config {
    // Synthwave
    pub synth_start: Mode,
    pub synth_vol: u8,   // 0..=10
    pub rock_chord: bool,
    // General
    pub accent_idx: usize,
    pub led_on: bool,
    pub led_bright: u8,  // 0..=10
    pub intro_on: bool,
    pub disp_bright: u8, // 1..=10
    pub lang: u8,        // 0 = English, 1 = Turkce
    // File Browser
    pub sort_by: u8, // 0 = name, 1 = size
    pub show_hidden: bool,
    pub confirm_delete: bool,
}

impl Config {
    pub fn new() -> Self {
        Config {
            synth_start: Mode::MajorPenta,
            synth_vol: 8,
            rock_chord: true,
            accent_idx: 0,
            led_on: true,
            led_bright: 5,
            intro_on: true,
            disp_bright: 10,
            lang: 0,
            sort_by: 0,
            show_hidden: false,
            confirm_delete: true,
        }
    }

    pub fn apply_lang(&self) {
        i18n::set_idx(self.lang);
    }

    pub fn load<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        let mut buf = [0u8; 16];
        let mut n = 0usize;
        let ok = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let mut dir = vol.open_root_dir().ok()?;
            dir.change_dir(DIR_APP).ok()?;
            dir.change_dir(DIR_DATA).ok()?;
            let file = dir.open_file_in_dir(FILE_CFG, FileMode::ReadOnly).ok()?;
            n = file.read(&mut buf).ok()?;
            Some(())
        })();
        if ok.is_none() || n < 6 || buf[0] != b'E' || buf[1] != b'C' || buf[2] < 1 {
            return;
        }
        // v1+ fields
        self.synth_start = Mode::from_index(buf[3]);
        self.accent_idx = buf[4] as usize; // retained for CONFIG.BIN compatibility (unused)
        self.led_on = buf[5] != 0;
        // v2 fields
        if buf[2] >= 2 && n >= 14 {
            self.synth_vol = buf[6].min(10);
            self.rock_chord = buf[7] != 0;
            self.led_bright = buf[8].min(10);
            self.intro_on = buf[9] != 0;
            self.disp_bright = buf[10].clamp(1, 10);
            self.sort_by = if buf[11] == 1 { 1 } else { 0 };
            self.show_hidden = buf[12] != 0;
            self.confirm_delete = buf[13] != 0;
        }
        // v3 fields
        if buf[2] >= 3 && n >= 15 {
            self.lang = if buf[14] == 1 { 1 } else { 0 };
        }
    }

    pub fn save<D: BlockDevice, T: TimeSource>(&self, vm: &VolumeManager<D, T>) {
        let buf = [
            b'E',
            b'C',
            VERSION,
            self.synth_start.index(),
            self.accent_idx as u8,
            self.led_on as u8,
            self.synth_vol,
            self.rock_chord as u8,
            self.led_bright,
            self.intro_on as u8,
            self.disp_bright,
            self.sort_by,
            self.show_hidden as u8,
            self.confirm_delete as u8,
            self.lang,
        ];
        let _ = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let mut dir = vol.open_root_dir().ok()?;
            let _ = dir.make_dir_in_dir(DIR_APP);
            dir.change_dir(DIR_APP).ok()?;
            let _ = dir.make_dir_in_dir(DIR_DATA);
            dir.change_dir(DIR_DATA).ok()?;
            let file = dir.open_file_in_dir(FILE_CFG, FileMode::ReadWriteCreateOrTruncate).ok()?;
            file.write(&buf).ok()?;
            file.flush().ok()?;
            Some(())
        })();
    }
}

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
    Row::Header(|| i18n::t("GENERAL", "GENEL")),
    Row::Item(Item::Language),
    Row::Item(Item::Led),
    Row::Item(Item::LedBright),
    Row::Item(Item::Intro),
    Row::Item(Item::DispBright),
    Row::Header(|| i18n::t("SYNTHWAVE", "SYNTHWAVE")),
    Row::Item(Item::SynthScale),
    Row::Item(Item::SynthVol),
    Row::Item(Item::RockChord),
    Row::Header(|| i18n::t("FILE BROWSER", "DOSYA TARAYICI")),
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
            theme::topbar(d, i18n::t("Settings", "Ayarlar"));
            theme::hint(
                d,
                i18n::t("up/down select   left/right change   ` menu", "yukari/asagi sec   sol/sag degistir   ` menu"),
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
        i18n::t("On", "Acik")
    } else {
        i18n::t("Off", "Kapali")
    }
}

fn item_label(it: Item) -> &'static str {
    match it {
        Item::Language => i18n::t("Language", "Dil"),
        Item::Led => i18n::t("LED", "LED"),
        Item::LedBright => i18n::t("LED bright", "LED parlak"),
        Item::Intro => i18n::t("Boot intro", "Acilis"),
        Item::DispBright => i18n::t("Brightness", "Parlaklik"),
        Item::SynthScale => i18n::t("Start scale", "Baslangic gam"),
        Item::SynthVol => i18n::t("Start volume", "Baslangic ses"),
        Item::RockChord => i18n::t("Power chord", "Power chord"),
        Item::SortBy => i18n::t("Sort by", "Siralama"),
        Item::ShowHidden => i18n::t("Show hidden", "Gizliyi goster"),
        Item::ConfirmDel => i18n::t("Confirm del", "Silme onayi"),
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
                i18n::t("Size", "Boyut")
            } else {
                i18n::t("Name", "Isim")
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

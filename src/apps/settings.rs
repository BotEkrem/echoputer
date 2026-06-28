//! Settings app — grouped preferences (General / Synthwave / File Browser / Offload),
//! persisted to /ECHO/DATA/CONFIG.BIN + the offload server config to /OFFLOAD.CFG
//! (best-effort; works fine with no SD card). Most rows cycle a value with LEFT/RIGHT;
//! the Offload rows are TEXT fields edited with the keyboard (ENTER opens the editor).

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

use crate::config::{Config, OffloadCfg};
use crate::hal::keymap;
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
    // Offload server (text-entry fields, edited via the keyboard).
    OffHost,
    OffPort,
    OffPsk,
    OffUplink,
    OffUpass,
}

impl Item {
    /// Offload fields are typed on the keyboard, not cycled with LEFT/RIGHT.
    fn is_text(self) -> bool {
        matches!(self, Item::OffHost | Item::OffPort | Item::OffPsk | Item::OffUplink | Item::OffUpass)
    }
    /// Secrets are masked in the list (shown plaintext only while editing).
    fn is_secret(self) -> bool {
        matches!(self, Item::OffPsk | Item::OffUpass)
    }
    /// Edit-buffer cap matching the backing `OffloadCfg` field, so the typed value
    /// is WYSIWYG with what `commit()` will actually store (never silently truncated).
    fn max_len(self) -> usize {
        match self {
            Item::OffHost => 40,
            Item::OffPort => 5, // u16 max 65535
            Item::OffUplink => 32,
            Item::OffPsk | Item::OffUpass => 64,
            _ => 64,
        }
    }
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
    Row::Header(|| i18n::t(settings::OFFLOAD)),
    Row::Item(Item::OffHost),
    Row::Item(Item::OffPort),
    Row::Item(Item::OffPsk),
    Row::Item(Item::OffUplink),
    Row::Item(Item::OffUpass),
];

const TOP: i32 = 22;
const ROW_H: i32 = 11;
const VISIBLE: usize = 9;

const K_UP: (u8, u8) = (2, 11);
const K_DOWN: (u8, u8) = (3, 11);
const K_LEFT: (u8, u8) = (3, 10);
const K_RIGHT: (u8, u8) = (3, 12);
const K_ENTER: (u8, u8) = (2, 13);

/// What a key press changed, so the caller persists the right file.
#[derive(PartialEq, Eq)]
pub enum Saved {
    None,
    Config,
    Offload,
}

pub struct Settings {
    sel: usize, // index into ROWS (always an Item)
    scroll: usize,
    editing: Option<Item>, // Some -> typing into `buf` for this offload field
    buf: [u8; 64],
    buf_len: usize,
    caps: bool, // "Aa" toggle for the text editor
}

impl Settings {
    pub fn new() -> Self {
        Settings { sel: 1, scroll: 0, editing: None, buf: [0; 64], buf_len: 0, caps: false }
    }

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>, cfg: &Config, off: &OffloadCfg) {
        self.sel = 1;
        self.scroll = 0;
        self.editing = None;
        self.draw(d, cfg, off, true);
    }

    /// Returns what changed (so the caller persists CONFIG.BIN and/or OFFLOAD.CFG).
    pub fn on_key(
        &mut self,
        rc: (u8, u8),
        cfg: &mut Config,
        off: &mut OffloadCfg,
        d: &mut impl DrawTarget<Color = Rgb565>,
    ) -> Saved {
        // ---- text-edit mode (offload fields) ----
        if let Some(it) = self.editing {
            match rc {
                K_ENTER => {
                    self.commit(it, off);
                    self.editing = None;
                    self.draw(d, cfg, off, true);
                    return Saved::Offload;
                }
                keymap::K_BKSP => {
                    self.buf_len = self.buf_len.saturating_sub(1);
                    self.draw(d, cfg, off, false);
                    return Saved::None;
                }
                _ => {
                    if let Some(b) = keymap::ch_shift(rc.0, rc.1, self.caps) {
                        // port is digit-only; every field caps at its backing capacity.
                        let allow = it != Item::OffPort || b.is_ascii_digit();
                        if allow && self.buf_len < it.max_len() {
                            self.buf[self.buf_len] = b;
                            self.buf_len += 1;
                            self.draw(d, cfg, off, false);
                        }
                    }
                    return Saved::None;
                }
            }
        }
        // ---- list navigation ----
        match rc {
            K_UP => {
                self.sel = prev_item(self.sel);
                self.fix_scroll();
                self.draw(d, cfg, off, false);
                Saved::None
            }
            K_DOWN => {
                self.sel = next_item(self.sel);
                self.fix_scroll();
                self.draw(d, cfg, off, false);
                Saved::None
            }
            K_LEFT => {
                if self.cur_item().map(Item::is_text).unwrap_or(false) {
                    return Saved::None; // text fields: only ENTER opens the editor
                }
                let full = self.cycle(cfg, false);
                self.draw(d, cfg, off, full);
                Saved::Config
            }
            K_RIGHT | K_ENTER => {
                if let Some(it) = self.cur_item() {
                    if it.is_text() {
                        self.start_edit(it, off);
                        self.draw(d, cfg, off, true);
                        return Saved::None;
                    }
                }
                let full = self.cycle(cfg, true);
                self.draw(d, cfg, off, full);
                Saved::Config
            }
            _ => Saved::None,
        }
    }

    /// ESC: cancel an in-progress edit (stay in Settings). Returns true if consumed.
    pub fn back(&mut self, d: &mut impl DrawTarget<Color = Rgb565>, cfg: &Config, off: &OffloadCfg) -> bool {
        if self.editing.is_some() {
            self.editing = None;
            self.draw(d, cfg, off, true);
            true
        } else {
            false
        }
    }

    /// The "Aa" key flips case while editing.
    pub fn toggle_caps(&mut self, d: &mut impl DrawTarget<Color = Rgb565>, cfg: &Config, off: &OffloadCfg) {
        if self.editing.is_some() {
            self.caps = !self.caps;
            self.draw(d, cfg, off, false);
        }
    }

    fn cur_item(&self) -> Option<Item> {
        match &ROWS[self.sel] {
            Row::Item(i) => Some(*i),
            _ => None,
        }
    }

    fn start_edit(&mut self, it: Item, off: &OffloadCfg) {
        let cur = field_str(it, off);
        let b = cur.as_bytes();
        let n = b.len().min(self.buf.len());
        self.buf[..n].copy_from_slice(&b[..n]);
        self.buf_len = n;
        self.caps = false;
        self.editing = Some(it);
    }

    fn commit(&self, it: Item, off: &mut OffloadCfg) {
        let v = core::str::from_utf8(&self.buf[..self.buf_len]).unwrap_or("");
        match it {
            Item::OffHost => off.set_host(v),
            Item::OffPort => off.set_port(v),
            Item::OffPsk => off.set_psk(v),
            Item::OffUplink => off.set_uplink_ssid(v),
            Item::OffUpass => off.set_uplink_pass(v),
            _ => {}
        }
    }

    fn fix_scroll(&mut self) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + VISIBLE {
            self.scroll = self.sel + 1 - VISIBLE;
        }
    }

    /// Apply a cycle change (Config items only; text items never reach here). Returns
    /// true if the whole screen must repaint (language switches every string).
    fn cycle(&self, cfg: &mut Config, fwd: bool) -> bool {
        let it = match self.cur_item() {
            Some(i) => i,
            None => return false,
        };
        match it {
            Item::Language => {
                let n = i18n::COUNT as u8;
                cfg.lang = if fwd { (cfg.lang + 1) % n } else { (cfg.lang + n - 1) % n };
                cfg.apply_lang();
                return true;
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
            // text items don't cycle
            Item::OffHost | Item::OffPort | Item::OffPsk | Item::OffUplink | Item::OffUpass => {}
        }
        false
    }

    pub fn draw(&self, d: &mut impl DrawTarget<Color = Rgb565>, cfg: &Config, off: &OffloadCfg, clear: bool) {
        if let Some(it) = self.editing {
            self.draw_edit(d, it, clear);
            return;
        }
        if clear {
            theme::clear(d);
            theme::topbar(d, i18n::t(settings::SETTINGS));
            theme::hint(d, i18n::t(settings::HINT));
        }

        let end = (self.scroll + VISIBLE).min(ROWS.len());
        for r in self.scroll..end {
            let y = TOP + (r - self.scroll) as i32 * ROW_H;
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
                    draw_value(d, cfg, off, *it, theme::W - theme::PAD - 8, y + 1, vc);
                }
            }
        }
        let drawn = end - self.scroll;
        if drawn < VISIBLE {
            let y = TOP + drawn as i32 * ROW_H;
            theme::fill(d, 0, y - 1, theme::W as u32, ((VISIBLE - drawn) as i32 * ROW_H) as u32, theme::BG);
        }
    }

    /// The keyboard editor for one offload field.
    fn draw_edit(&self, d: &mut impl DrawTarget<Color = Rgb565>, it: Item, clear: bool) {
        if clear {
            theme::clear(d);
            theme::topbar(d, i18n::t(settings::SETTINGS));
            theme::text(d, item_label(it), theme::PAD, 28, theme::BODY_FONT, theme::accent());
            theme::hint(d, i18n::t(settings::EDIT_HINT));
        }
        // value box (self-clearing band) + the typed text with a cursor
        theme::fill(d, theme::PAD, 42, (theme::W - 2 * theme::PAD) as u32, 16, theme::SURFACE2);
        let s = core::str::from_utf8(&self.buf[..self.buf_len]).unwrap_or("");
        let shown = alloc::format!("{s}_");
        theme::text(d, &shown, theme::PAD + 4, 46, theme::BODY_FONT, theme::FG);
        theme::text_right(d, if self.caps { "ABC" } else { "abc" }, theme::W - theme::PAD - 4, 46, theme::BODY_FONT, theme::MUTED);
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
        Item::OffHost => i18n::t(settings::OFF_HOST),
        Item::OffPort => i18n::t(settings::OFF_PORT),
        Item::OffPsk => i18n::t(settings::OFF_PSK),
        Item::OffUplink => i18n::t(settings::OFF_UPLINK),
        Item::OffUpass => i18n::t(settings::OFF_UPASS),
    }
}

/// The current value of an offload field as a String (for the editor + reset).
fn field_str(it: Item, off: &OffloadCfg) -> alloc::string::String {
    match it {
        Item::OffHost => off.host_str().into(),
        Item::OffPort => alloc::format!("{}", off.port),
        Item::OffPsk => off.psk_str().into(),
        Item::OffUplink => off.uplink_ssid_str().into(),
        Item::OffUpass => off.uplink_pass_str().into(),
        _ => alloc::string::String::new(),
    }
}

fn draw_value(d: &mut impl DrawTarget<Color = Rgb565>, cfg: &Config, off: &OffloadCfg, it: Item, xr: i32, y: i32, col: Rgb565) {
    let mut nb = [0u8; 3];
    // offload rows: show value (secrets masked, empties as a dash)
    if it.is_text() {
        let set = match it {
            Item::OffHost => off.host_len > 0,
            Item::OffPsk => off.psk_len > 0,
            Item::OffUplink => off.uplink_ssid_len > 0,
            Item::OffUpass => off.uplink_pass_len > 0,
            _ => true, // port always has a value
        };
        if it.is_secret() {
            theme::text_right(d, if set { i18n::t(settings::SET) } else { "-" }, xr, y, theme::BODY_FONT, col);
            return;
        }
        let owned: alloc::string::String = match it {
            Item::OffHost => off.host_str().chars().take(14).collect(),
            Item::OffPort => alloc::format!("{}", off.port),
            Item::OffUplink => off.uplink_ssid_str().chars().take(14).collect(),
            _ => alloc::string::String::new(),
        };
        let show = if owned.is_empty() { "-" } else { &owned };
        theme::text_right(d, show, xr, y, theme::BODY_FONT, col);
        return;
    }
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
        _ => "",
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

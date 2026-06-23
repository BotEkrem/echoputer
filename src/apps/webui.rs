//! Web UI app — the on-device half: scan for WiFi networks, pick one, type the
//! password, then show connection status + the dashboard URL while the radio
//! layer (`radio::webui`) connects as a station and serves an HTTP file/system
//! dashboard on the leased IP. This module is purely UI + state; it never touches
//! smoltcp or the SD card. `main` drives the scan (via `Radio::scan`) and, on
//! `Action::Connect`, the blocking serve loop (via `Radio::run_webui`), repainting
//! through `draw_status` from the serve loop's tick callback.

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

use crate::hal::keymap;
use crate::{i18n, theme};

const MAX_APS: usize = 24;
const ROW_VISIBLE: usize = 5;
const SSID_MAX: usize = 32;
const PW_MAX: usize = 64;

struct Ap {
    ssid: [u8; SSID_MAX],
    ssid_len: u8,
    rssi: i8,
    channel: u8,
    auth: &'static str,
    secured: bool,
}

impl Ap {
    const fn empty() -> Self {
        Ap { ssid: [0; SSID_MAX], ssid_len: 0, rssi: 0, channel: 0, auth: "", secured: false }
    }
}

const MAX_KNOWN: usize = 8;

/// A remembered network (SSID + password), loaded from SD on entry so a known
/// network's password field comes pre-filled (just press ENTER, or edit if it
/// changed).
struct Cred {
    ssid: [u8; SSID_MAX],
    ssid_len: u8,
    pw: [u8; PW_MAX],
    pw_len: u8,
}

impl Cred {
    const fn empty() -> Self {
        Cred { ssid: [0; SSID_MAX], ssid_len: 0, pw: [0; PW_MAX], pw_len: 0 }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Scan,
    Password,
    Status,
}

/// What `on_key` asks `main` to do next.
pub enum Action {
    None,
    Redraw,
    /// The user picked a network (and entered a password if it was secured).
    /// `main` reads `ssid()/password()/auth()` and runs `Radio::run_webui`.
    Connect,
}

/// Connection phase shown on the status screen (set by `main` from the radio
/// layer's serve state).
#[derive(Clone, Copy)]
pub enum Phase {
    Connecting,
    Serving { ip: [u8; 4], hits: u32 },
    Failed(&'static str),
}

pub struct WebUi {
    view: View,
    aps: [Ap; MAX_APS],
    ap_count: usize,
    sel: usize,
    scroll: usize,
    pw: [u8; PW_MAX],
    pw_len: usize,
    caps: bool,
    scan_failed: bool,
    known: [Cred; MAX_KNOWN],
    known_count: usize,
}

impl WebUi {
    pub fn new() -> Self {
        WebUi {
            view: View::Scan,
            aps: [const { Ap::empty() }; MAX_APS],
            ap_count: 0,
            sel: 0,
            scroll: 0,
            pw: [0; PW_MAX],
            pw_len: 0,
            caps: false,
            scan_failed: false,
            known: [const { Cred::empty() }; MAX_KNOWN],
            known_count: 0,
        }
    }

    // ---- remembered networks (loaded from SD by main on entry) ------------

    pub fn clear_known(&mut self) {
        self.known_count = 0;
    }

    /// Remember a saved network's password (main calls this per stored cred).
    pub fn add_known(&mut self, ssid: &[u8], pw: &[u8]) {
        if self.known_count >= MAX_KNOWN || ssid.len() > SSID_MAX || pw.len() > PW_MAX {
            return;
        }
        let c = &mut self.known[self.known_count];
        c.ssid[..ssid.len()].copy_from_slice(ssid);
        c.ssid_len = ssid.len() as u8;
        c.pw[..pw.len()].copy_from_slice(pw);
        c.pw_len = pw.len() as u8;
        self.known_count += 1;
    }

    /// The saved password for `ssid`, copied out (so it can be written into `pw`
    /// without aliasing the `known` borrow).
    fn known_pw(&self, ssid: &[u8]) -> Option<([u8; PW_MAX], usize)> {
        for c in &self.known[..self.known_count] {
            if &c.ssid[..c.ssid_len as usize] == ssid {
                return Some((c.pw, c.pw_len as usize));
            }
        }
        None
    }

    // ---- scan intake (driven by main via Radio::scan) ---------------------

    /// Reset before a (re)scan; `main` calls this, then `push_ap` per network.
    pub fn begin_scan(&mut self) {
        self.ap_count = 0;
        self.sel = 0;
        self.scroll = 0;
        self.scan_failed = false;
    }

    pub fn mark_scan_failed(&mut self) {
        self.scan_failed = true;
    }

    pub fn push_ap(&mut self, ssid: &[u8], rssi: i8, channel: u8, auth: &'static str, secured: bool) {
        if self.ap_count >= MAX_APS {
            return;
        }
        let ap = &mut self.aps[self.ap_count];
        let n = ssid.len().min(SSID_MAX);
        ap.ssid[..n].copy_from_slice(&ssid[..n]);
        ap.ssid_len = n as u8;
        ap.rssi = rssi;
        ap.channel = channel;
        ap.auth = auth;
        ap.secured = secured;
        self.ap_count += 1;
    }

    // ---- selection accessors (read by main on Action::Connect) ------------

    pub fn ssid(&self) -> &str {
        let ap = &self.aps[self.sel.min(MAX_APS - 1)];
        core::str::from_utf8(&ap.ssid[..ap.ssid_len as usize]).unwrap_or("")
    }

    pub fn password(&self) -> &str {
        core::str::from_utf8(&self.pw[..self.pw_len]).unwrap_or("")
    }

    // ---- lifecycle --------------------------------------------------------

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.view = View::Scan;
        self.draw_scanning(d);
    }

    /// Called after the scan completes to show the list.
    pub fn show_list<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.view = View::Scan;
        self.draw_list(d);
    }

    /// G0 / Backspace: from the password field -> back to the list; from the list
    /// -> false (pop to the home menu). (Status is left while the radio loop runs;
    /// it returns through main's abort path, not here.)
    pub fn back<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        match self.view {
            View::Password => {
                self.pw_len = 0;
                self.view = View::Scan;
                self.draw_list(d);
                true
            }
            View::Scan | View::Status => false,
        }
    }

    /// True while typing the password (so main routes Backspace here, not to back).
    pub fn is_editing(&self) -> bool {
        self.view == View::Password
    }

    pub fn toggle_caps<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        if self.view == View::Password {
            self.caps = !self.caps;
            self.draw_password(d);
        }
    }

    // ---- input ------------------------------------------------------------

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        match self.view {
            View::Scan => self.key_list(rc, d),
            View::Password => self.key_password(rc, d),
            View::Status => Action::None,
        }
    }

    fn key_list<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        match rc {
            crate::K_UP => {
                if self.ap_count > 0 {
                    self.sel = if self.sel == 0 { self.ap_count - 1 } else { self.sel - 1 };
                    self.scroll = clamp(self.sel, self.scroll, ROW_VISIBLE);
                    self.draw_list(d);
                }
                Action::Redraw
            }
            crate::K_DOWN => {
                if self.ap_count > 0 {
                    self.sel = (self.sel + 1) % self.ap_count;
                    self.scroll = clamp(self.sel, self.scroll, ROW_VISIBLE);
                    self.draw_list(d);
                }
                Action::Redraw
            }
            crate::K_ENTER => {
                if self.ap_count == 0 {
                    return Action::None;
                }
                if self.aps[self.sel].secured {
                    // pre-fill the password if this network was saved before (the
                    // field is editable + visible, so they can fix a changed one)
                    self.pw_len = 0;
                    let sl = self.aps[self.sel].ssid_len as usize;
                    let mut ssid = [0u8; SSID_MAX];
                    ssid[..sl].copy_from_slice(&self.aps[self.sel].ssid[..sl]);
                    if let Some((pw, pl)) = self.known_pw(&ssid[..sl]) {
                        self.pw[..pl].copy_from_slice(&pw[..pl]);
                        self.pw_len = pl;
                    }
                    self.caps = false;
                    self.view = View::Password;
                    self.draw_password(d);
                    Action::Redraw
                } else {
                    self.pw_len = 0; // open network
                    Action::Connect
                }
            }
            _ => Action::None,
        }
    }

    fn key_password<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        if rc == crate::K_ENTER {
            return Action::Connect;
        }
        if rc == keymap::K_BKSP {
            self.pw_len = self.pw_len.saturating_sub(1);
            self.draw_password(d);
            return Action::Redraw;
        }
        if let Some(b) = keymap::ch_shift(rc.0, rc.1, self.caps) {
            if self.pw_len < PW_MAX {
                self.pw[self.pw_len] = b;
                self.pw_len += 1;
                self.draw_password(d);
            }
        }
        Action::Redraw
    }

    // ---- drawing ----------------------------------------------------------

    fn draw_scanning<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, "Web UI");
        theme::text_center(
            d,
            i18n::t("scanning WiFi...", "WiFi taraniyor..."),
            theme::W / 2,
            theme::H / 2,
            theme::TITLE_FONT,
            theme::FG,
        );
    }

    fn draw_list<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Pick a network", "Bir ag sec"));
        if self.scan_failed {
            theme::text_center(
                d,
                i18n::t("WiFi unavailable", "WiFi kullanilamiyor"),
                theme::W / 2,
                theme::H / 2,
                theme::BODY_FONT,
                theme::DESTRUCTIVE,
            );
            theme::hint(d, i18n::t("G0 back", "G0 geri"));
            return;
        }
        if self.ap_count == 0 {
            theme::text_center(
                d,
                i18n::t("no networks found", "ag bulunamadi"),
                theme::W / 2,
                theme::H / 2,
                theme::BODY_FONT,
                theme::MUTED,
            );
        } else {
            for row in 0..ROW_VISIBLE {
                let idx = self.scroll + row;
                if idx >= self.ap_count {
                    break;
                }
                let ap = &self.aps[idx];
                let y = 22 + row as i32 * 16;
                let highlight = idx == self.sel;
                if highlight {
                    theme::fill(d, theme::PAD - 2, y - 2, (theme::W - 2 * theme::PAD + 4) as u32, 15, theme::SURFACE2);
                }
                let name: alloc::string::String = if ap.ssid_len == 0 {
                    alloc::string::String::from(i18n::t("<hidden>", "<gizli>"))
                } else {
                    core::str::from_utf8(&ap.ssid[..ap.ssid_len as usize])
                        .unwrap_or("?")
                        .chars()
                        .take(15)
                        .collect()
                };
                let col = if highlight { theme::accent() } else { theme::FG };
                theme::text(d, &name, theme::PAD, y, theme::BODY_FONT, col);
                let lock = if ap.secured { "*" } else { " " };
                let info = alloc::format!("{:>4} {}{}", ap.rssi, lock, ap.auth);
                theme::text_right(d, &info, theme::W - theme::PAD, y, theme::BODY_FONT, theme::MUTED);
            }
        }
        let hint = alloc::format!(
            "{} {}   ENTER {}   G0",
            self.ap_count,
            i18n::t("nets", "ag"),
            i18n::t("connect", "baglan")
        );
        theme::hint(d, &hint);
    }

    fn draw_password<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, self.ssid_titled());
        theme::card(d, theme::PAD, 44, (theme::W - 2 * theme::PAD) as u32, 22, Some(theme::accent()));
        // Show the password in clear text (the keyboard is tiny and easy to fat-
        // finger; on a personal device seeing it beats hiding a typo) + a cursor.
        let mut shown: alloc::string::String =
            core::str::from_utf8(&self.pw[..self.pw_len]).unwrap_or("").into();
        shown.push('_');
        theme::text(d, &shown, theme::PAD + 8, 51, theme::TITLE_FONT, theme::FG);
        theme::text_right(
            d,
            if self.caps { "ABC" } else { "abc" },
            theme::W - theme::PAD - 6,
            51,
            theme::BODY_FONT,
            theme::MUTED,
        );
        theme::hint(d, i18n::t("type  bksp del  ENTER connect  G0 back", "yaz  bksp sil  ENTER baglan  G0 geri"));
    }

    fn ssid_titled(&self) -> &str {
        let s = self.ssid();
        if s.is_empty() {
            "WiFi"
        } else {
            s
        }
    }

    /// Status screen: connecting / serving (shows the dashboard IP) / failed.
    /// Called by `main` from the serve loop's tick callback, so it must fully
    /// repaint each time.
    pub fn draw_status<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, phase: Phase) {
        self.view = View::Status;
        theme::clear(d);
        theme::topbar(d, "Web UI");
        match phase {
            Phase::Connecting => {
                theme::text_center(
                    d,
                    i18n::t("connecting...", "baglaniliyor..."),
                    theme::W / 2,
                    theme::H / 2,
                    theme::BODY_FONT,
                    theme::MUTED,
                );
                theme::hint(d, i18n::t("G0 cancel", "G0 iptal"));
            }
            Phase::Serving { ip, hits } => {
                theme::text_center(
                    d,
                    i18n::t("Dashboard live at", "Panel yayinda:"),
                    theme::W / 2,
                    40,
                    theme::BODY_FONT,
                    theme::MUTED,
                );
                let url = alloc::format!("http://{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
                theme::text_center(d, &url, theme::W / 2, 62, theme::TITLE_FONT, theme::accent());
                let reqs = alloc::format!("{} {}", hits, i18n::t("requests", "istek"));
                theme::text_center(d, &reqs, theme::W / 2, 88, theme::BODY_FONT, theme::MUTED);
                theme::hint(d, i18n::t("open it on a PC on this WiFi   G0 stop", "ayni WiFi'deki PC'den ac   G0 durdur"));
            }
            Phase::Failed(msg) => {
                theme::text_center(d, i18n::t("connection failed", "baglanti basarisiz"), theme::W / 2, 46, theme::BODY_FONT, theme::DESTRUCTIVE);
                theme::text_center(d, msg, theme::W / 2, 68, theme::BODY_FONT, theme::MUTED);
                theme::hint(d, i18n::t("G0 back", "G0 geri"));
            }
        }
    }
}

/// Keep the selected row within the visible window (mirrors the menu helper).
fn clamp(sel: usize, scroll: usize, visible: usize) -> usize {
    if sel < scroll {
        sel
    } else if sel >= scroll + visible {
        sel + 1 - visible
    } else {
        scroll
    }
}

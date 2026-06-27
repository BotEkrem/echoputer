//! Hacking menu — WiFi/BLE reconnaissance and active attack tools.
//!
//! Tools are listed in one screen grouped by **difficulty** (Basic / Intermediate
//! / Advanced, coloured green/orange/red) so a newcomer works up from the safe
//! passive tools instead of jumping straight to a disruptive one. Selecting a
//! tool opens its **detail** page — **Use**, **Wiki** (bilingual help) and, where
//! relevant, **Settings** (per-tool config, e.g. the beacon-spam SSID source).
//! Offensive tools are gated behind a one-key confirmation.
//!
//! This module is pure UI + state. `main` owns the radios (see `radio.rs`) and
//! feeds results in through the intake methods; long-running tools are driven by
//! `main`, which repaints [`draw_running`] and polls for an abort key.

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_hal::i2c::I2c;

use crate::apps::wiki;
use crate::hal::{bmi270, keymap};
use crate::radio::{ble_spam, camscan, netscan, portal};
use crate::i18n;
use crate::i18n::hacking;
use crate::theme;

pub const MAX_APS: usize = 64;
pub const MAX_BLE: usize = 64;

/// Obvious fake SSIDs for the beacon/probe spam demos (English set).
pub const SPAM_SSIDS: [&str; 16] = [
    "FBI Surveillance Van",
    "Free WiFi - Click Here",
    "Pretty Fly for a WiFi",
    "Mom Use This One",
    "Hidden Network?",
    "Virus Distribution",
    "Tell My WiFi Love Her",
    "Drop It Like Its Hotspot",
    "Loading...",
    "DEFINITELY NOT A TRAP",
    "Area 51 Guest",
    "It Hurts When IP",
    "Wu-Tang LAN",
    "The LAN Before Time",
    "Bill Wi the Science Fi",
    "404 Network Unavailable",
];

/// Turkish fake-SSID set (ASCII, no diacritics) — selectable RNG name language.
pub const SPAM_SSIDS_TR: [&str; 16] = [
    "Bedava Internet",
    "Komsunun Wifisi",
    "Buraya Tikla",
    "Sifre 12345678",
    "Polis Araci Degil",
    "Anneminki Bu",
    "Gizli Ag mi?",
    "Catidaki Modem",
    "Yukleniyor...",
    "Kesinlikle Tuzak Degil",
    "Beles Net",
    "Modemi Acdik",
    "Baglanma Sakin",
    "Misafir Agi",
    "Hizli Internet",
    "Virus Dagitim Merkezi",
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    // Basic (passive recon)
    WifiScan,
    WifiAnalyze,
    BleScan,
    Detector,
    // Intermediate (active noise / lure)
    BeaconSpam,
    ProbeFlood,
    BleSpam,
    EvilTwin,
    // Advanced (disruptive / capture / intrusion)
    Deauth,
    Handshake,
    EvilPortal,
    NetScan,
    CamScan,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Diff {
    Basic,
    Inter,
    Adv,
}
impl Diff {
    fn label(self) -> &'static str {
        match self {
            Diff::Basic => i18n::t(hacking::BASIC),
            Diff::Inter => i18n::t(hacking::INTERMEDIATE),
            Diff::Adv => i18n::t(hacking::ADVANCED),
        }
    }
    fn color(self) -> Rgb565 {
        match self {
            Diff::Basic => Rgb565::new(7, 46, 12),  // green
            Diff::Inter => Rgb565::new(29, 34, 3),   // orange
            Diff::Adv => theme::DESTRUCTIVE,         // red
        }
    }
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::WifiScan => i18n::t(hacking::WIFI_SCANNER),
            Tool::WifiAnalyze => i18n::t(hacking::WIFI_ANALYZER),
            Tool::BleScan => i18n::t(hacking::BLE_SCANNER),
            Tool::Detector => i18n::t(hacking::DEAUTH_DETECTOR),
            Tool::Deauth => i18n::t(hacking::DEAUTH_FLOOD),
            Tool::BeaconSpam => i18n::t(hacking::BEACON_SPAM),
            Tool::ProbeFlood => i18n::t(hacking::PROBE_FLOOD),
            Tool::EvilTwin => i18n::t(hacking::EVIL_TWIN),
            Tool::Handshake => i18n::t(hacking::HANDSHAKE_CAPTURE),
            Tool::EvilPortal => i18n::t(hacking::EVIL_PORTAL),
            Tool::NetScan => i18n::t(hacking::LAN_SCAN),
            Tool::CamScan => i18n::t(hacking::CAM_FINDER),
            Tool::BleSpam => i18n::t(hacking::BLE_SPAM),
        }
    }
    fn difficulty(self) -> Diff {
        match self {
            Tool::WifiScan | Tool::WifiAnalyze | Tool::BleScan | Tool::Detector => Diff::Basic,
            Tool::BeaconSpam | Tool::ProbeFlood | Tool::BleSpam | Tool::EvilTwin => Diff::Inter,
            Tool::Deauth | Tool::Handshake | Tool::EvilPortal | Tool::NetScan | Tool::CamScan => Diff::Adv,
        }
    }
    fn offensive(self) -> bool {
        !matches!(self, Tool::WifiScan | Tool::WifiAnalyze | Tool::BleScan | Tool::Detector)
    }
    fn has_settings(self) -> bool {
        matches!(self, Tool::BeaconSpam | Tool::ProbeFlood | Tool::BleSpam | Tool::EvilPortal)
    }
    fn target_title(self) -> &'static str {
        match self {
            Tool::EvilTwin => i18n::t(hacking::EVIL_TWIN_PICK_AP),
            Tool::Handshake => i18n::t(hacking::HANDSHAKE_PICK_AP),
            Tool::NetScan => i18n::t(hacking::LAN_SCAN_PICK_AP),
            Tool::CamScan => i18n::t(hacking::CAM_FINDER_PICK_AP),
            _ => i18n::t(hacking::DEAUTH_PICK_TARGET),
        }
    }
    fn target_verb(self) -> &'static str {
        match self {
            Tool::EvilTwin => i18n::t(hacking::CLONE),
            Tool::Handshake => i18n::t(hacking::CAPTURE_VERB),
            Tool::NetScan | Tool::CamScan => i18n::t(hacking::SCAN_VERB),
            _ => i18n::t(hacking::DEAUTH_VERB),
        }
    }
}

// ---- the difficulty-grouped list (headers + tools, in display order) ----
enum LRow {
    Head(Diff),
    Tool(Tool),
}
const LIST: [LRow; 16] = [
    LRow::Head(Diff::Basic),
    LRow::Tool(Tool::WifiScan),
    LRow::Tool(Tool::WifiAnalyze),
    LRow::Tool(Tool::BleScan),
    LRow::Tool(Tool::Detector),
    LRow::Head(Diff::Inter),
    LRow::Tool(Tool::BeaconSpam),
    LRow::Tool(Tool::ProbeFlood),
    LRow::Tool(Tool::BleSpam),
    LRow::Tool(Tool::EvilTwin),
    LRow::Head(Diff::Adv),
    LRow::Tool(Tool::Deauth),
    LRow::Tool(Tool::Handshake),
    LRow::Tool(Tool::EvilPortal),
    LRow::Tool(Tool::NetScan),
    LRow::Tool(Tool::CamScan),
];

// ---- per-tool settings (session-only) ----
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NameSrc {
    RandomEn,
    RandomTr,
    Custom,
}
impl NameSrc {
    fn label(self) -> &'static str {
        match self {
            NameSrc::RandomEn => i18n::t(hacking::RANDOM_EN),
            NameSrc::RandomTr => i18n::t(hacking::RANDOM_TR),
            NameSrc::Custom => i18n::t(hacking::CUSTOM),
        }
    }
}

struct Cfg {
    name_src: NameSrc,
    prefix: [u8; 16],
    prefix_len: usize,
    ble_mode_idx: usize,
    portal: [u8; 24],
    portal_len: usize,
}
impl Cfg {
    fn new() -> Self {
        let mut c = Cfg {
            name_src: NameSrc::RandomEn,
            prefix: [0; 16],
            prefix_len: 0,
            ble_mode_idx: 0,
            portal: [0; 24],
            portal_len: 0,
        };
        c.set_prefix(b"ATAKAN");
        c.set_portal(b"Free WiFi");
        c
    }
    fn set_prefix(&mut self, s: &[u8]) {
        let n = s.len().min(16);
        self.prefix[..n].copy_from_slice(&s[..n]);
        self.prefix_len = n;
    }
    fn set_portal(&mut self, s: &[u8]) {
        let n = s.len().min(24);
        self.portal[..n].copy_from_slice(&s[..n]);
        self.portal_len = n;
    }
}

/// Which text field the TextInput view edits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edit {
    Prefix,
    Portal,
    /// WiFi password for joining a secured AP (Camera Finder / LAN Scan).
    WifiPass,
}

/// A configurable setting row inside a tool's Settings screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CfgRow {
    NameSrc,
    CustomName,
    BleMode,
    PortalName,
}
fn cfg_rows(t: Tool) -> &'static [CfgRow] {
    match t {
        Tool::BeaconSpam | Tool::ProbeFlood => &[CfgRow::NameSrc, CfgRow::CustomName],
        Tool::BleSpam => &[CfgRow::BleMode],
        Tool::EvilPortal => &[CfgRow::PortalName],
        _ => &[],
    }
}

// ---- the three options on a tool's detail page ----
const DETAIL_OPTS: usize = 3; // Use / Wiki / Settings (Settings hidden if none)

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    List,
    Detail,
    Wiki,
    ToolCfg,
    TextInput,
    Confirm,
    WifiList,
    WifiAnalyze,
    BleList,
    Detector,
    Targets,
    Running,
    Done,
}

/// What `main` should do after a key press.
#[derive(Clone, Copy)]
pub enum Action {
    None,
    Redraw,
    Run(Tool),
    ScanTargets,
    Deauth,
    EvilTwin,
    Handshake,
    NetScan,
    CamScan,
    Portal,
    BleSpam(ble_spam::Mode),
}

#[derive(Clone, Copy)]
struct Ap {
    ssid: [u8; 32],
    ssid_len: u8,
    bssid: [u8; 6],
    rssi: i8,
    channel: u8,
    auth: &'static str,
    age: u8, // full-rescans since last seen (0 = fresh); >= STALE_AGE => [OFF]
}
impl Ap {
    const EMPTY: Ap = Ap { ssid: [0; 32], ssid_len: 0, bssid: [0; 6], rssi: 0, channel: 0, auth: "", age: 0 };
}

#[derive(Clone, Copy)]
struct Ble {
    addr: [u8; 6],
    rssi: i8,
    name: [u8; 20],
    name_len: u8,
    age: u8,
}
impl Ble {
    const EMPTY: Ble = Ble { addr: [0; 6], rssi: 0, name: [0; 20], name_len: 0, age: 0 };
}

/// Rescans a device may be missed before it is drawn as `[OFF]`.
const STALE_AGE: u8 = 3;

/// Which live-rescan the radar wants while a scanner view is open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RadarKind {
    Wifi,
    Ble,
}

#[derive(Clone, Copy, Default)]
struct Det {
    deauth: u32,
    disassoc: u32,
    beacon: u32,
    frames: u32,
}

const ROW_VISIBLE: usize = 6;
const LIST_VISIBLE: usize = 6;

pub struct Hacking {
    view: View,
    sel: usize,    // selection in the active list view (LIST index / detail opt / cfg row / ap)
    scroll: usize, // scroll offset for the active list view
    wiki_scroll: usize,
    pending: Tool, // the selected / active tool
    edit: Edit,
    aps: [Ap; MAX_APS],
    ap_count: usize,
    bles: [Ble; MAX_BLE],
    ble_count: usize,
    det: Det,
    attack_sent: u32,
    scan_failed: bool,
    cfg: Cfg,
    caps: bool, // "Aa" caps toggle for the SSID/portal name fields
    frame: u32, // radar sweep animation counter
    heading: f32, // gyro-integrated heading (deg); rotates the radar field
    fail_msg: Option<&'static str>, // specific failure reason shown instead of "radio error"
    wifi_pass: [u8; 64], // typed WiFi password for joining a secured AP / cracked key
    wifi_pass_len: usize,
    wifi_crack: bool, // true = user chose attack/crack (TAB) instead of a typed password
    hs_captured: bool, // last handshake attempt extracted a full msg1+msg2
}

impl Hacking {
    pub fn new() -> Self {
        Self {
            view: View::List,
            sel: 1,
            scroll: 0,
            wiki_scroll: 0,
            pending: Tool::WifiScan,
            edit: Edit::Prefix,
            aps: [Ap::EMPTY; MAX_APS],
            ap_count: 0,
            bles: [Ble::EMPTY; MAX_BLE],
            ble_count: 0,
            det: Det::default(),
            attack_sent: 0,
            scan_failed: false,
            cfg: Cfg::new(),
            caps: true, // SSID/portal names default to uppercase; "Aa" toggles
            frame: 0,
            heading: 0.0,
            fail_msg: None,
            wifi_pass: [0; 64],
            wifi_pass_len: 0,
            wifi_crack: false,
            hs_captured: false,
        }
    }

    /// Periodic animation tick (called ~25 Hz by main). Spins the radar sweep and
    /// rotates the blip field by the gyro yaw (turn the device -> radar turns with
    /// you) on the WiFi/BLE scanner views. Returns true when it repainted.
    pub fn tick<I: I2c, D: DrawTarget<Color = Rgb565>>(&mut self, i2c: &mut I, d: &mut D) -> bool {
        if !matches!(self.view, View::WifiList | View::BleList | View::Targets) {
            return false;
        }
        self.frame = self.frame.wrapping_add(1);
        // gyro yaw -> heading; a deadzone kills the still-bias drift
        if bmi270::ready() {
            if let Some(g) = bmi270::read_gyro(i2c) {
                let yaw = g[2]; // Z axis (vertical); flip sign / pick axis on-device
                if libm::fabsf(yaw) > 5.0 {
                    self.heading += yaw * 0.04; // dt ~= 40 ms
                    // per-frame delta is well under 360, so one adjust re-wraps
                    if self.heading >= 360.0 {
                        self.heading -= 360.0;
                    } else if self.heading < 0.0 {
                        self.heading += 360.0;
                    }
                }
            }
        }
        self.draw(d, false);
        true
    }

    /// Flip the caps state (driven by the "Aa" key) and refresh the indicator.
    pub fn toggle_caps<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        if self.view == View::TextInput {
            self.caps = !self.caps;
            self.draw(d, false);
        }
    }

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.view = View::List;
        self.sel = 1;
        self.scroll = 0;
        self.draw(d, true);
    }

    // ----------------- config getters (read by main) -----------------
    pub fn name_src(&self) -> NameSrc {
        self.cfg.name_src
    }
    pub fn prefix_owned(&self) -> ([u8; 16], usize) {
        (self.cfg.prefix, self.cfg.prefix_len)
    }
    pub fn ble_mode(&self) -> ble_spam::Mode {
        ble_spam::Mode::ALL[self.cfg.ble_mode_idx]
    }
    pub fn portal_ssid_owned(&self) -> ([u8; 24], usize) {
        (self.cfg.portal, self.cfg.portal_len)
    }

    /// Selected deauth/handshake target: (bssid, channel). None if not picking.
    pub fn target(&self) -> Option<([u8; 6], u8)> {
        if self.view == View::Targets && self.sel < self.ap_count {
            Some((self.aps[self.sel].bssid, self.aps[self.sel].channel))
        } else {
            None
        }
    }
    /// Selected target's SSID copied out (for evil twin / LAN scan) + channel.
    /// Also valid on the WiFi-password input screen (same picked AP, view changed).
    pub fn target_ssid_owned(&self) -> Option<([u8; 32], usize, u8)> {
        let on_pass = self.view == View::TextInput && self.edit == Edit::WifiPass;
        if (self.view == View::Targets || on_pass) && self.sel < self.ap_count {
            let ap = &self.aps[self.sel];
            Some((ap.ssid, ap.ssid_len as usize, ap.channel))
        } else {
            None
        }
    }
    /// For Camera Finder / LAN Scan join: the WiFi credential the user chose.
    /// `None` = attack/crack (TAB); `Some(pass)` = join with this password
    /// (empty string = open network).
    pub fn wifi_known(&self) -> Option<([u8; 64], usize)> {
        if self.wifi_crack {
            None
        } else {
            Some((self.wifi_pass, self.wifi_pass_len))
        }
    }
    pub fn attack_title(&self) -> &'static str {
        self.pending.name()
    }

    /// Back one level. true = stayed inside Hacking; false = pop to the app menu.
    pub fn back<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        match self.view {
            View::List => return false,
            View::Detail => {
                self.view = View::List;
            }
            View::TextInput => {
                if self.edit == Edit::WifiPass {
                    // password screen came from the AP picker -> go back there
                    self.view = View::Targets;
                } else {
                    self.view = View::ToolCfg;
                    self.sel = 0;
                }
            }
            // Wiki / ToolCfg / Confirm / result screens -> the tool's detail page
            _ => {
                self.view = View::Detail;
                self.sel = 0;
            }
        }
        self.draw(d, true);
        true
    }

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        match self.view {
            View::List => self.key_list(rc, d),
            View::Detail => self.key_detail(rc, d),
            View::Wiki => self.key_wiki(rc, d),
            View::ToolCfg => self.key_cfg(rc, d),
            View::TextInput => self.key_text(rc, d),
            View::Confirm => self.key_confirm(rc),
            View::Targets => self.key_targets(rc, d),
            View::WifiList => self.key_rescan(rc, Tool::WifiScan),
            View::WifiAnalyze => self.key_rescan(rc, Tool::WifiAnalyze),
            View::BleList => self.key_rescan(rc, Tool::BleScan),
            View::Detector => self.key_rescan(rc, Tool::Detector),
            View::Done => self.key_done(rc),
            View::Running => Action::None,
        }
    }

    // ----------------------------- List -----------------------------
    fn key_list<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        match rc {
            crate::K_UP => {
                self.sel = prev_tool_row(self.sel);
                self.fix_list_scroll();
                self.draw(d, false);
                Action::Redraw
            }
            crate::K_DOWN => {
                self.sel = next_tool_row(self.sel);
                self.fix_list_scroll();
                self.draw(d, false);
                Action::Redraw
            }
            crate::K_ENTER => {
                if let LRow::Tool(t) = LIST[self.sel] {
                    self.pending = t;
                    self.view = View::Detail;
                    self.sel = 0;
                    self.draw(d, true);
                }
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn fix_list_scroll(&mut self) {
        // keep the selected row (and its header just above) visible
        if self.sel <= self.scroll {
            self.scroll = self.sel.saturating_sub(1);
        } else if self.sel >= self.scroll + LIST_VISIBLE {
            self.scroll = self.sel + 1 - LIST_VISIBLE;
        }
    }

    // ---------------------------- Detail ----------------------------
    fn detail_opts(&self) -> usize {
        if self.pending.has_settings() {
            DETAIL_OPTS
        } else {
            2
        }
    }
    fn key_detail<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        let n = self.detail_opts();
        match rc {
            crate::K_UP => {
                self.sel = if self.sel == 0 { n - 1 } else { self.sel - 1 };
                self.draw(d, false);
                Action::Redraw
            }
            crate::K_DOWN => {
                self.sel = (self.sel + 1) % n;
                self.draw(d, false);
                Action::Redraw
            }
            crate::K_ENTER => match self.sel {
                0 => {
                    // Use
                    if self.pending.offensive() {
                        self.view = View::Confirm;
                        self.draw(d, true);
                        Action::Redraw
                    } else {
                        self.dispatch(self.pending)
                    }
                }
                1 => {
                    // Wiki
                    self.view = View::Wiki;
                    self.wiki_scroll = 0;
                    self.draw(d, true);
                    Action::Redraw
                }
                _ => {
                    // Settings
                    self.view = View::ToolCfg;
                    self.sel = 0;
                    self.draw(d, true);
                    Action::Redraw
                }
            },
            _ => Action::None,
        }
    }

    /// Turn a chosen tool into the action main should run.
    fn dispatch(&mut self, tool: Tool) -> Action {
        self.pending = tool;
        match tool {
            Tool::WifiScan | Tool::WifiAnalyze | Tool::BleScan | Tool::Detector
            | Tool::BeaconSpam | Tool::ProbeFlood => Action::Run(tool),
            Tool::BleSpam => Action::BleSpam(self.ble_mode()),
            Tool::Deauth | Tool::EvilTwin | Tool::Handshake | Tool::NetScan | Tool::CamScan => Action::ScanTargets,
            Tool::EvilPortal => Action::Portal,
        }
    }

    fn key_confirm(&mut self, rc: (u8, u8)) -> Action {
        match rc {
            crate::K_ENTER => self.dispatch(self.pending),
            _ => Action::None,
        }
    }

    // ----------------------------- Wiki -----------------------------
    fn key_wiki<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        let lines = wiki::get(self.pending).lines().count();
        match rc {
            crate::K_UP => {
                if self.wiki_scroll > 0 {
                    self.wiki_scroll -= 1;
                    self.draw(d, true);
                }
                Action::Redraw
            }
            crate::K_DOWN => {
                if self.wiki_scroll + WIKI_VISIBLE < lines {
                    self.wiki_scroll += 1;
                    self.draw(d, true);
                }
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    // -------------------------- Tool Settings -----------------------
    fn key_cfg<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        let rows = cfg_rows(self.pending);
        if rows.is_empty() {
            return Action::None;
        }
        match rc {
            crate::K_UP => {
                self.sel = if self.sel == 0 { rows.len() - 1 } else { self.sel - 1 };
                self.draw(d, false);
                Action::Redraw
            }
            crate::K_DOWN => {
                self.sel = (self.sel + 1) % rows.len();
                self.draw(d, false);
                Action::Redraw
            }
            crate::K_LEFT => {
                self.cfg_cycle(rows[self.sel], false, d);
                Action::Redraw
            }
            crate::K_RIGHT => {
                self.cfg_cycle(rows[self.sel], true, d);
                Action::Redraw
            }
            crate::K_ENTER => {
                match rows[self.sel] {
                    CfgRow::CustomName => {
                        self.edit = Edit::Prefix;
                        self.view = View::TextInput;
                        self.draw(d, true);
                    }
                    CfgRow::PortalName => {
                        self.edit = Edit::Portal;
                        self.view = View::TextInput;
                        self.draw(d, true);
                    }
                    other => self.cfg_cycle(other, true, d), // toggle-style rows
                }
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn cfg_cycle<D: DrawTarget<Color = Rgb565>>(&mut self, row: CfgRow, fwd: bool, d: &mut D) {
        match row {
            CfgRow::NameSrc => {
                self.cfg.name_src = match (self.cfg.name_src, fwd) {
                    (NameSrc::RandomEn, true) => NameSrc::RandomTr,
                    (NameSrc::RandomTr, true) => NameSrc::Custom,
                    (NameSrc::Custom, true) => NameSrc::RandomEn,
                    (NameSrc::RandomEn, false) => NameSrc::Custom,
                    (NameSrc::RandomTr, false) => NameSrc::RandomEn,
                    (NameSrc::Custom, false) => NameSrc::RandomTr,
                };
            }
            CfgRow::BleMode => {
                let n = ble_spam::Mode::ALL.len();
                self.cfg.ble_mode_idx = if fwd {
                    (self.cfg.ble_mode_idx + 1) % n
                } else {
                    (self.cfg.ble_mode_idx + n - 1) % n
                };
            }
            CfgRow::CustomName | CfgRow::PortalName => {}
        }
        self.draw(d, false);
    }

    // --------------------------- Text input -------------------------
    fn key_text<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        // WiFi password screen (Camera Finder / LAN Scan on a secured AP):
        // ENTER = join with the typed password, TAB = attack/crack instead.
        if self.edit == Edit::WifiPass {
            if rc == crate::K_ENTER || rc == keymap::K_TAB {
                self.wifi_crack = rc == keymap::K_TAB;
                // leave the view as-is so main can still read the picked SSID + cred
                return if matches!(self.pending, Tool::NetScan) {
                    Action::NetScan
                } else {
                    Action::CamScan
                };
            }
            if rc == keymap::K_BKSP {
                self.wifi_pass_len = self.wifi_pass_len.saturating_sub(1);
                self.draw(d, false);
                return Action::Redraw;
            }
            if let Some(b) = keymap::ch_shift(rc.0, rc.1, self.caps) {
                if self.wifi_pass_len < self.wifi_pass.len() {
                    self.wifi_pass[self.wifi_pass_len] = b;
                    self.wifi_pass_len += 1;
                }
                self.draw(d, false);
            }
            return Action::Redraw;
        }
        if rc == crate::K_ENTER {
            self.view = View::ToolCfg;
            self.sel = 0;
            self.draw(d, true);
            return Action::Redraw;
        }
        if rc == keymap::K_BKSP {
            match self.edit {
                Edit::Prefix => self.cfg.prefix_len = self.cfg.prefix_len.saturating_sub(1),
                Edit::Portal => self.cfg.portal_len = self.cfg.portal_len.saturating_sub(1),
                Edit::WifiPass => {} // handled in the early branch above
            }
            self.draw(d, false);
            return Action::Redraw;
        }
        if let Some(b) = keymap::ch_shift(rc.0, rc.1, self.caps) {
            match self.edit {
                Edit::Prefix => {
                    if self.cfg.prefix_len < self.cfg.prefix.len() {
                        self.cfg.prefix[self.cfg.prefix_len] = b;
                        self.cfg.prefix_len += 1;
                    }
                }
                Edit::Portal => {
                    if self.cfg.portal_len < self.cfg.portal.len() {
                        self.cfg.portal[self.cfg.portal_len] = b;
                        self.cfg.portal_len += 1;
                    }
                }
                Edit::WifiPass => {} // handled in the early branch above
            }
            self.draw(d, false);
        }
        Action::Redraw
    }

    // --------------------------- Targets ----------------------------
    fn key_targets<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) -> Action {
        match rc {
            crate::K_UP => {
                if self.ap_count > 0 {
                    self.sel = if self.sel == 0 { self.ap_count - 1 } else { self.sel - 1 };
                    self.scroll = clamp(self.sel, self.scroll, ROW_VISIBLE);
                    self.draw(d, false);
                }
                Action::Redraw
            }
            crate::K_DOWN => {
                if self.ap_count > 0 {
                    self.sel = (self.sel + 1) % self.ap_count;
                    self.scroll = clamp(self.sel, self.scroll, ROW_VISIBLE);
                    self.draw(d, false);
                }
                Action::Redraw
            }
            crate::K_ENTER => {
                if self.ap_count == 0 {
                    return Action::None;
                }
                match self.pending {
                    Tool::EvilTwin => Action::EvilTwin,
                    Tool::Handshake => Action::Handshake,
                    // Camera Finder / LAN Scan JOIN the network -> if it's secured,
                    // ask for the password (ENTER = join with it, TAB = attack/crack).
                    Tool::NetScan | Tool::CamScan => {
                        self.wifi_pass_len = 0;
                        self.wifi_crack = false;
                        if self.aps[self.sel].auth != "open" {
                            self.edit = Edit::WifiPass;
                            self.view = View::TextInput;
                            self.draw(d, true);
                            Action::Redraw
                        } else if matches!(self.pending, Tool::NetScan) {
                            Action::NetScan
                        } else {
                            Action::CamScan
                        }
                    }
                    _ => Action::Deauth,
                }
            }
            _ => Action::None,
        }
    }

    /// ENTER re-runs the tool that produced the current result screen.
    fn key_rescan(&mut self, rc: (u8, u8), tool: Tool) -> Action {
        match rc {
            crate::K_ENTER => Action::Run(tool),
            // WiFi scanner is a radar: UP/DOWN move the highlighted blip.
            crate::K_UP if self.view == View::WifiList => {
                if self.ap_count > 0 {
                    self.sel = (self.sel + self.ap_count - 1) % self.ap_count;
                }
                Action::Redraw
            }
            crate::K_DOWN if self.view == View::WifiList => {
                if self.ap_count > 0 {
                    self.sel = (self.sel + 1) % self.ap_count;
                }
                Action::Redraw
            }
            // BLE scanner is a radar too: UP/DOWN move the highlighted blip.
            crate::K_UP if self.view == View::BleList => {
                if self.ble_count > 0 {
                    self.sel = (self.sel + self.ble_count - 1) % self.ble_count;
                }
                Action::Redraw
            }
            crate::K_DOWN if self.view == View::BleList => {
                if self.ble_count > 0 {
                    self.sel = (self.sel + 1) % self.ble_count;
                }
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn key_done(&mut self, rc: (u8, u8)) -> Action {
        match rc {
            crate::K_ENTER => match self.pending {
                Tool::Deauth | Tool::EvilTwin | Tool::Handshake | Tool::NetScan | Tool::CamScan => Action::ScanTargets,
                Tool::EvilPortal => Action::Portal,
                Tool::BleSpam => Action::BleSpam(self.ble_mode()),
                other => Action::Run(other),
            },
            _ => Action::None,
        }
    }

    // ----------------- result intake (called by main) -----------------
    pub fn begin_wifi_results(&mut self) {
        self.ap_count = 0;
        self.scroll = 0;
        self.sel = 0;
        self.scan_failed = false;
    }
    pub fn push_ap(&mut self, ssid: &str, bssid: [u8; 6], rssi: i8, channel: u8, auth: &'static str) {
        if self.ap_count >= MAX_APS {
            return;
        }
        let rec = &mut self.aps[self.ap_count];
        let b = ssid.as_bytes();
        let n = b.len().min(32);
        rec.ssid[..n].copy_from_slice(&b[..n]);
        rec.ssid_len = n as u8;
        rec.bssid = bssid;
        rec.rssi = rssi;
        rec.channel = channel;
        rec.auth = auth;
        rec.age = 0;
        self.ap_count += 1;
    }

    // ---------------- live radar rescan (driven by main) ----------------
    /// Which live rescan the current view wants, if any (radar screens only).
    pub fn radar_kind(&self) -> Option<RadarKind> {
        match self.view {
            View::WifiList | View::Targets => Some(RadarKind::Wifi),
            View::BleList => Some(RadarKind::Ble),
            _ => None,
        }
    }
    /// Channel of the highlighted AP (for the fast single-channel track), if any.
    pub fn selected_channel(&self) -> Option<u8> {
        if matches!(self.view, View::WifiList | View::Targets) && self.sel < self.ap_count {
            Some(self.aps[self.sel].channel)
        } else {
            None
        }
    }
    /// Age every known AP by one before a full-scan merge (un-refreshed ones go [OFF]).
    pub fn begin_wifi_merge(&mut self) {
        for ap in self.aps[..self.ap_count].iter_mut() {
            ap.age = ap.age.saturating_add(1);
        }
    }
    /// Merge one scanned AP: refresh it if known (by BSSID), else append it.
    pub fn merge_ap(&mut self, ssid: &str, bssid: [u8; 6], rssi: i8, channel: u8, auth: &'static str) {
        for i in 0..self.ap_count {
            if self.aps[i].bssid == bssid {
                self.aps[i].rssi = rssi;
                self.aps[i].channel = channel;
                self.aps[i].age = 0;
                return;
            }
        }
        self.push_ap(ssid, bssid, rssi, channel, auth);
    }
    pub fn begin_ble_merge(&mut self) {
        for d in self.bles[..self.ble_count].iter_mut() {
            d.age = d.age.saturating_add(1);
        }
    }
    pub fn merge_ble(&mut self, addr: [u8; 6], rssi: i8, name: Option<&str>) {
        for i in 0..self.ble_count {
            if self.bles[i].addr == addr {
                self.bles[i].rssi = rssi;
                self.bles[i].age = 0;
                return;
            }
        }
        self.push_ble(addr, rssi, name);
    }
    pub fn show_wifi<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.view = View::WifiList;
        self.scroll = 0;
        self.draw(d, true);
    }
    pub fn show_analyzer<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.view = View::WifiAnalyze;
        self.draw(d, true);
    }
    pub fn show_targets<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.view = View::Targets;
        self.sel = 0;
        self.scroll = 0;
        self.draw(d, true);
    }

    pub fn begin_ble_results(&mut self) {
        self.ble_count = 0;
        self.scroll = 0;
        self.scan_failed = false;
    }
    pub fn push_ble(&mut self, addr: [u8; 6], rssi: i8, name: Option<&str>) {
        if self.ble_count >= MAX_BLE {
            return;
        }
        let rec = &mut self.bles[self.ble_count];
        rec.addr = addr;
        rec.rssi = rssi;
        rec.age = 0;
        rec.name_len = 0;
        if let Some(n) = name {
            let b = n.as_bytes();
            let len = b.len().min(20);
            rec.name[..len].copy_from_slice(&b[..len]);
            rec.name_len = len as u8;
        }
        self.ble_count += 1;
    }
    pub fn show_ble<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.view = View::BleList;
        self.scroll = 0;
        self.sel = 0;
        self.draw(d, true);
    }

    pub fn set_detector_results(&mut self, deauth: u32, disassoc: u32, beacon: u32, frames: u32) {
        self.det = Det { deauth, disassoc, beacon, frames };
        self.scan_failed = false;
    }
    pub fn show_detector<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.view = View::Detector;
        self.draw(d, true);
    }

    pub fn show_attack_done<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, sent: Option<u32>) {
        match sent {
            Some(n) => {
                self.attack_sent = n;
                self.scan_failed = false;
                self.fail_msg = None;
            }
            None => self.scan_failed = true,
        }
        self.view = View::Done;
        self.draw(d, true);
    }

    /// Result intake for the handshake capture + offline crack. `eapol` = frames
    /// seen, `captured` = a full handshake was extracted, `cracked` = the recovered
    /// passphrase (stored in `wifi_pass` so the Done screen can show it).
    pub fn show_handshake<D: DrawTarget<Color = Rgb565>>(
        &mut self,
        d: &mut D,
        eapol: u32,
        captured: bool,
        cracked: Option<&str>,
    ) {
        self.attack_sent = eapol;
        self.hs_captured = captured;
        self.scan_failed = false;
        self.fail_msg = None;
        self.wifi_pass_len = 0;
        if let Some(p) = cracked {
            let b = p.as_bytes();
            let n = b.len().min(64);
            self.wifi_pass[..n].copy_from_slice(&b[..n]);
            self.wifi_pass_len = n;
        }
        self.view = View::Done;
        self.draw(d, true);
    }

    /// Show a SPECIFIC failure reason (e.g. "assoc fail", "no DHCP lease") instead
    /// of the generic "radio error" — so the user sees what actually went wrong.
    pub fn show_attack_failed<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, msg: &'static str) {
        self.fail_msg = Some(msg);
        self.scan_failed = true;
        self.view = View::Done;
        self.draw(d, true);
    }

    pub fn set_scan_failed(&mut self) {
        self.scan_failed = true;
    }


    pub fn set_running(&mut self) {
        self.view = View::Running;
    }


    // ----------------------------- drawing -----------------------------
    pub fn draw_busy<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, title: &str, msg: &str) {
        theme::clear(d);
        theme::topbar(d, title);
        theme::text_center(d, msg, theme::W / 2, theme::H / 2 - 6, theme::TITLE_FONT, theme::accent());
        theme::text_center(d, i18n::t(hacking::PLEASE_WAIT), theme::W / 2, theme::H / 2 + 10, theme::BODY_FONT, theme::MUTED);
    }

    pub fn draw<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        match self.view {
            View::List => self.draw_list(d, clear),
            View::Detail => self.draw_detail(d, clear),
            View::Wiki => self.draw_wiki(d),
            View::ToolCfg => self.draw_cfg(d, clear),
            View::TextInput => self.draw_text(d),
            View::Confirm => self.draw_confirm(d, clear),
            View::WifiList => self.draw_aplist(d, clear, Tool::WifiScan.name(), false),
            View::Targets => {
                let title = self.pending.target_title();
                self.draw_aplist(d, clear, title, true)
            }
            View::WifiAnalyze => self.draw_analyzer(d, clear),
            View::BleList => self.draw_ble(d, clear),
            View::Detector => self.draw_detector(d, clear),
            View::Done => self.draw_done(d, clear),
            View::Running => {}
        }
    }

    fn draw_list<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
            theme::topbar(d, "Hacking");
        }
        for vis in 0..LIST_VISIBLE {
            let y = 22 + vis as i32 * 16;
            theme::fill(d, 0, y - 1, theme::W as u32, 15, theme::BG);
            let idx = self.scroll + vis;
            if idx >= LIST.len() {
                continue;
            }
            match &LIST[idx] {
                LRow::Head(diff) => {
                    theme::text(d, diff.label(), theme::PAD, y + 1, theme::BODY_FONT, diff.color());
                    theme::hline(d, y + 11, theme::BORDER);
                }
                LRow::Tool(t) => {
                    let selected = idx == self.sel;
                    if selected {
                        theme::card(d, theme::PAD, y - 1, (theme::W - 2 * theme::PAD) as u32, 14, Some(theme::accent()));
                    }
                    // difficulty dot
                    theme::fill(d, theme::PAD + 5, y + 4, 4, 4, t.difficulty().color());
                    let name_col = if selected { theme::FG } else { theme::MUTED };
                    theme::text(d, t.name(), theme::PAD + 14, y + 1, theme::BODY_FONT, name_col);
                    if t.offensive() {
                        theme::text_right(d, "ATK", theme::W - theme::PAD - 14, y + 1, theme::BODY_FONT, theme::DESTRUCTIVE);
                    }
                    if selected {
                        theme::text_right(d, ">", theme::W - theme::PAD - 4, y + 1, theme::BODY_FONT, theme::accent());
                    }
                }
            }
        }
        theme::hint(d, i18n::t(hacking::ENTER_OPEN_BACK));
    }

    fn draw_detail<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
            theme::topbar(d, self.pending.name());
            // difficulty badge under the title
            let diff = self.pending.difficulty();
            theme::text(d, diff.label(), theme::PAD, 20, theme::BODY_FONT, diff.color());
            if self.pending.offensive() {
                theme::text_right(d, i18n::t(hacking::ATTACK), theme::W - theme::PAD, 20, theme::BODY_FONT, theme::DESTRUCTIVE);
            }
        }
        let opts = self.detail_opts();
        let labels = [
            i18n::t(hacking::USE_TOOL),
            i18n::t(hacking::WIKI),
            i18n::t(hacking::SETTINGS),
        ];
        for i in 0..opts {
            let y = 40 + i as i32 * 22;
            theme::fill(d, 0, y - 1, theme::W as u32, 21, theme::BG);
            let selected = i == self.sel;
            if selected {
                theme::card(d, theme::PAD, y, (theme::W - 2 * theme::PAD) as u32, 19, Some(theme::accent()));
            }
            let col = if selected { theme::FG } else { theme::MUTED };
            theme::text(d, labels[i], theme::PAD + 10, y + 5, theme::TITLE_FONT, col);
            if selected {
                theme::text_right(d, ">", theme::W - theme::PAD - 8, y + 5, theme::TITLE_FONT, theme::accent());
            }
        }
        theme::hint(d, i18n::t(hacking::ENTER_SELECT_BACK));
    }

    fn draw_wiki<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, self.pending.name());
        let body = wiki::get(self.pending);
        let total = body.lines().count();
        for (row, line) in body.lines().skip(self.wiki_scroll).take(WIKI_VISIBLE).enumerate() {
            let y = 22 + row as i32 * 12;
            theme::text(d, line, theme::PAD, y, theme::BODY_FONT, theme::FG);
        }
        if self.wiki_scroll + WIKI_VISIBLE < total {
            theme::text_right(d, "v", theme::W - theme::PAD, theme::HINT_Y - 14, theme::BODY_FONT, theme::accent());
        }
        theme::hint(d, i18n::t(hacking::SCROLL_BACK));
    }

    fn draw_cfg<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
            theme::topbar(d, self.pending.name());
            theme::text(d, i18n::t(hacking::SETTINGS), theme::PAD, 20, theme::BODY_FONT, theme::accent());
        }
        let rows = cfg_rows(self.pending);
        for (i, row) in rows.iter().enumerate() {
            let y = 36 + i as i32 * 16;
            theme::fill(d, 0, y - 1, theme::W as u32, 15, theme::BG);
            let selected = i == self.sel;
            if selected {
                theme::fill(d, theme::PAD, y - 1, (theme::W - 2 * theme::PAD) as u32, 14, theme::SURFACE2);
                theme::fill(d, theme::PAD, y - 1, 3, 14, theme::accent());
            }
            let lc = if selected { theme::FG } else { theme::MUTED };
            let vc = if selected { theme::accent() } else { theme::MUTED };
            let (label, value) = self.cfg_row_text(*row);
            theme::text(d, label, theme::PAD + 9, y + 2, theme::BODY_FONT, lc);
            theme::text_right(d, value, theme::W - theme::PAD - 8, y + 2, theme::BODY_FONT, vc);
        }
        theme::hint(d, i18n::t(hacking::CFG_CHANGE_EDIT_BACK));
    }

    /// (label, value) for a settings row. Value strings borrow self's buffers.
    fn cfg_row_text(&self, row: CfgRow) -> (&str, &str) {
        match row {
            CfgRow::NameSrc => (i18n::t(hacking::NAME_SOURCE), self.cfg.name_src.label()),
            CfgRow::CustomName => (
                i18n::t(hacking::CUSTOM_NAME),
                core::str::from_utf8(&self.cfg.prefix[..self.cfg.prefix_len]).unwrap_or("?"),
            ),
            CfgRow::BleMode => (i18n::t(hacking::MODE), self.ble_mode().label()),
            CfgRow::PortalName => (
                i18n::t(hacking::AP_NAME),
                core::str::from_utf8(&self.cfg.portal[..self.cfg.portal_len]).unwrap_or("?"),
            ),
        }
    }

    fn draw_text<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        theme::clear(d);
        let (title, buf): (&str, &str) = match self.edit {
            Edit::Prefix => (
                i18n::t(hacking::CUSTOM_SSID_NAME),
                core::str::from_utf8(&self.cfg.prefix[..self.cfg.prefix_len]).unwrap_or(""),
            ),
            Edit::Portal => (
                i18n::t(hacking::PORTAL_AP_NAME),
                core::str::from_utf8(&self.cfg.portal[..self.cfg.portal_len]).unwrap_or(""),
            ),
            Edit::WifiPass => (
                i18n::t(hacking::WIFI_PASS_TITLE),
                core::str::from_utf8(&self.wifi_pass[..self.wifi_pass_len]).unwrap_or(""),
            ),
        };
        theme::topbar(d, title);
        // input box
        theme::card(d, theme::PAD, 44, (theme::W - 2 * theme::PAD) as u32, 22, Some(theme::accent()));
        let shown: alloc::string::String = alloc::format!("{}_", buf);
        theme::text(d, &shown, theme::PAD + 8, 51, theme::TITLE_FONT, theme::FG);
        // caps indicator in the box's right edge (the "Aa" key toggles it)
        theme::text_right(d, if self.caps { "ABC" } else { "abc" }, theme::W - theme::PAD - 6, 51, theme::BODY_FONT, theme::MUTED);
        if matches!(self.edit, Edit::Prefix) {
            theme::text_center(d, i18n::t(hacking::BECOMES_NAME), theme::W / 2, 78, theme::BODY_FONT, theme::MUTED);
        } else if matches!(self.edit, Edit::WifiPass) {
            theme::text_center(d, i18n::t(hacking::WIFI_PASS_NOTE), theme::W / 2, 78, theme::BODY_FONT, theme::MUTED);
        }
        let hint = if matches!(self.edit, Edit::WifiPass) {
            i18n::t(hacking::WIFI_PASS_HINT)
        } else {
            i18n::t(hacking::TYPE_BKSP_OK_CANCEL)
        };
        theme::hint(d, hint);
    }

    fn draw_confirm<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
        }
        theme::topbar(d, self.pending.name());
        theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);
        theme::text_center(d, i18n::t(hacking::ACTIVE_ATTACK), theme::W / 2, 44, theme::TITLE_FONT, theme::DESTRUCTIVE);
        theme::text_center(d, self.pending.name(), theme::W / 2, 66, theme::BODY_FONT, theme::MUTED);
        theme::hint(d, i18n::t(hacking::ENTER_START_CANCEL));
    }

    fn draw_aplist<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool, title: &str, select: bool) {
        if clear {
            theme::clear(d);
        }
        theme::topbar(d, title);
        theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);

        if self.scan_failed {
            self.draw_failed(d);
            return;
        }
        if self.ap_count == 0 {
            theme::text_center(d, i18n::t(hacking::NO_NETWORKS_FOUND), theme::W / 2, theme::H / 2, theme::BODY_FONT, theme::MUTED);
            theme::hint(d, i18n::t(hacking::ENTER_RUN_AGAIN_BACK));
            return;
        }

        let sel = self.sel.min(self.ap_count - 1);
        // radar backdrop with the spinning sweep (animated by tick())
        radar_frame(d, (self.frame.wrapping_mul(6) % 360) as f32);

        // plot every AP: angle from BSSID, radius from RSSI (near centre = strong)
        for i in 0..self.ap_count {
            let ap = &self.aps[i];
            let (x, y) = polar(RADAR_CX, RADAR_CY, rssi_frac(ap.rssi) * RADAR_R as f32, addr_angle(&ap.bssid) + self.heading);
            let open = ap.auth == "open";
            if i == sel {
                theme::ring(d, x, y, 4, theme::accent());
                theme::disc(d, x, y, 2, theme::accent());
            } else if ap.age >= STALE_AGE {
                theme::disc(d, x, y, 1, theme::FAINT); // gone/[OFF]
            } else {
                let col = if open { RADAR_GREEN } else { theme::MUTED };
                theme::disc(d, x, y, if open { 2 } else { 1 }, col);
            }
        }

        // info panel for the highlighted AP
        let ap = &self.aps[sel];
        let name: alloc::string::String = if ap.ssid_len == 0 {
            alloc::string::String::from(i18n::t(hacking::HIDDEN))
        } else {
            core::str::from_utf8(&ap.ssid[..ap.ssid_len as usize]).unwrap_or("?").chars().take(20).collect()
        };
        theme::text(d, &name, PANEL_X, 24, theme::BODY_FONT, theme::accent());
        let off = if ap.age >= STALE_AGE { " [OFF]" } else { "" };
        theme::text(d, &alloc::format!("{} dBm{}", ap.rssi, off), PANEL_X, 40, theme::BODY_FONT, theme::FG);
        theme::text(d, &alloc::format!("~{} m", rssi_meters(ap.rssi)), PANEL_X, 52, theme::BODY_FONT, RADAR_GREEN);
        theme::text(d, &alloc::format!("ch{}  {}", ap.channel, ap.auth), PANEL_X, 64, theme::BODY_FONT, theme::MUTED);
        theme::text(d, &alloc::format!("{}/{}", sel + 1, self.ap_count), PANEL_X, 80, theme::BODY_FONT, theme::MUTED);
        // legend
        theme::disc(d, PANEL_X + 2, 99, 2, RADAR_GREEN);
        theme::text(d, "open", PANEL_X + 8, 95, theme::BODY_FONT, theme::MUTED);
        theme::disc(d, PANEL_X + 2, 110, 1, theme::MUTED);
        theme::text(d, "wpa", PANEL_X + 8, 106, theme::BODY_FONT, theme::MUTED);

        if select {
            let h = alloc::format!("{}   {} {}   ESC", i18n::t(hacking::MOVE), i18n::t(hacking::ENTER), self.pending.target_verb());
            theme::hint(d, &h);
        } else {
            let h = alloc::format!("{}  {} {}  ENTER {}  ESC", i18n::t(hacking::MOVE), self.ap_count, i18n::t(hacking::NETS), i18n::t(hacking::RESCAN));
            theme::hint(d, &h);
        }
    }

    fn draw_analyzer<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
        }
        theme::topbar(d, Tool::WifiAnalyze.name());
        theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);

        if self.scan_failed {
            self.draw_failed(d);
            return;
        }
        let mut counts = [0u16; 14];
        for i in 0..self.ap_count {
            let ch = self.aps[i].channel as usize;
            if (1..=13).contains(&ch) {
                counts[ch] += 1;
            }
        }
        let maxc = counts.iter().copied().max().unwrap_or(0).max(1);
        let base_y = 104i32;
        let top_y = 28i32;
        let max_h = base_y - top_y;
        let step = 17i32;
        let x0 = theme::PAD + 2;
        for ch in 1..=13usize {
            let x = x0 + (ch as i32 - 1) * step;
            let h = (counts[ch] as i32 * max_h) / maxc as i32;
            let busiest = counts[ch] > 0 && counts[ch] == counts.iter().copied().max().unwrap_or(0);
            let col = if busiest { theme::accent() } else { theme::MUTED };
            if h > 0 {
                theme::fill(d, x, base_y - h, 13, h as u32, col);
            }
            let lbl = alloc::format!("{}", ch);
            let lcol = if ch == 1 || ch == 6 || ch == 11 { theme::FG } else { theme::FAINT };
            theme::text(d, &lbl, x, base_y + 3, theme::BODY_FONT, lcol);
        }
        theme::hline(d, base_y + 1, theme::BORDER);
        let hint = alloc::format!("{} {}   ENTER {}   ESC", self.ap_count, i18n::t(hacking::NETS), i18n::t(hacking::RESCAN));
        theme::hint(d, &hint);
    }

    fn draw_ble<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
        }
        theme::topbar(d, Tool::BleScan.name());
        theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);

        if self.scan_failed {
            self.draw_failed(d);
            return;
        }
        if self.ble_count == 0 {
            theme::text_center(d, i18n::t(hacking::NO_DEVICES_FOUND), theme::W / 2, theme::H / 2, theme::BODY_FONT, theme::MUTED);
            theme::hint(d, i18n::t(hacking::ENTER_RUN_AGAIN_BACK));
            return;
        }

        let sel = self.sel.min(self.ble_count - 1);
        radar_frame(d, (self.frame.wrapping_mul(6) % 360) as f32);

        // plot every BLE device: angle from MAC, radius from RSSI
        for i in 0..self.ble_count {
            let dev = &self.bles[i];
            // declutter: in a crowded room hide the weak/distant ones (keep the
            // selected + the near/strong), so the radar stays readable.
            if self.ble_count > 24 && i != sel && dev.rssi < -85 {
                continue;
            }
            let (x, y) = polar(RADAR_CX, RADAR_CY, rssi_frac(dev.rssi) * RADAR_R as f32, addr_angle(&dev.addr) + self.heading);
            let named = dev.name_len > 0;
            if i == sel {
                theme::ring(d, x, y, 4, theme::accent());
                theme::disc(d, x, y, 2, theme::accent());
            } else if dev.age >= STALE_AGE {
                theme::disc(d, x, y, 1, theme::FAINT);
            } else {
                let col = if named { RADAR_GREEN } else { theme::MUTED };
                theme::disc(d, x, y, if named { 2 } else { 1 }, col);
            }
        }

        // info panel for the highlighted device
        let dev = &self.bles[sel];
        let name: alloc::string::String = if dev.name_len == 0 {
            let a = dev.addr;
            alloc::format!("{:02X}:{:02X}:{:02X}:{:02X}", a[2], a[3], a[4], a[5])
        } else {
            core::str::from_utf8(&dev.name[..dev.name_len as usize]).unwrap_or("?").chars().take(20).collect()
        };
        theme::text(d, &name, PANEL_X, 24, theme::BODY_FONT, theme::accent());
        let off = if dev.age >= STALE_AGE { " [OFF]" } else { "" };
        theme::text(d, &alloc::format!("{} dBm{}", dev.rssi, off), PANEL_X, 40, theme::BODY_FONT, theme::FG);
        theme::text(d, &alloc::format!("~{} m", ble_meters(dev.rssi)), PANEL_X, 52, theme::BODY_FONT, RADAR_GREEN);
        let a = dev.addr;
        theme::text(d, &alloc::format!("{:02X}:{:02X}:{:02X}", a[3], a[4], a[5]), PANEL_X, 64, theme::BODY_FONT, theme::MUTED);
        theme::text(d, &alloc::format!("{}/{}", sel + 1, self.ble_count), PANEL_X, 80, theme::BODY_FONT, theme::MUTED);
        // legend
        theme::disc(d, PANEL_X + 2, 99, 2, RADAR_GREEN);
        theme::text(d, "named", PANEL_X + 8, 95, theme::BODY_FONT, theme::MUTED);
        theme::disc(d, PANEL_X + 2, 110, 1, theme::MUTED);
        theme::text(d, "anon", PANEL_X + 8, 106, theme::BODY_FONT, theme::MUTED);

        let hint = alloc::format!("{}   {} {}   ENTER {}   ESC", i18n::t(hacking::MOVE), self.ble_count, i18n::t(hacking::DEV), i18n::t(hacking::RESCAN));
        theme::hint(d, &hint);
    }

    fn draw_detector<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
        }
        theme::topbar(d, Tool::Detector.name());
        theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);

        if self.scan_failed {
            self.draw_failed(d);
            return;
        }
        let det = self.det;
        let attack = det.deauth + det.disassoc;
        if attack >= 3 {
            theme::text_center(d, i18n::t(hacking::DEAUTH_ATTACK_ALERT), theme::W / 2, 34, theme::TITLE_FONT, theme::DESTRUCTIVE);
        } else {
            theme::text_center(d, i18n::t(hacking::CLEAR), theme::W / 2, 34, theme::TITLE_FONT, theme::accent());
        }
        let rows = [
            ("deauth", det.deauth, det.deauth > 0),
            ("disassoc", det.disassoc, det.disassoc > 0),
            ("beacons", det.beacon, false),
            ("frames", det.frames, false),
        ];
        for (i, (label, val, hot)) in rows.iter().enumerate() {
            let y = 52 + i as i32 * 14;
            theme::text(d, label, theme::PAD + 10, y, theme::BODY_FONT, theme::MUTED);
            let col = if *hot { theme::DESTRUCTIVE } else { theme::FG };
            let v = alloc::format!("{}", val);
            theme::text_right(d, &v, theme::W - theme::PAD - 10, y, theme::BODY_FONT, col);
        }
        theme::hint(d, i18n::t(hacking::ENTER_RELISTEN_BACK));
    }

    fn draw_done<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
        }
        theme::topbar(d, self.pending.name());
        theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);
        if self.scan_failed {
            self.draw_failed(d);
            return;
        }
        if matches!(self.pending, Tool::Handshake | Tool::EvilTwin) {
            if self.wifi_pass_len > 0 {
                // cracked: show the recovered passphrase
                theme::text_center(d, i18n::t(hacking::WIFI_CRACKED), theme::W / 2, 36, theme::TITLE_FONT, RADAR_GREEN);
                let pass = core::str::from_utf8(&self.wifi_pass[..self.wifi_pass_len]).unwrap_or("?");
                theme::text_center(d, &alloc::format!("pass: {}", pass), theme::W / 2, 60, theme::TITLE_FONT, theme::FG);
                theme::text_center(d, &alloc::format!("{} EAPOL", self.attack_sent), theme::W / 2, 82, theme::BODY_FONT, theme::MUTED);
            } else if self.hs_captured {
                // full handshake but the password wasn't in the built-in list
                theme::text_center(d, i18n::t(hacking::HANDSHAKE_CAPTURED), theme::W / 2, 40, theme::TITLE_FONT, theme::accent());
                theme::text_center(d, &alloc::format!("{} EAPOL", self.attack_sent), theme::W / 2, 62, theme::BODY_FONT, theme::FG);
                theme::text_center(d, i18n::t(hacking::PASS_LOCKED), theme::W / 2, 80, theme::BODY_FONT, theme::MUTED);
            } else {
                theme::text_center(d, i18n::t(hacking::NO_HANDSHAKE), theme::W / 2, 40, theme::TITLE_FONT, theme::MUTED);
                theme::text_center(d, &alloc::format!("{} EAPOL", self.attack_sent), theme::W / 2, 62, theme::BODY_FONT, theme::FG);
            }
        } else if matches!(self.pending, Tool::EvilPortal) {
            theme::text_center(d, i18n::t(hacking::PORTAL_STOPPED), theme::W / 2, 40, theme::TITLE_FONT, theme::accent());
            let line = alloc::format!("{} {}", self.attack_sent, i18n::t(hacking::CREDENTIALS_CAPTURED));
            theme::text_center(d, &line, theme::W / 2, 64, theme::BODY_FONT, theme::FG);
        } else if matches!(self.pending, Tool::NetScan) {
            theme::text_center(d, i18n::t(hacking::SCAN_DONE), theme::W / 2, 40, theme::TITLE_FONT, theme::accent());
            let line = alloc::format!("{} {}", self.attack_sent, i18n::t(hacking::OPEN_PORTS));
            theme::text_center(d, &line, theme::W / 2, 64, theme::BODY_FONT, theme::FG);
        } else if matches!(self.pending, Tool::CamScan) {
            theme::text_center(d, i18n::t(hacking::SCAN_DONE), theme::W / 2, 40, theme::TITLE_FONT, theme::accent());
            let line = alloc::format!("{} {}", self.attack_sent, i18n::t(hacking::CAMERAS_FOUND));
            theme::text_center(d, &line, theme::W / 2, 64, theme::BODY_FONT, theme::FG);
        } else if matches!(self.pending, Tool::Deauth) {
            // raw deauth TX is rejected by this ESP IDF blob -> attack_sent stays 0; be honest.
            theme::text_center(d, i18n::t(hacking::DEAUTH_NA), theme::W / 2, 38, theme::TITLE_FONT, theme::DESTRUCTIVE);
            theme::text_center(d, i18n::t(hacking::USE_HANDSHAKE), theme::W / 2, 60, theme::BODY_FONT, theme::MUTED);
            let line = alloc::format!("{} {}", self.attack_sent, i18n::t(hacking::FRAMES));
            theme::text_center(d, &line, theme::W / 2, 80, theme::BODY_FONT, theme::FAINT);
        } else {
            theme::text_center(d, i18n::t(hacking::STOPPED), theme::W / 2, 40, theme::TITLE_FONT, theme::accent());
            let unit = if matches!(self.pending, Tool::BleSpam) {
                i18n::t(hacking::ADVERTS)
            } else {
                i18n::t(hacking::FRAMES)
            };
            let line = alloc::format!("{} {}", self.attack_sent, unit);
            theme::text_center(d, &line, theme::W / 2, 64, theme::BODY_FONT, theme::FG);
        }
        theme::hint(d, i18n::t(hacking::ENTER_RUN_AGAIN_BACK));
    }

    fn draw_failed<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        // the SPECIFIC reason if one was set, else the generic label
        let msg = self.fail_msg.unwrap_or_else(|| i18n::t(hacking::RADIO_ERROR));
        // BODY_FONT (not TITLE) so longer reason strings fit the 240px width
        theme::text_center(d, msg, theme::W / 2, theme::H / 2 - 6, theme::BODY_FONT, theme::DESTRUCTIVE);
        theme::text_center(d, i18n::t(hacking::ENTER_TO_RETRY), theme::W / 2, theme::H / 2 + 10, theme::BODY_FONT, theme::MUTED);
        theme::hint(d, i18n::t(hacking::ENTER_RETRY_BACK));
    }
}

const WIKI_VISIBLE: usize = 8;

/// Live attack progress screen (free fn — borrows neither Hacking nor the radio).
pub fn draw_running<D: DrawTarget<Color = Rgb565>>(d: &mut D, title: &str, unit: &str, count: u32) {
    theme::topbar(d, title);
    theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);
    theme::text_center(d, i18n::t(hacking::ATTACK_RUNNING), theme::W / 2, 38, theme::TITLE_FONT, theme::DESTRUCTIVE);
    let line = alloc::format!("{} {}", count, unit);
    theme::text_center(d, &line, theme::W / 2, 62, theme::TITLE_FONT, theme::accent());
    theme::hint(d, i18n::t(hacking::ANY_KEY_TO_STOP));
}

/// Live evil-portal screen, painted by main between polls.
pub fn draw_portal<D: DrawTarget<Color = Rgb565>>(d: &mut D, ssid: &str, st: &portal::Stats) {
    theme::topbar(d, Tool::EvilPortal.name());
    theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);
    let ssid_line = alloc::format!("AP: {}", ssid);
    theme::text_center(d, &ssid_line, theme::W / 2, 26, theme::BODY_FONT, theme::accent());
    let svc = alloc::format!("dhcp {}  dns {}  http {}", st.dhcp, st.dns, st.http);
    theme::text_center(d, &svc, theme::W / 2, 44, theme::BODY_FONT, theme::MUTED);
    let creds = alloc::format!("{}: {}", i18n::t(hacking::CAPTURED), st.creds);
    let col = if st.creds > 0 { theme::DESTRUCTIVE } else { theme::FG };
    theme::text_center(d, &creds, theme::W / 2, 62, theme::TITLE_FONT, col);
    if st.creds > 0 {
        let u = alloc::format!("user: {}", st.user_str());
        let p = alloc::format!("pass: {}", st.pass_str());
        theme::text(d, &u, theme::PAD, 82, theme::BODY_FONT, theme::FG);
        theme::text(d, &p, theme::PAD, 94, theme::BODY_FONT, theme::FG);
    }
    theme::hint(d, i18n::t(hacking::ANY_KEY_TO_STOP));
}

// ------------------------------- radar view --------------------------------

/// Green used for "open"/"owned" radar blips (matches the Basic-tier green).
const RADAR_GREEN: Rgb565 = Rgb565::new(7, 46, 12);
/// Radar geometry (left half of the screen; right half is the info panel).
const RADAR_CX: i32 = 60;
const RADAR_CY: i32 = 72;
const RADAR_R: i32 = 46;
const PANEL_X: i32 = 116;

/// Polar -> screen point. `deg` clockwise from +x.
fn polar(cx: i32, cy: i32, radius: f32, deg: f32) -> (i32, i32) {
    let a = deg * core::f32::consts::PI / 180.0;
    ((cx as f32 + radius * libm::cosf(a)) as i32, (cy as f32 + radius * libm::sinf(a)) as i32)
}

/// Draw the radar backdrop: range rings, crosshairs, centre, and the sweep line.
fn radar_frame<D: DrawTarget<Color = Rgb565>>(d: &mut D, sweep_deg: f32) {
    theme::ring(d, RADAR_CX, RADAR_CY, RADAR_R, theme::BORDER);
    theme::ring(d, RADAR_CX, RADAR_CY, RADAR_R * 2 / 3, theme::BORDER);
    theme::ring(d, RADAR_CX, RADAR_CY, RADAR_R / 3, theme::BORDER);
    theme::line(d, RADAR_CX - RADAR_R, RADAR_CY, RADAR_CX + RADAR_R, RADAR_CY, theme::BORDER);
    theme::line(d, RADAR_CX, RADAR_CY - RADAR_R, RADAR_CX, RADAR_CY + RADAR_R, theme::BORDER);
    let (sx, sy) = polar(RADAR_CX, RADAR_CY, RADAR_R as f32, sweep_deg);
    theme::line(d, RADAR_CX, RADAR_CY, sx, sy, theme::accent());
    theme::disc(d, RADAR_CX, RADAR_CY, 2, theme::accent());
}

/// Stable bearing for a 6-byte address (so each AP/host keeps a fixed angle).
fn addr_angle(bytes: &[u8]) -> f32 {
    let h = bytes.iter().fold(0u32, |a, &b| a.wrapping_mul(31).wrapping_add(b as u32));
    (h % 360) as f32
}

/// RSSI -> radial fraction (0=centre/strong .. 1=edge/weak). ~ -30..-90 dBm.
fn rssi_frac(rssi: i8) -> f32 {
    let f = (-(rssi as f32) - 30.0) / 60.0;
    f.clamp(0.12, 1.0)
}

/// Rough RSSI -> metres via log-distance path loss: d = 10^((txRef - rssi)/(10n)).
fn rssi_meters_ref(rssi: i8, tx_ref: f32, n: f32) -> u32 {
    let d = libm::powf(10.0, (tx_ref - rssi as f32) / (10.0 * n));
    d.clamp(1.0, 999.0) as u32
}
/// WiFi AP distance (TxRef -40 dBm @1m, n=2.5).
fn rssi_meters(rssi: i8) -> u32 {
    rssi_meters_ref(rssi, -40.0, 2.5)
}
/// BLE distance (TxRef -59 dBm @1m, n=2.0 — weaker tx than WiFi).
fn ble_meters(rssi: i8) -> u32 {
    rssi_meters_ref(rssi, -59.0, 2.0)
}

/// Live LAN-scan screen, painted by main between polls.
pub fn draw_netscan<D: DrawTarget<Color = Rgb565>>(d: &mut D, st: &netscan::NetResult) {
    theme::topbar(d, Tool::NetScan.name());
    theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);
    let phase = alloc::format!("{}: {}", i18n::t(hacking::PHASE), st.phase);
    theme::text_center(d, &phase, theme::W / 2, 26, theme::BODY_FONT, theme::accent());
    if st.got_ip {
        let ip = alloc::format!("ip {}.{}.{}.{}", st.ip[0], st.ip[1], st.ip[2], st.ip[3]);
        let gw = alloc::format!("gw {}.{}.{}.{}", st.gw[0], st.gw[1], st.gw[2], st.gw[3]);
        theme::text(d, &ip, theme::PAD, 42, theme::BODY_FONT, theme::FG);
        theme::text_right(d, &gw, theme::W - theme::PAD, 42, theme::BODY_FONT, theme::MUTED);
        let mut line: alloc::string::String = alloc::string::String::new();
        for (i, &p) in netscan::PORTS.iter().enumerate() {
            if st.open[i] {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(&alloc::format!("{}", p));
            }
        }
        let cnt = alloc::format!("{} ({}/{}):", i18n::t(hacking::OPEN), st.open_count(), st.scanned);
        theme::text(d, &cnt, theme::PAD, 60, theme::BODY_FONT, theme::MUTED);
        let col = if st.open_count() > 0 { theme::DESTRUCTIVE } else { theme::FG };
        theme::text(d, if line.is_empty() { "-" } else { &line }, theme::PAD, 74, theme::BODY_FONT, col);
        if st.banner_len > 0 {
            let b = alloc::format!("srv: {}", st.banner_str());
            theme::text(d, &b, theme::PAD, 90, theme::BODY_FONT, theme::accent());
        }
        if st.wifi_pass_len > 0 {
            theme::text(d, &alloc::format!("wifi: {}", st.wifi_pass_str()), theme::PAD, 102, theme::BODY_FONT, RADAR_GREEN);
        }
    } else {
        // during the WPA ladder show the attempt count, else the DHCP wait
        let msg = if st.phase == "wifi crack" {
            alloc::format!("wifi crack {}", st.scanned)
        } else {
            alloc::string::String::from(i18n::t(hacking::JOINING_DHCP))
        };
        theme::text_center(d, &msg, theme::W / 2, theme::H / 2, theme::BODY_FONT, theme::accent());
    }
    theme::hint(d, i18n::t(hacking::ANY_KEY_TO_STOP));
}

/// Live Camera-Finder screen — a radar: gateway at centre, discovered hosts as
/// blips (camera = red, cracked = green ringed), sweep spinning with progress.
pub fn draw_camscan<D: DrawTarget<Color = Rgb565>>(d: &mut D, st: &camscan::CamResult) {
    theme::topbar(d, Tool::CamScan.name());
    theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);

    if !st.got_ip {
        let msg = if st.total > 0 && st.phase == "wifi crack" {
            alloc::format!("wifi crack {}/{}", st.probed, st.total)
        } else {
            alloc::string::String::from(i18n::t(hacking::JOINING_DHCP))
        };
        theme::text_center(d, &msg, theme::W / 2, theme::H / 2, theme::BODY_FONT, theme::accent());
        theme::hint(d, i18n::t(hacking::ANY_KEY_TO_STOP));
        return;
    }

    // sweep angle advances with probe progress -> radar "spins" as it scans
    let sweep = (st.probed as f32 * 6.0) % 360.0;
    radar_frame(d, sweep);

    // each found host: angle from its last octet, ring by port (80 inner / 8080 outer)
    for h in st.hosts.iter() {
        let frac = if h.port == 80 { 0.45 } else { 0.75 };
        let (x, y) = polar(RADAR_CX, RADAR_CY, frac * RADAR_R as f32, addr_angle(&h.ip));
        if h.cred_len > 0 {
            theme::disc(d, x, y, 3, RADAR_GREEN);
            theme::ring(d, x, y, 5, RADAR_GREEN);
        } else if h.is_camera {
            theme::disc(d, x, y, 3, theme::DESTRUCTIVE);
        } else {
            theme::disc(d, x, y, 1, theme::MUTED);
        }
    }

    // info panel — headline shows the cracked WiFi pass if we broke in, else phase
    if st.wifi_pass_len > 0 {
        theme::text(d, &alloc::format!("w:{}", st.wifi_pass_str()), PANEL_X, 24, theme::BODY_FONT, RADAR_GREEN);
    } else {
        theme::text(d, &alloc::format!("[{}]", st.phase), PANEL_X, 24, theme::BODY_FONT, theme::accent());
    }
    theme::text(d, &alloc::format!("{}/{}", st.probed, st.total), PANEL_X, 38, theme::BODY_FONT, theme::FG);
    theme::text(d, &alloc::format!("hosts {}", st.live), PANEL_X, 50, theme::BODY_FONT, theme::MUTED);
    theme::text(d, &alloc::format!("cam {}", st.cam_count()), PANEL_X, 62, theme::BODY_FONT, theme::DESTRUCTIVE);
    theme::text(d, &alloc::format!("pwn {}", st.cracked_count()), PANEL_X, 74, theme::BODY_FONT, RADAR_GREEN);
    // a couple of cracked creds (most interesting finds)
    let mut y = 90;
    for h in st.hosts.iter().filter(|h| h.cred_len > 0).take(3) {
        theme::text(d, &alloc::format!(".{} {}", h.ip[3], h.cred_str()), PANEL_X, y, theme::BODY_FONT, RADAR_GREEN);
        y += 11;
    }
    theme::hint(d, i18n::t(hacking::ANY_KEY_TO_STOP));
}

// ---- list navigation helpers (skip the difficulty headers) ----
fn next_tool_row(from: usize) -> usize {
    let mut i = from;
    while i + 1 < LIST.len() {
        i += 1;
        if matches!(LIST[i], LRow::Tool(_)) {
            return i;
        }
    }
    from
}
fn prev_tool_row(from: usize) -> usize {
    let mut i = from;
    while i > 0 {
        i -= 1;
        if matches!(LIST[i], LRow::Tool(_)) {
            return i;
        }
    }
    from
}

#[inline]
fn clamp(sel: usize, scroll: usize, visible: usize) -> usize {
    if sel < scroll {
        sel
    } else if sel >= scroll + visible {
        sel + 1 - visible
    } else {
        scroll
    }
}

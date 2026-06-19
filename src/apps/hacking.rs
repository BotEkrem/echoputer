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

use crate::apps::wiki;
use crate::hal::keymap;
use crate::radio::{ble_spam, netscan, portal};
use crate::i18n;
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
            Diff::Basic => i18n::t("BASIC", "TEMEL"),
            Diff::Inter => i18n::t("INTERMEDIATE", "ORTA"),
            Diff::Adv => i18n::t("ADVANCED", "ILERI"),
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
            Tool::WifiScan => i18n::t("WiFi Scanner", "WiFi Tarayici"),
            Tool::WifiAnalyze => i18n::t("WiFi Analyzer", "WiFi Analiz"),
            Tool::BleScan => i18n::t("BLE Scanner", "BLE Tarayici"),
            Tool::Detector => i18n::t("Deauth Detector", "Deauth Dedektor"),
            Tool::Deauth => i18n::t("Deauth Flood", "Deauth Seli"),
            Tool::BeaconSpam => i18n::t("Beacon Spam", "Beacon Spam"),
            Tool::ProbeFlood => i18n::t("Probe Flood", "Probe Seli"),
            Tool::EvilTwin => i18n::t("Evil Twin", "Evil Twin"),
            Tool::Handshake => i18n::t("Handshake Capture", "Handshake Yakalama"),
            Tool::EvilPortal => i18n::t("Evil Portal", "Evil Portal"),
            Tool::NetScan => i18n::t("LAN Scan", "LAN Tarama"),
            Tool::BleSpam => i18n::t("BLE Spam", "BLE Spam"),
        }
    }
    fn difficulty(self) -> Diff {
        match self {
            Tool::WifiScan | Tool::WifiAnalyze | Tool::BleScan | Tool::Detector => Diff::Basic,
            Tool::BeaconSpam | Tool::ProbeFlood | Tool::BleSpam | Tool::EvilTwin => Diff::Inter,
            Tool::Deauth | Tool::Handshake | Tool::EvilPortal | Tool::NetScan => Diff::Adv,
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
            Tool::EvilTwin => i18n::t("Evil Twin: pick AP", "Evil Twin: AP sec"),
            Tool::Handshake => i18n::t("Handshake: pick AP", "Handshake: AP sec"),
            Tool::NetScan => i18n::t("LAN Scan: pick open AP", "LAN Scan: acik AP sec"),
            _ => i18n::t("Deauth: pick target", "Deauth: hedef sec"),
        }
    }
    fn target_verb(self) -> &'static str {
        match self {
            Tool::EvilTwin => i18n::t("clone", "klonla"),
            Tool::Handshake => i18n::t("capture", "yakala"),
            Tool::NetScan => i18n::t("scan", "tara"),
            _ => i18n::t("deauth", "deauth"),
        }
    }
}

// ---- the difficulty-grouped list (headers + tools, in display order) ----
enum LRow {
    Head(Diff),
    Tool(Tool),
}
const LIST: [LRow; 15] = [
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
            NameSrc::RandomEn => i18n::t("Random EN", "Rastgele EN"),
            NameSrc::RandomTr => i18n::t("Random TR", "Rastgele TR"),
            NameSrc::Custom => i18n::t("Custom", "Ozel"),
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
}
impl Ap {
    const EMPTY: Ap = Ap { ssid: [0; 32], ssid_len: 0, bssid: [0; 6], rssi: 0, channel: 0, auth: "" };
}

#[derive(Clone, Copy)]
struct Ble {
    addr: [u8; 6],
    rssi: i8,
    name: [u8; 20],
    name_len: u8,
}
impl Ble {
    const EMPTY: Ble = Ble { addr: [0; 6], rssi: 0, name: [0; 20], name_len: 0 };
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
        }
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
    pub fn target_ssid_owned(&self) -> Option<([u8; 32], usize, u8)> {
        if self.view == View::Targets && self.sel < self.ap_count {
            let ap = &self.aps[self.sel];
            Some((ap.ssid, ap.ssid_len as usize, ap.channel))
        } else {
            None
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
                self.view = View::ToolCfg;
                self.sel = 0;
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
            Tool::Deauth | Tool::EvilTwin | Tool::Handshake | Tool::NetScan => Action::ScanTargets,
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
                    Action::None
                } else {
                    match self.pending {
                        Tool::EvilTwin => Action::EvilTwin,
                        Tool::Handshake => Action::Handshake,
                        Tool::NetScan => Action::NetScan,
                        _ => Action::Deauth,
                    }
                }
            }
            _ => Action::None,
        }
    }

    /// ENTER re-runs the tool that produced the current result screen.
    fn key_rescan(&mut self, rc: (u8, u8), tool: Tool) -> Action {
        match rc {
            crate::K_ENTER => Action::Run(tool),
            crate::K_UP if matches!(self.view, View::WifiList | View::BleList) => {
                if self.scroll > 0 {
                    self.scroll -= 1;
                }
                Action::Redraw
            }
            crate::K_DOWN if matches!(self.view, View::WifiList | View::BleList) => {
                let count = if self.view == View::WifiList { self.ap_count } else { self.ble_count };
                if self.scroll + ROW_VISIBLE < count {
                    self.scroll += 1;
                }
                Action::Redraw
            }
            _ => Action::None,
        }
    }

    fn key_done(&mut self, rc: (u8, u8)) -> Action {
        match rc {
            crate::K_ENTER => match self.pending {
                Tool::Deauth | Tool::EvilTwin | Tool::Handshake | Tool::NetScan => Action::ScanTargets,
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
        self.ap_count += 1;
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
            }
            None => self.scan_failed = true,
        }
        self.view = View::Done;
        self.draw(d, true);
    }

    pub fn set_scan_failed(&mut self) {
        self.scan_failed = true;
    }

    pub fn set_running(&mut self) {
        self.view = View::Running;
    }

    /// True while a text field is open (so main lets Backspace edit text instead
    /// of treating it as "go back").
    pub fn is_editing(&self) -> bool {
        self.view == View::TextInput
    }

    // ----------------------------- drawing -----------------------------
    pub fn draw_busy<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, title: &str, msg: &str) {
        theme::clear(d);
        theme::topbar(d, title);
        theme::text_center(d, msg, theme::W / 2, theme::H / 2 - 6, theme::TITLE_FONT, theme::accent());
        theme::text_center(d, i18n::t("please wait", "lutfen bekleyin"), theme::W / 2, theme::H / 2 + 10, theme::BODY_FONT, theme::MUTED);
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
        theme::hint(d, i18n::t("ENTER open   G0 back", "ENTER ac   G0 geri"));
    }

    fn draw_detail<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
            theme::topbar(d, self.pending.name());
            // difficulty badge under the title
            let diff = self.pending.difficulty();
            theme::text(d, diff.label(), theme::PAD, 20, theme::BODY_FONT, diff.color());
            if self.pending.offensive() {
                theme::text_right(d, i18n::t("ATTACK", "SALDIRI"), theme::W - theme::PAD, 20, theme::BODY_FONT, theme::DESTRUCTIVE);
            }
        }
        let opts = self.detail_opts();
        let labels = [
            i18n::t("Use tool", "Araci kullan"),
            i18n::t("Wiki", "Wiki"),
            i18n::t("Settings", "Ayarlar"),
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
        theme::hint(d, i18n::t("ENTER select   G0 back", "ENTER sec   G0 geri"));
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
        theme::hint(d, i18n::t("up/down scroll   G0 back", "yukari/asagi kaydir   G0 geri"));
    }

    fn draw_cfg<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
            theme::topbar(d, self.pending.name());
            theme::text(d, i18n::t("Settings", "Ayarlar"), theme::PAD, 20, theme::BODY_FONT, theme::accent());
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
        theme::hint(d, i18n::t("left/right change   ENTER edit   G0 back", "sol/sag degistir   ENTER duzenle   G0 geri"));
    }

    /// (label, value) for a settings row. Value strings borrow self's buffers.
    fn cfg_row_text(&self, row: CfgRow) -> (&str, &str) {
        match row {
            CfgRow::NameSrc => (i18n::t("Name source", "Isim kaynagi"), self.cfg.name_src.label()),
            CfgRow::CustomName => (
                i18n::t("Custom name", "Ozel isim"),
                core::str::from_utf8(&self.cfg.prefix[..self.cfg.prefix_len]).unwrap_or("?"),
            ),
            CfgRow::BleMode => (i18n::t("Mode", "Mod"), self.ble_mode().label()),
            CfgRow::PortalName => (
                i18n::t("AP name", "AP adi"),
                core::str::from_utf8(&self.cfg.portal[..self.cfg.portal_len]).unwrap_or("?"),
            ),
        }
    }

    fn draw_text<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        theme::clear(d);
        let (title, buf): (&str, &str) = match self.edit {
            Edit::Prefix => (
                i18n::t("Custom SSID name", "Ozel SSID adi"),
                core::str::from_utf8(&self.cfg.prefix[..self.cfg.prefix_len]).unwrap_or(""),
            ),
            Edit::Portal => (
                i18n::t("Portal AP name", "Portal AP adi"),
                core::str::from_utf8(&self.cfg.portal[..self.cfg.portal_len]).unwrap_or(""),
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
            theme::text_center(d, i18n::t("becomes NAME001, NAME002 ...", "NAME001, NAME002 ... olur"), theme::W / 2, 78, theme::BODY_FONT, theme::MUTED);
        }
        theme::hint(d, i18n::t("type   bksp delete   ENTER ok   G0 cancel", "yaz   bksp sil   ENTER tamam   G0 iptal"));
    }

    fn draw_confirm<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
        }
        theme::topbar(d, self.pending.name());
        theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);
        theme::text_center(d, i18n::t("ACTIVE ATTACK", "AKTIF SALDIRI"), theme::W / 2, 44, theme::TITLE_FONT, theme::DESTRUCTIVE);
        theme::text_center(d, self.pending.name(), theme::W / 2, 66, theme::BODY_FONT, theme::MUTED);
        theme::hint(d, i18n::t("ENTER start   G0 cancel", "ENTER baslat   G0 iptal"));
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
            theme::text_center(d, i18n::t("no networks found", "ag bulunamadi"), theme::W / 2, theme::H / 2, theme::BODY_FONT, theme::MUTED);
        } else {
            for row in 0..ROW_VISIBLE {
                let idx = self.scroll + row;
                if idx >= self.ap_count {
                    break;
                }
                let ap = &self.aps[idx];
                let y = 22 + row as i32 * 16;
                let highlight = select && idx == self.sel;
                if highlight {
                    theme::fill(d, theme::PAD - 2, y - 2, (theme::W - 2 * theme::PAD + 4) as u32, 15, theme::SURFACE2);
                }
                let name: alloc::string::String = if ap.ssid_len == 0 {
                    alloc::string::String::from(i18n::t("<hidden>", "<gizli>"))
                } else {
                    core::str::from_utf8(&ap.ssid[..ap.ssid_len as usize]).unwrap_or("?").chars().take(15).collect()
                };
                let col = if highlight { theme::accent() } else { theme::FG };
                theme::text(d, &name, theme::PAD, y, theme::BODY_FONT, col);
                let info = alloc::format!("{:>4}  c{:<2} {}", ap.rssi, ap.channel, ap.auth);
                theme::text_right(d, &info, theme::W - theme::PAD, y, theme::BODY_FONT, theme::MUTED);
            }
        }
        if select {
            let h = alloc::format!("{} {}   G0", i18n::t("ENTER", "ENTER"), self.pending.target_verb());
            theme::hint(d, &h);
        } else {
            let hint = alloc::format!("{} {}   ENTER {}   G0", self.ap_count, i18n::t("nets", "ag"), i18n::t("rescan", "tekrar"));
            theme::hint(d, &hint);
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
        let hint = alloc::format!("{} {}   ENTER {}   G0", self.ap_count, i18n::t("nets", "ag"), i18n::t("rescan", "tekrar"));
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
            theme::text_center(d, i18n::t("no devices found", "cihaz bulunamadi"), theme::W / 2, theme::H / 2, theme::BODY_FONT, theme::MUTED);
        } else {
            for row in 0..ROW_VISIBLE {
                let idx = self.scroll + row;
                if idx >= self.ble_count {
                    break;
                }
                let dev = &self.bles[idx];
                let y = 22 + row as i32 * 16;
                let name: alloc::string::String = if dev.name_len == 0 {
                    let a = dev.addr;
                    alloc::format!("{:02X}:{:02X}:{:02X}:{:02X}", a[2], a[3], a[4], a[5])
                } else {
                    core::str::from_utf8(&dev.name[..dev.name_len as usize]).unwrap_or("?").chars().take(16).collect()
                };
                theme::text(d, &name, theme::PAD, y, theme::BODY_FONT, theme::FG);
                let info = alloc::format!("{:>4} dBm", dev.rssi);
                theme::text_right(d, &info, theme::W - theme::PAD, y, theme::BODY_FONT, theme::MUTED);
            }
        }
        let hint = alloc::format!("{} {}   ENTER {}   G0", self.ble_count, i18n::t("dev", "cihaz"), i18n::t("rescan", "tekrar"));
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
            theme::text_center(d, i18n::t("! DEAUTH ATTACK !", "! DEAUTH SALDIRISI !"), theme::W / 2, 34, theme::TITLE_FONT, theme::DESTRUCTIVE);
        } else {
            theme::text_center(d, i18n::t("clear", "temiz"), theme::W / 2, 34, theme::TITLE_FONT, theme::accent());
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
        theme::hint(d, i18n::t("ENTER re-listen   G0 back", "ENTER tekrar dinle   G0 geri"));
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
        if matches!(self.pending, Tool::Handshake) {
            let got = self.attack_sent >= 2;
            let (verdict, col) = if got {
                (i18n::t("handshake captured", "handshake yakalandi"), theme::accent())
            } else {
                (i18n::t("no handshake", "handshake yok"), theme::MUTED)
            };
            theme::text_center(d, verdict, theme::W / 2, 40, theme::TITLE_FONT, col);
            let line = alloc::format!("{} EAPOL", self.attack_sent);
            theme::text_center(d, &line, theme::W / 2, 64, theme::BODY_FONT, theme::FG);
        } else if matches!(self.pending, Tool::EvilPortal) {
            theme::text_center(d, i18n::t("portal stopped", "portal durdu"), theme::W / 2, 40, theme::TITLE_FONT, theme::accent());
            let line = alloc::format!("{} {}", self.attack_sent, i18n::t("credentials captured", "kimlik yakalandi"));
            theme::text_center(d, &line, theme::W / 2, 64, theme::BODY_FONT, theme::FG);
        } else if matches!(self.pending, Tool::NetScan) {
            theme::text_center(d, i18n::t("scan done", "tarama bitti"), theme::W / 2, 40, theme::TITLE_FONT, theme::accent());
            let line = alloc::format!("{} {}", self.attack_sent, i18n::t("open ports", "acik port"));
            theme::text_center(d, &line, theme::W / 2, 64, theme::BODY_FONT, theme::FG);
        } else {
            theme::text_center(d, i18n::t("stopped", "durdu"), theme::W / 2, 40, theme::TITLE_FONT, theme::accent());
            let unit = if matches!(self.pending, Tool::BleSpam) {
                i18n::t("adverts", "reklam")
            } else {
                i18n::t("frames", "cerceve")
            };
            let line = alloc::format!("{} {}", self.attack_sent, unit);
            theme::text_center(d, &line, theme::W / 2, 64, theme::BODY_FONT, theme::FG);
        }
        theme::hint(d, i18n::t("ENTER run again   G0 back", "ENTER tekrar   G0 geri"));
    }

    fn draw_failed<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::text_center(d, i18n::t("radio error", "radyo hatasi"), theme::W / 2, theme::H / 2 - 6, theme::TITLE_FONT, theme::DESTRUCTIVE);
        theme::text_center(d, i18n::t("ENTER to retry", "ENTER tekrar dene"), theme::W / 2, theme::H / 2 + 10, theme::BODY_FONT, theme::MUTED);
        theme::hint(d, i18n::t("ENTER retry   G0 back", "ENTER tekrar   G0 geri"));
    }
}

const WIKI_VISIBLE: usize = 8;

/// Live attack progress screen (free fn — borrows neither Hacking nor the radio).
pub fn draw_running<D: DrawTarget<Color = Rgb565>>(d: &mut D, title: &str, unit: &str, count: u32) {
    theme::topbar(d, title);
    theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);
    theme::text_center(d, i18n::t("ATTACK RUNNING", "SALDIRI CALISIYOR"), theme::W / 2, 38, theme::TITLE_FONT, theme::DESTRUCTIVE);
    let line = alloc::format!("{} {}", count, unit);
    theme::text_center(d, &line, theme::W / 2, 62, theme::TITLE_FONT, theme::accent());
    theme::hint(d, i18n::t("any key / G0 to stop", "durdurmak icin tus / G0"));
}

/// Live evil-portal screen, painted by main between polls.
pub fn draw_portal<D: DrawTarget<Color = Rgb565>>(d: &mut D, ssid: &str, st: &portal::Stats) {
    theme::topbar(d, Tool::EvilPortal.name());
    theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);
    let ssid_line = alloc::format!("AP: {}", ssid);
    theme::text_center(d, &ssid_line, theme::W / 2, 26, theme::BODY_FONT, theme::accent());
    let svc = alloc::format!("dhcp {}  dns {}  http {}", st.dhcp, st.dns, st.http);
    theme::text_center(d, &svc, theme::W / 2, 44, theme::BODY_FONT, theme::MUTED);
    let creds = alloc::format!("{}: {}", i18n::t("captured", "yakalanan"), st.creds);
    let col = if st.creds > 0 { theme::DESTRUCTIVE } else { theme::FG };
    theme::text_center(d, &creds, theme::W / 2, 62, theme::TITLE_FONT, col);
    if st.creds > 0 {
        let u = alloc::format!("user: {}", st.user_str());
        let p = alloc::format!("pass: {}", st.pass_str());
        theme::text(d, &u, theme::PAD, 82, theme::BODY_FONT, theme::FG);
        theme::text(d, &p, theme::PAD, 94, theme::BODY_FONT, theme::FG);
    }
    theme::hint(d, i18n::t("any key / G0 to stop", "durdurmak icin tus / G0"));
}

/// Live LAN-scan screen, painted by main between polls.
pub fn draw_netscan<D: DrawTarget<Color = Rgb565>>(d: &mut D, st: &netscan::NetResult) {
    theme::topbar(d, Tool::NetScan.name());
    theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);
    let phase = alloc::format!("{}: {}", i18n::t("phase", "asama"), st.phase);
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
        let cnt = alloc::format!("{} ({}/{}):", i18n::t("open", "acik"), st.open_count(), st.scanned);
        theme::text(d, &cnt, theme::PAD, 60, theme::BODY_FONT, theme::MUTED);
        let col = if st.open_count() > 0 { theme::DESTRUCTIVE } else { theme::FG };
        theme::text(d, if line.is_empty() { "-" } else { &line }, theme::PAD, 74, theme::BODY_FONT, col);
    } else {
        theme::text_center(d, i18n::t("joining + DHCP...", "baglaniyor + DHCP..."), theme::W / 2, theme::H / 2, theme::BODY_FONT, theme::MUTED);
    }
    theme::hint(d, i18n::t("any key / G0 to stop", "durdurmak icin tus / G0"));
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

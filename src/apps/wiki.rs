//! Bilingual per-tool help, shown in each tool's Wiki screen. Turkish is written
//! without diacritics (the display font is ASCII-only). Lines are pre-wrapped for
//! the 240px screen; the Wiki view scrolls them.

use crate::apps::hacking::Tool;
use crate::i18n;
use crate::i18n::wiki;

/// The wiki body for a tool, in the active language.
pub fn get(t: Tool) -> &'static str {
    match t {
        Tool::WifiScan => i18n::t(wiki::WIFI_SCAN),
        Tool::WifiAnalyze => i18n::t(wiki::WIFI_ANALYZE),
        Tool::BleScan => i18n::t(wiki::BLE_SCAN),
        Tool::Detector => i18n::t(wiki::DETECTOR),
        Tool::BeaconSpam => i18n::t(wiki::BEACON),
        Tool::ProbeFlood => i18n::t(wiki::PROBE),
        Tool::BleSpam => i18n::t(wiki::BLE_SPAM),
        Tool::EvilTwin => i18n::t(wiki::EVIL_TWIN),
        Tool::Deauth => i18n::t(wiki::DEAUTH),
        Tool::Handshake => i18n::t(wiki::HANDSHAKE),
        Tool::EvilPortal => i18n::t(wiki::PORTAL),
        Tool::NetScan => i18n::t(wiki::NETSCAN),
        Tool::CamScan => i18n::t(wiki::CAMSCAN),
        Tool::Wardrive => i18n::t(wiki::WARDRIVE),
        Tool::Pmkid => i18n::t(wiki::PMKID),
    }
}

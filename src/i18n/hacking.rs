//! hacking screen translations.
use super::Msg;

pub const BASIC: Msg = ["BASIC", "TEMEL"];
pub const INTERMEDIATE: Msg = ["INTERMEDIATE", "ORTA"];
pub const ADVANCED: Msg = ["ADVANCED", "ILERI"];

pub const WIFI_SCANNER: Msg = ["WiFi Scanner", "WiFi Tarayici"];
pub const WIFI_ANALYZER: Msg = ["WiFi Analyzer", "WiFi Analiz"];
pub const BLE_SCANNER: Msg = ["BLE Scanner", "BLE Tarayici"];
pub const DEAUTH_DETECTOR: Msg = ["Deauth Detector", "Deauth Dedektor"];
pub const DEAUTH_FLOOD: Msg = ["Deauth Flood", "Deauth Seli"];
pub const BEACON_SPAM: Msg = ["Beacon Spam", "Beacon Spam"];
pub const PROBE_FLOOD: Msg = ["Probe Flood", "Probe Seli"];
pub const EVIL_TWIN: Msg = ["Evil Twin", "Evil Twin"];
pub const HANDSHAKE_CAPTURE: Msg = ["Handshake Capture", "Handshake Yakalama"];
pub const EVIL_PORTAL: Msg = ["Evil Portal", "Evil Portal"];
pub const LAN_SCAN: Msg = ["LAN Scan", "LAN Tarama"];
pub const BLE_SPAM: Msg = ["BLE Spam", "BLE Spam"];

pub const EVIL_TWIN_PICK_AP: Msg = ["Evil Twin: pick AP", "Evil Twin: AP sec"];
pub const HANDSHAKE_PICK_AP: Msg = ["Handshake: pick AP", "Handshake: AP sec"];
pub const LAN_SCAN_PICK_AP: Msg = ["LAN Scan: pick open AP", "LAN Scan: acik AP sec"];
pub const DEAUTH_PICK_TARGET: Msg = ["Deauth: pick target", "Deauth: hedef sec"];

pub const CLONE: Msg = ["clone", "klonla"];
pub const CAPTURE_VERB: Msg = ["capture", "yakala"];
pub const SCAN_VERB: Msg = ["scan", "tara"];
pub const DEAUTH_VERB: Msg = ["deauth", "deauth"];

pub const RANDOM_EN: Msg = ["Random EN", "Rastgele EN"];
pub const RANDOM_TR: Msg = ["Random TR", "Rastgele TR"];
pub const CUSTOM: Msg = ["Custom", "Ozel"];

pub const PLEASE_WAIT: Msg = ["please wait", "lutfen bekleyin"];

pub const ENTER_OPEN_BACK: Msg = ["ENTER open   ESC back", "ENTER ac   ESC geri"];

pub const ATTACK: Msg = ["ATTACK", "SALDIRI"];
pub const USE_TOOL: Msg = ["Use tool", "Araci kullan"];
pub const WIKI: Msg = ["Wiki", "Wiki"];
pub const SETTINGS: Msg = ["Settings", "Ayarlar"];
pub const ENTER_SELECT_BACK: Msg = ["ENTER select   ESC back", "ENTER sec   ESC geri"];

pub const SCROLL_BACK: Msg = ["up/down scroll   ESC back", "yukari/asagi kaydir   ESC geri"];

pub const CFG_CHANGE_EDIT_BACK: Msg = ["left/right change   ENTER edit   ESC back", "sol/sag degistir   ENTER duzenle   ESC geri"];

pub const NAME_SOURCE: Msg = ["Name source", "Isim kaynagi"];
pub const CUSTOM_NAME: Msg = ["Custom name", "Ozel isim"];
pub const MODE: Msg = ["Mode", "Mod"];
pub const AP_NAME: Msg = ["AP name", "AP adi"];

pub const CUSTOM_SSID_NAME: Msg = ["Custom SSID name", "Ozel SSID adi"];
pub const PORTAL_AP_NAME: Msg = ["Portal AP name", "Portal AP adi"];
pub const BECOMES_NAME: Msg = ["becomes NAME001, NAME002 ...", "NAME001, NAME002 ... olur"];
pub const TYPE_BKSP_OK_CANCEL: Msg = ["type   bksp delete   ENTER ok   ESC cancel", "yaz   bksp sil   ENTER tamam   ESC iptal"];

pub const ACTIVE_ATTACK: Msg = ["ACTIVE ATTACK", "AKTIF SALDIRI"];
pub const ENTER_START_CANCEL: Msg = ["ENTER start   ESC cancel", "ENTER baslat   ESC iptal"];

pub const NO_NETWORKS_FOUND: Msg = ["no networks found", "ag bulunamadi"];
pub const HIDDEN: Msg = ["<hidden>", "<gizli>"];
pub const ENTER: Msg = ["ENTER", "ENTER"];
pub const NETS: Msg = ["nets", "ag"];
pub const RESCAN: Msg = ["rescan", "tekrar"];

pub const NO_DEVICES_FOUND: Msg = ["no devices found", "cihaz bulunamadi"];
pub const DEV: Msg = ["dev", "cihaz"];

pub const DEAUTH_ATTACK_ALERT: Msg = ["! DEAUTH ATTACK !", "! DEAUTH SALDIRISI !"];
pub const CLEAR: Msg = ["clear", "temiz"];
pub const ENTER_RELISTEN_BACK: Msg = ["ENTER re-listen   ESC back", "ENTER tekrar dinle   ESC geri"];

pub const HANDSHAKE_CAPTURED: Msg = ["handshake captured", "handshake yakalandi"];
pub const NO_HANDSHAKE: Msg = ["no handshake", "handshake yok"];
pub const PORTAL_STOPPED: Msg = ["portal stopped", "portal durdu"];
pub const CREDENTIALS_CAPTURED: Msg = ["credentials captured", "kimlik yakalandi"];
pub const SCAN_DONE: Msg = ["scan done", "tarama bitti"];
pub const OPEN_PORTS: Msg = ["open ports", "acik port"];
pub const STOPPED: Msg = ["stopped", "durdu"];
pub const ADVERTS: Msg = ["adverts", "reklam"];
pub const FRAMES: Msg = ["frames", "cerceve"];
pub const ENTER_RUN_AGAIN_BACK: Msg = ["ENTER run again   ESC back", "ENTER tekrar   ESC geri"];

pub const RADIO_ERROR: Msg = ["radio error", "radyo hatasi"];
pub const ENTER_TO_RETRY: Msg = ["ENTER to retry", "ENTER tekrar dene"];
pub const ENTER_RETRY_BACK: Msg = ["ENTER retry   ESC back", "ENTER tekrar   ESC geri"];

pub const ATTACK_RUNNING: Msg = ["ATTACK RUNNING", "SALDIRI CALISIYOR"];
pub const ANY_KEY_TO_STOP: Msg = ["any key to stop", "durdurmak icin tus"];

pub const CAPTURED: Msg = ["captured", "yakalanan"];
pub const PHASE: Msg = ["phase", "asama"];
pub const OPEN: Msg = ["open", "acik"];
pub const JOINING_DHCP: Msg = ["joining + DHCP...", "baglaniyor + DHCP..."];

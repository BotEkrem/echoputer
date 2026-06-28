//! Wardriving log: repeatedly scan WiFi (+ BLE) and append each newly-seen device to
//! an SD CSV (`WARDRIVE.CSV`). No GPS on the Cardputer, so this is a signal/inventory
//! log (first-seen RSSI + channel + encryption), not a geo track.

use alloc::string::String;
use embedded_sdmmc::{BlockDevice, Mode, TimeSource, VolumeIdx, VolumeManager};

use super::{ScannedAp, ScannedBle};

const WARDRIVE_FILE: &str = "WARDRIVE.CSV"; // SD root, 8.3 name

/// Running tally shown by the UI while the log fills.
#[derive(Clone, Copy)]
pub struct WardriveState {
    pub got_file: bool, // the CSV opened OK on SD
    pub aps: u32,       // unique APs logged
    pub ble: u32,       // unique BLE devices logged
    pub rounds: u32,    // scan rounds completed
}
impl WardriveState {
    pub fn new() -> Self {
        Self { got_file: false, aps: 0, ble: 0, rounds: 0 }
    }
}

/// Quote a field for CSV: wrap in `"`, double internal quotes, drop control bytes
/// (so an SSID with a comma/quote/newline can't corrupt the row).
fn csv_field(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\"\""),
            c if (c as u32) < 0x20 => {} // drop CR/LF/control
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn hex6(m: &[u8; 6]) -> String {
    let mut s = String::with_capacity(17);
    const H: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in m.iter().enumerate() {
        if i > 0 {
            s.push(':');
        }
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 0x0f) as usize] as char);
    }
    s
}

pub fn csv_header() -> &'static str {
    "type,bssid,name,rssi,channel,enc\n"
}

/// One CSV row for a scanned AP: `AP,<bssid>,"<ssid>",<rssi>,<ch>,<enc>`.
pub fn ap_row(ap: &ScannedAp) -> String {
    alloc::format!("AP,{},{},{},{},{}\n", hex6(&ap.bssid), csv_field(&ap.ssid), ap.rssi, ap.channel, ap.auth)
}

/// One CSV row for a scanned BLE device: `BLE,<addr>,"<name>",<rssi>,,`.
pub fn ble_row(b: &ScannedBle) -> String {
    let name = b.name.as_deref().unwrap_or("");
    alloc::format!("BLE,{},{},{},,\n", hex6(&b.addr), csv_field(name), b.rssi)
}

/// Truncate/create the CSV and write the header. Returns false if SD isn't writable.
pub fn init<D: BlockDevice, T: TimeSource>(vm: &VolumeManager<D, T>) -> bool {
    (|| -> Option<()> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let dir = vol.open_root_dir().ok()?;
        let f = dir.open_file_in_dir(WARDRIVE_FILE, Mode::ReadWriteCreateOrTruncate).ok()?;
        f.write(csv_header().as_bytes()).ok()?;
        f.flush().ok()?;
        Some(())
    })()
    .is_some()
}

/// Append `rows` (already-formatted CSV text) to the log.
pub fn append<D: BlockDevice, T: TimeSource>(vm: &VolumeManager<D, T>, rows: &str) -> bool {
    if rows.is_empty() {
        return true;
    }
    (|| -> Option<()> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let dir = vol.open_root_dir().ok()?;
        let f = dir.open_file_in_dir(WARDRIVE_FILE, Mode::ReadWriteCreateOrAppend).ok()?;
        f.write(rows.as_bytes()).ok()?;
        f.flush().ok()?;
        Some(())
    })()
    .is_some()
}

/// Verify CSV field escaping + row formatting (run by `networktest`, no SD).
#[cfg(feature = "networktest")]
pub fn networktest() {
    use esp_println::println;
    println!("[*] wardrive CSV rows (no SD)...");
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut chk = |name: &str, cond: bool| {
        if cond {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL {name}");
        }
    };
    // an SSID with a comma, a quote, and a newline must not break the row
    let ap = ScannedAp {
        ssid: String::from("a,b\"c\nd"),
        bssid: [0x00, 0x11, 0x22, 0xaa, 0xbb, 0xcc],
        rssi: -42,
        channel: 6,
        auth: "wpa2",
    };
    let row = ap_row(&ap);
    chk("ap row tail", row.ends_with(",-42,6,wpa2\n"));
    chk("ap bssid hex", row.contains("00:11:22:aa:bb:cc"));
    chk("ssid quoted+escaped", row.contains("\"a,b\"\"cd\"")); // quote doubled, newline dropped
    chk("one line", row.matches('\n').count() == 1);
    chk("header cols", csv_header() == "type,bssid,name,rssi,channel,enc\n");
    let b = ScannedBle { addr: [0xde, 0xad, 0xbe, 0xef, 0x00, 0x01], rssi: -70, name: Some(String::from("Mi Band")) };
    let brow = ble_row(&b);
    chk("ble row", brow.starts_with("BLE,de:ad:be:ef:00:01,\"Mi Band\",-70,,\n"));
    let bn = ScannedBle { addr: [0; 6], rssi: -80, name: None };
    chk("ble no name", ble_row(&bn).contains(",\"\",-80,,"));
    println!("    wardrive csv: {pass} pass, {fail} fail");
}

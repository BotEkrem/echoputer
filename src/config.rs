//! User configuration shared across the firmware. The `Config` value (defaults +
//! load/save) lives here — central and independent of the Settings UI that edits
//! it ([`crate::apps::settings`]). Persisted to /ECHO/DATA/CONFIG.BIN (best-effort;
//! works fine with no SD card).

use alloc::string::String;
use embedded_sdmmc::{BlockDevice, Mode as FileMode, TimeSource, VolumeIdx, VolumeManager};

use crate::apps::scales::Mode;
use crate::i18n;

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
            // Create the dirs only when they don't already exist. save() fires on
            // every settings change; the old unconditional make_dir_in_dir scanned
            // both dirs (returning AlreadyExists) on each save — wasted I/O at
            // 400 kHz. change_dir-first skips that on the steady-state path.
            if dir.change_dir(DIR_APP).is_err() {
                dir.make_dir_in_dir(DIR_APP).ok()?;
                dir.change_dir(DIR_APP).ok()?;
            }
            if dir.change_dir(DIR_DATA).is_err() {
                dir.make_dir_in_dir(DIR_DATA).ok()?;
                dir.change_dir(DIR_DATA).ok()?;
            }
            let file = dir.open_file_in_dir(FILE_CFG, FileMode::ReadWriteCreateOrTruncate).ok()?;
            file.write(&buf).ok()?;
            file.flush().ok()?;
            Some(())
        })();
    }
}

// --------------------------- offload server config ---------------------------

const OFFLOAD_FILE: &str = "OFFLOAD.CFG"; // SD root, 8.3 name

/// WPA crack-offload "server provider": where the device POSTs a captured `.22000` to
/// be cracked by a PC (see offload/crack-server). Edited on-device in Settings AND in
/// the Web UI; persisted to `/OFFLOAD.CFG` (a tiny `key=value` text file).
pub struct OffloadCfg {
    pub host: [u8; 40],
    pub host_len: usize,
    pub port: u16,
    pub psk: [u8; 64],
    pub psk_len: usize,
    pub uplink_ssid: [u8; 32],
    pub uplink_ssid_len: usize,
    pub uplink_pass: [u8; 64],
    pub uplink_pass_len: usize,
}

#[allow(dead_code)] // load/save consumed by the Settings UI + Web UI + offload flow (wired next)
impl OffloadCfg {
    pub const fn new() -> Self {
        Self {
            host: [0; 40],
            host_len: 0,
            port: 8080,
            psk: [0; 64],
            psk_len: 0,
            uplink_ssid: [0; 32],
            uplink_ssid_len: 0,
            uplink_pass: [0; 64],
            uplink_pass_len: 0,
        }
    }
    pub fn host_str(&self) -> &str {
        core::str::from_utf8(&self.host[..self.host_len]).unwrap_or("")
    }
    pub fn psk_str(&self) -> &str {
        core::str::from_utf8(&self.psk[..self.psk_len]).unwrap_or("")
    }
    pub fn uplink_ssid_str(&self) -> &str {
        core::str::from_utf8(&self.uplink_ssid[..self.uplink_ssid_len]).unwrap_or("")
    }
    pub fn uplink_pass_str(&self) -> &str {
        core::str::from_utf8(&self.uplink_pass[..self.uplink_pass_len]).unwrap_or("")
    }
    /// Ready to offload? Need at least a host + an uplink network to reach it.
    pub fn configured(&self) -> bool {
        self.host_len > 0 && self.uplink_ssid_len > 0
    }

    fn set(dst: &mut [u8], len: &mut usize, v: &str) {
        let b = v.as_bytes();
        let n = b.len().min(dst.len());
        dst[..n].copy_from_slice(&b[..n]);
        *len = n;
    }
    pub fn set_host(&mut self, v: &str) {
        Self::set(&mut self.host, &mut self.host_len, v);
    }
    pub fn set_psk(&mut self, v: &str) {
        Self::set(&mut self.psk, &mut self.psk_len, v);
    }
    pub fn set_uplink_ssid(&mut self, v: &str) {
        Self::set(&mut self.uplink_ssid, &mut self.uplink_ssid_len, v);
    }
    pub fn set_uplink_pass(&mut self, v: &str) {
        Self::set(&mut self.uplink_pass, &mut self.uplink_pass_len, v);
    }
    pub fn set_port(&mut self, v: &str) {
        if let Ok(p) = v.trim().parse::<u16>() {
            self.port = p;
        }
    }

    /// Parse a `key=value` text buffer (host/port/psk/uplink/upass); unknown keys ignored.
    pub fn parse(buf: &[u8]) -> Self {
        let mut c = Self::new();
        for line in buf.split(|&b| b == b'\n' || b == b'\r') {
            let s = core::str::from_utf8(line).unwrap_or("").trim();
            if s.is_empty() || s.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = s.split_once('=') {
                let v = v.trim();
                match k.trim() {
                    "host" => c.set_host(v),
                    "port" => c.set_port(v),
                    "psk" => c.set_psk(v),
                    "uplink" => c.set_uplink_ssid(v),
                    "upass" => c.set_uplink_pass(v),
                    _ => {}
                }
            }
        }
        c
    }

    /// Serialize to the `key=value` text form written to SD / shown in the Web UI.
    pub fn serialize(&self) -> String {
        let mut s = String::new();
        s.push_str("host=");
        s.push_str(self.host_str());
        s.push_str(&alloc::format!("\nport={}\npsk=", self.port));
        s.push_str(self.psk_str());
        s.push_str("\nuplink=");
        s.push_str(self.uplink_ssid_str());
        s.push_str("\nupass=");
        s.push_str(self.uplink_pass_str());
        s.push('\n');
        s
    }

    pub fn load<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        let mut buf = [0u8; 256];
        let mut n = 0usize;
        let ok = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let dir = vol.open_root_dir().ok()?;
            let f = dir.open_file_in_dir(OFFLOAD_FILE, FileMode::ReadOnly).ok()?;
            while n < buf.len() {
                match f.read(&mut buf[n..]).ok()? {
                    0 => break,
                    k => n += k,
                }
            }
            Some(())
        })();
        if ok.is_some() && n > 0 {
            *self = Self::parse(&buf[..n]);
        }
    }

    pub fn save<D: BlockDevice, T: TimeSource>(&self, vm: &VolumeManager<D, T>) {
        let data = self.serialize();
        let _ = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let dir = vol.open_root_dir().ok()?;
            let f = dir.open_file_in_dir(OFFLOAD_FILE, FileMode::ReadWriteCreateOrTruncate).ok()?;
            f.write(data.as_bytes()).ok()?;
            f.flush().ok()?;
            Some(())
        })();
    }
}

/// Self-test the offload-config parser + round-trip (run by `networktest`, no SD).
#[cfg(feature = "networktest")]
pub fn networktest() {
    use esp_println::println;
    println!("[*] offload config parse/round-trip (no SD)...");
    let src = b"# offload\nhost=192.168.1.50\nport=9000\npsk=deadbeef\nuplink=LabNet\nupass=hunter2\n";
    let c = OffloadCfg::parse(src);
    let ok = c.host_str() == "192.168.1.50"
        && c.port == 9000
        && c.psk_str() == "deadbeef"
        && c.uplink_ssid_str() == "LabNet"
        && c.uplink_pass_str() == "hunter2"
        && c.configured();
    let c2 = OffloadCfg::parse(c.serialize().as_bytes());
    let rt = c2.host_str() == "192.168.1.50" && c2.port == 9000 && c2.psk_str() == "deadbeef" && c2.uplink_ssid_str() == "LabNet";
    let pass = (ok as u32) + (rt as u32);
    if pass != 2 {
        println!("    FAIL offload cfg: parse={ok} roundtrip={rt}");
    }
    println!("    offload cfg: {pass} pass, {} fail", 2 - pass);
}

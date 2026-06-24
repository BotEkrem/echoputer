//! User configuration shared across the firmware. The `Config` value (defaults +
//! load/save) lives here — central and independent of the Settings UI that edits
//! it ([`crate::apps::settings`]). Persisted to /ECHO/DATA/CONFIG.BIN (best-effort;
//! works fine with no SD card).

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

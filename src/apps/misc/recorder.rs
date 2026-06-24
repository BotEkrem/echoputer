//! Mic recorder — captures the onboard MEMS mic (ES8311 ADC -> I2S0 RX) to a WAV file
//! on the SD card. Stereo 16 kHz / 16-bit (the raw I2S frame; the mono mic sits in the
//! left slot). Files land at /ECHO/REC0.WAV .. REC9.WAV (the index cycles per session).
//!
//! NOT built on the emugbc (colour) build: the RX DMA buffer would shrink the tight
//! CPU0 stack enough to push the Web UI back over its overflow limit. Test on emu,player.
//!
//! Data flow: main owns the I2S RX circular transfer (always reading). While we're
//! recording, main pops fresh PCM and calls [`Recorder::feed`], which streams it to the
//! open file and tracks a peak level. The WAV header sizes are patched on stop.

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_sdmmc::{BlockDevice, Mode, RawDirectory, RawFile, RawVolume, TimeSource, VolumeIdx, VolumeManager};
use esp_hal::time::Instant;

use crate::{i18n, theme};
use crate::i18n::recorder;

const DIR_APP: &str = "ECHO";
const SAMPLE_RATE: u32 = 16000;
const CHANNELS: u16 = 2;
const BITS: u16 = 16;

pub struct Recorder {
    recording: bool,
    vol: Option<RawVolume>,
    dir: Option<RawDirectory>,
    file: Option<RawFile>,
    data_bytes: u32,
    start: Instant,
    counter: u32, // file-name index, cycles 0..9 per session
    peak: i16,    // running peak sample magnitude, for the level meter
    msg: Option<&'static str>,
}

impl Recorder {
    pub fn new() -> Self {
        Recorder {
            recording: false,
            vol: None,
            dir: None,
            file: None,
            data_bytes: 0,
            start: Instant::now(),
            counter: 0,
            peak: 0,
            msg: None,
        }
    }

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        self.recording = false;
        self.msg = None;
        self.peak = 0;
        self.draw(d);
    }

    /// Stop a recording in progress and release the SD handles (called when leaving).
    pub fn finalize<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>) {
        if self.recording {
            self.stop(sd);
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recording
    }

    pub fn on_key<D: BlockDevice, T: TimeSource>(&mut self, rc: (u8, u8), sd: &VolumeManager<D, T>, d: &mut impl DrawTarget<Color = Rgb565>) {
        if rc == crate::K_ENTER {
            if self.recording {
                self.stop(sd);
            } else {
                self.startrec(sd);
            }
            self.draw(d);
        }
    }

    pub fn tick(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        if self.recording {
            self.draw(d); // refresh the timer + level meter ~every loop
            self.peak = (self.peak as i32 * 7 / 8) as i16; // decay the meter
            true
        } else {
            false
        }
    }

    // ---- recording lifecycle ----

    fn startrec<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>) {
        self.msg = None;
        let name = FILE_NAMES[(self.counter % 10) as usize];
        match self.open(sd, name) {
            Ok(()) => {
                self.recording = true;
                self.data_bytes = 0;
                self.peak = 0;
                self.start = Instant::now();
            }
            Err(e) => {
                self.msg = Some(e);
                self.close(sd);
            }
        }
    }

    fn open<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>, name: &str) -> Result<(), &'static str> {
        let vol = sd.open_raw_volume(VolumeIdx(0)).map_err(|_| "no card")?;
        self.vol = Some(vol);
        let root = sd.open_root_dir(vol).map_err(|_| "fs error")?;
        let app = sd.open_dir(root, DIR_APP).map_err(|_| "no /ECHO")?;
        let _ = sd.close_dir(root);
        self.dir = Some(app);
        let file = sd
            .open_file_in_dir(app, name, Mode::ReadWriteCreateOrTruncate)
            .map_err(|_| "open failed")?;
        self.file = Some(file);
        // Write the WAV header with placeholder sizes; patched in stop().
        let hdr = wav_header(0);
        sd.write(file, &hdr).map_err(|_| "write failed")?;
        Ok(())
    }

    fn stop<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>) {
        if let Some(file) = self.file {
            // Patch the two size fields now that we know the byte count.
            let riff = 36 + self.data_bytes;
            if sd.file_seek_from_start(file, 4).is_ok() {
                let _ = sd.write(file, &riff.to_le_bytes());
            }
            if sd.file_seek_from_start(file, 40).is_ok() {
                let _ = sd.write(file, &self.data_bytes.to_le_bytes());
            }
        }
        self.close(sd);
        self.recording = false;
        self.counter = self.counter.wrapping_add(1);
    }

    fn close<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>) {
        if let Some(f) = self.file.take() {
            let _ = sd.close_file(f);
        }
        if let Some(dr) = self.dir.take() {
            let _ = sd.close_dir(dr);
        }
        if let Some(v) = self.vol.take() {
            let _ = sd.close_volume(v);
        }
    }

    /// Stream a chunk of freshly-captured PCM (raw I2S RX bytes) to the file and update
    /// the level meter. Called by main while recording.
    pub fn feed<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>, bytes: &[u8]) {
        if !self.recording {
            return;
        }
        if let Some(file) = self.file {
            if sd.write(file, bytes).is_ok() {
                self.data_bytes = self.data_bytes.saturating_add(bytes.len() as u32);
            }
        }
        // Peak over the 16-bit samples in this chunk (for the meter).
        let mut i = 0;
        while i + 1 < bytes.len() {
            let s = i16::from_le_bytes([bytes[i], bytes[i + 1]]);
            let mag = s.unsigned_abs() as i16;
            if mag > self.peak {
                self.peak = mag;
            }
            i += 2;
        }
    }

    // ---- drawing ----

    fn draw(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::clear(d);
        theme::topbar(d, i18n::t(recorder::TITLE));
        if let Some(e) = self.msg {
            theme::text(d, e, theme::PAD, 44, theme::TITLE_FONT, theme::DESTRUCTIVE);
            theme::hint(d, i18n::t(recorder::ENTER_RECORD_BACK));
            return;
        }
        if self.recording {
            let secs = self.start.elapsed().as_secs() as u32;
            let mut tb = [0u8; 12];
            let ts = fmt_mmss(secs, &mut tb);
            theme::text_center(d, ts, theme::W / 2, 40, theme::TITLE_FONT, theme::DESTRUCTIVE);
            theme::text_center(d, i18n::t(recorder::REC), theme::W / 2, 22, theme::BODY_FONT, theme::DESTRUCTIVE);
            // level meter: a bar proportional to the recent peak (0..32767)
            let w = (self.peak as i32 * (theme::W as i32 - 2 * theme::PAD) / 32767).clamp(0, theme::W as i32 - 2 * theme::PAD);
            theme::fill(d, theme::PAD, 60, (theme::W - 2 * theme::PAD) as u32, 10, theme::SURFACE2);
            theme::fill(d, theme::PAD, 60, w as u32, 10, theme::accent());
            // size so far (KB)
            let mut kb = [0u8; 12];
            let ks = fmt_kb(self.data_bytes, &mut kb);
            theme::text_center(d, ks, theme::W / 2, 80, theme::BODY_FONT, theme::MUTED);
            theme::hint(d, i18n::t(recorder::ENTER_STOP_SAVE));
        } else {
            theme::text_center(d, i18n::t(recorder::READY), theme::W / 2, 44, theme::TITLE_FONT, theme::MUTED);
            theme::text_center(d, i18n::t(recorder::SAVES_TO), theme::W / 2, 66, theme::BODY_FONT, theme::FAINT);
            theme::hint(d, i18n::t(recorder::ENTER_RECORD_BACK));
        }
    }
}

/// 8.3 file names for the per-session recording index.
const FILE_NAMES: [&str; 10] = [
    "REC0.WAV", "REC1.WAV", "REC2.WAV", "REC3.WAV", "REC4.WAV", "REC5.WAV", "REC6.WAV", "REC7.WAV", "REC8.WAV", "REC9.WAV",
];

/// A 44-byte canonical PCM WAV header for 16 kHz / 16-bit / stereo with `data` bytes of
/// audio (0 as a placeholder at start; patched on stop).
fn wav_header(data: u32) -> [u8; 44] {
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * (BITS as u32 / 8);
    let block_align = CHANNELS * (BITS / 8);
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&CHANNELS.to_le_bytes());
    h[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&block_align.to_le_bytes());
    h[34..36].copy_from_slice(&BITS.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

/// Seconds -> "M:SS".
fn fmt_mmss(secs: u32, buf: &mut [u8; 12]) -> &str {
    let m = secs / 60;
    let s = secs % 60;
    let mut i = 0;
    push_u32(buf, &mut i, m);
    push(buf, &mut i, b':');
    push(buf, &mut i, b'0' + (s / 10) as u8);
    push(buf, &mut i, b'0' + (s % 10) as u8);
    core::str::from_utf8(&buf[..i]).unwrap_or("0:00")
}

/// Bytes -> "NN KB".
fn fmt_kb(bytes: u32, buf: &mut [u8; 12]) -> &str {
    let mut i = 0;
    push_u32(buf, &mut i, bytes / 1024);
    for &b in b" KB" {
        push(buf, &mut i, b);
    }
    core::str::from_utf8(&buf[..i]).unwrap_or("0 KB")
}

fn push(buf: &mut [u8], i: &mut usize, b: u8) {
    if *i < buf.len() {
        buf[*i] = b;
        *i += 1;
    }
}

fn push_u32(buf: &mut [u8], i: &mut usize, v: u32) {
    let mut tmp = [0u8; 10];
    let mut n = v;
    let mut c = 0;
    if n == 0 {
        push(buf, i, b'0');
        return;
    }
    while n > 0 && c < tmp.len() {
        tmp[c] = b'0' + (n % 10) as u8;
        n /= 10;
        c += 1;
    }
    while c > 0 {
        c -= 1;
        push(buf, i, tmp[c]);
    }
}

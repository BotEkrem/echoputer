//! Audio Player app.
//!
//! Plays the `.wav` (and, with `--features player`, `.mp3`) files in `/ECHO/MUSIC/`
//! on the SD card. WAV is decoded in pure Rust; MP3 uses the vendored minimp3 core
//! (see `mp3`). Every source is resampled to the firmware's native 16 kHz (see
//! `resample`) and pushed through the same I2S audio path the synth and the Game
//! Boy emulator use — no per-file I2S re-clocking, so the shared audio pipeline is
//! untouched and the (tiny) onboard speaker hears no difference.
//!
//! Streaming model (mirrors the emulator): the SD volume/dir/file handles are held
//! open for the playing session; `pump` runs every main-loop iteration to read +
//! decode + resample the next slice into a ring, and `audio_fill` drains that ring
//! into the I2S DMA buffer (silence on underrun / when paused). Decode buffers are
//! heap-allocated only while playing (the radio is idle here).

mod resample;
mod wav;
#[cfg(feature = "player")]
mod mp3;

use crate::hal::fb::FrameBuf;
use crate::{i18n, theme};
use alloc::boxed::Box;
use embedded_sdmmc::{
    BlockDevice, DirEntry, LfnBuffer, Mode, RawDirectory, RawFile, RawVolume, ShortFileName,
    TimeSource, VolumeIdx, VolumeManager,
};
use resample::Resampler;
use theme::{BODY_FONT, FG, MUTED, PAD, W};

const DIR_APP: &str = "ECHO";
const DIR_MUSIC: &str = "MUSIC";
const MAX_TRACKS: usize = 48;
const SHORT_CAP: usize = 13;
const DISP_CAP: usize = 40;
const TOP: i32 = 24;
const ROW_H: i32 = 13;
const VISIBLE: usize = 7;
const SEEK_STEP: i32 = 10; // seconds per Left/Right
const VOL_STEP: u8 = 10;

// keys (logical row, col) — arrows/enter match main.rs; [ and ] are prev/next track.
const K_PREV: (u8, u8) = (1, 11); // '['
const K_NEXT: (u8, u8) = (1, 12); // ']'

// ---- 16 kHz output ring ----
// Decode (producer, `pump`) and the I2S feed (consumer, `audio_fill`) never run
// concurrently in the single-threaded main loop, so plain wrapping indices suffice.
// ~256 ms at 16 kHz; the I2S DMA buffer (0.5 s) is the real slack. Sized so one whole
// upsampled MP3 frame (a sub-16 kHz source -> up to 2304 output frames) fits in one
// go. The buffer + the MP3 decode scratch are heap-allocated only while a track plays
// (like the emulator's caches), so they stay OUT of permanent .bss — a permanent
// reservation that big starves the boot stack and panics at boot.
const ORING_CAP: usize = 8192; // i16 (4096 stereo frames)

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    None,
    Wav,
    Mp3,
}

#[derive(Clone, Copy)]
struct Entry {
    short: [u8; SHORT_CAP], // 8.3 name — used to open the file
    short_len: u8,
    disp: [u8; DISP_CAP], // long name — shown to the user
    disp_len: u8,
    is_mp3: bool,
}

impl Entry {
    const EMPTY: Entry = Entry {
        short: [0; SHORT_CAP],
        short_len: 0,
        disp: [0; DISP_CAP],
        disp_len: 0,
        is_mp3: false,
    };
    fn short_str(&self) -> &str {
        core::str::from_utf8(&self.short[..self.short_len as usize]).unwrap_or("")
    }
    fn disp_str(&self) -> &str {
        core::str::from_utf8(&self.disp[..self.disp_len as usize]).unwrap_or("?")
    }
}

enum State {
    List,
    Playing,
    Error(&'static str),
}

pub struct Player {
    state: State,
    sel: usize,
    scroll: usize,
    count: usize,
    tracks: [Entry; MAX_TRACKS],
    // session handles, held open while a track is loaded
    vol: Option<RawVolume>,
    dir: Option<RawDirectory>,
    file: Option<RawFile>,
    file_len: u32,
    track_idx: usize,
    kind: Kind,
    playing: bool,
    volume: u8,
    // 16 kHz output ring (heap-allocated while playing)
    oring: Option<Box<[i16]>>,
    ohead: usize,
    otail: usize,
    // MP3 decode output scratch (heap-allocated while playing)
    #[cfg(feature = "player")]
    pcm: Option<Box<[i16]>>,
    resampler: Resampler,
    cur_rate: u32,
    // WAV stream
    wav: wav::WavFmt,
    read_pos: u32,
    // MP3 stream
    #[cfg(feature = "player")]
    mp3: mp3::Mp3,
    // UI
    shown_sec: u32,
}

impl Player {
    pub fn new() -> Self {
        Player {
            state: State::List,
            sel: 0,
            scroll: 0,
            count: 0,
            tracks: [Entry::EMPTY; MAX_TRACKS],
            vol: None,
            dir: None,
            file: None,
            file_len: 0,
            track_idx: 0,
            kind: Kind::None,
            playing: false,
            volume: 80,
            oring: None,
            ohead: 0,
            otail: 0,
            #[cfg(feature = "player")]
            pcm: None,
            resampler: Resampler::new(),
            cur_rate: 0,
            wav: wav::WavFmt::EMPTY,
            read_pos: 0,
            #[cfg(feature = "player")]
            mp3: mp3::Mp3::new(),
            shown_sec: 0,
        }
    }

    /// True while a track is loaded (so the main loop routes G0 to play/pause and
    /// the audio fill to the Player).
    pub fn in_playing(&self) -> bool {
        matches!(self.state, State::Playing)
    }

    // ---- entry / exit -----------------------------------------------------

    pub fn enter<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>, d: &mut FrameBuf) {
        self.state = State::List;
        self.scan(vm);
        self.draw_list(d);
    }

    /// Backspace / (in the list) the back key: in a track -> stop and return to the
    /// list; in the list -> false (the caller pops to the home menu).
    pub fn back<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>, d: &mut FrameBuf) -> bool {
        match self.state {
            State::Playing | State::Error(_) => {
                self.stop_session(vm);
                self.state = State::List;
                self.draw_list(d);
                true
            }
            State::List => false,
        }
    }

    /// Close all SD handles + free decode buffers. MUST run before leaving the app
    /// (the VolumeManager allows only one open volume; a leak breaks SD for every
    /// other app).
    pub fn stop_session<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        self.playing = false;
        if let Some(f) = self.file.take() {
            let _ = vm.close_file(f);
        }
        if let Some(dir) = self.dir.take() {
            let _ = vm.close_dir(dir);
        }
        if let Some(v) = self.vol.take() {
            let _ = vm.close_volume(v);
        }
        #[cfg(feature = "player")]
        self.mp3.free();
        // Return the heap audio buffers (radio/other apps reclaim the heap).
        self.oring = None;
        #[cfg(feature = "player")]
        {
            self.pcm = None;
        }
        self.ring_reset();
        self.kind = Kind::None;
    }

    // ---- input ------------------------------------------------------------

    pub fn on_key<D: BlockDevice, T: TimeSource>(
        &mut self,
        rc: (u8, u8),
        vm: &VolumeManager<D, T>,
        d: &mut FrameBuf,
    ) {
        match self.state {
            State::List => match rc {
                crate::K_UP => {
                    if self.sel > 0 {
                        self.sel -= 1;
                        self.reclamp();
                        self.draw_list(d);
                    }
                }
                crate::K_DOWN => {
                    if self.sel + 1 < self.count {
                        self.sel += 1;
                        self.reclamp();
                        self.draw_list(d);
                    }
                }
                crate::K_ENTER => {
                    if self.count > 0 {
                        self.launch(self.sel, vm, d);
                    }
                }
                _ => {}
            },
            State::Playing => match rc {
                crate::K_ENTER => self.toggle_pause(d),
                crate::K_LEFT => self.seek(vm, -SEEK_STEP, d),
                crate::K_RIGHT => self.seek(vm, SEEK_STEP, d),
                crate::K_UP => {
                    let v = (self.volume + VOL_STEP).min(100);
                    self.set_vol(v, d);
                }
                crate::K_DOWN => {
                    let v = self.volume.saturating_sub(VOL_STEP);
                    self.set_vol(v, d);
                }
                K_PREV => {
                    if self.track_idx > 0 {
                        self.launch(self.track_idx - 1, vm, d);
                    }
                }
                K_NEXT => {
                    if self.track_idx + 1 < self.count {
                        self.launch(self.track_idx + 1, vm, d);
                    }
                }
                _ => {}
            },
            State::Error(_) => {}
        }
    }

    /// G0 while a track is loaded: toggle play/pause.
    pub fn toggle_pause(&mut self, d: &mut FrameBuf) {
        if matches!(self.state, State::Playing) {
            self.playing = !self.playing;
            self.shown_sec = u32::MAX; // force a redraw of the state line
            self.draw_playing(d);
        }
    }

    fn set_vol(&mut self, v: u8, d: &mut FrameBuf) {
        self.volume = v;
        self.shown_sec = u32::MAX;
        self.draw_playing(d);
    }

    fn seek<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>, delta: i32, d: &mut FrameBuf) {
        let (pos, total) = self.times();
        let mut ns = pos as i32 + delta;
        if ns < 0 {
            ns = 0;
        }
        let ns = (ns as u32).min(total);
        match self.kind {
            Kind::Wav => {
                self.read_pos = self.wav.data_off_for_sec(ns);
                if let Some(f) = self.file {
                    let _ = vm.file_seek_from_start(f, self.wav.data_start + self.read_pos);
                }
            }
            #[cfg(feature = "player")]
            Kind::Mp3 => {
                let byte = (ns as u64 * self.mp3.bitrate_kbps as u64 * 125).min(self.file_len as u64) as u32;
                if let Some(f) = self.file {
                    self.mp3.seek_to(vm, f, byte);
                }
            }
            #[cfg(not(feature = "player"))]
            Kind::Mp3 => {}
            Kind::None => {}
        }
        self.resampler.reset();
        self.ring_reset();
        self.shown_sec = u32::MAX;
        self.draw_playing(d);
    }

    // ---- ring helpers (heap-backed; valid while a track is loaded) --------

    fn ring_reset(&mut self) {
        self.ohead = 0;
        self.otail = 0;
    }

    /// Stereo frames the ring can still accept.
    fn ring_free_frames(&self) -> usize {
        let used = (self.ohead + ORING_CAP - self.otail) % ORING_CAP;
        (ORING_CAP - 1 - used) / 2
    }

    // ---- per-iteration audio --------------------------------------------------

    /// Decode/read + resample the next slice into the ring (call every main-loop
    /// iteration while the Player screen is active). Auto-advances to the next track
    /// at end of file.
    pub fn pump<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        if !self.playing || !matches!(self.state, State::Playing) {
            return;
        }
        for _ in 0..6 {
            // Decode only with room for a whole unit ahead: one MP3 frame can yield
            // up to 2304 output frames (a sub-16 kHz source upsampled); WAV input is
            // capped to the free space inside pump_wav, so a smaller gate suffices.
            let need = if self.kind == Kind::Mp3 { 2400 } else { 1024 };
            if self.ring_free_frames() < need {
                break;
            }
            let ok = match self.kind {
                Kind::Wav => self.pump_wav(vm),
                Kind::Mp3 => self.pump_mp3(vm),
                Kind::None => false,
            };
            if !ok {
                // track ended (or read error) -> advance, else stop at the end
                if !self.advance(vm) {
                    self.playing = false;
                }
                break;
            }
        }
    }

    fn pump_wav<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) -> bool {
        let Some(file) = self.file else { return false };
        let remaining = self.wav.data_len.saturating_sub(self.read_pos);
        if remaining == 0 {
            return false;
        }
        // Seek explicitly each chunk so a short read can't desync the stream.
        let abs = self.wav.data_start + self.read_pos;
        if vm.file_seek_from_start(file, abs).is_err() {
            return false;
        }
        let mut buf = [0u8; 2048];
        let ba = self.wav.block_align as usize;
        // Cap the read so the resampled output can't overflow the ring: a source at
        // `sample_rate` yields ~input*16000/sample_rate output frames, so to stay
        // within `free` output frames read at most free*sample_rate/16000 input frames.
        let free = self.ring_free_frames() as u64;
        let max_in = (free * self.wav.sample_rate as u64 / crate::apps::synth::SAMPLE_RATE as u64).max(1);
        let cap = (max_in as usize).saturating_mul(ba).min(buf.len());
        let want = (cap as u32).min(remaining) as usize;
        if want == 0 {
            return true; // nothing to read this pass (ring nearly full)
        }
        let mut got = 0;
        while got < want {
            match vm.read(file, &mut buf[got..want]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(_) => break,
            }
        }
        if got == 0 {
            return false;
        }
        let fmt = self.wav;
        // Split-borrow the resampler + ring fields so the resampler's `feed` can push
        // straight into the ring from inside the convert closure.
        let Some(oring) = self.oring.as_mut() else { return false };
        let rs = &mut self.resampler;
        let ohead = &mut self.ohead;
        let otail = self.otail; // tail only moves in audio_fill (not during pump)
        let mut push = |a: i16, b: i16| -> bool {
            let h = *ohead;
            let n1 = (h + 1) % ORING_CAP;
            let n2 = (h + 2) % ORING_CAP;
            if n1 == otail || n2 == otail {
                return false;
            }
            oring[h] = a;
            oring[n1] = b;
            *ohead = n2;
            true
        };
        let consumed = wav::convert(&fmt, &buf[..got], |l, r| {
            rs.feed(l, r, &mut push);
        });
        if consumed == 0 {
            return false;
        }
        self.read_pos += consumed as u32;
        true
    }

    fn pump_mp3<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) -> bool {
        #[cfg(feature = "player")]
        {
            let Some(file) = self.file else { return false };
            // Decode one frame into the heap PCM scratch.
            let res = {
                let Some(pcm) = self.pcm.as_mut() else { return false };
                self.mp3.decode(vm, file, pcm)
            };
            let (samples, ch) = match res {
                Some(x) => x,
                None => return false,
            };
            if self.cur_rate != self.mp3.hz && self.mp3.hz != 0 {
                self.cur_rate = self.mp3.hz;
                self.resampler.set_rate(self.mp3.hz);
            }
            // Split-borrow: read the PCM scratch, push resampled frames into the ring.
            let Some(pcm) = self.pcm.as_ref() else { return true };
            let Some(oring) = self.oring.as_mut() else { return true };
            let rs = &mut self.resampler;
            let ohead = &mut self.ohead;
            let otail = self.otail;
            let mut push = |a: i16, b: i16| -> bool {
                let h = *ohead;
                let n1 = (h + 1) % ORING_CAP;
                let n2 = (h + 2) % ORING_CAP;
                if n1 == otail || n2 == otail {
                    return false;
                }
                oring[h] = a;
                oring[n1] = b;
                *ohead = n2;
                true
            };
            if ch >= 2 {
                for i in 0..samples {
                    rs.feed(pcm[2 * i], pcm[2 * i + 1], &mut push);
                }
            } else {
                for i in 0..samples {
                    rs.feed(pcm[i], pcm[i], &mut push);
                }
            }
            true
        }
        #[cfg(not(feature = "player"))]
        {
            let _ = vm;
            false
        }
    }

    /// Drain the ring into the I2S sample buffer, scaled by volume; silence when
    /// paused or on underrun. The main loop calls this instead of the synth fill.
    pub fn audio_fill(&mut self, out: &mut [i16]) {
        let vol = self.volume as i32;
        let head = self.ohead;
        let mut tail = self.otail;
        let drained = if !self.playing {
            false
        } else if let Some(oring) = self.oring.as_ref() {
            let mut i = 0;
            while i + 1 < out.len() {
                if tail == head {
                    out[i] = 0;
                    out[i + 1] = 0;
                } else {
                    out[i] = (oring[tail] as i32 * vol / 100) as i16;
                    let t1 = (tail + 1) % ORING_CAP;
                    out[i + 1] = (oring[t1] as i32 * vol / 100) as i16;
                    tail = (tail + 2) % ORING_CAP;
                }
                i += 2;
            }
            true
        } else {
            false
        };
        if drained {
            self.otail = tail;
        } else {
            for o in out.iter_mut() {
                *o = 0;
            }
        }
    }

    // ---- per-frame UI -----------------------------------------------------

    /// 40 ms tick: repaint the progress/time when the second changes. Returns true
    /// if it drew (so the main loop blits).
    pub fn tick(&mut self, d: &mut FrameBuf) -> bool {
        if !matches!(self.state, State::Playing) {
            return false;
        }
        let (pos, _) = self.times();
        if pos != self.shown_sec {
            self.shown_sec = pos;
            self.draw_playing(d);
            return true;
        }
        false
    }

    // ---- track lifecycle --------------------------------------------------

    fn launch<D: BlockDevice, T: TimeSource>(&mut self, idx: usize, vm: &VolumeManager<D, T>, d: &mut FrameBuf) {
        let r = self.open_session(vm).and_then(|_| self.open_track(idx, vm));
        match r {
            Ok(()) => {
                self.ring_reset();
                self.playing = true;
                self.state = State::Playing;
                self.shown_sec = u32::MAX;
                self.draw_playing(d);
            }
            Err(e) => {
                self.stop_session(vm);
                self.state = State::Error(e);
                self.draw_error(d, e);
            }
        }
    }

    /// Advance to the next track in the list (used at end of file). Returns false at
    /// the end of the list.
    fn advance<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) -> bool {
        if self.track_idx + 1 >= self.count {
            return false;
        }
        let next = self.track_idx + 1;
        if self.open_track(next, vm).is_ok() {
            self.ring_reset();
            self.playing = true;
            self.shown_sec = u32::MAX;
            true
        } else {
            false
        }
    }

    fn open_session<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) -> Result<(), &'static str> {
        if self.vol.is_some() {
            return Ok(()); // session already open (track change)
        }
        let vol = vm.open_raw_volume(VolumeIdx(0)).map_err(|_| "no card")?;
        self.vol = Some(vol);
        let root = vm.open_root_dir(vol).map_err(|_| "fs error")?;
        let app = vm.open_dir(root, DIR_APP).map_err(|_| "no /ECHO")?;
        let _ = vm.close_dir(root);
        let dir = vm.open_dir(app, DIR_MUSIC).map_err(|_| "no /MUSIC")?;
        let _ = vm.close_dir(app);
        self.dir = Some(dir);
        // Allocate the audio buffers from the heap (the radio is idle during
        // playback, so the heap is free — kept off permanent .bss to spare the boot
        // stack). Freed in stop_session.
        self.oring = Some(alloc::vec![0i16; ORING_CAP].into_boxed_slice());
        self.ohead = 0;
        self.otail = 0;
        #[cfg(feature = "player")]
        {
            self.pcm = Some(alloc::vec![0i16; mp3::MAX_SAMPLES].into_boxed_slice());
            if !self.mp3.alloc() {
                return Err("low memory");
            }
        }
        Ok(())
    }

    fn open_track<D: BlockDevice, T: TimeSource>(&mut self, idx: usize, vm: &VolumeManager<D, T>) -> Result<(), &'static str> {
        if idx >= self.count {
            return Err("bad track");
        }
        if let Some(f) = self.file.take() {
            let _ = vm.close_file(f);
        }
        let dir = self.dir.ok_or("no dir")?;
        let entry = self.tracks[idx];
        let file = vm.open_file_in_dir(dir, entry.short_str(), Mode::ReadOnly).map_err(|_| "open failed")?;
        self.file = Some(file);
        self.track_idx = idx;
        self.file_len = vm.file_length(file).unwrap_or(0);
        self.cur_rate = 0;
        if entry.is_mp3 {
            #[cfg(feature = "player")]
            {
                self.mp3.start(vm, file);
                self.kind = Kind::Mp3;
            }
            #[cfg(not(feature = "player"))]
            {
                return Err("need player build");
            }
        } else {
            let fmt = wav::parse(vm, file, self.file_len)?;
            self.wav = fmt;
            self.read_pos = 0;
            self.cur_rate = fmt.sample_rate;
            self.resampler.set_rate(fmt.sample_rate);
            let _ = vm.file_seek_from_start(file, fmt.data_start);
            self.kind = Kind::Wav;
        }
        self.resampler.reset();
        Ok(())
    }

    fn times(&self) -> (u32, u32) {
        match self.kind {
            Kind::Wav => (self.wav.pos_secs(self.read_pos), self.wav.total_secs()),
            #[cfg(feature = "player")]
            Kind::Mp3 => (self.mp3.pos_secs(), self.mp3.total_secs(self.file_len)),
            #[cfg(not(feature = "player"))]
            Kind::Mp3 => (0, 0),
            Kind::None => (0, 0),
        }
    }

    // ---- library ----------------------------------------------------------

    fn scan<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        self.count = 0;
        self.sel = 0;
        self.scroll = 0;
        // Scan via the wrapper API (gives long file names); the streaming session
        // re-opens by the 8.3 short name through the raw API.
        let _ = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let mut dir = vol.open_root_dir().ok()?;
            dir.change_dir(DIR_APP).ok()?;
            dir.change_dir(DIR_MUSIC).ok()?;
            let mut lfn_store = [0u8; 300];
            let mut lfn_buf = LfnBuffer::new(&mut lfn_store);
            dir.iterate_dir_lfn(&mut lfn_buf, |e: &DirEntry, lfn: Option<&str>| {
                if self.count >= MAX_TRACKS || e.attributes.is_directory() {
                    return;
                }
                if let Some(entry) = track_entry(&e.name, lfn) {
                    self.tracks[self.count] = entry;
                    self.count += 1;
                }
            })
            .ok()?;
            Some(())
        })();
    }

    // ---- rendering --------------------------------------------------------

    fn reclamp(&mut self) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + VISIBLE {
            self.scroll = self.sel + 1 - VISIBLE;
        }
    }

    fn draw_list(&self, d: &mut FrameBuf) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Player", "Oynatici"));
        if self.count == 0 {
            theme::text(d, i18n::t("No audio in /ECHO/MUSIC/", "/ECHO/MUSIC/ bos"), PAD, TOP + 6, BODY_FONT, MUTED);
            theme::text(d, i18n::t("Copy .wav / .mp3 files there", ".wav / .mp3 dosyalari koy"), PAD, TOP + 22, BODY_FONT, theme::FAINT);
        } else {
            let end = (self.scroll + VISIBLE).min(self.count);
            for (row, i) in (self.scroll..end).enumerate() {
                let y = TOP + row as i32 * ROW_H;
                let selected = i == self.sel;
                let col = if selected { theme::accent() } else { MUTED };
                if selected {
                    theme::text(d, ">", PAD, y, BODY_FONT, theme::accent());
                }
                theme::text(d, self.tracks[i].disp_str(), PAD + 12, y, BODY_FONT, col);
            }
        }
        theme::hint(d, i18n::t("UP/DN pick  ENTER play  ` menu", "YUK/AS sec  ENTER cal  ` menu"));
    }

    fn draw_playing(&self, d: &mut FrameBuf) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Player", "Oynatici"));
        let entry = &self.tracks[self.track_idx];
        // track name (clips at the screen edge if very long)
        theme::text(d, entry.disp_str(), PAD, TOP + 2, BODY_FONT, FG);
        let tag = if entry.is_mp3 { "MP3" } else { "WAV" };
        theme::text(d, tag, W - PAD - 18, TOP + 2, BODY_FONT, theme::accent());

        // state line
        let st = if self.playing {
            i18n::t("PLAYING", "CALIYOR")
        } else {
            i18n::t("PAUSED", "DURAKLADI")
        };
        theme::text(d, st, PAD, TOP + 20, BODY_FONT, if self.playing { theme::accent() } else { MUTED });

        // progress meter + time
        let (pos, total) = self.times();
        let frac = if total > 0 { pos as f32 / total as f32 } else { 0.0 };
        theme::meter(d, PAD, TOP + 40, W - 2 * PAD, 6, frac, theme::accent());
        let mut tb = [0u8; 24];
        let n = fmt_pos_total(&mut tb, pos, total);
        theme::text(d, core::str::from_utf8(&tb[..n]).unwrap_or(""), PAD, TOP + 52, BODY_FONT, MUTED);

        // volume
        let mut vb = [0u8; 12];
        let vn = fmt_vol(&mut vb, self.volume);
        theme::text_right(d, core::str::from_utf8(&vb[..vn]).unwrap_or(""), W - PAD, TOP + 52, BODY_FONT, MUTED);

        theme::hint(d, i18n::t("ENT play/pause <>seek ^v vol []trk", "ENT cal/dur <>sar ^v ses []parca"));
    }

    fn draw_error(&self, d: &mut FrameBuf, msg: &str) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Player", "Oynatici"));
        theme::text(d, msg, PAD, TOP + 10, theme::TITLE_FONT, FG);
        theme::hint(d, i18n::t("` menu", "` menu"));
    }
}

/// Build a track entry from a directory entry, keeping `.wav` (always) and `.mp3`
/// (only with the `player` feature). `is_mp3` is set only for accepted MP3s.
fn track_entry(sfn: &ShortFileName, lfn: Option<&str>) -> Option<Entry> {
    let ext = sfn.extension();
    let is_wav = ext.eq_ignore_ascii_case(b"WAV");
    let is_mp3 = ext.eq_ignore_ascii_case(b"MP3");
    #[cfg(feature = "player")]
    let accept_mp3 = is_mp3;
    #[cfg(not(feature = "player"))]
    let accept_mp3 = false;
    if !(is_wav || accept_mp3) {
        return None;
    }
    let base = sfn.base_name();
    let mut short = [0u8; SHORT_CAP];
    let mut i = 0;
    for &b in base.iter().take(8) {
        short[i] = b;
        i += 1;
    }
    short[i] = b'.';
    i += 1;
    for &b in ext.iter().take(3) {
        short[i] = b;
        i += 1;
    }
    let short_len = i as u8;

    let mut e = Entry::EMPTY;
    e.short = short;
    e.short_len = short_len;
    e.is_mp3 = accept_mp3;
    match lfn {
        Some(l) => e.disp_len = copy_into(l, &mut e.disp) as u8,
        None => {
            let n = short_len as usize;
            e.disp[..n].copy_from_slice(&short[..n]);
            e.disp_len = short_len;
        }
    }
    Some(e)
}

fn copy_into(s: &str, buf: &mut [u8; DISP_CAP]) -> usize {
    let mut n = 0;
    for &c in s.as_bytes() {
        if n < DISP_CAP {
            buf[n] = c;
            n += 1;
        }
    }
    n
}

fn push_u32(buf: &mut [u8], mut at: usize, v: u32) -> usize {
    let mut tmp = [0u8; 10];
    let mut n = v;
    let mut i = 0;
    if n == 0 {
        tmp[0] = b'0';
        i = 1;
    } else {
        while n > 0 {
            tmp[i] = b'0' + (n % 10) as u8;
            n /= 10;
            i += 1;
        }
    }
    while i > 0 && at < buf.len() {
        i -= 1;
        buf[at] = tmp[i];
        at += 1;
    }
    at
}

fn push_mmss(buf: &mut [u8], mut at: usize, sec: u32) -> usize {
    at = push_u32(buf, at, sec / 60);
    if at < buf.len() {
        buf[at] = b':';
        at += 1;
    }
    let s = sec % 60;
    if at < buf.len() {
        buf[at] = b'0' + (s / 10) as u8;
        at += 1;
    }
    if at < buf.len() {
        buf[at] = b'0' + (s % 10) as u8;
        at += 1;
    }
    at
}

fn fmt_pos_total(buf: &mut [u8], pos: u32, total: u32) -> usize {
    let mut at = push_mmss(buf, 0, pos);
    for &c in b" / " {
        if at < buf.len() {
            buf[at] = c;
            at += 1;
        }
    }
    push_mmss(buf, at, total)
}

fn fmt_vol(buf: &mut [u8], v: u8) -> usize {
    let mut at = 0;
    for &c in b"VOL " {
        buf[at] = c;
        at += 1;
    }
    at = push_u32(buf, at, v as u32);
    if at < buf.len() {
        buf[at] = b'%';
        at += 1;
    }
    at
}

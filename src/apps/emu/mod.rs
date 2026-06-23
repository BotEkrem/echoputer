//! Game Boy emulator app (Peanut-GB core).
//!
//! The "library" is simply whatever `.gb` / `.gbc` files sit in `/ECHO/ROMS/` on
//! the SD card — drop a ROM there and it shows up. Picking one opens it in place
//! (no copy): the ROM is read on demand through an SD-backed bank cache, cart RAM
//! is kept in SRAM and mirrored to a `.sav` next to the ROM.
//!
//! Storage rationale: the ADV has no PSRAM and a 1 MB ROM does not fit in SRAM, so
//! the ROM stays on the card (see `rom`). The C core is compiled by `build.rs` and
//! bridged in `core`.

mod ffi;
mod input;
mod rom;
#[cfg(feature = "emutest")]
mod test_rom;
mod video;

use crate::hal::fb::FrameBuf;
use crate::{i18n, theme};
use esp_hal::time::{Duration, Instant};
use embedded_sdmmc::{
    BlockDevice, DirEntry, Mode, RawDirectory, RawFile, RawVolume, ShortFileName, TimeSource,
    VolumeIdx, VolumeManager,
};
use input::Pad;

/// Drain queued Game Boy audio into the I2S sample buffer. The main loop calls
/// this (instead of the synth) while a game is playing.
pub fn audio_fill(out: &mut [i16]) {
    ffi::audio_fill(out);
}

const DIR_APP: &str = "ECHO";
const DIR_ROMS: &str = "ROMS";
const MAX_ROMS: usize = 24;
const TOP: i32 = 30;
const ROW_H: i32 = 14;
const VISIBLE: usize = 6;

#[derive(Clone, Copy)]
struct Entry {
    /// 8.3 name as a display/openable string, e.g. "POKERED.GB".
    name: [u8; 13],
    len: u8,
}

impl Entry {
    const EMPTY: Entry = Entry {
        name: [0; 13],
        len: 0,
    };
    fn as_str(&self) -> &str {
        // ASCII 8.3, always valid UTF-8.
        core::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("")
    }
}

enum State {
    List,
    Playing,
    Error,
}

pub struct Emu {
    state: State,
    sel: usize,
    scroll: usize,
    count: usize,
    roms: [Entry; MAX_ROMS],
    pad: Pad,
    // Session handles, held open while a game is running.
    vol: Option<RawVolume>,
    dir: Option<RawDirectory>,
    file: Option<RawFile>,
    save_name: [u8; 13],
    save_len: u8,
    save_size: usize,
    last_save: Instant,    // for the periodic .sav flush while playing
    vol_shown_at: Instant, // when the volume overlay was last triggered
}

impl Emu {
    pub fn new() -> Self {
        Emu {
            state: State::List,
            sel: 0,
            scroll: 0,
            count: 0,
            roms: [Entry::EMPTY; MAX_ROMS],
            pad: Pad::new(),
            vol: None,
            dir: None,
            file: None,
            save_name: [0; 13],
            save_len: 0,
            save_size: 0,
            last_save: Instant::now(),
            vol_shown_at: Instant::now(),
        }
    }

    /// True while a game is actually running (so the main loop knows G0 should
    /// cycle the volume rather than exit).
    pub fn is_playing(&self) -> bool {
        matches!(self.state, State::Playing)
    }

    /// G0 while playing: step the output volume and flash a small overlay.
    pub fn bump_volume(&mut self, d: &mut FrameBuf) {
        ffi::cycle_volume();
        self.vol_shown_at = Instant::now();
        self.draw_volume_overlay(d);
    }

    fn draw_volume_overlay(&self, d: &mut FrameBuf) {
        let s = alloc::format!("VOL {}%", ffi::volume());
        theme::fill(d, 78, 2, 84, 18, theme::BG);
        theme::fill(d, 78, 2, 84, 2, theme::accent());
        theme::text_center(d, &s, theme::W / 2, 7, theme::BODY_FONT, theme::FG);
    }

    // ---- entry / exit -----------------------------------------------------

    pub fn enter<D: BlockDevice, T: TimeSource>(
        &mut self,
        vm: &VolumeManager<D, T>,
        d: &mut FrameBuf,
    ) {
        self.state = State::List;
        self.scan(vm);
        self.draw_list(d);
    }

    /// G0 / Backspace: in a game -> save and return to the list; in the list ->
    /// false (pop to the home menu).
    pub fn back<D: BlockDevice, T: TimeSource>(
        &mut self,
        vm: &VolumeManager<D, T>,
        d: &mut FrameBuf,
    ) -> bool {
        match self.state {
            State::Playing | State::Error => {
                self.stop(vm);
                self.state = State::List;
                self.draw_list(d);
                true
            }
            State::List => false,
        }
    }

    // ---- input ------------------------------------------------------------

    /// The emulator needs press *and* release (held buttons), unlike the one-shot
    /// apps, so the main loop routes raw key events here.
    pub fn on_event<D: BlockDevice, T: TimeSource>(
        &mut self,
        rc: (u8, u8),
        pressed: bool,
        vm: &VolumeManager<D, T>,
        d: &mut FrameBuf,
    ) {
        match self.state {
            State::List => {
                if !pressed {
                    return;
                }
                match rc {
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
                }
            }
            State::Playing => {
                self.pad.set(rc, pressed);
            }
            State::Error => {}
        }
    }

    // ---- per-frame --------------------------------------------------------

    /// Runs one Game Boy frame while playing; returns true if it drew (so the main
    /// loop blits the framebuffer).
    pub fn tick<D: BlockDevice, T: TimeSource>(
        &mut self,
        vm: &VolumeManager<D, T>,
        d: &mut FrameBuf,
    ) -> bool {
        if !matches!(self.state, State::Playing) {
            return false;
        }
        let cache = ffi::cache();
        cache.attach(vm);
        ffi::set_joypad(self.pad.bits());
        ffi::run_frame(d);
        cache.detach();
        ffi::pump_audio(); // queue this frame's sound for the I2S feed
        // Periodically flush the battery save: an in-game SAVE writes cart RAM
        // (sets the dirty flag), and we mirror it to the .sav within a few seconds
        // so a power-cycle can't lose it. Cart RAM is only touched around saves, so
        // this rarely fires (no per-frame SD hiccup).
        if ffi::cart_dirty() && self.last_save.elapsed() >= Duration::from_secs(3) {
            self.last_save = Instant::now();
            self.write_save(vm);
            ffi::clear_cart_dirty();
        }
        // Keep the volume overlay up for ~1.2 s after a G0 press (the frame redraw
        // above would otherwise erase it immediately).
        if self.vol_shown_at.elapsed() < Duration::from_millis(1200) {
            self.draw_volume_overlay(d);
        }
        true
    }

    // ---- library ----------------------------------------------------------

    fn scan<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        self.count = 0;
        self.sel = 0;
        self.scroll = 0;
        let mut fill = |vm: &VolumeManager<D, T>| -> Option<()> {
            let vol = vm.open_raw_volume(VolumeIdx(0)).ok()?;
            let root = vm.open_root_dir(vol).ok()?;
            let dir = vm.open_dir(root, DIR_APP).and_then(|app| {
                let r = vm.open_dir(app, DIR_ROMS);
                let _ = vm.close_dir(app);
                r
            });
            let _ = vm.close_dir(root);
            let dir = match dir {
                Ok(dir) => dir,
                Err(_) => {
                    let _ = vm.close_volume(vol);
                    return Some(());
                }
            };
            vm.iterate_dir(dir, |e: &DirEntry| {
                if self.count >= MAX_ROMS || e.attributes.is_directory() {
                    return;
                }
                if let Some(entry) = rom_entry(&e.name) {
                    self.roms[self.count] = entry;
                    self.count += 1;
                }
            })
            .ok();
            let _ = vm.close_dir(dir);
            let _ = vm.close_volume(vol);
            Some(())
        };
        fill(vm);
    }

    fn launch<D: BlockDevice, T: TimeSource>(
        &mut self,
        idx: usize,
        vm: &VolumeManager<D, T>,
        d: &mut FrameBuf,
    ) {
        let entry = self.roms[idx];
        match self.open_rom(&entry, vm) {
            Ok(()) => {
                video::clear(d.raw_mut());
                ffi::audio_reset();
                self.state = State::Playing;
                self.pad.clear();
            }
            Err(msg) => {
                self.stop(vm);
                self.state = State::Error;
                self.draw_error(d, msg);
            }
        }
    }

    fn open_rom<D: BlockDevice, T: TimeSource>(
        &mut self,
        entry: &Entry,
        vm: &VolumeManager<D, T>,
    ) -> Result<(), &'static str> {
        let vol = vm.open_raw_volume(VolumeIdx(0)).map_err(|_| "no card")?;
        self.vol = Some(vol);
        let root = vm.open_root_dir(vol).map_err(|_| "fs error")?;
        let app = vm.open_dir(root, DIR_APP).map_err(|_| "no /ECHO")?;
        let _ = vm.close_dir(root);
        let dir = vm.open_dir(app, DIR_ROMS).map_err(|_| "no /ROMS")?;
        let _ = vm.close_dir(app);
        self.dir = Some(dir);
        let file = vm
            .open_file_in_dir(dir, entry.as_str(), Mode::ReadOnly)
            .map_err(|_| "open failed")?;
        self.file = Some(file);
        let len = vm.file_length(file).unwrap_or(0);
        if len < 0x150 {
            return Err("bad ROM");
        }

        // Allocate the bank-cache buffers from the heap (radio is idle during
        // play). Bail gracefully if the heap is too full to host them.
        let cache = ffi::cache();
        if !cache.alloc_buffers() {
            cache.free_buffers();
            return Err("low memory");
        }
        cache.set_file(file, len);
        cache.attach(vm);
        cache.prime();
        cache.detach();

        // Init the core (reads the header through the cache).
        ffi::init().map_err(|_| "init failed")?;

        // Allocate cart RAM to the game's save size (from the heap), then load
        // the .sav if present.
        self.save_size = ffi::save_size();
        ffi::alloc_cart(self.save_size);
        self.set_save_name(entry);
        self.load_save(vm);
        ffi::clear_cart_dirty();
        self.last_save = Instant::now();
        Ok(())
    }

    /// Save cart RAM and release the open volume/file handles.
    fn stop<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        if ffi::cart_dirty() {
            self.write_save(vm);
            ffi::clear_cart_dirty();
        }
        if let Some(f) = self.file.take() {
            let _ = vm.close_file(f);
        }
        if let Some(dir) = self.dir.take() {
            let _ = vm.close_dir(dir);
        }
        if let Some(v) = self.vol.take() {
            let _ = vm.close_volume(v);
        }
        // Release the heap-allocated cart RAM + bank cache back for other apps.
        ffi::free_cart();
        ffi::cache().free_buffers();
        ffi::audio_reset();
        self.save_size = 0;
    }

    // ---- save files -------------------------------------------------------

    fn set_save_name(&mut self, entry: &Entry) {
        // Replace the extension with SAV: "POKERED.GB" -> "POKERED.SAV".
        let s = entry.as_str();
        let base = s.split('.').next().unwrap_or(s);
        let mut n = [0u8; 13];
        let mut i = 0;
        for &b in base.as_bytes().iter().take(8) {
            n[i] = b;
            i += 1;
        }
        for &b in b".SAV" {
            n[i] = b;
            i += 1;
        }
        self.save_name = n;
        self.save_len = i as u8;
    }

    fn save_str(&self) -> &str {
        core::str::from_utf8(&self.save_name[..self.save_len as usize]).unwrap_or("")
    }

    fn load_save<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        if self.save_size == 0 {
            return;
        }
        let Some(dir) = self.dir else { return };
        if let Ok(f) = vm.open_file_in_dir(dir, self.save_str(), Mode::ReadOnly) {
            let ram = ffi::cart();
            let mut off = 0;
            while off < self.save_size {
                match vm.read(f, &mut ram[off..self.save_size]) {
                    Ok(0) => break,
                    Ok(n) => off += n,
                    Err(_) => break,
                }
            }
            let _ = vm.close_file(f);
        }
    }

    fn write_save<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        if self.save_size == 0 {
            return;
        }
        let Some(dir) = self.dir else { return };
        if let Ok(f) = vm.open_file_in_dir(dir, self.save_str(), Mode::ReadWriteCreateOrTruncate) {
            let ram = ffi::cart();
            let _ = vm.write(f, &ram[..self.save_size]);
            let _ = vm.close_file(f);
        }
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
        theme::topbar(d, i18n::t("Game Boy", "Game Boy"));
        if self.count == 0 {
            theme::text(
                d,
                i18n::t("No ROMs in /ECHO/ROMS/", "/ECHO/ROMS/ bos"),
                theme::PAD,
                TOP + 6,
                theme::BODY_FONT,
                theme::MUTED,
            );
            theme::text(
                d,
                i18n::t("Copy .gb / .gbc files there", ".gb / .gbc dosyalari koy"),
                theme::PAD,
                TOP + 22,
                theme::BODY_FONT,
                theme::FAINT,
            );
        } else {
            let end = (self.scroll + VISIBLE).min(self.count);
            for (row, i) in (self.scroll..end).enumerate() {
                let y = TOP + row as i32 * ROW_H;
                let selected = i == self.sel;
                let col = if selected { theme::accent() } else { theme::MUTED };
                if selected {
                    theme::text(d, ">", theme::PAD, y, theme::BODY_FONT, theme::accent());
                }
                theme::text(d, self.roms[i].as_str(), theme::PAD + 12, y, theme::BODY_FONT, col);
            }
        }
        theme::hint(
            d,
            i18n::t("UP/DN pick  ENTER play  ` menu", "YUK/AS sec  ENTER oyna  ` menu"),
        );
    }

    fn draw_error(&self, d: &mut FrameBuf, msg: &str) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Game Boy", "Game Boy"));
        theme::text(d, msg, theme::PAD, TOP + 10, theme::TITLE_FONT, theme::FG);
        theme::hint(d, i18n::t("` menu", "` menu"));
    }
}

/// Boot self-test (`--features emutest`): exercises the GB core on-device over
/// serial, then falls through to the normal menu. No SD ROM or keypress needed.
///
/// Phase A runs the embedded test ROM straight from flash (proves C-link, gb_init,
/// CPU, PPU -> framebuffer). Phase B writes that ROM to the SD card and re-runs it
/// through the *real* path (open file -> bank cache -> SD reads), proving the
/// SD-backed ROM access the actual library uses. The test ROM runs from bank 1, so
/// Phase B genuinely loads a switchable bank from the card.
#[cfg(feature = "emutest")]
pub fn selftest<D: BlockDevice, T: TimeSource>(vm: &VolumeManager<D, T>, d: &mut FrameBuf) {
    use crate::hal::fb::W;
    use embedded_graphics::prelude::*;
    use esp_hal::time::Instant;

    esp_println::println!("\n>>> EMU SELFTEST <<<");

    // -- Phase A: embedded ROM (proves the core) --
    ffi::cache().set_embedded(&test_rom::TEST_ROM);
    ffi::alloc_cart(0);
    if let Err(e) = ffi::init() {
        esp_println::println!("A gb_init: ERR {} | >>> EMU SELFTEST DONE <<<\n", e);
        return;
    }
    ffi::reset_lcd_calls();
    let t0 = Instant::now();
    for _ in 0..60 {
        ffi::run_frame(d);
    }
    let ms = t0.elapsed().as_millis() as u32;
    let px = d.raw_mut()[67 * W + 120];
    esp_println::println!(
        "A(embedded): init=OK fps={} ppu_lines={} px=r{} g{} b{}",
        if ms > 0 { 60_000 / ms } else { 9999 },
        ffi::lcd_calls(),
        px.r(),
        px.g(),
        px.b()
    );

    // -- Phase F: audio — poke a square-wave tone, confirm the APU emits sound --
    ffi::audio_write_reg(0xFF26, 0x80); // NR52: APU power on
    ffi::audio_write_reg(0xFF25, 0xFF); // NR51: all channels to both speakers
    ffi::audio_write_reg(0xFF24, 0x77); // NR50: max volume L+R
    ffi::audio_write_reg(0xFF11, 0x80); // NR11: channel 1, 50% duty
    ffi::audio_write_reg(0xFF12, 0xF0); // NR12: volume 15, no envelope
    ffi::audio_write_reg(0xFF13, 0x00); // NR13: frequency low
    ffi::audio_write_reg(0xFF14, 0x87); // NR14: trigger + frequency high
    let peak = ffi::audio_peak();
    esp_println::println!(
        "F(audio): APU peak={} ({})",
        peak,
        if peak > 64 { "OK non-silent" } else { "FAIL silent" }
    );

    // -- Phase C: input plumbing (Rust pad -> emu_set_joypad -> gb.direct.joypad) --
    let mut input_ok = true;
    for bit in [
        input::btn::A,
        input::btn::B,
        input::btn::START,
        input::btn::SELECT,
        input::btn::UP,
        input::btn::DOWN,
        input::btn::LEFT,
        input::btn::RIGHT,
    ] {
        ffi::set_joypad(bit);
        // active-high `bit` -> wrapper stores ~bit; invert on read to recover it.
        if !ffi::get_joypad() != bit {
            input_ok = false;
        }
    }
    ffi::set_joypad(0);
    esp_println::println!("C(input): Rust->core joypad {}", if input_ok { "OK" } else { "FAIL" });

    // -- Phase B + D: SD round-trip for the bank cache and the .sav --
    selftest_sd(vm, d);

    esp_println::println!(">>> EMU SELFTEST DONE <<<\n");
}

/// Phase B helper: write the test ROM to /ECHO/_EMUTEST.GB, then run it through
/// the SD bank cache and report. /ECHO already exists (Notes/Settings use it), so
/// no directory creation is needed.
#[cfg(feature = "emutest")]
fn selftest_sd<D: BlockDevice, T: TimeSource>(vm: &VolumeManager<D, T>, d: &mut FrameBuf) {
    use embedded_sdmmc::{Mode, VolumeIdx};
    use esp_hal::time::Instant;

    const NAME: &str = "_EMUTEST.GB";
    let Ok(vol) = vm.open_raw_volume(VolumeIdx(0)) else {
        esp_println::println!("B(SD): no card");
        return;
    };
    let cleanup = |dir: Option<embedded_sdmmc::RawDirectory>, file: Option<embedded_sdmmc::RawFile>| {
        if let Some(f) = file {
            let _ = vm.close_file(f);
        }
        if let Some(dir) = dir {
            let _ = vm.close_dir(dir);
        }
        let _ = vm.close_volume(vol);
    };
    let Ok(root) = vm.open_root_dir(vol) else {
        esp_println::println!("B(SD): no fs");
        cleanup(None, None);
        return;
    };
    let dir = match vm.open_dir(root, DIR_APP) {
        Ok(a) => {
            let _ = vm.close_dir(root);
            a
        }
        Err(_) => {
            esp_println::println!("B(SD): no /ECHO");
            cleanup(Some(root), None);
            return;
        }
    };
    // Write the embedded ROM to the card.
    match vm.open_file_in_dir(dir, NAME, Mode::ReadWriteCreateOrTruncate) {
        Ok(f) => {
            let w = vm.write(f, &test_rom::TEST_ROM).is_ok();
            let _ = vm.close_file(f);
            if !w {
                esp_println::println!("B(SD): write failed");
                cleanup(Some(dir), None);
                return;
            }
        }
        Err(_) => {
            esp_println::println!("B(SD): create failed");
            cleanup(Some(dir), None);
            return;
        }
    }
    // Re-open read-only and run through the real bank cache.
    let Ok(file) = vm.open_file_in_dir(dir, NAME, Mode::ReadOnly) else {
        esp_println::println!("B(SD): reopen failed");
        cleanup(Some(dir), None);
        return;
    };
    let len = vm.file_length(file).unwrap_or(0);
    let cache = ffi::cache();
    if !cache.alloc_buffers() {
        esp_println::println!("B(SD): low memory");
        cache.free_buffers();
        cleanup(Some(dir), Some(file));
        return;
    }
    cache.set_file(file, len); // clears embedded mode -> reads now hit SD
    cache.attach(vm);
    cache.prime();
    cache.detach();
    if let Err(e) = ffi::init() {
        esp_println::println!("B(SD): init ERR {}", e);
        ffi::cache().free_buffers();
        cleanup(Some(dir), Some(file));
        return;
    }
    ffi::reset_lcd_calls();
    let t0 = Instant::now();
    for _ in 0..60 {
        let c = ffi::cache();
        c.attach(vm);
        ffi::run_frame(d);
        ffi::cache().detach();
    }
    let ms = t0.elapsed().as_millis() as u32;
    esp_println::println!(
        "B(SD): rom={}B init=OK fps={} ppu_lines={} bank_loads={} (>=2 = SD cache works)",
        len,
        if ms > 0 { 60_000 / ms } else { 9999 },
        ffi::lcd_calls(),
        ffi::cache().bank_loads()
    );
    ffi::cache().free_buffers();
    let _ = vm.close_file(file);

    // -- Phase D: cart-RAM .sav round-trip (write pattern -> SD -> clear -> reload) --
    ffi::alloc_cart(512);
    for (i, b) in ffi::cart().iter_mut().enumerate() {
        *b = (i as u8) ^ 0x5A;
    }
    let mut saved = false;
    if let Ok(f) = vm.open_file_in_dir(dir, "_EMUTEST.SAV", Mode::ReadWriteCreateOrTruncate) {
        saved = vm.write(f, ffi::cart()).is_ok();
        let _ = vm.close_file(f);
    }
    for b in ffi::cart().iter_mut() {
        *b = 0;
    }
    let mut save_ok = false;
    if saved {
        if let Ok(f) = vm.open_file_in_dir(dir, "_EMUTEST.SAV", Mode::ReadOnly) {
            let cart = ffi::cart();
            let mut off = 0;
            while off < cart.len() {
                match vm.read(f, &mut cart[off..]) {
                    Ok(0) => break,
                    Ok(n) => off += n,
                    Err(_) => break,
                }
            }
            let _ = vm.close_file(f);
            save_ok = ffi::cart().iter().enumerate().all(|(i, &b)| b == ((i as u8) ^ 0x5A));
        }
    }
    ffi::free_cart();
    esp_println::println!("D(save): .sav round-trip {}", if save_ok { "OK" } else { "FAIL" });

    // -- Phase E: library scan finds a ROM in /ECHO/ROMS/ --
    // Write the ROM, then RELEASE the volume before scanning: the VolumeManager
    // allows only one open volume, and `scan` opens its own (as it does in the
    // real app, where nothing else holds the card open).
    let _ = vm.make_dir_in_dir(dir, DIR_ROMS); // ok if it already exists
    if let Ok(roms) = vm.open_dir(dir, DIR_ROMS) {
        if let Ok(f) = vm.open_file_in_dir(roms, "_EMUTEST.GB", Mode::ReadWriteCreateOrTruncate) {
            let _ = vm.write(f, &test_rom::TEST_ROM[..0x200]);
            let _ = vm.close_file(f);
        }
        let _ = vm.close_dir(roms);
    }
    cleanup(Some(dir), None);

    let mut probe = Emu::new();
    probe.scan(vm);
    esp_println::println!(
        "E(library): scan found {} ROM(s) in /ECHO/ROMS/ (first: {})",
        probe.count,
        if probe.count > 0 { probe.roms[0].as_str() } else { "-" }
    );
}

/// Build a library entry from a directory entry's 8.3 name, keeping only
/// `.GB` / `.GBC` files. Returns the openable/display name "BASE.EXT".
fn rom_entry(sfn: &ShortFileName) -> Option<Entry> {
    let base = sfn.base_name();
    let ext = sfn.extension();
    if !(ext.eq_ignore_ascii_case(b"GB") || ext.eq_ignore_ascii_case(b"GBC")) {
        return None;
    }
    let mut name = [0u8; 13];
    let mut i = 0;
    for &b in base.iter().take(8) {
        name[i] = b;
        i += 1;
    }
    name[i] = b'.';
    i += 1;
    for &b in ext.iter().take(3) {
        name[i] = b;
        i += 1;
    }
    Some(Entry {
        name,
        len: i as u8,
    })
}

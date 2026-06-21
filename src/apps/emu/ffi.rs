//! FFI bridge to the vendored Peanut-GB core (compiled by `build.rs`).
//!
//! Peanut-GB's callbacks are bare C function pointers with no user context, so the
//! emulator state lives in `static`s reached from the `#[no_mangle]` callbacks
//! below. This is sound because the emulator only ever runs inside the single main
//! loop — never re-entrantly and never from another core.
//!
//! Memory: the 512 KB SRAM has no room to *permanently* reserve the ROM bank cache
//! plus cart RAM, so the cartridge RAM (up to 32 KB) is allocated from the heap
//! only while a game is loaded (the WiFi/BLE radio that normally owns the heap is
//! idle during play). The bank cache stays a `static` (see `rom`).
//!
//! The C side (`vendor/peanut_gb/wrapper.c`) exposes a thin `emu_*` ABI and calls
//! back into the four `rust_*` functions here.

use super::{rom::RomCache, video};
use crate::hal::fb::{FrameBuf, H, W};
use alloc::boxed::Box;
use core::ptr::{addr_of, addr_of_mut};
use embedded_graphics::pixelcolor::Rgb565;

static mut ROM: RomCache = RomCache::new();
/// Cartridge RAM, heap-allocated to the loaded game's exact save size (0 = none).
static mut CART: Option<Box<[u8]>> = None;
/// Framebuffer pointer, valid only for the duration of `run_frame`.
static mut FB: *mut Rgb565 = core::ptr::null_mut();

// ---- C ABI exposed by wrapper.c -------------------------------------------

extern "C" {
    /// Initialise the core against the `rust_*` callbacks. 0 = ok, else error code.
    fn emu_init() -> i32;
    /// Run one ~70224-cycle frame (drives `rust_lcd_line` for each visible line).
    fn emu_run_frame();
    /// Set the held buttons (active-high; see `input::btn`).
    fn emu_set_joypad(bits: u8);
    /// Read back the joypad register (active-low), for the input self-test.
    #[cfg(feature = "emutest")]
    fn emu_get_joypad() -> u8;
    /// Cartridge-RAM save size in bytes for the loaded ROM.
    fn emu_save_size() -> u32;
    /// int16 stereo samples the APU produces per emulated frame.
    fn emu_audio_count() -> u32;
    /// Fill `out` (>= emu_audio_count()) with one frame of APU audio.
    fn emu_audio_frame(out: *mut i16);
    /// Poke an APU register directly (self-test tone only).
    #[cfg(feature = "emutest")]
    fn emu_audio_write(addr: u16, val: u8);
}

// Audio ring buffer: single producer (the emu frame) -> single consumer (the I2S
// fill in the main loop), same thread, so plain wrapping indices suffice.
// int16s of audio ring. The GBC build trims it (the larger CGB core needs the
// RAM); both refill every loop iteration so the smaller ring is still fine.
#[cfg(feature = "emugbc")]
const RING: usize = 1024;
#[cfg(not(feature = "emugbc"))]
const RING: usize = 3072;
static mut ARING: [i16; RING] = [0; RING];
static mut AHEAD: usize = 0;
static mut ATAIL: usize = 0;

/// Generate one frame of GB audio into the ring (call right after `run_frame`).
pub fn pump_audio() {
    let mut buf = [0i16; 700];
    let n = (unsafe { emu_audio_count() } as usize).min(buf.len());
    unsafe { emu_audio_frame(buf.as_mut_ptr()) };
    let ring = unsafe { &mut *addr_of_mut!(ARING) };
    let (mut head, tail) = unsafe { (AHEAD, ATAIL) };
    for &s in &buf[..n] {
        let next = (head + 1) % RING;
        if next == tail {
            break; // full: drop the overflow
        }
        ring[head] = s;
        head = next;
    }
    unsafe { AHEAD = head };
}

/// Output volume, 0..=100 in 20% steps. G0 cycles it while playing (the raw APU
/// is loud). Starts at a comfortable 60%.
static mut AUDIO_VOL: u8 = 60;

pub fn volume() -> u8 {
    unsafe { *addr_of!(AUDIO_VOL) }
}

/// Step the volume 60->80->100->0->20->... and return the new level.
pub fn cycle_volume() -> u8 {
    let v = unsafe { *addr_of!(AUDIO_VOL) };
    let n = if v >= 100 { 0 } else { v + 20 };
    unsafe {
        *addr_of_mut!(AUDIO_VOL) = n;
    }
    n
}

/// Drain `out.len()` samples into the I2S buffer (silence on underrun), scaled by
/// the current volume.
pub fn audio_fill(out: &mut [i16]) {
    let ring = unsafe { &*addr_of!(ARING) };
    let head = unsafe { AHEAD };
    let vol = unsafe { *addr_of!(AUDIO_VOL) } as i32;
    let mut tail = unsafe { ATAIL };
    for o in out.iter_mut() {
        if tail == head {
            *o = 0;
        } else {
            *o = ((ring[tail] as i32 * vol) / 100) as i16;
            tail = (tail + 1) % RING;
        }
    }
    unsafe { ATAIL = tail };
}

/// Reset the audio ring (on entering/leaving a game).
pub fn audio_reset() {
    unsafe {
        AHEAD = 0;
        ATAIL = 0;
    }
}

#[cfg(feature = "emutest")]
pub fn audio_write_reg(addr: u16, val: u8) {
    unsafe { emu_audio_write(addr, val) }
}

/// Peak |sample| of one freshly generated APU frame (self-test).
#[cfg(feature = "emutest")]
pub fn audio_peak() -> i16 {
    let mut buf = [0i16; 700];
    let n = (unsafe { emu_audio_count() } as usize).min(buf.len());
    unsafe { emu_audio_frame(buf.as_mut_ptr()) };
    buf[..n].iter().map(|s| s.saturating_abs()).max().unwrap_or(0)
}

// ---- Rust callbacks invoked by the C core ---------------------------------

#[no_mangle]
extern "C" fn rust_rom_read(addr: u32) -> u8 {
    unsafe { (*addr_of_mut!(ROM)).read(addr) }
}

#[no_mangle]
extern "C" fn rust_ram_read(addr: u32) -> u8 {
    match unsafe { &*addr_of!(CART) } {
        Some(ram) => ram.get(addr as usize).copied().unwrap_or(0xFF),
        None => 0xFF,
    }
}

/// Set whenever the game writes cart RAM, so the app can flush the `.sav`
/// periodically (an in-game SAVE writes cart RAM) instead of only on a clean exit
/// — the device gets power-cycled a lot, and we don't want to lose the save.
static mut CART_DIRTY: bool = false;

#[no_mangle]
extern "C" fn rust_ram_write(addr: u32, val: u8) {
    if let Some(ram) = unsafe { &mut *addr_of_mut!(CART) } {
        if let Some(b) = ram.get_mut(addr as usize) {
            *b = val;
            unsafe {
                *addr_of_mut!(CART_DIRTY) = true;
            }
        }
    }
}

/// True if cart RAM changed since the last `clear_cart_dirty`.
pub fn cart_dirty() -> bool {
    unsafe { *addr_of!(CART_DIRTY) }
}

pub fn clear_cart_dirty() {
    unsafe {
        *addr_of_mut!(CART_DIRTY) = false;
    }
}

/// Count of `lcd_draw_line` callbacks, for the boot self-test diagnostics.
#[cfg(feature = "emutest")]
static mut LCD_CALLS: u32 = 0;

/// DMG core (Peanut-GB) LCD callback: 160 palette indices (2-bit shade).
#[cfg(not(feature = "emugbc"))]
#[no_mangle]
extern "C" fn rust_lcd_line(pixels: *const u8, line: u8) {
    #[cfg(feature = "emutest")]
    unsafe {
        *addr_of_mut!(LCD_CALLS) = (*addr_of!(LCD_CALLS)).wrapping_add(1);
    }
    let fb = unsafe { *addr_of!(FB) };
    if fb.is_null() || pixels.is_null() {
        return;
    }
    // SAFETY: the wrapper always passes a 160-byte scanline; `fb` is our framebuffer.
    let px = unsafe { &*(pixels as *const [u8; video::GB_W]) };
    let fbs = unsafe { core::slice::from_raw_parts_mut(fb, W * H) };
    video::draw_line(fbs, px, line);
}

/// CGB core (Walnut-CGB) LCD callback: 160 pixels already resolved to RGB565 by
/// the wrapper (works for both Game Boy Color colour and DMG greyscale).
#[cfg(feature = "emugbc")]
#[no_mangle]
extern "C" fn rust_lcd_line_rgb(pixels: *const u16, line: u8) {
    #[cfg(feature = "emutest")]
    unsafe {
        *addr_of_mut!(LCD_CALLS) = (*addr_of!(LCD_CALLS)).wrapping_add(1);
    }
    let fb = unsafe { *addr_of!(FB) };
    if fb.is_null() || pixels.is_null() {
        return;
    }
    // SAFETY: the wrapper always passes a 160-entry RGB565 scanline.
    let px = unsafe { &*(pixels as *const [u16; video::GB_W]) };
    let fbs = unsafe { core::slice::from_raw_parts_mut(fb, W * H) };
    video::draw_line_rgb(fbs, px, line);
}

// ---- Safe-ish surface used by the app -------------------------------------

/// The ROM bank cache (the app binds the open file + SD access through it).
pub fn cache() -> &'static mut RomCache {
    unsafe { &mut *addr_of_mut!(ROM) }
}

/// Allocate cart RAM of the given size from the heap (frees any previous one).
pub fn alloc_cart(size: usize) {
    let cart = if size == 0 {
        None
    } else {
        Some(alloc::vec![0u8; size].into_boxed_slice())
    };
    unsafe {
        *addr_of_mut!(CART) = cart;
    }
}

/// Free the cart RAM (call when leaving a game).
pub fn free_cart() {
    unsafe {
        *addr_of_mut!(CART) = None;
    }
}

/// The current cart RAM (empty slice if the game has none / not allocated).
pub fn cart() -> &'static mut [u8] {
    match unsafe { &mut *addr_of_mut!(CART) } {
        Some(ram) => &mut ram[..],
        None => &mut [],
    }
}

/// Initialise the core for the currently bound ROM.
pub fn init() -> Result<(), i32> {
    match unsafe { emu_init() } {
        0 => Ok(()),
        e => Err(e),
    }
}

/// Run one frame, drawing into `fb`. The ROM cache must be `attach`ed first.
pub fn run_frame(fb: &mut FrameBuf) {
    unsafe {
        *addr_of_mut!(FB) = fb.raw_mut().as_mut_ptr();
        emu_run_frame();
        *addr_of_mut!(FB) = core::ptr::null_mut();
    }
}

pub fn set_joypad(bits: u8) {
    unsafe { emu_set_joypad(bits) }
}

/// Read back the joypad register (active-low). Self-test only.
#[cfg(feature = "emutest")]
pub fn get_joypad() -> u8 {
    unsafe { emu_get_joypad() }
}

pub fn save_size() -> usize {
    unsafe { emu_save_size() as usize }
}

#[cfg(feature = "emutest")]
pub fn reset_lcd_calls() {
    unsafe {
        *addr_of_mut!(LCD_CALLS) = 0;
    }
}

#[cfg(feature = "emutest")]
pub fn lcd_calls() -> u32 {
    unsafe { *addr_of!(LCD_CALLS) }
}

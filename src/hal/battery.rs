//! Battery state shared across screens (updated from the ADC in the main loop,
//! read by the top-bar indicator and the Charge screen).

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

static LEVEL: AtomicU8 = AtomicU8::new(0);
static PRESENT: AtomicBool = AtomicBool::new(false);

/// `level` 0..=100, `present` = a plausible battery voltage was read.
pub fn set(level: u8, present: bool) {
    LEVEL.store(level.min(100), Ordering::Relaxed);
    PRESENT.store(present, Ordering::Relaxed);
}

pub fn level() -> u8 {
    LEVEL.load(Ordering::Relaxed)
}

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

/// Battery millivolts (after the ×2 divider) -> 0..=100%.
/// Identical to bmorcelli's Launcher and M5Unified: linear 3300 mV..4100 mV
/// (`(mv - 3300) / 800 * 100`), so our reading matches what the launcher shows.
pub fn mv_to_percent(mv: u16) -> u8 {
    let mv = mv.clamp(3300, 4100);
    (((mv - 3300) as u32 * 100) / 800) as u8
}

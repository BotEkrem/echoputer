//! Minimal inline localisation. A global current-language flag (like the theme
//! accent) plus a `t(en, tr)` picker used at draw time, so switching language
//! re-renders every screen live — no reboot, no string-table bookkeeping.
//!
//! The display font is ASCII-only, so the Turkish strings are written WITHOUT
//! diacritics (ç/ş/ı/ğ/ü/ö -> c/s/i/g/u/o). Fully readable; guarantees correct
//! rendering on the ST7789 without swapping in an ISO-8859-9 font.

use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Tr,
}

/// Number of supported languages.
pub const COUNT: usize = 2;
/// Setting values, shown in Settings. ASCII only.
pub const NAMES: [&str; COUNT] = ["English", "Turkce"];

static LANG: AtomicU8 = AtomicU8::new(0); // 0 = English (default)

pub fn set_idx(i: u8) {
    LANG.store(if (i as usize) < COUNT { i } else { 0 }, Ordering::Relaxed);
}
pub fn idx() -> u8 {
    LANG.load(Ordering::Relaxed)
}
pub fn current() -> Lang {
    if idx() == 1 {
        Lang::Tr
    } else {
        Lang::En
    }
}

/// Pick the string for the active language.
#[inline]
pub fn t(en: &'static str, tr: &'static str) -> &'static str {
    match current() {
        Lang::En => en,
        Lang::Tr => tr,
    }
}

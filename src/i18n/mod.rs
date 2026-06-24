//! Localisation. A global current-language flag (like the theme accent) plus a
//! `t(msg)` picker used at draw time, so switching language re-renders every
//! screen live. Translation text lives in the per-screen CATALOG modules in this
//! directory (`menu`, `hacking`, …) as [`Msg`] consts — never inline in the app
//! code, so all translations are in one place and adding a language is local.
//!
//! The display font is ASCII-only, so the Turkish strings are written WITHOUT
//! diacritics (ç/ş/ı/ğ/ü/ö -> c/s/i/g/u/o). Fully readable; guarantees correct
//! rendering on the ST7789 without swapping in an ISO-8859-9 font.

use core::sync::atomic::{AtomicU8, Ordering};

// Per-screen translation catalogs — all translation text lives in these modules.
pub mod app;
pub mod calc;
pub mod charge;
pub mod chip8;
pub mod convert;
pub mod dice;
#[cfg(feature = "emu")]
pub mod emu;
pub mod g2048;
pub mod games;
pub mod hacking;
pub mod ir;
pub mod level;
pub mod menu;
pub mod misc;
pub mod notes;
pub mod player;
pub mod pong;
pub mod qr;
#[cfg(not(feature = "emugbc"))]
pub mod recorder;
pub mod settings;
pub mod snake;
pub mod stepcount;
pub mod stopwatch;
pub mod sysinfo;
pub mod tetris;
pub mod webui;
pub mod wiki;

/// Number of supported languages.
pub const COUNT: usize = 2;
/// Setting values, shown in Settings. ASCII only.
pub const NAMES: [&str; COUNT] = ["English", "Turkce"];

/// A translated message: one string per language, ordered like [`NAMES`]. The
/// per-screen catalog modules in this directory define `Msg` consts; call sites
/// pass the const to [`t`], so no translation text lives in the app code.
pub type Msg = [&'static str; COUNT];

static LANG: AtomicU8 = AtomicU8::new(0); // 0 = English (default)

pub fn set_idx(i: u8) {
    LANG.store(if (i as usize) < COUNT { i } else { 0 }, Ordering::Relaxed);
}
pub fn idx() -> u8 {
    LANG.load(Ordering::Relaxed)
}

/// Pick the active language's string from a catalog [`Msg`].
#[inline]
pub fn t(m: Msg) -> &'static str {
    m[idx() as usize]
}

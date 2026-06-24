//! The user-facing screens: the app launcher, boot splash, and each app
//! (Synthwave, File Browser, Charge, Settings, Hacking) plus their helpers.
//! Re-exported flat at the crate root (see main.rs). The arcade games live under
//! `games/` and the small extras under `misc/` (each a sub-launcher + its apps).

pub mod browser;
pub mod charge;
#[cfg(feature = "emu")]
pub mod emu;
pub mod games;
pub mod hacking;
pub mod menu;
pub mod misc;
pub mod notes;
pub mod player;
pub mod repl;
pub mod scales;
pub mod settings;
pub mod splash;
pub mod stopwatch;
pub mod synth;
pub mod sysinfo;
pub mod ui;
pub mod webui;
pub mod wiki;

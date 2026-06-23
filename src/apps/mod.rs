//! The user-facing screens: the app launcher, boot splash, and each app
//! (Synthwave, File Browser, Charge, Settings, Hacking) plus their helpers.
//! Re-exported flat at the crate root (see main.rs).

pub mod browser;
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
pub mod qr_encode;
#[cfg(not(feature = "emugbc"))]
pub mod recorder;
pub mod repl;
pub mod scales;
pub mod settings;
pub mod snake;
pub mod splash;
pub mod stepcount;
pub mod stopwatch;
pub mod synth;
pub mod sysinfo;
pub mod tetris;
pub mod ui;
pub mod webui;
pub mod wiki;

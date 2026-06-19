//! Hardware abstraction: low-level board drivers + the off-screen framebuffer +
//! the keyboard character map. Re-exported flat at the crate root (see main.rs)
//! so the rest of the firmware can keep using `crate::battery`, `crate::fb`, etc.

pub mod battery;
pub mod es8311;
pub mod fb;
pub mod keymap;
pub mod tca8418;
pub mod ws2812;

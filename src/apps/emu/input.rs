//! Keyboard -> Game Boy buttons.
//!
//! Games need *held* state, not one-shot presses, so the app feeds every press
//! and release here and we keep a bitmask. `bits()` returns an active-high mask
//! (1 = pressed); the C wrapper inverts it into Peanut-GB's active-low joypad.
//!
//! Layout (two-handed, holding the device like a Game Boy):
//!   D-pad  -> the `; , . /` arrow cluster (right hand)
//!   A      -> `l`     B -> `k`   (right of the home row)
//!   Start  -> Enter   Select -> Space

/// Active-high button bits, matching Peanut-GB's JOYPAD_* order so the wrapper's
/// conversion is a single bitwise-NOT.
pub mod btn {
    pub const A: u8 = 1 << 0;
    pub const B: u8 = 1 << 1;
    pub const SELECT: u8 = 1 << 2;
    pub const START: u8 = 1 << 3;
    pub const RIGHT: u8 = 1 << 4;
    pub const LEFT: u8 = 1 << 5;
    pub const UP: u8 = 1 << 6;
    pub const DOWN: u8 = 1 << 7;
}

// Action keys (the arrow cluster lives in crate::K_*).
const K_A: (u8, u8) = (2, 10); // 'l'
const K_B: (u8, u8) = (2, 9); // 'k'
const K_SELECT: (u8, u8) = (3, 13); // space

fn bit_for(rc: (u8, u8)) -> Option<u8> {
    Some(match rc {
        crate::K_UP => btn::UP,
        crate::K_DOWN => btn::DOWN,
        crate::K_LEFT => btn::LEFT,
        crate::K_RIGHT => btn::RIGHT,
        crate::K_ENTER => btn::START,
        K_A => btn::A,
        K_B => btn::B,
        K_SELECT => btn::SELECT,
        _ => return None,
    })
}

/// Held-button state for one game session.
pub struct Pad {
    held: u8,
}

impl Pad {
    pub fn new() -> Self {
        Self { held: 0 }
    }

    pub fn clear(&mut self) {
        self.held = 0;
    }

    /// Apply a key event. Returns true if it mapped to a GB button (so the caller
    /// knows the key was consumed by the game rather than a UI action).
    pub fn set(&mut self, rc: (u8, u8), pressed: bool) -> bool {
        match bit_for(rc) {
            Some(bit) => {
                if pressed {
                    self.held |= bit;
                } else {
                    self.held &= !bit;
                }
                true
            }
            None => false,
        }
    }

    /// Active-high pressed mask for `emu_set_joypad`.
    pub fn bits(&self) -> u8 {
        self.held
    }
}

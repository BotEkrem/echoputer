//! TCA8418 I2C keypad scanner — init, FIFO read, and decode/remap to the
//! Cardputer ADV's logical 4x14 keyboard layout.
//!
//! Init + decode are taken verbatim from M5's `keyboard.cpp` (CardputerADV branch)
//! and the Adafruit_TCA8418 library. We poll the FIFO instead of using the INT
//! line (GPIO11) — simpler, and equivalent as long as we clear INT_STAT.

use embedded_hal::i2c::I2c;

/// 7-bit I2C address (fixed).
pub const ADDR: u8 = 0x34;

// Register map (subset we use).
const CFG: u8 = 0x01;
const INT_STAT: u8 = 0x02;
const KEY_LCK_EC: u8 = 0x03;
const KEY_EVENT_A: u8 = 0x04;

/// A decoded key event in the logical 4x14 layout.
#[derive(Clone, Copy)]
pub struct KeyEvent {
    pub pressed: bool,
    pub row: u8, // 0..3  (row 0 = number row, row 3 = bottom row)
    pub col: u8, // 0..13
}

fn w<I: I2c>(i2c: &mut I, reg: u8, val: u8) -> Result<(), I::Error> {
    i2c.write(ADDR, &[reg, val])
}

fn r<I: I2c>(i2c: &mut I, reg: u8) -> Result<u8, I::Error> {
    let mut b = [0u8; 1];
    i2c.write_read(ADDR, &[reg], &mut b)?;
    Ok(b[0])
}

/// Configure the controller as a 7x8 key matrix with key-event interrupts.
pub fn init<I: I2c>(i2c: &mut I) -> Result<(), I::Error> {
    // all GPIO = input
    w(i2c, 0x23, 0x00)?;
    w(i2c, 0x24, 0x00)?;
    w(i2c, 0x25, 0x00)?;
    // all pins generate key events
    w(i2c, 0x20, 0xFF)?;
    w(i2c, 0x21, 0xFF)?;
    w(i2c, 0x22, 0xFF)?;
    // all pins falling-edge interrupt
    w(i2c, 0x26, 0x00)?;
    w(i2c, 0x27, 0x00)?;
    w(i2c, 0x28, 0x00)?;
    // enable interrupt on all pins
    w(i2c, 0x1A, 0xFF)?;
    w(i2c, 0x1B, 0xFF)?;
    w(i2c, 0x1C, 0xFF)?;
    // matrix(7, 8): rows mask = 0x7F, cols mask = 0xFF (KP_GPIO_3 left default)
    w(i2c, 0x1D, 0x7F)?;
    w(i2c, 0x1E, 0xFF)?;
    flush(i2c)?;
    // enable key-event (KE_IEN) + GPI (GPI_IEN) interrupts
    let cfg = r(i2c, CFG)?;
    w(i2c, CFG, cfg | 0x03)?;
    Ok(())
}

/// Drain the key-event FIFO and clear interrupt status.
pub fn flush<I: I2c>(i2c: &mut I) -> Result<(), I::Error> {
    for _ in 0..16 {
        if r(i2c, KEY_EVENT_A)? == 0 {
            break;
        }
    }
    let _ = r(i2c, 0x11)?;
    let _ = r(i2c, 0x12)?;
    let _ = r(i2c, 0x13)?;
    w(i2c, INT_STAT, 0x03)?;
    Ok(())
}

/// Pop and decode one key event. Returns `None` when the FIFO is empty.
pub fn next_event<I: I2c>(i2c: &mut I) -> Result<Option<KeyEvent>, I::Error> {
    let count = r(i2c, KEY_LCK_EC)? & 0x0F;
    if count == 0 {
        // still clear K_INT so the controller releases its INT line
        w(i2c, INT_STAT, 0x01)?;
        return Ok(None);
    }
    let ev = r(i2c, KEY_EVENT_A)?;
    w(i2c, INT_STAT, 0x01)?; // clear K_INT
    if ev == 0 {
        return Ok(None);
    }

    // Per the TCA8418 datasheet, the key-event MSB is the make/break flag:
    // bit7 = 1 => key PRESS, bit7 = 0 => key RELEASE. (This was inverted before,
    // which made actions fire on finger-lift and broke hold-to-repeat.)
    let pressed = (ev & 0x80) != 0;
    let n = (ev & 0x7F).wrapping_sub(1);
    let hw_row = n / 10; // 0..6
    let hw_col = n % 10; // 0..9 (only 0..7 are matrix columns)
    if hw_col > 7 {
        return Ok(None); // GPIO event, not part of the 7x8 matrix
    }

    // M5 remap -> logical 4x14 (identical to the original Cardputer layout)
    let row = hw_col % 4;
    let col = hw_row * 2 + if hw_col > 3 { 1 } else { 0 };
    Ok(Some(KeyEvent { pressed, row, col }))
}

//! ES8311 mono audio codec — init over I2C.
//!
//! This is M5Unified's *exact* Cardputer ADV speaker bring-up
//! (`_speaker_enabled_cb_cardputer_adv`): 8 register writes. The codec takes its
//! MCLK from the BCLK/SCK pin (reg 0x01 = 0xB5), so the I2S peripheral must NOT
//! drive a separate MCLK pin. 16-bit I2S slave (SDP regs left at reset default).

use embedded_hal::i2c::I2c;

/// 7-bit I2C address (CE tied low).
pub const ADDR: u8 = 0x18;

/// (reg, value) pairs, verbatim from M5Unified.
const INIT: [(u8, u8); 8] = [
    (0x00, 0x80), // RESET / CSM power-on
    (0x01, 0xB5), // CLK_MANAGER: MCLK sourced from BCLK pin, clocks enabled
    (0x02, 0x18), // CLK_MANAGER: MULT_PRE = 3
    (0x0D, 0x01), // SYSTEM: power up analog circuitry
    (0x12, 0x00), // SYSTEM: power up DAC
    (0x13, 0x10), // SYSTEM: enable output drive
    (0x32, 0xBF), // DAC digital volume (~0 dB)
    (0x37, 0x08), // bypass DAC equalizer
];

/// Run the full init sequence. Returns Err on the first failed I2C write.
pub fn init<I: I2c>(i2c: &mut I) -> Result<(), I::Error> {
    for (reg, val) in INIT {
        i2c.write(ADDR, &[reg, val])?;
    }
    Ok(())
}

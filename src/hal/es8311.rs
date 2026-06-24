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

/// ES8311 ADC / microphone enable, applied AFTER `init()` (Cardputer ADV). Powers up
/// the PGA + ADC, routes the Mic1 differential input at +24 dB, and forces 16-bit ADC
/// output to match the ESP32-S3 16-bit I2S RX. The DAC/speaker keeps working under the
/// 0x01=0xBA clock value, so playback + capture run together (the I2S TX must keep
/// running to clock the codec). Source: M5Unified `_microphone_enabled_cb_cardputer_adv`
/// cross-checked against the ES8311 User Guide Rev1.11.
#[cfg(not(feature = "emugbc"))]
const ADC_INIT: [(u8, u8); 6] = [
    (0x01, 0xBF), // CLK_MANAGER: ALL clock domains on (DAC bits 0xB5 | ADC bits 0x0A).
    // 0xBA cleared the DAC-clock bits -> playback went silent; 0xBF keeps DAC + ADC.
    (0x0E, 0x02), // SYSTEM: power up analog PGA + ADC modulator
    (0x14, 0x18), // SYSTEM: LINSEL=1 (Mic1p-Mic1n) + PGAGAIN=8 -> +24 dB for the MEMS mic
    (0x17, 0xBF), // ADC: digital volume 0 dB
    (0x1C, 0x6A), // ADC: EQ bypass + DC-offset removal (HPF)
    (0x0A, 0x0C), // SDP_OUT: 16-bit word length (match the 16-bit I2S RX)
];

/// Enable the microphone (ADC) path. Call after [`init`]. Returns Err on the first
/// failed I2C write. (Only the non-emugbc builds include the mic recorder.)
#[cfg(not(feature = "emugbc"))]
pub fn enable_adc<I: I2c>(i2c: &mut I) -> Result<(), I::Error> {
    for (reg, val) in ADC_INIT {
        i2c.write(ADDR, &[reg, val])?;
    }
    Ok(())
}

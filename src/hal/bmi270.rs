//! BMI270 6-axis IMU (accel + gyro) on the internal I2C bus (SDA8/SCL9), for the Misc
//! bubble level + step counter. The BMI270 boots with no firmware, so [`init`] uploads
//! an 8 KB config blob (see `bmi270_config.rs`, in flash) before the sensor measures.
//!
//! There is NO magnetometer on the Cardputer ADV, so a true compass isn't possible —
//! the apps use the accelerometer only. Thin register driver over a borrowed `&mut I2c`,
//! mirroring es8311.rs / tca8418.rs; the resolved address + ready flag live in atomics
//! so the apps can read without holding driver state.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::I2c;
use esp_hal::delay::Delay;

use crate::hal::bmi270_config::BMI270_CONFIG;

const PRIMARY: u8 = 0x68;
const SECONDARY: u8 = 0x69; // M5 may strap either; probe both
const CHIP_ID_VAL: u8 = 0x24;

static ADDR: AtomicU8 = AtomicU8::new(PRIMARY);
static READY: AtomicBool = AtomicBool::new(false);

/// True once [`init`] found + configured a BMI270.
pub fn ready() -> bool {
    READY.load(Ordering::Relaxed)
}

fn rd<I: I2c>(i2c: &mut I, addr: u8, reg: u8) -> Option<u8> {
    let mut b = [0u8; 1];
    i2c.write_read(addr, &[reg], &mut b).ok()?;
    Some(b[0])
}

fn wr<I: I2c>(i2c: &mut I, addr: u8, reg: u8, val: u8) -> bool {
    i2c.write(addr, &[reg, val]).is_ok()
}

/// Probe (0x68 then 0x69), upload the config blob, enable the accelerometer. Call once
/// at boot after the other I2C parts are up. Returns false if no BMI270 answers — the
/// apps then show "no IMU" instead of garbage.
pub fn init<I: I2c>(i2c: &mut I) -> bool {
    let addr = if rd(i2c, PRIMARY, 0x00) == Some(CHIP_ID_VAL) {
        PRIMARY
    } else if rd(i2c, SECONDARY, 0x00) == Some(CHIP_ID_VAL) {
        SECONDARY
    } else {
        return false;
    };
    ADDR.store(addr, Ordering::Relaxed);
    let mut d = Delay::new();

    if !wr(i2c, addr, 0x7C, 0x00) {
        return false; // PWR_CONF: disable advanced power save before the config load
    }
    d.delay_us(450);
    if !wr(i2c, addr, 0x59, 0x00) {
        return false; // INIT_CTRL = 0: prepare for config load
    }

    // Stream the 8 KB config to INIT_DATA (0x5E) in 256-byte chunks, setting the load
    // address (in 16-bit words) before each chunk.
    const CH: usize = 256;
    let mut buf = [0u8; 1 + CH];
    buf[0] = 0x5E;
    let mut idx = 0;
    while idx + CH <= BMI270_CONFIG.len() {
        let word = (idx / 2) as u16;
        if !wr(i2c, addr, 0x5B, (word & 0x0F) as u8) || !wr(i2c, addr, 0x5C, (word >> 4) as u8) {
            return false;
        }
        buf[1..].copy_from_slice(&BMI270_CONFIG[idx..idx + CH]);
        if i2c.write(addr, &buf).is_err() {
            return false;
        }
        idx += CH;
    }
    if !wr(i2c, addr, 0x59, 0x01) {
        return false; // INIT_CTRL = 1: config load complete
    }
    d.delay_ms(20);

    // Poll INTERNAL_STATUS (0x21) until init_ok (low nibble == 1).
    let mut ok = false;
    for _ in 0..50 {
        if rd(i2c, addr, 0x21).map(|s| s & 0x0F) == Some(0x01) {
            ok = true;
            break;
        }
        d.delay_ms(1);
    }
    if !ok {
        return false;
    }

    wr(i2c, addr, 0x7D, 0x0E); // PWR_CTRL: accel + gyro + temp on
    wr(i2c, addr, 0x40, 0xA8); // ACC_CONF: 100 Hz, normal filter
    wr(i2c, addr, 0x41, 0x01); // ACC_RANGE: +-4 g
    wr(i2c, addr, 0x42, 0xA8); // GYR_CONF: 100 Hz, normal filter
    wr(i2c, addr, 0x43, 0x00); // GYR_RANGE: +-2000 dps
    d.delay_ms(5);
    READY.store(true, Ordering::Relaxed);
    true
}

/// Read the accelerometer as [x, y, z] in g (+-4 g full scale). None on I2C error.
pub fn read_accel<I: I2c>(i2c: &mut I) -> Option<[f32; 3]> {
    let addr = ADDR.load(Ordering::Relaxed);
    let mut b = [0u8; 6];
    i2c.write_read(addr, &[0x0C], &mut b).ok()?;
    const S: f32 = 4.0 / 32768.0;
    Some([
        i16::from_le_bytes([b[0], b[1]]) as f32 * S,
        i16::from_le_bytes([b[2], b[3]]) as f32 * S,
        i16::from_le_bytes([b[4], b[5]]) as f32 * S,
    ])
}

/// Read the gyroscope as [x, y, z] in deg/s (+-2000 dps full scale). None on I2C
/// error. Data register 0x12 (DATA_14), right after the accel block.
pub fn read_gyro<I: I2c>(i2c: &mut I) -> Option<[f32; 3]> {
    let addr = ADDR.load(Ordering::Relaxed);
    let mut b = [0u8; 6];
    i2c.write_read(addr, &[0x12], &mut b).ok()?;
    const S: f32 = 2000.0 / 32768.0;
    Some([
        i16::from_le_bytes([b[0], b[1]]) as f32 * S,
        i16::from_le_bytes([b[2], b[3]]) as f32 * S,
        i16::from_le_bytes([b[4], b[5]]) as f32 * S,
    ])
}

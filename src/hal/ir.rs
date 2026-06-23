//! Onboard IR transmitter (GPIO44) on RMT channel 1.
//!
//! The Cardputer has an IR LED but NO receiver, so this is transmit-only: it sends
//! NEC-protocol frames (the most common consumer-IR format) under a ~38 kHz carrier.
//! You drive it with a 32-bit NEC code — the value any online NEC remote database
//! lists for your device — and a couple of well-known TV-power codes ship as presets.
//!
//! RMT setup (done in main): channel 1, clk_divider 80 so one pulse tick = 1 us, with
//! carrier modulation on. The NEC timings below are therefore written directly in
//! microseconds. CARRIER_* assume the carrier counts in that same 1 MHz channel clock.

use esp_hal::gpio::Level;
use esp_hal::rmt::{Channel, PulseCode, Tx, TxChannelConfig};
use esp_hal::Blocking;

/// Channel clock divider: 80 MHz / 80 = 1 MHz, so one pulse tick = 1 us (NEC timings
/// below are in microseconds and fit a u16).
pub const CLK_DIV: u8 = 80;

/// 38 kHz carrier as (high, low) ticks of the 1 MHz channel clock (~26 us period).
/// TUNE HERE if a device won't respond: some silicon counts the carrier in the
/// undivided 80 MHz clock instead — then use ~1053 / 1052. 13/13 assumes 1 MHz.
const CARRIER_HIGH: u16 = 13;
const CARRIER_LOW: u16 = 13;

// NEC protocol timings, in microseconds.
const LEAD_MARK: u16 = 9000;
const LEAD_SPACE: u16 = 4500;
const BIT_MARK: u16 = 560;
const ZERO_SPACE: u16 = 560;
const ONE_SPACE: u16 = 1690;

/// A configured IR TX channel (RMT channel 1, carrier on). Built in main().
pub type IrChannel = Channel<'static, Blocking, Tx>;

/// RMT TX config for the IR channel: 1 us ticks + 38 kHz carrier on the mark phase.
pub fn tx_config() -> TxChannelConfig {
    TxChannelConfig::default()
        .with_clk_divider(CLK_DIV)
        .with_carrier_modulation(true)
        .with_carrier_high(CARRIER_HIGH)
        .with_carrier_low(CARRIER_LOW)
        .with_carrier_level(Level::High)
}

/// Build a NEC frame: lead burst + 32 data bits (MSB first) + a stop mark + the end
/// marker = 35 pulse codes. Each data bit is a 560 us mark then a 560 us (0) or
/// 1690 us (1) space; the carrier rides the High (mark) phases.
fn nec_frame(code: u32) -> [PulseCode; 35] {
    let mut f = [PulseCode::end_marker(); 35];
    f[0] = PulseCode::new(Level::High, LEAD_MARK, Level::Low, LEAD_SPACE);
    for bit in 0..32usize {
        let one = (code >> (31 - bit)) & 1 != 0; // MSB first
        let space = if one { ONE_SPACE } else { ZERO_SPACE };
        f[1 + bit] = PulseCode::new(Level::High, BIT_MARK, Level::Low, space);
    }
    f[33] = PulseCode::new(Level::High, BIT_MARK, Level::Low, 0); // stop mark
    f // f[34] stays the end marker
}

/// Well-known published 32-bit NEC TV-power codes (name, code), sent MSB-first. These
/// are the famous LIRC/IRremote values; add anything else via the app's Custom entry.
pub const PRESETS: [(&str, u32); 3] = [
    ("Samsung TV power", 0xE0E0_40BF),
    ("LG TV power", 0x20DF_10EF),
    ("NEC TV power", 0x807F_02FD),
];

/// Owns the IR RMT channel; sends NEC codes. The channel is consumed by `transmit`
/// and handed back by `wait`, so it lives in an Option across each send.
pub struct IrTx {
    chan: Option<IrChannel>,
}

impl IrTx {
    pub fn new(chan: IrChannel) -> Self {
        IrTx { chan: Some(chan) }
    }

    /// Transmit one NEC 32-bit code (blocking; a frame is ~67 ms).
    pub fn send_nec(&mut self, code: u32) {
        let frame = nec_frame(code);
        if let Some(ch) = self.chan.take() {
            let ch = match ch.transmit(&frame) {
                Ok(t) => t.wait().unwrap_or_else(|(_, c)| c),
                Err((_, c)) => c,
            };
            self.chan = Some(ch);
        }
    }
}

//! Onboard IR transmitter (GPIO44) on RMT channel 1 — hardware carrier + envelope.
//!
//! Speaks NEC (Samsung/LG etc.), RC5 + RC6 (Philips / Arcelik / Beko / Grundig), and Sony SIRC.
//! The headline is a single "blast all" power button that fires every brand's power code.
//!
//! GOTCHA (cost us the whole bring-up): GPIO44 is the ESP32-S3 ROM UART0-RX (U0RXD) console pin.
//! Its RTC **digital pad-hold** can be left latched from boot, which FREEZES the pad so NO output
//! (RMT/LEDC/GPIO) ever reaches it even though every config register looks correct (RMT even
//! returns transmit-OK). esp-hal never clears a digital pad-hold, so main() does it explicitly
//! (RTC_CNTL dig_pad_hold = 0 + force-unhold) BEFORE this channel is built. With that cleared,
//! the RMT hardware carrier drives the LED fine.
//!
//! RMT: channel 1, clk_divider 80 -> 1 us duration ticks, 38 kHz carrier. NOTE the carrier counts
//! in the UNDIVIDED ~80 MHz RMT group clock (not the divided channel clock), so the carrier
//! high/low are ~80x the duration ticks: 80e6/(1053+1052) = ~38.0 kHz. 38 kHz suits NEC exactly
//! and is what real remotes use for RC5 too (a captured Grundig RC5 frame is 38 kHz); close enough
//! for RC5/RC6 (36 kHz) and Sony (40 kHz) receivers at handheld range.

use esp_hal::gpio::Level;
use esp_hal::rmt::{Channel, PulseCode, Tx, TxChannelConfig};
use esp_hal::time::{Duration, Instant};
use esp_hal::Blocking;

/// Channel clock divider: 80 MHz / 80 = 1 MHz, so one *duration* tick = 1 us.
pub const CLK_DIV: u8 = 80;

/// 38 kHz carrier (high, low) ticks of the UNDIVIDED ~80 MHz RMT group clock, 50% duty.
const CARRIER_HIGH: u16 = 1053;
const CARRIER_LOW: u16 = 1052;

/// The IR line protocol a code is transmitted with.
#[derive(Clone, Copy, PartialEq)]
pub enum Protocol {
    /// NEC: 32-bit value, MSB-first.
    Nec,
    /// Philips RC5: 14-bit biphase. Code = `(address << 8) | command` (addr 5-bit, cmd 6-bit).
    Rc5,
    /// Philips RC6 mode 0. Code = `(address << 8) | command` (both 8-bit).
    Rc6,
    /// Sony SIRC 12-bit. Code = `(address << 8) | command` (addr 5-bit, cmd 7-bit).
    Sony,
}

// NEC protocol timings, in microseconds.
const NEC_LEAD_MARK: u16 = 9000;
const NEC_LEAD_SPACE: u16 = 4500;
const NEC_BIT_MARK: u16 = 560;
const NEC_ZERO_SPACE: u16 = 560;
const NEC_ONE_SPACE: u16 = 1690;

/// RC5 half-bit (us); a full bit is two of these (1778 us).
const RC5_HALF: u16 = 889;

// RC6 mode-0 timings (us).
const RC6_LEAD_MARK: u16 = 2666;
const RC6_LEAD_SPACE: u16 = 889;
const RC6_HALF: u16 = 222;
const RC6_TOGGLE_HALF: u16 = 444;

// Sony SIRC timings (us).
const SONY_START_MARK: u16 = 2400;
const SONY_GAP: u16 = 600;
const SONY_ONE_MARK: u16 = 1200;

/// Gap between repeated frames of the same code (a real remote repeats a held key; RC5/RC6
/// receivers often need 2+ frames to accept a command).
const REPEAT_GAP_MS: u64 = 40;
const REPEATS: u8 = 3;

/// Max (mark, space) pairs in any frame (NEC = 34; RC6 ~ 22).
pub const MAX_PAIRS: usize = 40;

/// Power-toggle codes blasted by the "all TVs" button — one per major protocol/brand.
pub const POWER_CODES: [(Protocol, u32); 7] = [
    (Protocol::Rc5, 0x0C),        // Philips / Arcelik / Grundig / many EU TVs (RC5 sys0 power)
    (Protocol::Rc5, 0x20),        // Beko-chassis power (RC5 sys0 alt)
    (Protocol::Rc6, 0x0C),        // Philips & newer smart/Android TVs (RC6 mode0, addr0)
    (Protocol::Nec, 0xE0E0_40BF), // Samsung
    (Protocol::Nec, 0x20DF_10EF), // LG
    (Protocol::Nec, 0x807F_02FD), // generic NEC TVs
    (Protocol::Sony, 0x0115),     // Sony (SIRC: addr 1, cmd 0x15)
];

/// A configured IR TX channel (RMT channel 1, carrier on). Built in main().
pub type IrChannel = Channel<'static, Blocking, Tx>;

/// RMT TX config for the IR channel: 1 us duration ticks + a 38 kHz carrier on the mark, and
/// the idle line actively held low (active-high direct-drive LED).
pub fn tx_config() -> TxChannelConfig {
    TxChannelConfig::default()
        .with_clk_divider(CLK_DIV)
        .with_carrier_modulation(true)
        .with_carrier_high(CARRIER_HIGH)
        .with_carrier_low(CARRIER_LOW)
        .with_carrier_level(Level::High)
        .with_idle_output(true)
        .with_idle_output_level(Level::Low)
}

/// Owns the IR RMT channel; sends NEC / RC5 / RC6 / Sony codes. The channel is consumed by
/// `transmit` and handed back by `wait`, so it lives in an Option across each send.
pub struct IrTx {
    chan: Option<IrChannel>,
    toggle: bool, // RC5/RC6 toggle bit — flipped per press so a TV reads each tap as a new key
}

impl IrTx {
    pub fn new(chan: IrChannel) -> Self {
        IrTx { chan: Some(chan), toggle: false }
    }

    /// Blast every power code in [`POWER_CODES`] (each repeated like a held key). One fresh
    /// toggle for the whole blast. ~1.5 s.
    pub fn blast_power(&mut self) {
        self.toggle = !self.toggle;
        for &(proto, code) in POWER_CODES.iter() {
            self.send_burst(proto, code);
        }
    }

    /// Send a single code as one fresh key press (new toggle, repeated). Used by the custom row.
    pub fn tap(&mut self, proto: Protocol, code: u32) {
        self.toggle = !self.toggle;
        self.send_burst(proto, code);
    }

    fn send_burst(&mut self, proto: Protocol, code: u32) {
        for k in 0..REPEATS {
            self.send_one(proto, code);
            if k + 1 < REPEATS {
                delay_ms(REPEAT_GAP_MS);
            }
        }
    }

    /// Render the protocol frame to RMT pulse codes and transmit it.
    fn send_one(&mut self, proto: Protocol, code: u32) {
        let mut pairs = [(0u16, 0u16); MAX_PAIRS];
        let n = frame_pairs(proto, code, self.toggle, &mut pairs);
        // Each (mark, space) us pair -> one PulseCode (carrier rides the High/mark). A final
        // (mark, 0) pair doubles as the stop+end marker (length2 == 0); the trailing end_marker
        // covers frames that end on a space.
        let mut frame = [PulseCode::end_marker(); MAX_PAIRS + 1];
        for i in 0..n {
            frame[i] = PulseCode::new(Level::High, pairs[i].0, Level::Low, pairs[i].1);
        }
        self.transmit(&frame[..n + 1]);
    }

    fn transmit(&mut self, frame: &[PulseCode]) {
        if let Some(ch) = self.chan.take() {
            let ch = match ch.transmit(frame) {
                Ok(t) => t.wait().unwrap_or_else(|(_, c)| c),
                Err((_, c)) => c,
            };
            self.chan = Some(ch);
        }
    }
}

/// Busy-wait `ms` milliseconds (the IR send path is blocking anyway).
fn delay_ms(ms: u64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {}
}

/// Build the (mark_us, space_us) pairs for any protocol/code into `out`, returning the count.
pub fn frame_pairs(proto: Protocol, code: u32, toggle: bool, out: &mut [(u16, u16)]) -> usize {
    match proto {
        Protocol::Nec => nec_pairs(code, out),
        Protocol::Rc5 => rc5_pairs(((code >> 8) & 0x1F) as u8, (code & 0x3F) as u8, toggle, out),
        Protocol::Rc6 => rc6_pairs(((code >> 8) & 0xFF) as u8, (code & 0xFF) as u8, toggle, out),
        Protocol::Sony => sony_pairs((code & 0x7F) as u8, ((code >> 8) & 0x1F) as u8, out),
    }
}

/// NEC: lead + 32 data bits (MSB-first) + stop mark. 34 (mark, space) pairs.
fn nec_pairs(code: u32, out: &mut [(u16, u16)]) -> usize {
    out[0] = (NEC_LEAD_MARK, NEC_LEAD_SPACE);
    for bit in 0..32usize {
        let one = (code >> (31 - bit)) & 1 != 0;
        out[1 + bit] = (NEC_BIT_MARK, if one { NEC_ONE_SPACE } else { NEC_ZERO_SPACE });
    }
    out[33] = (NEC_BIT_MARK, 0); // stop mark
    34
}

/// Fold a half-bit level list (`true` = mark) into (mark_us, space_us) pairs: run-length merge
/// equal levels, drop the leading space, pair each mark-run with the following space-run.
/// `start` is the first free index in `out` (after a leader, if any). Returns the new count.
fn fold_manchester(levels: &[bool], durs: &[u16], out: &mut [(u16, u16)], start: usize) -> usize {
    let h = levels.len();
    let mut n = start;
    let mut i = 0usize;
    while i < h && !levels[i] {
        i += 1; // drop the opening space
    }
    while i < h {
        let mut mark = 0u16;
        while i < h && levels[i] {
            mark += durs[i];
            i += 1;
        }
        let mut space = 0u16;
        while i < h && !levels[i] {
            space += durs[i];
            i += 1;
        }
        if n < out.len() {
            out[n] = (mark, space);
            n += 1;
        }
    }
    n
}

/// RC5: 14 bits MSB-first (S1=1, S2=1, toggle, 5-bit addr, 6-bit cmd), 889 us half-bits.
/// Logical 1 = space then mark; 0 = mark then space.
fn rc5_pairs(addr: u8, cmd: u8, toggle: bool, out: &mut [(u16, u16)]) -> usize {
    let mut bits = [false; 14];
    bits[0] = true;
    bits[1] = true;
    bits[2] = toggle;
    for i in 0..5 {
        bits[3 + i] = (addr >> (4 - i)) & 1 != 0;
    }
    for i in 0..6 {
        bits[8 + i] = (cmd >> (5 - i)) & 1 != 0;
    }
    let mut levels = [false; 28];
    let durs = [RC5_HALF; 28];
    for (i, &b) in bits.iter().enumerate() {
        levels[2 * i] = !b;
        levels[2 * i + 1] = b;
    }
    fold_manchester(&levels, &durs, out, 0)
}

/// RC6 mode 0: leader (2666/889) + 21 bits MSB-first (start=1, 3 mode bits=0, double-width
/// toggle, 8-bit addr, 8-bit cmd), 222 us half-bits (toggle 444). Logical 1 = mark then space;
/// 0 = space then mark (opposite of RC5).
fn rc6_pairs(addr: u8, cmd: u8, toggle: bool, out: &mut [(u16, u16)]) -> usize {
    let mut bits: [(bool, bool); 21] = [(false, false); 21];
    bits[0] = (true, false);
    bits[4] = (toggle, true);
    for i in 0..8 {
        bits[5 + i] = ((addr >> (7 - i)) & 1 != 0, false);
    }
    for i in 0..8 {
        bits[13 + i] = ((cmd >> (7 - i)) & 1 != 0, false);
    }
    let mut levels = [false; 42];
    let mut durs = [0u16; 42];
    let mut h = 0usize;
    for &(val, dbl) in bits.iter() {
        let d = if dbl { RC6_TOGGLE_HALF } else { RC6_HALF };
        levels[h] = val;
        durs[h] = d;
        h += 1;
        levels[h] = !val;
        durs[h] = d;
        h += 1;
    }
    out[0] = (RC6_LEAD_MARK, RC6_LEAD_SPACE);
    fold_manchester(&levels[..h], &durs[..h], out, 1)
}

/// Sony SIRC 12-bit: start (2400/600) + 7 cmd bits + 5 addr bits, LSB-first, pulse-width coded
/// (1 = 1200 us mark, 0 = 600 us mark; each followed by a 600 us space).
fn sony_pairs(cmd: u8, addr: u8, out: &mut [(u16, u16)]) -> usize {
    out[0] = (SONY_START_MARK, SONY_GAP);
    let mut n = 1;
    for i in 0..7 {
        let mark = if (cmd >> i) & 1 != 0 { SONY_ONE_MARK } else { SONY_GAP };
        out[n] = (mark, SONY_GAP);
        n += 1;
    }
    for i in 0..5 {
        let mark = if (addr >> i) & 1 != 0 { SONY_ONE_MARK } else { SONY_GAP };
        out[n] = (mark, SONY_GAP);
        n += 1;
    }
    n
}

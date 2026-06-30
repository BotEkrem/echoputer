//! WPA/WPA2-PSK offline cracking primitives (SHA1 + HMAC-SHA1 + PBKDF2).
//!
//! The "independent" path: once a 4-way handshake (or PMKID) is captured, candidate
//! passphrases can be tested OFFLINE — no per-try association — so a large built-in
//! wordlist becomes practical on-device. The PSK/PMK is
//! `PBKDF2-HMAC-SHA1(passphrase, ssid, 4096, 32)`; from there the PTK + MIC verify a
//! candidate against the captured handshake (added with the capture step).
//!
//! Pure (no deps beyond alloc); verified by `networktest` against RFC 6070 +
//! the 802.11i PMK test vector.
//!
//! Some crypto primitives are `pub` building blocks exercised mainly by `networktest`;
//! the few that aren't reachable in a plain build keep a local `allow(dead_code)`.
#![cfg_attr(not(feature = "networktest"), allow(dead_code))]

use alloc::string::String;
use alloc::vec::Vec;

// --------------------------------- SHA1 ------------------------------------

/// SHA-1 digest (20 bytes). Small inputs only (creds/handshake material).
pub fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    let mut data: Vec<u8> = Vec::with_capacity(msg.len() + 72);
    data.extend_from_slice(msg);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_be_bytes()); // SHA-1 length is big-endian

    for chunk in data.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = if i < 20 {
                ((b & c) | ((!b) & d), 0x5A827999u32)
            } else if i < 40 {
                (b ^ c ^ d, 0x6ED9EBA1)
            } else if i < 60 {
                ((b & c) | (b & d) | (c & d), 0x8F1BBCDC)
            } else {
                (b ^ c ^ d, 0xCA62C1D6)
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

/// HMAC-SHA1 (20-byte tag).
pub fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&sha1(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Vec::with_capacity(64 + msg.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(msg);
    let ih = sha1(&inner);
    let mut outer = Vec::with_capacity(84);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&ih);
    sha1(&outer)
}

/// PBKDF2-HMAC-SHA1 into `dk` (any length up to a few blocks). For WPA:
/// `dk` is 32 bytes, `salt` is the SSID, `c` is 4096.
pub fn pbkdf2_sha1(pass: &[u8], salt: &[u8], c: u32, dk: &mut [u8]) {
    let mut block = 1u32;
    let mut off = 0usize;
    while off < dk.len() {
        let mut s = Vec::with_capacity(salt.len() + 4);
        s.extend_from_slice(salt);
        s.extend_from_slice(&block.to_be_bytes());
        let mut u = hmac_sha1(pass, &s);
        let mut t = u;
        for _ in 1..c {
            u = hmac_sha1(pass, &u);
            for i in 0..20 {
                t[i] ^= u[i];
            }
        }
        let n = core::cmp::min(20, dk.len() - off);
        dk[off..off + n].copy_from_slice(&t[..n]);
        off += n;
        block += 1;
    }
}

/// WPA/WPA2 PMK = PBKDF2-HMAC-SHA1(passphrase, ssid, 4096, 32).
pub fn wpa_pmk(passphrase: &str, ssid: &str) -> [u8; 32] {
    let mut pmk = [0u8; 32];
    pbkdf2_sha1(passphrase.as_bytes(), ssid.as_bytes(), 4096, &mut pmk);
    pmk
}

// ------------------------- handshake / candidate check ----------------------

/// A captured WPA 4-way handshake — everything needed to test passphrases
/// offline. `eapol` is the msg2 EAPOL-Key frame with its MIC field ZEROED (the
/// capture step does that); `mic` is the captured MIC to match.
pub struct Handshake {
    pub ssid: [u8; 32],
    pub ssid_len: usize,
    pub ap_mac: [u8; 6],
    pub cli_mac: [u8; 6],
    pub anonce: [u8; 32],
    pub snonce: [u8; 32],
    pub eapol: [u8; 256],
    pub eapol_len: usize,
    pub mic: [u8; 16],
    pub key_ver: u8, // 2 = WPA2/CCMP (HMAC-SHA1 MIC); 1 = WPA/TKIP (HMAC-MD5, TODO)
    /// When `Some`, this is a clientless PMKID capture: crack via the PMK-Name HMAC
    /// instead of the 4-way MIC (anonce/snonce/eapol/mic are then unused).
    pub pmkid: Option<[u8; 16]>,
}
impl Handshake {
    pub fn new() -> Self {
        Self {
            ssid: [0; 32],
            ssid_len: 0,
            ap_mac: [0; 6],
            cli_mac: [0; 6],
            anonce: [0; 32],
            snonce: [0; 32],
            eapol: [0; 256],
            eapol_len: 0,
            mic: [0; 16],
            key_ver: 2,
            pmkid: None,
        }
    }
    pub fn ssid_str(&self) -> &str {
        core::str::from_utf8(&self.ssid[..self.ssid_len]).unwrap_or("")
    }
}

/// PRF-512: out = HMAC-SHA1(key, label || 0x00 || data || i) over i = 0,1,2,...
fn prf512(key: &[u8], label: &[u8], data: &[u8], out: &mut [u8; 64]) {
    let mut off = 0usize;
    let mut i = 0u8;
    while off < 64 {
        let mut buf = Vec::with_capacity(label.len() + data.len() + 2);
        buf.extend_from_slice(label);
        buf.push(0x00);
        buf.extend_from_slice(data);
        buf.push(i);
        let h = hmac_sha1(key, &buf);
        let n = core::cmp::min(20, 64 - off);
        out[off..off + n].copy_from_slice(&h[..n]);
        off += n;
        i += 1;
    }
}

/// Does `pass` produce the captured MIC? PMK -> PTK (PRF) -> KCK=PTK[0..16] ->
/// MIC = HMAC-SHA1(KCK, eapol)[0..16], compared to the captured MIC. WPA2 only.
pub fn check_passphrase(hs: &Handshake, pass: &str) -> bool {
    // PMKID path (clientless): PMKID = HMAC-SHA1(PMK, "PMK Name" || AP || STA)[0..16].
    if let Some(want) = hs.pmkid {
        let pmk = wpa_pmk(pass, hs.ssid_str());
        let mut m = Vec::with_capacity(20);
        m.extend_from_slice(b"PMK Name");
        m.extend_from_slice(&hs.ap_mac);
        m.extend_from_slice(&hs.cli_mac);
        return hmac_sha1(&pmk, &m)[..16] == want;
    }
    if hs.key_ver != 2 {
        return false; // WPA1/TKIP (HMAC-MD5) not handled yet
    }
    let pmk = wpa_pmk(pass, hs.ssid_str());
    // B = min(AA,SA)||max(AA,SA)||min(ANonce,SNonce)||max(ANonce,SNonce)
    let mut b = Vec::with_capacity(76);
    let (m1, m2) = if hs.ap_mac <= hs.cli_mac { (&hs.ap_mac, &hs.cli_mac) } else { (&hs.cli_mac, &hs.ap_mac) };
    b.extend_from_slice(m1);
    b.extend_from_slice(m2);
    let (n1, n2) = if hs.anonce <= hs.snonce { (&hs.anonce, &hs.snonce) } else { (&hs.snonce, &hs.anonce) };
    b.extend_from_slice(n1);
    b.extend_from_slice(n2);
    let mut ptk = [0u8; 64];
    prf512(&pmk, b"Pairwise key expansion", &b, &mut ptk);
    let mic = hmac_sha1(&ptk[..16], &hs.eapol[..hs.eapol_len]); // KCK = first 16 bytes
    mic[..16] == hs.mic
}

/// Stream candidate passphrases from a newline-separated wordlist buffer (e.g. an
/// SD `wifi_pass.txt` read into memory), testing each against `hs`. Only 8..=63-byte
/// lines (the WPA2 passphrase range) are tried. Returns the first match. `tick(i)`
/// reports progress and aborts when it returns false. Heap-light: borrows `bytes`,
/// only the winning line is copied out.
pub fn crack_bytes(
    hs: &Handshake,
    bytes: &[u8],
    mut tick: impl FnMut(u32) -> bool,
) -> Option<alloc::string::String> {
    let mut i = 0u32;
    for line in bytes.split(|&b| b == b'\n' || b == b'\r') {
        if line.len() < 8 || line.len() > 63 {
            continue;
        }
        i = i.wrapping_add(1);
        if !tick(i) {
            break;
        }
        if let Ok(s) = core::str::from_utf8(line) {
            if check_passphrase(hs, s) {
                return Some(s.into());
            }
        }
    }
    None
}

/// Render the handshake as a hashcat **WPA mode 22000** EAPOL line:
/// `WPA*02*MIC*AP_MAC*STA_MAC*ESSID*ANONCE*EAPOL*MESSAGEPAIR`. This is the offload
/// export — drop it on the SD, then crack on a PC with `hashcat -m 22000` (or feed a
/// crack server). EAPOL is the msg2 802.1X frame with its MIC field already zeroed,
/// exactly what `check_passphrase` consumes.
pub fn to_hc22000(hs: &Handshake) -> String {
    let mut s = String::new();
    // PMKID line (type 01): WPA*01*PMKID*AP_MAC*STA_MAC*ESSID*** (no nonce/eapol/mp).
    if let Some(pmkid) = hs.pmkid {
        s.push_str("WPA*01*");
        hex_push(&mut s, &pmkid);
        s.push('*');
        hex_push(&mut s, &hs.ap_mac);
        s.push('*');
        hex_push(&mut s, &hs.cli_mac);
        s.push('*');
        hex_push(&mut s, &hs.ssid[..hs.ssid_len]);
        s.push_str("***");
        return s;
    }
    s.push_str("WPA*02*");
    hex_push(&mut s, &hs.mic);
    s.push('*');
    hex_push(&mut s, &hs.ap_mac);
    s.push('*');
    hex_push(&mut s, &hs.cli_mac);
    s.push('*');
    hex_push(&mut s, &hs.ssid[..hs.ssid_len]);
    s.push('*');
    hex_push(&mut s, &hs.anonce);
    s.push('*');
    hex_push(&mut s, &hs.eapol[..hs.eapol_len]);
    // messagepair 00 = "M1+M2, EAPOL from M2" — correct for hashcat -m 22000 in BOTH the
    // passive case and the evil-twin case (there `anonce` is our injected EVIL_ANONCE, which
    // legitimately plays M1's nonce). The byte does not encode capture provenance.
    s.push_str("*00");
    s
}

fn hex_push(s: &mut String, b: &[u8]) {
    const H: &[u8; 16] = b"0123456789abcdef";
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 0x0f) as usize] as char);
    }
}

// --------------------------- 802.11 / EAPOL parse ---------------------------

/// One parsed EAPOL-Key frame lifted out of a raw promiscuous 802.11 capture.
/// `frame` is the 802.1X authentication frame (the version byte through the end
/// of Key Data) — exactly what the MIC is computed over once its MIC field is
/// zeroed. Borrows the captured packet; the caller copies out what it needs.
pub struct EapolKey<'a> {
    pub msg: u8,         // 4-way message number 1..=4 (0 = not a usable pairwise msg)
    pub key_ver: u8,     // Key Descriptor Version (2 = HMAC-SHA1 / CCMP)
    pub nonce: [u8; 32], // ANonce (msg 1/3) or SNonce (msg 2/4)
    pub mic: [u8; 16],
    pub replay: u64,    // Key Replay Counter (msg2 echoes the msg1 it answers)
    pub frame: &'a [u8],
    pub sta: [u8; 6],   // the non-AP station
    pub bssid: [u8; 6], // the access point
    /// RSN PMKID lifted from msg1's Key Data (clientless crack material), if present.
    pub pmkid: Option<[u8; 16]>,
}

/// Parse a raw 802.11 frame (as delivered by the promiscuous sniffer) into its
/// EAPOL-Key fields, or `None` if it is not a WPA pairwise 4-way handshake frame.
/// Tolerates QoS / variable-length MAC headers by scanning for the LLC/SNAP shim.
///
/// EAPOL-Key body layout (offsets from the body start, IEEE 802.11i):
/// `0` descriptor type, `1` key info (2), `3` key len (2), `5` replay (8),
/// `13` key nonce (32), `45` IV (16), `61` RSC (8), `69` reserved (8),
/// `77` key MIC (16), `93` key data len (2), `95` key data.
pub fn parse_eapol(d: &[u8]) -> Option<EapolKey<'_>> {
    // must be a Data-type frame (FC type bits = 0b10)
    if d.len() < 36 || (d[0] & 0x0C) != 0x08 {
        return None;
    }
    // locate the LLC/SNAP header (AA AA 03 ..) + EAPOL ethertype (88 8E). The MAC
    // header is 24 bytes (26 with QoS, +6 with addr4), so scan a short window.
    let scan_end = core::cmp::min(d.len().saturating_sub(8), 40);
    let mut i = 24usize;
    let llc = loop {
        if i > scan_end {
            return None;
        }
        if d[i] == 0xAA && d[i + 1] == 0xAA && d[i + 2] == 0x03 && d[i + 6] == 0x88 && d[i + 7] == 0x8E {
            break i;
        }
        i += 1;
    };
    let p = llc + 8; // start of the 802.1X header (version, type, length[2])
    if p + 4 > d.len() || d[p + 1] != 0x03 {
        return None; // 802.1X type 3 = EAPOL-Key
    }
    let bs = p + 4; // EAPOL-Key body
    if bs + 95 > d.len() {
        return None; // need at least through the Key Data Length field
    }
    let key_info = ((d[bs + 1] as u16) << 8) | d[bs + 2] as u16;
    if key_info & 0x0008 == 0 {
        return None; // not a Pairwise key (group rekey etc.)
    }
    let ack = key_info & 0x0080 != 0;
    let has_mic = key_info & 0x0100 != 0;
    let secure = key_info & 0x0200 != 0;
    let msg = match (ack, has_mic, secure) {
        (true, false, _) => 1,
        (false, true, false) => 2,
        (true, true, _) => 3,
        (false, true, true) => 4,
        _ => 0,
    };
    let mut nonce = [0u8; 32];
    nonce.copy_from_slice(&d[bs + 13..bs + 45]);
    let mut mic = [0u8; 16];
    mic.copy_from_slice(&d[bs + 77..bs + 93]);
    let replay = u64::from_be_bytes(d[bs + 5..bs + 13].try_into().unwrap());
    // the full 802.1X frame = 4-byte header + the body length it declares. Floor at
    // 95 so the returned frame always reaches the MIC field (offset 81..97) — a
    // truncated/forged frame must not be accepted with an un-zeroed MIC.
    let body_len = ((d[p + 2] as usize) << 8) | d[p + 3] as usize;
    if body_len < 95 {
        return None;
    }
    let frame_end = core::cmp::min(p + 4 + body_len, d.len());
    let frame = &d[p..frame_end];
    // msg1 may carry an RSN PMKID KDE in its (unencrypted) Key Data — a clientless
    // crack handle (no msg2 needed). Key Data Length @ bs+93, Key Data @ bs+95.
    let kd_len = ((d[bs + 93] as usize) << 8) | d[bs + 94] as usize;
    // only msg1 with UNENCRYPTED Key Data (key-info bit 0x1000 clear) carries a
    // readable PMKID KDE; a real clientless-PMKID AP always sends it in the clear.
    let enc_key_data = key_info & 0x1000 != 0;
    let pmkid = if msg == 1 && !enc_key_data && kd_len >= 20 {
        let kd_start = bs + 95;
        let kd_end = core::cmp::min(kd_start + kd_len, d.len());
        find_pmkid(&d[kd_start..kd_end])
    } else {
        None
    };
    // STA / AP address depends on the ToDS/FromDS direction bits
    let (tods, fromds) = (d[1] & 0x01 != 0, d[1] & 0x02 != 0);
    let (bssid, sta) = if tods && !fromds {
        (slice6(d, 4), slice6(d, 10)) // STA -> AP: a1 = BSSID, a2 = STA
    } else if !tods && fromds {
        (slice6(d, 10), slice6(d, 4)) // AP -> STA: a1 = STA, a2 = BSSID
    } else {
        (slice6(d, 16), slice6(d, 10)) // fallback: a3 = BSSID
    };
    Some(EapolKey { msg, key_ver: (key_info & 0x0007) as u8, nonce, mic, replay, frame, sta, bssid, pmkid })
}

/// Walk EAPOL Key-Data KDEs for the RSN PMKID KDE
/// (`0xDD <len> 00-0F-AC 04 <16-byte PMKID>`) and return the PMKID. A KDE with an
/// all-zero PMKID is treated as absent (some APs pad one in without a real value).
fn find_pmkid(kd: &[u8]) -> Option<[u8; 16]> {
    let mut i = 0usize;
    while i + 2 <= kd.len() {
        let id = kd[i];
        let len = kd[i + 1] as usize;
        if i + 2 + len > kd.len() {
            break;
        }
        let body = &kd[i + 2..i + 2 + len];
        if id == 0xDD && body.len() >= 20 && body[0] == 0x00 && body[1] == 0x0F && body[2] == 0xAC && body[3] == 0x04 {
            let mut p = [0u8; 16];
            p.copy_from_slice(&body[4..20]);
            if p != [0u8; 16] {
                return Some(p);
            }
        }
        i += 2 + len;
    }
    None
}

#[inline]
fn slice6(d: &[u8], off: usize) -> [u8; 6] {
    let mut a = [0u8; 6];
    a.copy_from_slice(&d[off..off + 6]);
    a
}

// -------------------------------- self-test --------------------------------

#[cfg(feature = "networktest")]
fn hex_into(bytes: &[u8], out: &mut [u8]) {
    const H: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in bytes.iter().enumerate() {
        if i * 2 + 1 < out.len() {
            out[i * 2] = H[(b >> 4) as usize];
            out[i * 2 + 1] = H[(b & 0x0f) as usize];
        }
    }
}

/// Verify SHA1 / HMAC-SHA1 / PBKDF2 / WPA-PMK against published test vectors
/// (run by `networktest`, no network). PBKDF2 is RFC 6070; PMK is the 802.11i vector.
#[cfg(feature = "networktest")]
pub fn networktest() {
    use esp_println::println;
    println!("[*] WPA crypto: SHA1 + HMAC + PBKDF2 (no network)...");
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut chk = |label: &str, got: &[u8], want: &str| {
        let mut hx = [0u8; 64];
        hex_into(got, &mut hx);
        let g = core::str::from_utf8(&hx[..got.len() * 2]).unwrap_or("");
        if g == want {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL {label}: got {g} want {want}");
        }
    };

    // SHA-1 known answers
    chk("sha1(abc)", &sha1(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    chk("sha1()", &sha1(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    // HMAC-SHA1 RFC 2202 (key=0x0b*20, "Hi There")
    chk(
        "hmac(HiThere)",
        &hmac_sha1(&[0x0b; 20], b"Hi There"),
        "b617318655057264e28bc0b6fb378c8ef146be00",
    );
    // PBKDF2-HMAC-SHA1 RFC 6070
    let mut dk = [0u8; 20];
    pbkdf2_sha1(b"password", b"salt", 1, &mut dk);
    chk("pbkdf2 c=1", &dk, "0c60c80f961f0e71f3a9b524af6012062fe037a6");
    pbkdf2_sha1(b"password", b"salt", 4096, &mut dk);
    chk("pbkdf2 c=4096", &dk, "4b007901b765489abead49d926f721d065a429c1");
    // WPA PMK (IEEE 802.11i Annex H.4): passphrase "password", ssid "IEEE"
    chk(
        "wpa_pmk(password,IEEE)",
        &wpa_pmk("password", "IEEE"),
        "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e",
    );

    // 4-way handshake parse + offline crack. Synthetic + self-consistent: the PMK
    // is anchored to the IEEE vector above; this proves the EAPOL byte offsets,
    // the MIC-field zeroing, and the PTK/MIC -> check_passphrase wiring all line
    // up end-to-end. (PTK/MIC interop vs a real AP is the live on-device test.)
    let (ok_parse, ok_crack, ok_reject) = eapol_roundtrip();
    if ok_parse {
        pass += 1;
    } else {
        fail += 1;
        println!("    FAIL eapol parse (msg/nonce/mac extraction)");
    }
    if ok_crack {
        pass += 1;
    } else {
        fail += 1;
        println!("    FAIL eapol crack (right pass not accepted)");
    }
    if ok_reject {
        pass += 1;
    } else {
        fail += 1;
        println!("    FAIL eapol reject (wrong pass accepted)");
    }

    // wordlist crack: stream candidates from a byte buffer, find the known one.
    {
        let hs = synth_handshake("12345678", "test");
        let wl = b"aaaaaaaa\nshort\n00000000\n12345678\nzzzzzzzzzz\n";
        let found = crack_bytes(&hs, wl, |_| true);
        if found.as_deref() == Some("12345678") {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL wordlist crack: {found:?}");
        }
    }
    // msg1 builder: build a msg1 frame, parse it back, verify it is well-formed.
    if m1_builder_ok() {
        pass += 1;
    } else {
        fail += 1;
        println!("    FAIL m1 builder roundtrip");
    }
    // .22000 export: structure + field placement of the hashcat line.
    if hc22000_ok() {
        pass += 1;
    } else {
        fail += 1;
        println!("    FAIL hc22000 export format");
    }
    // PMKID: parse the KDE from a synthetic msg1, clientless crack, type-01 export.
    if pmkid_ok() {
        pass += 1;
    } else {
        fail += 1;
        println!("    FAIL pmkid (parse/crack/export)");
    }

    println!("    wpa crypto: {pass} pass, {fail} fail");
}

/// PMKID end-to-end self-test: build a synthetic msg1 carrying an RSN PMKID KDE,
/// confirm `parse_eapol` lifts it, that a PMKID `Handshake` cracks to the right
/// passphrase (and rejects a wrong one), and that the type-01 `.22000` export is
/// well formed.
#[cfg(feature = "networktest")]
fn pmkid_ok() -> bool {
    let ap = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    let sta = [0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB];
    let ssid = "test";
    let psk = "12345678";
    // canonical PMKID = HMAC-SHA1(PMK, "PMK Name" || AP || STA)[0..16]
    let pmk = wpa_pmk(psk, ssid);
    let mut m: Vec<u8> = Vec::new();
    m.extend_from_slice(b"PMK Name");
    m.extend_from_slice(&ap);
    m.extend_from_slice(&sta);
    let mut pmkid = [0u8; 16];
    pmkid.copy_from_slice(&hmac_sha1(&pmk, &m)[..16]);

    // (a) parse: a synthetic msg1 carrying the PMKID KDE must yield it back
    let anonce = [0x5au8; 32];
    let m1 = build_m1_pmkid(ap, sta, &anonce, &pmkid);
    let parse_ok = match parse_eapol(&m1) {
        Some(k) => k.msg == 1 && k.pmkid == Some(pmkid) && k.bssid == ap && k.sta == sta,
        None => false,
    };

    // (b) crack: a PMKID handshake accepts the right pass, rejects a wrong one
    let mut hs = Handshake::new();
    let sb = ssid.as_bytes();
    hs.ssid[..sb.len()].copy_from_slice(sb);
    hs.ssid_len = sb.len();
    hs.ap_mac = ap;
    hs.cli_mac = sta;
    hs.pmkid = Some(pmkid);
    let crack_ok = check_passphrase(&hs, psk) && !check_passphrase(&hs, "wrongpass");

    // (c) export: WPA*01*pmkid*ap*sta*essid*** (9 fields, last three empty)
    let line = to_hc22000(&hs);
    let parts: Vec<&str> = line.split('*').collect();
    let export_ok = parts.len() == 9
        && parts[0] == "WPA"
        && parts[1] == "01"
        && parts[2].len() == 32
        && parts[3].len() == 12
        && parts[4].len() == 12
        && parts[5] == "74657374"
        && parts[6].is_empty()
        && parts[7].is_empty()
        && parts[8].is_empty();

    parse_ok && crack_ok && export_ok
}

/// Build a synthetic msg1 (`build_eapol`) and append an RSN PMKID KDE
/// (`DD 14 00-0F-AC 04 <16>`) to its Key Data, fixing the 802.1X body length and the
/// Key Data Length fields so `parse_eapol` walks it correctly.
#[cfg(feature = "networktest")]
fn build_m1_pmkid(ap: [u8; 6], sta: [u8; 6], anonce: &[u8; 32], pmkid: &[u8; 16]) -> Vec<u8> {
    let mut f = build_eapol(false, 0x008A, anonce, ap, sta); // msg1, key-data-len 0
    let mut kde = [0u8; 22];
    kde[..6].copy_from_slice(&[0xDD, 0x14, 0x00, 0x0F, 0xAC, 0x04]);
    kde[6..22].copy_from_slice(pmkid);
    // last 2 bytes of the body = Key Data Length (currently 00 00) -> 22, then append.
    let n = f.len();
    f[n - 2] = 0x00;
    f[n - 1] = 22;
    f.extend_from_slice(&kde);
    // 802.1X body length field (MAC 24 + LLC/SNAP 8 + 2) = bytes 34,35: 95 -> 117.
    let body: u16 = 95 + 22;
    f[34] = (body >> 8) as u8;
    f[35] = (body & 0xff) as u8;
    f
}

/// Verify the hashcat 22000 export has the right 9 `*`-separated fields with the
/// expected lengths and the SSID hex, from a known synthetic handshake.
#[cfg(feature = "networktest")]
fn hc22000_ok() -> bool {
    let hs = synth_handshake("12345678", "test");
    let line = to_hc22000(&hs);
    let parts: Vec<&str> = line.split('*').collect();
    parts.len() == 9
        && parts[0] == "WPA"
        && parts[1] == "02"
        && parts[2].len() == 32 // MIC (16 B)
        && parts[3].len() == 12 // AP MAC
        && parts[4].len() == 12 // STA MAC
        && parts[5] == "74657374" // "test"
        && parts[6].len() == 64 // ANonce (32 B)
        && !parts[7].is_empty() // EAPOL
        && parts[8] == "00"
}

/// Build a known-crackable synthetic Handshake (passphrase `psk`, ssid `ssid`) for
/// the wordlist self-test — the msg2 MIC is computed so `check_passphrase`/`crack_bytes`
/// must accept `psk`.
#[cfg(feature = "networktest")]
fn synth_handshake(psk: &str, ssid: &str) -> Handshake {
    let ap = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    let sta = [0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB];
    let anonce = [0x11u8; 32];
    let snonce = [0x22u8; 32];
    let pmk = wpa_pmk(psk, ssid);
    let mut b: Vec<u8> = Vec::new();
    let (lo, hi) = if ap <= sta { (ap, sta) } else { (sta, ap) };
    b.extend_from_slice(&lo);
    b.extend_from_slice(&hi);
    let (nlo, nhi) = if anonce <= snonce { (anonce, snonce) } else { (snonce, anonce) };
    b.extend_from_slice(&nlo);
    b.extend_from_slice(&nhi);
    let mut ptk = [0u8; 64];
    prf512(&pmk, b"Pairwise key expansion", &b, &mut ptk);
    let m2 = build_eapol(true, 0x010A, &snonce, ap, sta);
    let k2 = parse_eapol(&m2).unwrap();
    let p = k2.frame.as_ptr() as usize - m2.as_ptr() as usize;
    let computed = hmac_sha1(&ptk[..16], &m2[p..p + k2.frame.len()]);
    let mut hs = Handshake::new();
    let sb = ssid.as_bytes();
    hs.ssid[..sb.len()].copy_from_slice(sb);
    hs.ssid_len = sb.len();
    hs.ap_mac = ap;
    hs.cli_mac = sta;
    hs.anonce = anonce;
    hs.snonce = snonce;
    hs.mic.copy_from_slice(&computed[..16]);
    hs.key_ver = 2;
    let el = k2.frame.len().min(256);
    hs.eapol[..el].copy_from_slice(&k2.frame[..el]);
    for x in &mut hs.eapol[81..97] {
        *x = 0;
    }
    hs.eapol_len = el;
    hs
}

/// Build a msg1 frame via wifi_frames::eapol_m1, parse it back, and verify it is a
/// well-formed pairwise msg1 with the ANonce/addresses we put in.
#[cfg(feature = "networktest")]
fn m1_builder_ok() -> bool {
    let ap = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    let client = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
    let anonce = [0x5au8; 32];
    let mut buf = [0u8; super::wifi_frames::EAPOL_M1_LEN + 8];
    let n = super::wifi_frames::eapol_m1(&mut buf, client, ap, &anonce, 1000);
    match parse_eapol(&buf[..n]) {
        Some(k) => k.msg == 1 && k.nonce == anonce && k.bssid == ap && k.sta == client && k.replay == 1000,
        None => false,
    }
}

/// Build a synthetic 802.11 EAPOL-Key frame (no key data) for the self-test.
#[cfg(feature = "networktest")]
fn build_eapol(tods: bool, key_info: u16, nonce: &[u8; 32], ap: [u8; 6], sta: [u8; 6]) -> Vec<u8> {
    let mut f: Vec<u8> = Vec::new();
    // 802.11 data header (24 B): FC type=data subtype 0, then ToDS/FromDS flags
    f.push(0x08);
    f.push(if tods { 0x01 } else { 0x02 });
    f.extend_from_slice(&[0, 0]); // duration
    if tods {
        f.extend_from_slice(&ap); // a1 = BSSID
        f.extend_from_slice(&sta); // a2 = STA
        f.extend_from_slice(&ap); // a3
    } else {
        f.extend_from_slice(&sta); // a1 = STA
        f.extend_from_slice(&ap); // a2 = BSSID
        f.extend_from_slice(&ap); // a3
    }
    f.extend_from_slice(&[0, 0]); // sequence control
    // LLC/SNAP + EAPOL ethertype
    f.extend_from_slice(&[0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00, 0x88, 0x8E]);
    // 802.1X header: version 2, type 3 (EAPOL-Key), body length 95 (no key data)
    let body_len: u16 = 95;
    f.push(0x02);
    f.push(0x03);
    f.push((body_len >> 8) as u8);
    f.push((body_len & 0xff) as u8);
    // EAPOL-Key body
    f.push(0x02); // descriptor type (RSN)
    f.push((key_info >> 8) as u8);
    f.push((key_info & 0xff) as u8);
    f.extend_from_slice(&[0x00, 0x10]); // key length 16
    f.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]); // replay counter
    f.extend_from_slice(nonce); // key nonce (32)
    f.extend_from_slice(&[0u8; 16]); // key IV
    f.extend_from_slice(&[0u8; 8]); // key RSC
    f.extend_from_slice(&[0u8; 8]); // reserved
    f.extend_from_slice(&[0u8; 16]); // key MIC (zeroed; signed below by the caller)
    f.extend_from_slice(&[0x00, 0x00]); // key data length 0
    f
}

/// Run the round-trip in BOTH min/max orderings so the lo-first AND hi-first arms
/// of the MAC and nonce sorts in `check_passphrase` both get exercised (a wrong
/// sort would crack the test case it happens to match and silently fail the other).
#[cfg(feature = "networktest")]
fn eapol_roundtrip() -> (bool, bool, bool) {
    let a = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    let b = [0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB];
    let lo = rt_case(a, b, [0x11u8; 32], [0x22u8; 32]); // ap < sta, anonce < snonce
    let hi = rt_case(b, a, [0x44u8; 32], [0x22u8; 32]); // ap > sta, anonce > snonce
    (lo.0 && hi.0, lo.1 && hi.1, lo.2 && hi.2)
}

/// Build msg1 + msg2 for the given addresses/nonces, parse them, sign msg2 with the
/// PTK, assemble a Handshake the way the radio layer will, and crack it. Returns
/// (parse_ok, right_pass_accepted, wrong_pass_rejected).
#[cfg(feature = "networktest")]
fn rt_case(ap: [u8; 6], sta: [u8; 6], anonce: [u8; 32], snonce: [u8; 32]) -> (bool, bool, bool) {
    let ssid = "test";
    let psk = "12345678";
    // msg1 (AP->STA): pairwise + ack, no mic   -> ver2|pairwise|ack = 0x008A
    let m1 = build_eapol(false, 0x008A, &anonce, ap, sta);
    // msg2 (STA->AP): pairwise + mic, no ack   -> ver2|pairwise|mic = 0x010A
    let mut m2 = build_eapol(true, 0x010A, &snonce, ap, sta);

    let (k1, k2) = match (parse_eapol(&m1), parse_eapol(&m2)) {
        (Some(a), Some(b)) => (a, b),
        _ => return (false, false, false),
    };
    let ok_parse = k1.msg == 1
        && k2.msg == 2
        && k1.nonce == anonce
        && k2.nonce == snonce
        && k2.sta == sta
        && k2.bssid == ap;

    // derive KCK and sign the msg2 802.1X frame (MIC field is currently zero)
    let pmk = wpa_pmk(psk, ssid);
    let mut b: Vec<u8> = Vec::new();
    let (lo, hi) = if ap <= sta { (ap, sta) } else { (sta, ap) };
    b.extend_from_slice(&lo);
    b.extend_from_slice(&hi);
    let (nlo, nhi) = if anonce <= snonce { (anonce, snonce) } else { (snonce, anonce) };
    b.extend_from_slice(&nlo);
    b.extend_from_slice(&nhi);
    let mut ptk = [0u8; 64];
    prf512(&pmk, b"Pairwise key expansion", &b, &mut ptk);
    let p = k2.frame.as_ptr() as usize - m2.as_ptr() as usize; // offset of the 802.1X header
    let flen = k2.frame.len();
    let computed = hmac_sha1(&ptk[..16], &m2[p..p + flen]);
    m2[p + 81..p + 97].copy_from_slice(&computed[..16]); // MIC field at frame offset 81

    // assemble the Handshake exactly as the radio capture path does
    let k2b = match parse_eapol(&m2) {
        Some(k) => k,
        None => return (ok_parse, false, false),
    };
    let mut hs = Handshake::new();
    let sb = ssid.as_bytes();
    hs.ssid[..sb.len()].copy_from_slice(sb);
    hs.ssid_len = sb.len();
    hs.ap_mac = ap;
    hs.cli_mac = sta;
    hs.anonce = anonce;
    hs.snonce = snonce;
    hs.mic = k2b.mic;
    hs.key_ver = k2b.key_ver;
    let el = k2b.frame.len().min(256);
    hs.eapol[..el].copy_from_slice(&k2b.frame[..el]);
    for x in &mut hs.eapol[81..97] {
        *x = 0; // zero the MIC field for the verify (capture path does this)
    }
    hs.eapol_len = el;

    let ok_crack = check_passphrase(&hs, psk);
    let ok_reject = !check_passphrase(&hs, "wrongpass");
    (ok_parse, ok_crack, ok_reject)
}

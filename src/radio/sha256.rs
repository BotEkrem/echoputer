//! SHA-256 + HMAC-SHA256 (pure Rust, no_std). Used to authenticate WPA crack-offload
//! requests without ever sending the shared secret on the wire: the device signs the
//! request body with `HMAC-SHA256(psk, body)` and the server verifies the signature.
//!
//! Anchored to NIST (SHA-256) + RFC 4231 (HMAC-SHA256) test vectors in `networktest`.

use alloc::string::String;
use alloc::vec::Vec;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// One-shot SHA-256 of `msg`.
pub fn sha256(msg: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let bitlen = (msg.len() as u64).wrapping_mul(8);
    let mut data: Vec<u8> = Vec::with_capacity(msg.len() + 72);
    data.extend_from_slice(msg);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bitlen.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in data.chunks_exact(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

/// HMAC-SHA256 (RFC 2104) of `msg` under `key`.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&sha256(key));
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
    let ih = sha256(&inner);
    let mut outer = Vec::with_capacity(96);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&ih);
    sha256(&outer)
}

/// `HMAC-SHA256(key, msg)` as a 64-char lowercase hex string — the value sent in the
/// `X-Offload-Sig` header.
pub fn hmac_sha256_hex(key: &str, msg: &[u8]) -> String {
    let mac = hmac_sha256(key.as_bytes(), msg);
    let mut s = String::with_capacity(64);
    const H: &[u8; 16] = b"0123456789abcdef";
    for b in mac {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 0x0f) as usize] as char);
    }
    s
}

/// Verify SHA-256 + HMAC-SHA256 against NIST / RFC 4231 vectors (run by `networktest`).
#[cfg(feature = "networktest")]
pub fn networktest() {
    use esp_println::println;
    println!("[*] SHA-256 + HMAC-SHA256 (no network)...");
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut chk = |label: &str, got: &[u8], want: &str| {
        let mut s = String::new();
        const H: &[u8; 16] = b"0123456789abcdef";
        for &b in got {
            s.push(H[(b >> 4) as usize] as char);
            s.push(H[(b & 0x0f) as usize] as char);
        }
        if s == want {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL {label}: got {s} want {want}");
        }
    };
    // NIST SHA-256 known answers
    chk("sha256(abc)", &sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    chk("sha256()", &sha256(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    // RFC 4231 HMAC-SHA256 test case 1: key = 0x0b * 20, data = "Hi There"
    chk(
        "hmac256(HiThere)",
        &hmac_sha256(&[0x0b; 20], b"Hi There"),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
    );
    // RFC 4231 test case 2: key = "Jefe", data = "what do ya want for nothing?"
    chk(
        "hmac256(Jefe)",
        &hmac_sha256(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
    );
    // long key (>64 B) path: key gets pre-hashed
    chk(
        "hmac256(longkey)",
        &hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First"),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
    );
    println!("    sha256/hmac: {pass} pass, {fail} fail");
}

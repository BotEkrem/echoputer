//! HTTP Digest access authentication (RFC 2617) for the CCTV default-cred ladder.
//!
//! Many cameras (Hikvision, some Dahua/Axis) answer with `WWW-Authenticate:
//! Digest ...` rather than Basic, so a Basic cred can never get past their 401.
//! This module parses the Digest challenge, computes the response (it carries a
//! tiny self-contained MD5), and builds the `Authorization: Digest ...` value.
//!
//! Plain `MD5` / `qop=auth` only (the overwhelmingly common camera case);
//! `MD5-sess` is not handled. Pure functions → unit-tested by `networktest`.

use alloc::vec::Vec;

// --------------------------------- MD5 -------------------------------------

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];
const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// MD5 digest of `msg`. Small inputs only (credentials/URIs), so it pads into a
/// heap buffer and processes 64-byte blocks.
pub fn md5(msg: &[u8]) -> [u8; 16] {
    let mut data: Vec<u8> = Vec::with_capacity(msg.len() + 72);
    data.extend_from_slice(msg);
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in data.chunks_exact(64) {
        let mut m = [0u32; 16];
        for i in 0..16 {
            m[i] = u32::from_le_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | (!b & d), i)
            } else if i < 32 {
                ((d & b) | (!d & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | !d), (7 * i) % 16)
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

const HEX: &[u8; 16] = b"0123456789abcdef";
fn hex_into(bytes: &[u8], out: &mut [u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        if i * 2 + 1 < out.len() {
            out[i * 2] = HEX[(b >> 4) as usize];
            out[i * 2 + 1] = HEX[(b & 0x0f) as usize];
        }
    }
}

/// MD5 of `parts` joined by ':' , hex-encoded (32 lowercase chars) into `out`.
fn md5_hex_join(parts: &[&[u8]], out: &mut [u8; 32]) {
    let mut buf: Vec<u8> = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            buf.push(b':');
        }
        buf.extend_from_slice(p);
    }
    let d = md5(&buf);
    hex_into(&d, out);
}

// ------------------------------ challenge ----------------------------------

/// Parsed `WWW-Authenticate: Digest ...` challenge.
pub struct Challenge {
    pub is_digest: bool,
    pub qop_auth: bool,
    pub realm: [u8; 64],
    pub realm_len: usize,
    pub nonce: [u8; 96],
    pub nonce_len: usize,
    pub opaque: [u8; 64],
    pub opaque_len: usize,
}
impl Challenge {
    fn new() -> Self {
        Self {
            is_digest: false,
            qop_auth: false,
            realm: [0; 64],
            realm_len: 0,
            nonce: [0; 96],
            nonce_len: 0,
            opaque: [0; 64],
            opaque_len: 0,
        }
    }
    pub fn realm_str(&self) -> &str {
        core::str::from_utf8(&self.realm[..self.realm_len]).unwrap_or("")
    }
    pub fn nonce_str(&self) -> &str {
        core::str::from_utf8(&self.nonce[..self.nonce_len]).unwrap_or("")
    }
    pub fn opaque_str(&self) -> &str {
        core::str::from_utf8(&self.opaque[..self.opaque_len]).unwrap_or("")
    }
}

/// Parse a `WWW-Authenticate` value into a [`Challenge`].
pub fn parse_challenge(www_auth: &str) -> Challenge {
    let s = www_auth.as_bytes();
    let mut c = Challenge::new();
    c.is_digest = s.len() >= 6 && s[..6].eq_ignore_ascii_case(b"Digest");
    c.realm_len = find_value(s, b"realm", &mut c.realm);
    c.nonce_len = find_value(s, b"nonce", &mut c.nonce);
    c.opaque_len = find_value(s, b"opaque", &mut c.opaque);
    let mut qop = [0u8; 40];
    let qn = find_value(s, b"qop", &mut qop);
    // qop may be `auth` or `auth,auth-int` -> we support `auth`
    c.qop_auth = contains(&qop[..qn], b"auth");
    c
}

/// Find `key = value` (value quoted or a bare token), case-insensitive key with
/// a word boundary before it. Writes the value into `out`, returns its length.
fn find_value(s: &[u8], key: &[u8], out: &mut [u8]) -> usize {
    let mut i = 0;
    while i + key.len() <= s.len() {
        let mut matches = true;
        for j in 0..key.len() {
            if s[i + j].to_ascii_lowercase() != key[j] {
                matches = false;
                break;
            }
        }
        let boundary = i == 0 || !s[i - 1].is_ascii_alphanumeric();
        if matches && boundary {
            let mut k = i + key.len();
            while k < s.len() && s[k] == b' ' {
                k += 1;
            }
            if k < s.len() && s[k] == b'=' {
                k += 1;
                while k < s.len() && s[k] == b' ' {
                    k += 1;
                }
                let mut o = 0;
                if k < s.len() && s[k] == b'"' {
                    k += 1;
                    while k < s.len() && s[k] != b'"' {
                        if o < out.len() {
                            out[o] = s[k];
                            o += 1;
                        }
                        k += 1;
                    }
                } else {
                    while k < s.len() && s[k] != b',' && s[k] != b' ' {
                        if o < out.len() {
                            out[o] = s[k];
                            o += 1;
                        }
                        k += 1;
                    }
                }
                return o;
            }
        }
        i += 1;
    }
    0
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return needle.is_empty();
    }
    'outer: for i in 0..=hay.len() - needle.len() {
        for j in 0..needle.len() {
            if hay[i + j] != needle[j] {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

// ------------------------------ response -----------------------------------

/// Compute the Digest `response` value (32 hex chars) into `out`.
/// `nc`/`cnonce` only matter when `qop_auth`.
#[allow(clippy::too_many_arguments)]
pub fn response_hex(
    user: &str,
    realm: &str,
    pass: &str,
    method: &str,
    uri: &str,
    nonce: &str,
    qop_auth: bool,
    nc: &str,
    cnonce: &str,
    out: &mut [u8; 32],
) {
    let mut ha1 = [0u8; 32];
    md5_hex_join(&[user.as_bytes(), realm.as_bytes(), pass.as_bytes()], &mut ha1);
    let mut ha2 = [0u8; 32];
    md5_hex_join(&[method.as_bytes(), uri.as_bytes()], &mut ha2);
    if qop_auth {
        md5_hex_join(
            &[&ha1[..], nonce.as_bytes(), nc.as_bytes(), cnonce.as_bytes(), &b"auth"[..], &ha2[..]],
            out,
        );
    } else {
        md5_hex_join(&[&ha1[..], nonce.as_bytes(), &ha2[..]], out);
    }
}

/// Build the `Authorization: Digest ...` header VALUE (the part after
/// "Authorization: ") into `out`. Returns its length.
#[allow(clippy::too_many_arguments)]
pub fn build_header(
    user: &str,
    realm: &str,
    nonce: &str,
    uri: &str,
    response: &str,
    opaque: Option<&str>,
    qop_auth: bool,
    nc: &str,
    cnonce: &str,
    out: &mut [u8],
) -> usize {
    use core::fmt::Write;
    let mut w = Buf { b: out, n: 0 };
    let _ = write!(
        w,
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", algorithm=MD5, response=\"{}\"",
        user, realm, nonce, uri, response
    );
    if qop_auth {
        let _ = write!(w, ", qop=auth, nc={}, cnonce=\"{}\"", nc, cnonce);
    }
    if let Some(o) = opaque {
        if !o.is_empty() {
            let _ = write!(w, ", opaque=\"{}\"", o);
        }
    }
    w.n
}

struct Buf<'a> {
    b: &'a mut [u8],
    n: usize,
}
impl core::fmt::Write for Buf<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let take = core::cmp::min(bytes.len(), self.b.len().saturating_sub(self.n));
        self.b[self.n..self.n + take].copy_from_slice(&bytes[..take]);
        self.n += take;
        Ok(())
    }
}

// -------------------------------- self-test --------------------------------

/// Unit-test MD5 + the Digest computation against RFC vectors (run by
/// `networktest`, no network).
#[cfg(feature = "networktest")]
pub fn selftest() {
    use esp_println::println;
    println!("[*] MD5 + HTTP Digest (no network)...");
    let mut pass = 0u32;
    let mut fail = 0u32;

    // MD5 known-answer tests
    let md5_cases: &[(&[u8], &str)] = &[
        (b"".as_slice(), "d41d8cd98f00b204e9800998ecf8427e"),
        (b"abc".as_slice(), "900150983cd24fb0d6963f7d28e17f72"),
        (b"The quick brown fox jumps over the lazy dog".as_slice(), "9e107d9d372bb6826bd81d3542a419d6"),
    ];
    for (i, (msg, want)) in md5_cases.iter().enumerate() {
        let d = md5(msg);
        let mut hx = [0u8; 32];
        hex_into(&d, &mut hx);
        let got = core::str::from_utf8(&hx).unwrap_or("");
        if got == *want {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL md5 #{i}: got {got:?} want {want:?}");
        }
    }

    // RFC 2617 §3.5 worked example
    let mut resp = [0u8; 32];
    response_hex(
        "Mufasa",
        "testrealm@host.com",
        "Circle Of Life",
        "GET",
        "/dir/index.html",
        "dcd98b7102dd2f0e8b11d0f600bfb0c093",
        true,
        "00000001",
        "0a4f113b",
        &mut resp,
    );
    let got = core::str::from_utf8(&resp).unwrap_or("");
    if got == "6629fae49393a05397450978507c4ef1" {
        pass += 1;
    } else {
        fail += 1;
        println!("    FAIL digest response: got {got:?} want 6629fae49393a05397450978507c4ef1");
    }

    // challenge parse
    let ch = parse_challenge(
        "Digest realm=\"IP Camera\", nonce=\"abc123def\", qop=\"auth\", opaque=\"deadbeef\"",
    );
    if ch.is_digest
        && ch.realm_str() == "IP Camera"
        && ch.nonce_str() == "abc123def"
        && ch.qop_auth
        && ch.opaque_str() == "deadbeef"
    {
        pass += 1;
    } else {
        fail += 1;
        println!(
            "    FAIL parse: digest={} realm={:?} nonce={:?} qop={} opaque={:?}",
            ch.is_digest, ch.realm_str(), ch.nonce_str(), ch.qop_auth, ch.opaque_str()
        );
    }

    println!("    md5+digest: {pass} pass, {fail} fail");
}

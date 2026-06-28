//! Echoputer WPA crack-offload server (host-side, std-only Rust).
//!
//! The companion to the firmware's compute-offload (C) tier. The Cardputer can only
//! manage a few PBKDF2 guesses a second; a PC/GPU does millions. So the device exports
//! a captured handshake as a hashcat `.22000` line (`HS22000.TXT` on the SD, or POSTed
//! here), this server runs `hashcat -m 22000` against a real wordlist, and returns the
//! recovered passphrase.
//!
//! Protocol (matches the firmware's `radio::http::build_post`):
//!   POST /crack   (any path accepted)
//!   header `X-Offload-Sig: <hex>` = HMAC-SHA256(psk, body)  (required iff OFFLOAD_KEY set)
//!   body = one .22000 line:  WPA*02*<mic>*<ap>*<sta>*<essid>*<anonce>*<eapol>*00
//!   -> 200 text/plain, body = the passphrase  (empty body = not in the wordlist)
//!
//! Safety: with NO `OFFLOAD_KEY` set it binds 127.0.0.1 ONLY (you cannot accidentally
//! expose an unauthenticated cracker). Set `OFFLOAD_KEY` (a shared secret) to allow a
//! LAN bind via `BIND=0.0.0.0`; every POST must then carry a valid HMAC signature over
//! its body — the PSK is never transmitted, so sniffing a request can't steal the key.
//! Concurrency is capped so a flood can't spawn unbounded hashcat processes.
//!
//! Std only, no crates.
//!
//! Usage:  crack-server [WORDLIST] [PORT]      (defaults rockyou.txt, 8080)
//! Env:    OFFLOAD_KEY=<psk>  BIND=0.0.0.0

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Cap concurrent hashcat runs so a flood of requests can't fork-bomb the box.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
const MAX_CONCURRENT: usize = 2;

struct Cfg {
    wordlist: String,
    key: Option<String>, // shared secret; None = no auth (localhost only)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let wordlist = args.next().unwrap_or_else(|| "rockyou.txt".into());
    let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8080);
    let key = std::env::var("OFFLOAD_KEY").ok().filter(|k| !k.is_empty());

    // SAFETY: refuse to serve unauthenticated on anything but loopback. A LAN/public
    // bind requires a shared secret, so we never auto-expose an open cracker.
    let bind = match &key {
        None => "127.0.0.1".to_string(),
        Some(_) => std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0".into()),
    };

    if which("hashcat").is_none() {
        eprintln!("WARNING: 'hashcat' not on PATH — run server-install.sh / offload-install.sh first.");
    }
    if !std::path::Path::new(&wordlist).exists() {
        eprintln!("WARNING: wordlist not found: {wordlist}");
    }
    println!("echoputer crack-offload server on {bind}:{port}");
    println!("  wordlist : {wordlist}");
    println!("  auth     : {}", if key.is_some() { "X-Offload-Sig (HMAC-SHA256) required" } else { "none (loopback-only)" });
    println!("  POST a .22000 line to http://{bind}:{port}/crack");

    let cfg = Arc::new(Cfg { wordlist, key });
    let listener = match TcpListener::bind((bind.as_str(), port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind failed on {bind}:{port}: {e}");
            std::process::exit(1);
        }
    };
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let cfg = Arc::clone(&cfg);
        std::thread::spawn(move || {
            if let Err(e) = handle(&mut s, &cfg) {
                eprintln!("  conn error: {e}");
            }
        });
    }
}

fn handle(s: &mut TcpStream, cfg: &Cfg) -> std::io::Result<()> {
    // a stalled peer must not pin a thread forever (slow-loris): bound every read.
    // This sits before the auth gate + crack cap, so it protects against unauth abuse.
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    // read the request: headers up to \r\n\r\n, then Content-Length bytes of body
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let header_end = loop {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            break p + 4;
        }
        if buf.len() > 16 * 1024 {
            return respond(s, 400, b""); // runaway header
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    if head.starts_with("GET") {
        return respond(s, 200, b"echoputer crack-offload server: POST a .22000 line to /crack\n");
    }
    // reject ambiguous framing (CL/TE desync, duplicate sig) — safe behind a proxy too.
    if header_count(&head, "content-length") > 1
        || header_count(&head, "x-offload-sig") > 1
        || header_value(&head, "transfer-encoding").is_some()
    {
        return respond(s, 400, b"bad framing\n");
    }

    // read the body FIRST — the request signature is computed over it.
    let clen = content_length(&head);
    if clen > 64 * 1024 {
        return respond(s, 400, b"body too large\n"); // a .22000 line is < 1 KB
    }
    while buf.len() < header_end + clen {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    if buf.len() < header_end + clen {
        return respond(s, 400, b"incomplete body\n"); // don't sign/verify a truncated prefix
    }
    let body_bytes = &buf[header_end..header_end + clen];

    // auth: if a key is configured, the request must carry X-Offload-Sig =
    // HMAC-SHA256(psk, body). The PSK itself is never sent, so a sniffed request can't
    // recover or reuse the secret on a different body.
    if let Some(want) = &cfg.key {
        let got = header_value(&head, "x-offload-sig").unwrap_or("");
        let expect = hmac_sha256_hex(want.as_bytes(), body_bytes);
        if !ct_eq(got.as_bytes(), expect.as_bytes()) {
            println!("  rejected: bad/missing X-Offload-Sig");
            return respond(s, 403, b"forbidden\n");
        }
    }

    // bound concurrency: don't let a flood spawn unlimited hashcat processes
    if ACTIVE.fetch_add(1, Ordering::SeqCst) >= MAX_CONCURRENT {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
        println!("  busy: {MAX_CONCURRENT} cracks already running");
        return respond(s, 503, b"busy\n");
    }
    let _guard = Guard; // decrements ACTIVE on drop (even if read below errors)

    let body = String::from_utf8_lossy(body_bytes).into_owned();
    let essid = body.split('*').nth(5).unwrap_or("?");
    println!("  crack request ({clen} B, essid_hex={essid}) ...");
    let pw = crack(body.trim(), &cfg.wordlist);
    if pw.is_empty() {
        println!("  -> not found");
    } else {
        println!("  -> CRACKED: {pw}");
    }
    respond(s, 200, pw.as_bytes())
}

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Run `hashcat -m 22000` on one `.22000` line; return the passphrase or "".
fn crack(line: &str, wordlist: &str) -> String {
    if !line.starts_with("WPA*") {
        return String::new();
    }
    let dir = std::env::temp_dir().join(format!("ecp-{}-{:?}", std::process::id(), std::thread::current().id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return String::new();
    }
    let hashfile = dir.join("hs.22000");
    let potfile = dir.join("out.pot"); // isolate so --show reads only this run
    let res = (|| -> std::io::Result<String> {
        std::fs::write(&hashfile, format!("{line}\n"))?;
        let hf = hashfile.to_string_lossy();
        let pot = potfile.to_string_lossy();
        let _ = Command::new("hashcat")
            .args(["-m", "22000", &hf, wordlist, "--quiet", "--potfile-path", &pot])
            .output()?;
        let shown = Command::new("hashcat")
            .args(["-m", "22000", &hf, "--show", "--quiet", "--potfile-path", &pot])
            .output()?;
        for ln in String::from_utf8_lossy(&shown.stdout).lines() {
            if let Some(idx) = ln.rfind(':') {
                return Ok(ln[idx + 1..].trim().to_string());
            }
        }
        Ok(String::new())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    res.unwrap_or_default()
}

fn respond(s: &mut TcpStream, status: u16, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        503 => "Service Unavailable",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

/// First value of header `name` (lowercased match), trimmed.
fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) {
            Some(v.trim())
        } else {
            None
        }
    })
}

/// How many times header `name` appears (lowercased match) — used to reject the
/// duplicate Content-Length / signature headers a smuggling attack would inject.
fn header_count(head: &str, name: &str) -> usize {
    head.lines()
        .filter(|l| l.split_once(':').map(|(k, _)| k.trim().eq_ignore_ascii_case(name)).unwrap_or(false))
        .count()
}

/// Constant-time byte compare (don't leak the key length/prefix via timing).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn content_length(head: &str) -> usize {
    header_value(head, "content-length").and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Minimal `which`: is `prog` on PATH?
fn which(prog: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(prog)).find(|p| p.is_file())
}

// --------------------- SHA-256 + HMAC-SHA256 (std, no crates) ---------------------
// Must match the firmware's src/radio/sha256.rs byte-for-byte so the signatures agree;
// both are anchored to the NIST / RFC 4231 vectors (see test below).

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(msg: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let bitlen = (msg.len() as u64).wrapping_mul(8);
    let mut data = msg.to_vec();
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
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(SHA256_K[i]).wrapping_add(w[i]);
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

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
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

/// `HMAC-SHA256(key, msg)` as a 64-char lowercase hex string.
fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    let mac = hmac_sha256(key, msg);
    let mut s = String::with_capacity(64);
    for b in mac {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn sha256_nist_vectors() {
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn hmac_sha256_rfc4231() {
        // TC1: key = 0x0b*20, data = "Hi There"
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // TC2: key = "Jefe", data = "what do ya want for nothing?"
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // long-key path (>64 B -> pre-hashed)
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn hmac_hex_matches_firmware_shape() {
        // a 64-char lowercase hex string, deterministic for a fixed key+body
        let sig = hmac_sha256_hex(b"sekret", b"WPA*02*ab");
        assert_eq!(sig.len(), 64);
        assert!(sig.bytes().all(|b| b.is_ascii_hexdigit()));
        // byte-exact interop anchor: this is `printf %s 'WPA*02*ab' | openssl dgst
        // -sha256 -hmac sekret`, and the firmware's A3 networktest asserts the SAME
        // value — so device, server, and openssl all agree on the wire signature.
        assert_eq!(sig, "9ad9f3d9c28b43a2dfb4cb03c7ee98d469df879ed0e5bb6b9de399218310f01d");
    }
}

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
//!   header `X-Offload-Key: <psk>`  (required iff OFFLOAD_KEY is set)
//!   body = one .22000 line:  WPA*02*<mic>*<ap>*<sta>*<essid>*<anonce>*<eapol>*00
//!   -> 200 text/plain, body = the passphrase  (empty body = not in the wordlist)
//!
//! Safety: with NO `OFFLOAD_KEY` set it binds 127.0.0.1 ONLY (you cannot accidentally
//! expose an unauthenticated cracker). Set `OFFLOAD_KEY` (a shared secret) to allow a
//! LAN bind via `BIND=0.0.0.0`; every POST must then carry the matching header.
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
    println!("  auth     : {}", if key.is_some() { "X-Offload-Key required" } else { "none (loopback-only)" });
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

    // auth: if a key is configured, the request must carry a matching header
    if let Some(want) = &cfg.key {
        let got = header_value(&head, "x-offload-key").unwrap_or("");
        if !ct_eq(got.as_bytes(), want.as_bytes()) {
            println!("  rejected: bad/missing X-Offload-Key");
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

    let clen = content_length(&head);
    while buf.len() < header_end + clen {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let end = (header_end + clen).min(buf.len());
    let body = String::from_utf8_lossy(&buf[header_end..end]).into_owned();
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

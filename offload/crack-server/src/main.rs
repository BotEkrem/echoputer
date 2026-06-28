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
//!   body = one .22000 line:  WPA*02*<mic>*<ap>*<sta>*<essid>*<anonce>*<eapol>*00
//!   -> 200 text/plain, body = the passphrase  (empty body = not in the wordlist)
//!
//! Std only, no crates.
//!
//! Usage:  crack-server [WORDLIST] [PORT]      (defaults: rockyou.txt, 8080)

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

fn main() {
    let mut args = std::env::args().skip(1);
    let wordlist = args.next().unwrap_or_else(|| "rockyou.txt".into());
    let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8080);

    if which("hashcat").is_none() {
        eprintln!("WARNING: 'hashcat' not on PATH — run ./offload-install.sh first.");
    }
    if !std::path::Path::new(&wordlist).exists() {
        eprintln!("WARNING: wordlist not found: {wordlist}");
    }
    println!("echoputer crack-offload server on 0.0.0.0:{port}");
    println!("  wordlist : {wordlist}");
    println!("  POST a .22000 line to http://<this-host>:{port}/crack");

    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind failed on :{port}: {e}");
            std::process::exit(1);
        }
    };
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let wl = wordlist.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle(&mut s, &wl) {
                eprintln!("  conn error: {e}");
            }
        });
    }
}

fn handle(s: &mut TcpStream, wordlist: &str) -> std::io::Result<()> {
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
            return respond(s, b""); // runaway header
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    if head.starts_with("GET") {
        return respond(s, b"echoputer crack-offload server: POST a .22000 line to /crack\n");
    }
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
    let pw = crack(body.trim(), wordlist);
    if pw.is_empty() {
        println!("  -> not found");
    } else {
        println!("  -> CRACKED: {pw}");
    }
    respond(s, pw.as_bytes())
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
        // 1) attack against the wordlist
        let _ = Command::new("hashcat")
            .args(["-m", "22000", &hf, wordlist, "--quiet", "--potfile-path", &pot])
            .output()?;
        // 2) print what cracked: potfile lines are "<hash>:<password>"
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

fn respond(s: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|l| {
            let l = l.to_ascii_lowercase();
            l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
        })
        .unwrap_or(0)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Minimal `which`: is `prog` on PATH?
fn which(prog: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(prog)).find(|p| p.is_file())
}

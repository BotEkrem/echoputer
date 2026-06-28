//! Minimal HTTP/1.0 client over smoltcp (Tier 3 / CCTV swiss-knife core).
//!
//! This is the reusable request primitive that the LAN/camera tooling builds on.
//! It operates on the CALLER's smoltcp stack (interface + phy + socket set + a
//! TCP socket handle) so the same DHCP-leased station can be reused for many
//! requests without re-associating: the caller brings up the stack (see
//! [`crate::radio::netscan`] for the DHCP pattern), this module does one
//! connect + `GET` + response-head read.
//!
//! Plaintext HTTP only — no TLS — so it works against ports 80/8080 and the
//! many IP cameras / DVRs that expose an unencrypted web UI. TLS ports
//! (443/8443) are out of scope here.

use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::Ipv4Address;

use esp_hal::time::{Duration, Instant};

use crate::radio::portal::WifiPhy;

/// Parsed head of an HTTP response. Body is intentionally not retained — the
/// banner-grab use only needs the status line + `Server:` header. Later builds
/// (camera fingerprint / default-creds) can extend this with body + realm.
pub struct HttpResp {
    /// TCP connection reached `Established` and the request was sent.
    pub connected: bool,
    /// Parsed status code (e.g. 200, 401), or 0 if the response was unparsable.
    pub status: u16,
    /// Captured `Server:` header value (truncated to fit).
    pub server: [u8; 48],
    pub server_len: usize,
    /// Captured `WWW-Authenticate:` value (truncated) — the realm is a strong
    /// camera/DVR brand hint, and a 401 challenge is ubiquitous on cameras. Sized
    /// to hold a full Digest challenge (realm+nonce+qop+opaque) for the cred ladder.
    pub www_auth: [u8; 200],
    pub www_auth_len: usize,
}

impl HttpResp {
    fn new() -> Self {
        Self {
            connected: false,
            status: 0,
            server: [0; 48],
            server_len: 0,
            www_auth: [0; 200],
            www_auth_len: 0,
        }
    }

    /// The captured `Server:` header value as a string slice.
    pub fn server_str(&self) -> &str {
        core::str::from_utf8(&self.server[..self.server_len]).unwrap_or("")
    }

    /// The captured `WWW-Authenticate:` value as a string slice.
    pub fn www_auth_str(&self) -> &str {
        core::str::from_utf8(&self.www_auth[..self.www_auth_len]).unwrap_or("")
    }

    /// Render a compact one-line banner into `dst` ("`<port> <server>`", or
    /// "`<port> http <status>`" when there is no Server header). Returns the
    /// number of bytes written. Lets a caller fill a fixed buffer without alloc.
    pub fn write_banner(&self, port: u16, dst: &mut [u8]) -> usize {
        use core::fmt::Write;
        let mut w = Buf { b: dst, n: 0, overflow: false };
        let srv = self.server_str();
        if !srv.is_empty() {
            let _ = write!(w, "{} {}", port, srv);
        } else if self.status != 0 {
            let _ = write!(w, "{} http {}", port, self.status);
        }
        w.n
    }
}

/// Connect to `ip:port`, send `GET path`, read + parse the response head.
///
/// Operates on the caller's `iface`/`device`/`sockets` + a TCP socket `tcp_h`
/// (whose buffers the caller sized). `local_port` is the ephemeral source port
/// to bind. `auth` is the full `Authorization` header VALUE (e.g. `"Basic
/// dXNlcjpwYXNz"` or `"Digest username=..., ..."`), or `None`. `now()` must
/// return the smoltcp clock the caller uses
/// for `iface.poll`. Blocking, with hard per-phase timeouts; the socket is left
/// aborted on return.
#[allow(clippy::too_many_arguments)]
pub fn http_head(
    iface: &mut Interface,
    device: &mut WifiPhy,
    sockets: &mut SocketSet<'_>,
    tcp_h: SocketHandle,
    ip: Ipv4Address,
    port: u16,
    path: &str,
    auth: Option<&str>,
    local_port: u16,
    now: &dyn Fn() -> SmolInstant,
) -> HttpResp {
    let mut resp = HttpResp::new();

    // ---- (1) fresh connection ----
    {
        let cx = iface.context();
        let s = sockets.get_mut::<tcp::Socket>(tcp_h);
        s.abort();
        if s.connect(cx, (ip, port), local_port).is_err() {
            return resp;
        }
    }
    let t = Instant::now();
    loop {
        iface.poll(now(), device, sockets);
        let st = sockets.get_mut::<tcp::Socket>(tcp_h).state();
        if st == tcp::State::Established {
            break;
        }
        // RST/refused: Closed again shortly after the SYN attempt.
        if st == tcp::State::Closed && t.elapsed() >= Duration::from_millis(80) {
            return resp;
        }
        if t.elapsed() >= Duration::from_millis(900) {
            sockets.get_mut::<tcp::Socket>(tcp_h).abort();
            return resp;
        }
    }
    resp.connected = true;

    // ---- (2) send the request ----
    // 512 so a full Digest `Authorization` header (realm+nonce+response+opaque)
    // fits alongside the request line and base headers.
    let mut reqbuf = [0u8; 512];
    let req = build_request(&mut reqbuf, path, ip, auth);
    {
        let t2 = Instant::now();
        let mut sent = 0usize;
        loop {
            iface.poll(now(), device, sockets);
            let s = sockets.get_mut::<tcp::Socket>(tcp_h);
            if s.can_send() {
                // keep queuing until the whole request is in — it may exceed the
                // socket's tx buffer (large Digest headers), so send in chunks as
                // the buffer drains.
                match s.send_slice(&req[sent..]) {
                    Ok(k) => {
                        sent += k;
                        if sent >= req.len() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            if t2.elapsed() >= Duration::from_millis(500) {
                sockets.get_mut::<tcp::Socket>(tcp_h).abort();
                return resp;
            }
        }
    }

    // ---- (3) read the response head ----
    let mut buf = [0u8; 512];
    let mut filled = 0usize;
    let t3 = Instant::now();
    loop {
        iface.poll(now(), device, sockets);
        let s = sockets.get_mut::<tcp::Socket>(tcp_h);
        if s.can_recv() {
            let n = s
                .recv(|data| {
                    let take = core::cmp::min(data.len(), buf.len() - filled);
                    buf[filled..filled + take].copy_from_slice(&data[..take]);
                    (take, take)
                })
                .unwrap_or(0);
            filled += n;
            if filled >= buf.len() {
                break;
            }
        } else if matches!(s.state(), tcp::State::CloseWait | tcp::State::Closed) {
            // server finished (Connection: close) and nothing left buffered
            break;
        }
        if t3.elapsed() >= Duration::from_millis(1200) {
            break;
        }
    }
    sockets.get_mut::<tcp::Socket>(tcp_h).abort();

    parse_head(&buf[..filled], &mut resp);
    resp
}

/// Connect to `ip:port`, POST `body` to `path` (adding the `X-Offload-Key` PSK header
/// when `key` is Some), read the full response, and return its BODY as an owned String
/// — the crack-offload reply (the recovered passphrase, or empty). `None` on connect/IO
/// failure. Mirrors `http_head` but sends a body and returns the response body. The read
/// window is long because the server runs hashcat synchronously before replying.
/// Returns `(status, body)`: the HTTP status code (`0` = connect/send/read failure,
/// truncated request, or user abort) and the trimmed response body (`None` if empty).
/// `tick() -> false` aborts the long synchronous-crack read wait. The caller maps
/// the status (200 = passphrase body; 403/503 = the server's auth/busy errors).
#[allow(dead_code)] // the device-side offload flow wires this (assoc -> post_body)
#[allow(clippy::too_many_arguments)]
pub fn post_body(
    iface: &mut Interface,
    device: &mut WifiPhy,
    sockets: &mut SocketSet<'_>,
    tcp_h: SocketHandle,
    ip: Ipv4Address,
    port: u16,
    path: &str,
    body: &str,
    key: Option<&str>,
    local_port: u16,
    now: &dyn Fn() -> SmolInstant,
    mut tick: impl FnMut() -> bool,
) -> (u16, Option<alloc::string::String>) {
    // ---- (0) build the request first; refuse if it doesn't fit (no corrupt body) ----
    let mut reqbuf = [0u8; 1024];
    let req_len = build_post(&mut reqbuf, path, ip, body, key).len();
    if req_len == 0 {
        return (0, None); // .22000 line too big for the request buffer
    }
    // ---- (1) fresh connection ----
    {
        let cx = iface.context();
        let s = sockets.get_mut::<tcp::Socket>(tcp_h);
        s.abort();
        if s.connect(cx, (ip, port), local_port).is_err() {
            return (0, None);
        }
    }
    let t = Instant::now();
    loop {
        iface.poll(now(), device, sockets);
        let st = sockets.get_mut::<tcp::Socket>(tcp_h).state();
        if st == tcp::State::Established {
            break;
        }
        if st == tcp::State::Closed && t.elapsed() >= Duration::from_millis(80) {
            return (0, None);
        }
        if t.elapsed() >= Duration::from_millis(900) {
            sockets.get_mut::<tcp::Socket>(tcp_h).abort();
            return (0, None);
        }
    }
    // ---- (2) send the POST ----
    {
        let t2 = Instant::now();
        let mut sent = 0usize;
        loop {
            iface.poll(now(), device, sockets);
            let s = sockets.get_mut::<tcp::Socket>(tcp_h);
            if s.can_send() {
                match s.send_slice(&reqbuf[sent..req_len]) {
                    Ok(k) => {
                        sent += k;
                        if sent >= req_len {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            if t2.elapsed() >= Duration::from_millis(500) {
                sockets.get_mut::<tcp::Socket>(tcp_h).abort();
                return (0, None);
            }
        }
    }
    // ---- (3) read the full response; the server cracks synchronously, so wait ----
    let mut buf = [0u8; 512];
    let mut filled = 0usize;
    let t3 = Instant::now();
    loop {
        iface.poll(now(), device, sockets);
        let s = sockets.get_mut::<tcp::Socket>(tcp_h);
        if s.can_recv() {
            let n = s
                .recv(|data| {
                    let take = core::cmp::min(data.len(), buf.len() - filled);
                    buf[filled..filled + take].copy_from_slice(&data[..take]);
                    (take, take)
                })
                .unwrap_or(0);
            filled += n;
            if filled >= buf.len() {
                break;
            }
        } else if matches!(s.state(), tcp::State::CloseWait | tcp::State::Closed) {
            break;
        }
        if !tick() {
            sockets.get_mut::<tcp::Socket>(tcp_h).abort();
            return (0, None); // user abort during the crack wait
        }
        if t3.elapsed() >= Duration::from_secs(90) {
            break;
        }
    }
    sockets.get_mut::<tcp::Socket>(tcp_h).abort();
    let resp = &buf[..filled];
    // status line: "HTTP/1.x SSS ..."
    let status = if resp.len() >= 12 && &resp[..7] == b"HTTP/1." && resp[9..12].iter().all(u8::is_ascii_digit) {
        (resp[9] - b'0') as u16 * 100 + (resp[10] - b'0') as u16 * 10 + (resp[11] - b'0') as u16
    } else {
        0
    };
    let b = resp_body(resp);
    let body_opt = if b.is_empty() {
        None
    } else {
        Some(alloc::string::String::from_utf8_lossy(b).trim().into())
    };
    (status, body_opt)
}

// ----------------------------- request build ------------------------------

struct Buf<'a> {
    b: &'a mut [u8],
    n: usize,
    overflow: bool,
}
impl core::fmt::Write for Buf<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let take = core::cmp::min(bytes.len(), self.b.len().saturating_sub(self.n));
        self.b[self.n..self.n + take].copy_from_slice(&bytes[..take]);
        self.n += take;
        if take < bytes.len() {
            self.overflow = true;
        }
        Ok(())
    }
}

fn build_request<'a>(buf: &'a mut [u8], path: &str, ip: Ipv4Address, auth: Option<&str>) -> &'a [u8] {
    use core::fmt::Write;
    let o = ip.octets();
    let mut w = Buf { b: buf, n: 0, overflow: false };
    let _ = write!(
        w,
        "GET {} HTTP/1.0\r\nHost: {}.{}.{}.{}\r\nUser-Agent: echoputer\r\nAccept: */*\r\n",
        path, o[0], o[1], o[2], o[3]
    );
    if let Some(a) = auth {
        let _ = write!(w, "Authorization: {}\r\n", a);
    }
    let _ = write!(w, "Connection: close\r\n\r\n");
    let n = w.n;
    &buf[..n]
}

/// Build an HTTP/1.0 POST with a text body — the crack-server offload: POST a `.22000`
/// line to a server, then read the recovered passphrase from the response body. The
/// body is `&str` since a `.22000` line is ASCII. `key` adds the `X-Offload-Key` PSK
/// header the crack-server requires.
#[cfg_attr(not(feature = "networktest"), allow(dead_code))]
fn build_post<'a>(buf: &'a mut [u8], path: &str, ip: Ipv4Address, body: &str, key: Option<&str>) -> &'a [u8] {
    use core::fmt::Write;
    let o = ip.octets();
    let mut w = Buf { b: buf, n: 0, overflow: false };
    let _ = write!(w, "POST {} HTTP/1.0\r\nHost: {}.{}.{}.{}\r\nUser-Agent: echoputer\r\n", path, o[0], o[1], o[2], o[3]);
    if let Some(k) = key {
        let _ = write!(w, "X-Offload-Key: {k}\r\n");
    }
    let _ = write!(w, "Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
    // If anything didn't fit, the body got truncated while Content-Length still
    // claims body.len() — sending that would hang/garble the server. Signal the
    // caller (empty slice) instead of emitting a corrupt, too-large request.
    if w.overflow {
        return &[];
    }
    let n = w.n;
    &buf[..n]
}

/// The body of an HTTP response = everything after the first CRLFCRLF (or LFLF).
fn resp_body(buf: &[u8]) -> &[u8] {
    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        &buf[p + 4..]
    } else if let Some(p) = buf.windows(2).position(|w| w == b"\n\n") {
        &buf[p + 2..]
    } else {
        &[]
    }
}

// ------------------------------ response parse -----------------------------

fn parse_head(buf: &[u8], resp: &mut HttpResp) {
    // status line: "HTTP/1.x SSS ..."
    if buf.len() >= 12 && &buf[..7] == b"HTTP/1." {
        let d = &buf[9..12];
        if d.iter().all(u8::is_ascii_digit) {
            resp.status =
                (d[0] - b'0') as u16 * 100 + (d[1] - b'0') as u16 * 10 + (d[2] - b'0') as u16;
        }
    }
    if let Some(v) = find_header(buf, b"server") {
        let take = core::cmp::min(v.len(), resp.server.len());
        resp.server[..take].copy_from_slice(&v[..take]);
        resp.server_len = take;
    }
    if let Some(v) = find_header(buf, b"www-authenticate") {
        let take = core::cmp::min(v.len(), resp.www_auth.len());
        resp.www_auth[..take].copy_from_slice(&v[..take]);
        resp.www_auth_len = take;
    }
}

/// Find a header value by (case-insensitive) name. Headers follow the status
/// line, so each is preceded by a CRLF — we anchor on that.
fn find_header<'a>(buf: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i + 2 < buf.len() {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' {
            let start = i + 2;
            if start + name.len() < buf.len()
                && ci_eq(&buf[start..start + name.len()], name)
                && buf[start + name.len()] == b':'
            {
                let mut v = start + name.len() + 1;
                while v < buf.len() && (buf[v] == b' ' || buf[v] == b'\t') {
                    v += 1;
                }
                // value runs to the next CR (start of CRLF) or end-of-buffer (a
                // response truncated mid-value still yields the full prefix).
                let mut e = v;
                while e < buf.len() && buf[e] != b'\r' {
                    e += 1;
                }
                return Some(&buf[v..e]);
            }
        }
        i += 1;
    }
    None
}

fn ci_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

// --------------------------------- self-test --------------------------------

/// Boot network self-test for the HTTP client core (no WiFi/AP/DHCP needed),
/// run by the `networktest` build.
///
/// Two layers, both deterministic:
///   (A) feed canned real-world server replies straight through the parser +
///       exercise the request builder — proves status/header extraction and the
///       exact request bytes;
///   (B) a full TCP round-trip over a smoltcp **loopback** device: a temporary
///       in-firmware HTTP "server" socket answers a client socket on the same
///       interface, so connect → send → recv → parse runs end-to-end against a
///       real (if local) TCP connection. The only thing this can't cover is the
///       physical WiFi link itself — that is still validated on real hardware by
///       the LAN-scan step's banner grab (in the `selftest` build).
///
/// `#[inline(never)]`: keeps this diagnostic body out of the giant `main`, whose
/// `.text` would otherwise overflow the Xtensa l32r literal range when linked.
#[cfg(feature = "networktest")]
#[inline(never)]
pub fn networktest() {
    use esp_println::println;

    println!("[*] HTTP client core (no network)...");
    let mut pass = 0u32;
    let mut fail = 0u32;

    // ---- (A1) response-head parser, against real embedded/camera servers ----
    let cases: &[(&[u8], u16, &str)] = &[
        (b"HTTP/1.1 200 OK\r\nServer: nginx/1.18.0\r\nContent-Type: text/html\r\n\r\n<html>", 200, "nginx/1.18.0"),
        (b"HTTP/1.1 401 Unauthorized\r\nServer: App-webs/\r\nWWW-Authenticate: Digest realm=\"IPCamera\"\r\n\r\n", 401, "App-webs/"),
        (b"HTTP/1.0 200 OK\r\nServer: GoAhead-Webs\r\n\r\n", 200, "GoAhead-Webs"),
        (b"HTTP/1.1 200 OK\r\nServer: lighttpd/1.4.35\r\n\r\n", 200, "lighttpd/1.4.35"),
        (b"HTTP/1.1 200 OK\r\nServer: Boa/0.94.14rc21\r\n\r\n", 200, "Boa/0.94.14rc21"),
        (b"HTTP/1.1 200 OK\r\nServer: thttpd/2.25b\r\n\r\n", 200, "thttpd/2.25b"),
        (b"HTTP/1.1 200 OK\r\nserver: MyCam/1.0\r\n\r\n", 200, "MyCam/1.0"),       // lowercase name
        (b"HTTP/1.1 200 OK\r\nServer:\tDahua-Webs\r\n\r\n", 200, "Dahua-Webs"),    // tab after colon
        (b"HTTP/1.1 302 Found\r\nLocation: /doc/page/login.asp\r\nServer: webserver\r\n\r\n", 302, "webserver"),
        (b"HTTP/1.1 200 OK\r\nServer-Timing: cdn;dur=1\r\nContent-Type: text/html\r\n\r\n", 200, ""), // prefix decoy, no real Server
        (b"HTTP/1.1 200 OK\r\nServerfoo: bar\r\nServer: realcam/2\r\n\r\n", 200, "realcam/2"),         // decoy before real
        (b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n", 200, ""),          // no Server header
        (b"HTTP/1.1 200 OK\r\nServer: trunc-no-crlf", 200, "trunc-no-crlf"),       // truncated mid-value
        (b"garbage data no http\r\n\r\n", 0, ""),                                  // not HTTP
        (b"", 0, ""),                                                              // empty
    ];
    for (i, (raw, est, esrv)) in cases.iter().enumerate() {
        let mut r = HttpResp::new();
        parse_head(raw, &mut r);
        if r.status == *est && r.server_str() == *esrv {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL parse #{i}: got {}/{:?} want {}/{:?}", r.status, r.server_str(), est, esrv);
        }
    }

    // ---- (A2) request builder ----
    {
        let mut buf = [0u8; 256];
        let req = build_request(&mut buf, "/", Ipv4Address::new(192, 168, 1, 10), None);
        let want: &[u8] = b"GET / HTTP/1.0\r\nHost: 192.168.1.10\r\nUser-Agent: echoputer\r\nAccept: */*\r\nConnection: close\r\n\r\n";
        if req == want {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL build basic: {:?}", core::str::from_utf8(req));
        }
    }
    {
        let mut buf = [0u8; 256];
        let req = build_request(&mut buf, "/onvif/device_service", Ipv4Address::new(10, 0, 0, 9), Some("Basic YWRtaW46MTIzNA=="));
        let s = core::str::from_utf8(req).unwrap_or("");
        if s.starts_with("GET /onvif/device_service HTTP/1.0\r\n")
            && s.contains("\r\nAuthorization: Basic YWRtaW46MTIzNA==\r\n")
            && s.ends_with("\r\n\r\n")
        {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL build auth: {:?}", s);
        }
    }
    {
        // a too-small buffer must truncate, never panic
        let mut tiny = [0u8; 10];
        let _ = build_request(&mut tiny, "/", Ipv4Address::new(1, 2, 3, 4), None);
        pass += 1;
    }
    // ---- (A3) POST builder (crack-server offload), incl the PSK header ----
    {
        let mut buf = [0u8; 256];
        let req = build_post(&mut buf, "/crack", Ipv4Address::new(10, 0, 0, 2), "WPA*02*ab", Some("sekret"));
        let s = core::str::from_utf8(req).unwrap_or("");
        if s.starts_with("POST /crack HTTP/1.0\r\n")
            && s.contains("\r\nX-Offload-Key: sekret\r\n")
            && s.contains("\r\nContent-Length: 9\r\n")
            && s.ends_with("\r\n\r\nWPA*02*ab")
        {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL build post: {s:?}");
        }
    }
    // ---- (A4) response-body extraction (offload reply parse) ----
    {
        let ok1 = resp_body(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\npassw0rd") == b"passw0rd";
        let ok2 = resp_body(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").is_empty();
        let ok3 = resp_body(b"no headers here").is_empty();
        if ok1 && ok2 && ok3 {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL resp_body: {ok1} {ok2} {ok3}");
        }
    }
    println!("    parser+builder: {pass} pass, {fail} fail");

    // ---- (B) full TCP round-trip over loopback (temp in-firmware server) ----
    loopback_roundtrip();
}

/// Stand up a temporary HTTP server + client on a single smoltcp loopback
/// interface and run one request through the real TCP stack.
#[cfg(feature = "networktest")]
fn loopback_roundtrip() {
    use esp_hal::time::{Duration, Instant};
    use esp_println::println;
    use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
    use smoltcp::phy::{Loopback, Medium};
    use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Cidr};

    println!("[*] HTTP loopback round-trip (temp server, no WiFi)...");

    let mut dev = Loopback::new(Medium::Ethernet);
    let t0 = Instant::now();
    let now = || SmolInstant::from_millis(t0.elapsed().as_millis() as i64);

    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress([0x02, 0, 0, 0, 0, 1])));
    cfg.random_seed = 0xC0FF_EE01;
    let mut iface = Interface::new(cfg, &mut dev, now());
    let host = Ipv4Address::new(192, 168, 69, 1);
    iface.update_ip_addrs(|a| {
        let _ = a.push(IpCidr::Ipv4(Ipv4Cidr::new(host, 24)));
    });

    let mut storage = [SocketStorage::EMPTY; 2];
    let mut sockets = SocketSet::new(&mut storage[..]);

    // temp server socket, listening on :80
    let mut s_rx = [0u8; 256];
    let mut s_tx = [0u8; 512];
    let sh = sockets.add(tcp::Socket::new(
        tcp::SocketBuffer::new(&mut s_rx[..]),
        tcp::SocketBuffer::new(&mut s_tx[..]),
    ));
    if sockets.get_mut::<tcp::Socket>(sh).listen(80).is_err() {
        println!("    FAIL  server listen");
        return;
    }

    // client socket
    let mut c_rx = [0u8; 512];
    let mut c_tx = [0u8; 256];
    let ch = sockets.add(tcp::Socket::new(
        tcp::SocketBuffer::new(&mut c_rx[..]),
        tcp::SocketBuffer::new(&mut c_tx[..]),
    ));
    {
        let cx = iface.context();
        if sockets.get_mut::<tcp::Socket>(ch).connect(cx, (host, 80u16), 49600u16).is_err() {
            println!("    FAIL  client connect");
            return;
        }
    }

    // what the temp server answers (a Hikvision-style 401)
    let reply: &[u8] = b"HTTP/1.1 401 Unauthorized\r\nServer: App-webs/\r\nWWW-Authenticate: Digest realm=\"IPCamera\"\r\nContent-Length: 0\r\n\r\n";
    // the request the client sends — same builder http_head() uses
    let mut reqbuf = [0u8; 256];
    let req = build_request(&mut reqbuf, "/", host, None);

    let mut sent = false;
    let mut served = false;
    let mut rbuf = [0u8; 512];
    let mut filled = 0usize;
    let start = Instant::now();
    loop {
        iface.poll(now(), &mut dev, &mut sockets);

        // client: send once connected, then drain anything received
        {
            let c = sockets.get_mut::<tcp::Socket>(ch);
            if !sent && c.state() == tcp::State::Established && c.can_send() {
                let _ = c.send_slice(req);
                sent = true;
            }
            if c.can_recv() {
                let n = c
                    .recv(|d| {
                        let take = core::cmp::min(d.len(), rbuf.len() - filled);
                        rbuf[filled..filled + take].copy_from_slice(&d[..take]);
                        (take, take)
                    })
                    .unwrap_or(0);
                filled += n;
            }
        }

        // temp server: consume the request, reply, close
        {
            let s = sockets.get_mut::<tcp::Socket>(sh);
            if !served && s.can_recv() {
                let _ = s.recv(|d| (d.len(), ()));
                if s.can_send() {
                    let _ = s.send_slice(reply);
                    s.close();
                    served = true;
                }
            }
        }

        if filled > 0 && matches!(sockets.get::<tcp::Socket>(ch).state(), tcp::State::CloseWait | tcp::State::Closed) {
            break;
        }
        if start.elapsed() >= Duration::from_secs(3) {
            break;
        }
    }

    let mut resp = HttpResp::new();
    resp.connected = sent;
    parse_head(&rbuf[..filled], &mut resp);

    let realm_ok = resp.www_auth_str().contains("IPCamera");
    if resp.connected && resp.status == 401 && resp.server_str() == "App-webs/" && realm_ok {
        println!(
            "    OK  {} B, status={} server={:?} realm={:?}",
            filled, resp.status, resp.server_str(), resp.www_auth_str()
        );
    } else {
        println!(
            "    FAIL  connected={} bytes={} status={} server={:?} realm={:?}",
            resp.connected, filled, resp.status, resp.server_str(), resp.www_auth_str()
        );
    }
}

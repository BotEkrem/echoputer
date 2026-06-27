//! LAN scanner (Tier 3): join an OPEN WiFi network as a station, pull a DHCP
//! lease, then TCP connect-scan the gateway's common ports. Open networks only —
//! WPA would need on-device password entry (a separate keyboard-input feature).
//!
//! Reuses the smoltcp `phy::Device` adapter from `portal` over the STATION
//! interface. The whole thing is a one-shot blocking tool like the others:
//! `radio.rs` associates the STA, this module owns the TCP/IP stack + scan loop.

use esp_println::println;
use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
use smoltcp::socket::{dhcpv4, tcp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Address};

use esp_hal::time::{Duration, Instant};
use esp_radio::wifi::Interface as WifiIface;

use crate::radio::portal::WifiPhy;

/// Common service ports probed on the gateway.
pub const PORTS: [u16; 12] = [21, 22, 23, 53, 80, 139, 443, 445, 3389, 7547, 8080, 8443];

#[derive(Clone)]
pub struct NetResult {
    pub got_ip: bool,
    pub ip: [u8; 4],
    pub gw: [u8; 4],
    pub scanned: usize, // ports probed so far
    pub open: [bool; 12],
    pub phase: &'static str,
    /// Server banner grabbed from the first open HTTP port (camera/DVR hint).
    pub banner: [u8; 48],
    pub banner_len: usize,
    /// Cracked WiFi password (if the joined AP was encrypted), else empty.
    pub wifi_pass: [u8; 24],
    pub wifi_pass_len: usize,
}
impl NetResult {
    pub fn new() -> Self {
        Self {
            got_ip: false,
            ip: [0; 4],
            gw: [0; 4],
            scanned: 0,
            open: [false; 12],
            phase: "join",
            banner: [0; 48],
            banner_len: 0,
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
    pub fn open_count(&self) -> usize {
        self.open.iter().filter(|&&o| o).count()
    }
    /// The grabbed HTTP banner, if any.
    pub fn banner_str(&self) -> &str {
        core::str::from_utf8(&self.banner[..self.banner_len]).unwrap_or("")
    }
}

/// Drive the scan on an already-associated STA `iface`. `tick(&NetResult)` is
/// called periodically so the caller can repaint + poll for an abort key.
pub fn scan(iface_sta: WifiIface<'static>, mac: [u8; 6], mut tick: impl FnMut(&NetResult) -> bool) -> NetResult {
    let mut device = WifiPhy::new(iface_sta);
    let t0 = Instant::now();
    let now = || SmolInstant::from_millis(t0.elapsed().as_millis() as i64);

    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    cfg.random_seed = t0.duration_since_epoch().as_micros() | 1;
    let mut iface = Interface::new(cfg, &mut device, now());

    let mut storage = [SocketStorage::EMPTY; 4];
    let mut sockets = SocketSet::new(&mut storage[..]);
    let dhcp_h = sockets.add(dhcpv4::Socket::new());

    let mut res = NetResult::new();

    // ---- phase 1: DHCP lease (up to 12 s) ----
    res.phase = "dhcp";
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
                    res.gw = r.octets();
                }
                break;
            }
        }
        if !tick(&res) {
            return res;
        }
        if t0.elapsed() >= Duration::from_secs(12) {
            res.phase = "no lease";
            return res;
        }
    }
    if res.gw == [0, 0, 0, 0] {
        res.phase = "no gateway";
        return res;
    }

    // ---- phase 2: TCP connect-scan the gateway ----
    res.phase = "scan";
    let gw = Ipv4Address::new(res.gw[0], res.gw[1], res.gw[2], res.gw[3]);
    let mut rx = [0u8; 256];
    let mut tx = [0u8; 256];
    let tcp_h = sockets.add(tcp::Socket::new(
        tcp::SocketBuffer::new(&mut rx[..]),
        tcp::SocketBuffer::new(&mut tx[..]),
    ));

    for (i, &port) in PORTS.iter().enumerate() {
        // start a fresh connection attempt
        {
            let cx = iface.context();
            let s = sockets.get_mut::<tcp::Socket>(tcp_h);
            s.abort();
            let _ = s.connect(cx, (gw, port), 49152 + i as u16);
        }
        let port_start = Instant::now();
        loop {
            iface.poll(now(), &mut device, &mut sockets);
            let st = sockets.get_mut::<tcp::Socket>(tcp_h).state();
            match st {
                tcp::State::Established => {
                    res.open[i] = true;
                    break;
                }
                tcp::State::Closed if port_start.elapsed() >= Duration::from_millis(80) => {
                    // RST/refused (Closed after a SynSent attempt)
                    break;
                }
                _ => {}
            }
            if port_start.elapsed() >= Duration::from_millis(700) {
                // no response -> filtered/closed
                break;
            }
            if !tick(&res) {
                let s = sockets.get_mut::<tcp::Socket>(tcp_h);
                s.abort();
                return res;
            }
        }
        res.scanned = i + 1;
        if res.open[i] {
            println!("[NETSCAN] {}.{}.{}.{}:{} OPEN", res.gw[0], res.gw[1], res.gw[2], res.gw[3], port);
            // Grab the HTTP Server banner from the first open web port — the
            // seed for camera/DVR fingerprinting. Plaintext only (skip TLS).
            if res.banner_len == 0 && (port == 80 || port == 8080) {
                let r = crate::radio::http::http_head(
                    &mut iface,
                    &mut device,
                    &mut sockets,
                    tcp_h,
                    gw,
                    port,
                    "/",
                    None,
                    50000 + i as u16,
                    &now,
                );
                if r.connected {
                    res.banner_len = r.write_banner(port, &mut res.banner);
                    if res.banner_len > 0 {
                        println!("[NETSCAN] banner: {}", res.banner_str());
                    }
                }
            }
        }
        if !tick(&res) {
            break;
        }
    }
    sockets.get_mut::<tcp::Socket>(tcp_h).abort();
    res.phase = "done";
    res
}

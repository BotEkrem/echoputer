//! Web UI serve layer: pull a DHCP lease on an already-associated STA interface,
//! then run a single-socket HTTP/1.1 server on :80 that exposes a system + SD
//! filesystem dashboard (browse / download / upload / delete / mkdir).
//!
//! Composes the existing radio plumbing: the `portal::WifiPhy` smoltcp adapter and
//! the netscan DHCP-lease loop. Unlike the captive portal there is no SoftAP and
//! no DHCP/DNS server — we are a client on someone's LAN, serving on our leased
//! IP. One TCP socket, close-after-response, so the dashboard JS issues requests
//! sequentially. Large transfers (file download/upload) are streamed in fixed
//! chunks, polling smoltcp between chunks, so nothing buffers a whole file in RAM.

use embedded_sdmmc::{BlockDevice, Mode, TimeSource, VolumeIdx, VolumeManager};
use esp_hal::time::{Duration, Instant};
use esp_radio::wifi::Interface as WifiIface;
use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
use smoltcp::socket::{dhcpv4, tcp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr};

use crate::radio::portal::WifiPhy;

/// System metrics snapshot, gathered by `main` (it owns the heap/battery/efuse
/// reads) and rendered into `/api/sys` without touching hardware mid-serve.
pub struct SysSnapshot {
    pub heap_free: usize,
    pub heap_used: usize,
    pub heap_total: usize,
    pub uptime_s: u64,
    pub batt_pct: i32, // -1 == on USB / no reading
    pub mac: [u8; 6],
}

// ---- long-filename sidecar index (/ECHO/DATA/NAMES.IDX) ----------------
// FAT on embedded-sdmmc 0.9 can only CREATE 8.3 short names, so uploads are stored
// 8.3 on the card but we keep a `SHORT83\tLong Name.ext\n` map here and show the
// long name in the Web UI. The card stays standard FAT (a PC sees the 8.3 names);
// the GB emulator scans by 8.3 too, so nothing else is affected.
const IDX_FILE: &str = "NAMES.IDX";

/// Append a `short\tlong\n` mapping (best-effort; skipped if the names match).
fn index_append<D: BlockDevice, T: TimeSource>(vm: &VolumeManager<D, T>, short: &[u8], long: &[u8]) {
    if short == long || long.is_empty() {
        return;
    }
    let _ = (|| -> Option<()> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let mut dir = vol.open_root_dir().ok()?;
        dir.change_dir(CRED_APP).ok()?;
        if dir.change_dir(CRED_DATA).is_err() {
            dir.make_dir_in_dir(CRED_DATA).ok()?;
            dir.change_dir(CRED_DATA).ok()?;
        }
        let f = dir.open_file_in_dir(IDX_FILE, Mode::ReadWriteCreateOrAppend).ok()?;
        f.write(short).ok()?;
        f.write(b"\t").ok()?;
        f.write(long).ok()?;
        f.write(b"\n").ok()?;
        f.flush().ok()?;
        Some(())
    })();
}

/// Load the whole index into `buf`; returns its length (0 if absent).
fn index_load<D: BlockDevice, T: TimeSource>(vm: &VolumeManager<D, T>, buf: &mut [u8]) -> usize {
    let mut n = 0usize;
    let _ = (|| -> Option<()> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let mut dir = vol.open_root_dir().ok()?;
        dir.change_dir(CRED_APP).ok()?;
        dir.change_dir(CRED_DATA).ok()?;
        let f = dir.open_file_in_dir(IDX_FILE, Mode::ReadOnly).ok()?;
        while n < buf.len() {
            match f.read(&mut buf[n..]).ok()? {
                0 => break,
                k => n += k,
            }
        }
        Some(())
    })();
    n
}

/// Find the long name mapped to an 8.3 `short` within a loaded index buffer.
fn index_lookup<'a>(idx: &'a [u8], short: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i < idx.len() {
        let line_end = idx[i..].iter().position(|&b| b == b'\n').map(|p| i + p).unwrap_or(idx.len());
        let line = &idx[i..line_end];
        if let Some(t) = line.iter().position(|&b| b == b'\t') {
            if &line[..t] == short {
                return Some(&line[t + 1..]);
            }
        }
        i = line_end + 1;
    }
    None
}

#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    Dhcp,
    Serving,
    NoLease,
}

pub struct ServeState {
    pub phase: Phase,
    pub ip: [u8; 4],
    pub hits: u32,
}

const ROOT: &str = "ECHO_ROOT"; // sentinel: path "" means the card root

// ---- saved WiFi credentials (/ECHO/DATA/WIFI.BIN) ----------------------
// Records are `ssid_len(1) | ssid | pw_len(1) | pw`, concatenated. Small + simple;
// the file holds up to ~10 networks.
const CRED_APP: &str = "ECHO";
const CRED_DATA: &str = "DATA";
const CRED_FILE: &str = "WIFI.BIN";
const CRED_BUF: usize = 1200;

/// Read saved credentials, calling `push(ssid, pw)` for each. Silent if absent.
pub fn load_creds<D: BlockDevice, T: TimeSource>(
    vm: &VolumeManager<D, T>,
    mut push: impl FnMut(&[u8], &[u8]),
) {
    let mut buf = [0u8; CRED_BUF];
    let mut n = 0usize;
    let ok = (|| -> Option<()> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let mut dir = vol.open_root_dir().ok()?;
        dir.change_dir(CRED_APP).ok()?;
        dir.change_dir(CRED_DATA).ok()?;
        let file = dir.open_file_in_dir(CRED_FILE, Mode::ReadOnly).ok()?;
        while n < buf.len() {
            match file.read(&mut buf[n..]).ok()? {
                0 => break,
                k => n += k,
            }
        }
        Some(())
    })();
    if ok.is_none() {
        return;
    }
    let mut i = 0;
    while i < n {
        let sl = buf[i] as usize;
        i += 1;
        if sl == 0 || i + sl > n {
            break;
        }
        let ssid_lo = i;
        i += sl;
        if i >= n {
            break;
        }
        let pl = buf[i] as usize;
        i += 1;
        if i + pl > n {
            break;
        }
        let (s0, s1, p0, p1) = (ssid_lo, ssid_lo + sl, i, i + pl);
        i += pl;
        push(&buf[s0..s1], &buf[p0..p1]);
    }
}

/// Add or update the credential for `ssid` (keeps the rest). Best-effort.
pub fn save_cred<D: BlockDevice, T: TimeSource>(vm: &VolumeManager<D, T>, ssid: &str, pw: &str) {
    // Rebuild the record buffer: existing entries (minus any with this ssid) + the new one.
    let mut out = [0u8; CRED_BUF];
    let mut w = 0usize;
    let mut put = |ssid: &[u8], pw: &[u8], out: &mut [u8], w: &mut usize| {
        if ssid.len() > 32 || pw.len() > 64 || *w + 2 + ssid.len() + pw.len() > out.len() {
            return;
        }
        out[*w] = ssid.len() as u8;
        *w += 1;
        out[*w..*w + ssid.len()].copy_from_slice(ssid);
        *w += ssid.len();
        out[*w] = pw.len() as u8;
        *w += 1;
        out[*w..*w + pw.len()].copy_from_slice(pw);
        *w += pw.len();
    };
    let ssid_b = ssid.as_bytes();
    load_creds(vm, |s, p| {
        if s != ssid_b {
            put(s, p, &mut out, &mut w);
        }
    });
    put(ssid_b, pw.as_bytes(), &mut out, &mut w);

    let _ = (|| -> Option<()> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let mut dir = vol.open_root_dir().ok()?;
        dir.change_dir(CRED_APP).ok()?;
        // ensure DATA exists (settings normally makes it, but be safe)
        if dir.change_dir(CRED_DATA).is_err() {
            dir.make_dir_in_dir(CRED_DATA).ok()?;
            dir.change_dir(CRED_DATA).ok()?;
        }
        let file = dir.open_file_in_dir(CRED_FILE, Mode::ReadWriteCreateOrTruncate).ok()?;
        file.write(&out[..w]).ok()?;
        file.flush().ok()?;
        Some(())
    })();
}

/// Serve until `tick(&ServeState)` returns false (the user aborts). `vm` is the
/// SD volume manager; `sys` is the dashboard's system page.
pub fn serve<D: BlockDevice, T: TimeSource>(
    sta: WifiIface<'static>,
    mac: [u8; 6],
    vm: &VolumeManager<D, T>,
    sys: &SysSnapshot,
    mut tick: impl FnMut(&ServeState) -> bool,
) -> ServeState {
    let mut device = WifiPhy::new(sta);
    let t0 = Instant::now();
    let now = |t0: Instant| SmolInstant::from_millis(t0.elapsed().as_millis() as i64);

    let mut cfg = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    cfg.random_seed = t0.duration_since_epoch().as_micros() | 1;
    let mut iface = Interface::new(cfg, &mut device, now(t0));

    let mut storage = [SocketStorage::EMPTY; 4];
    let mut sockets = SocketSet::new(&mut storage[..]);
    let dhcp_h = sockets.add(dhcpv4::Socket::new());

    let mut st = ServeState { phase: Phase::Dhcp, ip: [0; 4], hits: 0 };

    // ---- phase 1: DHCP lease (up to 12 s) ----
    loop {
        iface.poll(now(t0), &mut device, &mut sockets);
        if let Some(dhcpv4::Event::Configured(c)) = sockets.get_mut::<dhcpv4::Socket>(dhcp_h).poll() {
            iface.update_ip_addrs(|a| {
                a.clear();
                let _ = a.push(IpCidr::Ipv4(c.address));
            });
            st.ip = c.address.address().octets();
            if let Some(r) = c.router {
                let _ = iface.routes_mut().add_default_ipv4_route(r);
            }
            break;
        }
        if !tick(&st) {
            return st;
        }
        if t0.elapsed() >= Duration::from_secs(12) {
            st.phase = Phase::NoLease;
            return st;
        }
    }
    // Free the DHCP socket slot; we keep the lease (no renewal for a watched tool).
    sockets.remove(dhcp_h);
    st.phase = Phase::Serving;

    // ---- phase 2: single-socket HTTP server on :80 ----
    let mut http_rx = [0u8; 4096];
    let mut http_tx = [0u8; 4096];
    let mut http_sock = tcp::Socket::new(
        tcp::SocketBuffer::new(&mut http_rx[..]),
        tcp::SocketBuffer::new(&mut http_tx[..]),
    );
    http_sock.set_timeout(Some(smoltcp::time::Duration::from_secs(30)));
    let http_h = sockets.add(http_sock);
    let _ = sockets.get_mut::<tcp::Socket>(http_h).listen(80);

    let mut last_tick = Instant::now();
    loop {
        iface.poll(now(t0), &mut device, &mut sockets);

        // (re)arm the listener when idle
        {
            let s = sockets.get_mut::<tcp::Socket>(http_h);
            if !s.is_open() {
                let _ = s.listen(80);
            } else if s.state() == tcp::State::CloseWait {
                s.close();
            }
        }

        if sockets.get_mut::<tcp::Socket>(http_h).can_recv() {
            handle_request(http_h, &mut iface, &mut device, &mut sockets, t0, vm, sys);
            sockets.get_mut::<tcp::Socket>(http_h).close();
            st.hits = st.hits.wrapping_add(1);
        }

        if last_tick.elapsed() >= Duration::from_millis(150) {
            last_tick = Instant::now();
            if !tick(&st) {
                break;
            }
        }
    }
    sockets.get_mut::<tcp::Socket>(http_h).abort();
    st
}

type Net<'a, 'b> = (
    &'a mut Interface,
    &'a mut WifiPhy,
    &'a mut SocketSet<'b>,
);

/// Read the request head (until CRLFCRLF), parse it, and dispatch. Streaming
/// bodies (upload) read the rest straight off the socket.
fn handle_request<D: BlockDevice, T: TimeSource>(
    h: smoltcp::iface::SocketHandle,
    iface: &mut Interface,
    device: &mut WifiPhy,
    sockets: &mut SocketSet,
    t0: Instant,
    vm: &VolumeManager<D, T>,
    sys: &SysSnapshot,
) {
    // Accumulate the request head.
    let mut head = [0u8; 1024];
    let mut hlen = 0usize;
    let mut body_in_head = 0usize; // bytes already past CRLFCRLF sitting in `head`
    let mut head_end = 0usize;
    let start = Instant::now();
    loop {
        poll(iface, device, sockets, t0);
        let s = sockets.get_mut::<tcp::Socket>(h);
        if s.can_recv() {
            let got = s.recv_slice(&mut head[hlen..]).unwrap_or(0);
            hlen += got;
        }
        if let Some(p) = find4(&head[..hlen], b"\r\n\r\n") {
            head_end = p + 4;
            body_in_head = hlen - head_end;
            break;
        }
        if hlen >= head.len() || start.elapsed() >= Duration::from_secs(10) || !sockets.get_mut::<tcp::Socket>(h).may_recv() {
            break;
        }
    }
    if head_end == 0 {
        send_all(h, iface, device, sockets, t0, RESP_400);
        return;
    }

    // Parse: METHOD SP PATH SP HTTP, then headers.
    let line_end = find2(&head[..head_end], b"\r\n").unwrap_or(0);
    let line = &head[..line_end];
    let is_post = line.starts_with(b"POST");
    let p0 = line.iter().position(|&b| b == b' ').map(|i| i + 1).unwrap_or(0);
    let rest = &line[p0..];
    let p1 = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
    let raw_path = &rest[..p1];
    let (path, query) = split_query(raw_path);

    let content_len = header_usize(&head[..head_end], b"content-length:");
    let mut fname = [0u8; 64];
    let fname_len = header_value(&head[..head_end], b"x-filename:", &mut fname);

    // ---- routing ----
    if !is_post {
        if path == b"/" || path == b"/index.html" {
            send_headers(h, iface, device, sockets, t0, b"text/html", DASHBOARD.len());
            send_all(h, iface, device, sockets, t0, DASHBOARD);
        } else if path == b"/api/sys" {
            send_sys(h, iface, device, sockets, t0, sys);
        } else if path == b"/fs" {
            send_listing(h, iface, device, sockets, t0, vm, query);
        } else if path == b"/download" {
            send_file(h, iface, device, sockets, t0, vm, query);
        } else {
            send_all(h, iface, device, sockets, t0, RESP_404);
        }
        return;
    }

    // POST routes.
    if path == b"/upload" {
        let name = &fname[..fname_len];
        let ok = recv_to_file(
            h, iface, device, sockets, t0, vm, query, name,
            &head[head_end..hlen],
            content_len,
        );
        send_all(h, iface, device, sockets, t0, if ok { RESP_OK } else { RESP_500 });
    } else if path == b"/delete" {
        let ok = fs_delete(vm, query);
        send_all(h, iface, device, sockets, t0, if ok { RESP_OK } else { RESP_500 });
    } else if path == b"/mkdir" {
        let ok = fs_mkdir(vm, query);
        send_all(h, iface, device, sockets, t0, if ok { RESP_OK } else { RESP_500 });
    } else {
        // drain any small body then 404
        let _ = (body_in_head, content_len);
        send_all(h, iface, device, sockets, t0, RESP_404);
    }
}

// ----------------------------- responses --------------------------------

fn send_sys(
    h: smoltcp::iface::SocketHandle,
    iface: &mut Interface,
    device: &mut WifiPhy,
    sockets: &mut SocketSet,
    t0: Instant,
    s: &SysSnapshot,
) {
    let body = alloc::format!(
        "{{\"heap_free\":{},\"heap_used\":{},\"heap_total\":{},\"uptime\":{},\"batt\":{},\"mac\":\"{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}\",\"chip\":\"ESP32-S3\"}}",
        s.heap_free, s.heap_used, s.heap_total, s.uptime_s, s.batt_pct,
        s.mac[0], s.mac[1], s.mac[2], s.mac[3], s.mac[4], s.mac[5]
    );
    send_headers(h, iface, device, sockets, t0, b"application/json", body.len());
    send_all(h, iface, device, sockets, t0, body.as_bytes());
}

/// `GET /fs?path=...` — JSON `{"path":"..","entries":[{"name","size","dir"}..]}`.
fn send_listing<D: BlockDevice, T: TimeSource>(
    h: smoltcp::iface::SocketHandle,
    iface: &mut Interface,
    device: &mut WifiPhy,
    sockets: &mut SocketSet,
    t0: Instant,
    vm: &VolumeManager<D, T>,
    query: &[u8],
) {
    let mut pbuf = [0u8; 128];
    let path = url_path_param(query, &mut pbuf);

    // collect entries (short 8.3 names, so download/delete can re-open them)
    const MAXE: usize = 64;
    let mut names = [[0u8; 13]; MAXE];
    let mut nlens = [0u8; MAXE];
    let mut sizes = [0u32; MAXE];
    let mut dirs = [false; MAXE];
    let mut count = 0usize;

    let _ = (|| -> Option<()> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let mut dir = vol.open_root_dir().ok()?;
        for comp in PathIter::new(path) {
            dir.change_dir(comp).ok()?;
        }
        dir.iterate_dir(|e: &embedded_sdmmc::DirEntry| {
            if count >= MAXE || e.attributes.is_volume() {
                return;
            }
            let mut nm = [0u8; 13];
            let n = fmt_short(&e.name, &mut nm);
            if (n == 1 && nm[0] == b'.') || (n == 2 && nm[0] == b'.' && nm[1] == b'.') {
                return;
            }
            names[count] = nm;
            nlens[count] = n as u8;
            sizes[count] = e.size;
            dirs[count] = e.attributes.is_directory();
            count += 1;
        })
        .ok()?;
        Some(())
    })();

    // load the long-name sidecar once, then attach a "long" field per entry
    let mut idxbuf = [0u8; 2048];
    let idxn = index_load(vm, &mut idxbuf);

    let mut json = alloc::string::String::new();
    json.push_str("{\"path\":\"");
    json.push_str(core::str::from_utf8(path).unwrap_or(""));
    json.push_str("\",\"entries\":[");
    for i in 0..count {
        if i > 0 {
            json.push(',');
        }
        let short = &names[i][..nlens[i] as usize];
        let nm = core::str::from_utf8(short).unwrap_or("?");
        let long = index_lookup(&idxbuf[..idxn], short)
            .and_then(|l| core::str::from_utf8(l).ok())
            .unwrap_or(nm);
        json.push_str("{\"name\":\"");
        json.push_str(nm);
        json.push_str("\",\"long\":\"");
        for c in long.chars() {
            if c == '"' || c == '\\' {
                json.push('_');
            } else {
                json.push(c);
            }
        }
        json.push_str("\",\"size\":");
        json.push_str(&alloc::format!("{}", sizes[i]));
        json.push_str(",\"dir\":");
        json.push_str(if dirs[i] { "true" } else { "false" });
        json.push('}');
    }
    json.push_str("]}");

    send_headers(h, iface, device, sockets, t0, b"application/json", json.len());
    send_all(h, iface, device, sockets, t0, json.as_bytes());
}

/// `GET /download?path=...` — stream the file body in 512-byte chunks.
fn send_file<D: BlockDevice, T: TimeSource>(
    h: smoltcp::iface::SocketHandle,
    iface: &mut Interface,
    device: &mut WifiPhy,
    sockets: &mut SocketSet,
    t0: Instant,
    vm: &VolumeManager<D, T>,
    query: &[u8],
) {
    let mut pbuf = [0u8; 128];
    let path = url_path_param(query, &mut pbuf);
    // split into dir path + file name (last component)
    let (dirpath, name) = split_last(path);

    let opened = (|| -> Option<u32> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let mut dir = vol.open_root_dir().ok()?;
        for comp in PathIter::new(dirpath) {
            dir.change_dir(comp).ok()?;
        }
        let f = dir.open_file_in_dir(core::str::from_utf8(name).ok()?, Mode::ReadOnly).ok()?;
        Some(f.length())
    })();
    let total = match opened {
        Some(t) => t,
        None => {
            send_all(h, iface, device, sockets, t0, RESP_404);
            return;
        }
    };

    send_headers(h, iface, device, sockets, t0, b"application/octet-stream", total as usize);

    // stream: re-open + seek per refill keeps borrows simple (SD is the bottleneck).
    let mut off = 0u32;
    let mut chunk = [0u8; 512];
    while off < total {
        let mut got = 0usize;
        let _ = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let mut dir = vol.open_root_dir().ok()?;
            for comp in PathIter::new(dirpath) {
                dir.change_dir(comp).ok()?;
            }
            let f = dir.open_file_in_dir(core::str::from_utf8(name).ok()?, Mode::ReadOnly).ok()?;
            f.seek_from_start(off).ok()?;
            got = f.read(&mut chunk).ok()?;
            Some(())
        })();
        if got == 0 {
            break;
        }
        send_all(h, iface, device, sockets, t0, &chunk[..got]);
        off += got as u32;
        if !sockets.get_mut::<tcp::Socket>(h).may_send() {
            break;
        }
    }
}

/// Stream a POST body to an SD file: write the bytes already read with the head,
/// then keep recv'ing until `content_len` bytes are consumed.
fn recv_to_file<D: BlockDevice, T: TimeSource>(
    h: smoltcp::iface::SocketHandle,
    iface: &mut Interface,
    device: &mut WifiPhy,
    sockets: &mut SocketSet,
    t0: Instant,
    vm: &VolumeManager<D, T>,
    query: &[u8],
    fname: &[u8],
    initial: &[u8],
    content_len: usize,
) -> bool {
    let mut pbuf = [0u8; 128];
    let dirpath = url_path_param(query, &mut pbuf);
    // 8.3-coerce the filename (embedded-sdmmc 0.9 won't create LFN entries).
    let mut nm = [0u8; 13];
    let nlen = coerce_83(fname, &mut nm);
    let name = match core::str::from_utf8(&nm[..nlen]) {
        Ok(s) if !s.is_empty() => s,
        _ => return false,
    };

    let vol = match vm.open_volume(VolumeIdx(0)) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let mut dir = match vol.open_root_dir() {
        Ok(d) => d,
        Err(_) => return false,
    };
    for comp in PathIter::new(dirpath) {
        if dir.change_dir(comp).is_err() {
            return false;
        }
    }
    let file = match dir.open_file_in_dir(name, Mode::ReadWriteCreateOrTruncate) {
        Ok(f) => f,
        Err(_) => return false,
    };

    let mut written = 0usize;
    if !initial.is_empty() {
        if file.write(initial).is_err() {
            return false;
        }
        written += initial.len();
    }

    let mut buf = [0u8; 1024];
    let start = Instant::now();
    while written < content_len {
        poll(iface, device, sockets, t0);
        let s = sockets.get_mut::<tcp::Socket>(h);
        if s.can_recv() {
            let want = (content_len - written).min(buf.len());
            let got = s.recv_slice(&mut buf[..want]).unwrap_or(0);
            if got > 0 {
                if file.write(&buf[..got]).is_err() {
                    return false;
                }
                written += got;
                continue;
            }
        }
        if !sockets.get_mut::<tcp::Socket>(h).may_recv() && !sockets.get_mut::<tcp::Socket>(h).can_recv() {
            break;
        }
        if start.elapsed() >= Duration::from_secs(120) {
            break;
        }
    }
    let ok = file.flush().is_ok() && written >= content_len;
    if ok {
        // remember the original long name for this 8.3 file (Web UI display)
        index_append(vm, &nm[..nlen], fname);
    }
    ok
}

fn fs_delete<D: BlockDevice, T: TimeSource>(vm: &VolumeManager<D, T>, query: &[u8]) -> bool {
    let mut pbuf = [0u8; 128];
    let path = url_path_param(query, &mut pbuf);
    let (dirpath, name) = split_last(path);
    (|| -> Option<()> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let mut dir = vol.open_root_dir().ok()?;
        for comp in PathIter::new(dirpath) {
            dir.change_dir(comp).ok()?;
        }
        dir.delete_file_in_dir(core::str::from_utf8(name).ok()?).ok()?;
        Some(())
    })()
    .is_some()
}

fn fs_mkdir<D: BlockDevice, T: TimeSource>(vm: &VolumeManager<D, T>, query: &[u8]) -> bool {
    let mut pbuf = [0u8; 128];
    let path = url_path_param(query, &mut pbuf);
    let (dirpath, name) = split_last(path);
    (|| -> Option<()> {
        let vol = vm.open_volume(VolumeIdx(0)).ok()?;
        let mut dir = vol.open_root_dir().ok()?;
        for comp in PathIter::new(dirpath) {
            dir.change_dir(comp).ok()?;
        }
        dir.make_dir_in_dir(core::str::from_utf8(name).ok()?).ok()?;
        Some(())
    })()
    .is_some()
}

// ----------------------------- smoltcp I/O ------------------------------

fn poll(iface: &mut Interface, device: &mut WifiPhy, sockets: &mut SocketSet, t0: Instant) {
    let now = SmolInstant::from_millis(t0.elapsed().as_millis() as i64);
    iface.poll(now, device, sockets);
}

/// Send a whole slice, polling between writes so smoltcp actually flushes it.
fn send_all(
    h: smoltcp::iface::SocketHandle,
    iface: &mut Interface,
    device: &mut WifiPhy,
    sockets: &mut SocketSet,
    t0: Instant,
    data: &[u8],
) {
    let mut off = 0usize;
    let start = Instant::now();
    while off < data.len() {
        poll(iface, device, sockets, t0);
        let s = sockets.get_mut::<tcp::Socket>(h);
        if !s.may_send() {
            break;
        }
        if s.can_send() {
            match s.send_slice(&data[off..]) {
                Ok(n) => off += n,
                Err(_) => break,
            }
        }
        if start.elapsed() >= Duration::from_secs(20) {
            break;
        }
    }
    poll(iface, device, sockets, t0);
}

fn send_headers(
    h: smoltcp::iface::SocketHandle,
    iface: &mut Interface,
    device: &mut WifiPhy,
    sockets: &mut SocketSet,
    t0: Instant,
    ctype: &[u8],
    len: usize,
) {
    let hdr = alloc::format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        core::str::from_utf8(ctype).unwrap_or("application/octet-stream"),
        len
    );
    send_all(h, iface, device, sockets, t0, hdr.as_bytes());
}

// ----------------------------- parsing ----------------------------------

fn find2(hay: &[u8], pat: &[u8; 2]) -> Option<usize> {
    hay.windows(2).position(|w| w == pat)
}
fn find4(hay: &[u8], pat: &[u8; 4]) -> Option<usize> {
    hay.windows(4).position(|w| w == pat)
}

fn split_query(raw: &[u8]) -> (&[u8], &[u8]) {
    match raw.iter().position(|&b| b == b'?') {
        Some(i) => (&raw[..i], &raw[i + 1..]),
        None => (raw, &[]),
    }
}

/// Parse a `Content-Length`-style numeric header (case-insensitive name).
fn header_usize(head: &[u8], name_lc: &[u8]) -> usize {
    match header_line(head, name_lc) {
        Some(v) => {
            let mut n = 0usize;
            for &b in v {
                if b.is_ascii_digit() {
                    n = n * 10 + (b - b'0') as usize;
                }
            }
            n
        }
        None => 0,
    }
}

/// Copy a header's value into `out`, returns its length.
fn header_value(head: &[u8], name_lc: &[u8], out: &mut [u8]) -> usize {
    match header_line(head, name_lc) {
        Some(v) => {
            let n = v.len().min(out.len());
            out[..n].copy_from_slice(&v[..n]);
            n
        }
        None => 0,
    }
}

/// Find a header line by case-insensitive name and return its trimmed value.
fn header_line<'a>(head: &'a [u8], name_lc: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i < head.len() {
        let end = find2(&head[i..], b"\r\n").map(|p| i + p).unwrap_or(head.len());
        let line = &head[i..end];
        if line.len() > name_lc.len()
            && line[..name_lc.len()].eq_ignore_ascii_case(name_lc)
        {
            let mut v = &line[name_lc.len()..];
            while let [b' ', r @ ..] = v {
                v = r;
            }
            return Some(v);
        }
        i = end + 2;
        if end >= head.len() {
            break;
        }
    }
    None
}

/// Extract+decode the `path` query param (`?path=...&...`) into `out`; returns the
/// slice. An absent/empty path is the card root (empty slice).
fn url_path_param<'a>(query: &[u8], out: &'a mut [u8]) -> &'a [u8] {
    // find "path=" (it's the only param we use)
    let key = b"path=";
    let mut i = 0;
    let mut val: &[u8] = &[];
    while i < query.len() {
        let amp = query[i..].iter().position(|&b| b == b'&').map(|p| i + p).unwrap_or(query.len());
        let kv = &query[i..amp];
        if kv.len() >= key.len() && &kv[..key.len()] == key {
            val = &kv[key.len()..];
            break;
        }
        i = amp + 1;
    }
    url_decode(val, out)
}

fn url_decode<'a>(src: &[u8], out: &'a mut [u8]) -> &'a [u8] {
    let mut n = 0;
    let mut i = 0;
    while i < src.len() && n < out.len() {
        match src[i] {
            b'%' if i + 2 < src.len() => {
                let hi = hexval(src[i + 1]);
                let lo = hexval(src[i + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out[n] = (hi << 4) | lo;
                    i += 3;
                } else {
                    out[n] = src[i];
                    i += 1;
                }
            }
            b'+' => {
                out[n] = b' ';
                i += 1;
            }
            c => {
                out[n] = c;
                i += 1;
            }
        }
        n += 1;
    }
    // strip a leading '/'
    let s = &out[..n];
    if s.first() == Some(&b'/') {
        &out[1..n]
    } else {
        &out[..n]
    }
}

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Iterate the '/'-separated, non-empty components of a path.
struct PathIter<'a> {
    rest: &'a [u8],
}
impl<'a> PathIter<'a> {
    fn new(p: &'a [u8]) -> Self {
        PathIter { rest: p }
    }
}
impl<'a> Iterator for PathIter<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        while self.rest.first() == Some(&b'/') {
            self.rest = &self.rest[1..];
        }
        if self.rest.is_empty() {
            return None;
        }
        let end = self.rest.iter().position(|&b| b == b'/').unwrap_or(self.rest.len());
        let comp = &self.rest[..end];
        self.rest = &self.rest[end..];
        // reject "." and "..", keep traversal inside the card
        let s = core::str::from_utf8(comp).ok()?;
        if s == "." || s == ".." {
            return self.next();
        }
        Some(s)
    }
}

/// Split a path into (dir-part, last-component).
fn split_last(path: &[u8]) -> (&[u8], &[u8]) {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (&[], path),
    }
}

/// Format a `ShortFileName` as "BASE.EXT" into `out`; returns the length.
fn fmt_short(sfn: &embedded_sdmmc::ShortFileName, out: &mut [u8]) -> usize {
    let base = sfn.base_name();
    let ext = sfn.extension();
    let mut n = 0;
    for &b in base.iter().take(8) {
        if n < out.len() {
            out[n] = b;
            n += 1;
        }
    }
    if !ext.is_empty() {
        if n < out.len() {
            out[n] = b'.';
            n += 1;
        }
        for &b in ext.iter().take(3) {
            if n < out.len() {
                out[n] = b;
                n += 1;
            }
        }
    }
    n
}

/// Coerce an arbitrary upload filename to a valid uppercase 8.3 name.
fn coerce_83(src: &[u8], out: &mut [u8]) -> usize {
    let dot = src.iter().rposition(|&b| b == b'.');
    let (base, ext): (&[u8], &[u8]) = match dot {
        Some(d) => (&src[..d], &src[d + 1..]),
        None => (src, &[]),
    };
    let mut n = 0;
    for &b in base.iter().filter(|&&b| b.is_ascii_alphanumeric()).take(8) {
        out[n] = b.to_ascii_uppercase();
        n += 1;
    }
    if n == 0 {
        out[0] = b'F';
        n = 1;
    }
    if !ext.is_empty() {
        out[n] = b'.';
        n += 1;
        for &b in ext.iter().filter(|&&b| b.is_ascii_alphanumeric()).take(3) {
            out[n] = b.to_ascii_uppercase();
            n += 1;
        }
    }
    n
}

// ----------------------------- static page ------------------------------

const RESP_OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\nok";
const RESP_400: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_404: &[u8] = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_500: &[u8] = b"HTTP/1.1 500 Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

// Self-contained dashboard: system stats + a file manager (browse/download/
// upload/delete/mkdir). Sequential fetches, no external assets.
const DASHBOARD: &[u8] = include_bytes!("webui_dashboard.html");

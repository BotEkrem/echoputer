# Dev tools

Helpers for exercising the firmware against a PC instead of dedicated hardware.

## fakecam.py — IP-camera emulator for the Camera Finder

`fakecam.py` is a self-contained Python 3 HTTP server (standard library only — no pip
installs, no sidecar files) that pretends to be a network camera, so you can validate the
Hacking suite's **Camera Finder** end to end without owning a physical IP camera. It mirrors
exactly what `src/radio/camscan.rs` looks for:

| Camera Finder phase | What fakecam returns |
| --- | --- |
| sweep (find live HTTP host) | listens on the chosen port; any TCP+HTTP response = a live host |
| fingerprint / `classify()` | a brand `Server:` header (`Dahua…`, `GoAhead-Webs`, `App-webs/`) → the host is tagged as that brand (red camera blip) |
| auth detection | `401` + `WWW-Authenticate:` (Digest or Basic) → host flagged `auth_digest` / `auth_basic` |
| default-credential ladder | accepts `admin:admin` (the first pair in `CREDS`) → ladder succeeds on attempt #1 (green blip, `admin:admin` shown) |
| snapshot | serves a real JPEG (starts with the `FF D8 FF` SOI marker) on any non-root path → saved to the SD card as `SNAP.JPG` |
| PTZ | accepts Dahua `ptz.cgi` and Foscam-clone `decoder_control.cgi` GETs and logs the direction |

### Run

```bash
python3 tools/fakecam.py                   # Dahua  + Digest auth + ptz.cgi   (default; richest path)
python3 tools/fakecam.py --brand goahead   # GoAhead + Basic auth + decoder_control.cgi PTZ
python3 tools/fakecam.py --brand hikvision # Hikvision + Digest + ISAPI snapshot (PTZ shows n/a — expected)
python3 tools/fakecam.py --port 80         # port 80 needs sudo on macOS/Linux; 8080 (default) does not
python3 tools/fakecam.py --user admin --pass admin
```

The Cardputer probes ports **80 and 8080**, so the no-sudo default of `8080` is fine.

### Setup

1. Put the Cardputer and the machine running `fakecam.py` on the **same subnet**. The
   ESP32-S3 is 2.4 GHz-only and joins the AP as a station; the easiest controlled setup is a
   phone hotspot (enable *Maximize Compatibility* so it advertises 2.4 GHz) that both join —
   that keeps them on one `/24` and avoids dual-band routers handing out split subnets.
2. Start `fakecam.py` on the PC. Allow the incoming-connection prompt if your OS firewall asks.
3. Find the PC's IP on that subnet (`ipconfig getifaddr en0` on macOS, `ip addr` on Linux).
4. On the Cardputer: **Hacking → Camera Finder**, pick the AP, enter its password (or press
   `TAB` for the weak-password ladder). The sweep finds the PC, fingerprints it, runs the
   credential ladder, pulls a snapshot to `SNAP.JPG`, and (Dahua/GoAhead) enables `C` to drive PTZ.

### Expected

- **Device:** sweep counter climbs → a red camera blip (`cam 1`) → green blip with `admin:admin`
  (`pwn 1`) → done screen `1 cameras found` + `snapshot 2 KB` + `C: control camera`. Press `C`,
  then the arrow keys, for PTZ.
- **fakecam log:** `[401] … challenge` (fingerprint) → `[200] auth OK (admin:admin)` (ladder) →
  `[JPEG] snapshot …` → `[PTZ] Up/Down/Left/Right`.
- **SD card:** `SNAP.JPG` is a valid JPEG (the embedded "FAKE CAM" test image).

See the [Camera Tools](https://github.com/BotEkrem/echoputer/wiki/Camera-Tools) wiki page for
how the on-device side works.

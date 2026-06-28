# Crack-offload server (compute-offload "C" tier)

The PC side of the firmware's WPA offload, written in **Rust** (`std` only, no crates).
The Cardputer (ESP32-S3) cracks at only a few PBKDF2 guesses a second; a PC/GPU does
millions. So the device exports a captured handshake as a hashcat **`.22000`** line and
a real machine does the heavy cracking.

## Two ways to use it

**1. Manual (works today, no server needed).** After a capture (Handshake Capture or
Evil Twin), the firmware writes the handshake to the SD as **`HS22000.TXT`**. Pull the
card and crack on any PC:

```bash
hashcat -m 22000 HS22000.TXT rockyou.txt
```

**2. Server (`crack-server/`), live offload.** A std-only Rust HTTP service that accepts
the `.22000` line and returns the recovered passphrase. The device-side flow is fully
wired: configure the **Offload** server (IP/port/PSK + an uplink WiFi) in Settings or the
Web UI, and when an on-device crack misses, the firmware auto-joins the uplink, POSTs the
`.22000` signed with `X-Offload-Sig: HMAC-SHA256(PSK, body)` (the PSK never goes on the
wire), and shows the passphrase the server recovers. Works for both 4-way (`WPA*02*`) and
PMKID (`WPA*01*`) lines.

## Setup

**Recommended — a server box on the lab LAN** (`server-install.sh`, shipped as a release
asset): a `curl | sudo bash` one-liner that downloads the release binary, SHA256-verifies
it, installs `hashcat` + a wordlist, **generates and prints a PSK**, and runs a hardened
systemd service:

```bash
curl -fsSL <release>/server-install.sh | sudo bash
```

Put the printed **PSK** into the device's Offload settings (Settings or Web UI). With a
PSK set the server binds the LAN and every request must carry a valid `X-Offload-Sig`
HMAC over its body — a bad/missing signature gets a `403`. With no PSK it binds
`127.0.0.1` only, so it can never be accidentally exposed unauthenticated.

**Build from source instead:**

```bash
cd offload
./offload-install.sh        # installs hashcat, pins the crate to your host target, builds it
# then run (the installer prints the exact path):
OFFLOAD_KEY=<psk> BIND=0.0.0.0 crack-server/target/<host-triple>/release/crack-server rockyou.txt 8080
```

> The repo root pins Cargo to the **xtensa** target (for the firmware), so `crack-server`
> carries its own `.cargo/config.toml` overriding `build.target` back to the host. The
> installer regenerates it for your machine; the committed default is `aarch64-apple-darwin`.
> First build compiles host `std` from source (~30 s) because the root config's `build-std`
> is inherited (Cargo joins those arrays, so it can't be unset — we add `std` instead).

## Protocol

```
POST /crack            (any path accepted)
Content-Type: text/plain
X-Offload-Sig: <hex>   = HMAC-SHA256(PSK, body)   (required iff OFFLOAD_KEY is set)
<body> = one .22000 line:  WPA*02*<mic>*<ap>*<sta>*<essid>*<anonce>*<eapol>*00
                       (or WPA*01*<pmkid>*<ap>*<sta>*<essid>*** for a PMKID)

200 OK, text/plain
<body> = the passphrase   (empty body = not found in the wordlist)
403/503               = bad/missing signature / server busy
```

Test it without the device (signing the body the way the firmware does):

```bash
BODY=$(tr -d '\r\n' < HS22000.TXT)
SIG=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$PSK" | sed 's/^.*= //')
curl -s -H "X-Offload-Sig: $SIG" --data-binary "$BODY" http://localhost:8080/crack ; echo
```


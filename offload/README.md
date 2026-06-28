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

**2. Server (`crack-server/`).** A std-only Rust HTTP service that accepts the `.22000`
line over HTTP and returns the recovered passphrase — the target of the firmware's
`radio::http::build_post`. (The device-side *live* POST flow — associate to the server's
network, POST, read the result — is not wired into the firmware yet; the server is ready
and standalone-testable in the meantime.)

## Setup

```bash
cd offload
./offload-install.sh        # installs hashcat, pins the crate to your host target, builds it
# then run (the installer prints the exact path):
crack-server/target/<host-triple>/release/crack-server rockyou.txt 8080
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
<body> = one .22000 line:  WPA*02*<mic>*<ap_mac>*<sta_mac>*<essid>*<anonce>*<eapol>*00

200 OK, text/plain
<body> = the passphrase   (empty body = not found in the wordlist)
```

Test it without the device — pull `HS22000.TXT` off the SD and:

```bash
curl -s --data-binary @HS22000.TXT http://localhost:8080/crack ; echo
```


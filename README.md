# Echoputer (In Development)

A small operating system for the M5Stack Cardputer ADV, written in no_std Rust on
top of esp-hal. It brings up the hardware itself at boot and drops into a home
screen, and from there everything runs as a self-contained app: a keyboard synth,
an SD-card file browser, a battery/charge screen, settings, and a WiFi/BLE
security toolkit. There is no IDF app framework underneath. Echoputer is the whole
software stack on the device, from the drivers up to the apps it hosts.

It targets the *ADV* revision of the Cardputer, not the original. The ADV has a
TCA8418 I2C keyboard, an ES8311 I2C audio codec, and a separate SPI bus for the SD
card, so the pin map and drivers here will not match the older board.

Two things tie the apps together. Each one has its own accent colour, taken from
an evenly spaced hue wheel, so the home screen reads as a gradient and whatever
app you are in tints the top bar and the RGB LED to match. And the whole interface
is bilingual, English and Turkish, switchable live from Settings.

## Apps

Hacking is the security toolkit and has its own section below. The rest:

- Synthwave turns the keyboard into a small wavetable synth. Notes are quantised
  to a scale, with pitch rising left to right and bottom to top, so mashing keys
  still comes out musical. G0 cycles the scale and the LED pulses with the audio.
- File Browser walks the SD card and opens any file in a text or hex viewer it
  picks automatically. Deleting a file is real and asks first.
- Charge is a full-screen battery gauge for leaving the device on the charger;
  the ADV only charges while it is powered on.
- Settings holds the preferences, grouped into General, Synthwave and File
  Browser, and writes them to `/ECHO/DATA/CONFIG.BIN`. With no card inserted
  everything still works, the settings just don't persist across reboots.

## Hacking suite

A WiFi/BLE toolkit covering both passive recon and active attacks, with the
offensive tools sitting right next to the passive detector that flags them, so
you can watch an attack land and a defence catch it.

The tools live in one list, grouped by how much damage they can do: Basic (green)
for passive recon, Intermediate (orange) for noisy or lure-style activity, and
Advanced (red) for anything disruptive, capturing, or intrusive. The idea is that
someone new starts at the top and works down rather than opening the most
dangerous tool first. Picking a tool opens a small page with three choices: run
it, read its wiki (a short bilingual note on what it does, when it is used, and
how to defend against it), or open its settings if it has any. Anything offensive
is marked and asks for one confirmation before it fires, and a running attack
keeps going, with a live counter, until you press a key.

| Tier | Tool | What it does |
|------|------|--------------|
| Basic | WiFi Scanner | lists nearby access points with signal, channel and encryption |
| Basic | WiFi Analyzer | the same scan drawn as a 2.4 GHz channel-usage histogram |
| Basic | BLE Scanner | lists nearby Bluetooth LE devices |
| Basic | Deauth Detector | listens for deauth/disassoc frames, i.e. an attack in progress |
| Intermediate | Beacon Spam | floods fake beacons so bogus networks fill the air |
| Intermediate | Probe Flood | floods fake probe requests from phantom clients |
| Intermediate | BLE Spam | pairing-popup and junk-name advertising spam |
| Intermediate | Evil Twin | clones a real AP's SSID on its channel as a lure |
| Advanced | Deauth Flood | kicks every client off a target AP |
| Advanced | Handshake Capture | deauths a target, then sniffs the WPA handshake |
| Advanced | Evil Portal | open AP with DHCP/DNS hijack and a fake login page |
| Advanced | LAN Scan | joins an open network, gets a lease, port-scans the gateway |

Beacon Spam and Probe Flood let you choose the SSID source in their settings: a
random English set, a random Turkish set, or a custom prefix you type on the
keyboard that becomes NAME001, NAME002 and so on. The Evil Portal's AP name and
the BLE Spam mode are configurable the same way.

Under the hood, raw 802.11 frames go out through the sniffer's injection path and
BLE advertising is driven over the controller's HCI transport. The Evil Portal and
LAN Scan run a small TCP/IP stack (smoltcp, no allocator) over the SoftAP or
station interface, with the DHCP and DNS servers and the HTTP handling written by
hand. WiFi and BLE come up one at a time and the active one is torn down before
the other starts; running both alongside the framebuffer does not fit in the S3's
RAM, which showed up early on as a boot-time stack panic.

A separate self-test build (`cargo build --features selftest`) drives every tool
once over the serial port at boot, which is how the radio paths get exercised
without pressing keys.

Some things are deliberately left out because they don't belong on a no_std
Cardputer: SSH (no no_std crypto stack to lean on), the assorted host-protocol
attacks like SIP, UPnP, LDAP and NTLMv2/Responder (large, niche stacks), and an
IMSI catcher, which would need a cellular radio the chip simply doesn't have.

## Controls

Up and down move, ENTER selects, and left/right change a value where that makes
sense. Backspace steps back one level; the backtick key in the top-left corner
jumps straight to the home screen. The G0 button also steps back, except in
Synthwave where it cycles the scale and on the charge screen where it blanks the
display. The top bar shows battery percent and a fill icon on the right.

## Building and flashing

You need Espressif's Xtensa Rust toolchain and espflash:

```bash
cargo install espup --locked
espup install                 # installs the 'esp' toolchain + LLVM + GCC
. $HOME/export-esp.sh         # source this in every shell before building
cargo install espflash --locked
```

Build a release image and turn it into the app-only `.bin`:

```bash
. $HOME/export-esp.sh
cargo build --release
espflash save-image --chip esp32s3 \
  target/xtensa-esp32s3-none-elf/release/echoputer \
  echoputer.bin
```

To install it through bmorcelli's Launcher, drop `echoputer.bin` onto a FAT32 SD
card, open the Launcher's SD menu and install it from there. To skip the launcher
and flash straight over USB:

```bash
espflash flash --port /dev/cu.usbmodem* --baud 921600 --monitor \
  target/xtensa-esp32s3-none-elf/release/echoputer
```

## How it fits together

The source is split into layers: drivers at the bottom, a radio subsystem, and the
apps on top. `theme` and `i18n` are used everywhere, so they stay at the root.

```
src/
  main.rs       boot, hardware bring-up, the app state machine and key routing
  theme.rs      the shared visual language: colour tokens and widgets
  i18n.rs       inline English/Turkish strings, switched at runtime
  palette.rs    the per-app accent colours (an HSV hue wheel)
  selftest.rs   the serial self-test build (behind --features selftest)

  hal/          board drivers, the framebuffer and the keymap
    fb, battery, es8311, tca8418, ws2812, keymap

  radio/        the WiFi/BLE stack the Hacking app drives
    mod.rs        Radio, the sole owner of the WiFi+BLE peripherals
    wifi_frames   raw 802.11 frame builders
    ble_spam      BLE advertising payloads
    portal        the evil/captive portal (smoltcp + DHCP/DNS/HTTP)
    netscan       the LAN scanner

  apps/         the home screen and the apps
    menu, splash, synth, scales, ui, browser, charge, settings, hacking, wiki
```

## Hardware

| Block | Connection |
|-------|------------|
| Display, ST7789V2 240x135 | SPI2, SCK 36, MOSI 35, CS 37, DC 34, RST 33, backlight 38 |
| Keyboard, TCA8418 @ 0x34 | I2C, SDA 8, SCL 9 |
| Audio, ES8311 @ 0x18 + NS4150B | I2S0, BCLK 41, WS 43, DOUT 42 |
| SD card | SPI3, SCK 40, MOSI 14, MISO 39, CS 12, FAT32 |
| RGB LED, WS2812 | GPIO21 |
| Button, G0 | GPIO0 |
| Battery sense | ADC on GPIO10, 2:1 divider |

## Notes

- Long file names are shown, but files open by their short 8.3 name underneath.
- The SD bus runs at a fixed 400 kHz, which initialises reliably across cards but
  isn't tuned for throughput. Fine for browsing and previews.
- The radio tools only mean anything against real hardware, so their on-device
  behaviour (injection, capture, the portal's credential grab) is checked on the
  board rather than in a host test.

## Contributing

It's a personal project and a little opinionated, but patches and ideas are
welcome. For anything past a small fix, open an issue first so we can agree on the
shape of it before you write much code.

A few house rules:

- Keep everything `no_std`, and build with the esp toolchain
  (`cargo build --release`) before opening a PR. Match the style and comment
  density of the file you're working in.
- If you touch the radio code, the self-test build (`--features selftest`) is the
  quickest way to check it on real hardware over serial.
- New hacking tools should be gated behind the same confirmation and documented
  with a wiki entry, like the rest.

## License

MIT, see [LICENSE](LICENSE).

## Sources and inspiration

- [Evil-M5Project](https://github.com/7h30th3r0n3/Evil-M5Project) by 7h30th3r0n3
- [Launcher](https://github.com/bmorcelli/Launcher) by bmorcelli
- [AdvanceOS-for-cardputer](https://github.com/bomberman30/AdvanceOS-for-cardputer) by bomberman30

Built on the [esp-rs](https://github.com/esp-rs) ecosystem; the docs and crate
sources that came in most useful:

- [esp-hal](https://github.com/esp-rs/esp-hal) — the HAL everything sits on
- [esp-radio](https://crates.io/crates/esp-radio) — the WiFi/BLE stack
- [smoltcp](https://docs.rs/smoltcp) — the TCP/IP stack under the portal and scanner
- [bt-hci](https://docs.rs/bt-hci) — typed BLE HCI commands
- [embedded-graphics](https://docs.rs/embedded-graphics) — all the drawing
- IEEE 802.11, [DHCP (RFC 2131)](https://www.rfc-editor.org/rfc/rfc2131) and [DNS (RFC 1035)](https://www.rfc-editor.org/rfc/rfc1035) for the hand-rolled frames and servers

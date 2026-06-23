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

- REPL is an interactive shell over a small built-in interpreter. It's not a real
  language VM, just a no_std tree-walker for a familiar scripting subset: int,
  float, bool, str, list and dict; arithmetic, comparisons and `and`/`or`/`not`;
  `if`/`elif`/`else`, `while`, `for`, `def`/`return`; and builtins like `print`,
  `len` and `range`. Blocks are indented and run on a blank line, and `help`
  lists the syntax. A step budget and a recursion cap keep a bad script from
  hanging or crashing the device.
- Games is a small arcade launcher: Snake, 2048, Tetris and Pong. Pick one from
  the list and play; G0/Backspace steps back to the list, then to the home menu.
  They share one keyboard interface, so the arrow cluster steers/moves and ENTER
  acts (serve, hard-drop) where it applies. Built with `--features emu` (or
  `emugbc` for colour) it gains a fifth entry, **Game Boy**: an emulator over a
  vendored C core (Peanut-GB for DMG, Walnut-CGB for Game Boy Color). It lists the
  `.gb`/`.gbc` ROMs you drop in `/ECHO/ROMS/` and runs the selected one straight
  from the card — the ROM stays on the SD, read on demand through a 512-byte
  sector cache (the SD bus is re-clocked to 20 MHz so that isn't a crawl), scaled
  to the full panel, with sound through the codec (G0 cycles the volume in-game).
  The cartridge battery save is mirrored to a `.sav` next to the ROM, flushed
  periodically and on exit, so progress survives a power-cut. Controls: the arrow
  cluster is the D-pad, `l`/`k` are A/B, ENTER is Start, Space is Select; `` ` ``
  or Backspace leaves the game (saving first).
- Web UI puts the device on your WiFi and serves a dashboard you open from a PC.
  It scans for networks, you pick one and type the password (networks you've
  joined are remembered, so the password comes pre-filled next time), and once
  connected the screen shows the dashboard's IP. From a browser on the same
  network you get live system stats and full management of the SD card: browse,
  download, upload (including drag-and-drop), delete files and make folders. It's
  a hand-rolled HTTP server over smoltcp on the station interface — no SoftAP — and
  long filenames are preserved in the dashboard through a sidecar index even
  though the card itself stays 8.3.
- Player is an audio player for the `.wav` (and, with `--features player`, `.mp3`)
  files you drop in `/ECHO/MUSIC/`. WAV is decoded in pure Rust; MP3 uses the
  vendored minimp3 core. Every source is resampled to the firmware's native 16 kHz
  and played through the same I2S path the synth and emulator use — at the onboard
  speaker's bandwidth the difference from a higher rate is inaudible, and the shared
  audio pipeline stays untouched. ENTER (or G0) plays/pauses, left/right seek ±10 s,
  up/down change volume, `[`/`]` step to the previous/next track, and a track auto-
  advances to the next at its end. `` ` `` or Backspace leaves.
- Synthwave turns the keyboard into a small wavetable synth. Notes are quantised
  to a scale, with pitch rising left to right and bottom to top, so mashing keys
  still comes out musical. G0 cycles the scale and the LED pulses with the audio.
- File Browser walks the SD card and opens any file in a text or hex viewer it
  picks automatically. Deleting a file is real and asks first.
- Stopwatch is a stopwatch and a countdown timer in one (there is no wall clock —
  the ADV has no RTC battery, so time of day is left out on purpose). ENTER
  starts and pauses; left/right switch mode; up/down set the timer target.
- Notes is a tiny text editor over six SD slots under `/ECHO/NOTES/`. Pick a
  slot, type (ENTER inserts a newline, the `Aa` key toggles case), and it saves
  back to the card automatically when you leave the editor.
- Misc is a sub-launcher (like Games) that groups the small extra apps in a
  scrolling list: **Chip-8**, a tiny CHIP-8 interpreter that runs a ROM dropped at
  `/ECHO/CHIP8.CH8` (the `1234`/`qwer`/`asdf`/`zxcv` block maps to the hex keypad,
  arrows alias 2/4/6/8); **Calc**, an immediate-execution calculator (the `=`/`+`
  key adds, ENTER is equals, `x` multiplies); **Convert**, a unit converter across
  length, mass, temperature and data; **Dice**, dice presets (d4–d100 and a coin)
  plus a custom "random between X and Y" range you type in; **QR**, which turns
  typed text — a URL, a WiFi string — into a scannable QR code on the panel; **IR**,
  a transmit-only remote that fires NEC codes from the onboard IR LED on GPIO44
  (a few well-known TV-power presets plus a custom 32-bit code entered in hex; aim
  the top edge at the device); and, on every build except `emugbc`, **Mic**, a
  recorder that captures the onboard microphone to a WAV at `/ECHO/REC0.WAV` (the
  colour build leaves it out — its RAM is too tight to add the capture buffer next
  to the radio). `` ` ``/Backspace steps back to the list, then to the home menu.
- Charge is a full-screen battery gauge for leaving the device on the charger;
  the ADV only charges while it is powered on.
- Settings holds the preferences, grouped into General, Synthwave and File
  Browser, and writes them to `/ECHO/DATA/CONFIG.BIN`. With no card inserted
  everything still works, the settings just don't persist across reboots.
- System is a read-only monitor: free/used heap, uptime, battery, MAC address
  and chip, refreshed once a second.

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
Synthwave where it cycles the scale, in the Game Boy emulator where it cycles the
volume, and on the charge screen where it blanks the display. The top bar shows
battery percent and a fill icon on the right.

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

### Cargo features

These are compile-time Cargo features — flags you pass to the build
(`cargo build --release --features …`), not options you toggle on the device. All are
off by default; the emulator and MP3 decoder compile the vendored C cores in `vendor/`
(so they need the Xtensa GCC `espup` installs) and aren't worth the RAM otherwise.
They combine freely — the two cores' big buffers are heap-allocated only
while in use, and the emulator and Player never run at once, so neither permanently
reserves the RAM the boot stack needs.

| Feature | What it adds |
|---------|--------------|
| *(default)* | base firmware: WAV-only Player, no Game Boy. Pure Rust, no C. |
| `player` | MP3 decode in the Player (vendors minimp3). |
| `emu` | Game Boy emulator, monochrome (Peanut-GB). Clean in-game audio. |
| `emugbc` | Game Boy Color (Walnut-CGB): the colour palette, but the heavier core runs ~27 fps off the SD card so its in-game audio is choppy — the DMG (`emu`) build sounds clean. The colour core's RAM use also drops the Misc **Mic** recorder from this build (its capture buffer won't fit beside the radio). |
| `emutest` | boot-time serial self-test of the emulator core (implies `emu`). |
| `selftest` | boot-time serial self-test of every radio tool. |
| `audiodiag` | logs I2S audio health (throughput, underruns) over serial once a second, for debugging the audio path. |

Combine them as you like — `cargo build --release --features emugbc,player` is the
full build. You don't have to build any of these yourself, though: every push to
`main` builds all six and publishes them as a GitHub Release tagged with that commit,
so every build is kept and you can tell exactly which one you have. The newest is
always the repo's [latest release](../../releases/latest) (each is a per-run CI
artifact too). Grab the `.bin` whose name lists the features you want (`echoputer-base`,
`echoputer-mp3`, `echoputer-gameboy`, `echoputer-gameboy-mp3`, `echoputer-gameboy-color`,
`echoputer-gameboy-color-mp3`) and flash it as above.

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
    fb, battery, es8311, tca8418, ws2812, ir (IR-LED NEC transmitter), keymap

  radio/        the WiFi/BLE stack the Hacking and Web UI apps drive
    mod.rs        Radio, the sole owner of the WiFi+BLE peripherals
    wifi_frames   raw 802.11 frame builders
    ble_spam      BLE advertising payloads
    portal        the evil/captive portal (smoltcp + DHCP/DNS/HTTP)
    netscan       the LAN scanner
    webui         the Web UI station dashboard (smoltcp HTTP + SD file mgmt)

  apps/         the home screen and the apps
    menu, splash, repl, games (snake, g2048, tetris, pong), stopwatch, notes,
    sysinfo, synth, scales, ui, browser, webui, charge, settings, hacking, wiki
    misc          the Misc sub-launcher + its apps: chip8, calc, convert, dice,
                  qr (+ qr_encode), ir (NEC remote); recorder (mic -> WAV) off emugbc
    emu/          the Game Boy emulator (mod, ffi, rom, input, video), behind
                  --features emu; the vendored C cores live in vendor/
    player/       the audio player (mod, wav, resample; mp3 behind --features
                  player) — WAV/MP3 resampled to 16 kHz onto the shared I2S path
```

## Hardware

| Block | Connection |
|-------|------------|
| Display, ST7789V2 240x135 | SPI2, SCK 36, MOSI 35, CS 37, DC 34, RST 33, backlight 38 |
| Keyboard, TCA8418 @ 0x34 | I2C, SDA 8, SCL 9 |
| Audio, ES8311 @ 0x18 + NS4150B | I2S0, BCLK 41, WS 43, DOUT 42; DIN 46 (mic ADC, off emugbc) |
| SD card | SPI3, SCK 40, MOSI 14, MISO 39, CS 12, FAT32 |
| RGB LED, WS2812 | GPIO21 (RMT ch0) |
| IR transmitter | GPIO44 (RMT ch1), 38 kHz NEC, transmit-only |
| Button, G0 | GPIO0 |
| Battery sense | ADC on GPIO10, 2:1 divider |

## Notes

- Long file names are shown, but files open by their short 8.3 name underneath.
- The SD bus initialises at 400 kHz (the SD spec needs the handshake slow) and is
  then re-clocked to 20 MHz, so file transfers (Web UI uploads) and emulator ROM
  reads aren't crippled by the slow init clock.
- The radio tools only mean anything against real hardware, so their on-device
  behaviour (injection, capture, the portal's credential grab) is checked on the
  board rather than in a host test.
- Heap budget (no PSRAM, 512 KB SRAM): the WiFi/BLE radio keeps part of the heap for
  the session once it's used, and the Player's MP3 decode (~43 KB) and the Game Boy
  emulator each want a big chunk of that same heap. So opening the MP3 player or the
  emulator *after* using Web UI / Hacking in the same session can report "low memory"
  — reboot to reclaim it. WAV playback and everything else are unaffected, and on a
  fresh boot it all just works.

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
- [Peanut-GB](https://github.com/deltabeard/Peanut-GB) by deltabeard — the Game Boy
  (DMG) core and its companion `minigb_apu` sound emulator that the Game Boy app
  embeds; the colour (`emugbc`) build uses a Game Boy Color fork of it
- [minimp3](https://github.com/lieff/minimp3) by lieff — the public-domain (CC0)
  single-header MP3 decoder the Player embeds for `.mp3` (behind `--features player`)

Built on the [esp-rs](https://github.com/esp-rs) ecosystem; the docs and crate
sources that came in most useful:

- [esp-hal](https://github.com/esp-rs/esp-hal) — the HAL everything sits on
- [esp-radio](https://crates.io/crates/esp-radio) — the WiFi/BLE stack
- [smoltcp](https://docs.rs/smoltcp) — the TCP/IP stack under the portal and scanner
- [bt-hci](https://docs.rs/bt-hci) — typed BLE HCI commands
- [embedded-graphics](https://docs.rs/embedded-graphics) — all the drawing
- IEEE 802.11, [DHCP (RFC 2131)](https://www.rfc-editor.org/rfc/rfc2131) and [DNS (RFC 1035)](https://www.rfc-editor.org/rfc/rfc1035) for the hand-rolled frames and servers

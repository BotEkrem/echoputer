//! Echoputer — a small operating system for the M5Stack Cardputer ADV.
//!
//! Brings up the hardware at boot, then drops into a home screen you launch apps
//! from: Hacking (WiFi/BLE recon + attack tools), Synthwave, File Browser, Charge,
//! Settings. Everything runs bare-metal on the ESP32-S3 — no IDF app framework.
//! Controls: home screen ↑/↓ + ENTER. ESC (the ` top-left key) goes BACK one level.
//! Backspace is a normal key (text apps delete with it). G0 toggles the screen to sleep
//! (any key wakes it) — except where it has a real function (Synthwave: cycle the mode;
//! Player/Emu: playback; Remote: USB/BLE toggle).
//!
//! Hardware (Cardputer ADV):
//!   Display  ST7789V2 240x135  SPI2: SCK=36 MOSI=35 CS=37 DC=34 RST=33 BL=38(PWM)
//!   Keyboard TCA8418  @ I2C 0x34            (internal I2C bus SDA=8 SCL=9)
//!   Audio    ES8311   @ I2C 0x18 + NS4150B; I2S0: BCLK=41 WS=43 DOUT=42
//!   SD card  SPI3: SCK=40 MOSI=14 MISO=39 CS=12
//!   LED      WS2812 on GPIO21       Button G0 = GPIO0

#![no_std]
#![no_main]

// Module tree (src/ is grouped into hal/ radio/ apps/; theme + i18n + radio's
// `Radio` are reached by their full paths — `crate::radio::Radio`, etc.):
mod hal; // board drivers + framebuffer + keymap
mod radio; // WiFi/BLE stack + attack/portal/scan payloads
mod apps; // launcher + the app screens
mod config; // central user-settings struct (load/save); edited by apps::settings
mod i18n;
mod palette; // per-app accent colours (HSV hue wheel)
mod theme;
#[cfg(feature = "selftest")]
mod selftest;

// main's own imports of the grouped submodules it drives (hal/ radio/ apps/).
// Everything else is reached by full path: crate::radio::Radio, crate::theme, etc.
use crate::apps::{
    browser, charge, games, hacking, menu, misc, notes, player, repl, scales, settings, splash, stopwatch, synth,
    sysinfo, ui, webui,
};
use crate::hal::{battery, bmi270, es8311, fb, ir, tca8418, usb_hid, ws2812};
use crate::i18n::app;
use crate::radio::portal;

use esp_backtrace as _;
extern crate alloc; // esp-radio scan APIs return alloc::vec::Vec

// ESP-IDF app descriptor — required by espflash/the bootloader to validate the image.
esp_bootloader_esp_idf::esp_app_desc!();

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

use esp_hal::{
    analog::adc::{Adc, AdcCalCurve, AdcConfig, AdcPin, Attenuation},
    clock::CpuClock,
    delay::Delay,
    gpio::{DriveMode, Input, InputConfig, Level, Output, OutputConfig, Pull},
    interrupt::software::SoftwareInterruptControl,
    i2c::master::{Config as I2cConfig, I2c},
    i2s::master::{Channels, Config as I2sConfig, DataFormat, I2s},
    ledc::{
        channel::{self, ChannelIFace},
        timer::{self, TimerIFace},
        Ledc, LSGlobalClkSource, LowSpeed,
    },
    main,
    rmt::{Rmt, TxChannelConfig, TxChannelCreator},
    spi::{
        master::{Config as SpiConfig, Spi},
        Mode,
    },
    time::{Duration, Instant, Rate},
    timer::timg::TimerGroup,
};

use embedded_hal_bus::spi::ExclusiveDevice;
use embedded_sdmmc::{SdCard, VolumeManager};
use mipidsi::{
    interface::SpiInterface,
    models::ST7789,
    options::{ColorInversion, Orientation, Rotation},
    Builder,
};

use scales::Mode as ScaleMode;

/// Off-screen framebuffer backing store (rendered into, then blitted in one pass).
static mut FB_DATA: [Rgb565; fb::W * fb::H] = [Rgb565::new(0, 0, 0); fb::W * fb::H];

/// Root note (C3) of the playable range.
const ROOT_MIDI: u8 = 48;
/// Audio render chunk: 256 stereo frames (1024 bytes).
const CHUNK_FRAMES: usize = 256;

// Named keys (logical row, col).
const K_HOME: (u8, u8) = (0, 0); // the ` (top-left / ESC) key: go BACK one level
pub(crate) const K_UP: (u8, u8) = (2, 11);
pub(crate) const K_DOWN: (u8, u8) = (3, 11);
pub(crate) const K_LEFT: (u8, u8) = (3, 10);
pub(crate) const K_RIGHT: (u8, u8) = (3, 12);
pub(crate) const K_ENTER: (u8, u8) = (2, 13);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Menu,
    Repl,
    Games,
    #[cfg(feature = "emu")]
    Emu,
    Stopwatch,
    Sysinfo,
    Notes,
    Misc,
    Synth,
    WebUi,
    Player,
    Browser,
    Settings,
    Charge,
    Hacking,
}

fn key_to_degree(row: u8, col: u8, mode: ScaleMode) -> usize {
    let len = mode.intervals().len();
    let octave = (3 - row.min(3)) as usize;
    let deg_in_row = (col.min(13) as usize * len) / 14;
    octave * len + deg_in_row
}

fn status_dots<D: DrawTarget<Color = Rgb565>>(d: &mut D, audio_ok: bool, kbd_ok: bool) {
    theme::fill(d, theme::W - 17, 125, 5, 5, if audio_ok { theme::accent() } else { theme::DESTRUCTIVE });
    theme::fill(d, theme::W - 9, 125, 5, 5, if kbd_ok { theme::accent() } else { theme::DESTRUCTIVE });
}

/// Backlight duty percent from a 1..=10 brightness setting (floored so the
/// screen can never be fully blacked out).
fn bl_pct(b: u8) -> u8 {
    ((b.clamp(1, 10) as u16) * 10).max(15) as u8
}

/// User brightness for the onboard WS2812 (0.0..=1.0). The LED shares the LCD
/// backlight's power rail and (per M5's docs) is only powered cleanly at FULL
/// backlight; at any reduced screen brightness the dimmed backlight ripples the
/// rail and the LED flickers. So we drive it only at full screen brightness and
/// hold it dark otherwise — an off LED can't flicker. Returns 0.0 when off/dimmed.
pub(crate) fn led_brightness(led_on: bool, led_bright: u8, disp_bright: u8) -> f32 {
    if led_on && disp_bright >= 10 {
        led_bright as f32 / 10.0
    } else {
        0.0
    }
}

/// Keep the selected item within the visible scroll window.
fn clamp_scroll(sel: usize, scroll: usize, visible: usize) -> usize {
    if sel < scroll {
        sel
    } else if sel >= scroll + visible {
        sel + 1 - visible
    } else {
        scroll
    }
}

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // Diagnostic: probe external PSRAM and report its size, then continue. Psram::new
    // safely returns a 0-byte range if no chip is present (no hang). Decides the
    // emulator's ROM-storage strategy (PSRAM-resident vs flash partition).
    #[cfg(feature = "psramprobe")]
    {
        let psram =
            esp_hal::psram::Psram::new(peripherals.PSRAM, esp_hal::psram::PsramConfig::default());
        let (addr, size) = psram.raw_parts();
        esp_println::println!(
            "\n>>> PSRAM PROBE: {} bytes ({} KB / {} MB) @ {:p} <<<\n",
            size,
            size / 1024,
            size / (1024 * 1024),
            addr
        );
    }

    // Heap for the esp-radio (WiFi/BLE) stack used by the Hacking menu. This only
    // reserves static RAM; nothing allocates until the radio is initialised, so it is
    // safe to set up at boot. Must exist before esp_rtos::start (scheduler) runs.
    //
    // 72 KB is enough for ONE radio at a time. WiFi+BLE *coexistence* does not fit:
    // the framebuffer is a 64 KB static, and a heap big enough for both stacks
    // (~144 KB) starved the boot stack (SP-out-of-range panic). So `radio.rs` keeps
    // the radios mutually exclusive — it frees one before bringing up the other —
    // which keeps the working set inside 72 KB.
    esp_alloc::heap_allocator!(size: 72 * 1024);

    let mut delay = Delay::new();

    // ---------------- Backlight PWM (LEDC on GPIO38) ----------------
    let mut ledc = Ledc::new(peripherals.LEDC);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);
    let mut bl_timer = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    bl_timer
        .configure(timer::config::Config {
            // 10-bit, NOT 8-bit: on the 80 MHz APB clock, 256 Hz at 8-bit needs a
            // divider (~1221) beyond the LEDC max (~1024), so `configure` returns
            // Err(Divisor) and the unwrap PANICS at boot -> black screen. 10-bit needs
            // div ~305 (in range). set_duty() takes a percentage, so the resolution
            // change does not affect any call site.
            duty: timer::config::Duty::Duty10Bit,
            clock_source: timer::LSClockSource::APBClk,
            // 1200 Hz: the onboard WS2812 shares this backlight's power rail, and a
            // low PWM frequency (~256 Hz, near the LED's ~400 Hz response) ripples
            // that rail and makes the LED flicker when the screen is dimmed. 1200 Hz
            // sits well above that response so the ripple averages out, while staying
            // far below the ~5 kHz at which this panel collapsed to black above ~80%
            // duty. Divider stays in range (~65 at 10-bit), so configure() won't panic.
            frequency: Rate::from_hz(1200),
        })
        .unwrap();
    let mut backlight = ledc.channel(channel::Number::Channel0, peripherals.GPIO38);
    backlight
        .configure(channel::config::Config {
            timer: &bl_timer,
            duty_pct: 0, // start DARK; raised only after the panel is cleared (no boot flash)
            drive_mode: DriveMode::PushPull,
        })
        .unwrap();

    // ---------------- Display: SPI2 + ST7789 ----------------
    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default().with_frequency(Rate::from_mhz(40)).with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO36)
    .with_mosi(peripherals.GPIO35);

    let dc = Output::new(peripherals.GPIO34, Level::Low, OutputConfig::default());
    let cs = Output::new(peripherals.GPIO37, Level::High, OutputConfig::default());
    let rst = Output::new(peripherals.GPIO33, Level::High, OutputConfig::default());

    let spi_device = ExclusiveDevice::new(spi, cs, delay).unwrap();
    // mipidsi chunks the 64,800-byte frame through this staging buffer; 4 KB cuts
    // a full blit from ~127 SPI transactions to ~16 (byte-identical output).
    // Heap, not main()'s stack frame: on the RAM-tight emugbc build the CPU0 stack is
    // only ~51 KB (leftover RWDATA after the colour core's big statics), and main()'s
    // large standing frame is what tips the Web UI serve path into a stack overflow.
    // (Heap, not a `static`: a static grows .bss and shrinks that leftover stack 1:1.)
    let mut spi_buf = alloc::vec![0u8; 4096].into_boxed_slice();
    let di = SpiInterface::new(spi_device, dc, &mut spi_buf[..]);

    let mut display = Builder::new(ST7789, di)
        .display_size(135, 240)
        .display_offset(52, 40)
        .invert_colors(ColorInversion::Inverted)
        .orientation(Orientation::new().rotate(Rotation::Deg90))
        .reset_pin(rst)
        .init(&mut delay)
        .unwrap();

    // The panel powers up with random GRAM. Clear it to black with the backlight still
    // OFF, so the first thing the user ever sees is black — not the white/garbled
    // "lost signal" flash that the uninitialised framebuffer shows otherwise.
    let _ = display.clear(Rgb565::BLACK);

    // ---------------- SD card (SPI3) + config (BEFORE intro so saved prefs apply) ----------------
    let sd_spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default().with_frequency(Rate::from_khz(400)).with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(peripherals.GPIO40)
    .with_mosi(peripherals.GPIO14)
    .with_miso(peripherals.GPIO39);
    let sd_cs = Output::new(peripherals.GPIO12, Level::High, OutputConfig::default());
    let sd_dev = ExclusiveDevice::new(sd_spi, sd_cs, Delay::new()).unwrap();
    let sdcard = SdCard::new(sd_dev, Delay::new());
    let vm = VolumeManager::new(sdcard, browser::DummyTimeSource);

    // The SD bus MUST initialise at <=400 kHz, but that clock cripples everything
    // after (emulator bank reads were ~0.3 s each -> games crawl; uploads ~50 KB/s).
    // Force the card to acquire at 400 kHz, then re-clock SPI3 up to 20 MHz for all
    // subsequent transfers (the card supports 25 MHz in SPI mode; the display bus
    // already runs at 40 MHz, so the board handles it).
    let _ = vm.device(|sd| {
        let _ = sd.num_bytes(); // force the card to acquire (init) at 400 kHz
        sd.spi(|dev| {
            let _ = dev.bus_mut().apply_config(
                &SpiConfig::default().with_frequency(Rate::from_mhz(20)).with_mode(Mode::_0),
            );
        });
        browser::DummyTimeSource // VolumeManager::device requires the closure to return T
    });

    let mut config = crate::config::Config::new();
    config.load(&vm); // best-effort: no card / no file -> defaults
    config.apply_lang(); // set UI language before the first frame is drawn
    // (accent is set per-app from the palette wheel — see menu::draw / app entry)
    let _ = backlight.set_duty(bl_pct(config.disp_bright));

    // ---------------- Battery ADC (GPIO10 on ADC1, curve-calibrated -> mV) ----------------
    let mut adc_config = AdcConfig::new();
    let mut bat_pin: AdcPin<_, _, AdcCalCurve<_>> =
        adc_config.enable_pin_with_cal(peripherals.GPIO10, Attenuation::_11dB);
    let mut adc = Adc::new(peripherals.ADC1, adc_config);
    {
        let mv = adc.read_blocking(&mut bat_pin) as u32 * 2; // 2:1 divider
        let present = (3000..=4500).contains(&mv);
        battery::set(battery::mv_to_percent(mv as u16), present);
    }

    // ---------------- Onboard WS2812 LED (GPIO21) ----------------
    // Set up + clear the LED BEFORE the splash: the WS2812 powers on to a stray
    // (often white) pixel, and if we only drove it from the main loop it would glow
    // through boot/splash even when the LED is meant to be off at low brightness.
    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(80)).unwrap();
    let mut led = rmt
        .channel0
        // Actively drive GPIO21 LOW between frames (idle_output). The WS2812 wants
        // its data line resting low; the default leaves it released, which on the
        // Cardputer's shared LED/backlight rail invites spurious re-latching (flicker).
        .configure_tx(
            &TxChannelConfig::default()
                .with_clk_divider(1)
                .with_idle_output(true)
                .with_idle_output_level(Level::Low),
        )
        .unwrap()
        .with_pin(peripherals.GPIO21);
    // Clear the power-on pixel now; the main loop then drives it per the brightness
    // setting (accent wave only at full brightness, off otherwise).
    led = match led.transmit(&ws2812::encode(0, 0, 0)) {
        Ok(tx) => tx.wait().unwrap_or_else(|(_, c)| c),
        Err((_, c)) => c,
    };

    // ---------------- IR transmitter (GPIO44) ----------------
    // CRITICAL: GPIO44 is the ROM UART0-RX (U0RXD) console pin. The boot/UART-detach path can
    // leave its RTC digital PAD-HOLD latched, which FREEZES the pad at its boot drive — so every
    // later GPIO/LEDC/RMT output write looks correct in the registers (RMT even returns TX-OK) but
    // NOTHING reaches the pin (camera-confirmed zero IR emission; the sibling GPIO43=U0TXD works
    // because it isn't held). esp-hal never clears a digital pad-hold, so we do it ourselves here,
    // before configuring GPIO44 — release all digital pad-holds + force-unhold.
    // SAFETY: RTC_CNTL is otherwise idle at this point and we rely on no pin being held.
    unsafe {
        let rtc = &*esp32s3::RTC_CNTL::PTR;
        rtc.dig_pad_hold().write(|w| w.dig_pad_hold().bits(0));
        rtc.dig_iso().modify(|_, w| w.dg_pad_force_unhold().set_bit());
    }
    #[cfg(not(feature = "irtest"))]
    let ir_tx = ir::IrTx::new(
        rmt.channel1
            .configure_tx(&ir::tx_config())
            .unwrap()
            .with_pin(peripherals.GPIO44),
    );

    // Boot-time IR self-test (`--features irtest`): drive GPIO44 with the LEDC peripheral — a
    // HARDWARE carrier (like the proven Ultimate-Remote firmware) — and software-gate the mark/
    // space envelope. SWEEPS carrier frequency x protocol/code forever, logging each over serial
    // AND showing "freq / code" on screen. Point the Cardputer at a TV; when it toggles, read the
    // on-screen freq+code (or the latest IRTEST log). Diverges — the `!` coerces to IrTx so the
    // (now unreachable) app below still type-checks; GPIO44 is consumed only here.
    #[cfg(feature = "irtest")]
    let ir_tx: ir::IrTx = {
        use esp_hal::gpio::Pin as _; // for .degrade()
        use ir::Protocol::{Nec, Rc5, Rc6, Sony};
        let freqs: [(u32, &str); 4] = [(36_000, "36k"), (38_000, "38k"), (40_000, "40k"), (33_000, "33k")];
        let codes: &[(ir::Protocol, u32, &str)] = &[
            (Rc5, 0x0C, "RC5.0C"),
            (Rc5, 0x20, "RC5.20"),
            (Rc6, 0x0C, "RC6.0C"),
            (Nec, 0xE0E0_40BF, "NEC.SAM"),
            (Nec, 0x20DF_10EF, "NEC.LG"),
            (Sony, 0x0115, "SONY"),
        ];
        let mut ir_timer = ledc.timer::<LowSpeed>(timer::Number::Timer1);
        let mut gpio44 = peripherals.GPIO44.degrade();
        esp_println::println!("=== IR SELFTEST LEDC sweep: {} freqs x {} codes ===", freqs.len(), codes.len());
        let mut n = 0usize;
        loop {
            for &(freq, flabel) in freqs.iter() {
                ir_timer
                    .configure(timer::config::Config {
                        duty: timer::config::Duty::Duty8Bit,
                        clock_source: timer::LSClockSource::APBClk,
                        frequency: Rate::from_hz(freq),
                    })
                    .unwrap();
                let mut ir_ch = ledc.channel(channel::Number::Channel1, gpio44.reborrow());
                ir_ch
                    .configure(channel::config::Config {
                        timer: &ir_timer,
                        duty_pct: 0,
                        drive_mode: DriveMode::PushPull,
                    })
                    .unwrap();
                for &(proto, code, clabel) in codes.iter() {
                    esp_println::println!("IRTEST #{} {}Hz {} code={:#010X}", n, freq, clabel, code);
                    theme::clear(&mut display);
                    theme::topbar(&mut display, "IR SELFTEST - watch TV");
                    theme::text_center(&mut display, flabel, theme::W / 2, 34, theme::TITLE_FONT, theme::accent());
                    theme::text_center(&mut display, clabel, theme::W / 2, 78, theme::TITLE_FONT, theme::FG);
                    let mut buf = [(0u16, 0u16); ir::MAX_PAIRS];
                    let cnt = ir::frame_pairs(proto, code, n & 1 == 0, &mut buf);
                    for _ in 0..3 {
                        for &(mark, space) in &buf[..cnt] {
                            if mark > 0 {
                                let _ = ir_ch.set_duty(30);
                                delay.delay_micros(mark as u32);
                            }
                            let _ = ir_ch.set_duty(0);
                            if space > 0 {
                                delay.delay_micros(space as u32);
                            }
                        }
                        let _ = ir_ch.set_duty(0);
                        delay.delay_millis(40);
                    }
                    delay.delay_millis(1500);
                    n += 1;
                }
            }
        }
    };

    // ---------------- Boot intro (skippable in Settings) ----------------
    if config.intro_on {
        splash::run(&mut display, &mut delay);
    }

    let mut mode = config.synth_start;
    let mut synth = synth::Synth::new();
    let mut browser = browser::Browser::new();
    let mut settings_ui = settings::Settings::new();
    let mut hacking = hacking::Hacking::new();
    let mut webui = webui::WebUi::new();
    // true when the shared WiFi picker was opened from the "Internet" menu item
    // (connect-and-stay) rather than the Web UI item (connect-then-serve).
    let mut wifi_connect_only = false;
    let mut player = player::Player::new();
    let mut repl = repl::Repl::new();
    let mut games = games::Games::new();
    // The Remote app is handed the USB-OTG peripheral + its D+/D- pins (claimed
    // lazily only if the user switches Remote to USB — see apps::misc::remote).
    let mut misc = misc::Misc::new(
        ir_tx,
        usb_hid::UsbParts { usb0: peripherals.USB0, dp: peripherals.GPIO20, dm: peripherals.GPIO19 },
    );
    #[cfg(feature = "emu")]
    let mut emu = apps::emu::Emu::new();
    let mut stopwatch = stopwatch::Stopwatch::new();
    let mut sysinfo = sysinfo::Sysinfo::new();
    let mut notes = notes::Notes::new();
    // The Hacking menu's WiFi/BLE radios live behind one owner; peripherals are
    // taken lazily on first use, then kept for the session (see radio.rs).
    let mut radio = radio::Radio::new(peripherals.WIFI, peripherals.BT);

    // ---------------- Internal I2C (keyboard + codec) ----------------
    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .unwrap()
    .with_sda(peripherals.GPIO8)
    .with_scl(peripherals.GPIO9);

    let audio_ok = es8311::init(&mut i2c).is_ok();
    let kbd_ok = tca8418::init(&mut i2c).is_ok();
    // BMI270 IMU (Misc Level + Step counter): probe + upload its 8 KB config blob.
    let _ = bmi270::init(&mut i2c);

    // ---------------- I2S audio out ----------------
    // The GBC build (Walnut-CGB) needs the RAM the larger colour core eats, so it
    // trims this DMA ring (still ~0.13 s at 16 kHz, refilled every loop). The
    // DMG/default builds keep the roomier 0.5 s buffer.
    // GBC build: the circular-DMA macro lays out the right descriptor count for
    // write_dma_circular even with a small 4 KB ring, reclaiming RAM for the
    // larger colour core. The DMG/default build keeps its roomier 32 KB buffer
    // (plain dma_buffers! happens to give enough descriptors at that size).
    // emugbc keeps a small audio DMA buffer (the colour core needs the RAM). Subtlety:
    // the I2S TX chain ALWAYS re-chunks at the fixed CHUNK_SIZE (4092) at runtime — the
    // macro's chunk-size argument only sizes the static descriptor ARRAY, not the
    // transfer. So the buffer LENGTH must not leave a tiny tail descriptor at the
    // circular wrap: 8192 splits into 4092 + 4092 + 8, and that 8-byte tail (suc_eof
    // set, like every circular descriptor) replays a ~125 us discontinuity every buffer
    // loop -> an audible periodic click (NOT an underrun; throughput stays real-time).
    // A length <= 2*4092 = 8184 trips fill()'s circular even-split into 3 EQUAL
    // descriptors (8184 -> 3*2728), so there is no tail and no click. The DMG/default
    // 32000-byte buffer splits into large ~4092 segments (smallest 3356), no tiny tail.
    #[cfg(feature = "emugbc")]
    let (_, _, tx_buffer, tx_descriptors) = esp_hal::dma_circular_buffers_chunk_size!(0, 8184, 4092);
    #[cfg(not(feature = "emugbc"))]
    let (_, _, tx_buffer, tx_descriptors) = esp_hal::dma_circular_buffers!(0, 32000);
    let i2s = I2s::new(
        peripherals.I2S0,
        peripherals.DMA_CH0,
        I2sConfig::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(synth::SAMPLE_RATE))
            .with_data_format(DataFormat::Data16Channel16)
            .with_channels(Channels::STEREO),
    )
    .unwrap();
    let mut i2s_tx = i2s
        .i2s_tx
        .with_bclk(peripherals.GPIO41)
        .with_ws(peripherals.GPIO43)
        .with_dout(peripherals.GPIO42)
        .build(tx_descriptors);
    // The circular DMA transfer is held in an Option so it can be DROPPED + RECREATED
    // to resync a WEDGED ring: if the main loop ever stalls longer than the DMA buffer
    // (e.g. the emulator's launch reads the ROM/save off SD for >128 ms), the ring
    // fully underruns and esp-hal's circular transfer wedges — available()/push()
    // return Late forever and the SHARED I2S audio (synth + menu too) stays dead until
    // reboot. esp-hal's circular API can't resync in place, and a struct that owns
    // `i2s_tx` while also holding a transfer that borrows it would be self-referential
    // (not expressible in safe Rust), so we reach `i2s_tx` + `tx_buffer` through raw
    // pointers to decouple the transfer's borrow lifetime and recreate it on demand.
    //
    // SAFETY: `i2s_tx` and the static `tx_buffer` both live for the rest of main(),
    // strictly outliving every transfer made from them, and exactly one transfer
    // exists at a time (drop the old before making the new). `i2s_tx` is never touched
    // except through this pointer after this point.
    let i2s_tx_ptr: *mut _ = &mut i2s_tx;
    // *mut _ keeps the buffer's concrete `[u8; N]` array type (the dma_buffers! macro
    // returns &mut [u8; N]); `[u8; N]: ReadBuffer` but the unsized `[u8]` is not.
    let tx_buf_ptr: *mut _ = tx_buffer;
    let mut transfer = Some(unsafe { (*i2s_tx_ptr).write_dma_circular(&mut *tx_buf_ptr) }.unwrap());
    let mut audio_recover_at = Instant::now();

    // (WS2812 LED is initialised + cleared before the splash, above, so its
    // power-on pixel doesn't glow white through boot when the LED is meant to be
    // off — it must not be re-initialised here.)
    let mut led_phase = 0.0f32;
    let mut led_was_dark = false; // true once we've sent the off-frame for a dark LED

    // ---------------- Button + state machine ----------------
    let g0 = Input::new(peripherals.GPIO0, InputConfig::default().with_pull(Pull::Up));
    let mut g0_prev_low = false;

    let mut samples = [0i16; CHUNK_FRAMES * 2];
    let chunk_bytes = core::mem::size_of_val(&samples); // i16 buffer pushed as raw LE bytes
    // Audio-health diagnostics (--features audiodiag): once a second, report bytes
    // actually pushed to the I2S DMA vs the 64000 B/s real-time target, underrun
    // count, and the max free space seen. Distinguishes "CPU too slow" (late>0) from
    // "DMA not draining" (pushed << 64000) from "flowing but glitched" (pushed≈64000).
    #[cfg(feature = "audiodiag")]
    let (mut diag_pushed, mut diag_late, mut diag_avail_max) = (0u32, 0u32, 0usize);
    #[cfg(feature = "audiodiag")]
    let mut diag_last = Instant::now();
    let mut last_anim = Instant::now();
    let mut last_full_scan = Instant::now(); // radar: full rescan cadence
    let mut last_fast_scan = Instant::now(); // radar: locked-target single-channel
    let mut last_vu_fw: i32 = -1; // last drawn VU meter fill width (px); gates Synth redraws
    // Hold-to-repeat (PC-style): SET on press, CLEAR on release (now that the
    // press/release decode is correct), synthesize repeats in between. A 4 s cap
    // is a belt-and-suspenders fail-safe against a lost release.
    let mut kbd_held: Option<(u8, u8)> = None;
    let mut kbd_held_since = Instant::now();
    let mut kbd_last_repeat = Instant::now();

    // off-screen framebuffer; UI renders here, then one blit per frame -> no flash
    let mut fbuf = fb::FrameBuf::new(unsafe { &mut *core::ptr::addr_of_mut!(FB_DATA) });

    // Emulator boot self-test: exercise the GB core on-device over serial, then
    // continue to the normal menu. Off unless built with `--features emutest`.
    #[cfg(feature = "emutest")]
    apps::emu::selftest(&vm, &mut fbuf);

    let mut screen = Screen::Menu;
    let mut menu_sel: usize = 0;
    let mut menu_scroll: usize = 0;
    let mut last_batt: u8 = 255;
    let mut last_present = false;
    let mut last_input = Instant::now();
    let mut last_batt_check = Instant::now();
    let mut screen_off = false;
    let mut dirty = false;

    menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
    let _ = display.set_pixels(0, 0, (fb::W - 1) as u16, (fb::H - 1) as u16, fbuf.pixels());

    // Start the preemptive scheduler. esp-radio (WiFi/BLE, used lazily in the Hacking
    // menu) requires it to be running before any radio init. All hardware above is
    // already up and the first frame is on screen, so if this ever misbehaves the menu
    // is still visible. The main loop below runs as the scheduler's main task; TIMG0 +
    // software-interrupt 0 are dedicated to it (Instant/Delay keep using the SYSTIMER).
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    // USB-serial self-test build: exercise every radio tool over serial, then fall
    // through to the normal menu. No-op in a normal build.
    #[cfg(feature = "selftest")]
    selftest::run(&mut radio);

    // Network-stack self-test build: HTTP-client core (parser + builder + a
    // smoltcp loopback round-trip) + the camera classifier, no radio needed.
    // No-op in a normal build.
    #[cfg(feature = "networktest")]
    {
        crate::radio::http::networktest();
        crate::radio::camscan::networktest();
        crate::radio::wpa::networktest();
        crate::config::networktest();
    }

    loop {
        // ---- keyboard (with hold-to-repeat) ----
        loop {
            let ev = match tca8418::next_event(&mut i2c) {
                Ok(Some(e)) => {
                    // Track the held key for auto-repeat. Shift/ENTER/home never
                    // repeat (that would spam case-toggles / launches / menu jumps).
                    let rc = (e.row, e.col);
                    let repeatable = rc != crate::hal::keymap::K_SHIFT
                        && rc != K_ENTER
                        && rc != K_HOME
                        // Remote's USB keyboard wants ONE report per physical press;
                        // auto-repeat would spam the host. Mouse mode keeps repeat.
                        && !(screen == Screen::Misc && misc.remote_typing());
                    if e.pressed && repeatable {
                        kbd_held = Some(rc);
                        kbd_held_since = Instant::now();
                        kbd_last_repeat = Instant::now();
                        #[cfg(feature = "keylog")]
                        esp_println::println!("EV down r{} c{} -> SET", e.row, e.col);
                    } else if !e.pressed && kbd_held == Some(rc) {
                        kbd_held = None; // release stops the repeat immediately
                        #[cfg(feature = "keylog")]
                        esp_println::println!("EV up   r{} c{} -> CLEAR", e.row, e.col);
                    } else {
                        #[cfg(feature = "keylog")]
                        esp_println::println!(
                            "EV {} r{} c{} -> noop (held={:?})",
                            if e.pressed { "down" } else { "up  " },
                            e.row,
                            e.col,
                            kbd_held
                        );
                    }
                    e
                }
                // No hardware event pending: synthesize a repeat for the held key
                // once past the initial delay and due. The held key is cleared by
                // its release event (reliable now that the decode is correct), so
                // no time cap is needed.
                _ => match kbd_held {
                    Some((r, c))
                        if kbd_held_since.elapsed() >= Duration::from_millis(280)
                            && kbd_last_repeat.elapsed() >= Duration::from_millis(120) =>
                    {
                        kbd_last_repeat = Instant::now();
                        #[cfg(feature = "keylog")]
                        esp_println::println!("REPEAT r{} c{}", r, c);
                        tca8418::KeyEvent { pressed: true, row: r, col: c }
                    }
                    _ => break,
                },
            };
            // The "Aa" key is a caps toggle (the keyboard has no hardware caps-lock):
            // tap to flip case for whichever screen is taking text input. It types
            // nothing itself; we act on the press edge and ignore the release.
            if (ev.row, ev.col) == crate::hal::keymap::K_SHIFT {
                if ev.pressed {
                    match screen {
                        Screen::Repl => repl.toggle_caps(&mut fbuf),
                        Screen::Hacking => hacking.toggle_caps(&mut fbuf),
                        Screen::Notes => notes.toggle_caps(&mut fbuf),
                        Screen::WebUi => webui.toggle_caps(&mut fbuf),
                        Screen::Misc => misc.toggle_caps(&mut fbuf),
                        _ => {}
                    }
                    last_input = Instant::now();
                    dirty = true; // flush the indicator this frame, not on the next key
                }
                continue;
            }
            // The emulator wants press AND release (held buttons), unlike the
            // one-shot apps, so handle it before the press-only path below. The
            // back/home keys leave the game (saving cart RAM first).
            #[cfg(feature = "emu")]
            if screen == Screen::Emu {
                let rc = (ev.row, ev.col);
                if ev.pressed && rc == K_HOME {
                    // ESC = back one level: playing -> ROM list (saves cart RAM); from the ROM
                    // list -> the Games launcher. (Backspace is a game button now, below.)
                    if !emu.back(&vm, &mut fbuf) {
                        screen = Screen::Games;
                        games.enter(&mut fbuf);
                    }
                    last_input = Instant::now();
                    dirty = true;
                    continue;
                }
                emu.on_event((ev.row, ev.col), ev.pressed, &vm, &mut fbuf);
                if ev.pressed {
                    last_input = Instant::now();
                    dirty = true;
                }
                continue;
            }

            if !ev.pressed {
                continue;
            }
            last_input = Instant::now();
            if screen_off {
                // Any key wakes the screen (the panel kept its last frame, so just turn the
                // backlight back on) and is consumed — it doesn't also act on the active screen.
                let _ = backlight.set_duty(bl_pct(config.disp_bright));
                screen_off = false;
                dirty = true;
                continue;
            }
            dirty = true;
            let rc = (ev.row, ev.col);

            // ESC (the ` top-left key) = go BACK one level. Menus aren't deeply nested, so a
            // single back is enough — there's no separate "jump all the way home" anymore.
            // Backspace no longer navigates: it falls through to the active screen as a normal
            // key (text apps delete a char with it; others ignore it).
            if rc == K_HOME {
                match screen {
                    Screen::Menu => {}
                    Screen::WebUi => {
                        if !webui.back(&mut fbuf) {
                            screen = Screen::Menu;
                            menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                        }
                    }
                    Screen::Hacking => {
                        if !hacking.back(&mut fbuf) {
                            screen = Screen::Menu;
                            menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                        }
                    }
                    Screen::Games => {
                        if !games.back(&mut fbuf) {
                            screen = Screen::Menu;
                            menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                        }
                    }
                    Screen::Misc => {
                        if !misc.back(&vm, &mut fbuf) {
                            screen = Screen::Menu;
                            menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                        }
                    }
                    Screen::Player => {
                        // playing -> back to the track list; list -> home menu
                        if !player.back(&vm, &mut fbuf) {
                            screen = Screen::Menu;
                            menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                        }
                    }
                    Screen::Notes => {
                        if !notes.back(&vm, &mut fbuf) {
                            screen = Screen::Menu;
                            menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                        }
                    }
                    _ => {
                        // flat screens (Synth, Settings, Sysinfo, Stopwatch, Browser, Charge,
                        // Repl): one level up = the home menu.
                        screen = Screen::Menu;
                        menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                    }
                }
                continue;
            }

            match screen {
                Screen::Menu => match rc {
                    K_UP => {
                        menu_sel = if menu_sel == 0 { menu::APPS.len() - 1 } else { menu_sel - 1 };
                        menu_scroll = clamp_scroll(menu_sel, menu_scroll, menu::VISIBLE);
                        menu::draw(&mut fbuf, menu_sel, menu_scroll, false);
                    }
                    K_DOWN => {
                        menu_sel = (menu_sel + 1) % menu::APPS.len();
                        menu_scroll = clamp_scroll(menu_sel, menu_scroll, menu::VISIBLE);
                        menu::draw(&mut fbuf, menu_sel, menu_scroll, false);
                    }
                    K_ENTER => match menu::APPS[menu_sel].kind {
                        menu::AppKind::Synth => {
                            mode = config.synth_start;
                            synth.set_volume(config.synth_vol);
                            synth.set_power_chord(config.rock_chord);
                            screen = Screen::Synth;
                            ui::draw_static(&mut fbuf, mode, synth.volume());
                            status_dots(&mut fbuf, audio_ok, kbd_ok);
                        }
                        menu::AppKind::WebUi => {
                            wifi_connect_only = false; // connect, then serve the dashboard
                            screen = Screen::WebUi;
                            synth.silence();
                            if radio::wifi_connected() {
                                // Already connected (via the Internet/Connect item): serve the
                                // dashboard on the EXISTING link — don't make the user re-pick.
                                let mac = esp_hal::efuse::base_mac_address();
                                let mb = mac.as_bytes();
                                let sys = radio::webui::SysSnapshot {
                                    heap_free: esp_alloc::HEAP.free(),
                                    heap_used: esp_alloc::HEAP.used(),
                                    heap_total: esp_alloc::HEAP.stats().size,
                                    uptime_s: sysinfo.uptime_s(),
                                    batt_pct: if battery::present() { battery::level() as i32 } else { -1 },
                                    mac: [mb[0], mb[1], mb[2], mb[3], mb[4], mb[5]],
                                };
                                webui.draw_status(&mut fbuf, webui::Phase::Connecting);
                                let _ = display.set_pixels(0, 0, (fb::W - 1) as u16, (fb::H - 1) as u16, fbuf.pixels());
                                let mut last = Instant::now();
                                let _ = radio.serve_webui(&vm, &sys, |st| {
                                    let mut stop = false;
                                    while let Ok(Some(ev)) = tca8418::next_event(&mut i2c) {
                                        if ev.pressed {
                                            stop = true;
                                        }
                                    }
                                    if g0.is_low() {
                                        stop = true;
                                    }
                                    if stop {
                                        return false;
                                    }
                                    if last.elapsed() >= Duration::from_millis(180) {
                                        last = Instant::now();
                                        let phase = match st.phase {
                                            radio::webui::Phase::Serving => {
                                                webui::Phase::Serving { ip: st.ip, hits: st.hits }
                                            }
                                            _ => webui::Phase::Connecting,
                                        };
                                        webui.draw_status(&mut fbuf, phase);
                                        let _ = display.set_pixels(0, 0, (fb::W - 1) as u16, (fb::H - 1) as u16, fbuf.pixels());
                                    }
                                    true
                                });
                                g0_prev_low = g0.is_low();
                                screen = Screen::Menu;
                                menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                            } else {
                                webui.enter(&mut fbuf); // "scanning WiFi..."
                                let _ = display.set_pixels(0, 0, (fb::W - 1) as u16, (fb::H - 1) as u16, fbuf.pixels());
                                webui.begin_scan();
                                match radio.scan() {
                                    Some(aps) => {
                                        for ap in &aps {
                                            let secured = ap.auth != "open";
                                            webui.push_ap(ap.ssid.as_bytes(), ap.rssi, ap.channel, ap.auth, secured);
                                        }
                                    }
                                    None => webui.mark_scan_failed(),
                                }
                                webui.show_list(&mut fbuf);
                                // load remembered networks so a known one's password
                                // comes pre-filled when picked.
                                webui.clear_known();
                                radio::webui::load_creds(&vm, |ssid, pw| webui.add_known(ssid, pw));
                            }
                        }
                        menu::AppKind::Player => {
                            // Free the radio's heap — the Player allocates its decode
                            // buffers (~59 KB) from the same heap the radio holds for
                            // the session, and they never run together.
                            radio.shutdown();
                            screen = Screen::Player;
                            synth.silence();
                            player.enter(&vm, &mut fbuf);
                        }
                        menu::AppKind::Browser => {
                            screen = Screen::Browser;
                            synth.silence();
                            browser.set_opts(config.sort_by, config.show_hidden, config.confirm_delete);
                            browser.enter(&vm, &mut fbuf);
                        }
                        menu::AppKind::Charge => {
                            screen = Screen::Charge;
                            synth.silence();
                            charge::draw(&mut fbuf, true);
                        }
                        menu::AppKind::Settings => {
                            screen = Screen::Settings;
                            synth.silence();
                            settings_ui.enter(&mut fbuf, &config);
                        }
                        menu::AppKind::Hacking => {
                            screen = Screen::Hacking;
                            synth.silence();
                            hacking.enter(&mut fbuf);
                        }
                        menu::AppKind::Repl => {
                            screen = Screen::Repl;
                            synth.silence();
                            repl.enter(&mut fbuf);
                        }
                        menu::AppKind::Games => {
                            screen = Screen::Games;
                            synth.silence();
                            games.enter(&mut fbuf);
                        }
                        menu::AppKind::Misc => {
                            screen = Screen::Misc;
                            synth.silence();
                            misc.enter(&mut fbuf);
                        }
                        menu::AppKind::Stopwatch => {
                            screen = Screen::Stopwatch;
                            synth.silence();
                            stopwatch.enter(&mut fbuf);
                        }
                        menu::AppKind::Sysinfo => {
                            screen = Screen::Sysinfo;
                            synth.silence();
                            sysinfo.enter(&mut fbuf);
                        }
                        menu::AppKind::Notes => {
                            screen = Screen::Notes;
                            synth.silence();
                            notes.enter(&vm, &mut fbuf);
                        }
                        // The global WiFi connection. Connected -> disconnect; else
                        // open the SHARED WiFi picker (the same scan/list/password UI
                        // as Web UI). On connect we STAY connected — see
                        // Action::Connect + `wifi_connect_only`.
                        menu::AppKind::Internet => {
                            synth.silence();
                            if radio::wifi_connected() {
                                radio.disconnect_sta();
                                menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                            } else {
                                wifi_connect_only = true;
                                screen = Screen::WebUi;
                                webui.enter(&mut fbuf); // "scanning WiFi..."
                                let _ = display.set_pixels(0, 0, (fb::W - 1) as u16, (fb::H - 1) as u16, fbuf.pixels());
                                webui.begin_scan();
                                match radio.scan() {
                                    Some(aps) => {
                                        for ap in &aps {
                                            let secured = ap.auth != "open";
                                            webui.push_ap(ap.ssid.as_bytes(), ap.rssi, ap.channel, ap.auth, secured);
                                        }
                                    }
                                    None => webui.mark_scan_failed(),
                                }
                                webui.show_list(&mut fbuf);
                                webui.clear_known();
                                radio::webui::load_creds(&vm, |ssid, pw| webui.add_known(ssid, pw));
                            }
                        }
                    },
                    _ => {}
                },
                Screen::Synth => match rc {
                    K_UP => {
                        synth.volume_up();
                        ui::draw_volume(&mut fbuf, synth.volume(), mode);
                    }
                    K_DOWN => {
                        synth.volume_down();
                        ui::draw_volume(&mut fbuf, synth.volume(), mode);
                    }
                    _ => {
                        let degree = key_to_degree(ev.row, ev.col, mode);
                        let midi = scales::scale_note_midi(mode, ROOT_MIDI, degree);
                        synth.trigger(scales::midi_to_freq(midi), mode);
                        ui::draw_note(&mut fbuf, Some(midi), mode);
                    }
                },
                Screen::Repl => repl.on_key(rc, &mut fbuf),
                Screen::Misc => misc.on_key(rc, &vm, &mut fbuf),
                Screen::Games => {
                    // true == the user picked "Game Boy"; hand off to the emulator.
                    if games.on_key(rc, &mut fbuf) {
                        #[cfg(feature = "emu")]
                        {
                            // Free the radio's heap — the emulator's ROM cache + cart
                            // RAM need the heap the radio holds for the session.
                            radio.shutdown();
                            screen = Screen::Emu;
                            synth.silence();
                            emu.enter(&vm, &mut fbuf);
                        }
                    }
                }
                // Emulator key events are handled earlier (it needs key releases too),
                // so this arm is unreachable; it only satisfies the match.
                #[cfg(feature = "emu")]
                Screen::Emu => {}
                Screen::WebUi => match webui.on_key(rc, &mut fbuf) {
                    webui::Action::Connect => {
                        // Copy ssid/pw out so `webui` is free for the tick repaint.
                        let mut ssid_b = [0u8; 32];
                        let s = webui.ssid().as_bytes();
                        let sl = s.len().min(32);
                        ssid_b[..sl].copy_from_slice(&s[..sl]);
                        let mut pw_b = [0u8; 64];
                        let p = webui.password().as_bytes();
                        let pl = p.len().min(64);
                        pw_b[..pl].copy_from_slice(&p[..pl]);
                        let ssid = core::str::from_utf8(&ssid_b[..sl]).unwrap_or("");
                        let pw = core::str::from_utf8(&pw_b[..pl]).unwrap_or("");

                        // Establish the ONE shared, persistent connection — used by
                        // the Internet item and the Web UI dashboard alike. Abortable.
                        webui.draw_status(&mut fbuf, webui::Phase::Connecting);
                        let _ = display.set_pixels(0, 0, (fb::W - 1) as u16, (fb::H - 1) as u16, fbuf.pixels());
                        let ok = radio
                            .connect_sta(ssid, pw, || {
                                let mut stop = false;
                                while let Ok(Some(ev)) = tca8418::next_event(&mut i2c) {
                                    if ev.pressed {
                                        stop = true;
                                    }
                                }
                                if g0.is_low() {
                                    stop = true;
                                }
                                !stop
                            })
                            .is_some();
                        g0_prev_low = g0.is_low();
                        if ok {
                            radio::webui::save_cred(&vm, ssid, pw); // good password -> remember
                        }

                        if !ok {
                            webui.draw_status(
                                &mut fbuf,
                                webui::Phase::Failed(i18n::t(app::CONNECT_FAILED)),
                            );
                        } else if wifi_connect_only {
                            // came from the Internet item -> stay connected, go home.
                            screen = Screen::Menu;
                            menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                        } else {
                            // Web UI dashboard: serve on the shared connection, which
                            // persists afterwards (the link is NOT torn down here).
                            let mac = esp_hal::efuse::base_mac_address();
                            let mb = mac.as_bytes();
                            let sys = radio::webui::SysSnapshot {
                                heap_free: esp_alloc::HEAP.free(),
                                heap_used: esp_alloc::HEAP.used(),
                                heap_total: esp_alloc::HEAP.stats().size,
                                uptime_s: sysinfo.uptime_s(),
                                batt_pct: if battery::present() { battery::level() as i32 } else { -1 },
                                mac: [mb[0], mb[1], mb[2], mb[3], mb[4], mb[5]],
                            };
                            let mut last = Instant::now();
                            let _ = radio.serve_webui(&vm, &sys, |st| {
                                let mut stop = false;
                                while let Ok(Some(ev)) = tca8418::next_event(&mut i2c) {
                                    if ev.pressed {
                                        stop = true;
                                    }
                                }
                                if g0.is_low() {
                                    stop = true;
                                }
                                if stop {
                                    return false;
                                }
                                if last.elapsed() >= Duration::from_millis(180) {
                                    last = Instant::now();
                                    let phase = match st.phase {
                                        radio::webui::Phase::Serving => {
                                            webui::Phase::Serving { ip: st.ip, hits: st.hits }
                                        }
                                        _ => webui::Phase::Connecting,
                                    };
                                    webui.draw_status(&mut fbuf, phase);
                                    let _ = display.set_pixels(0, 0, (fb::W - 1) as u16, (fb::H - 1) as u16, fbuf.pixels());
                                }
                                true
                            });
                            g0_prev_low = g0.is_low();
                            screen = Screen::Menu;
                            menu::draw(&mut fbuf, menu_sel, menu_scroll, true);
                        }
                    }
                    webui::Action::Redraw | webui::Action::None => {}
                },
                Screen::Stopwatch => stopwatch.on_key(rc, &mut fbuf),
                Screen::Sysinfo => sysinfo.on_key(rc, &mut fbuf),
                Screen::Notes => notes.on_key(rc, &vm, &mut fbuf),
                Screen::Player => player.on_key(rc, &vm, &mut fbuf),
                Screen::Browser => browser.on_key(rc, &vm, &mut fbuf),
                Screen::Settings => {
                    if settings_ui.on_key(rc, &mut config, &mut fbuf) {
                        config.save(&vm); // persist (no-op if no SD card)
                        let _ = backlight.set_duty(bl_pct(config.disp_bright)); // live apply
                    }
                }
                Screen::Charge => {} // view-only; ` or G0 returns to the menu
                Screen::Hacking => {
                    // Recon tools draw a "busy" screen then run a one-shot blocking
                    // capture. Attacks loop until stopped: `run_attack!` paints a live
                    // counter and feeds the radio a tick closure that polls for an
                    // abort key (any key, or G0) — returning false stops the attack.
                    macro_rules! blit {
                        () => {
                            let _ = display.set_pixels(0, 0, (fb::W - 1) as u16, (fb::H - 1) as u16, fbuf.pixels());
                        };
                    }
                    macro_rules! run_attack {
                        ($unit:expr, $run:expr) => {{
                            let title = hacking.attack_title();
                            hacking.set_running();
                            hacking::draw_running(&mut fbuf, title, $unit, 0);
                            blit!();
                            let mut last = Instant::now();
                            let res = ($run)(|sent: u32| {
                                let mut stop = false;
                                while let Ok(Some(ev)) = tca8418::next_event(&mut i2c) {
                                    if ev.pressed {
                                        stop = true;
                                    }
                                }
                                if g0.is_low() {
                                    stop = true;
                                }
                                if stop {
                                    return false;
                                }
                                if last.elapsed() >= Duration::from_millis(150) {
                                    last = Instant::now();
                                    hacking::draw_running(&mut fbuf, title, $unit, sent);
                                    blit!();
                                }
                                true
                            });
                            // swallow the G0 edge if the user held it to stop
                            g0_prev_low = g0.is_low();
                            hacking.show_attack_done(&mut fbuf, res);
                        }};
                    }

                    match hacking.on_key(rc, &mut fbuf) {
                        hacking::Action::Run(tool) => match tool {
                            hacking::Tool::WifiScan | hacking::Tool::WifiAnalyze => {
                                let bt = tool.name();
                                hacking.draw_busy(&mut fbuf, bt, i18n::t(app::SCANNING));
                                blit!();
                                match radio.scan() {
                                    Some(aps) => {
                                        hacking.begin_wifi_results();
                                        for ap in &aps {
                                            hacking.push_ap(&ap.ssid, ap.bssid, ap.rssi, ap.channel, ap.auth);
                                        }
                                    }
                                    None => hacking.set_scan_failed(),
                                }
                                if matches!(tool, hacking::Tool::WifiScan) {
                                    hacking.show_wifi(&mut fbuf);
                                } else {
                                    hacking.show_analyzer(&mut fbuf);
                                }
                            }
                            hacking::Tool::BleScan => {
                                let bt = hacking::Tool::BleScan.name();
                                hacking.draw_busy(&mut fbuf, bt, i18n::t(app::SCANNING_BLE));
                                blit!();
                                match radio.ble_scan() {
                                    Some(devs) => {
                                        hacking.begin_ble_results();
                                        for dv in &devs {
                                            hacking.push_ble(dv.addr, dv.rssi, dv.name.as_deref());
                                        }
                                    }
                                    None => hacking.set_scan_failed(),
                                }
                                hacking.show_ble(&mut fbuf);
                            }
                            hacking::Tool::Detector => {
                                let bt = hacking::Tool::Detector.name();
                                hacking.draw_busy(&mut fbuf, bt, i18n::t(app::LISTENING));
                                blit!();
                                match radio.detect() {
                                    Some(r) => hacking.set_detector_results(r.deauth, r.disassoc, r.beacon, r.frames),
                                    None => hacking.set_scan_failed(),
                                }
                                hacking.show_detector(&mut fbuf);
                            }
                            hacking::Tool::BeaconSpam => {
                                let (pbuf, plen) = hacking.prefix_owned();
                                let prefix = core::str::from_utf8(&pbuf[..plen]).unwrap_or("ATAKAN");
                                let owned: alloc::vec::Vec<alloc::string::String> =
                                    if matches!(hacking.name_src(), hacking::NameSrc::Custom) {
                                        (1..=20u32).map(|i| alloc::format!("{}{:03}", prefix, i)).collect()
                                    } else {
                                        alloc::vec::Vec::new()
                                    };
                                let names: alloc::vec::Vec<&str> = match hacking.name_src() {
                                    hacking::NameSrc::RandomEn => hacking::SPAM_SSIDS.iter().copied().collect(),
                                    hacking::NameSrc::RandomTr => hacking::SPAM_SSIDS_TR.iter().copied().collect(),
                                    hacking::NameSrc::Custom => owned.iter().map(|s| s.as_str()).collect(),
                                };
                                run_attack!(i18n::t(app::BEACONS), |tick| radio.beacon_spam(&names, 6, tick));
                            }
                            hacking::Tool::ProbeFlood => {
                                let (pbuf, plen) = hacking.prefix_owned();
                                let prefix = core::str::from_utf8(&pbuf[..plen]).unwrap_or("ATAKAN");
                                let owned: alloc::vec::Vec<alloc::string::String> =
                                    if matches!(hacking.name_src(), hacking::NameSrc::Custom) {
                                        (1..=20u32).map(|i| alloc::format!("{}{:03}", prefix, i)).collect()
                                    } else {
                                        alloc::vec::Vec::new()
                                    };
                                let names: alloc::vec::Vec<&str> = match hacking.name_src() {
                                    hacking::NameSrc::RandomEn => hacking::SPAM_SSIDS.iter().copied().collect(),
                                    hacking::NameSrc::RandomTr => hacking::SPAM_SSIDS_TR.iter().copied().collect(),
                                    hacking::NameSrc::Custom => owned.iter().map(|s| s.as_str()).collect(),
                                };
                                run_attack!(i18n::t(app::PROBES), |tick| radio.probe_flood(&names, 6, tick));
                            }
                            // These reach the radio via ScanTargets / Portal / BleSpam below.
                            hacking::Tool::Deauth
                            | hacking::Tool::EvilTwin
                            | hacking::Tool::Handshake
                            | hacking::Tool::NetScan
                            | hacking::Tool::CamScan
                            | hacking::Tool::EvilPortal
                            | hacking::Tool::BleSpam => {}
                        },
                        hacking::Action::ScanTargets => {
                            let bt = hacking.attack_title();
                            hacking.draw_busy(&mut fbuf, bt, i18n::t(app::SCANNING));
                            blit!();
                            match radio.scan() {
                                Some(aps) => {
                                    hacking.begin_wifi_results();
                                    for ap in &aps {
                                        hacking.push_ap(&ap.ssid, ap.bssid, ap.rssi, ap.channel, ap.auth);
                                    }
                                }
                                None => hacking.set_scan_failed(),
                            }
                            hacking.show_targets(&mut fbuf);
                        }
                        hacking::Action::Deauth => {
                            if let Some((bssid, ch)) = hacking.target() {
                                run_attack!(i18n::t(app::FRAMES), |tick| radio.deauth(bssid, ch, tick));
                            }
                        }
                        hacking::Action::EvilTwin => {
                            // EVIL TWIN = rogue WPA2 AP cloning the target SSID + sniff the
                            // connecting client's msg2 (MIC from the REAL pass) -> crack offline.
                            // Forces clients onto our 2.4 GHz AP, no deauth. See run_evil_twin.
                            if let Some((sb, sl, ch)) = hacking.target_ssid_owned() {
                                let ssid = core::str::from_utf8(&sb[..sl]).unwrap_or("");
                                let title = hacking.attack_title();
                                hacking.set_running();
                                hacking::draw_running(&mut fbuf, title, "EAPOL", 0);
                                blit!();
                                let mut last = Instant::now();
                                // optional SD wordlist (/WIFIPASS.TXT — must be an 8.3 name)
                                // atop the built-in list. 4 KB cap keeps it heap-light w/ radio up.
                                let mut wl = alloc::vec![0u8; 4096];
                                let wln = radio::webui::read_root_file(&vm, "WIFIPASS.TXT", &mut wl);
                                let extra = if wln > 0 { Some(&wl[..wln]) } else { None };
                                let res = radio.run_evil_twin(ssid, ch, "00000000", extra, |n: u32| {
                                    let mut stop = false;
                                    while let Ok(Some(ev)) = tca8418::next_event(&mut i2c) {
                                        if ev.pressed {
                                            stop = true;
                                        }
                                    }
                                    if g0.is_low() {
                                        stop = true;
                                    }
                                    if stop {
                                        return false;
                                    }
                                    if last.elapsed() >= Duration::from_millis(150) {
                                        last = Instant::now();
                                        hacking::draw_running(&mut fbuf, title, "EAPOL", n);
                                        blit!();
                                    }
                                    true
                                });
                                g0_prev_low = g0.is_low();
                                match res {
                                    Some(o) => {
                                        // export the captured handshake (.22000) to SD for PC/offload cracking
                                        if let Some(ref line) = o.hc22000 {
                                            let _ = radio::webui::write_root_file(&vm, "HS22000.TXT", line.as_bytes());
                                        }
                                        hacking.show_handshake(&mut fbuf, o.eapol, o.captured, o.cracked.as_deref());
                                    }
                                    None => hacking.show_attack_failed(&mut fbuf, "radio busy"),
                                }
                            }
                        }
                        hacking::Action::Handshake => {
                            // capture the 4-way handshake (deauth-assisted) then crack it
                            // OFFLINE against the built-in weak list — see radio::handshake_crack.
                            if let (Some((bssid, ch)), Some((sb, sl, _))) =
                                (hacking.target(), hacking.target_ssid_owned())
                            {
                                let ssid = core::str::from_utf8(&sb[..sl]).unwrap_or("");
                                let title = hacking.attack_title();
                                hacking.set_running();
                                hacking::draw_running(&mut fbuf, title, "EAPOL", 0);
                                blit!();
                                let mut last = Instant::now();
                                // optional SD wordlist (/WIFIPASS.TXT — must be an 8.3 name)
                                // atop the built-in list. 4 KB cap keeps it heap-light w/ radio up.
                                let mut wl = alloc::vec![0u8; 4096];
                                let wln = radio::webui::read_root_file(&vm, "WIFIPASS.TXT", &mut wl);
                                let extra = if wln > 0 { Some(&wl[..wln]) } else { None };
                                let res = radio.handshake_crack(ssid, bssid, ch, extra, |n: u32| {
                                    let mut stop = false;
                                    while let Ok(Some(ev)) = tca8418::next_event(&mut i2c) {
                                        if ev.pressed {
                                            stop = true;
                                        }
                                    }
                                    if g0.is_low() {
                                        stop = true;
                                    }
                                    if stop {
                                        return false;
                                    }
                                    if last.elapsed() >= Duration::from_millis(150) {
                                        last = Instant::now();
                                        hacking::draw_running(&mut fbuf, title, "EAPOL", n);
                                        blit!();
                                    }
                                    true
                                });
                                g0_prev_low = g0.is_low();
                                match res {
                                    Some(o) => {
                                        // export the captured handshake (.22000) to SD for PC/offload cracking
                                        if let Some(ref line) = o.hc22000 {
                                            let _ = radio::webui::write_root_file(&vm, "HS22000.TXT", line.as_bytes());
                                        }
                                        hacking.show_handshake(&mut fbuf, o.eapol, o.captured, o.cracked.as_deref());
                                    }
                                    None => hacking.show_attack_failed(&mut fbuf, "radio busy"),
                                }
                            }
                        }
                        hacking::Action::Portal => {
                            // The portal runs its own poll loop; repaint live stats and
                            // poll for an abort key between polls.
                            let (sbuf, slen) = hacking.portal_ssid_owned();
                            let ssid = core::str::from_utf8(&sbuf[..slen]).unwrap_or("Free WiFi");
                            hacking.set_running();
                            hacking::draw_portal(&mut fbuf, ssid, &portal::Stats::new());
                            blit!();
                            let mut last = Instant::now();
                            let stats = radio.run_portal(ssid, 6, |s| {
                                let mut stop = false;
                                while let Ok(Some(ev)) = tca8418::next_event(&mut i2c) {
                                    if ev.pressed {
                                        stop = true;
                                    }
                                }
                                if g0.is_low() {
                                    stop = true;
                                }
                                if stop {
                                    return false;
                                }
                                if last.elapsed() >= Duration::from_millis(180) {
                                    last = Instant::now();
                                    hacking::draw_portal(&mut fbuf, ssid, s);
                                    blit!();
                                }
                                true
                            });
                            g0_prev_low = g0.is_low();
                            hacking.show_attack_done(&mut fbuf, stats.map(|s| s.creds));
                        }
                        hacking::Action::NetScan => {
                            if let Some((ssid_buf, ssid_len, _ch)) = hacking.target_ssid_owned() {
                                let ssid = core::str::from_utf8(&ssid_buf[..ssid_len]).unwrap_or("");
                                let wk = hacking.wifi_known();
                                let known = wk.as_ref().map(|(b, l)| core::str::from_utf8(&b[..*l]).unwrap_or(""));
                                hacking.set_running();
                                let bt = hacking::Tool::NetScan.name();
                                hacking.draw_busy(&mut fbuf, bt, i18n::t(app::JOINING));
                                blit!();
                                let mut last = Instant::now();
                                let res = radio.run_netscan(ssid, known, |st| {
                                    let mut stop = false;
                                    while let Ok(Some(ev)) = tca8418::next_event(&mut i2c) {
                                        if ev.pressed {
                                            stop = true;
                                        }
                                    }
                                    if g0.is_low() {
                                        stop = true;
                                    }
                                    if stop {
                                        return false;
                                    }
                                    if last.elapsed() >= Duration::from_millis(180) {
                                        last = Instant::now();
                                        hacking::draw_netscan(&mut fbuf, st);
                                        blit!();
                                    }
                                    true
                                });
                                g0_prev_low = g0.is_low();
                                match res {
                                    Ok(r) if r.got_ip => hacking.show_attack_done(&mut fbuf, Some(r.open_count() as u32)),
                                    Ok(r) => hacking.show_attack_failed(&mut fbuf, r.phase),
                                    Err(e) => hacking.show_attack_failed(&mut fbuf, e),
                                }
                            }
                        }
                        hacking::Action::CamScan => {
                            if let Some((ssid_buf, ssid_len, _ch)) = hacking.target_ssid_owned() {
                                let ssid = core::str::from_utf8(&ssid_buf[..ssid_len]).unwrap_or("");
                                let wk = hacking.wifi_known();
                                let known = wk.as_ref().map(|(b, l)| core::str::from_utf8(&b[..*l]).unwrap_or(""));
                                hacking.set_running();
                                let bt = hacking::Tool::CamScan.name();
                                hacking.draw_busy(&mut fbuf, bt, i18n::t(app::JOINING));
                                blit!();
                                let mut last = Instant::now();
                                let res = radio.run_camscan(ssid, known, |st| {
                                    let mut stop = false;
                                    while let Ok(Some(ev)) = tca8418::next_event(&mut i2c) {
                                        if ev.pressed {
                                            stop = true;
                                        }
                                    }
                                    if g0.is_low() {
                                        stop = true;
                                    }
                                    if stop {
                                        return false;
                                    }
                                    if last.elapsed() >= Duration::from_millis(180) {
                                        last = Instant::now();
                                        hacking::draw_camscan(&mut fbuf, st);
                                        blit!();
                                    }
                                    true
                                });
                                g0_prev_low = g0.is_low();
                                match res {
                                    Ok(r) if r.got_ip => hacking.show_attack_done(&mut fbuf, Some(r.cam_count() as u32)),
                                    Ok(r) => hacking.show_attack_failed(&mut fbuf, r.phase),
                                    Err(e) => hacking.show_attack_failed(&mut fbuf, e),
                                }
                            }
                        }
                        hacking::Action::BleSpam(mode) => {
                            run_attack!(i18n::t(app::ADVERTS), |tick| radio.ble_spam(mode, tick));
                        }
                        hacking::Action::Redraw | hacking::Action::None => {}
                    }
                }
            }
        }

        // ---- G0 button ----
        let low = g0.is_low();
        if low && !g0_prev_low {
            last_input = Instant::now();
            if screen_off {
                // G0 wakes the screen (the panel kept its last frame) and is consumed.
                let _ = backlight.set_duty(bl_pct(config.disp_bright));
                screen_off = false;
                dirty = true;
            } else {
                // G0 = screen-sleep toggle, EXCEPT on screens where it has a real per-app
                // function: Synth (mode), Emu/Player (playback), Misc (Remote connection
                // toggle). Anywhere else it puts the screen to sleep (any key or G0 wakes it).
                let consumed = match screen {
                    Screen::Synth => {
                        mode = mode.next();
                        ui::flash_mode(&mut fbuf, mode, synth.volume());
                        true
                    }
                    #[cfg(feature = "emu")]
                    Screen::Emu if emu.is_playing() => {
                        emu.bump_volume(&mut fbuf); // cycle in-game volume
                        true
                    }
                    Screen::Player if player.in_playing() => {
                        player.toggle_pause(&mut fbuf);
                        true
                    }
                    // Misc returns true only when G0 had a real function (the Remote app's
                    // USB/BLE connection toggle); otherwise it leaves it to us to sleep.
                    Screen::Misc => misc.g0(&mut fbuf),
                    _ => false,
                };
                if consumed {
                    dirty = true;
                } else {
                    let _ = backlight.set_duty(0);
                    screen_off = true;
                }
            }
        }
        g0_prev_low = low;

        // ---- emulator: run a Game Boy frame every iteration while playing ----
        // The core does ~92 fps unthrottled, so pace it by the work itself (one
        // frame + one blit per loop) rather than the 40 ms UI tick (which would
        // cap it to 25 fps). Audio is silenced on entry, so a slower loop here is
        // fine.
        #[cfg(feature = "emu")]
        if screen == Screen::Emu {
            dirty |= emu.tick(&vm, &mut fbuf);
        }

        // ---- audio Player: decode/read + resample the next slice into the ring
        // every iteration (the I2S DMA buffer gives the slack; UI repaint is paced
        // by the 40 ms tick below). ----
        if screen == Screen::Player {
            player.pump(&vm);
        }
        // ---- audio: keep the DMA buffer topped up (robust against slow frames) ----
        let mut audio_stuck = false;
        if let Some(tx) = transfer.as_mut() {
            loop {
                let avail = match tx.available() {
                    Ok(a) => a,
                    Err(_) => {
                        // circular DMA wedged (fully underran -> all descriptors CPU-owned)
                        audio_stuck = true;
                        #[cfg(feature = "audiodiag")]
                        {
                            diag_late += 1;
                        }
                        0
                    }
                };
                #[cfg(feature = "audiodiag")]
                if avail > diag_avail_max {
                    diag_avail_max = avail;
                }
                if avail < chunk_bytes {
                    break;
                }
                // While a Game Boy game is playing, feed the I2S from its APU instead
                // of the synth (the synth is silenced on entry).
                let mut filled = false;
                #[cfg(feature = "emu")]
                if screen == Screen::Emu {
                    apps::emu::audio_fill(&mut samples);
                    filled = true;
                }
                if screen == Screen::Player {
                    player.audio_fill(&mut samples);
                    filled = true;
                }
                if !filled {
                    synth.fill_stereo(&mut samples);
                }
                // ESP32-S3 is little-endian, so the i16 sample buffer already has the
                // exact byte layout the I2S DMA wants — reinterpret it as bytes instead
                // of repacking element by element (this runs in the tight audio feed).
                let raw = unsafe {
                    core::slice::from_raw_parts(samples.as_ptr() as *const u8, core::mem::size_of_val(&samples))
                };
                let pushed = match tx.push(raw) {
                    Ok(n) => n,
                    Err(_) => {
                        audio_stuck = true;
                        #[cfg(feature = "audiodiag")]
                        {
                            diag_late += 1;
                        }
                        0
                    }
                };
                #[cfg(feature = "audiodiag")]
                {
                    diag_pushed += pushed as u32;
                }
                if pushed == 0 {
                    break;
                }
            }
        }
        // Recover a wedged circular DMA (Late forever after a full underrun, e.g. the
        // emulator's launch SD reads stall the loop past the buffer): drop + recreate
        // the transfer to resync. Rate-limited so a chronically slow source (the CGB
        // emu from SD) just goes choppy instead of thrashing the DMA every iteration.
        if audio_stuck && audio_recover_at.elapsed() >= Duration::from_millis(40) {
            audio_recover_at = Instant::now();
            drop(transfer.take()); // Drop stops the DMA (frees the descriptors)
            // SAFETY: see the pointer setup above — single transfer at a time, and the
            // backing i2s_tx/tx_buffer outlive it.
            transfer = unsafe { (*i2s_tx_ptr).write_dma_circular(&mut *tx_buf_ptr) }.ok();
        }
        #[cfg(feature = "audiodiag")]
        if diag_last.elapsed() >= Duration::from_secs(1) {
            diag_last = Instant::now();
            esp_println::println!(
                "[AUD] pushed={}/64000 B/s  underruns={}  availmax={}",
                diag_pushed,
                diag_late,
                diag_avail_max
            );
            diag_pushed = 0;
            diag_late = 0;
            diag_avail_max = 0;
        }

        // ---- animation tick (~40 ms): LED accent wave (always live) + VU ----
        // ---- radar live rescan: WiFi/BLE scanner + AP-select keep refreshing so
        //      distances/blips track as you move (proximity). Scans block briefly,
        //      so the sweep pauses during each; the anim tick redraws fresh after.
        if screen == Screen::Hacking && !screen_off {
            match hacking.radar_kind() {
                Some(hacking::RadarKind::Wifi) => {
                    // fast: track the highlighted AP's own channel (~0.5 s)
                    if last_fast_scan.elapsed() >= Duration::from_millis(500) {
                        last_fast_scan = Instant::now();
                        if let Some(ch) = hacking.selected_channel() {
                            if let Some(aps) = radio.scan_channel(ch) {
                                for a in &aps {
                                    hacking.merge_ap(a.ssid.as_str(), a.bssid, a.rssi, a.channel, a.auth);
                                }
                            }
                        }
                    }
                    // slow: full refresh + age-out stale APs (~1.6 s)
                    if last_full_scan.elapsed() >= Duration::from_millis(1600) {
                        last_full_scan = Instant::now();
                        if let Some(aps) = radio.scan() {
                            hacking.begin_wifi_merge();
                            for a in &aps {
                                hacking.merge_ap(a.ssid.as_str(), a.bssid, a.rssi, a.channel, a.auth);
                            }
                        }
                    }
                }
                Some(hacking::RadarKind::Ble) => {
                    // BLE scan is a long blocking window; rescan less often so the
                    // radar animates smoothly between sweeps (was 2.5 s -> jerky).
                    if last_full_scan.elapsed() >= Duration::from_millis(5000) {
                        last_full_scan = Instant::now();
                        if let Some(devs) = radio.ble_scan() {
                            hacking.begin_ble_merge();
                            for dv in &devs {
                                hacking.merge_ble(dv.addr, dv.rssi, dv.name.as_deref());
                            }
                        }
                    }
                }
                None => {}
            }
        }

        if last_anim.elapsed() >= Duration::from_millis(40) {
            last_anim = Instant::now();
            led_phase += 0.18;
            if led_phase > 6.2831 {
                led_phase -= 6.2831;
            }
            // Drive the LED only at full screen brightness (see led_brightness): on a
            // dimmed backlight the shared power rail ripples and the LED flickers.
            let led_user = led_brightness(config.led_on, config.led_bright, config.disp_bright);
            if led_user > 0.0 {
                let (r, g, b) = ws2812::accent_wave(theme::accent(), synth.level(), led_phase, led_user);
                let data = ws2812::encode(r, g, b);
                led = match led.transmit(&data) {
                    Ok(tx) => tx.wait().unwrap_or_else(|(_, c)| c),
                    Err((_, c)) => c,
                };
                led_was_dark = false;
            } else if !led_was_dark {
                // LED just went dark — clear it once, then skip the sinf/encode/
                // blocking transmit on every later tick while it stays off.
                let data = ws2812::encode(0, 0, 0);
                led = match led.transmit(&data) {
                    Ok(tx) => tx.wait().unwrap_or_else(|(_, c)| c),
                    Err((_, c)) => c,
                };
                led_was_dark = true;
            }
            // per-app periodic updates (each app rate-limits itself and only
            // reports `true` when it actually redrew, so we blit no more than needed)
            match screen {
                Screen::Synth => {
                    // Only repaint (and blit 64 KB) when the meter's pixel fill
                    // actually moves; an idle Synth screen otherwise blits at 25 Hz
                    // for a bit-identical frame. meter() quantises level to this fw.
                    let vw = theme::W - 2 * theme::PAD;
                    let fw = (vw as f32 * synth.level().clamp(0.0, 1.0)) as i32;
                    if fw != last_vu_fw {
                        last_vu_fw = fw;
                        ui::draw_vu(&mut fbuf, synth.level(), synth.active_voices(), mode);
                        dirty = true;
                    }
                }
                Screen::Games => dirty |= games.tick(&mut fbuf),
                Screen::Hacking => dirty |= hacking.tick(&mut i2c, &mut fbuf),
                Screen::Misc => dirty |= misc.tick(&mut i2c, &mut fbuf),
                Screen::Player => dirty |= player.tick(&mut fbuf),
                Screen::Stopwatch => dirty |= stopwatch.tick(&mut fbuf),
                Screen::Sysinfo => dirty |= sysinfo.tick(&mut fbuf),
                _ => {}
            }
        }

        // ---- battery: timed re-check every 30 s (not every frame) ----
        if last_batt_check.elapsed() >= Duration::from_secs(30) {
            last_batt_check = Instant::now();
            let mv = adc.read_blocking(&mut bat_pin) as u32 * 2;
            let present = (3000..=4500).contains(&mv);
            let pct = battery::mv_to_percent(mv as u16);
            if pct != last_batt || present != last_present {
                last_batt = pct;
                last_present = present;
                battery::set(pct, present);
                if !screen_off {
                    match screen {
                        Screen::Charge => {
                            charge::draw(&mut fbuf, false);
                            dirty = true;
                        }
                        Screen::Browser => {}
                        #[cfg(feature = "emu")]
                        Screen::Emu => {} // don't paint the battery over the game frame
                        _ => {
                            theme::draw_battery(&mut fbuf, theme::W - theme::PAD, 3);
                            dirty = true;
                        }
                    }
                }
            }
        }

        // ---- Charge screen: blank the backlight after idle (device keeps charging) ----
        if screen == Screen::Charge && !screen_off && last_input.elapsed() >= Duration::from_secs(20) {
            let _ = backlight.set_duty(0);
            screen_off = true;
        }

        // ---- blit the whole framebuffer in one pass (no clear-then-draw flash) ----
        if dirty {
            let _ = display.set_pixels(0, 0, (fb::W - 1) as u16, (fb::H - 1) as u16, fbuf.pixels());
            dirty = false;
        }
    }
}

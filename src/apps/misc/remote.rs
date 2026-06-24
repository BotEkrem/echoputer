//! Remote — turns the Cardputer into a HID mouse + keyboard for a connected host.
//!
//! Connection type (G0 toggles): **Bluetooth** (default — a phase-2 stub for now,
//! so opening the app claims nothing and the USB-Serial-JTAG console stays alive)
//! or **USB** (working: enumerates over the S3 USB-OTG, claimed lazily on switch).
//! Mode (Tab toggles): Mouse (arrow keys move; `-`/`=` adjust DPI step) or Keyboard
//! (the Cardputer keyboard types straight through as HID; the Shift key toggles a
//! sticky shift). Backspace exits (handled by the Misc launcher).

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

use crate::hal::keymap;
use crate::hal::usb_hid::{UsbHid, UsbParts};
use crate::i18n::remote as tr;
use crate::{i18n, theme};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Mouse,
    Keyboard,
}

#[derive(Clone, Copy, PartialEq)]
enum Conn {
    Bluetooth,
    Usb,
}

pub struct Remote {
    mode: Mode,
    conn: Conn,
    dpi: i8,        // mouse pixel step per arrow press (1..=60)
    shift: bool,    // sticky shift toggle (keyboard mode)
    parts: Option<UsbParts>, // taken on first USB claim
    usb: Option<UsbHid>,
    last_ready: bool,
}

impl Remote {
    pub fn new(parts: UsbParts) -> Self {
        Remote {
            mode: Mode::Mouse,
            conn: Conn::Bluetooth, // default: claim nothing, keep the console alive
            dpi: 8,
            shift: false,
            parts: Some(parts),
            usb: None,
            last_ready: false,
        }
    }

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.draw(d);
    }

    pub fn exit(&mut self) {
        // A claimed USB-OTG device lingers until reboot (claim-once); switching away
        // from USB just stops polling/sending. Nothing to free here.
    }

    /// True in USB keyboard mode — main suppresses hold-to-repeat then, so a held
    /// key sends ONE HID report (no host-side key spam). Mouse mode keeps repeat.
    pub fn is_typing(&self) -> bool {
        self.conn == Conn::Usb && self.mode == Mode::Keyboard
    }

    /// G0: toggle the connection type. Switching to USB lazily claims the OTG
    /// peripheral (the console is lost from here until reboot).
    pub fn toggle_conn<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.conn = match self.conn {
            Conn::Bluetooth => {
                if self.usb.is_none() {
                    if let Some(p) = self.parts.take() {
                        self.usb = Some(UsbHid::claim(p));
                    }
                }
                Conn::Usb
            }
            Conn::Usb => Conn::Bluetooth,
        };
        self.last_ready = false;
        self.draw(d);
    }

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) {
        if rc == keymap::K_TAB {
            self.mode = match self.mode {
                Mode::Mouse => Mode::Keyboard,
                Mode::Keyboard => Mode::Mouse,
            };
            self.draw(d);
            return;
        }
        if self.conn != Conn::Usb {
            return; // Bluetooth is a phase-2 stub: claims nothing, sends nothing
        }
        match self.mode {
            Mode::Mouse => match rc {
                crate::K_UP => self.send_mouse(0, -self.dpi),
                crate::K_DOWN => self.send_mouse(0, self.dpi),
                crate::K_LEFT => self.send_mouse(-self.dpi, 0),
                crate::K_RIGHT => self.send_mouse(self.dpi, 0),
                keymap::K_MINUS => {
                    self.dpi = (self.dpi - 2).max(1);
                    self.draw(d);
                }
                keymap::K_EQUALS => {
                    self.dpi = (self.dpi + 2).min(60);
                    self.draw(d);
                }
                _ => {}
            },
            Mode::Keyboard => {
                if rc == keymap::K_SHIFT {
                    self.shift = !self.shift;
                    self.draw(d);
                    return;
                }
                let modi = if self.shift { 0x02 } else { 0x00 }; // LeftShift
                let usage = match rc {
                    crate::K_UP => Some(0x52),
                    crate::K_DOWN => Some(0x51),
                    crate::K_LEFT => Some(0x50),
                    crate::K_RIGHT => Some(0x4F),
                    _ => keymap::hid_usage(rc.0, rc.1),
                };
                if let Some(u) = usage {
                    self.send_key(modi, u);
                }
            }
        }
    }

    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        if self.conn == Conn::Usb {
            if let Some(u) = self.usb.as_mut() {
                u.poll();
                let r = u.ready();
                if r != self.last_ready {
                    self.last_ready = r;
                    self.draw(d);
                    return true;
                }
            }
        }
        false
    }

    fn send_mouse(&mut self, dx: i8, dy: i8) {
        if let Some(u) = self.usb.as_mut() {
            u.mouse_move(dx, dy);
        }
    }

    fn send_key(&mut self, modifier: u8, usage: u8) {
        if let Some(u) = self.usb.as_mut() {
            u.key(modifier, usage);
        }
    }

    fn draw<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, i18n::t(tr::TITLE));

        let conn = match self.conn {
            Conn::Usb => i18n::t(tr::USB),
            Conn::Bluetooth => i18n::t(tr::BLUETOOTH),
        };
        theme::text(d, conn, theme::PAD, 28, theme::TITLE_FONT, theme::accent());

        let mode = match self.mode {
            Mode::Mouse => i18n::t(tr::MOUSE),
            Mode::Keyboard => i18n::t(tr::KEYBOARD),
        };
        theme::text(d, mode, theme::PAD, 46, theme::TITLE_FONT, theme::FG);

        match self.conn {
            Conn::Bluetooth => {
                theme::text_center(d, i18n::t(tr::BLE_SOON), theme::W / 2, 82, theme::BODY_FONT, theme::MUTED);
            }
            Conn::Usb => {
                let ready = self.usb.as_ref().map(|u| u.ready()).unwrap_or(false);
                let status = if ready { i18n::t(tr::USB_READY) } else { i18n::t(tr::USB_WAIT) };
                theme::text(d, status, theme::PAD, 66, theme::BODY_FONT, theme::MUTED);
                match self.mode {
                    Mode::Mouse => {
                        let mut nb = [0u8; 8];
                        theme::text(d, i18n::t(tr::DPI), theme::PAD, 84, theme::BODY_FONT, theme::FAINT);
                        theme::text(d, fmt_u(self.dpi, &mut nb), theme::PAD + 30, 84, theme::BODY_FONT, theme::FG);
                    }
                    Mode::Keyboard => {
                        if self.shift {
                            theme::text(d, i18n::t(tr::SHIFT_ON), theme::PAD, 84, theme::BODY_FONT, theme::accent());
                        }
                    }
                }
            }
        }
        theme::hint(d, i18n::t(tr::HINT));
    }
}

/// Tiny unsigned-int formatter for the DPI readout (no alloc).
fn fmt_u(v: i8, buf: &mut [u8; 8]) -> &str {
    let mut n = v.max(0) as u8;
    let mut i = buf.len();
    if n == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10);
        n /= 10;
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

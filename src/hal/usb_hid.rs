//! USB HID transport for the Misc "Remote" app — the Cardputer enumerates as a
//! USB mouse + keyboard over the S3's native USB-OTG (the USB-C port).
//!
//! Claimed LAZILY: nothing here touches the USB peripheral until the Remote app
//! is switched to USB mode, so the USB-Serial-JTAG console (a separate peripheral)
//! stays alive by default. The endpoint memory + bus allocator are heap-leaked on
//! first claim (claim-once, never freed) so there is ZERO .bss / stack cost on
//! builds that never use it (notably the RAM-tight emugbc build).
//!
//! Two HID interfaces (mouse + keyboard) are exposed; reports go out as the host
//! polls. Driven by `poll()` each Remote tick.

use alloc::boxed::Box;

use esp_hal::otg_fs::{Usb, UsbBus};
use esp_hal::peripherals::{GPIO19, GPIO20, USB0};
use usb_device::bus::UsbBusAllocator;
use usb_device::class::UsbClass;
use usb_device::device::{UsbDevice, UsbDeviceBuilder, UsbDeviceState, UsbVidPid};
use usbd_hid::descriptor::{KeyboardReport, MouseReport, SerializedDescriptor};
use usbd_hid::hid_class::HIDClass;

type Bus = UsbBus<Usb<'static>>;

/// The USB-OTG peripheral + its D+/D- pins, owned by `main` and handed to the
/// Remote app so it can claim them lazily. On the S3 the USB-C D+/D- are the
/// dedicated OTG pins (D+ = GPIO20, D- = GPIO19).
pub struct UsbParts {
    pub usb0: USB0<'static>,
    pub dp: GPIO20<'static>,
    pub dm: GPIO19<'static>,
}

/// A live USB HID device (mouse + keyboard). Built by [`UsbHid::claim`].
pub struct UsbHid {
    dev: UsbDevice<'static, Bus>,
    mouse: HIDClass<'static, Bus>,
    kbd: HIDClass<'static, Bus>,
}

impl UsbHid {
    /// Bring up the USB-OTG device. Call once, when the user switches to USB mode.
    /// The endpoint memory + bus allocator are leaked to `'static` (the device
    /// lives until reboot — switching back to BLE just stops polling/sending).
    pub fn claim(p: UsbParts) -> Self {
        let usb = Usb::new(p.usb0, p.dp, p.dm);
        let ep: &'static mut [u32] = Box::leak(Box::new([0u32; 256]));
        let alloc: &'static UsbBusAllocator<Bus> = Box::leak(Box::new(UsbBus::new(usb, ep)));
        let mouse = HIDClass::new(alloc, MouseReport::desc(), 8);
        let kbd = HIDClass::new(alloc, KeyboardReport::desc(), 8);
        // 0x16c0/0x27dd is the pid.codes shared VID/PID for libre HID devices.
        let dev = UsbDeviceBuilder::new(alloc, UsbVidPid(0x16c0, 0x27dd))
            .device_class(0)
            .build();
        UsbHid { dev, mouse, kbd }
    }

    /// Service the USB stack (enumeration + control transfers). Call frequently.
    pub fn poll(&mut self) {
        let m: &mut dyn UsbClass<Bus> = &mut self.mouse;
        let k: &mut dyn UsbClass<Bus> = &mut self.kbd;
        self.dev.poll(&mut [m, k]);
    }

    /// True once the host has configured the device (reports will be delivered).
    pub fn ready(&self) -> bool {
        self.dev.state() == UsbDeviceState::Configured
    }

    /// Relative pointer move (signed pixel deltas).
    pub fn mouse_move(&mut self, dx: i8, dy: i8) {
        let _ = self.mouse.push_input(&MouseReport { buttons: 0, x: dx, y: dy, wheel: 0, pan: 0 });
    }

    /// Send one keypress (HID usage + modifier byte), then release.
    pub fn key(&mut self, modifier: u8, usage: u8) {
        let _ = self.kbd.push_input(&KeyboardReport {
            modifier,
            reserved: 0,
            leds: 0,
            keycodes: [usage, 0, 0, 0, 0, 0],
        });
        let _ = self.kbd.push_input(&KeyboardReport {
            modifier: 0,
            reserved: 0,
            leds: 0,
            keycodes: [0; 6],
        });
    }
}

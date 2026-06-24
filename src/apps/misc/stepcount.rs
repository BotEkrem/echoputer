//! Step counter — counts steps from the BMI270 accelerometer. Low-passes the accel
//! magnitude to a ~1 g baseline and counts upward peaks (foot strikes) above a
//! threshold, with a refractory period so one step isn't double-counted. ENTER resets.
//!
//! Software peak-detect on the raw accel (the BMI270 has a hardware step feature, but
//! that needs the config's feature pages enabled — this is simpler and good enough).

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};
use embedded_hal::i2c::I2c;
use esp_hal::time::{Duration, Instant};

use crate::hal::{bmi270, keymap};
use crate::i18n::stepcount;
use crate::{i18n, theme};

const TH: f32 = 0.22; // g above baseline to register a peak (tune for sensitivity)
const REFRACTORY_MS: u64 = 400; // min gap between counted steps (~walking cadence)

pub struct StepCount {
    count: u32,
    avg: f32,    // low-passed magnitude baseline (~1 g at rest)
    armed: bool, // true after a trough; a peak then counts
    last: Instant,
    shown: u32, // last drawn count
}

impl StepCount {
    pub fn new() -> Self {
        StepCount {
            count: 0,
            avg: 1.0,
            armed: false,
            last: Instant::now(),
            shown: u32::MAX,
        }
    }

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        self.shown = u32::MAX; // force a redraw
        theme::clear(d);
        theme::topbar(d, i18n::t(stepcount::STEP_COUNTER));
        if !bmi270::ready() {
            theme::text_center(d, i18n::t(stepcount::NO_IMU), theme::W / 2, 56, theme::TITLE_FONT, theme::DESTRUCTIVE);
            theme::hint(d, i18n::t(stepcount::BACK));
            return;
        }
        theme::text_center(d, i18n::t(stepcount::STEPS), theme::W / 2, 84, theme::BODY_FONT, theme::MUTED);
        self.draw_count(d);
        theme::hint(d, i18n::t(stepcount::WALK_HINT));
    }

    pub fn exit(&mut self) {}

    pub fn on_key(&mut self, rc: (u8, u8), d: &mut impl DrawTarget<Color = Rgb565>) {
        if rc == crate::K_ENTER || keymap::ch_shift(rc.0, rc.1, false) == Some(b'r') {
            self.count = 0;
            self.draw_count(d);
            self.shown = 0;
        }
    }

    pub fn tick(&mut self, i2c: &mut impl I2c, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        if !bmi270::ready() {
            return false;
        }
        let Some(a) = bmi270::read_accel(i2c) else {
            return false;
        };
        let mag = libm::sqrtf(a[0] * a[0] + a[1] * a[1] + a[2] * a[2]);
        self.avg = self.avg * 0.9 + mag * 0.1;
        let dynamic = mag - self.avg;

        if self.armed && dynamic > TH && self.last.elapsed() >= Duration::from_millis(REFRACTORY_MS) {
            self.count += 1;
            self.armed = false;
            self.last = Instant::now();
        } else if dynamic < -TH * 0.5 {
            self.armed = true; // re-arm on the trough between strides
        }

        if self.count != self.shown {
            self.draw_count(d);
            self.shown = self.count;
            true
        } else {
            false
        }
    }

    fn draw_count(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        let mut nb = [0u8; 12];
        let s = fmt_u32(self.count, &mut nb);
        theme::fill(d, 0, 40, theme::W as u32, 28, theme::BG);
        theme::text_center(d, s, theme::W / 2, 56, &FONT_10X20, theme::accent());
    }
}

fn fmt_u32(v: u32, buf: &mut [u8; 12]) -> &str {
    let mut tmp = [0u8; 10];
    let mut n = v;
    let mut i = 0;
    if n == 0 {
        return "0";
    }
    while n > 0 && i < tmp.len() {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    let mut j = 0;
    while i > 0 && j < buf.len() {
        i -= 1;
        buf[j] = tmp[i];
        j += 1;
    }
    core::str::from_utf8(&buf[..j]).unwrap_or("0")
}

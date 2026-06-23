//! Bubble level (su terazisi) — a spirit level from the BMI270 accelerometer. Lay the
//! Cardputer flat and the ball centres; tilt it and the ball rolls toward the low side.
//! The tilt angle (degrees from flat) is shown under the ring.
//!
//! Layout: outer ring centred at (120, 64) r38 (y26..102), angle readout at y106, hint
//! at the standard HINT_Y — no overlaps. Reads the accel in tick() via the shared I2C.

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle},
};
use embedded_hal::i2c::I2c;

use crate::hal::bmi270;
use crate::{i18n, theme};

const CX: i32 = theme::W / 2; // 120
const CY: i32 = 64;
const R: i32 = 38; // outer ring radius
const BR: i32 = 7; // ball radius
const K: f32 = 45.0; // g -> pixels (tune on-device if travel feels off)

/// Greenish "level" colour (Rgb565 r5/g6/b5).
const GREEN: Rgb565 = Rgb565::new(8, 56, 12);

pub struct Level {}

impl Level {
    pub fn new() -> Self {
        Level {}
    }

    pub fn enter(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::clear(d);
        theme::topbar(d, i18n::t("Level", "Su Terazisi"));
        if !bmi270::ready() {
            theme::text_center(d, i18n::t("no IMU detected", "IMU bulunamadi"), CX, CY, theme::TITLE_FONT, theme::DESTRUCTIVE);
            theme::hint(d, i18n::t("` back", "` geri"));
            return;
        }
        // static outer ring (the ball + crosshair are repainted each tick inside it)
        let st = PrimitiveStyle::with_stroke(theme::MUTED, 1);
        let _ = Circle::new(Point::new(CX - R, CY - R), (2 * R) as u32).into_styled(st).draw(d);
        theme::hint(d, i18n::t("lay flat; ball centres when level", "duz tut; teraziyken top ortalanir"));
    }

    pub fn exit(&mut self) {}

    pub fn on_key(&mut self, _rc: (u8, u8), _d: &mut impl DrawTarget<Color = Rgb565>) {}

    pub fn tick(&mut self, i2c: &mut impl I2c, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        if !bmi270::ready() {
            return false;
        }
        let Some(a) = bmi270::read_accel(i2c) else {
            return false;
        };
        let (ax, ay, az) = (a[0], a[1], a[2]);

        // Ball offset — rolls toward the low side. Sign/orientation is easy to tune
        // on-device (flip ax/ay here if it moves the wrong way).
        let max = (R - BR - 2) as f32;
        let bx = CX + (ax * K).clamp(-max, max) as i32;
        let by = CY + (ay * K).clamp(-max, max) as i32;

        // Tilt from flat: angle between the gravity vector and the Z (flat) axis.
        let horiz = libm::sqrtf(ax * ax + ay * ay);
        let ang = libm::atan2f(horiz, libm::fabsf(az)) * 180.0 / core::f32::consts::PI;
        let level = ang < 2.0;

        // Repaint the disc interior: erase, crosshair + centre target, then the ball.
        theme::fill(d, CX - R + 1, CY - R + 1, (2 * R - 2) as u32, (2 * R - 2) as u32, theme::BG);
        let faint = PrimitiveStyle::with_stroke(theme::FAINT, 1);
        let _ = Line::new(Point::new(CX - R + 5, CY), Point::new(CX + R - 5, CY)).into_styled(faint).draw(d);
        let _ = Line::new(Point::new(CX, CY - R + 5), Point::new(CX, CY + R - 5)).into_styled(faint).draw(d);
        let _ = Circle::new(Point::new(CX - 9, CY - 9), 18).into_styled(faint).draw(d);
        let col = if level { GREEN } else { theme::accent() };
        let _ = Circle::new(Point::new(bx - BR, by - BR), (2 * BR) as u32).into_styled(PrimitiveStyle::with_fill(col)).draw(d);

        // Angle readout under the ring.
        let mut tb = [0u8; 10];
        let s = fmt_ang(ang, &mut tb);
        theme::fill(d, 0, CY + R + 2, theme::W as u32, 12, theme::BG);
        theme::text_center(d, s, CX, CY + R + 4, theme::BODY_FONT, if level { GREEN } else { theme::MUTED });
        true
    }
}

/// Tilt angle -> "12.3 deg" (no degree glyph in the ASCII font).
fn fmt_ang(deg: f32, buf: &mut [u8; 10]) -> &str {
    let tenths = (deg * 10.0) as u32; // deg >= 0
    let mut i = 0;
    push_u32(buf, &mut i, tenths / 10);
    push(buf, &mut i, b'.');
    push(buf, &mut i, b'0' + (tenths % 10) as u8);
    for &b in b" deg" {
        push(buf, &mut i, b);
    }
    core::str::from_utf8(&buf[..i]).unwrap_or("0 deg")
}

fn push(buf: &mut [u8], i: &mut usize, b: u8) {
    if *i < buf.len() {
        buf[*i] = b;
        *i += 1;
    }
}

fn push_u32(buf: &mut [u8], i: &mut usize, v: u32) {
    let mut tmp = [0u8; 10];
    let mut n = v;
    let mut c = 0;
    if n == 0 {
        push(buf, i, b'0');
        return;
    }
    while n > 0 && c < tmp.len() {
        tmp[c] = b'0' + (n % 10) as u8;
        n /= 10;
        c += 1;
    }
    while c > 0 {
        c -= 1;
        push(buf, i, tmp[c]);
    }
}

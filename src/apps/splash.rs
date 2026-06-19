//! Boot intro (~3 s): echo ripples → the "Echoputer" wordmark drawn in a custom
//! geometric stroke font (capital E + lowercase) → accent underline → tagline.

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle},
};
use embedded_hal::delay::DelayNs;

use crate::theme;

// Custom geometric letterforms. Caps fill rows 0..10; lowercase x-height 4..10,
// ascenders to 0, descender (p) to 14. Scaled by S.
const S: i32 = 3;
const GAP: i32 = 4;
const CELL_W: i32 = 6;
const STROKE: u32 = 2;

const WORD: &[u8] = b"Echoputer";

fn segments(c: u8) -> &'static [(i32, i32, i32, i32)] {
    match c {
        b'E' => &[(0, 0, 0, 10), (0, 0, 5, 0), (0, 5, 4, 5), (0, 10, 5, 10)],
        b'c' => &[(4, 4, 0, 4), (0, 4, 0, 10), (0, 10, 4, 10)],
        b'h' => &[(0, 0, 0, 10), (0, 4, 4, 4), (4, 4, 4, 10)],
        b'o' => &[(0, 4, 4, 4), (4, 4, 4, 10), (4, 10, 0, 10), (0, 10, 0, 4)],
        b'p' => &[(0, 4, 0, 14), (0, 4, 4, 4), (4, 4, 4, 8), (4, 8, 0, 8)],
        b'u' => &[(0, 4, 0, 10), (0, 10, 4, 10), (4, 4, 4, 10)],
        b't' => &[(2, 1, 2, 10), (0, 4, 4, 4), (2, 10, 4, 10)],
        b'e' => &[(0, 4, 4, 4), (0, 4, 0, 10), (0, 10, 4, 10), (0, 7, 4, 7), (4, 4, 4, 7)],
        b'r' => &[(0, 4, 0, 10), (0, 4, 3, 4)],
        _ => &[],
    }
}

fn pitch() -> i32 {
    CELL_W * S + GAP
}

/// Center the wordmark on its ACTUAL ink extent. Glyph widths vary (caps fill ~5 cols,
/// 'r' only 3), so the fixed-cell estimate left a wider gap on the right and drifted the
/// word visibly left. Measure the real min/max x over every segment and center that.
fn word_start_x() -> i32 {
    let mut min_x = i32::MAX;
    let mut max_x = i32::MIN;
    for (k, &c) in WORD.iter().enumerate() {
        let off = k as i32 * pitch();
        for &(x0, _, x1, _) in segments(c) {
            min_x = min_x.min(off + x0.min(x1) * S);
            max_x = max_x.max(off + x0.max(x1) * S);
        }
    }
    theme::W / 2 - (min_x + max_x) / 2
}

fn draw_letter(d: &mut impl DrawTarget<Color = Rgb565>, c: u8, px: i32, py: i32, color: Rgb565) {
    let st = PrimitiveStyle::with_stroke(color, STROKE);
    for &(x0, y0, x1, y1) in segments(c) {
        let _ = Line::new(
            Point::new(px + x0 * S, py + y0 * S),
            Point::new(px + x1 * S, py + y1 * S),
        )
        .into_styled(st)
        .draw(d);
    }
}

fn dim(c: Rgb565, num: i32, den: i32) -> Rgb565 {
    Rgb565::new(
        (c.r() as i32 * num / den) as u8,
        (c.g() as i32 * num / den) as u8,
        (c.b() as i32 * num / den) as u8,
    )
}

/// Run the whole intro synchronously, then return.
pub fn run(d: &mut impl DrawTarget<Color = Rgb565>, delay: &mut impl DelayNs) {
    let cx = theme::W / 2;
    let py = 38;
    let cy = 58;
    // The intro is monochrome white, independent of the per-app accent (which boots
    // red for Hacking). The home screen takes over the colour once we exit.
    let tint = theme::FG;

    theme::clear(d);

    // 1) echo ripples expanding outward, decaying
    for i in 0..9i32 {
        let r = 6 + i * 13;
        let c = dim(tint, 9 - i, 9);
        let _ = Circle::with_center(Point::new(cx, cy), (r * 2) as u32)
            .into_styled(PrimitiveStyle::with_stroke(c, 1))
            .draw(d);
        delay.delay_ms(45);
    }
    delay.delay_ms(160);

    // 2) reveal the wordmark letter by letter
    theme::clear(d);
    let sx = word_start_x();
    for k in 0..WORD.len() {
        draw_letter(d, WORD[k], sx + k as i32 * pitch(), py, theme::FG);
        delay.delay_ms(70);
    }

    // 3) accent underline sweeps out from the centre (below the descender)
    let uy = 84;
    let full = 150;
    for step in 1..=12 {
        let w = full * step / 12;
        theme::fill(d, cx - w / 2, uy, w as u32, 2, tint);
        delay.delay_ms(22);
    }

    // 4) tagline "by BotEkrem" (by muted, BotEkrem accent) + hold
    let ty = 98;
    let x0 = (theme::W - (3 + 8) * 6) / 2;
    theme::text(d, "by", x0, ty, theme::BODY_FONT, theme::MUTED);
    theme::text(d, "BotEkrem", x0 + 18, ty, theme::BODY_FONT, tint);
    delay.delay_ms(1000);
}

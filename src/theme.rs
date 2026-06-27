//! Shared visual theme — a minimal, shadcn-inspired system used by every screen.
//!
//! Truly neutral (black / white / gray) surfaces with ONE accent colour that the
//! user can change in Settings (default: gold). Monospace type throughout.

use core::sync::atomic::{AtomicU32, Ordering};

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_8X13_BOLD},
        MonoFont, MonoTextStyle,
    },
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};

pub const W: i32 = 240;
pub const H: i32 = 135;
pub const PAD: i32 = 8;
pub const TOPBAR_Y: i32 = 17;
pub const HINT_Y: i32 = H - 12;

// ---- neutral grayscale tokens (gray v -> Rgb565::new(v, 2v, v)) ----
pub const BG: Rgb565 = Rgb565::new(1, 2, 1); // near-black
pub const SURFACE: Rgb565 = Rgb565::new(3, 6, 3); // card
pub const SURFACE2: Rgb565 = Rgb565::new(5, 10, 5); // raised / selected
pub const BORDER: Rgb565 = Rgb565::new(6, 12, 6); // hairline
pub const BORDER_HI: Rgb565 = Rgb565::new(9, 18, 9);
pub const FG: Rgb565 = Rgb565::new(31, 63, 31); // white
pub const MUTED: Rgb565 = Rgb565::new(19, 38, 19); // secondary
pub const FAINT: Rgb565 = Rgb565::new(13, 26, 13); // hints
pub const DESTRUCTIVE: Rgb565 = Rgb565::new(29, 17, 8); // red

// ---- accent: one dynamic colour, set per-app from the `palette` hue wheel ----
// Stored as packed RGB565 bits. The default (red = wheel slot 0) is overwritten
// the first time the menu draws.
static ACCENT: AtomicU32 = AtomicU32::new(0xF800);

/// Set the accent from an RGB565 colour (used per-app; see `crate::palette`).
pub fn set_accent_rgb(c: Rgb565) {
    ACCENT.store(c.into_storage() as u32, Ordering::Relaxed);
}

pub fn accent() -> Rgb565 {
    rgb565(ACCENT.load(Ordering::Relaxed) as u16)
}

pub const TITLE_FONT: &MonoFont = &FONT_8X13_BOLD;
pub const BODY_FONT: &MonoFont = &FONT_6X10;

// ---- primitives ----

pub fn clear(d: &mut impl DrawTarget<Color = Rgb565>) {
    let _ = d.clear(BG);
}

pub fn fill(d: &mut impl DrawTarget<Color = Rgb565>, x: i32, y: i32, w: u32, h: u32, c: Rgb565) {
    let _ = Rectangle::new(Point::new(x, y), Size::new(w, h))
        .into_styled(PrimitiveStyle::with_fill(c))
        .draw(d);
}

pub fn hline(d: &mut impl DrawTarget<Color = Rgb565>, y: i32, c: Rgb565) {
    let _ = Line::new(Point::new(0, y), Point::new(W - 1, y))
        .into_styled(PrimitiveStyle::with_stroke(c, 1))
        .draw(d);
}

/// Arbitrary line from (x0,y0) to (x1,y1). Used by the radar views.
pub fn line(d: &mut impl DrawTarget<Color = Rgb565>, x0: i32, y0: i32, x1: i32, y1: i32, c: Rgb565) {
    let _ = Line::new(Point::new(x0, y0), Point::new(x1, y1))
        .into_styled(PrimitiveStyle::with_stroke(c, 1))
        .draw(d);
}

/// Stroked circle of radius `r` centred at (cx,cy).
pub fn ring(d: &mut impl DrawTarget<Color = Rgb565>, cx: i32, cy: i32, r: i32, c: Rgb565) {
    if r < 1 {
        return;
    }
    let _ = Circle::new(Point::new(cx - r, cy - r), (r * 2) as u32)
        .into_styled(PrimitiveStyle::with_stroke(c, 1))
        .draw(d);
}

/// Filled circle (dot) of radius `r` centred at (cx,cy).
pub fn disc(d: &mut impl DrawTarget<Color = Rgb565>, cx: i32, cy: i32, r: i32, c: Rgb565) {
    if r < 1 {
        let _ = Rectangle::new(Point::new(cx, cy), Size::new(1, 1))
            .into_styled(PrimitiveStyle::with_fill(c))
            .draw(d);
        return;
    }
    let _ = Circle::new(Point::new(cx - r, cy - r), (r * 2) as u32)
        .into_styled(PrimitiveStyle::with_fill(c))
        .draw(d);
}

pub fn text(d: &mut impl DrawTarget<Color = Rgb565>, s: &str, x: i32, y: i32, f: &'static MonoFont, c: Rgb565) {
    let _ = Text::with_baseline(s, Point::new(x, y), MonoTextStyle::new(f, c), Baseline::Top).draw(d);
}

pub fn text_right(d: &mut impl DrawTarget<Color = Rgb565>, s: &str, x_right: i32, y: i32, f: &'static MonoFont, c: Rgb565) {
    let ts = TextStyleBuilder::new().alignment(Alignment::Right).baseline(Baseline::Top).build();
    let _ = Text::with_text_style(s, Point::new(x_right, y), MonoTextStyle::new(f, c), ts).draw(d);
}

pub fn text_center(d: &mut impl DrawTarget<Color = Rgb565>, s: &str, cx: i32, cy: i32, f: &'static MonoFont, c: Rgb565) {
    let ts = TextStyleBuilder::new().alignment(Alignment::Center).baseline(Baseline::Middle).build();
    let _ = Text::with_text_style(s, Point::new(cx, cy), MonoTextStyle::new(f, c), ts).draw(d);
}

/// Rounded card. `accent` Some -> selected/active (accent border + raised fill).
pub fn card(d: &mut impl DrawTarget<Color = Rgb565>, x: i32, y: i32, w: u32, h: u32, accent: Option<Rgb565>) {
    let (border, bg) = match accent {
        Some(a) => (a, SURFACE2),
        None => (BORDER, SURFACE),
    };
    let style = PrimitiveStyleBuilder::new()
        .stroke_color(border)
        .stroke_width(1)
        .fill_color(bg)
        .build();
    let _ = RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(x, y), Size::new(w, h)),
        Size::new(6, 6),
    )
    .into_styled(style)
    .draw(d);
}

/// Top bar: bold title (left), battery indicator (right), hairline divider.
pub fn topbar(d: &mut impl DrawTarget<Color = Rgb565>, title: &str) {
    text(d, title, PAD, 3, TITLE_FONT, FG);
    draw_battery(d, W - PAD, 3);
    hline(d, TOPBAR_Y, BORDER);
}

/// Top-right battery indicator (icon + %). Reads the global battery state.
pub fn draw_battery(d: &mut impl DrawTarget<Color = Rgb565>, x_right: i32, y: i32) {
    fill(d, x_right - 66, y, 66, 13, BG); // band widened to also cover the WiFi glyph
    wifi_icon(d, x_right - 64, y);
    if !crate::hal::battery::present() {
        text_right(d, "--", x_right, y + 1, BODY_FONT, MUTED);
        return;
    }
    let lvl = crate::hal::battery::level();
    let mut nb = [0u8; 5];
    let s = fmt_pct(lvl, &mut nb);
    text_right(d, s, x_right, y + 1, BODY_FONT, MUTED);
    let tw = s.len() as i32 * 6;
    batt_icon(d, x_right - tw - 4 - 16, y, lvl);
}

/// Connectivity glyph just left of the battery — deliberately unmistakable at a
/// glance: bright accent rising bars when the global STA link is up; dim bars
/// with a diagonal slash through them when it's down. Allocation-free and
/// branch-only (it runs in every topbar repaint).
fn wifi_icon(d: &mut impl DrawTarget<Color = Rgb565>, x: i32, y: i32) {
    let on = crate::radio::wifi_connected();
    let col = if on { accent() } else { FAINT };
    let base = y + 11;
    for (i, &h) in [3i32, 6, 9].iter().enumerate() {
        fill(d, x + i as i32 * 4, base - h, 3, h as u32, col);
    }
    if !on {
        // diagonal slash = unmistakably "not connected"
        let _ = Line::new(Point::new(x - 1, y + 2), Point::new(x + 11, y + 11))
            .into_styled(PrimitiveStyle::with_stroke(MUTED, 1))
            .draw(d);
    }
}

fn batt_icon(d: &mut impl DrawTarget<Color = Rgb565>, x: i32, y: i32, lvl: u8) {
    let col = if lvl <= 20 { DESTRUCTIVE } else { accent() };
    let _ = Rectangle::new(Point::new(x, y), Size::new(15, 9))
        .into_styled(PrimitiveStyle::with_stroke(MUTED, 1))
        .draw(d);
    fill(d, x + 15, y + 3, 2, 3, MUTED); // terminal nub
    let fw = ((lvl as i32) * 11 / 100).clamp(0, 11);
    if fw > 0 {
        fill(d, x + 2, y + 2, fw as u32, 5, col);
    }
}

fn fmt_pct(v: u8, buf: &mut [u8; 5]) -> &str {
    let mut tmp = [0u8; 3];
    let mut n = v as u16;
    let mut i = 0;
    if n == 0 {
        tmp[0] = b'0';
        i = 1;
    } else {
        while n > 0 && i < 3 {
            tmp[i] = b'0' + (n % 10) as u8;
            n /= 10;
            i += 1;
        }
    }
    let mut j = 0;
    while i > 0 {
        i -= 1;
        buf[j] = tmp[i];
        j += 1;
    }
    buf[j] = b'%';
    j += 1;
    core::str::from_utf8(&buf[..j]).unwrap_or("?")
}

/// Small rounded badge ending at `x_right`. Returns its left x.
pub fn badge(d: &mut impl DrawTarget<Color = Rgb565>, x_right: i32, y: i32, s: &str, accent: Rgb565) -> i32 {
    let w = s.len() as i32 * 6 + 12;
    let x = x_right - w;
    let style = PrimitiveStyleBuilder::new()
        .stroke_color(accent)
        .stroke_width(1)
        .fill_color(SURFACE2)
        .build();
    let _ = RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(x, y), Size::new(w as u32, 14)),
        Size::new(7, 7),
    )
    .into_styled(style)
    .draw(d);
    text(d, s, x + 6, y + 2, BODY_FONT, accent);
    x
}

/// Faint bottom hint line (ASCII only).
pub fn hint(d: &mut impl DrawTarget<Color = Rgb565>, s: &str) {
    fill(d, 0, HINT_Y - 2, W as u32, 14, BG);
    text(d, s, PAD, HINT_Y, BODY_FONT, FAINT);
}

/// Thin rounded level/progress bar; `frac` 0..1 fills with `accent`.
pub fn meter(d: &mut impl DrawTarget<Color = Rgb565>, x: i32, y: i32, w: i32, h: u32, frac: f32, accent: Rgb565) {
    let _ = RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(x, y), Size::new(w as u32, h)),
        Size::new((h / 2) as u32, (h / 2) as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(SURFACE2))
    .draw(d);
    let f = frac.clamp(0.0, 1.0);
    let fw = (w as f32 * f) as i32;
    if fw > 1 {
        let _ = RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(x, y), Size::new(fw as u32, h)),
            Size::new((h / 2) as u32, (h / 2) as u32),
        )
        .into_styled(PrimitiveStyle::with_fill(accent))
        .draw(d);
    }
}

/// Row of `n` ticks, `filled` in `accent`, rest BORDER_HI.
pub fn ticks(d: &mut impl DrawTarget<Color = Rgb565>, x: i32, y: i32, n: i32, filled: i32, accent: Rgb565) {
    let tw = 10u32;
    let gap = 2;
    for i in 0..n {
        let c = if i < filled { accent } else { BORDER_HI };
        fill(d, x + i * (tw as i32 + gap), y, tw, 6, c);
    }
}

#[inline]
pub fn rgb565(v: u16) -> Rgb565 {
    Rgb565::new(((v >> 11) & 0x1F) as u8, ((v >> 5) & 0x3F) as u8, (v & 0x1F) as u8)
}

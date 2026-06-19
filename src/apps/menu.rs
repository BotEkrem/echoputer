//! Home screen — the launcher/shell you pick apps from. Minimal, theme-driven,
//! 3 cards per view with scrolling; each card carries its app's accent colour.

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle, RoundedRectangle, Triangle},
};

use crate::i18n;
use crate::palette;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    Hacking,
    Repl,
    Synth,
    Browser,
    Charge,
    Settings,
}

pub struct App {
    pub kind: AppKind,
}

// Display order only. The icon, name, sub and action are all keyed off `kind`,
// NOT the array position, so reordering this list rearranges the menu.
pub const APPS: [App; 6] = [
    App { kind: AppKind::Hacking },
    App { kind: AppKind::Repl },
    App { kind: AppKind::Synth },
    App { kind: AppKind::Browser },
    App { kind: AppKind::Charge },
    App { kind: AppKind::Settings },
];

/// Localised display name for an app (resolved at draw time so it follows the
/// language setting live).
fn app_name(k: AppKind) -> &'static str {
    match k {
        AppKind::Hacking => "Hacking",
        AppKind::Repl => "REPL",
        AppKind::Synth => "Synthwave",
        AppKind::Browser => i18n::t("File Browser", "Dosya Tarayici"),
        AppKind::Charge => i18n::t("Charge", "Sarj"),
        AppKind::Settings => i18n::t("Settings", "Ayarlar"),
    }
}

fn app_sub(k: AppKind) -> &'static str {
    match k {
        AppKind::Hacking => i18n::t("WiFi/BLE recon + attacks", "WiFi/BLE kesif + saldiri"),
        AppKind::Repl => i18n::t("interactive scripting shell", "etkilesimli betik kabugu"),
        AppKind::Synth => i18n::t("melodic keyboard synth", "melodik klavye synth"),
        AppKind::Browser => i18n::t("browse + manage SD", "SD gez + yonet"),
        AppKind::Charge => i18n::t("battery status / charging", "pil durumu / sarj"),
        AppKind::Settings => i18n::t("theme + app preferences", "tema + uygulama ayarlari"),
    }
}

pub const VISIBLE: usize = 3;
const CARD_Y0: i32 = 22;
const CARD_H: u32 = 30;
const CARD_STEP: i32 = 34;

fn draw_icon(d: &mut impl DrawTarget<Color = Rgb565>, kind: AppKind, x: i32, y: i32, c: Rgb565) {
    let st = PrimitiveStyle::with_stroke(c, 1);
    let fl = PrimitiveStyle::with_fill(c);
    match kind {
        AppKind::Synth => {
            // musical note
            let _ = Circle::new(Point::new(x, y + 11), 6).into_styled(fl).draw(d);
            let _ = Line::new(Point::new(x + 5, y + 14), Point::new(x + 5, y)).into_styled(st).draw(d);
            let _ = Line::new(Point::new(x + 5, y), Point::new(x + 12, y + 3)).into_styled(st).draw(d);
        }
        AppKind::Browser => {
            // folder
            let _ = Rectangle::new(Point::new(x, y + 1), Size::new(7, 4)).into_styled(fl).draw(d);
            let _ = RoundedRectangle::with_equal_corners(
                Rectangle::new(Point::new(x, y + 4), Size::new(17, 12)),
                Size::new(2, 2),
            )
            .into_styled(st)
            .draw(d);
        }
        AppKind::Charge => {
            // battery
            let _ = Rectangle::new(Point::new(x, y + 3), Size::new(16, 11)).into_styled(st).draw(d);
            let _ = Rectangle::new(Point::new(x + 16, y + 6), Size::new(2, 5)).into_styled(fl).draw(d);
            for i in 0..3i32 {
                let _ = Rectangle::new(Point::new(x + 2 + i * 5, y + 5), Size::new(3, 7)).into_styled(fl).draw(d);
            }
        }
        AppKind::Settings => {
            // sliders
            let knobs = [5, 11, 8];
            for (r, &k) in knobs.iter().enumerate() {
                let yy = y + 2 + r as i32 * 6;
                let _ = Line::new(Point::new(x, yy), Point::new(x + 16, yy)).into_styled(st).draw(d);
                let _ = Rectangle::new(Point::new(x + k, yy - 2), Size::new(4, 5)).into_styled(fl).draw(d);
            }
        }
        AppKind::Repl => {
            // ">_" shell prompt
            let _ = Line::new(Point::new(x + 1, y + 3), Point::new(x + 7, y + 8)).into_styled(st).draw(d);
            let _ = Line::new(Point::new(x + 7, y + 8), Point::new(x + 1, y + 13)).into_styled(st).draw(d);
            let _ = Line::new(Point::new(x + 9, y + 13), Point::new(x + 16, y + 13)).into_styled(st).draw(d);
        }
        AppKind::Hacking => {
            // antenna / signal
            let _ = Line::new(Point::new(x + 8, y + 4), Point::new(x + 8, y + 16)).into_styled(st).draw(d);
            let _ = Circle::new(Point::new(x + 6, y), 5).into_styled(st).draw(d);
            let _ = Line::new(Point::new(x + 3, y + 16), Point::new(x + 13, y + 16)).into_styled(st).draw(d);
            let _ = Line::new(Point::new(x + 12, y + 5), Point::new(x + 16, y + 2)).into_styled(st).draw(d);
            let _ = Line::new(Point::new(x + 12, y + 9), Point::new(x + 17, y + 8)).into_styled(st).draw(d);
        }
    }
}

fn draw_card(d: &mut impl DrawTarget<Color = Rgb565>, app_idx: usize, pos: usize, selected: bool) {
    let y = CARD_Y0 + pos as i32 * CARD_STEP;
    // each app's own colour (hue wheel, keyed off menu position)
    let col = palette::wheel(app_idx);
    let acc = if selected { Some(col) } else { None };
    theme::card(d, theme::PAD, y, (theme::W - 2 * theme::PAD) as u32, CARD_H, acc);

    // the icon always carries the app's colour so every app reads as distinct
    draw_icon(d, APPS[app_idx].kind, theme::PAD + 14, y + 7, col);

    let name_col = if selected { theme::FG } else { theme::MUTED };
    theme::text(d, app_name(APPS[app_idx].kind), theme::PAD + 44, y + 5, theme::TITLE_FONT, name_col);
    theme::text(d, app_sub(APPS[app_idx].kind), theme::PAD + 44, y + 17, theme::BODY_FONT, theme::FAINT);

    if selected {
        theme::text_right(d, ">", theme::W - theme::PAD - 8, y + 9, theme::TITLE_FONT, col);
    }
}

/// `clear` only on entering the menu; updates skip it (cards self-clear -> no flicker).
pub fn draw(d: &mut impl DrawTarget<Color = Rgb565>, sel: usize, scroll: usize, clear: bool) {
    // The accent follows the highlighted app, so the top bar / battery / byline
    // match its colour. Entering the app keeps this colour (it themes the app).
    theme::set_accent_rgb(palette::wheel(sel));
    if clear {
        theme::clear(d);
    }
    // Redraw the top bar on every update (not just on entry) so the battery icon
    // and the "BotEkrem" byline track the highlighted app's accent colour live.
    theme::topbar(d, "Echoputer");
    let bx = theme::PAD + 9 * 8 + 6;
    theme::text(d, "by", bx, 6, theme::BODY_FONT, theme::MUTED);
    theme::text(d, "BotEkrem", bx + 18, 6, theme::BODY_FONT, theme::accent());

    let n = APPS.len();
    for pos in 0..VISIBLE {
        let y = CARD_Y0 + pos as i32 * CARD_STEP;
        theme::fill(d, 0, y - 1, theme::W as u32, CARD_H + 3, theme::BG); // self-clear band
        let idx = scroll + pos;
        if idx < n {
            draw_card(d, idx, pos, idx == sel);
        }
    }

    // scroll affordances
    let st = PrimitiveStyle::with_fill(theme::MUTED);
    if scroll > 0 {
        let _ = Triangle::new(Point::new(233, 26), Point::new(239, 26), Point::new(236, 22)).into_styled(st).draw(d);
    }
    if scroll + VISIBLE < n {
        let _ = Triangle::new(Point::new(233, 116), Point::new(239, 116), Point::new(236, 120)).into_styled(st).draw(d);
    }
}

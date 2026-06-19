//! Charge screen — a big live battery gauge. The Cardputer ADV only charges
//! while powered on, so this gives a "leave it here to charge" view.

use embedded_graphics::{
    mono_font::ascii::FONT_10X20,
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
};

use crate::hal::battery;
use crate::{i18n, theme};

const BX: i32 = 45;
const BY: i32 = 36;
const BW: i32 = 150;
const BH: i32 = 50;

/// `clear` only on entering; the ~1 s refresh passes false and repaints just the
/// fill + percent (no full-screen clear -> no on/off flash).
pub fn draw(d: &mut impl DrawTarget<Color = Rgb565>, clear: bool) {
    if clear {
        theme::clear(d);
        theme::topbar(d, i18n::t("Charge", "Sarj"));
        // static battery body + terminal nub
        let _ = Rectangle::new(Point::new(BX, BY), Size::new(BW as u32, BH as u32))
            .into_styled(PrimitiveStyle::with_stroke(theme::MUTED, 2))
            .draw(d);
        theme::fill(d, BX + BW, BY + 16, 6, 18, theme::MUTED);
    }

    // dynamic region: erase the inner fill + the text band, then repaint
    theme::fill(d, BX + 2, BY + 2, (BW - 4) as u32, (BH - 4) as u32, theme::BG);
    theme::fill(d, 0, BY + BH + 8, theme::W as u32, (theme::H - (BY + BH + 8)) as u32, theme::BG);

    if !battery::present() {
        theme::text_center(d, i18n::t("No battery", "Pil yok"), theme::W / 2, BY + BH / 2, &FONT_10X20, theme::MUTED);
        theme::text_center(d, i18n::t("running on USB power", "USB ile calisiyor"), theme::W / 2, BY + BH + 22, theme::BODY_FONT, theme::FAINT);
        return;
    }

    let lvl = battery::level();
    let col = if lvl <= 20 { theme::DESTRUCTIVE } else { theme::accent() };
    let innerw = (BW - 8) * lvl as i32 / 100;
    if innerw > 0 {
        theme::fill(d, BX + 4, BY + 4, innerw as u32, (BH - 8) as u32, col);
    }

    let mut nb = [0u8; 5];
    let s = pct_str(lvl, &mut nb);
    theme::text_center(d, s, theme::W / 2, BY + BH + 22, &FONT_10X20, theme::FG);
    theme::text_center(d, i18n::t("keep device on to charge", "sarj icin acik tutun"), theme::W / 2, BY + BH + 40, theme::BODY_FONT, theme::FAINT);
}

fn pct_str(v: u8, buf: &mut [u8; 5]) -> &str {
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

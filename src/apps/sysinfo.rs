//! System / About — a read-only live system monitor.
//!
//! A quiet info screen: muted labels on the left, bright values on the right,
//! at a 12 px row pitch. Mostly static, but the dynamic rows (heap usage,
//! uptime, battery) refresh once per second from [`tick`]. We only touch the
//! framebuffer when a value actually changed, so the main loop's per-frame blit
//! stays idle while this screen is open.

use alloc::format;
use alloc::string::String;

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use esp_hal::time::{Duration, Instant};

use crate::hal::battery;
use crate::{i18n, theme};

/// First row baseline; rows step down by [`ROW`].
const ROW0: i32 = 24;
/// Vertical pitch between rows.
const ROW: i32 = 12;
/// Right edge for values (a touch inside the right padding).
const VAL_X: i32 = theme::W - theme::PAD;

/// Which rows we draw, top to bottom. The label is fixed; the value is rebuilt
/// each refresh by [`value_for`]. Static rows (Chip, Cores, MAC) keep the same
/// text every refresh, so they're cheap no-ops after the first paint.
#[derive(Clone, Copy)]
enum Row {
    Chip,
    Cores,
    Heap,    // free / total KB
    HeapUse, // used KB
    Uptime,
    Battery,
    Mac,
}

impl Row {
    /// Rows whose value can change while the screen is open. The static rows
    /// (Chip, Cores, Mac) are painted once in `enter` and never re-checked, so
    /// `tick` skips them — no per-second String allocation for an unchanging value.
    fn is_dynamic(self) -> bool {
        matches!(self, Row::Heap | Row::HeapUse | Row::Uptime | Row::Battery)
    }
}

const ROWS: [Row; 7] = [
    Row::Chip,
    Row::Cores,
    Row::Heap,
    Row::HeapUse,
    Row::Uptime,
    Row::Battery,
    Row::Mac,
];

pub struct Sysinfo {
    /// Captured once in [`new`]; effectively boot time (main.rs builds us at
    /// startup). `elapsed()` on it is the uptime.
    boot: Instant,
    /// When we last repainted the dynamic values.
    last_refresh: Instant,
    /// Last rendered value per row, so we can skip rows whose text is unchanged
    /// and avoid redrawing (and dirtying the framebuffer) needlessly.
    shown: [String; ROWS.len()],
}

impl Sysinfo {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            boot: now,
            last_refresh: now,
            // Empty strings never match a real value, so the first draw paints
            // every row.
            shown: Default::default(),
        }
    }

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        theme::clear(d);
        theme::topbar(d, i18n::t("System", "Sistem"));

        // Labels are static — paint them once here.
        for (i, &row) in ROWS.iter().enumerate() {
            let y = ROW0 + i as i32 * ROW;
            theme::text(d, label_for(row), theme::PAD, y, theme::BODY_FONT, theme::MUTED);
        }

        self.last_refresh = Instant::now();
        for (i, &row) in ROWS.iter().enumerate() {
            self.draw_value(d, i, row);
        }

        theme::hint(d, i18n::t("live    ` menu", "canli    ` menu"));
    }

    /// No interactive controls — the screen is read-only. main.rs owns the home
    /// and back keys, so there's nothing to do here.
    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, _rc: (u8, u8), _d: &mut D) {}

    /// Refresh the dynamic values once a second. Returns true only if at least
    /// one value's text actually changed (so the main loop blits just then).
    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        if self.last_refresh.elapsed() < Duration::from_secs(1) {
            return false;
        }
        self.last_refresh = Instant::now();

        let mut changed = false;
        for (i, &row) in ROWS.iter().enumerate() {
            if !row.is_dynamic() {
                continue; // static row: drawn once in enter(), never re-allocated here
            }
            if value_for(row, self.boot) != self.shown[i] {
                self.draw_value(d, i, row);
                changed = true;
            }
        }
        changed
    }

    /// Repaint one row's value: clear its band, draw right-aligned, and cache it.
    fn draw_value<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, i: usize, row: Row) {
        let v = value_for(row, self.boot);
        let y = ROW0 + i as i32 * ROW;
        // Erase the value band only (labels live on the left, untouched).
        theme::fill(d, theme::W / 2, y, (theme::W / 2) as u32, ROW as u32, theme::BG);
        theme::text_right(d, &v, VAL_X, y, theme::BODY_FONT, theme::FG);
        self.shown[i] = v;
    }
}

fn label_for(row: Row) -> &'static str {
    match row {
        Row::Chip => i18n::t("Chip", "Yonga"),
        Row::Cores => i18n::t("Cores", "Cekirdek"),
        Row::Heap => i18n::t("Heap free", "Yigin bos"),
        Row::HeapUse => i18n::t("Heap used", "Yigin dolu"),
        Row::Uptime => i18n::t("Uptime", "Calisma"),
        Row::Battery => i18n::t("Battery", "Pil"),
        Row::Mac => "MAC",
    }
}

fn value_for(row: Row, boot: Instant) -> String {
    match row {
        Row::Chip => String::from("ESP32-S3"),
        Row::Cores => String::from("2"),
        Row::Heap => {
            // esp_alloc::HEAP exposes free()/used() (bytes) and stats().size (total).
            let free_kb = esp_alloc::HEAP.free() / 1024;
            let total_kb = esp_alloc::HEAP.stats().size / 1024;
            format!("{} / {} KB", free_kb, total_kb)
        }
        Row::HeapUse => {
            let used_kb = esp_alloc::HEAP.used() / 1024;
            format!("{} KB", used_kb)
        }
        Row::Uptime => fmt_uptime(boot.elapsed().as_secs()),
        Row::Battery => {
            if !battery::present() {
                String::from(i18n::t("USB power", "USB guc"))
            } else {
                format!("{}%", battery::level())
            }
        }
        Row::Mac => fmt_mac(),
    }
}

/// Seconds -> "HH:MM:SS" (hours uncapped, so a long uptime stays readable).
fn fmt_uptime(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

/// Factory base MAC from eFuse as "aa:bb:cc:dd:ee:ff".
fn fmt_mac() -> String {
    let mac = esp_hal::efuse::base_mac_address();
    let b = mac.as_bytes();
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5]
    )
}

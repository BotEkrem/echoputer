//! SD-card file browser + simple file manager.
//!
//! Navigates FAT volumes via embedded-sdmmc. To avoid self-referential lifetimes
//! we never store open Volume/Directory handles — every listing/read re-walks
//! from the root using the stored path components. 8.3 (short) file names.
//!
//! Controls: ↑/↓ select · → / ENTER open · ← parent dir · DEL delete file.

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_8X13_BOLD},
        MonoTextStyle,
    },
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use embedded_sdmmc::{
    BlockDevice, DirEntry, LfnBuffer, Mode, ShortFileName, TimeSource, Timestamp, VolumeIdx,
    VolumeManager,
};

use crate::theme;

/// Read-only fixed-time clock (we don't write file timestamps).
pub struct DummyTimeSource;
impl TimeSource for DummyTimeSource {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp::from_calendar(2024, 1, 1, 0, 0, 0).unwrap()
    }
}

const W: i32 = 240;
const H: i32 = 135;
const MAX_ENTRIES: usize = 48;
const MAX_DEPTH: usize = 8;
const SHORT_CAP: usize = 13;
const DISP_CAP: usize = 30;
const VIEW_CAP: usize = 512;

const LIST_TOP: i32 = 18;
const ROW_H: i32 = 12;
const VISIBLE: usize = 8;

const TXT_TOP: i32 = 18;
const TXT_ROW_H: i32 = 11;
const TXT_ROWS: usize = 9;
const TXT_COLS: usize = 39;
const HEX_ROWS: usize = 9;
const HEX_BPR: usize = 8;

const BG: Rgb565 = theme::BG;
const CYAN: Rgb565 = theme::FG;
const DIM: Rgb565 = theme::MUTED;
const FAINT: Rgb565 = theme::FAINT;
const WHITE: Rgb565 = theme::FG;
const SEL_BG: Rgb565 = theme::SURFACE2;
const ERR: Rgb565 = theme::DESTRUCTIVE;

// keys (logical row, col), matching main.rs
const K_UP: (u8, u8) = (2, 11);
const K_DOWN: (u8, u8) = (3, 11);
const K_ENTER: (u8, u8) = (2, 13);
const K_LEFT: (u8, u8) = (3, 10);
const K_RIGHT: (u8, u8) = (3, 12);
const K_DEL: (u8, u8) = (0, 13);

#[derive(Clone, Copy)]
struct Entry {
    short: [u8; SHORT_CAP], // 8.3 name — used to open/change/delete
    short_len: u8,
    disp: [u8; DISP_CAP], // long name — shown to the user
    disp_len: u8,
    is_dir: bool,
    is_parent: bool,
    size: u32,
}

impl Entry {
    const EMPTY: Entry = Entry {
        short: [0; SHORT_CAP],
        short_len: 0,
        disp: [0; DISP_CAP],
        disp_len: 0,
        is_dir: false,
        is_parent: false,
        size: 0,
    };
    fn disp_str(&self) -> &str {
        core::str::from_utf8(&self.disp[..self.disp_len as usize]).unwrap_or("?")
    }
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    List,
    File,
    Confirm,
}

pub struct Browser {
    path: [[u8; SHORT_CAP]; MAX_DEPTH],
    path_len: [u8; MAX_DEPTH],
    depth: usize,
    entries: [Entry; MAX_ENTRIES],
    count: usize,
    sel: usize,
    scroll: usize,
    view: View,
    ok: bool,
    // file viewer
    file_buf: [u8; VIEW_CAP],
    file_len: usize,
    file_off: u32,
    file_total: u32,
    is_text: bool,
    hist: [u32; 32],
    hist_len: usize,
    // options (from Settings)
    sort_by: u8, // 0 = name, 1 = size
    show_hidden: bool,
    confirm_delete: bool,
}

impl Browser {
    pub fn new() -> Self {
        Self {
            path: [[0; SHORT_CAP]; MAX_DEPTH],
            path_len: [0; MAX_DEPTH],
            depth: 0,
            entries: [Entry::EMPTY; MAX_ENTRIES],
            count: 0,
            sel: 0,
            scroll: 0,
            view: View::List,
            ok: false,
            file_buf: [0; VIEW_CAP],
            file_len: 0,
            file_off: 0,
            file_total: 0,
            is_text: true,
            hist: [0; 32],
            hist_len: 0,
            sort_by: 0,
            show_hidden: false,
            confirm_delete: true,
        }
    }

    pub fn set_opts(&mut self, sort_by: u8, show_hidden: bool, confirm_delete: bool) {
        self.sort_by = sort_by;
        self.show_hidden = show_hidden;
        self.confirm_delete = confirm_delete;
    }

    /// Open at the root and draw.
    pub fn enter<D: BlockDevice, T: TimeSource>(
        &mut self,
        vm: &VolumeManager<D, T>,
        d: &mut impl DrawTarget<Color = Rgb565>,
    ) {
        self.depth = 0;
        self.view = View::List;
        self.relist(vm);
        self.draw(d, true);
    }

    fn path_comp(&self, i: usize) -> &str {
        core::str::from_utf8(&self.path[i][..self.path_len[i] as usize]).unwrap_or("")
    }

    fn relist<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        let mut entries = [Entry::EMPTY; MAX_ENTRIES];
        let mut count = 0usize;
        if self.depth > 0 {
            let mut e = Entry::EMPTY;
            e.is_dir = true;
            e.is_parent = true;
            e.short[0] = b'.';
            e.short[1] = b'.';
            e.short_len = 2;
            e.disp[0] = b'.';
            e.disp[1] = b'.';
            e.disp_len = 2;
            entries[0] = e;
            count = 1;
        }

        let show_hidden = self.show_hidden;
        let done = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let mut dir = vol.open_root_dir().ok()?;
            for i in 0..self.depth {
                dir.change_dir(self.path_comp(i)).ok()?;
            }
            let mut lfn_store = [0u8; 300];
            let mut lfn_buf = LfnBuffer::new(&mut lfn_store);
            dir.iterate_dir_lfn(&mut lfn_buf, |e: &DirEntry, lfn: Option<&str>| {
                if count >= MAX_ENTRIES {
                    return;
                }
                if e.attributes.is_volume() || (!show_hidden && e.attributes.is_hidden()) {
                    return;
                }
                let mut entry = Entry::EMPTY;
                let n = fmt_short(&e.name, &mut entry.short);
                // skip "." and ".."
                if (n == 1 && entry.short[0] == b'.')
                    || (n == 2 && entry.short[0] == b'.' && entry.short[1] == b'.')
                {
                    return;
                }
                entry.short_len = n as u8;
                entry.disp_len = match lfn {
                    Some(l) => copy_into(l, &mut entry.disp) as u8,
                    None => {
                        entry.disp[..n].copy_from_slice(&entry.short[..n]);
                        n as u8
                    }
                };
                entry.is_dir = e.attributes.is_directory();
                entry.size = e.size;
                entries[count] = entry;
                count += 1;
            })
            .ok()?;
            Some(())
        })();

        // sort real entries (keep ".." at the top); dirs first, then by name/size
        let start = if self.depth > 0 { 1 } else { 0 };
        let sb = self.sort_by;
        if count > start {
            entries[start..count].sort_unstable_by(|a, b| {
                b.is_dir.cmp(&a.is_dir).then_with(|| {
                    if sb == 1 {
                        a.size.cmp(&b.size)
                    } else {
                        a.disp[..a.disp_len as usize].cmp(&b.disp[..b.disp_len as usize])
                    }
                })
            });
        }

        self.entries = entries;
        self.count = count;
        self.sel = 0;
        self.scroll = 0;
        self.ok = done.is_some();
    }

    fn fix_scroll(&mut self) {
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + VISIBLE {
            self.scroll = self.sel + 1 - VISIBLE;
        }
    }

    pub fn on_key<D: BlockDevice, T: TimeSource>(
        &mut self,
        rc: (u8, u8),
        vm: &VolumeManager<D, T>,
        d: &mut impl DrawTarget<Color = Rgb565>,
    ) {
        match self.view {
            View::List => match rc {
                K_UP => {
                    if self.sel > 0 {
                        self.sel -= 1;
                        self.fix_scroll();
                        self.draw(d, false);
                    }
                }
                K_DOWN => {
                    if self.sel + 1 < self.count {
                        self.sel += 1;
                        self.fix_scroll();
                        self.draw(d, false);
                    }
                }
                K_ENTER | K_RIGHT => self.open_selected(vm, d),
                K_LEFT => self.go_parent(vm, d),
                K_DEL => {
                    if self.count > 0 {
                        let e = self.entries[self.sel];
                        if !e.is_dir && !e.is_parent {
                            if self.confirm_delete {
                                self.view = View::Confirm;
                                self.draw(d, true);
                            } else {
                                self.do_delete(vm, d);
                            }
                        }
                    }
                }
                _ => {}
            },
            View::File => match rc {
                K_DOWN => {
                    // page by what was actually SHOWN (page_step), not the 512-byte read size,
                    // so the tail of a page with many short lines is still reachable
                    if self.file_off + self.page_step() < self.file_total {
                        if self.hist_len < self.hist.len() {
                            self.hist[self.hist_len] = self.file_off;
                            self.hist_len += 1;
                        }
                        self.file_off += self.page_step();
                        self.read_file_page(vm);
                        self.draw(d, true);
                    }
                }
                K_UP => {
                    if self.hist_len > 0 {
                        self.hist_len -= 1;
                        self.file_off = self.hist[self.hist_len];
                        self.read_file_page(vm);
                        self.draw(d, true);
                    }
                }
                K_LEFT | K_ENTER => {
                    self.view = View::List;
                    self.draw(d, true);
                }
                _ => {}
            },
            View::Confirm => match rc {
                K_ENTER => self.do_delete(vm, d),
                _ => {
                    self.view = View::List;
                    self.draw(d, true);
                }
            },
        }
    }

    fn go_parent<D: BlockDevice, T: TimeSource>(
        &mut self,
        vm: &VolumeManager<D, T>,
        d: &mut impl DrawTarget<Color = Rgb565>,
    ) {
        if self.depth > 0 {
            self.depth -= 1;
            self.relist(vm);
            self.draw(d, true);
        }
    }

    fn open_selected<D: BlockDevice, T: TimeSource>(
        &mut self,
        vm: &VolumeManager<D, T>,
        d: &mut impl DrawTarget<Color = Rgb565>,
    ) {
        if self.count == 0 {
            return;
        }
        let e = self.entries[self.sel];
        if e.is_parent {
            self.go_parent(vm, d);
            return;
        }
        if e.is_dir {
            if self.depth < MAX_DEPTH {
                let l = e.short_len as usize;
                self.path[self.depth][..l].copy_from_slice(&e.short[..l]);
                self.path_len[self.depth] = e.short_len;
                self.depth += 1;
                self.relist(vm);
            }
            self.draw(d, true);
            return;
        }
        // file -> viewer
        self.file_off = 0;
        self.hist_len = 0;
        self.read_file_page(vm);
        self.view = View::File;
        self.draw(d, true);
    }

    fn page_step(&self) -> u32 {
        // how many bytes the current page consumed/showed
        if self.is_text {
            self.text_consumed() as u32
        } else {
            (HEX_ROWS * HEX_BPR) as u32
        }
    }

    fn read_file_page<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        let e = self.entries[self.sel];
        let mut buf = [0u8; VIEW_CAP];
        let mut got = 0usize;
        let mut total = 0u32;

        let done = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let mut dir = vol.open_root_dir().ok()?;
            for i in 0..self.depth {
                dir.change_dir(self.path_comp(i)).ok()?;
            }
            let name = core::str::from_utf8(&e.short[..e.short_len as usize]).ok()?;
            let file = dir.open_file_in_dir(name, Mode::ReadOnly).ok()?;
            total = file.length();
            file.seek_from_start(self.file_off).ok()?;
            while got < VIEW_CAP {
                let n = file.read(&mut buf[got..]).ok()?;
                if n == 0 {
                    break;
                }
                got += n;
            }
            Some(())
        })();

        if done.is_some() {
            self.file_buf = buf;
            self.file_len = got;
            self.file_total = total;
            self.is_text = detect_text(&buf[..got]);
        } else {
            self.file_len = 0;
            self.file_total = 0;
            self.is_text = true;
        }
    }

    fn do_delete<D: BlockDevice, T: TimeSource>(
        &mut self,
        vm: &VolumeManager<D, T>,
        d: &mut impl DrawTarget<Color = Rgb565>,
    ) {
        let e = self.entries[self.sel];
        if !e.is_dir && !e.is_parent {
            let _ = (|| -> Option<()> {
                let vol = vm.open_volume(VolumeIdx(0)).ok()?;
                let mut dir = vol.open_root_dir().ok()?;
                for i in 0..self.depth {
                    dir.change_dir(self.path_comp(i)).ok()?;
                }
                let name = core::str::from_utf8(&e.short[..e.short_len as usize]).ok()?;
                dir.delete_file_in_dir(name).ok()?;
                Some(())
            })();
            self.relist(vm);
        }
        self.view = View::List;
        self.draw(d, true);
    }

    // ---------------- drawing ----------------

    /// `clear` only on entering / view change; list navigation passes false so
    /// the rows repaint over themselves — no full-screen clear, no flicker.
    pub fn draw(&self, d: &mut impl DrawTarget<Color = Rgb565>, clear: bool) {
        match self.view {
            View::List => self.draw_list(d, clear),
            View::File => self.draw_file(d),
            View::Confirm => self.draw_confirm(d),
        }
    }

    fn draw_list(&self, d: &mut impl DrawTarget<Color = Rgb565>, clear: bool) {
        if clear {
            let _ = d.clear(BG);
            let mut pb = [0u8; 64];
            let pl = self.fmt_path(&mut pb);
            text(d, core::str::from_utf8(&pb[..pl]).unwrap_or("/"), 4, 3, &FONT_8X13_BOLD, CYAN);
            theme::draw_battery(d, theme::W - theme::PAD, 3);
            line(d, 16);
            fill(d, 0, H - 13, W as u32, 13, BG);
            text(d, "enter open   < up   DEL del   ` menu", 4, H - 11, &FONT_6X10, FAINT);
            if !self.ok {
                text(d, "No SD card or not FAT32.", 6, 50, &FONT_6X10, ERR);
                text(d, "Insert a card, press ` then", 6, 66, &FONT_6X10, DIM);
                text(d, "re-open File Browser.", 6, 78, &FONT_6X10, DIM);
            } else if self.count == 0 {
                text(d, "(empty folder)", 6, 50, &FONT_6X10, DIM);
            }
        }
        if !self.ok || self.count == 0 {
            return;
        }

        for r in 0..VISIBLE {
            let y = LIST_TOP + r as i32 * ROW_H;
            fill(d, 0, y - 1, W as u32, ROW_H as u32, BG); // self-clear band
            let idx = self.scroll + r;
            if idx >= self.count {
                continue;
            }
            let e = &self.entries[idx];
            let selected = idx == self.sel;
            if selected {
                fill(d, 0, y - 1, W as u32, ROW_H as u32, SEL_BG);
                fill(d, 0, y - 1, 3, ROW_H as u32, theme::accent());
            }
            if e.is_dir {
                fill(d, 8, y + 1, 6, 3, theme::accent());
                fill(d, 8, y + 3, 11, 7, theme::accent());
            } else {
                let st = PrimitiveStyle::with_stroke(theme::MUTED, 1);
                let x = 9;
                let t = y;
                use embedded_graphics::primitives::Line;
                let _ = Line::new(Point::new(x, t), Point::new(x, t + 10)).into_styled(st).draw(d);
                let _ = Line::new(Point::new(x, t + 10), Point::new(x + 7, t + 10)).into_styled(st).draw(d);
                let _ = Line::new(Point::new(x + 7, t + 3), Point::new(x + 7, t + 10)).into_styled(st).draw(d);
                let _ = Line::new(Point::new(x, t), Point::new(x + 4, t)).into_styled(st).draw(d);
                let _ = Line::new(Point::new(x + 4, t), Point::new(x + 7, t + 3)).into_styled(st).draw(d);
            }
            let name_col = if selected { theme::FG } else { theme::MUTED };
            text(d, e.disp_str(), 22, y, &FONT_6X10, name_col);
            if !e.is_dir && !e.is_parent {
                let mut sb = [0u8; 8];
                let sl = fmt_size(e.size, &mut sb);
                let s = core::str::from_utf8(&sb[..sl]).unwrap_or("");
                let x = W - 4 - (s.len() as i32) * 6;
                text(d, s, x, y, &FONT_6X10, DIM);
            }
        }
    }

    fn draw_file(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        let _ = d.clear(BG);
        let e = self.entries[self.sel];
        text(d, e.disp_str(), 4, 2, &FONT_8X13_BOLD, CYAN);
        let tag = if self.is_text { "TXT" } else { "HEX" };
        text(d, tag, W - 4 - 18, 4, &FONT_6X10, theme::accent());
        line(d, 15);

        if self.file_len == 0 {
            text(d, "(empty or unreadable)", 6, 50, &FONT_6X10, DIM);
        } else if self.is_text {
            self.draw_text_page(d);
        } else {
            self.draw_hex_page(d);
        }

        // footer: offset / total
        fill(d, 0, H - 13, W as u32, 13, BG);
        let mut fb = [0u8; 40];
        let n = fmt_progress(self.file_off, self.file_len as u32, self.file_total, &mut fb);
        text(d, core::str::from_utf8(&fb[..n]).unwrap_or(""), 4, H - 11, &FONT_6X10, DIM);
    }

    fn text_consumed(&self) -> usize {
        // mirror of draw_text_page layout, but only counts consumed bytes
        let buf = &self.file_buf[..self.file_len];
        let mut col = 0;
        let mut row = 0;
        let mut i = 0;
        while i < buf.len() && row < TXT_ROWS {
            let c = buf[i];
            i += 1;
            if c == b'\n' {
                row += 1;
                col = 0;
                continue;
            }
            if c == b'\r' {
                continue;
            }
            col += 1;
            if col >= TXT_COLS {
                row += 1;
                col = 0;
            }
        }
        i.max(1)
    }

    fn draw_text_page(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        let buf = &self.file_buf[..self.file_len];
        let mut linebuf = [b' '; TXT_COLS];
        let mut col = 0;
        let mut row = 0usize;
        let mut i = 0;
        while i < buf.len() && row < TXT_ROWS {
            let c = buf[i];
            i += 1;
            if c == b'\n' {
                draw_mono_line(d, &linebuf[..col], row);
                row += 1;
                col = 0;
                continue;
            }
            if c == b'\r' {
                continue;
            }
            let ch = if c == b'\t' {
                b' '
            } else if (0x20..=0x7e).contains(&c) {
                c
            } else {
                b'.'
            };
            linebuf[col] = ch;
            col += 1;
            if col >= TXT_COLS {
                draw_mono_line(d, &linebuf[..col], row);
                row += 1;
                col = 0;
            }
        }
        if row < TXT_ROWS && col > 0 {
            draw_mono_line(d, &linebuf[..col], row);
        }
    }

    fn draw_hex_page(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        let buf = &self.file_buf[..self.file_len];
        let n = buf.len().min(HEX_ROWS * HEX_BPR);
        for row in 0..HEX_ROWS {
            let start = row * HEX_BPR;
            if start >= n {
                break;
            }
            let mut s = [b' '; 48];
            let mut p = 0usize;
            let off = self.file_off + start as u32;
            push_hex16(&mut s, &mut p, (off & 0xFFFF) as u16);
            s[p] = b' ';
            p += 1;
            for k in 0..HEX_BPR {
                if start + k < n {
                    push_hex8(&mut s, &mut p, buf[start + k]);
                    s[p] = b' ';
                    p += 1;
                } else {
                    s[p] = b' ';
                    s[p + 1] = b' ';
                    s[p + 2] = b' ';
                    p += 3;
                }
            }
            s[p] = b' ';
            p += 1;
            for k in 0..HEX_BPR {
                if start + k < n {
                    let c = buf[start + k];
                    s[p] = if (0x20..=0x7e).contains(&c) { c } else { b'.' };
                    p += 1;
                }
            }
            draw_mono_line(d, &s[..p], row);
        }
    }

    fn draw_confirm(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::card(d, 20, 44, 200, 46, Some(theme::DESTRUCTIVE));
        text(d, "Delete file?", 30, 51, &FONT_8X13_BOLD, ERR);
        text(d, self.entries[self.sel].disp_str(), 30, 67, &FONT_6X10, WHITE);
        text(d, "ENTER = yes     other = no", 30, 79, &FONT_6X10, DIM);
    }

    fn fmt_path(&self, buf: &mut [u8; 64]) -> usize {
        let mut p = 0;
        buf[p] = b'/';
        p += 1;
        for i in 0..self.depth {
            let comp = self.path_comp(i);
            for &c in comp.as_bytes() {
                if p < 63 {
                    buf[p] = c;
                    p += 1;
                }
            }
            if p < 63 {
                buf[p] = b'/';
                p += 1;
            }
        }
        p
    }
}

// ---------------- free helpers ----------------

fn text(d: &mut impl DrawTarget<Color = Rgb565>, s: &str, x: i32, y: i32, f: &'static embedded_graphics::mono_font::MonoFont, c: Rgb565) {
    let _ = Text::with_baseline(s, Point::new(x, y), MonoTextStyle::new(f, c), Baseline::Top).draw(d);
}

fn fill(d: &mut impl DrawTarget<Color = Rgb565>, x: i32, y: i32, w: u32, h: u32, c: Rgb565) {
    let _ = Rectangle::new(Point::new(x, y), Size::new(w, h)).into_styled(PrimitiveStyle::with_fill(c)).draw(d);
}

fn line(d: &mut impl DrawTarget<Color = Rgb565>, y: i32) {
    let _ = embedded_graphics::primitives::Line::new(Point::new(0, y), Point::new(W - 1, y))
        .into_styled(PrimitiveStyle::with_stroke(theme::BORDER, 1))
        .draw(d);
}

fn draw_mono_line(d: &mut impl DrawTarget<Color = Rgb565>, bytes: &[u8], row: usize) {
    if let Ok(s) = core::str::from_utf8(bytes) {
        text(d, s, 2, TXT_TOP + row as i32 * TXT_ROW_H, &FONT_6X10, WHITE);
    }
}

fn fmt_short(sfn: &ShortFileName, buf: &mut [u8; SHORT_CAP]) -> usize {
    use core::fmt::Write;
    struct Wr<'a> {
        b: &'a mut [u8; SHORT_CAP],
        n: usize,
    }
    impl core::fmt::Write for Wr<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &c in s.as_bytes() {
                if self.n < SHORT_CAP {
                    self.b[self.n] = c;
                    self.n += 1;
                }
            }
            Ok(())
        }
    }
    let mut w = Wr { b: buf, n: 0 };
    let _ = write!(w, "{}", sfn);
    w.n
}

fn copy_into(s: &str, buf: &mut [u8; DISP_CAP]) -> usize {
    let mut n = 0;
    for &c in s.as_bytes() {
        if n < DISP_CAP {
            buf[n] = c;
            n += 1;
        }
    }
    n
}

fn detect_text(b: &[u8]) -> bool {
    if b.is_empty() {
        return true;
    }
    let n = b.len().min(256);
    let mut printable = 0;
    for &c in &b[..n] {
        if c == 9 || c == 10 || c == 13 || (0x20..=0x7e).contains(&c) {
            printable += 1;
        }
    }
    printable * 100 >= n * 90
}

fn fmt_size(bytes: u32, buf: &mut [u8; 8]) -> usize {
    // "<n>B" / "<n>K" / "<n>M"
    let (val, unit) = if bytes >= 1024 * 1024 {
        (bytes / (1024 * 1024), b'M')
    } else if bytes >= 1024 {
        (bytes / 1024, b'K')
    } else {
        (bytes, b'B')
    };
    let mut p = push_u32(buf, 0, val);
    if p < buf.len() {
        buf[p] = unit;
        p += 1;
    }
    p
}

fn fmt_progress(off: u32, len: u32, total: u32, buf: &mut [u8; 40]) -> usize {
    // "0-512 / 4096 B"
    let mut p = push_u32_40(buf, 0, off);
    buf[p] = b'-';
    p += 1;
    p = push_u32_40(buf, p, off + len);
    for &c in b" / " {
        buf[p] = c;
        p += 1;
    }
    p = push_u32_40(buf, p, total);
    for &c in b" B" {
        if p < 40 {
            buf[p] = c;
            p += 1;
        }
    }
    p
}

fn push_u32(buf: &mut [u8; 8], at: usize, v: u32) -> usize {
    let mut tmp = [0u8; 10];
    let mut n = v;
    let mut i = 0;
    if n == 0 {
        tmp[i] = b'0';
        i += 1;
    }
    while n > 0 {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    let mut p = at;
    while i > 0 && p < buf.len() {
        i -= 1;
        buf[p] = tmp[i];
        p += 1;
    }
    p
}

fn push_u32_40(buf: &mut [u8; 40], at: usize, v: u32) -> usize {
    let mut tmp = [0u8; 10];
    let mut n = v;
    let mut i = 0;
    if n == 0 {
        tmp[i] = b'0';
        i += 1;
    }
    while n > 0 {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    let mut p = at;
    while i > 0 && p < 40 {
        i -= 1;
        buf[p] = tmp[i];
        p += 1;
    }
    p
}

fn hex_nib(n: u8) -> u8 {
    if n < 10 {
        b'0' + n
    } else {
        b'A' + (n - 10)
    }
}

fn push_hex8(buf: &mut [u8; 48], p: &mut usize, v: u8) {
    buf[*p] = hex_nib(v >> 4);
    buf[*p + 1] = hex_nib(v & 0xF);
    *p += 2;
}

fn push_hex16(buf: &mut [u8; 48], p: &mut usize, v: u16) {
    push_hex8(buf, p, (v >> 8) as u8);
    push_hex8(buf, p, (v & 0xFF) as u8);
}

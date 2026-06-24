//! Notes — a tiny text editor backed by the SD card.
//!
//! Six fixed slots live under /ECHO/NOTES/ as NOTE1.TXT .. NOTE6.TXT. The list
//! view picks a slot; opening one loads it into an in-memory buffer you type
//! into (the arrow cluster is free, so this is append-at-end editing: type,
//! Backspace deletes, ENTER inserts a newline). Edits are written back to the
//! card automatically when you leave the editor (G0/back, or the home key), so
//! there's no separate save key to remember. The "Aa" key toggles letter case.
//!
//! With no SD card inserted everything still runs — the load/save just no-op and
//! the buffer lives only in RAM for the session.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_sdmmc::{BlockDevice, DirEntry, Mode as FileMode, ShortFileName, TimeSource, VolumeIdx, VolumeManager};

use crate::hal::keymap;
use crate::i18n::notes;
use crate::{i18n, theme};

const DIR_APP: &str = "ECHO"; // 8.3 FAT short name
const DIR_NOTES: &str = "NOTES";
const SLOTS: usize = 6;
const SLOT_NAMES: [&str; SLOTS] = ["NOTE1.TXT", "NOTE2.TXT", "NOTE3.TXT", "NOTE4.TXT", "NOTE5.TXT", "NOTE6.TXT"];
const MAX_NOTE: usize = 1024; // bytes per note
const WRAP: usize = 39; // chars per display line at 6px on 240px
const VIS_ROWS: usize = 8; // editor lines shown
const ROW_H: i32 = 11;

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    List,
    Edit,
}

pub struct Notes {
    view: View,
    sel: usize,        // selected slot in the list
    slot: usize,       // slot currently open in the editor
    buf: String,       // editor contents
    used: [bool; SLOTS], // which slots have a file on the card
    dirty: bool,       // unsaved edits in `buf`
    caps: bool,        // "Aa" caps toggle
}

impl Notes {
    pub fn new() -> Self {
        Notes {
            view: View::List,
            sel: 0,
            slot: 0,
            buf: String::new(),
            used: [false; SLOTS],
            dirty: false,
            caps: false,
        }
    }

    /// In the editor (so main routes Backspace to us as delete, not back-to-menu).
    pub fn is_editing(&self) -> bool {
        self.view == View::Edit
    }

    // ----------------------------- SD I/O -----------------------------

    /// Mark which slots already have a file on the card. One directory pass:
    /// at 400 kHz each `open_file_in_dir` re-walks the NOTES directory, so probing
    /// all six slots that way froze the screen on entry. `iterate_dir` reads the
    /// directory exactly once.
    fn scan<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        let mut found = [false; SLOTS];
        let _ = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let mut dir = vol.open_root_dir().ok()?;
            dir.change_dir(DIR_APP).ok()?;
            dir.change_dir(DIR_NOTES).ok()?;
            dir.iterate_dir(|e: &DirEntry| {
                if e.attributes.is_directory() {
                    return;
                }
                let mut buf = [0u8; 13]; // "NOTE1.TXT" is 9 chars; 8.3 max is 12
                let n = fmt_name(&e.name, &mut buf);
                if let Ok(name) = core::str::from_utf8(&buf[..n]) {
                    for (i, slot) in SLOT_NAMES.iter().enumerate() {
                        if name.eq_ignore_ascii_case(slot) {
                            found[i] = true;
                        }
                    }
                }
            })
            .ok()?;
            Some(())
        })();
        self.used = found;
    }

    /// Load a slot's file into `buf` (empty buffer if it doesn't exist yet).
    fn load<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>, slot: usize) {
        self.buf.clear();
        let _ = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let mut dir = vol.open_root_dir().ok()?;
            dir.change_dir(DIR_APP).ok()?;
            dir.change_dir(DIR_NOTES).ok()?;
            let file = dir.open_file_in_dir(SLOT_NAMES[slot], FileMode::ReadOnly).ok()?;
            let mut chunk = [0u8; 64];
            loop {
                let n = file.read(&mut chunk).ok()?;
                if n == 0 {
                    break;
                }
                for &b in &chunk[..n] {
                    // keep printable ASCII + newlines; drop anything else
                    if b == b'\n' || (0x20..=0x7e).contains(&b) {
                        if self.buf.len() < MAX_NOTE {
                            self.buf.push(b as char);
                        }
                    }
                }
            }
            Some(())
        })();
        self.dirty = false;
    }

    /// Write `buf` back to the open slot's file (best-effort).
    pub fn save_if_dirty<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>) {
        if !self.dirty {
            return;
        }
        let ok = (|| -> Option<()> {
            let vol = vm.open_volume(VolumeIdx(0)).ok()?;
            let mut dir = vol.open_root_dir().ok()?;
            let _ = dir.make_dir_in_dir(DIR_APP);
            dir.change_dir(DIR_APP).ok()?;
            let _ = dir.make_dir_in_dir(DIR_NOTES);
            dir.change_dir(DIR_NOTES).ok()?;
            let file = dir.open_file_in_dir(SLOT_NAMES[self.slot], FileMode::ReadWriteCreateOrTruncate).ok()?;
            file.write(self.buf.as_bytes()).ok()?;
            file.flush().ok()?;
            Some(())
        })();
        if ok.is_some() {
            self.dirty = false;
            self.used[self.slot] = true;
        }
    }

    // --------------------------- interface ---------------------------

    pub fn enter<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>, d: &mut impl DrawTarget<Color = Rgb565>) {
        self.scan(vm);
        self.view = View::List;
        self.draw_list(d);
    }

    /// G0/back: editor -> save + back to the list; list -> false (pop to menu).
    pub fn back<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        if self.view == View::Edit {
            self.save_if_dirty(vm);
            self.scan(vm);
            self.view = View::List;
            self.draw_list(d);
            true
        } else {
            false
        }
    }

    /// Flip caps (driven by the "Aa" key); only meaningful in the editor.
    pub fn toggle_caps(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) {
        self.caps = !self.caps;
        if self.view == View::Edit {
            self.draw_edit(d);
        }
    }

    pub fn on_key<D: BlockDevice, T: TimeSource>(&mut self, rc: (u8, u8), vm: &VolumeManager<D, T>, d: &mut impl DrawTarget<Color = Rgb565>) {
        match self.view {
            View::List => match rc {
                crate::K_UP => {
                    if self.sel > 0 {
                        self.sel -= 1;
                        self.draw_list(d);
                    }
                }
                crate::K_DOWN => {
                    if self.sel + 1 < SLOTS {
                        self.sel += 1;
                        self.draw_list(d);
                    }
                }
                crate::K_ENTER => {
                    self.slot = self.sel;
                    self.load(vm, self.slot);
                    self.view = View::Edit;
                    self.draw_edit(d);
                }
                _ => {}
            },
            View::Edit => {
                if rc == crate::K_ENTER {
                    if self.buf.len() < MAX_NOTE {
                        self.buf.push('\n');
                        self.dirty = true;
                        self.draw_edit(d);
                    }
                } else if rc == keymap::K_BKSP {
                    if self.buf.pop().is_some() {
                        self.dirty = true;
                        self.draw_edit(d);
                    }
                } else if let Some(b) = keymap::ch_shift(rc.0, rc.1, self.caps) {
                    if self.buf.len() < MAX_NOTE {
                        self.buf.push(b as char);
                        self.dirty = true;
                        self.draw_edit(d);
                    }
                }
            }
        }
    }

    // ----------------------------- drawing -----------------------------

    fn draw_list(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::clear(d);
        theme::topbar(d, i18n::t(notes::NOTES));
        for i in 0..SLOTS {
            let y = 24 + i as i32 * 15;
            let selected = i == self.sel;
            let name = format!("{} {}", i18n::t(notes::NOTE), i + 1);
            let state = if self.used[i] {
                i18n::t(notes::WRITTEN)
            } else {
                i18n::t(notes::EMPTY)
            };
            let col = if selected { theme::accent() } else { theme::MUTED };
            if selected {
                theme::text(d, ">", theme::PAD, y, theme::BODY_FONT, theme::accent());
            }
            theme::text(d, &name, theme::PAD + 12, y, theme::BODY_FONT, col);
            theme::text_right(d, state, theme::W - theme::PAD, y, theme::BODY_FONT, theme::FAINT);
        }
        theme::hint(d, i18n::t(notes::LIST_HINT));
    }

    /// Wrap `buf` into display lines, breaking on '\n' and at the panel width.
    fn wrap(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let mut cur = String::new();
        for ch in self.buf.chars() {
            if ch == '\n' {
                lines.push(core::mem::take(&mut cur));
            } else {
                cur.push(ch);
                if cur.len() >= WRAP {
                    lines.push(core::mem::take(&mut cur));
                }
            }
        }
        lines.push(cur); // trailing line — where the cursor sits
        lines
    }

    fn draw_edit(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::clear(d);
        let mut title = format!("{} {}", i18n::t(notes::NOTE), self.slot + 1);
        if self.dirty {
            title.push_str(" *");
        }
        theme::topbar(d, &title);

        let lines = self.wrap();
        let start = lines.len().saturating_sub(VIS_ROWS);
        let last = lines.len() - 1;
        for (i, line) in lines[start..].iter().enumerate() {
            let y = 22 + i as i32 * ROW_H;
            // a block cursor on the final (current) line
            if start + i == last {
                let mut shown = line.clone();
                shown.push('_');
                theme::text(d, &shown, theme::PAD, y, theme::BODY_FONT, theme::FG);
            } else {
                theme::text(d, line, theme::PAD, y, theme::BODY_FONT, theme::FG);
            }
        }

        let hint = format!("ENTER nl  bksp  G0 save  {}", if self.caps { "ABC" } else { "abc" });
        theme::hint(d, &hint);
    }
}

/// Format an 8.3 directory-entry name (e.g. "NOTE1.TXT") into `buf`; returns its
/// length. ShortFileName implements Display, so we just render it into bytes.
fn fmt_name(name: &ShortFileName, buf: &mut [u8; 13]) -> usize {
    use core::fmt::Write;
    struct W<'a> {
        b: &'a mut [u8; 13],
        n: usize,
    }
    impl core::fmt::Write for W<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for &c in s.as_bytes() {
                if self.n < self.b.len() {
                    self.b[self.n] = c;
                    self.n += 1;
                }
            }
            Ok(())
        }
    }
    let mut w = W { b: buf, n: 0 };
    let _ = write!(w, "{}", name);
    w.n
}

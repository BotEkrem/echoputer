//! CHIP-8 — a tiny retro virtual machine, the smallest "emulator" Echoputer ships.
//!
//! Loads one ROM from `/ECHO/CHIP8.CH8` on the SD card and runs it: 64x32 mono
//! display scaled 3x into the framebuffer, 16-key hex pad mapped onto the Cardputer
//! keyboard (1234/qwer/asdf/zxcv) plus the arrows as 2/4/6/8 aliases.
//!
//! RAM: the 4 KB VM memory is heap-`Box`ed and held ONLY while a ROM is loaded, so
//! nothing big sits on the (tight, no-PSRAM) main stack frame when the app is idle.

use alloc::boxed::Box;
use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use embedded_sdmmc::{BlockDevice, Mode, TimeSource, VolumeIdx, VolumeManager};
use esp_hal::time::{Duration, Instant};

use crate::hal::keymap;
use crate::{i18n, theme};

const CW: usize = 64; // CHIP-8 display width
const CH: usize = 32; // CHIP-8 display height
const SCALE: i32 = 3;
const OX: i32 = (theme::W as i32 - CW as i32 * SCALE) / 2; // centred horizontally
const OY: i32 = 30; // below the top bar
const DIR_APP: &str = "ECHO";
const ROM_NAME: &str = "CHIP8.CH8"; // 8.3 short name

/// Standard CHIP-8 hex font (16 glyphs x 5 bytes), loaded at 0x000.
#[rustfmt::skip]
const FONT: [u8; 80] = [
    0xF0, 0x90, 0x90, 0x90, 0xF0, // 0
    0x20, 0x60, 0x20, 0x20, 0x70, // 1
    0xF0, 0x10, 0xF0, 0x80, 0xF0, // 2
    0xF0, 0x10, 0xF0, 0x10, 0xF0, // 3
    0x90, 0x90, 0xF0, 0x10, 0x10, // 4
    0xF0, 0x80, 0xF0, 0x10, 0xF0, // 5
    0xF0, 0x80, 0xF0, 0x90, 0xF0, // 6
    0xF0, 0x10, 0x20, 0x40, 0x40, // 7
    0xF0, 0x90, 0xF0, 0x90, 0xF0, // 8
    0xF0, 0x90, 0xF0, 0x10, 0xF0, // 9
    0xF0, 0x90, 0xF0, 0x90, 0x90, // A
    0xE0, 0x90, 0xE0, 0x90, 0xE0, // B
    0xF0, 0x80, 0x80, 0x80, 0xF0, // C
    0xE0, 0x90, 0x90, 0x90, 0xE0, // D
    0xF0, 0x80, 0xF0, 0x80, 0xF0, // E
    0xF0, 0x80, 0xF0, 0x80, 0x80, // F
];

/// The interpreter state. `mem` is heap-boxed; everything else is tiny, so the whole
/// struct (when present) is a few hundred bytes plus the 4 KB heap buffer.
struct Vm {
    mem: Box<[u8]>, // 4096 bytes; program loaded at 0x200
    v: [u8; 16],
    i: u16,
    pc: u16,
    stack: [u16; 16],
    sp: u8,
    dt: u8,
    st: u8,
    gfx: [u64; CH],   // one row per u64 (64 px); bit 63 == x=0
    shown: [u64; CH], // last-drawn frame, for per-pixel diffing
    keys: u16,        // pressed-key bitmask (decays in tick())
    rng: u32,
}

impl Vm {
    fn new() -> Self {
        let mut mem = alloc::vec![0u8; 4096].into_boxed_slice();
        mem[..80].copy_from_slice(&FONT);
        Vm {
            mem,
            v: [0; 16],
            i: 0,
            pc: 0x200,
            stack: [0; 16],
            sp: 0,
            dt: 0,
            st: 0,
            gfx: [0; CH],
            shown: [0; CH],
            keys: 0,
            rng: 0x2545_F491,
        }
    }

    #[inline]
    fn rd(&self, a: u16) -> u8 {
        self.mem[(a & 0x0FFF) as usize]
    }

    fn key(&self, k: u8) -> bool {
        self.keys & (1 << (k & 0xF)) != 0
    }

    /// Execute one opcode. All memory accesses are masked to 0..4096 so a bad ROM
    /// can never panic (a panic on this no_std target is a reboot).
    fn step(&mut self) {
        let op = ((self.rd(self.pc) as u16) << 8) | self.rd(self.pc.wrapping_add(1)) as u16;
        self.pc = self.pc.wrapping_add(2);
        let x = ((op & 0x0F00) >> 8) as usize;
        let y = ((op & 0x00F0) >> 4) as usize;
        let n = (op & 0x000F) as u8;
        let nn = (op & 0x00FF) as u8;
        let nnn = op & 0x0FFF;

        match op & 0xF000 {
            0x0000 => match op {
                0x00E0 => self.gfx = [0; CH], // CLS
                0x00EE => {
                    self.sp = self.sp.wrapping_sub(1);
                    self.pc = self.stack[self.sp as usize & 0xF];
                }
                _ => {} // 0nnn (SYS) ignored
            },
            0x1000 => self.pc = nnn,
            0x2000 => {
                self.stack[self.sp as usize & 0xF] = self.pc;
                self.sp = self.sp.wrapping_add(1);
                self.pc = nnn;
            }
            0x3000 => self.skip_if(self.v[x] == nn),
            0x4000 => self.skip_if(self.v[x] != nn),
            0x5000 => self.skip_if(self.v[x] == self.v[y]),
            0x6000 => self.v[x] = nn,
            0x7000 => self.v[x] = self.v[x].wrapping_add(nn),
            0x8000 => match n {
                0x0 => self.v[x] = self.v[y],
                0x1 => self.v[x] |= self.v[y],
                0x2 => self.v[x] &= self.v[y],
                0x3 => self.v[x] ^= self.v[y],
                0x4 => {
                    let (r, c) = self.v[x].overflowing_add(self.v[y]);
                    self.v[x] = r;
                    self.v[0xF] = c as u8;
                }
                0x5 => {
                    let (r, b) = self.v[x].overflowing_sub(self.v[y]);
                    self.v[x] = r;
                    self.v[0xF] = (!b) as u8;
                }
                0x6 => {
                    let c = self.v[x] & 1;
                    self.v[x] >>= 1;
                    self.v[0xF] = c;
                }
                0x7 => {
                    let (r, b) = self.v[y].overflowing_sub(self.v[x]);
                    self.v[x] = r;
                    self.v[0xF] = (!b) as u8;
                }
                0xE => {
                    let c = self.v[x] >> 7;
                    self.v[x] <<= 1;
                    self.v[0xF] = c;
                }
                _ => {}
            },
            0x9000 => self.skip_if(self.v[x] != self.v[y]),
            0xA000 => self.i = nnn,
            0xB000 => self.pc = nnn.wrapping_add(self.v[0] as u16),
            0xC000 => {
                self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                self.v[x] = (self.rng >> 16) as u8 & nn;
            }
            0xD000 => self.draw(x, y, n),
            0xE000 => match nn {
                0x9E => self.skip_if(self.key(self.v[x])),
                0xA1 => self.skip_if(!self.key(self.v[x])),
                _ => {}
            },
            0xF000 => match nn {
                0x07 => self.v[x] = self.dt,
                0x0A => {
                    // wait for a key: stall PC until one is down
                    match (0..16u8).find(|&k| self.key(k)) {
                        Some(k) => self.v[x] = k,
                        None => self.pc = self.pc.wrapping_sub(2),
                    }
                }
                0x15 => self.dt = self.v[x],
                0x18 => self.st = self.v[x],
                0x1E => self.i = self.i.wrapping_add(self.v[x] as u16),
                0x29 => self.i = (self.v[x] as u16 & 0xF) * 5,
                0x33 => {
                    let vx = self.v[x];
                    let i = self.i;
                    self.wr(i, vx / 100);
                    self.wr(i.wrapping_add(1), (vx / 10) % 10);
                    self.wr(i.wrapping_add(2), vx % 10);
                }
                0x55 => {
                    for r in 0..=x {
                        let b = self.v[r];
                        self.wr(self.i.wrapping_add(r as u16), b);
                    }
                }
                0x65 => {
                    for r in 0..=x {
                        self.v[r] = self.rd(self.i.wrapping_add(r as u16));
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    #[inline]
    fn wr(&mut self, a: u16, b: u8) {
        self.mem[(a & 0x0FFF) as usize] = b;
    }

    #[inline]
    fn skip_if(&mut self, cond: bool) {
        if cond {
            self.pc = self.pc.wrapping_add(2);
        }
    }

    /// DRW Vx,Vy,N — XOR an N-byte sprite; set VF on a pixel collision. Wraps the
    /// start coordinate (per the spec) but clips rows/columns past the edges.
    fn draw(&mut self, x: usize, y: usize, n: u8) {
        let sx = self.v[x] as usize % CW;
        let sy = self.v[y] as usize % CH;
        self.v[0xF] = 0;
        for row in 0..n as usize {
            let yy = sy + row;
            if yy >= CH {
                break;
            }
            let byte = self.rd(self.i.wrapping_add(row as u16));
            for bit in 0..8usize {
                if byte & (0x80 >> bit) == 0 {
                    continue;
                }
                let xx = sx + bit;
                if xx >= CW {
                    continue;
                }
                let mask = 1u64 << (63 - xx);
                if self.gfx[yy] & mask != 0 {
                    self.v[0xF] = 1;
                }
                self.gfx[yy] ^= mask;
            }
        }
    }
}

/// Map a Cardputer key to a CHIP-8 hex key. Arrows + ENTER alias the common
/// 2/4/6/8/5 directional keys so games using either convention are playable.
fn key_nibble(rc: (u8, u8)) -> Option<u8> {
    match rc {
        crate::K_UP => return Some(0x2),
        crate::K_DOWN => return Some(0x8),
        crate::K_LEFT => return Some(0x4),
        crate::K_RIGHT => return Some(0x6),
        crate::K_ENTER => return Some(0x5),
        _ => {}
    }
    Some(match keymap::ch_shift(rc.0, rc.1, false)? {
        b'1' => 0x1, b'2' => 0x2, b'3' => 0x3, b'4' => 0xC,
        b'q' => 0x4, b'w' => 0x5, b'e' => 0x6, b'r' => 0xD,
        b'a' => 0x7, b's' => 0x8, b'd' => 0x9, b'f' => 0xE,
        b'z' => 0xA, b'x' => 0x0, b'c' => 0xB, b'v' => 0xF,
        _ => return None,
    })
}

/// Read `/ECHO/CHIP8.CH8` into `mem[0x200..]`. Returns the byte count or an error.
fn load_rom<D: BlockDevice, T: TimeSource>(sd: &VolumeManager<D, T>, mem: &mut [u8]) -> Result<usize, &'static str> {
    let vol = sd.open_raw_volume(VolumeIdx(0)).map_err(|_| "no card")?;
    let root = sd.open_root_dir(vol).map_err(|_| "fs error")?;
    let app = sd.open_dir(root, DIR_APP).map_err(|_| "no /ECHO")?;
    let _ = sd.close_dir(root);
    let file = match sd.open_file_in_dir(app, ROM_NAME, Mode::ReadOnly) {
        Ok(f) => f,
        Err(_) => {
            let _ = sd.close_dir(app);
            let _ = sd.close_volume(vol);
            return Err("no CHIP8.CH8");
        }
    };
    let _ = sd.close_dir(app);
    let mut got = 0usize;
    let cap = mem.len() - 0x200;
    while got < cap {
        match sd.read(file, &mut mem[0x200 + got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(_) => break,
        }
    }
    let _ = sd.close_file(file);
    let _ = sd.close_volume(vol);
    if got == 0 {
        Err("empty ROM")
    } else {
        Ok(got)
    }
}

pub struct Chip8 {
    // The whole VM (4 KB RAM + frame buffers) is heap-boxed and held ONLY while a ROM
    // is loaded, so the idle app is ~40 bytes on main's tight stack frame.
    vm: Option<Box<Vm>>,
    err: Option<&'static str>,
    last_step: Instant,
    last_timer: Instant,
    key_at: Instant,
}

impl Chip8 {
    pub fn new() -> Self {
        let now = Instant::now();
        Chip8 {
            vm: None,
            err: None,
            last_step: now,
            last_timer: now,
            key_at: now,
        }
    }

    /// Allocate the VM, load the ROM from SD, and draw the initial screen.
    pub fn enter<D: BlockDevice, T: TimeSource>(&mut self, sd: &VolumeManager<D, T>, d: &mut impl DrawTarget<Color = Rgb565>) {
        let mut machine = Vm::new();
        match load_rom(sd, &mut machine.mem) {
            Ok(_) => {
                self.vm = Some(Box::new(machine)); // move the VM onto the heap
                self.err = None;
            }
            Err(e) => {
                self.err = Some(e); // `machine` drops here -> 4 KB freed
                self.vm = None;
            }
        }
        let now = Instant::now();
        self.last_step = now;
        self.last_timer = now;
        self.draw(d);
    }

    /// Free the heap VM (called when leaving the app).
    pub fn exit(&mut self) {
        self.vm = None;
    }

    pub fn on_key(&mut self, rc: (u8, u8), _d: &mut impl DrawTarget<Color = Rgb565>) {
        if let (Some(vm), Some(k)) = (self.vm.as_mut(), key_nibble(rc)) {
            vm.keys |= 1 << k;
            self.key_at = Instant::now();
        }
    }

    pub fn tick(&mut self, d: &mut impl DrawTarget<Color = Rgb565>) -> bool {
        if self.vm.is_none() {
            return false;
        }
        // Keys are press-only on this keyboard, so a tap is held ~120 ms then released.
        if self.key_at.elapsed() >= Duration::from_millis(120) {
            if let Some(vm) = self.vm.as_mut() {
                vm.keys = 0;
            }
        }
        // Run the CPU off the wall clock (~700 instructions/sec, capped per tick).
        let ela = self.last_step.elapsed();
        if ela >= Duration::from_millis(2) {
            self.last_step = Instant::now();
            let steps = (ela.as_millis() as u32 * 7 / 10).clamp(1, 40);
            if let Some(vm) = self.vm.as_mut() {
                for _ in 0..steps {
                    vm.step();
                }
            }
        }
        // Delay/sound timers count down at 60 Hz.
        if self.last_timer.elapsed() >= Duration::from_millis(16) {
            self.last_timer = Instant::now();
            if let Some(vm) = self.vm.as_mut() {
                vm.dt = vm.dt.saturating_sub(1);
                vm.st = vm.st.saturating_sub(1);
            }
        }
        // Repaint only the pixels that changed since the last frame.
        let mut changed = false;
        if let Some(vm) = self.vm.as_mut() {
            let cur = vm.gfx;
            for r in 0..CH {
                let diff = cur[r] ^ vm.shown[r];
                if diff == 0 {
                    continue;
                }
                changed = true;
                for x in 0..CW {
                    let mask = 1u64 << (63 - x);
                    if diff & mask == 0 {
                        continue;
                    }
                    let col = if cur[r] & mask != 0 { theme::accent() } else { theme::BG };
                    theme::fill(d, OX + x as i32 * SCALE, OY + r as i32 * SCALE, SCALE as u32, SCALE as u32, col);
                }
                vm.shown[r] = cur[r];
            }
        }
        changed
    }

    fn draw(&self, d: &mut impl DrawTarget<Color = Rgb565>) {
        theme::clear(d);
        theme::topbar(d, "Chip-8");
        if let Some(e) = self.err {
            theme::text(d, e, theme::PAD, 44, theme::TITLE_FONT, theme::FG);
            theme::text(
                d,
                i18n::t("put a ROM at /ECHO/CHIP8.CH8", "ROM'u /ECHO/CHIP8.CH8'e koy"),
                theme::PAD,
                64,
                theme::BODY_FONT,
                theme::MUTED,
            );
            theme::hint(d, i18n::t("` menu", "` menu"));
        } else {
            theme::hint(d, i18n::t("123 qwe asd zxc + arrows  ` back", "123 qwe asd zxc + ok  ` geri"));
        }
    }
}

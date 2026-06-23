//! SD-backed ROM cache.
//!
//! There is no PSRAM and a Pokémon ROM (1 MB) does not fit in the 512 KB SRAM, so
//! the ROM stays on the SD card and Peanut-GB reads it through `gb_rom_read`. We
//! pin bank 0 (the fixed first 16 KB) in RAM, and cache the switchable region at
//! **512-byte sector granularity** (one SD block per miss): 32 sectors of an
//! arbitrary mix of banks stay resident, and a miss fetches just 512 bytes instead
//! of a whole 16 KB bank. For a game whose hot code/data working set is a few KB
//! scattered across banks (Pokémon), that is far fewer and far cheaper reloads than
//! a single 16 KB bank slot — the difference between a crawl and playable on the
//! slow SD bus, without needing more RAM than the one-slot cache used.
//!
//! The buffers are heap-allocated only while a game is loaded (not reserved in
//! .bss), because the firmware's static RAM is already tight — permanently
//! reserving them starves esp-rtos's task stacks and the device won't even boot.
//!
//! The other catch: `gb_rom_read` is a bare C callback with no context, so the
//! cache is reached through a `static` (see `ffi`). The `VolumeManager` it needs is
//! owned by `main` and has a long generic type; rather than name it, `attach` is
//! generic and stores a type-erased pointer plus a monomorphised read thunk, set
//! for the duration of each emulated frame.

use alloc::boxed::Box;
use embedded_sdmmc::{BlockDevice, RawFile, TimeSource, VolumeManager};

pub const BANK: usize = 0x4000; // 16 KB (bank 0, pinned)
const SEC: usize = 512; // sector size = one SD block
const NSEC: usize = 32; // switchable-region sectors cached (32 * 512 = 16 KB)
const NO_SEC: u32 = u32::MAX;

/// Reads `buf.len()` bytes of the ROM at `offset` from the type-erased volume
/// manager. Returns false on any SD error.
type ReadFn = unsafe fn(vm: *const (), file: RawFile, offset: u32, buf: &mut [u8]) -> bool;

pub struct RomCache {
    /// When set (the boot self-test), ROM reads come straight from this flash-
    /// resident slice instead of the SD card / heap cache.
    embedded: Option<&'static [u8]>,
    file: Option<RawFile>,
    rom_size: u32,
    bank0: Option<Box<[u8]>>,
    sec: Option<Box<[u8]>>, // NSEC * SEC bytes
    sec_tag: [u32; NSEC],   // ROM sector number held in each slot (NO_SEC = empty)
    sec_used: [u32; NSEC],  // LRU stamp
    clock: u32,
    sd_loads: u32,
    // Per-frame SD access, set by `attach` / cleared by `detach`.
    vm: *const (),
    read_fn: Option<ReadFn>,
}

impl RomCache {
    pub const fn new() -> Self {
        RomCache {
            embedded: None,
            file: None,
            rom_size: 0,
            bank0: None,
            sec: None,
            sec_tag: [NO_SEC; NSEC],
            sec_used: [0; NSEC],
            clock: 0,
            sd_loads: 0,
            vm: core::ptr::null(),
            read_fn: None,
        }
    }

    /// SD loads so far (self-test diagnostic).
    #[cfg(feature = "emutest")]
    pub fn bank_loads(&self) -> u32 {
        self.sd_loads
    }

    /// Allocate the cache buffers from the heap (bank 0 + the sector pool, 32 KB
    /// total). Returns false if the heap is too full (caller aborts the launch).
    pub fn alloc_buffers(&mut self) -> bool {
        self.bank0 = Some(alloc::vec![0xFFu8; BANK].into_boxed_slice());
        self.sec = Some(alloc::vec![0xFFu8; NSEC * SEC].into_boxed_slice());
        self.sec_tag = [NO_SEC; NSEC];
        self.clock = 0;
        self.sd_loads = 0;
        self.bank0.is_some() && self.sec.is_some()
    }

    /// Free the cache buffers back to the heap (call when leaving a game).
    pub fn free_buffers(&mut self) {
        self.bank0 = None;
        self.sec = None;
        self.file = None;
    }

    /// Point ROM reads at a flash-resident slice (boot self-test), bypassing SD.
    #[cfg(feature = "emutest")]
    pub fn set_embedded(&mut self, rom: &'static [u8]) {
        self.embedded = Some(rom);
    }

    /// Bind the open ROM file (its handle stays valid for the whole session).
    /// Binding a real file disables any embedded-ROM mode (reads now hit SD).
    pub fn set_file(&mut self, file: RawFile, rom_size: u32) {
        self.embedded = None;
        self.file = Some(file);
        self.rom_size = rom_size;
        self.sec_tag = [NO_SEC; NSEC];
        self.clock = 0;
        self.sd_loads = 0;
    }

    /// Wire up SD access for the current frame (generic so the caller's concrete
    /// `VolumeManager` type is inferred; we keep only an erased pointer + thunk).
    pub fn attach<D, T, const MD: usize, const MF: usize, const MV: usize>(
        &mut self,
        vm: &VolumeManager<D, T, MD, MF, MV>,
    ) where
        D: BlockDevice,
        T: TimeSource,
    {
        self.vm = vm as *const _ as *const ();
        self.read_fn = Some(read_thunk::<D, T, MD, MF, MV>);
    }

    /// Drop the borrowed SD pointer once the frame is done.
    pub fn detach(&mut self) {
        self.vm = core::ptr::null();
        self.read_fn = None;
    }

    /// Load bank 0 into its buffer. Call once after `attach` + `alloc_buffers`,
    /// before the first frame.
    pub fn prime(&mut self) {
        let (vm, rf, file) = (self.vm, self.read_fn, self.file);
        if let (Some(rf), Some(file)) = (rf, file) {
            if let Some(buf) = self.bank0.as_mut() {
                unsafe {
                    rf(vm, file, 0, buf);
                }
                self.sd_loads += 1;
            }
        }
    }

    /// Peanut-GB reads a ROM byte at a resolved physical address.
    #[inline]
    pub fn read(&mut self, addr: u32) -> u8 {
        let a = addr as usize;
        if let Some(rom) = self.embedded {
            return rom.get(a).copied().unwrap_or(0xFF);
        }
        // Bank 0 is pinned.
        if a < BANK {
            return match self.bank0.as_ref() {
                Some(b) => b[a],
                None => 0xFF,
            };
        }
        // Switchable region: 512-byte sector cache.
        let sec = (a / SEC) as u32;
        let so = a % SEC;
        for i in 0..NSEC {
            if self.sec_tag[i] == sec {
                self.clock += 1;
                self.sec_used[i] = self.clock;
                return self.sec.as_ref().map_or(0xFF, |b| b[i * SEC + so]);
            }
        }
        // Miss: evict the LRU sector and read one 512-byte block.
        let v = self.lru_sec();
        let base = sec.saturating_mul(SEC as u32);
        let (vm, rf, file) = (self.vm, self.read_fn, self.file);
        let ok = match (rf, file, self.sec.as_mut()) {
            (Some(rf), Some(file), Some(buf)) => unsafe {
                rf(vm, file, base, &mut buf[v * SEC..v * SEC + SEC])
            },
            _ => false,
        };
        if ok {
            self.sec_tag[v] = sec;
            self.clock += 1;
            self.sec_used[v] = self.clock;
            self.sd_loads += 1;
            self.sec.as_ref().map_or(0xFF, |b| b[v * SEC + so])
        } else {
            0xFF
        }
    }

    fn lru_sec(&self) -> usize {
        let mut best = 0;
        for i in 1..NSEC {
            if self.sec_tag[i] == NO_SEC {
                return i;
            }
            if self.sec_used[i] < self.sec_used[best] {
                best = i;
            }
        }
        best
    }
}

/// Monomorphised per-concrete-`VolumeManager` reader. Stored type-erased and only
/// ever called while `attach` has set a live pointer (single-threaded main loop).
unsafe fn read_thunk<D, T, const MD: usize, const MF: usize, const MV: usize>(
    vm: *const (),
    file: RawFile,
    offset: u32,
    buf: &mut [u8],
) -> bool
where
    D: BlockDevice,
    T: TimeSource,
{
    let vm = &*(vm as *const VolumeManager<D, T, MD, MF, MV>);
    if vm.file_seek_from_start(file, offset).is_err() {
        return false;
    }
    let mut got = 0;
    while got < buf.len() {
        match vm.read(file, &mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(_) => return false,
        }
    }
    // Past end-of-ROM reads back as open-bus 0xFF.
    for b in &mut buf[got..] {
        *b = 0xFF;
    }
    true
}

//! A tiny, self-made 32 KB Game Boy ROM embedded in flash for the boot self-test
//! (`--features emutest`). It is NOT a copyrighted game — just enough to exercise
//! the whole core on hardware: a valid header (so `gb_init` accepts it) plus a
//! 11-byte program that turns the LCD on and spins, so the PPU runs and drives
//! `lcd_draw_line` for every visible line.
//!
//! Entry at 0x100 jumps into bank 1 (0x4000), so the CPU executes from the
//! *switchable* ROM region — which makes the SD bank cache actually load bank 1
//! from the card during the SD round-trip self-test (the embedded path serves it
//! directly). The loop at 0x4000:
//!   di                  ; F3
//!   ld   a, 0xE4        ; 3E E4   (BGP: shades 0,1,2,3)
//!   ldh  (0x47), a      ; E0 47
//!   ld   a, 0x91        ; 3E 91   (LCDC: LCD on, BG on, tile data @0x8000)
//!   ldh  (0x40), a      ; E0 40
//!   jr   $              ; 18 FE
//!
//! Header bytes 0x134..=0x14C are all zero, so the header checksum (x = sum of
//! -(byte)-1 over that range) is 0 - 25 = 0xE7.

pub static TEST_ROM: [u8; 0x8000] = build_rom();

const fn build_rom() -> [u8; 0x8000] {
    let mut rom = [0u8; 0x8000];
    // 0x100: jp 0x4000  (C3 00 40)
    rom[0x100] = 0xC3;
    rom[0x101] = 0x00;
    rom[0x102] = 0x40;
    // 0x4000 (bank 1): turn the LCD on and spin.
    let prog = [
        0xF3u8, 0x3E, 0xE4, 0xE0, 0x47, 0x3E, 0x91, 0xE0, 0x40, 0x18, 0xFE,
    ];
    let mut i = 0;
    while i < prog.len() {
        rom[0x4000 + i] = prog[i];
        i += 1;
    }
    // Cartridge type 0x147 = 0x00 (ROM only), ROM size 0x148 = 0x00 (32 KB),
    // RAM size 0x149 = 0x00 (none) — already zero. Header checksum:
    rom[0x14D] = 0xE7;
    rom
}

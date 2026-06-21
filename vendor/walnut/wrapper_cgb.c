/* Glue between the firmware (Rust) and the Walnut-CGB core (Game Boy + Game Boy
 * Color). Used instead of the Peanut-GB wrapper when the `emugbc` feature is on.
 *
 * Walnut-CGB is a single-header rewrite of Peanut-GB with CGB (colour) support.
 * Its API differs slightly: gb_init takes three ROM-read callbacks (8/16/32-bit,
 * the wide ones feed the dual-fetch CPU + DMA), and the LCD callback delivers
 * palette *indices* which we resolve to RGB565 through the core's `fixPalette`.
 *
 * It exposes the SAME `emu_*` ABI as the Peanut wrapper, so the Rust frontend is
 * unchanged; only the LCD path differs (RGB565 instead of shade indices). Build
 * flags (DMA mode, sample rate, audio format) live in build.rs. */

#define ENABLE_SOUND 1
#define ENABLE_LCD 1
#define WALNUT_GB_IS_LITTLE_ENDIAN 1

#include <stdint.h>

/* Walnut-CGB (ENABLE_SOUND) calls these bare; route them to minigb_apu. */
uint8_t audio_read(const uint16_t addr);
void audio_write(const uint16_t addr, const uint8_t val);

#include "walnut_cgb.h"
#include "minigb_apu.h"

/* The CGB core instance (larger than DMG: 16 KB VRAM + 32 KB WRAM banks). */
static struct gb_s GB;
static struct minigb_apu_ctx APU;

uint8_t audio_read(const uint16_t addr) { return minigb_apu_audio_read(&APU, addr); }
void audio_write(const uint16_t addr, const uint8_t val) {
    minigb_apu_audio_write(&APU, addr, val);
}

/* Implemented in Rust (src/apps/emu/ffi.rs). */
extern uint8_t rust_rom_read(uint32_t addr);
extern uint8_t rust_ram_read(uint32_t addr);
extern void rust_ram_write(uint32_t addr, uint8_t val);
extern void rust_lcd_line_rgb(const uint16_t *pixels, uint8_t line);

static uint8_t cb_rom_read(struct gb_s *gb, const uint_fast32_t addr) {
    (void)gb;
    return rust_rom_read((uint32_t)addr);
}

/* The 16/32-bit ROM reads (dual-fetch + DMA) are composed from byte reads so the
 * existing bank cache handles bank-boundary crossings correctly. Little-endian. */
static uint16_t cb_rom_read16(struct gb_s *gb, const uint_fast32_t addr) {
    (void)gb;
    return (uint16_t)rust_rom_read((uint32_t)addr) |
           ((uint16_t)rust_rom_read((uint32_t)addr + 1) << 8);
}

static uint32_t cb_rom_read32(struct gb_s *gb, const uint_fast32_t addr) {
    (void)gb;
    return (uint32_t)rust_rom_read((uint32_t)addr) |
           ((uint32_t)rust_rom_read((uint32_t)addr + 1) << 8) |
           ((uint32_t)rust_rom_read((uint32_t)addr + 2) << 16) |
           ((uint32_t)rust_rom_read((uint32_t)addr + 3) << 24);
}

static uint8_t cb_ram_read(struct gb_s *gb, const uint_fast32_t addr) {
    (void)gb;
    return rust_ram_read((uint32_t)addr);
}

static void cb_ram_write(struct gb_s *gb, const uint_fast32_t addr, const uint8_t val) {
    (void)gb;
    rust_ram_write((uint32_t)addr, val);
}

static void cb_error(struct gb_s *gb, const enum gb_error_e err, const uint16_t val) {
    (void)gb;
    (void)err;
    (void)val;
}

#if ENABLE_LCD
/* Resolve palette indices to RGB565 via the core's fixed palette and hand a full
 * colour line to Rust. Works for both DMG (greyscale palette) and CGB (colour). */
static void cb_lcd_line(struct gb_s *gb, const uint8_t *pixels, const uint_fast8_t line) {
    uint16_t rgb[160];
    for (int x = 0; x < 160; x++)
        rgb[x] = gb->cgb.fixPalette[pixels[x] & 0x3F];
    rust_lcd_line_rgb(rgb, (uint8_t)line);
}
#endif

/* ---- emu_* ABI (identical to the Peanut wrapper) ------------------------- */

int emu_init(void) {
    enum gb_init_error_e e = gb_init(&GB, cb_rom_read, cb_rom_read16, cb_rom_read32,
                                     cb_ram_read, cb_ram_write, cb_error, NULL);
    if (e != GB_INIT_NO_ERROR)
        return (int)e + 1;
#if ENABLE_LCD
    gb_init_lcd(&GB, cb_lcd_line);
#endif
    minigb_apu_audio_init(&APU);
    return 0;
}

unsigned emu_audio_count(void) { return AUDIO_SAMPLES_TOTAL; }
void emu_audio_frame(int16_t *out) { minigb_apu_audio_callback(&APU, out); }
void emu_audio_write(uint16_t addr, uint8_t val) { minigb_apu_audio_write(&APU, addr, val); }

/* Use the dual-fetch frame runner (16/32-bit paths, fastest on the ESP32-S3). */
void emu_run_frame(void) { gb_run_frame_dualfetch(&GB); }

void emu_set_joypad(uint8_t bits) { GB.direct.joypad = (uint8_t)~bits; }
uint8_t emu_get_joypad(void) { return GB.direct.joypad; }
uint32_t emu_save_size(void) { return (uint32_t)gb_get_save_size(&GB); }

/* Glue between the firmware (Rust) and the Peanut-GB core.
 *
 * Peanut-GB is a header-only emulator; including it here compiles the whole core
 * into this one translation unit. It calls back through function pointers that
 * take the `struct gb_s *` plus an address; we adapt those to the simpler
 * `rust_*` callbacks (which reach the emulator state through a Rust `static`, so
 * they ignore the gb pointer). The firmware drives us through the `emu_*` ABI.
 *
 * Build flags live in build.rs. Sound is off for now; the LCD is on. */

#define ENABLE_SOUND 1
#define ENABLE_LCD 1
#define PEANUT_GB_HIGH_LCD_ACCURACY 0 /* faster; flip to 1 if a game needs it */
/* ESP32-S3 (Xtensa LX7) is little-endian; set it explicitly so Peanut-GB's
 * compiler-macro auto-detection can't fall through to its #error. */
#define PEANUT_GB_IS_LITTLE_ENDIAN 1

#include <stdint.h>

/* Peanut-GB (ENABLE_SOUND) calls these bare functions for the sound registers;
 * we route them to the minigb_apu APU below. (AUDIO_SAMPLE_RATE is set to the
 * firmware's 16 kHz I2S rate via a -D flag in build.rs, for both C files.) */
uint8_t audio_read(const uint16_t addr);
void audio_write(const uint16_t addr, const uint8_t val);

#include "peanut_gb.h"
#include "minigb_apu.h"

/* The single core instance lives in .bss (SRAM); ~a few tens of KB. */
static struct gb_s GB;
/* The APU context (pure-integer; a couple of KB). */
static struct minigb_apu_ctx APU;

uint8_t audio_read(const uint16_t addr) { return minigb_apu_audio_read(&APU, addr); }
void audio_write(const uint16_t addr, const uint8_t val) {
    minigb_apu_audio_write(&APU, addr, val);
}

/* Implemented in Rust (src/apps/emu/core.rs). */
extern uint8_t rust_rom_read(uint32_t addr);
extern uint8_t rust_ram_read(uint32_t addr);
extern void rust_ram_write(uint32_t addr, uint8_t val);
extern void rust_lcd_line(const uint8_t *pixels, uint8_t line);

/* Trampolines adapting Peanut-GB's callback signatures to the Rust ABI. */
static uint8_t cb_rom_read(struct gb_s *gb, const uint_fast32_t addr) {
    (void)gb;
    return rust_rom_read((uint32_t)addr);
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
    /* Non-fatal: keep running. The frontend shows a generic error if init fails. */
}

#if ENABLE_LCD
static void cb_lcd_line(struct gb_s *gb, const uint8_t pixels[160],
                        const uint_fast8_t line) {
    (void)gb;
    rust_lcd_line(pixels, (uint8_t)line);
}
#endif

/* ---- emu_* ABI consumed by the Rust frontend ---------------------------- */

int emu_init(void) {
    enum gb_init_error_e e =
        gb_init(&GB, cb_rom_read, cb_ram_read, cb_ram_write, cb_error, NULL);
    if (e != GB_INIT_NO_ERROR)
        return (int)e + 1; /* non-zero == failure */
#if ENABLE_LCD
    gb_init_lcd(&GB, cb_lcd_line);
#endif
    minigb_apu_audio_init(&APU);
    return 0;
}

/* ---- audio (minigb_apu) -------------------------------------------------- */

/* int16 stereo samples produced per emulated frame (AUDIO_SAMPLES_TOTAL). */
unsigned emu_audio_count(void) { return AUDIO_SAMPLES_TOTAL; }

/* Fill `out` (>= emu_audio_count() int16s) with one frame of APU audio. */
void emu_audio_frame(int16_t *out) { minigb_apu_audio_callback(&APU, out); }

/* Direct APU register poke, for the self-test tone (no ROM needed). */
void emu_audio_write(uint16_t addr, uint8_t val) { minigb_apu_audio_write(&APU, addr, val); }

void emu_run_frame(void) { gb_run_frame(&GB); }

/* `bits` is active-high in JOYPAD_* order (A,B,Select,Start,Right,Left,Up,Down);
 * Peanut-GB's joypad register is active-low, so invert. */
void emu_set_joypad(uint8_t bits) { GB.direct.joypad = (uint8_t)~bits; }

/* Read back the joypad register (self-test: verify the Rust->C input path). */
uint8_t emu_get_joypad(void) { return GB.direct.joypad; }

uint32_t emu_save_size(void) { return (uint32_t)gb_get_save_size(&GB); }

/* Thin C ABI over the vendored minimp3 (single-header, public-domain CC0).
 *
 * Mirrors the Peanut-GB `wrapper.c` pattern: the Rust FFI in
 * `src/apps/player/mp3.rs` only ever calls these `mp3_*` functions.
 *
 * Build defines (set here, before the include): MINIMP3_IMPLEMENTATION (emit the
 * code), MINIMP3_NO_STDIO (no file I/O — we feed SD bytes), MINIMP3_ONLY_MP3 (drop
 * MP1/MP2), MINIMP3_NO_SIMD (portable path — Xtensa has no x86/ARM SIMD). We do NOT
 * define MINIMP3_FLOAT_OUTPUT: the default S16 output feeds the i16 I2S directly and
 * avoids minimp3's float synthesis filter (the LX-FPU weak spot).
 *
 * RAM: the decoder state (`mp3dec_t`, ~6.7 KB) and the per-frame scratch
 * (`mp3dec_scratch_t`, ~16 KB) are NOT kept in .bss. Together ~23 KB, a permanent
 * reservation that big starves the bare-metal boot stack (esp-hal stack-guard panic) —
 * worse when the Game Boy emulator (also large .bss) is built in alongside. So the
 * firmware allocates both on the heap (only while a track plays, like the emulator's
 * caches) and hands them in via `mp3_set_buffers`. The header is patched to reach the
 * scratch through `echoputer_scratch` (see the "ECHOPUTER PATCH" comments there).
 */

#define MINIMP3_IMPLEMENTATION
#define MINIMP3_NO_STDIO
#define MINIMP3_ONLY_MP3
#define MINIMP3_NO_SIMD
#define MINIMP3_NONSTANDARD_BUT_LOGICAL
#include "minimp3.h"

/* Firmware-provided heap buffers (set by mp3_set_buffers before any decode). */
static mp3dec_t *g_dec = 0;
mp3dec_scratch_t *echoputer_scratch = 0; /* referenced by the patched decode frame */

/* Sizes (in bytes) the firmware must allocate, 4-byte aligned (no doubles inside). */
unsigned int mp3_dec_size(void) { return (unsigned int)sizeof(mp3dec_t); }
unsigned int mp3_scratch_size(void) { return (unsigned int)sizeof(mp3dec_scratch_t); }

/* Bind the heap buffers (or clear them with NULLs on free). */
void mp3_set_buffers(void *dec, void *scratch) {
    g_dec = (mp3dec_t *)dec;
    echoputer_scratch = (mp3dec_scratch_t *)scratch;
}

/* (Re)initialise the decoder for a new stream. */
void mp3_dec_init(void) {
    if (g_dec) {
        mp3dec_init(g_dec);
    }
}

/* Decode one MP3 frame from `in[0..in_len]`.
 *
 * `out` must hold at least MINIMP3_MAX_SAMPLES_PER_FRAME (2304) shorts; it is filled
 * with interleaved S16 PCM. Returns the samples-per-channel decoded (0, 384, 576 or
 * 1152); 0 means no/invalid frame at the head of `in`. `*frame_bytes` is how many input
 * bytes this frame (or skipped junk, when the return is 0) consumed — the caller MUST
 * advance its read cursor by that much, even on a 0 return, or it spins on bad data.
 */
int mp3_decode(const unsigned char *in, int in_len, short *out, int *channels,
               int *hz, int *frame_bytes, int *bitrate_kbps) {
    mp3dec_frame_info_t info;
    if (!g_dec || !echoputer_scratch) {
        *channels = 0;
        *hz = 0;
        *frame_bytes = 0;
        *bitrate_kbps = 0;
        return 0;
    }
    int samples = mp3dec_decode_frame(g_dec, in, in_len, out, &info);
    *channels = info.channels;
    *hz = info.hz;
    *frame_bytes = info.frame_bytes;
    *bitrate_kbps = info.bitrate_kbps;
    return samples;
}

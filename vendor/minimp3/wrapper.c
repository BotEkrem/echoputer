/* Thin C ABI over the vendored minimp3 (single-header, public-domain CC0).
 *
 * Mirrors the Peanut-GB `wrapper.c` pattern: the decoder state lives in a file
 * `static` here (it is ~6.7 KB and the per-call scratch is stack-heavy, so we keep
 * it out of the Rust side / the task stack), and the Rust FFI in
 * `src/apps/player/mp3.rs` only ever calls these `mp3_*` functions.
 *
 * Build defines (set in build.rs): MINIMP3_IMPLEMENTATION (emit the code here),
 * MINIMP3_NO_STDIO (no file I/O — we feed SD bytes), MINIMP3_ONLY_MP3 (drop
 * MP1/MP2), MINIMP3_NO_SIMD (portable path — Xtensa has no x86/ARM SIMD). We do
 * NOT define MINIMP3_FLOAT_OUTPUT: the default S16 output feeds the i16 I2S
 * directly and avoids minimp3's float synthesis filter (the LX-FPU weak spot).
 */

#define MINIMP3_IMPLEMENTATION
#define MINIMP3_NO_STDIO
#define MINIMP3_ONLY_MP3
#define MINIMP3_NO_SIMD
#define MINIMP3_NONSTANDARD_BUT_LOGICAL
#include "minimp3.h"

static mp3dec_t g_dec;

/* (Re)initialise the decoder. Call before decoding a new stream. */
void mp3_dec_init(void) { mp3dec_init(&g_dec); }

/* Decode one MP3 frame from `in[0..in_len]`.
 *
 * `out` must hold at least MINIMP3_MAX_SAMPLES_PER_FRAME (2304) shorts; it is
 * filled with interleaved S16 PCM. Returns the samples-per-channel decoded
 * (0, 384, 576 or 1152); 0 means no/invalid frame at the head of `in`.
 *
 * `*channels`, `*hz` describe the decoded frame; `*frame_bytes` is how many input
 * bytes this frame (or the skipped junk, when the return is 0) consumed — the
 * caller MUST advance its read cursor by that much, even on a 0 return, or it
 * spins forever on bad data.
 */
int mp3_decode(const unsigned char *in, int in_len, short *out, int *channels,
               int *hz, int *frame_bytes, int *bitrate_kbps) {
    mp3dec_frame_info_t info;
    int samples = mp3dec_decode_frame(&g_dec, in, in_len, out, &info);
    *channels = info.channels;
    *hz = info.hz;
    *frame_bytes = info.frame_bytes;
    *bitrate_kbps = info.bitrate_kbps;
    return samples;
}

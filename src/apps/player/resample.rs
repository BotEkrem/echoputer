//! Linear resampler: any source sample rate -> the fixed 16 kHz I2S rate.
//!
//! The ES8311 + tiny onboard speaker can't reproduce much above ~8 kHz, so rather
//! than re-clock the shared I2S per file (an invasive, hardware-unverified change
//! to the audio path that the synth and the Game Boy emulator also drive), the
//! Player resamples every source down/up to the firmware's native 16 kHz. At this
//! speaker's bandwidth the difference from a higher output rate is inaudible, and
//! the existing 16 kHz audio-fill path is reused verbatim.
//!
//! Feed-driven: each decoded/parsed source frame is pushed in via [`feed`], which
//! emits zero or more 16 kHz output frames through the caller's `push` closure
//! (the Player's ring). Continuity (the inter-frame phase + the previous sample)
//! is held across calls, so streaming in arbitrary-sized source chunks is fine.

use crate::apps::synth::SAMPLE_RATE as OUT_RATE;

pub struct Resampler {
    /// Source frames consumed per output frame (= src_rate / 16000). >1 downsamples.
    step: f32,
    /// Position of the next output sample relative to `prev` (0.0 = at `prev`).
    pos: f32,
    prev_l: f32,
    prev_r: f32,
    have_prev: bool,
}

impl Resampler {
    pub const fn new() -> Self {
        Resampler { step: 1.0, pos: 0.0, prev_l: 0.0, prev_r: 0.0, have_prev: false }
    }

    /// Set the source rate (call on track open / when an MP3's rate is first known)
    /// and restart the phase.
    pub fn set_rate(&mut self, src_rate: u32) {
        let sr = if src_rate == 0 { OUT_RATE } else { src_rate };
        self.step = sr as f32 / OUT_RATE as f32;
        self.reset();
    }

    /// Drop the carried phase/sample (call on seek / track change).
    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.have_prev = false;
    }

    /// Feed one source frame; emits 0+ interpolated 16 kHz output frames via `push`.
    /// `push` returning false (ring full) is ignored: only the *output* is dropped,
    /// the resampler phase still advances, so playback stays in sync.
    #[inline]
    pub fn feed<F: FnMut(i16, i16) -> bool>(&mut self, l: i16, r: i16, push: &mut F) {
        let (cl, cr) = (l as f32, r as f32);
        if !self.have_prev {
            self.prev_l = cl;
            self.prev_r = cr;
            self.have_prev = true;
            return;
        }
        while self.pos < 1.0 {
            let t = self.pos;
            let ol = self.prev_l + (cl - self.prev_l) * t;
            let or = self.prev_r + (cr - self.prev_r) * t;
            let _ = push(ol as i16, or as i16);
            self.pos += self.step;
        }
        self.pos -= 1.0;
        self.prev_l = cl;
        self.prev_r = cr;
    }
}

//! Tiny polyphonic wavetable synth (f32 — the ESP32-S3 has a single-precision FPU).
//!
//! Each keypress triggers a plucked voice that rings out and fades, so mashing
//! keys layers overlapping notes into a melody. Output is 16-bit stereo PCM
//! (the ES8311 is mono, but the I2S frame is stereo; both channels carry the mix).

use crate::apps::scales::{Mode, Timbre};

/// I2S sample rate. Must match the ES8311 clock configuration (see audio driver).
pub const SAMPLE_RATE: u32 = 16_000;

const SR: f32 = SAMPLE_RATE as f32;
const TABLE_LEN: usize = 1024;
const TABLE_MASK: usize = TABLE_LEN - 1;
const VOICES: usize = 12;
const ATTACK_SECS: f32 = 0.004;
const ENV_CUTOFF: f32 = 0.0009;
const MASTER: f32 = 0.30;
/// 2^(7/12) — a perfect fifth, for rock power chords.
const FIFTH_RATIO: f32 = 1.498_307;

#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    in_attack: bool,
    phase: f32, // 0.0..1.0
    inc: f32,
    phase5: f32,
    inc5: f32,
    fifth: bool,
    env: f32,
    atk_inc: f32,
    decay: f32, // per-sample multiplier
    timbre: Timbre,
}

impl Voice {
    const SILENT: Voice = Voice {
        active: false,
        in_attack: false,
        phase: 0.0,
        inc: 0.0,
        phase5: 0.0,
        inc5: 0.0,
        fifth: false,
        env: 0.0,
        atk_inc: 0.0,
        decay: 0.0,
        timbre: Timbre::Sine,
    };

    #[inline]
    fn render(&mut self, sine: &[f32; TABLE_LEN]) -> f32 {
        if !self.active {
            return 0.0;
        }
        if self.in_attack {
            self.env += self.atk_inc;
            if self.env >= 1.0 {
                self.env = 1.0;
                self.in_attack = false;
            }
        } else {
            self.env *= self.decay;
            if self.env < ENV_CUTOFF {
                self.active = false;
                return 0.0;
            }
        }

        let mut s = osc(self.timbre, self.phase, sine);
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }

        if self.fifth {
            let s5 = osc(self.timbre, self.phase5, sine);
            self.phase5 += self.inc5;
            if self.phase5 >= 1.0 {
                self.phase5 -= 1.0;
            }
            s = (s + s5) * 0.6;
        }

        s * self.env
    }
}

#[inline]
fn osc(t: Timbre, phase: f32, sine: &[f32; TABLE_LEN]) -> f32 {
    match t {
        Timbre::Sine => sine[((phase * TABLE_LEN as f32) as usize) & TABLE_MASK],
        Timbre::Triangle => {
            if phase < 0.5 {
                4.0 * phase - 1.0
            } else {
                3.0 - 4.0 * phase
            }
        }
        Timbre::RockSaw => {
            // sawtooth pushed into hard clipping -> gritty overdrive
            let saw = 2.0 * phase - 1.0;
            (saw * 3.2).clamp(-1.0, 1.0)
        }
    }
}

/// Number of discrete volume steps (UI shows 0..=VOL_MAX).
pub const VOL_MAX: u8 = 10;

pub struct Synth {
    sine: [f32; TABLE_LEN],
    voices: [Voice; VOICES],
    /// Smoothed output level, for the UI VU meter (0.0..~1.0).
    level: f32,
    /// User volume, 0..=VOL_MAX.
    vol: u8,
    /// Whether Rock mode stacks a power-chord fifth.
    power_chord: bool,
}

impl Synth {
    pub fn new() -> Self {
        let mut sine = [0.0f32; TABLE_LEN];
        let mut i = 0;
        while i < TABLE_LEN {
            let ph = (i as f32) / (TABLE_LEN as f32);
            sine[i] = libm::sinf(ph * core::f32::consts::TAU);
            i += 1;
        }
        Self {
            sine,
            voices: [Voice::SILENT; VOICES],
            level: 0.0,
            vol: 8,
            power_chord: true,
        }
    }

    pub fn volume(&self) -> u8 {
        self.vol
    }

    pub fn set_volume(&mut self, v: u8) {
        self.vol = v.min(VOL_MAX);
    }

    pub fn set_power_chord(&mut self, on: bool) {
        self.power_chord = on;
    }

    pub fn volume_up(&mut self) {
        if self.vol < VOL_MAX {
            self.vol += 1;
        }
    }

    pub fn volume_down(&mut self) {
        if self.vol > 0 {
            self.vol -= 1;
        }
    }

    /// Start a new note.
    pub fn trigger(&mut self, freq: f32, mode: Mode) {
        let pc = self.power_chord;
        let idx = self.alloc_voice();
        let v = &mut self.voices[idx];
        v.active = true;
        v.in_attack = true;
        v.env = 0.0;
        v.phase = 0.0;
        v.phase5 = 0.0;
        v.inc = freq / SR;
        v.inc5 = freq * FIFTH_RATIO / SR;
        v.fifth = pc && mode == Mode::Rock;
        v.timbre = mode.timbre();
        v.atk_inc = 1.0 / (ATTACK_SECS * SR);
        v.decay = libm::expf(-1.0 / (mode.decay_secs() * SR));
    }

    /// Pick a free voice, or steal the quietest one.
    fn alloc_voice(&mut self) -> usize {
        let mut quietest = 0usize;
        let mut min_env = f32::INFINITY;
        for (i, v) in self.voices.iter().enumerate() {
            if !v.active {
                return i;
            }
            if v.env < min_env {
                min_env = v.env;
                quietest = i;
            }
        }
        quietest
    }

    /// Render interleaved stereo (L,R,L,R...) into `out`; `out.len()` must be even.
    pub fn fill_stereo(&mut self, out: &mut [i16]) {
        let frames = out.len() / 2;
        let mut peak = 0.0f32;
        for f in 0..frames {
            let mut mix = 0.0f32;
            for v in self.voices.iter_mut() {
                mix += v.render(&self.sine);
            }
            mix *= MASTER * (self.vol as f32 / VOL_MAX as f32);
            let s = mix.clamp(-1.0, 1.0);
            let a = if s < 0.0 { -s } else { s };
            if a > peak {
                peak = a;
            }
            let val = (s * 30_000.0) as i16;
            out[2 * f] = val;
            out[2 * f + 1] = val;
        }
        // smooth the VU level toward this block's peak
        self.level += (peak - self.level) * 0.30;
    }

    /// Silence all voices immediately (used when leaving the synth screen).
    pub fn silence(&mut self) {
        for v in self.voices.iter_mut() {
            v.active = false;
        }
        self.level = 0.0;
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }
}

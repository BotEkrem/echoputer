//! Musical scales and note math.
//!
//! Every playable note is drawn from a pentatonic/blues scale, so no matter which
//! keys are mashed the result stays consonant ("spam the keyboard -> a song").

/// Timbre flavour produced by [`Mode`]. Consumed by the synth voice renderer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Timbre {
    /// Soft sine — bright and clean.
    Sine,
    /// Mellow triangle — lo-fi / melancholic.
    Triangle,
    /// Distorted saw + power-chord fifth — rock.
    RockSaw,
}

/// Playing mode, cycled with the G0 button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Major pentatonic — happy / playful.
    MajorPenta,
    /// Minor pentatonic — lo-fi / moody.
    MinorPenta,
    /// Minor pentatonic + blue note, distorted, with a stacked fifth — rock 🤘
    Rock,
}

impl Mode {
    /// Cycle to the next mode (wraps).
    pub fn next(self) -> Mode {
        match self {
            Mode::MajorPenta => Mode::MinorPenta,
            Mode::MinorPenta => Mode::Rock,
            Mode::Rock => Mode::MajorPenta,
        }
    }

    /// Cycle to the previous mode (wraps). With 3 modes, prev == next twice.
    pub fn prev(self) -> Mode {
        self.next().next()
    }

    /// Stable index for persistence.
    pub fn index(self) -> u8 {
        match self {
            Mode::MajorPenta => 0,
            Mode::MinorPenta => 1,
            Mode::Rock => 2,
        }
    }

    pub fn from_index(i: u8) -> Mode {
        match i {
            1 => Mode::MinorPenta,
            2 => Mode::Rock,
            _ => Mode::MajorPenta,
        }
    }

    /// Short label for the UI.
    pub fn name(self) -> &'static str {
        match self {
            Mode::MajorPenta => "MAJOR",
            Mode::MinorPenta => "MINOR",
            Mode::Rock => "ROCK",
        }
    }

    /// Semitone offsets from the root for one octave of this scale.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Mode::MajorPenta => &[0, 2, 4, 7, 9],
            Mode::MinorPenta => &[0, 3, 5, 7, 10],
            // minor pentatonic + b5 "blue note" -> classic blues/rock box
            Mode::Rock => &[0, 3, 5, 6, 7, 10],
        }
    }

    pub fn timbre(self) -> Timbre {
        match self {
            Mode::MajorPenta => Timbre::Sine,
            Mode::MinorPenta => Timbre::Triangle,
            Mode::Rock => Timbre::RockSaw,
        }
    }

    /// Per-note ring-out time in seconds. Rock sustains longer (power chord).
    pub fn decay_secs(self) -> f32 {
        match self {
            Mode::MajorPenta => 0.55,
            Mode::MinorPenta => 0.70,
            Mode::Rock => 1.10,
        }
    }
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// MIDI note number -> frequency in Hz.
pub fn midi_to_freq(midi: u8) -> f32 {
    libm::powf(2.0, (midi as f32 - 69.0) / 12.0) * 440.0
}

/// The MIDI note for the `degree`-th step of `mode` above `root`, climbing
/// through octaves so larger degrees give higher pitches.
pub fn scale_note_midi(mode: Mode, root: u8, degree: usize) -> u8 {
    let iv = mode.intervals();
    let octave = (degree / iv.len()) as u16;
    let semis = root as u16 + octave * 12 + iv[degree % iv.len()] as u16;
    semis.min(127) as u8
}

/// Human-readable note name like "C4" into `buf`, returning the written slice.
pub fn note_name<'a>(midi: u8, buf: &'a mut [u8; 4]) -> &'a str {
    let name = NOTE_NAMES[(midi % 12) as usize];
    let octave = (midi / 12) as i8 - 1; // MIDI 60 == C4
    let nb = name.as_bytes();
    let mut i = 0;
    for &b in nb {
        buf[i] = b;
        i += 1;
    }
    if (0..=9).contains(&octave) {
        buf[i] = b'0' + octave as u8;
        i += 1;
    }
    // SAFETY: only ASCII letters/'#'/digits written above.
    core::str::from_utf8(&buf[..i]).unwrap_or("?")
}

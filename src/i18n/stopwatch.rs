//! stopwatch screen translations.
use super::Msg;

pub const STOPWATCH_UPPER: Msg = ["STOPWATCH", "KRONOMETRE"];
pub const TIMER_UPPER: Msg = ["TIMER", "GERI SAYIM"];
pub const STOPWATCH: Msg = ["Stopwatch", "Kronometre"];
pub const DONE: Msg = ["DONE", "BITTI"];
pub const RUNNING: Msg = ["RUNNING", "CALISIYOR"];
pub const PAUSED: Msg = ["PAUSED", "DURDU"];
pub const HINT_TIMER_PAUSED: Msg = [
    "Enter run  <> mode  up/dn +-10s  r reset",
    "Enter calis  <> mod  yuk/asa +-10s  r sifir",
];
pub const HINT_DEFAULT: Msg = ["Enter run/pause  <> mode  r reset", "Enter calis/dur  <> mod  r sifir"];

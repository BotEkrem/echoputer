//! ir screen translations.
use super::Msg;

pub const CUSTOM: Msg = ["Custom", "Ozel"];
pub const POWER_ALL: Msg = ["Power: ALL TVs", "Guc: TUM TV'ler"];
pub const SENT: Msg = ["sent", "gonderildi"];
#[allow(dead_code)] // temporarily unused during the blink-test diagnostic
pub const SENDING: Msg = ["blasting all codes...", "tum kodlar gonderiliyor..."];
pub const AIM_HINT: Msg = ["aim the top edge at the device", "ust kenari cihaza dogrult"];
pub const CUSTOM_HINT: Msg = ["type hex  bksp  ENTER send  ` back", "hex yaz  bksp  ENTER gonder  ` geri"];
pub const PICK_HINT: Msg = ["UP/DN pick  ENTER send  ` back", "YUK/AS sec  ENTER gonder  ` geri"];

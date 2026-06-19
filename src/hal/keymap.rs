//! Cardputer keyboard (TCA8418 4x14 matrix) -> character map, for text entry (the
//! REPL and the Hacking SSID/portal name fields). Case is chosen by the
//! caller's `shift` flag, which the "Aa" key toggles; digits, symbols and space
//! pass through. Modifier / navigation keys map to None.
//!
//! Layout matches the M5Cardputer silkscreen:
//!   row0:  ` 1 2 3 4 5 6 7 8 9 0 - =  <bksp>
//!   row1: <tab> q w e r t y u i o p [ ] \
//!   row2: <fn> <shift> a s d f g h j k l ; ' <enter>
//!   row3: <ctrl><opt><alt> z x c v b n m , . / <space>

// Lowercase base + shifted layers. `shift` (toggled by the "Aa" key) selects
// between them, giving lowercase identifiers or uppercase + symbols like ( ) " :.
const LOWER: [[u8; 14]; 4] = [
    [b'`', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', 0],
    [0, b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', b'[', b']', b'\\'],
    [0, 0, b'a', b's', b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', 0],
    [0, 0, 0, b'z', b'x', b'c', b'v', b'b', b'n', b'm', b',', b'.', b'/', b' '],
];
const SHIFTED: [[u8; 14]; 4] = [
    [b'~', b'!', b'@', b'#', b'$', b'%', b'^', b'&', b'*', b'(', b')', b'_', b'+', 0],
    [0, b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', b'O', b'P', b'{', b'}', b'|'],
    [0, 0, b'A', b'S', b'D', b'F', b'G', b'H', b'J', b'K', b'L', b':', b'"', 0],
    [0, 0, 0, b'Z', b'X', b'C', b'V', b'B', b'N', b'M', b'<', b'>', b'?', b' '],
];

/// The typed byte for a key, honouring `shift` (lowercase base / shifted symbols).
/// None for modifier/enter/backspace/unknown keys.
pub fn ch_shift(row: u8, col: u8, shift: bool) -> Option<u8> {
    if (row as usize) < 4 && (col as usize) < 14 {
        let b = if shift { SHIFTED } else { LOWER }[row as usize][col as usize];
        if b != 0 {
            return Some(b);
        }
    }
    None
}

/// Backspace key (top-right of row 0).
pub const K_BKSP: (u8, u8) = (0, 13);
/// Left shift key, tracked by callers of [`ch_shift`].
pub const K_SHIFT: (u8, u8) = (2, 1);

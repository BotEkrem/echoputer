//! Cardputer keyboard (TCA8418 4x14 matrix) -> character map, for text entry in
//! tool settings. Letters come out UPPERCASE (clean for SSIDs); digits, a few
//! symbols and space are passed through. Modifier / navigation keys map to None.
//!
//! Layout matches the M5Cardputer silkscreen:
//!   row0:  ` 1 2 3 4 5 6 7 8 9 0 - =  <bksp>
//!   row1: <tab> q w e r t y u i o p [ ] \
//!   row2: <fn> <shift> a s d f g h j k l ; ' <enter>
//!   row3: <ctrl><opt><alt> z x c v b n m , . / <space>

const MAP: [[u8; 14]; 4] = [
    [b'`', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', 0],
    [0, b'Q', b'W', b'E', b'R', b'T', b'Y', b'U', b'I', b'O', b'P', b'[', b']', b'\\'],
    [0, 0, b'A', b'S', b'D', b'F', b'G', b'H', b'J', b'K', b'L', b';', b'\'', 0],
    [0, 0, 0, b'Z', b'X', b'C', b'V', b'B', b'N', b'M', b',', b'.', b'/', b' '],
];

/// The typed byte for a key, or None for modifier/enter/backspace/unknown keys.
pub fn ch(row: u8, col: u8) -> Option<u8> {
    if (row as usize) < 4 && (col as usize) < 14 {
        let b = MAP[row as usize][col as usize];
        if b != 0 {
            return Some(b);
        }
    }
    None
}

/// Backspace key (top-right of row 0).
pub const K_BKSP: (u8, u8) = (0, 13);

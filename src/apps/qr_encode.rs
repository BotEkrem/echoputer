//! Self-contained QR-code encoder. `no_std` + `alloc`, no external crates.
//!
//! Byte (8-bit) mode only, error-correction level M, versions 1..=10
//! (QR Model 2 / ISO 18004). The encoder picks the smallest version that
//! fits, builds the data bitstream, computes Reed-Solomon error correction
//! over GF(256), interleaves the blocks, lays out the module matrix
//! (function patterns + zig-zag data placement), evaluates all eight data
//! masks with the four standard penalty rules and keeps the lowest-penalty
//! one, then writes the BCH format/version information.
//!
//! The output round-trips cleanly through standard QR decoders.

use alloc::vec;
use alloc::vec::Vec;

/// An encoded QR code: a square matrix of dark/light modules.
pub struct Qr {
    size: usize,
    modules: Vec<bool>,
}

impl Qr {
    /// Encode `data` in BYTE mode, error-correction level M, choosing the smallest
    /// version in 1..=10 that fits. Returns None if it doesn't fit version 10.
    pub fn encode(data: &[u8]) -> Option<Qr> {
        // Find the smallest version (1..=10) whose data capacity (in codewords)
        // can hold the encoded byte-mode stream.
        let mut version = 0usize;
        for v in 1..=10usize {
            let cap_bits = total_data_codewords(v) * 8;
            // mode (4) + count indicator + payload (8 bits each).
            let needed = 4 + char_count_bits(v) + data.len() * 8;
            if needed <= cap_bits {
                version = v;
                break;
            }
        }
        if version == 0 {
            return None;
        }

        // ---- Build the bit stream ----------------------------------------
        let mut bits = BitBuffer::new();
        // Mode indicator: byte mode = 0b0100.
        bits.push_bits(0b0100, 4);
        // Character count indicator.
        bits.push_bits(data.len() as u32, char_count_bits(version));
        // Payload bytes.
        for &b in data {
            bits.push_bits(b as u32, 8);
        }

        let total_data_cw = total_data_codewords(version);
        let capacity_bits = total_data_cw * 8;

        // Terminator: up to 4 zero bits.
        let remaining = capacity_bits - bits.len();
        let term = if remaining < 4 { remaining } else { 4 };
        bits.push_bits(0, term);

        // Pad to a byte boundary.
        while bits.len() % 8 != 0 {
            bits.push_bits(0, 1);
        }

        // Pad bytes 0xEC, 0x11 alternating until capacity.
        let mut pad_toggle = true;
        while bits.len() < capacity_bits {
            bits.push_bits(if pad_toggle { 0xEC } else { 0x11 }, 8);
            pad_toggle = !pad_toggle;
        }

        let data_codewords = bits.into_bytes();

        // ---- Split into blocks and compute EC ----------------------------
        let (ec_per_block, g1_blocks, g1_words, g2_blocks, g2_words) = ec_block_info(version);

        let mut data_blocks: Vec<Vec<u8>> = Vec::new();
        let mut ec_blocks: Vec<Vec<u8>> = Vec::new();

        let mut pos = 0usize;
        for _ in 0..g1_blocks {
            let blk = data_codewords[pos..pos + g1_words].to_vec();
            pos += g1_words;
            let ec = reed_solomon(&blk, ec_per_block);
            data_blocks.push(blk);
            ec_blocks.push(ec);
        }
        for _ in 0..g2_blocks {
            let blk = data_codewords[pos..pos + g2_words].to_vec();
            pos += g2_words;
            let ec = reed_solomon(&blk, ec_per_block);
            data_blocks.push(blk);
            ec_blocks.push(ec);
        }

        // ---- Interleave: data codewords then EC codewords ----------------
        let mut final_words: Vec<u8> = Vec::new();
        let max_data_len = g1_words.max(g2_words);
        for i in 0..max_data_len {
            for blk in &data_blocks {
                if i < blk.len() {
                    final_words.push(blk[i]);
                }
            }
        }
        for i in 0..ec_per_block {
            for blk in &ec_blocks {
                final_words.push(blk[i]);
            }
        }

        // Bit stream of the final message, plus remainder bits (zeros).
        let mut msg_bits: Vec<bool> = Vec::with_capacity(final_words.len() * 8 + 7);
        for &b in &final_words {
            for i in (0..8).rev() {
                msg_bits.push((b >> i) & 1 == 1);
            }
        }
        for _ in 0..remainder_bits(version) {
            msg_bits.push(false);
        }

        // ---- Build the module matrix -------------------------------------
        let size = version * 4 + 17;
        let mut matrix = Matrix::new(size);
        matrix.place_function_patterns(version);
        matrix.place_data(&msg_bits);

        // ---- Masking: evaluate all 8, keep the lowest-penalty one --------
        let mut best_penalty = u32::MAX;
        let mut best_matrix: Option<Matrix> = None;
        for mask in 0..8usize {
            let mut m = matrix.clone();
            m.apply_mask(mask);
            m.place_format_info(mask);
            if version >= 7 {
                m.place_version_info(version);
            }
            let p = m.penalty();
            if p < best_penalty {
                best_penalty = p;
                best_matrix = Some(m);
            }
        }
        let final_matrix = best_matrix?;

        Some(Qr {
            size,
            modules: final_matrix.modules,
        })
    }

    /// Side length in modules (the QR is size x size, no quiet zone included).
    pub fn size(&self) -> usize {
        self.size
    }

    /// True if module (x, y) is dark/black. x and y are in 0..size().
    pub fn module(&self, x: usize, y: usize) -> bool {
        self.modules[y * self.size + x]
    }
}

// ===================================================================
// Bit buffer
// ===================================================================

struct BitBuffer {
    bits: Vec<bool>,
}

impl BitBuffer {
    fn new() -> Self {
        BitBuffer { bits: Vec::new() }
    }
    fn len(&self) -> usize {
        self.bits.len()
    }
    fn push_bits(&mut self, value: u32, count: usize) {
        for i in (0..count).rev() {
            self.bits.push((value >> i) & 1 == 1);
        }
    }
    fn into_bytes(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.bits.len() / 8);
        let mut acc = 0u8;
        let mut n = 0usize;
        for b in self.bits {
            acc = (acc << 1) | (b as u8);
            n += 1;
            if n == 8 {
                out.push(acc);
                acc = 0;
                n = 0;
            }
        }
        out
    }
}

// ===================================================================
// Version / level-M tables
// ===================================================================

/// Character-count indicator width (byte mode): 8 bits for v1-9, 16 for v10+.
fn char_count_bits(version: usize) -> usize {
    if version <= 9 {
        8
    } else {
        16
    }
}

/// Total number of DATA codewords for level M, versions 1..=10.
fn total_data_codewords(version: usize) -> usize {
    match version {
        1 => 16,
        2 => 28,
        3 => 44,
        4 => 64,
        5 => 86,
        6 => 108,
        7 => 124,
        8 => 154,
        9 => 182,
        10 => 216,
        _ => 0,
    }
}

/// Returns (ec_codewords_per_block, group1_blocks, group1_data_words,
/// group2_blocks, group2_data_words) for level M.
fn ec_block_info(version: usize) -> (usize, usize, usize, usize, usize) {
    match version {
        1 => (10, 1, 16, 0, 0),
        2 => (16, 1, 28, 0, 0),
        3 => (26, 1, 44, 0, 0),
        4 => (18, 2, 32, 0, 0),
        5 => (24, 2, 43, 0, 0),
        6 => (16, 4, 27, 0, 0),
        7 => (18, 4, 31, 0, 0),
        8 => (22, 2, 38, 2, 39),
        9 => (22, 3, 36, 2, 37),
        10 => (26, 4, 43, 1, 44),
        _ => (0, 0, 0, 0, 0),
    }
}

/// Number of remainder bits appended after the data, per version.
fn remainder_bits(version: usize) -> usize {
    match version {
        2..=6 => 7,
        _ => 0,
    }
}

/// Alignment-pattern center coordinates per version (positions list). Centers
/// are the cartesian product of these positions, minus those that overlap a
/// finder pattern.
fn alignment_positions(version: usize) -> &'static [usize] {
    match version {
        2 => &[6, 18],
        3 => &[6, 22],
        4 => &[6, 26],
        5 => &[6, 30],
        6 => &[6, 34],
        7 => &[6, 22, 38],
        8 => &[6, 24, 42],
        9 => &[6, 26, 46],
        10 => &[6, 28, 50],
        _ => &[],
    }
}

// ===================================================================
// GF(256) Reed-Solomon (primitive polynomial 0x11D, generator alpha = 2)
// ===================================================================

struct Gf {
    exp: [u8; 512],
    log: [u8; 256],
}

impl Gf {
    fn new() -> Self {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for i in 0..255usize {
            exp[i] = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11D;
            }
        }
        for i in 255..512usize {
            exp[i] = exp[i - 255];
        }
        Gf { exp, log }
    }
    fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
        }
    }
}

/// Compute `ec_count` Reed-Solomon error-correction codewords for `data`.
fn reed_solomon(data: &[u8], ec_count: usize) -> Vec<u8> {
    let gf = Gf::new();

    // Generator polynomial as the product of (x - alpha^i) for i in 0..ec_count,
    // stored leading-coefficient-first (gen[0] is the x^ec_count term, always 1).
    let mut gen = vec![1u8];
    for i in 0..ec_count {
        // Multiply gen by (x - alpha^i) = (x + alpha^i) in GF(256).
        let alpha = gf.exp[i];
        let mut next = vec![0u8; gen.len() + 1];
        for (j, &g) in gen.iter().enumerate() {
            // x * g(x) keeps the same index (leading-first); alpha^i * g(x)
            // shifts toward the constant end (index + 1).
            next[j] ^= g;
            next[j + 1] ^= gf.mul(g, alpha);
        }
        gen = next;
    }

    // Polynomial long division of (data * x^ec_count) by gen. With gen[0] == 1
    // the leading term of the running remainder is cancelled at each step.
    let mut result = vec![0u8; data.len() + ec_count];
    result[..data.len()].copy_from_slice(data);

    for i in 0..data.len() {
        let coef = result[i];
        if coef != 0 {
            for j in 0..gen.len() {
                result[i + j] ^= gf.mul(gen[j], coef);
            }
        }
    }
    result[data.len()..].to_vec()
}

// ===================================================================
// Module matrix
// ===================================================================

#[derive(Clone)]
struct Matrix {
    size: usize,
    modules: Vec<bool>,
    /// True where a function pattern lives (must not be touched by data/mask).
    reserved: Vec<bool>,
}

impl Matrix {
    fn new(size: usize) -> Self {
        Matrix {
            size,
            modules: vec![false; size * size],
            reserved: vec![false; size * size],
        }
    }

    #[inline]
    fn idx(&self, x: usize, y: usize) -> usize {
        y * self.size + x
    }

    fn set(&mut self, x: usize, y: usize, dark: bool) {
        let i = self.idx(x, y);
        self.modules[i] = dark;
    }
    fn get(&self, x: usize, y: usize) -> bool {
        self.modules[self.idx(x, y)]
    }
    fn reserve(&mut self, x: usize, y: usize) {
        let i = self.idx(x, y);
        self.reserved[i] = true;
    }
    fn is_reserved(&self, x: usize, y: usize) -> bool {
        self.reserved[self.idx(x, y)]
    }

    fn set_fn(&mut self, x: usize, y: usize, dark: bool) {
        self.set(x, y, dark);
        self.reserve(x, y);
    }

    fn place_function_patterns(&mut self, version: usize) {
        let size = self.size;

        // Finder patterns (with separators) at three corners.
        self.place_finder(0, 0);
        self.place_finder(size - 7, 0);
        self.place_finder(0, size - 7);

        // Timing patterns.
        for i in 8..size - 8 {
            let dark = i % 2 == 0;
            self.set_fn(i, 6, dark);
            self.set_fn(6, i, dark);
        }

        // Dark module.
        self.set_fn(8, size - 8, true);

        // Reserve format-info areas.
        self.reserve_format_areas();

        // Alignment patterns (skip ones overlapping the finder patterns).
        let positions = alignment_positions(version);
        for &ay in positions {
            for &ax in positions {
                let near_tl = ax <= 7 && ay <= 7;
                let near_tr = ax >= size - 8 && ay <= 7;
                let near_bl = ax <= 7 && ay >= size - 8;
                if near_tl || near_tr || near_bl {
                    continue;
                }
                self.place_alignment(ax, ay);
            }
        }

        // Reserve version-info areas (v >= 7).
        if version >= 7 {
            self.reserve_version_areas();
        }
    }

    fn place_finder(&mut self, ox: usize, oy: usize) {
        // 7x7 finder.
        for dy in 0..7 {
            for dx in 0..7 {
                let dark = dx == 0
                    || dx == 6
                    || dy == 0
                    || dy == 6
                    || (dx >= 2 && dx <= 4 && dy >= 2 && dy <= 4);
                self.set_fn(ox + dx, oy + dy, dark);
            }
        }
        // Separator: a one-module light border. Sweep the full frame
        // (offsets -1..=7 on both axes) and reserve every cell that lies just
        // outside the finder but inside the matrix.
        let size = self.size;
        for dy in -1isize..=7 {
            for dx in -1isize..=7 {
                if dx >= 0 && dx <= 6 && dy >= 0 && dy <= 6 {
                    continue; // finder interior, already drawn
                }
                let cx = ox as isize + dx;
                let cy = oy as isize + dy;
                if cx >= 0 && cy >= 0 && (cx as usize) < size && (cy as usize) < size {
                    self.set_fn(cx as usize, cy as usize, false);
                }
            }
        }
    }

    fn place_alignment(&mut self, cx: usize, cy: usize) {
        for dy in -2isize..=2 {
            for dx in -2isize..=2 {
                let x = (cx as isize + dx) as usize;
                let y = (cy as isize + dy) as usize;
                let ring = dx.abs() == 2 || dy.abs() == 2;
                let center = dx == 0 && dy == 0;
                self.set_fn(x, y, ring || center);
            }
        }
    }

    fn reserve_format_areas(&mut self) {
        let size = self.size;
        for i in 0..9 {
            self.reserve(i, 8);
            self.reserve(8, i);
        }
        for i in 0..8 {
            self.reserve(size - 1 - i, 8);
            self.reserve(8, size - 1 - i);
        }
    }

    fn reserve_version_areas(&mut self) {
        let size = self.size;
        // Two 6x3 blocks adjacent to the top-right and bottom-left finders.
        for y in 0..6 {
            for x in 0..3 {
                self.reserve(x + size - 11, y);
                self.reserve(y, x + size - 11);
            }
        }
    }

    fn place_data(&mut self, bits: &[bool]) {
        let size = self.size;
        let mut bit_idx = 0usize;
        let mut col = (size - 1) as isize;
        let mut upward = true;

        while col > 0 {
            // Skip the vertical timing column.
            if col == 6 {
                col -= 1;
            }
            let rows: Vec<usize> = if upward {
                (0..size).rev().collect()
            } else {
                (0..size).collect()
            };
            for y in rows {
                for c in 0..2 {
                    let x = (col - c) as usize;
                    if !self.is_reserved(x, y) && bit_idx < bits.len() {
                        self.set(x, y, bits[bit_idx]);
                        bit_idx += 1;
                    }
                }
            }
            col -= 2;
            upward = !upward;
        }
    }

    fn mask_condition(mask: usize, x: usize, y: usize) -> bool {
        let (i, j) = (y, x); // i = row, j = column per spec.
        match mask {
            0 => (i + j) % 2 == 0,
            1 => i % 2 == 0,
            2 => j % 3 == 0,
            3 => (i + j) % 3 == 0,
            4 => (i / 2 + j / 3) % 2 == 0,
            5 => (i * j) % 2 + (i * j) % 3 == 0,
            6 => ((i * j) % 2 + (i * j) % 3) % 2 == 0,
            7 => ((i + j) % 2 + (i * j) % 3) % 2 == 0,
            _ => false,
        }
    }

    fn apply_mask(&mut self, mask: usize) {
        let size = self.size;
        for y in 0..size {
            for x in 0..size {
                if !self.is_reserved(x, y) && Self::mask_condition(mask, x, y) {
                    let i = self.idx(x, y);
                    self.modules[i] = !self.modules[i];
                }
            }
        }
    }

    fn place_format_info(&mut self, mask: usize) {
        // Level M = 0b00. 5-bit data = (level << 3) | mask.
        let data = (0b00u32 << 3) | (mask as u32);
        let bits = format_bits(data); // 15-bit codeword.
        let size = self.size;
        let bit = |i: usize| (bits >> i) & 1 == 1; // i in 0..15, 0 = LSB.

        // First copy: winds around the top-left finder corner at (8,8).
        for i in 0..6 {
            self.set_fn(8, i, bit(i));
        }
        self.set_fn(8, 7, bit(6));
        self.set_fn(8, 8, bit(7));
        self.set_fn(7, 8, bit(8));
        for i in 9..15 {
            self.set_fn(14 - i, 8, bit(i));
        }

        // Second copy: bottom-left vertical + top-right horizontal.
        for i in 0..8 {
            self.set_fn(size - 1 - i, 8, bit(i));
        }
        for i in 8..15 {
            self.set_fn(8, size - 15 + i, bit(i));
        }
    }

    fn place_version_info(&mut self, version: usize) {
        let bits = version_bits(version as u32);
        let size = self.size;
        for i in 0..18 {
            let b = (bits >> i) & 1 == 1;
            let x = i / 3;
            let y = size - 11 + (i % 3);
            self.set_fn(x, y, b); // bottom-left block
            self.set_fn(y, x, b); // top-right block (transposed)
        }
    }

    // ---- Penalty rules ----------------------------------------------------

    fn penalty(&self) -> u32 {
        let size = self.size;
        let mut total = 0u32;

        // Rule 1: runs of 5+ same-color modules in a row/column.
        for y in 0..size {
            total += self.run_penalty_line(|i| self.get(i, y));
        }
        for x in 0..size {
            total += self.run_penalty_line(|i| self.get(x, i));
        }

        // Rule 2: 2x2 blocks of the same color.
        for y in 0..size - 1 {
            for x in 0..size - 1 {
                let c = self.get(x, y);
                if self.get(x + 1, y) == c && self.get(x, y + 1) == c && self.get(x + 1, y + 1) == c
                {
                    total += 3;
                }
            }
        }

        // Rule 3: finder-like patterns 1:1:3:1:1 with 4 light on either side.
        let pattern1 = [true, false, true, true, true, false, true, false, false, false, false];
        let pattern2 = [false, false, false, false, true, false, true, true, true, false, true];
        for y in 0..size {
            for x in 0..=size - 11 {
                if self.match_pattern_h(x, y, &pattern1) || self.match_pattern_h(x, y, &pattern2) {
                    total += 40;
                }
            }
        }
        for x in 0..size {
            for y in 0..=size - 11 {
                if self.match_pattern_v(x, y, &pattern1) || self.match_pattern_v(x, y, &pattern2) {
                    total += 40;
                }
            }
        }

        // Rule 4: dark-module proportion deviation from 50%.
        let dark: usize = self.modules.iter().filter(|&&m| m).count();
        let total_modules = size * size;
        let percent = dark * 100 / total_modules;
        let prev = (percent / 5) * 5;
        let next = prev + 5;
        let lower = if (prev as i32 - 50).abs() <= (next as i32 - 50).abs() {
            prev
        } else {
            next
        };
        total += ((lower as i32 - 50).abs() / 5) as u32 * 10;

        total
    }

    fn run_penalty_line<F: Fn(usize) -> bool>(&self, get: F) -> u32 {
        let size = self.size;
        let mut penalty = 0u32;
        let mut run = 1usize;
        let mut prev = get(0);
        for i in 1..size {
            let cur = get(i);
            if cur == prev {
                run += 1;
            } else {
                if run >= 5 {
                    penalty += 3 + (run - 5) as u32;
                }
                run = 1;
                prev = cur;
            }
        }
        if run >= 5 {
            penalty += 3 + (run - 5) as u32;
        }
        penalty
    }

    fn match_pattern_h(&self, x: usize, y: usize, pat: &[bool; 11]) -> bool {
        for k in 0..11 {
            if self.get(x + k, y) != pat[k] {
                return false;
            }
        }
        true
    }
    fn match_pattern_v(&self, x: usize, y: usize, pat: &[bool; 11]) -> bool {
        for k in 0..11 {
            if self.get(x, y + k) != pat[k] {
                return false;
            }
        }
        true
    }
}

// ===================================================================
// BCH codes for format / version info
// ===================================================================

/// 15-bit format information: BCH(15,5), generator 0x537, masked with 0x5412.
fn format_bits(data: u32) -> u32 {
    let mut v = data << 10;
    let g = 0x537u32;
    while bit_len(v) >= 11 {
        v ^= g << (bit_len(v) - 11);
    }
    ((data << 10) | v) ^ 0x5412
}

/// 18-bit version information: BCH(18,6), generator 0x1F25.
fn version_bits(version: u32) -> u32 {
    let mut v = version << 12;
    let g = 0x1F25u32;
    while bit_len(v) >= 13 {
        v ^= g << (bit_len(v) - 13);
    }
    (version << 12) | v
}

fn bit_len(mut v: u32) -> usize {
    let mut len = 0;
    while v != 0 {
        v >>= 1;
        len += 1;
    }
    len
}

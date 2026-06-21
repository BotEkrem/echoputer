//! Minimal RIFF/WAVE parser for the audio Player (pure Rust, no decoder).
//!
//! Handles uncompressed PCM (`audio_format == 1`), 8- or 16-bit, mono or stereo —
//! the formats `ffmpeg -c:a pcm_s16le` / `pcm_u8` produce. Streaming itself is done
//! by the Player (sequential `vm.read` from `data_start`, converted to source
//! frames and resampled); this module only locates the `fmt `/`data` chunks and
//! describes the stream.

use embedded_sdmmc::{BlockDevice, RawFile, TimeSource, VolumeManager};

#[derive(Clone, Copy)]
pub struct WavFmt {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits: u8,
    /// Bytes per sample frame = channels * bits/8.
    pub block_align: u16,
    /// File offset of the first PCM byte.
    pub data_start: u32,
    /// Length of the PCM data in bytes.
    pub data_len: u32,
}

impl WavFmt {
    pub const EMPTY: WavFmt = WavFmt {
        sample_rate: 0,
        channels: 0,
        bits: 0,
        block_align: 0,
        data_start: 0,
        data_len: 0,
    };

    /// Total playing time in whole seconds (0 if unknown).
    pub fn total_secs(&self) -> u32 {
        let bps = self.sample_rate * self.block_align as u32;
        if bps == 0 {
            0
        } else {
            self.data_len / bps
        }
    }

    /// Seconds elapsed at `data_pos` bytes into the PCM data.
    pub fn pos_secs(&self, data_pos: u32) -> u32 {
        let bps = self.sample_rate * self.block_align as u32;
        if bps == 0 {
            0
        } else {
            data_pos / bps
        }
    }

    /// Byte offset within the data for `sec` seconds, block-aligned & clamped.
    pub fn data_off_for_sec(&self, sec: u32) -> u32 {
        let bps = self.sample_rate * self.block_align as u32;
        let mut off = sec.saturating_mul(bps);
        if off > self.data_len {
            off = self.data_len;
        }
        if self.block_align > 0 {
            off -= off % self.block_align as u32;
        }
        off
    }
}

fn le16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Read exactly `buf.len()` bytes from `file` at `off` (false on short read / error).
fn read_at<D: BlockDevice, T: TimeSource>(
    vm: &VolumeManager<D, T>,
    file: RawFile,
    off: u32,
    buf: &mut [u8],
) -> bool {
    if vm.file_seek_from_start(file, off).is_err() {
        return false;
    }
    let mut got = 0;
    while got < buf.len() {
        match vm.read(file, &mut buf[got..]) {
            Ok(0) => return false,
            Ok(n) => got += n,
            Err(_) => return false,
        }
    }
    true
}

/// Parse the header, walking RIFF chunks until `fmt `+`data` are found. Returns
/// the described format, or an error string for the UI.
pub fn parse<D: BlockDevice, T: TimeSource>(
    vm: &VolumeManager<D, T>,
    file: RawFile,
    file_len: u32,
) -> Result<WavFmt, &'static str> {
    let mut hdr = [0u8; 12];
    if !read_at(vm, file, 0, &mut hdr) {
        return Err("read error");
    }
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return Err("not a WAV");
    }

    let mut fmt = WavFmt::EMPTY;
    let mut have_fmt = false;
    let mut off: u32 = 12;
    // Walk chunk headers (8 bytes each: 4-byte id + 4-byte LE size, data padded to
    // even length) until we have both fmt and data, or run off the end.
    while off + 8 <= file_len {
        let mut ch = [0u8; 8];
        if !read_at(vm, file, off, &mut ch) {
            break;
        }
        let id = &ch[0..4];
        let size = le32(&ch[4..8]);
        let body = off + 8;
        if id == b"fmt " {
            let mut f = [0u8; 16];
            if size < 16 || !read_at(vm, file, body, &mut f) {
                return Err("bad fmt");
            }
            let audio_format = le16(&f[0..2]);
            if audio_format != 1 {
                return Err("not PCM");
            }
            fmt.channels = le16(&f[2..4]) as u8;
            fmt.sample_rate = le32(&f[4..8]);
            fmt.bits = le16(&f[14..16]) as u8;
            fmt.block_align = le16(&f[12..14]);
            have_fmt = true;
        } else if id == b"data" {
            fmt.data_start = body;
            // Clamp the data length to what is actually in the file.
            fmt.data_len = size.min(file_len.saturating_sub(body));
            // We have everything we need.
            if have_fmt {
                break;
            }
        }
        // advance past this chunk (sizes are padded to an even byte count)
        let step = 8u32.saturating_add(size).saturating_add(size & 1);
        off = off.saturating_add(step);
    }

    if !have_fmt || fmt.data_start == 0 {
        return Err("bad WAV");
    }
    if !(fmt.bits == 8 || fmt.bits == 16) || !(fmt.channels == 1 || fmt.channels == 2) {
        return Err("unsup format");
    }
    if fmt.block_align == 0 {
        fmt.block_align = (fmt.channels as u16) * (fmt.bits as u16 / 8);
    }
    Ok(fmt)
}

/// Convert a buffer of raw PCM `bytes` (whole frames) into source stereo frames,
/// calling `emit(l, r)` for each. Returns the number of bytes consumed (whole
/// frames only; a trailing partial frame is left for the next call).
pub fn convert<F: FnMut(i16, i16)>(fmt: &WavFmt, bytes: &[u8], mut emit: F) -> usize {
    let ba = fmt.block_align as usize;
    if ba == 0 {
        return 0;
    }
    let frames = bytes.len() / ba;
    match (fmt.bits, fmt.channels) {
        (16, 2) => {
            for i in 0..frames {
                let o = i * 4;
                let l = i16::from_le_bytes([bytes[o], bytes[o + 1]]);
                let r = i16::from_le_bytes([bytes[o + 2], bytes[o + 3]]);
                emit(l, r);
            }
        }
        (16, 1) => {
            for i in 0..frames {
                let o = i * 2;
                let s = i16::from_le_bytes([bytes[o], bytes[o + 1]]);
                emit(s, s);
            }
        }
        (8, 2) => {
            for i in 0..frames {
                let o = i * 2;
                let l = ((bytes[o] as i16) - 128) << 8;
                let r = ((bytes[o + 1] as i16) - 128) << 8;
                emit(l, r);
            }
        }
        (8, 1) => {
            for i in 0..frames {
                let s = ((bytes[i] as i16) - 128) << 8;
                emit(s, s);
            }
        }
        _ => return 0,
    }
    frames * ba
}

//! Streaming MP3 decode for the audio Player — a thin Rust front end over the
//! vendored minimp3 core (compiled by `build.rs`, behind the `player` feature).
//!
//! minimp3's low-level frame API is zero-malloc and STDIO-free: we feed it a window
//! of the file and it returns one frame of S16 PCM plus how many input bytes it
//! consumed. We keep a ~16 KB input buffer (heap-allocated only while a track plays,
//! mirroring the emulator's bank cache — the radio is idle in the Player), refill it
//! from the SD card, slide the consumed bytes off the front, and skip an ID3v2 tag
//! up front so a big album-art tag doesn't push the first frame out of the window.
//!
//! Decoder state (~6.7 KB `mp3dec_t`, plus a stack-heavy per-call scratch) lives in
//! a C `static` in `vendor/minimp3/wrapper.c`, so nothing large rides the task stack.

use alloc::boxed::Box;
use embedded_sdmmc::{BlockDevice, RawFile, TimeSource, VolumeManager};

/// Interleaved S16 samples one decoded frame can hold (MINIMP3_MAX_SAMPLES_PER_FRAME).
pub const MAX_SAMPLES: usize = 1152 * 2;

extern "C" {
    fn mp3_dec_init();
    fn mp3_decode(
        input: *const u8,
        in_len: i32,
        out: *mut i16,
        channels: *mut i32,
        hz: *mut i32,
        frame_bytes: *mut i32,
        bitrate_kbps: *mut i32,
    ) -> i32;
    /// Byte sizes the firmware must allocate for the decoder state + scratch.
    fn mp3_dec_size() -> u32;
    fn mp3_scratch_size() -> u32;
    /// Bind the heap decoder/scratch buffers (NULLs to clear on free).
    fn mp3_set_buffers(dec: *mut u8, scratch: *mut u8);
}

const IN_CAP: usize = 16 * 1024;
/// Refill the input window once it drops below this (keeps enough lookahead for
/// reliable frame sync — the minimp3 README suggests ~10 frames / ~16 KB).
const REFILL_BELOW: usize = 4 * 1024;

pub struct Mp3 {
    inbuf: Option<Box<[u8]>>,
    // Decoder state (~6.7 KB) + per-frame scratch (~16 KB), heap-allocated while
    // playing and handed to the C core (see wrapper.c). `[u32]` for 4-byte alignment
    // (the structs hold floats; Xtensa needs aligned access). Kept off .bss so the
    // permanent reservation doesn't starve the boot stack.
    dec_buf: Option<Box<[u32]>>,
    scratch_buf: Option<Box<[u32]>>,
    in_len: usize,
    eof: bool,
    /// Input bytes consumed by the decoder so far (for the position estimate).
    bytes_done: u64,
    pub hz: u32,
    pub channels: u8,
    pub bitrate_kbps: u32,
}

impl Mp3 {
    pub const fn new() -> Self {
        Mp3 {
            inbuf: None,
            dec_buf: None,
            scratch_buf: None,
            in_len: 0,
            eof: false,
            bytes_done: 0,
            hz: 0,
            channels: 0,
            bitrate_kbps: 0,
        }
    }

    /// Allocate the input window + decoder state + scratch from the heap and bind the
    /// state/scratch into the C core. False if anything didn't allocate.
    pub fn alloc(&mut self) -> bool {
        if self.inbuf.is_some() {
            return true; // already allocated for this session
        }
        let dn = (unsafe { mp3_dec_size() } as usize + 3) / 4;
        let sn = (unsafe { mp3_scratch_size() } as usize + 3) / 4;
        // Fallible: on OOM leave everything None and return false (no panic). Any
        // partially-allocated buffers in the tuple drop here, freeing their heap.
        let (Some(inbuf), Some(dec_buf), Some(scratch_buf)) =
            (super::try_box::<u8>(IN_CAP), super::try_box::<u32>(dn), super::try_box::<u32>(sn))
        else {
            return false;
        };
        self.inbuf = Some(inbuf);
        self.dec_buf = Some(dec_buf);
        self.scratch_buf = Some(scratch_buf);
        let dp = self.dec_buf.as_mut().unwrap().as_mut_ptr() as *mut u8;
        let sp = self.scratch_buf.as_mut().unwrap().as_mut_ptr() as *mut u8;
        unsafe { mp3_set_buffers(dp, sp) };
        true
    }

    pub fn free(&mut self) {
        // Clear the C pointers BEFORE dropping the buffers, so a stray decode can't
        // touch freed memory.
        unsafe { mp3_set_buffers(core::ptr::null_mut(), core::ptr::null_mut()) };
        self.inbuf = None;
        self.dec_buf = None;
        self.scratch_buf = None;
    }

    /// Begin decoding `file` from the top: reset the decoder, clear the window, and
    /// skip an ID3v2 tag if present.
    pub fn start<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>, file: RawFile) {
        unsafe { mp3_dec_init() };
        self.in_len = 0;
        self.eof = false;
        self.bytes_done = 0;
        self.hz = 0;
        self.channels = 0;
        self.bitrate_kbps = 0;
        let mut h = [0u8; 10];
        let skip = if read_at(vm, file, 0, &mut h) && &h[0..3] == b"ID3" {
            // ID3v2 size is a 28-bit syncsafe integer (7 bits per byte).
            let sz = ((h[6] & 0x7f) as u32) << 21
                | ((h[7] & 0x7f) as u32) << 14
                | ((h[8] & 0x7f) as u32) << 7
                | (h[9] & 0x7f) as u32;
            10 + sz
        } else {
            0
        };
        let _ = vm.file_seek_from_start(file, skip);
        self.bytes_done = skip as u64;
    }

    /// Re-seek to `byte` in the file and resync the decoder there (approximate CBR
    /// seek — minimp3 finds the next frame boundary).
    pub fn seek_to<D: BlockDevice, T: TimeSource>(
        &mut self,
        vm: &VolumeManager<D, T>,
        file: RawFile,
        byte: u32,
    ) {
        unsafe { mp3_dec_init() };
        self.in_len = 0;
        self.eof = false;
        self.bytes_done = byte as u64;
        let _ = vm.file_seek_from_start(file, byte);
    }

    fn refill<D: BlockDevice, T: TimeSource>(&mut self, vm: &VolumeManager<D, T>, file: RawFile) {
        if self.eof {
            return;
        }
        let Some(buf) = self.inbuf.as_mut() else { return };
        while self.in_len < buf.len() {
            match vm.read(file, &mut buf[self.in_len..]) {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(n) => self.in_len += n,
                Err(_) => {
                    self.eof = true;
                    break;
                }
            }
        }
    }

    /// Decode the next audio frame into `pcm` (interleaved S16). Returns
    /// `(samples_per_channel, channels)`, or `None` at end of stream.
    pub fn decode<D: BlockDevice, T: TimeSource>(
        &mut self,
        vm: &VolumeManager<D, T>,
        file: RawFile,
        pcm: &mut [i16],
    ) -> Option<(usize, u8)> {
        if self.inbuf.is_none() {
            return None;
        }
        // Bound the work per call: a handful of frames' worth of junk-skipping is
        // plenty; never spin.
        for _ in 0..48 {
            if self.in_len < REFILL_BELOW && !self.eof {
                self.refill(vm, file);
            }
            if self.in_len == 0 {
                return None;
            }
            let (mut ch, mut hz, mut fb, mut br) = (0i32, 0i32, 0i32, 0i32);
            let samples = {
                let buf = self.inbuf.as_ref().unwrap();
                unsafe {
                    mp3_decode(
                        buf.as_ptr(),
                        self.in_len as i32,
                        pcm.as_mut_ptr(),
                        &mut ch,
                        &mut hz,
                        &mut fb,
                        &mut br,
                    )
                }
            };
            if fb <= 0 {
                // Not enough buffered to find a frame: refill, or give up at EOF.
                if self.eof {
                    return None;
                }
                self.refill(vm, file);
                continue;
            }
            let consumed = (fb as usize).min(self.in_len);
            {
                let buf = self.inbuf.as_mut().unwrap();
                buf.copy_within(consumed..self.in_len, 0);
            }
            self.in_len -= consumed;
            self.bytes_done += consumed as u64;
            if samples > 0 {
                self.hz = hz as u32;
                self.channels = ch.max(1) as u8;
                if br > 0 {
                    self.bitrate_kbps = br as u32;
                }
                return Some((samples as usize, ch.max(1) as u8));
            }
            // samples == 0 but bytes consumed: junk/garbage skipped — keep going.
        }
        None
    }

    pub fn pos_secs(&self) -> u32 {
        if self.bitrate_kbps == 0 {
            0
        } else {
            (self.bytes_done / (self.bitrate_kbps as u64 * 125)) as u32
        }
    }

    pub fn total_secs(&self, file_len: u32) -> u32 {
        if self.bitrate_kbps == 0 {
            0
        } else {
            file_len / (self.bitrate_kbps * 125)
        }
    }
}

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

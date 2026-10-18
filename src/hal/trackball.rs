//! Grove Mini Track Ball (Seeed, STM8 @ I2C 0x4A) — a 5-way trackball + click read
//! on a SECOND I2C bus wired to the Grove PORT.A pins (SDA/SCL on GPIO1/GPIO2). The
//! module accumulates movement and click counts in registers 0x00..=0x04; each is
//! read one byte at a time with the module's read-mode handshake:
//!   write [0x00 (read-mode), reg, 1]   then   read 1 byte  (the count).
//!
//! We turn ball motion into arrow-key presses and clicks into ENTER / ESC(=back),
//! emitted as the same `(row,col)` [`KeyEvent`]s the physical keyboard produces, so
//! the entire menu/app navigation path works unchanged:
//!   roll         -> Up / Down / Left / Right
//!   single click -> ENTER
//!   double click -> ESC (back one level; the ` key's job)
//!
//! Robustness: we don't assume whether the module auto-clears a counter on read or
//! on its own timer — [`delta`] handles both (new = cur-last when rising, else cur),
//! so neither re-counts stale motion nor misses a reset.

use crate::hal::tca8418::KeyEvent;
use crate::{K_DOWN, K_ENTER, K_HOME, K_LEFT, K_RIGHT, K_UP};
use embedded_hal::i2c::I2c;
use esp_hal::time::{Duration, Instant};

/// 7-bit I2C address of the Grove Mini Track Ball.
pub const ADDR: u8 = 0x4A;

// Movement / click counters (the module's own frame of reference).
const R_UP: u8 = 0x00;
const R_DOWN: u8 = 0x01;
const R_LEFT: u8 = 0x02;
const R_RIGHT: u8 = 0x03;
const R_CLICK: u8 = 0x04;
/// Prefix byte that puts the module in read mode for the following register.
const READ_MODE: u8 = 0x00;

// Nav keys we synthesize are the crate-root K_* (row,col) constants — one source of
// truth with the physical keyboard, so the dispatch can't tell ball from keypress.

/// How often to read the module over I2C.
const POLL_MS: u64 = 30;
/// Single-vs-double-click window: a 2nd click within this is a double (= ESC).
const DOUBLE_MS: u64 = 350;
/// Deadzone: raw ball counts that must accumulate on an axis before one nav step is
/// emitted. The ball reports lots of tiny counts; without this it feels hair-trigger.
/// Higher = less sensitive (needs a bigger roll per step). Tune to taste. Micro-moves
/// in one direction BANK indefinitely toward this — many tiny rolls = one nav step —
/// and only a change of direction discards the bank (see below).
const STEP_THRESHOLD: u16 = 3;
/// Cap queued steps per axis so one hard flick can't spray a long scroll.
const MAX_QUEUE: u8 = 4;

#[inline]
fn key(rc: (u8, u8)) -> KeyEvent {
    KeyEvent { pressed: true, row: rc.0, col: rc.1 }
}

/// New activity since the last reading, tolerant of either clear strategy: a rising
/// count is `cur-last` (accumulate-until-timer); a drop means the counter was
/// cleared and `cur` is already the fresh total (auto-clear-on-read / wrap).
#[inline]
fn delta(cur: u8, last: u8) -> u8 {
    if cur >= last {
        cur - last
    } else {
        cur
    }
}

fn read_reg<I: I2c>(i2c: &mut I, reg: u8) -> Option<u8> {
    i2c.write(ADDR, &[READ_MODE, reg, 1]).ok()?;
    let mut b = [0u8; 1];
    i2c.read(ADDR, &mut b).ok()?;
    Some(b[0])
}

fn read_all<I: I2c>(i2c: &mut I) -> Option<[u8; 5]> {
    Some([
        read_reg(i2c, R_UP)?,
        read_reg(i2c, R_DOWN)?,
        read_reg(i2c, R_LEFT)?,
        read_reg(i2c, R_RIGHT)?,
        read_reg(i2c, R_CLICK)?,
    ])
}

/// Stateful driver: poll it every main-loop pass via [`Trackball::next`]; it returns
/// at most one synthetic key event per call (queued motion drains one step per pass).
pub struct Trackball {
    last: [u8; 5],         // last raw counts: up,down,left,right,click
    accum: u16,            // raw counts banked for the current `active` direction
    active: Option<usize>, // which direction (0..4 = up,down,left,right) is banking
    queue: [u8; 4],        // pending nav steps to emit: up,down,left,right
    last_poll: Instant,    // I2C read cadence gate
    click_wait: Option<Instant>, // a single click seen, awaiting a possible double
    /// True once the module has ACKed; a build with no ball wired stays false and
    /// [`Trackball::next`] is then a cheap no-op.
    pub present: bool,
}

impl Trackball {
    pub fn new() -> Self {
        Self {
            last: [0; 5],
            accum: 0,
            active: None,
            queue: [0; 4],
            last_poll: Instant::now(),
            click_wait: None,
            present: false,
        }
    }

    /// Probe the module once (reads the click register). Sets + returns `present`.
    pub fn probe<I: I2c>(&mut self, i2c: &mut I) -> bool {
        self.present = read_reg(i2c, R_CLICK).is_some();
        self.present
    }

    fn pop_motion(&mut self) -> Option<KeyEvent> {
        // Axes inverted for this mount (Grove socket pointing DOWN rotates the module
        // 180°): the UP counter drives Down, LEFT drives Right, etc. Index order is the
        // register order [up,down,left,right]; the mapped keys are the flipped pair.
        const DIRS: [(u8, u8); 4] = [K_DOWN, K_UP, K_RIGHT, K_LEFT];
        for i in 0..4 {
            if self.queue[i] > 0 {
                self.queue[i] -= 1;
                return Some(key(DIRS[i]));
            }
        }
        None
    }

    /// The next synthetic key event from the ball, or `None`. Drains queued motion
    /// first, then resolves a pending single-click into ENTER once its double-click
    /// window lapses, then (on cadence) polls the module for new motion/clicks.
    pub fn next<I: I2c>(&mut self, i2c: &mut I) -> Option<KeyEvent> {
        if let Some(ev) = self.pop_motion() {
            return Some(ev);
        }
        // A lone click whose double-click window has passed -> it was a single -> ENTER.
        if let Some(t) = self.click_wait {
            if t.elapsed() >= Duration::from_millis(DOUBLE_MS) {
                self.click_wait = None;
                return Some(key(K_ENTER));
            }
        }
        if !self.present || self.last_poll.elapsed() < Duration::from_millis(POLL_MS) {
            return None;
        }
        self.last_poll = Instant::now();

        let cur = match read_all(i2c) {
            Some(c) => c,
            None => return None, // transient bus error: try again next cadence
        };
        let mut d = [0u16; 4];
        for i in 0..4 {
            d[i] = delta(cur[i], self.last[i]) as u16;
        }
        let clicks = delta(cur[4], self.last[4]);
        self.last = cur;

        // Pick the dominant direction this poll (the ball rarely moves two axes at once;
        // if it does, the stronger wins). Then BANK it toward STEP_THRESHOLD:
        //  - same direction as we're already banking -> keep adding (micro-rolls stack up,
        //    with no time limit, until they total one full step)
        //  - a different direction -> that's a direction change, so discard the old bank
        //    and start fresh on the new one (prevents a half-step one way leaking the other)
        // A poll with no motion leaves the bank untouched (so slow nudges still accumulate).
        let mut m = 0usize;
        let mut best = 0u16;
        for i in 0..4 {
            if d[i] > best {
                best = d[i];
                m = i;
            }
        }
        if best > 0 {
            if self.active != Some(m) {
                self.active = Some(m);
                self.accum = 0;
            }
            self.accum = self.accum.saturating_add(best);
            while self.accum >= STEP_THRESHOLD && self.queue[m] < MAX_QUEUE {
                self.queue[m] += 1;
                self.accum -= STEP_THRESHOLD;
            }
        }

        // Click logic. Two clicks inside one poll, or a 2nd click within the window,
        // is a double -> ESC/back. A first lone click arms the window; if nothing
        // follows, the top-of-fn timeout above turns it into ENTER.
        if clicks >= 2 {
            self.click_wait = None;
            return Some(key(K_HOME));
        }
        if clicks == 1 {
            match self.click_wait {
                Some(t) if t.elapsed() < Duration::from_millis(DOUBLE_MS) => {
                    self.click_wait = None;
                    return Some(key(K_HOME));
                }
                _ => self.click_wait = Some(Instant::now()),
            }
        }

        self.pop_motion()
    }
}

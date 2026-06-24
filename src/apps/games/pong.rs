//! Pong — you on the left paddle, a beatable AI on the right, a ball ricocheting
//! between them in the band under the topbar. `tick` advances the ball on a fast
//! ~18 ms cadence for smooth motion; up/down nudge your paddle, ENTER serves when
//! the ball is parked between points. Walls bounce the ball; paddle hits kick its
//! vertical angle by where it lands, so you can aim. First to 5 takes the match,
//! then any key starts over.
//!
//! Self-contained: a small LCG (advanced on every tick and key) picks the serve
//! direction — no RNG crate, no float trig (the ball carries an explicit vx/vy).

use embedded_graphics::{mono_font::ascii::FONT_10X20, pixelcolor::Rgb565, prelude::*};

use esp_hal::time::{Duration, Instant};

use crate::{i18n, theme};
use crate::i18n::pong;

// ---- playfield: the band between the topbar divider and the hint line ----
const FIELD_TOP: f32 = 20.0;
const FIELD_BOT: f32 = theme::HINT_Y as f32; // 123
const FIELD_H: f32 = FIELD_BOT - FIELD_TOP; // 103
const FIELD_W: f32 = theme::W as f32; // 240

// ---- paddles ----
const PADDLE_W: f32 = 4.0;
const PADDLE_H: f32 = 22.0;
const PLAYER_X: f32 = 4.0; // left gutter
const AI_X: f32 = FIELD_W - 4.0 - PADDLE_W; // right gutter
const PADDLE_SPEED: f32 = 5.5; // px per key press (hold-to-repeat gives smooth continuous motion)
const AI_SPEED: f32 = 2.2; // px per step cap (kept slow -> beatable)

// ---- ball ----
const BALL: f32 = 4.0; // ball is a BALL x BALL square
const BALL_SPEED_X: f32 = 2.6; // horizontal speed (constant magnitude)
const BALL_VY_MAX: f32 = 3.4; // clamp the vertical component
const BALL_VY_KICK: f32 = 3.0; // how hard the paddle face deflects vertically

// ---- match ----
const WIN_SCORE: u8 = 5;
const STEP_MS: u64 = 18; // ball cadence
const AUTO_SERVE_MS: u64 = 900; // pause after a point, then auto-serve

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle, // ball parked, waiting for ENTER / auto-serve
    Play, // ball in flight
    Over, // someone reached WIN_SCORE
}

pub struct Pong {
    // positions in field-local-ish coords (paddles share the field rect; the ball
    // y is absolute screen y so collisions read directly against FIELD_TOP/BOT).
    player_y: f32, // top of the player's paddle (screen y)
    ai_y: f32,     // top of the AI's paddle (screen y)
    ball_x: f32,   // ball top-left (screen x)
    ball_y: f32,   // ball top-left (screen y)
    vx: f32,
    vy: f32,
    p_score: u8,
    ai_score: u8,
    phase: Phase,
    // last-drawn rects so tick() can erase exactly what moved (no full clears).
    last_player_y: f32,
    last_ai_y: f32,
    last_ball_x: f32,
    last_ball_y: f32,
    serve_dir: f32, // +1 toward AI, -1 toward player (who lost serves next)
    rng: u32,
    last_step: Instant,
    idle_since: Instant,
}

impl Pong {
    pub fn new() -> Self {
        let mut p = Pong {
            player_y: 0.0,
            ai_y: 0.0,
            ball_x: 0.0,
            ball_y: 0.0,
            vx: 0.0,
            vy: 0.0,
            p_score: 0,
            ai_score: 0,
            phase: Phase::Idle,
            last_player_y: 0.0,
            last_ai_y: 0.0,
            last_ball_x: 0.0,
            last_ball_y: 0.0,
            serve_dir: 1.0,
            rng: 0x2468_ACE1, // fixed seed; advanced on every tick + key
            last_step: Instant::now(),
            idle_since: Instant::now(),
        };
        p.reset();
        p
    }

    /// Fresh match: scores zero, paddles centred, ball parked for the first serve.
    fn reset(&mut self) {
        let mid = FIELD_TOP + (FIELD_H - PADDLE_H) / 2.0;
        self.player_y = mid;
        self.ai_y = mid;
        self.p_score = 0;
        self.ai_score = 0;
        self.phase = Phase::Idle;
        // First serve direction from the LCG (toward AI or player).
        self.serve_dir = if self.rand() & 1 == 0 { 1.0 } else { -1.0 };
        self.park_ball();
        self.sync_last();
        let now = Instant::now();
        self.last_step = now;
        self.idle_since = now;
    }

    /// Centre the ball with zero velocity, ready to be served.
    fn park_ball(&mut self) {
        self.ball_x = (FIELD_W - BALL) / 2.0;
        self.ball_y = FIELD_TOP + (FIELD_H - BALL) / 2.0;
        self.vx = 0.0;
        self.vy = 0.0;
    }

    /// Launch the parked ball in `serve_dir` with a small RNG vertical lean.
    fn serve(&mut self) {
        // vy in roughly -1.5 .. +1.5, derived from the LCG.
        let r = (self.rand() >> 8) as i32 % 300; // 0..299
        let vy = (r as f32 - 150.0) / 100.0; // -1.5 .. 1.49
        self.vx = BALL_SPEED_X * self.serve_dir;
        self.vy = vy;
        self.phase = Phase::Play;
    }

    /// Snapshot current sprite positions as the "last drawn" set.
    fn sync_last(&mut self) {
        self.last_player_y = self.player_y;
        self.last_ai_y = self.ai_y;
        self.last_ball_x = self.ball_x;
        self.last_ball_y = self.ball_y;
    }

    /// Advance the LCG and return the new state (state = state*1664525 + 1013904223).
    fn rand(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.rng
    }

    fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
        if v < lo {
            lo
        } else if v > hi {
            hi
        } else {
            v
        }
    }

    // ---- drawing ----

    fn draw_paddle<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: f32, y: f32, col: Rgb565) {
        theme::fill(d, x as i32, y as i32, PADDLE_W as u32, PADDLE_H as u32, col);
    }

    fn erase_paddle<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: f32, y: f32) {
        theme::fill(d, x as i32, y as i32, PADDLE_W as u32, PADDLE_H as u32, theme::BG);
    }

    fn draw_ball<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: f32, y: f32, col: Rgb565) {
        theme::fill(d, x as i32, y as i32, BALL as u32, BALL as u32, col);
    }

    fn erase_ball<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: f32, y: f32) {
        theme::fill(d, x as i32, y as i32, BALL as u32, BALL as u32, theme::BG);
    }

    /// Dotted centre net, drawn once on a full board paint.
    fn draw_net<D: DrawTarget<Color = Rgb565>>(d: &mut D) {
        let cx = (theme::W / 2) - 1;
        let mut y = FIELD_TOP as i32 + 2;
        while y < FIELD_BOT as i32 - 4 {
            theme::fill(d, cx, y, 2, 4, theme::FAINT);
            y += 8;
        }
    }

    /// Scores in the top-bar band: player on the left of centre, AI to its right.
    /// (The far-right ~52px belongs to the battery indicator.)
    fn draw_scores<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        // erase the score area between the title and the battery
        theme::fill(d, 86, 3, 70, 13, theme::BG);
        let mut pb = [0u8; 4];
        let mut ab = [0u8; 4];
        let ps = fmt_u8(self.p_score, &mut pb);
        let as_ = fmt_u8(self.ai_score, &mut ab);
        // player score, then a separator, then AI score, around screen centre
        let cx = theme::W / 2;
        theme::text_right(d, ps, cx - 6, 4, theme::TITLE_FONT, theme::accent());
        theme::text_center(d, "-", cx, 4 + 6, theme::BODY_FONT, theme::MUTED);
        theme::text(d, as_, cx + 6, 4, theme::TITLE_FONT, theme::FG);
    }

    /// Full board paint: clear the playfield, then net, paddles, ball, scores.
    fn draw_board<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        theme::fill(d, 0, FIELD_TOP as i32, FIELD_W as u32, FIELD_H as u32, theme::BG);
        Self::draw_net(d);
        Self::draw_paddle(d, PLAYER_X, self.player_y, theme::accent());
        Self::draw_paddle(d, AI_X, self.ai_y, theme::FG);
        Self::draw_ball(d, self.ball_x, self.ball_y, theme::FG);
        self.draw_scores(d);
    }

    /// Match-over overlay: winner + restart prompt, centred on the board.
    fn draw_over<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D) {
        let player_won = self.p_score > self.ai_score;
        let cy = FIELD_TOP as i32 + FIELD_H as i32 / 2;
        theme::card(d, 36, cy - 30, (theme::W - 72) as u32, 56, Some(theme::accent()));
        let title = if player_won {
            i18n::t(pong::YOU_WIN)
        } else {
            i18n::t(pong::AI_WINS)
        };
        theme::text_center(d, title, theme::W / 2, cy - 12, &FONT_10X20, theme::FG);
        let mut buf = [0u8; 12];
        let line = fmt_score_line(self.p_score, self.ai_score, &mut buf);
        theme::text_center(d, line, theme::W / 2, cy + 8, theme::BODY_FONT, theme::accent());
        theme::hint(d, i18n::t(pong::PLAY_AGAIN));
    }

    // ---- public interface (called by main.rs) ----

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.reset();
        theme::clear(d);
        theme::topbar(d, i18n::t(pong::PONG));
        self.draw_board(d);
        theme::hint(d, i18n::t(pong::MOVE_SERVE));
    }

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) {
        // Stir the RNG on every key so serves aren't perfectly deterministic.
        let _ = self.rand();

        if self.phase == Phase::Over {
            // Any key restarts a fresh match.
            self.reset();
            theme::clear(d);
            theme::topbar(d, i18n::t(pong::PONG));
            self.draw_board(d);
            theme::hint(d, i18n::t(pong::MOVE_SERVE));
            return;
        }

        let lo = FIELD_TOP;
        let hi = FIELD_BOT - PADDLE_H;
        match rc {
            crate::K_UP => {
                self.player_y = Self::clampf(self.player_y - PADDLE_SPEED, lo, hi);
                self.redraw_player(d);
            }
            crate::K_DOWN => {
                self.player_y = Self::clampf(self.player_y + PADDLE_SPEED, lo, hi);
                self.redraw_player(d);
            }
            crate::K_ENTER => {
                if self.phase == Phase::Idle {
                    self.serve();
                }
            }
            _ => {}
        }
    }

    /// Redraw just the player paddle, erasing the slice it left behind.
    fn redraw_player<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        if (self.player_y - self.last_player_y).abs() >= 1.0 {
            Self::erase_paddle(d, PLAYER_X, self.last_player_y);
            Self::draw_paddle(d, PLAYER_X, self.player_y, theme::accent());
            self.last_player_y = self.player_y;
        }
    }

    pub fn tick<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) -> bool {
        // Keep the LCG churning even when idle so timing seeds the randomness.
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);

        if self.phase == Phase::Over {
            return false; // frozen until a key restarts
        }
        if self.last_step.elapsed() < Duration::from_millis(STEP_MS) {
            return false; // not time to step yet — leave the framebuffer alone
        }
        self.last_step = Instant::now();

        if self.phase == Phase::Idle {
            // Auto-serve after a short pause so the match never stalls.
            if self.idle_since.elapsed() >= Duration::from_millis(AUTO_SERVE_MS) {
                self.serve();
            } else {
                return false; // parked: nothing visibly changed this step
            }
        }

        let mut changed = false;

        // ---- AI tracks the ball's centre with a capped speed (beatable) ----
        let ai_center = self.ai_y + PADDLE_H / 2.0;
        let ball_center = self.ball_y + BALL / 2.0;
        let diff = ball_center - ai_center;
        let mv = Self::clampf(diff, -AI_SPEED, AI_SPEED);
        self.ai_y = Self::clampf(self.ai_y + mv, FIELD_TOP, FIELD_BOT - PADDLE_H);

        // ---- advance the ball ----
        self.ball_x += self.vx;
        self.ball_y += self.vy;

        // top / bottom walls
        if self.ball_y <= FIELD_TOP {
            self.ball_y = FIELD_TOP;
            self.vy = self.vy.abs();
        } else if self.ball_y + BALL >= FIELD_BOT {
            self.ball_y = FIELD_BOT - BALL;
            self.vy = -self.vy.abs();
        }

        // player paddle (left): ball moving left, overlapping the paddle column
        if self.vx < 0.0
            && self.ball_x <= PLAYER_X + PADDLE_W
            && self.ball_x >= PLAYER_X - PADDLE_W
            && self.ball_y + BALL >= self.player_y
            && self.ball_y <= self.player_y + PADDLE_H
        {
            self.ball_x = PLAYER_X + PADDLE_W;
            self.vx = self.vx.abs();
            self.deflect(self.player_y);
        }

        // AI paddle (right): ball moving right, overlapping the paddle column
        if self.vx > 0.0
            && self.ball_x + BALL >= AI_X
            && self.ball_x + BALL <= AI_X + PADDLE_W * 2.0
            && self.ball_y + BALL >= self.ai_y
            && self.ball_y <= self.ai_y + PADDLE_H
        {
            self.ball_x = AI_X - BALL;
            self.vx = -self.vx.abs();
            self.deflect(self.ai_y);
        }

        // ---- scoring: ball escaped past a paddle ----
        if self.ball_x + BALL < 0.0 {
            // past the player -> AI scores
            self.ai_score = self.ai_score.saturating_add(1);
            if self.ai_score >= WIN_SCORE {
                self.finish(d);
                return true;
            }
            self.serve_dir = -1.0; // serve toward the player who just lost
            self.point_reset(d);
            return true;
        } else if self.ball_x > FIELD_W {
            // past the AI -> player scores
            self.p_score = self.p_score.saturating_add(1);
            if self.p_score >= WIN_SCORE {
                self.finish(d);
                return true;
            }
            self.serve_dir = 1.0; // serve toward the AI who just lost
            self.point_reset(d);
            return true;
        }

        // ---- repaint the moved sprites (erase old slice, draw new) ----
        // AI paddle
        if (self.ai_y - self.last_ai_y).abs() >= 1.0 {
            Self::erase_paddle(d, AI_X, self.last_ai_y);
            Self::draw_paddle(d, AI_X, self.ai_y, theme::FG);
            self.last_ai_y = self.ai_y;
            changed = true;
        }
        // ball: erase its old square, redraw the net cell it may have covered, draw new
        if self.ball_x as i32 != self.last_ball_x as i32 || self.ball_y as i32 != self.last_ball_y as i32 {
            Self::erase_ball(d, self.last_ball_x, self.last_ball_y);
            self.repair_net(d, self.last_ball_x, self.last_ball_y);
            Self::draw_ball(d, self.ball_x, self.ball_y, theme::FG);
            self.last_ball_x = self.ball_x;
            self.last_ball_y = self.ball_y;
            changed = true;
        }

        changed
    }

    /// Kick the vertical velocity by where the ball met the paddle face
    /// (top of paddle -> upward, bottom -> downward), then clamp.
    fn deflect(&mut self, paddle_y: f32) {
        let ball_center = self.ball_y + BALL / 2.0;
        let paddle_center = paddle_y + PADDLE_H / 2.0;
        // -1 .. +1 across the paddle face
        let rel = (ball_center - paddle_center) / (PADDLE_H / 2.0);
        self.vy = Self::clampf(rel * BALL_VY_KICK, -BALL_VY_MAX, BALL_VY_MAX);
    }

    /// A point was scored (no winner yet): park the ball, repaint the board so the
    /// in-flight trail and old paddle positions are cleared, and start the pause.
    fn point_reset<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.park_ball();
        self.phase = Phase::Idle;
        self.idle_since = Instant::now();
        self.draw_board(d);
        self.sync_last();
    }

    /// Match reached WIN_SCORE: lock to Over and paint the result overlay.
    fn finish<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.phase = Phase::Over;
        self.draw_board(d); // ensure final score shows behind the card
        self.draw_over(d);
    }

    /// If the ball's old footprint overlapped the centre net, restore that dash so
    /// the net doesn't get eaten as the ball crosses it.
    fn repair_net<D: DrawTarget<Color = Rgb565>>(&self, d: &mut D, x: f32, _y: f32) {
        let cx = (theme::W / 2) - 1;
        let xi = x as i32;
        // net occupies the 2px column [cx, cx+1]; redraw if the ball touched it
        if xi + BALL as i32 > cx && xi < cx + 2 {
            let mut ny = FIELD_TOP as i32 + 2;
            while ny < FIELD_BOT as i32 - 4 {
                theme::fill(d, cx, ny, 2, 4, theme::FAINT);
                ny += 8;
            }
        }
    }
}

/// `u8` (0..=255) -> decimal, into `buf`. Returns the slice as &str.
fn fmt_u8(v: u8, buf: &mut [u8; 4]) -> &str {
    let mut tmp = [0u8; 3];
    let mut n = v;
    let mut i = 0;
    if n == 0 {
        tmp[0] = b'0';
        i = 1;
    } else {
        while n > 0 && i < tmp.len() {
            tmp[i] = b'0' + (n % 10);
            n /= 10;
            i += 1;
        }
    }
    let mut j = 0;
    while i > 0 {
        i -= 1;
        buf[j] = tmp[i];
        j += 1;
    }
    core::str::from_utf8(&buf[..j]).unwrap_or("0")
}

/// "P N - N" on the game-over card (ASCII only).
fn fmt_score_line(p: u8, ai: u8, buf: &mut [u8; 12]) -> &str {
    let mut j = 0;
    let mut push = |b: u8, j: &mut usize| {
        if *j < buf.len() {
            buf[*j] = b;
            *j += 1;
        }
    };
    let mut pb = [0u8; 4];
    let ps = fmt_u8(p, &mut pb);
    for &b in ps.as_bytes() {
        push(b, &mut j);
    }
    push(b' ', &mut j);
    push(b'-', &mut j);
    push(b' ', &mut j);
    let mut ab = [0u8; 4];
    let as_ = fmt_u8(ai, &mut ab);
    for &b in as_.as_bytes() {
        push(b, &mut j);
    }
    core::str::from_utf8(&buf[..j]).unwrap_or("0 - 0")
}

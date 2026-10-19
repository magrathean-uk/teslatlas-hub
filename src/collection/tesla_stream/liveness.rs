// SPDX-License-Identifier: AGPL-3.0-only

//! The timers of one stream session (C4 part P3): the Ping schedule, the
//! dead-socket deadline (R29), the data-silence deadline (R30) and the host-sleep
//! check (R58). Pure state over monotonic instants and wall milliseconds that
//! the caller reads, so every rule is tested with exact values; the read loop
//! in `session.rs` waits for the deadlines this type reports.

use std::time::Duration;

use tokio::time::Instant;

use crate::collector::K_CLOCK_DIVERGENCE;

use super::policy::ping_interval;

/// Ping intervals without any inbound frame after which the socket is dead
/// (R29): 45 s at the default interval, 15 s to 45 s overall. RFC 6455 section
/// 5.5.2 obliges the peer to answer a Ping, so liveness does not depend on how
/// often the car sends data; three intervals tolerate two lost or late pongs,
/// including TCP retransmission at an initial 1 s timeout that doubles (RFC
/// 6298: 1 + 2 + 4 s); detection within a minute lets C3 fall back to polling
/// long before C1 closes a drive for lack of evidence (`T_gap`).
const DEAD_SOCKET_INTERVALS: u32 = 3;

/// How long a session must stay open after its first accepted frame before the
/// transport back-off counter restarts (R37): 60 s, four default ping
/// intervals, of a working session. A path that fails right after its first
/// frame therefore keeps backing off instead of retrying at the shortest delay,
/// while a session that has run for a minute shows the path works.
const STABLE_AFTER_FIRST_FRAME: Duration = Duration::from_secs(60);

/// The clocks of one session. Liveness starts when the socket opens (R29): the
/// Pings and the dead-socket deadline run through Admitting (a lock wait of up
/// to 90 s) and Subscribing, because a server whose idle limit is shorter than
/// that wait would otherwise drop a socket that nothing answered or read. The
/// data-silence clock starts later, at the subscribe write (R30), because no
/// data can arrive before the subscription exists.
pub(super) struct SessionClocks {
    /// When the last inbound WebSocket frame of any kind arrived (R29).
    last_inbound: Instant,
    /// The subscribe write (until [`Self::on_subscribed`] runs, the socket's
    /// opening), or when the last `data:update` that was not rejected arrived
    /// (R30).
    last_data: Instant,
    ping_interval: Duration,
    next_ping: Instant,
    /// After a host sleep (R58): the shortened dead-socket deadline, one ping
    /// interval after the Ping sent at the detecting tick; any inbound frame
    /// restores the normal rule.
    sleep_deadline: Option<Instant>,
    /// When the session counts as stable (R37): [`STABLE_AFTER_FIRST_FRAME`]
    /// after its first accepted frame, until the reset has been applied.
    stable_at: Option<Instant>,
    wall_at_last_check: i64,
    mono_at_last_check: Instant,
}

impl SessionClocks {
    /// Starts the clocks when the socket opens, at `now` (mono) and `wall_ms`
    /// (wall), with the default ping interval (R29).
    pub(super) fn start(now: Instant, wall_ms: i64) -> Self {
        let interval = ping_interval(None);
        Self {
            last_inbound: now,
            last_data: now,
            ping_interval: interval,
            next_ping: now + interval,
            sleep_deadline: None,
            stable_at: None,
            wall_at_last_check: wall_ms,
            mono_at_last_check: now,
        }
    }

    /// The subscribe frame was written at `now`: the data-silence clock counts
    /// from here (R30).
    pub(super) fn on_subscribed(&mut self, now: Instant) {
        self.last_data = now;
    }

    /// An inbound WebSocket frame of any kind arrived (R29): the liveness clock
    /// is reset and a host-sleep deadline is lifted (R58).
    pub(super) fn on_inbound(&mut self, now: Instant) {
        self.last_inbound = now;
        self.sleep_deadline = None;
    }

    /// A `data:update` for the session's tag that was not rejected arrived (R30):
    /// accepted, duplicate, or with invalid fields.
    pub(super) fn on_data(&mut self, now: Instant) {
        self.last_data = now;
    }

    /// The session's first accepted frame (a duplicate included, R26) arrived
    /// at `now`: the stable-session timer starts (R37). Called once per
    /// session.
    pub(super) fn on_first_accepted(&mut self, now: Instant) {
        self.stable_at = Some(now + STABLE_AFTER_FIRST_FRAME);
    }

    /// When the transport counter is to be reset (R37): 60 s after the first
    /// accepted frame while the session is open. `None` before that frame and
    /// after the reset.
    pub(super) fn stable_at(&self) -> Option<Instant> {
        self.stable_at
    }

    /// The reset of R37 was applied: the timer does not fire again in this
    /// session.
    pub(super) fn on_stable_applied(&mut self) {
        self.stable_at = None;
    }

    /// A successful hello gave a valid `connection_timeout` (R27, R29): the next
    /// Ping is sent one new interval after the hello, and later Pings follow at
    /// the new interval, so the server's idle limit applies at once.
    pub(super) fn adopt_ping_interval(&mut self, now: Instant, interval: Duration) {
        self.ping_interval = interval;
        self.next_ping = now + interval;
    }

    /// When the next Ping is due.
    pub(super) fn next_ping(&self) -> Instant {
        self.next_ping
    }

    /// The current ping interval.
    #[cfg(test)]
    pub(super) fn ping_interval(&self) -> Duration {
        self.ping_interval
    }

    /// When the socket counts as dead (R29): three current ping intervals after
    /// the last inbound frame, or, after a host sleep (R58), one interval after
    /// the Ping sent at the detecting tick, whichever is earlier.
    pub(super) fn dead_at(&self) -> Instant {
        let normal = self.last_inbound + self.ping_interval * DEAD_SOCKET_INTERVALS;
        match self.sleep_deadline {
            Some(deadline) => deadline.min(normal),
            None => normal,
        }
    }

    /// When the session is silent for `limit` (R30), counted from the subscribe
    /// write or the last data frame that was not rejected. The host-sleep rule
    /// does not change this clock.
    pub(super) fn silent_at(&self, limit: Duration) -> Instant {
        self.last_data + limit
    }

    /// A ping tick (R29): schedules the next one and runs the host-sleep check
    /// of R58, which returns `true` when the host has slept. The check is
    /// `drift = (wall_now - wall_prev) - (mono_now - mono_prev) > K_CLOCK_DIVERGENCE`
    /// since the previous tick (the session start for the first). The monotonic
    /// clock does not advance while the host sleeps (C3 R2), so the drift is
    /// the time slept; the same constant as C3 R2 means the collector and the
    /// stream agree on what counts as host sleep, and 60 s is above timer and
    /// scheduling jitter. The caller sends the tick's Ping at once either way;
    /// on a sleep the dead-socket deadline becomes one ping interval from it.
    pub(super) fn on_ping_tick(&mut self, now: Instant, wall_ms: i64) -> bool {
        let wall_delta = wall_ms.saturating_sub(self.wall_at_last_check);
        let mono_delta = i64::try_from(
            now.saturating_duration_since(self.mono_at_last_check)
                .as_millis(),
        )
        .unwrap_or(i64::MAX);
        let drift = wall_delta.saturating_sub(mono_delta);
        self.wall_at_last_check = wall_ms;
        self.mono_at_last_check = now;
        self.next_ping = now + self.ping_interval;
        let slept = i128::from(drift) > K_CLOCK_DIVERGENCE.as_millis() as i128;
        if slept {
            let shortened = now + self.ping_interval;
            self.sleep_deadline = Some(match self.sleep_deadline {
                Some(existing) => existing.min(shortened),
                None => shortened,
            });
        }
        slept
    }
}

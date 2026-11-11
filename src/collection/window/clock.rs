// SPDX-License-Identifier: AGPL-3.0-only

//! Clocks, the hard-expiry guard and the network time probe.
//!
//! Every time source is injected so tests can step wall time, monotonic time
//! and the boot time independently.

use std::{
    net::{ToSocketAddrs, UdpSocket},
    process::Command,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use super::{CLOCK_FLOOR_EPOCH_SECONDS, DRIFT_TOLERANCE_SECONDS, refusal::Refusal};

pub trait WindowClock: Send + Sync + std::fmt::Debug {
    /// Wall time, whole seconds since the Unix epoch.
    fn wall(&self) -> i64;
    /// Monotonic seconds. Within one process it never runs back.
    fn mono(&self) -> u64;
    /// Whether `mono` is boot-scoped and counts sleep: one origin for the
    /// whole boot, so a reading taken before a process restart stays
    /// comparable after it, and time asleep is counted. Only then can an Open
    /// window keep its original monotonic deadline across a restart; when it
    /// is false, an Open window closes `clock_invalid` at restart (fail
    /// closed).
    fn mono_boot_scoped(&self) -> bool;
    /// The boot time as Unix seconds; it differs after a reboot.
    fn boot_time(&self) -> Result<i64, Refusal>;
}

/// The system clock. Its monotonic reading is `std::time::Instant`, which is
/// process-scoped and, on macOS, does not count sleep: within a process the
/// wall deadline still bounds a window across sleep, but an Open window cannot
/// survive a restart on it and closes `clock_invalid`. A boot-scoped,
/// sleep-counting source (Darwin `CLOCK_MONOTONIC`, Linux `CLOCK_BOOTTIME`)
/// needs a safe binding the crate does not enable yet.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowClock for SystemClock {
    fn wall(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_secs() as i64,
            Err(before) => -(before.duration().as_secs() as i64),
        }
    }

    fn mono(&self) -> u64 {
        self.origin.elapsed().as_secs()
    }

    fn mono_boot_scoped(&self) -> bool {
        false
    }

    fn boot_time(&self) -> Result<i64, Refusal> {
        let output = Command::new("/usr/sbin/sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .map_err(|_| Refusal::ClockInvalid)?;
        parse_boot_time(&String::from_utf8_lossy(&output.stdout)).ok_or(Refusal::ClockInvalid)
    }
}

/// Reads `{ sec = 1731000000, usec = 0 } ...`.
pub(crate) fn parse_boot_time(text: &str) -> Option<i64> {
    let after = text.split("sec =").nth(1)?;
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// A clock the tests step by hand. Its monotonic reading models a
/// boot-scoped, sleep-counting clock: a process restart does not move it.
#[derive(Debug)]
pub struct ManualClock {
    inner: Mutex<ManualClockState>,
}

#[derive(Debug, Clone, Copy)]
struct ManualClockState {
    wall: i64,
    mono: u64,
    boot: i64,
    boot_scoped: bool,
}

impl ManualClock {
    pub fn new(wall: i64) -> Self {
        Self {
            inner: Mutex::new(ManualClockState {
                wall,
                mono: 1_000,
                boot: wall - 10_000,
                boot_scoped: true,
            }),
        }
    }

    /// Time passes: wall and monotonic both advance (a clock that counts sleep).
    pub fn advance(&self, seconds: u64) {
        let mut state = self.inner.lock().expect("clock lock");
        state.wall += seconds as i64;
        state.mono += seconds;
    }

    /// Only the wall clock moves: a step or rollback by an operator or NTP.
    pub fn shift_wall(&self, seconds: i64) {
        self.inner.lock().expect("clock lock").wall += seconds;
    }

    pub fn set_boot(&self, boot: i64) {
        self.inner.lock().expect("clock lock").boot = boot;
    }

    /// Sets the monotonic reading, to model a source that runs back.
    pub fn set_mono(&self, mono: u64) {
        self.inner.lock().expect("clock lock").mono = mono;
    }

    /// Models a process-scoped monotonic source (its origin is per process).
    pub fn set_boot_scoped(&self, boot_scoped: bool) {
        self.inner.lock().expect("clock lock").boot_scoped = boot_scoped;
    }
}

impl WindowClock for ManualClock {
    fn wall(&self) -> i64 {
        self.inner.lock().expect("clock lock").wall
    }

    fn mono(&self) -> u64 {
        self.inner.lock().expect("clock lock").mono
    }

    fn mono_boot_scoped(&self) -> bool {
        self.inner.lock().expect("clock lock").boot_scoped
    }

    fn boot_time(&self) -> Result<i64, Refusal> {
        Ok(self.inner.lock().expect("clock lock").boot)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockVerdict {
    Ok,
    Expired,
    Invalid,
}

/// The hard-expiry guard (design 2.6). The deadline is the wall `expires_at`
/// and, equally, the open reading plus the authorised remainder on the
/// monotonic clock, so stepping the wall clock back cannot extend a window.
/// The monotonic deadline is fixed when the window opens and persisted; a
/// restart restores it rather than computing a new one. The watermark is the
/// highest wall time seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockGuard {
    pub anchor_wall: i64,
    pub anchor_mono: u64,
    pub deadline_wall: i64,
    pub deadline_mono: u64,
    pub watermark: i64,
}

/// The monotonic deadline for a window anchored at (`wall`, `mono`).
pub fn deadline_mono_for(wall: i64, mono: u64, deadline_wall: i64) -> u64 {
    mono.saturating_add((deadline_wall - wall).max(0) as u64)
}

impl ClockGuard {
    /// Anchors the guard at `now`: the authorised remainder is what is left
    /// of the grant at this moment.
    pub fn anchored(wall: i64, mono: u64, deadline_wall: i64, watermark: i64) -> Self {
        Self {
            anchor_wall: wall,
            anchor_mono: mono,
            deadline_wall,
            deadline_mono: deadline_mono_for(wall, mono, deadline_wall),
            watermark: watermark.max(wall),
        }
    }

    pub fn deadline_mono(&self) -> u64 {
        self.deadline_mono
    }

    /// Checks one reading and advances the watermark when it passes.
    pub fn check(&mut self, wall: i64, mono: u64) -> ClockVerdict {
        if wall < CLOCK_FLOOR_EPOCH_SECONDS || wall + DRIFT_TOLERANCE_SECONDS < self.watermark {
            return ClockVerdict::Invalid;
        }
        let wall_elapsed = wall - self.anchor_wall;
        let mono_elapsed = mono.saturating_sub(self.anchor_mono) as i64;
        if wall_elapsed + DRIFT_TOLERANCE_SECONDS < mono_elapsed {
            return ClockVerdict::Invalid;
        }
        self.watermark = self.watermark.max(wall);
        if wall >= self.deadline_wall || mono >= self.deadline_mono {
            ClockVerdict::Expired
        } else {
            ClockVerdict::Ok
        }
    }
}

/// Measures the clock offset (seconds, positive when this host is behind)
/// against a network time server. No Tesla endpoint is involved.
pub trait OffsetProbe: Send + Sync {
    fn offset_seconds(&self) -> Result<f64, Refusal>;
}

/// An injected offset or failure.
#[derive(Debug)]
pub struct ManualProbe {
    result: Mutex<Result<f64, Refusal>>,
    calls: Mutex<u32>,
}

impl ManualProbe {
    pub fn new(offset: f64) -> Self {
        Self {
            result: Mutex::new(Ok(offset)),
            calls: Mutex::new(0),
        }
    }

    pub fn set(&self, result: Result<f64, Refusal>) {
        *self.result.lock().expect("probe lock") = result;
    }

    pub fn calls(&self) -> u32 {
        *self.calls.lock().expect("probe lock")
    }
}

impl OffsetProbe for ManualProbe {
    fn offset_seconds(&self) -> Result<f64, Refusal> {
        *self.calls.lock().expect("probe lock") += 1;
        *self.result.lock().expect("probe lock")
    }
}

const NTP_UNIX_DELTA: f64 = 2_208_988_800.0;
const NTP_PACKET: usize = 48;

fn ntp_timestamp(seconds: f64) -> [u8; 8] {
    let ntp = seconds + NTP_UNIX_DELTA;
    let whole = ntp.floor();
    let fraction = ((ntp - whole) * 4_294_967_296.0) as u32;
    let mut bytes = [0u8; 8];
    bytes[..4].copy_from_slice(&(whole as u32).to_be_bytes());
    bytes[4..].copy_from_slice(&fraction.to_be_bytes());
    bytes
}

fn read_ntp_timestamp(bytes: &[u8]) -> f64 {
    let whole = u32::from_be_bytes(bytes[..4].try_into().expect("four bytes")) as f64;
    let fraction = u32::from_be_bytes(bytes[4..8].try_into().expect("four bytes")) as f64;
    whole + fraction / 4_294_967_296.0 - NTP_UNIX_DELTA
}

pub(crate) fn sntp_request(sent: f64) -> [u8; NTP_PACKET] {
    let mut packet = [0u8; NTP_PACKET];
    packet[0] = 0x23; // version 4, client mode
    packet[40..48].copy_from_slice(&ntp_timestamp(sent));
    packet
}

/// Computes the offset from a server reply: `sent` and `received` are local
/// wall seconds (t0 and t3). The reply must echo our transmit timestamp.
pub(crate) fn sntp_offset(sent: f64, reply: &[u8], received: f64) -> Result<f64, Refusal> {
    if reply.len() < NTP_PACKET {
        return Err(Refusal::ClockOffset);
    }
    let leap = reply[0] >> 6;
    let mode = reply[0] & 7;
    let stratum = reply[1];
    if leap == 3 || mode != 4 || stratum == 0 || reply[24..32] != sntp_request(sent)[40..48] {
        return Err(Refusal::ClockOffset);
    }
    let received_by_server = read_ntp_timestamp(&reply[32..40]);
    let transmitted = read_ntp_timestamp(&reply[40..48]);
    if reply[40..48].iter().all(|byte| *byte == 0) {
        return Err(Refusal::ClockOffset);
    }
    Ok(((received_by_server - sent) + (transmitted - received)) / 2.0)
}

/// SNTP over UDP. Blocking: call from a blocking task.
#[derive(Debug, Clone)]
pub struct SntpProbe {
    server: String,
    timeout: Duration,
}

impl SntpProbe {
    pub fn new(server: &str, timeout: Duration) -> Self {
        Self {
            server: server.to_owned(),
            timeout,
        }
    }

    fn now() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or(0.0)
    }
}

impl OffsetProbe for SntpProbe {
    fn offset_seconds(&self) -> Result<f64, Refusal> {
        let address = self
            .server
            .to_socket_addrs()
            .map_err(|_| Refusal::ClockOffset)?
            .find(|address| address.is_ipv4())
            .ok_or(Refusal::ClockOffset)?;
        let socket = UdpSocket::bind("0.0.0.0:0").map_err(|_| Refusal::ClockOffset)?;
        socket
            .set_read_timeout(Some(self.timeout))
            .and_then(|()| socket.set_write_timeout(Some(self.timeout)))
            .map_err(|_| Refusal::ClockOffset)?;
        socket.connect(address).map_err(|_| Refusal::ClockOffset)?;
        let sent = Self::now();
        socket
            .send(&sntp_request(sent))
            .map_err(|_| Refusal::ClockOffset)?;
        let mut reply = [0u8; 128];
        let length = socket.recv(&mut reply).map_err(|_| Refusal::ClockOffset)?;
        sntp_offset(sent, &reply[..length], Self::now())
    }
}

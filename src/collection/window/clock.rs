// SPDX-License-Identifier: AGPL-3.0-only

//! Clocks, the hard-expiry guard and the network time probe.
//!
//! Every time source is injected so tests can step wall time, monotonic time
//! and the boot time independently.

use std::{
    net::{ToSocketAddrs, UdpSocket},
    process::Command,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(not(target_vendor = "apple"))]
use std::time::Instant;

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

/// The system clock.
///
/// On Darwin its monotonic reading is `CLOCK_MONOTONIC`, which has one origin
/// for the whole boot and keeps counting while the machine sleeps, so a
/// reading persisted by one service process stays comparable in the next and
/// an Open window keeps its original monotonic deadline across a restart. A
/// reboot starts a new origin; the boot identity (`kern.boottime`) changes
/// with it and closes the window. Elsewhere the reading falls back to the
/// process-scoped `std::time::Instant`, reported as not boot-scoped, so an
/// Open window fails closed at restart there.
#[derive(Debug)]
pub struct SystemClock {
    #[cfg(not(target_vendor = "apple"))]
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            #[cfg(not(target_vendor = "apple"))]
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

    #[cfg(target_vendor = "apple")]
    fn mono(&self) -> u64 {
        let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        // A negative reading cannot occur; mapping it to zero puts it behind
        // any anchor, which fails closed.
        u64::try_from(now.tv_sec).unwrap_or(0)
    }

    #[cfg(not(target_vendor = "apple"))]
    fn mono(&self) -> u64 {
        self.origin.elapsed().as_secs()
    }

    fn mono_boot_scoped(&self) -> bool {
        cfg!(target_vendor = "apple")
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
        // A monotonic reading behind its anchor means the source ran back or
        // restarted; the remainder can no longer be measured.
        if mono < self.anchor_mono {
            return ClockVerdict::Invalid;
        }
        let wall_elapsed = wall - self.anchor_wall;
        let mono_elapsed = (mono - self.anchor_mono) as i64;
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

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    const PROBE_ENV: &str = "TESLATLAS_WINDOW_CLOCK_PROBE";
    /// Wall stepping by NTP since boot stays far inside this; a clock that
    /// did not count sleep, or a process-scoped origin, misses it by the
    /// whole time asleep or the whole uptime.
    const BOOT_ORIGIN_TOLERANCE_SECONDS: i64 = 30;

    /// Runs only inside the child process the restart test spawns: a fresh
    /// process prints its own reading of the system clock.
    #[test]
    fn window_resume_system_clock_child_probe() {
        if std::env::var_os(PROBE_ENV).is_none() {
            return;
        }
        let clock = SystemClock::new();
        println!(
            "probe mono={} boot={}",
            clock.mono(),
            clock.boot_time().expect("boot time")
        );
    }

    fn probe_in_new_process() -> (u64, i64) {
        let module = module_path!().split_once("::").expect("crate path").1;
        let output = Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                &format!("{module}::window_resume_system_clock_child_probe"),
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROBE_ENV, "1")
            .output()
            .expect("child process");
        assert!(output.status.success(), "child probe failed");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout
            .lines()
            .find_map(|line| line.split_once("probe mono="))
            .map(|(_, rest)| rest)
            .expect("probe line");
        let mut mono = None;
        let mut boot = None;
        for field in format!("mono={line}").split_whitespace() {
            match field.split_once('=') {
                Some(("mono", value)) => mono = value.parse().ok(),
                Some(("boot", value)) => boot = value.parse().ok(),
                _ => {}
            }
        }
        (mono.expect("mono"), boot.expect("boot"))
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn window_resume_system_clock_reading_survives_a_process_restart() {
        let clock = SystemClock::new();
        assert!(clock.mono_boot_scoped());
        let boot = clock.boot_time().expect("boot time");
        let before = clock.mono();
        let (child_mono, child_boot) = probe_in_new_process();
        let after = SystemClock::new().mono();
        assert_eq!(child_boot, boot, "same boot, same identity");
        assert!(
            before <= child_mono && child_mono <= after,
            "a new process reads the same clock: {before} <= {child_mono} <= {after}"
        );
        // A deadline anchored before the restart still holds after it.
        let wall = clock.wall();
        let guard = ClockGuard::anchored(wall, before, wall + 3600, wall);
        let mut resumed = guard;
        assert_eq!(resumed.check(wall, child_mono), ClockVerdict::Ok);
        assert_eq!(resumed.deadline_mono(), before + 3600);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn window_resume_system_clock_counts_from_boot_and_through_sleep() {
        let clock = SystemClock::new();
        let boot = clock.boot_time().expect("boot time");
        let origin = clock.wall() - clock.mono() as i64;
        assert!(
            (origin - boot).abs() <= BOOT_ORIGIN_TOLERANCE_SECONDS,
            "monotonic origin {origin} is the boot {boot}"
        );
    }

    #[test]
    fn window_resume_clock_guard_reading_behind_its_anchor_is_invalid() {
        let wall = CLOCK_FLOOR_EPOCH_SECONDS + 1_000_000;
        let mut guard = ClockGuard::anchored(wall, 5_000, wall + 3600, wall);
        assert_eq!(guard.check(wall + 10, 5_010), ClockVerdict::Ok);
        assert_eq!(guard.check(wall + 20, 4_999), ClockVerdict::Invalid);
        assert_eq!(guard.check(wall + 3600, 8_600), ClockVerdict::Expired);
    }

    #[test]
    fn window_resume_boot_time_parses_kern_boottime() {
        assert_eq!(
            parse_boot_time("{ sec = 1793974448, usec = 158203 } Fri Nov  6 14:14:08 2026"),
            Some(1_793_974_448)
        );
        assert_eq!(parse_boot_time(""), None);
    }
}

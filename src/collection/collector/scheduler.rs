// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollPhase {
    Driving,
    Charging,
    Updating,
    Online,
}

fn poll_phase_for_vehicle_state(state: &str) -> PollPhase {
    match state {
        "driving" => PollPhase::Driving,
        "charging" => PollPhase::Charging,
        "updating" => PollPhase::Updating,
        _ => PollPhase::Online,
    }
}

fn generic_api_retry_delay(scheduled: &ScheduledVehicle) -> Duration {
    #[cfg(test)]
    if let Some(delay) = supervised_test_owner_api_failure_retry() {
        return delay;
    }
    match scheduled.vehicle.state.as_str() {
        "asleep" | "offline" | "start" | "suspended" => GENERIC_OTHER_RETRY,
        "driving" => GENERIC_DRIVING_RETRY,
        "charging" => GENERIC_CHARGING_RETRY,
        "updating" => GENERIC_ONLINE_RETRY,
        "online" => match scheduled.last_phase {
            PollPhase::Driving => GENERIC_DRIVING_RETRY,
            PollPhase::Charging => GENERIC_CHARGING_RETRY,
            PollPhase::Updating | PollPhase::Online => GENERIC_ONLINE_RETRY,
        },
        _ => GENERIC_OTHER_RETRY,
    }
}

#[derive(Clone, Debug)]
struct ScheduledVehicle {
    vehicle: Vehicle,
    settings: crate::hub_pack::ProjectionCarSettings,
    next_poll: Instant,
    failure_backoff: Duration,
    last_phase: PollPhase,
    state_since: Instant,
    offline_timeout_emitted: bool,
    last_used: Instant,
    suspended: bool,
    stream_healthy: bool,
    stream_outage_started_at: Option<Instant>,
    consecutive_stream_failures: u32,
    pre_online: PreOnlineCheck,
    service_mode: bool,
    offline_state_fetch_due: Option<Instant>,
}

#[derive(Default)]
struct VehicleFuseState {
    api_errors: Vec<Instant>,
    api_blown_until: Option<Instant>,
    not_found: Vec<Instant>,
    not_found_blown_until: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreOnlineCheck {
    Idle,
    Probing {
        deadline: Instant,
    },
    ConfirmedFake {
        deadline: Instant,
    },
    ConfirmedReal,
    /// `vehicle_data` is safe without a live stream gate. Reached either after
    /// the bounded silent-stream fallback or after one successful gated read.
    OwnerApiReady,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamOutageStatus {
    consecutive_failures: u32,
    outage_duration: Duration,
    owner_api_fallback_scheduled: bool,
    live_power_gate: bool,
    phase: PollPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamRecoveryStatus {
    failures: u32,
    outage_duration: Duration,
}

enum StreamRecovery {
    Unchanged,
    Recovered(StreamRecoveryStatus),
}

enum StreamOutage {
    Ignored,
    Active(StreamOutageStatus),
}

const PRE_ONLINE_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_SETTINGS_REFRESH: Duration = Duration::from_secs(30);
const STREAM_EVENT_DRAIN_INTERVAL: Duration = Duration::from_millis(100);

const fn collection_sleep_cap(has_active_streams: bool) -> Duration {
    if has_active_streams {
        STREAM_EVENT_DRAIN_INTERVAL
    } else {
        CONTROL_SETTINGS_REFRESH
    }
}

struct VehicleScheduler {
    cadence: CollectorCadence,
    vehicles: BTreeMap<VehicleId, ScheduledVehicle>,
    next_discovery: Instant,
    discovery_backoff: Duration,
    vehicle_fuses: HashMap<VehicleId, VehicleFuseState>,
}

impl VehicleScheduler {
    fn new(cadence: CollectorCadence, now: Instant) -> Self {
        Self {
            cadence,
            vehicles: BTreeMap::new(),
            next_discovery: now,
            discovery_backoff: cadence.sleeping,
            vehicle_fuses: HashMap::new(),
        }
    }

    fn discovery_due(&self, now: Instant) -> bool {
        now >= self.next_discovery
    }

    fn apply_control_settings(
        &mut self,
        configured: &[(uuid::Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
        now: Instant,
    ) -> Vec<VehicleId> {
        let mut disconnect = Vec::new();
        let mut rediscover = false;
        let removed = self
            .vehicles
            .keys()
            .copied()
            .filter(|vehicle_id| {
                !configured
                    .iter()
                    .any(|(_, eid, _)| vehicle_id.get() == *eid as u64)
            })
            .collect::<Vec<_>>();
        for vehicle_id in removed {
            self.vehicles.remove(&vehicle_id);
            self.vehicle_fuses.remove(&vehicle_id);
            disconnect.push(vehicle_id);
            rediscover = true;
        }
        for scheduled in self.vehicles.values_mut() {
            let Some((_, _, settings)) = configured
                .iter()
                .find(|(_, eid, _)| scheduled.vehicle.id.get() == *eid as u64)
            else {
                continue;
            };
            if scheduled.settings == *settings {
                continue;
            }
            let was_enabled = scheduled.settings.enabled;
            let was_streaming = scheduled.settings.use_streaming_api;
            scheduled.settings = settings.clone();
            scheduled.vehicle.settings = settings.clone();
            if !settings.enabled || !settings.use_streaming_api {
                scheduled.stream_healthy = false;
                scheduled.stream_outage_started_at = None;
                scheduled.consecutive_stream_failures = 0;
                scheduled.pre_online = PreOnlineCheck::Idle;
                disconnect.push(scheduled.vehicle.id);
            } else if scheduled.vehicle.is_online() {
                scheduled.next_poll = now;
                rediscover |= !was_enabled || !was_streaming;
            }
        }
        if rediscover {
            self.next_discovery = now;
        }
        disconnect
    }

    fn accept_discovery(&mut self, vehicles: Vec<Vehicle>, now: Instant) -> Vec<Vehicle> {
        let mut discovered = BTreeMap::new();
        let mut events = Vec::new();
        for vehicle in vehicles {
            let previous = self.vehicles.get(&vehicle.id);
            let state_changed =
                previous.is_none_or(|scheduled| scheduled.vehicle.state != vehicle.state);
            let newly_online = vehicle.is_online()
                && previous.is_none_or(|scheduled| !scheduled.vehicle.is_online());
            let next_poll = if newly_online {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.next_poll)
            };
            let failure_backoff = if newly_online {
                self.cadence.online
            } else {
                previous.map_or(self.cadence.online, |scheduled| scheduled.failure_backoff)
            };
            let last_phase = if state_changed {
                poll_phase_for_vehicle_state(&vehicle.state)
            } else {
                previous.map_or(PollPhase::Online, |scheduled| scheduled.last_phase)
            };
            let state_since = if state_changed {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.state_since)
            };
            let last_used = if newly_online {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.last_used)
            };
            let offline_timeout = vehicle.state == "offline"
                && !state_changed
                && previous.is_some_and(|scheduled| !scheduled.offline_timeout_emitted)
                && now.saturating_duration_since(state_since) >= self.cadence.offline_drive_timeout;
            if state_changed || offline_timeout {
                events.push(vehicle.clone());
            }
            let stream_healthy = if state_changed || !vehicle.settings.use_streaming_api {
                false
            } else {
                previous.is_some_and(|scheduled| scheduled.stream_healthy)
            };
            let service_mode = previous.is_some_and(|scheduled| scheduled.service_mode);
            let pre_online =
                if !vehicle.is_online() || !vehicle.settings.use_streaming_api || service_mode {
                    PreOnlineCheck::Idle
                } else if newly_online {
                    PreOnlineCheck::Probing {
                        deadline: now + PRE_ONLINE_TIMEOUT,
                    }
                } else {
                    previous.map_or(PreOnlineCheck::Idle, |scheduled| scheduled.pre_online)
                };
            discovered.insert(
                vehicle.id,
                ScheduledVehicle {
                    settings: vehicle.settings.clone(),
                    vehicle,
                    next_poll,
                    failure_backoff,
                    last_phase,
                    state_since,
                    offline_timeout_emitted: if state_changed {
                        false
                    } else {
                        previous.is_some_and(|scheduled| scheduled.offline_timeout_emitted)
                            || offline_timeout
                    },
                    last_used,
                    suspended: if state_changed {
                        false
                    } else {
                        previous.is_some_and(|scheduled| scheduled.suspended)
                    },
                    stream_healthy,
                    stream_outage_started_at: if state_changed {
                        None
                    } else {
                        previous.and_then(|scheduled| scheduled.stream_outage_started_at)
                    },
                    consecutive_stream_failures: if state_changed {
                        0
                    } else {
                        previous.map_or(0, |scheduled| scheduled.consecutive_stream_failures)
                    },
                    pre_online,
                    service_mode,
                    offline_state_fetch_due: if state_changed {
                        None
                    } else {
                        previous.and_then(|scheduled| scheduled.offline_state_fetch_due)
                    },
                },
            );
        }
        self.vehicles = discovered;
        self.vehicle_fuses
            .retain(|id, _| self.vehicles.contains_key(id));
        for id in self.vehicles.keys().copied() {
            self.vehicle_fuses.entry(id).or_default();
        }
        self.next_discovery = now + self.cadence.sleeping;
        self.discovery_backoff = self.cadence.sleeping;
        events
    }

    fn accept_vehicle_state(
        &mut self,
        vehicle_id: VehicleId,
        state: String,
        now: Instant,
    ) -> Vec<Vehicle> {
        let Some(mut vehicle) = self
            .vehicles
            .get(&vehicle_id)
            .map(|scheduled| scheduled.vehicle.clone())
        else {
            return Vec::new();
        };
        vehicle.state = state;
        let events = self.accept_discovery_mode(vec![vehicle], now, false);
        if let Some(scheduled) = self.vehicles.get_mut(&vehicle_id) {
            scheduled.offline_state_fetch_due = None;
        }
        events
    }

    fn accept_discovery_mode(
        &mut self,
        vehicles: Vec<Vehicle>,
        now: Instant,
        replace_all: bool,
    ) -> Vec<Vehicle> {
        let mut discovered = if replace_all {
            BTreeMap::new()
        } else {
            self.vehicles.clone()
        };
        let mut events = Vec::new();
        for vehicle in vehicles {
            let previous = self.vehicles.get(&vehicle.id);
            let state_changed =
                previous.is_none_or(|scheduled| scheduled.vehicle.state != vehicle.state);
            let newly_online = vehicle.is_online()
                && previous.is_none_or(|scheduled| !scheduled.vehicle.is_online());
            let next_poll = if newly_online {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.next_poll)
            };
            let failure_backoff = if newly_online {
                self.cadence.online
            } else {
                previous.map_or(self.cadence.online, |scheduled| scheduled.failure_backoff)
            };
            let last_phase = if state_changed {
                poll_phase_for_vehicle_state(&vehicle.state)
            } else {
                previous.map_or(PollPhase::Online, |scheduled| scheduled.last_phase)
            };
            let state_since = if state_changed {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.state_since)
            };
            let last_used = if newly_online {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.last_used)
            };
            let offline_timeout = vehicle.state == "offline"
                && !state_changed
                && previous.is_some_and(|scheduled| !scheduled.offline_timeout_emitted)
                && now.saturating_duration_since(state_since) >= self.cadence.offline_drive_timeout;
            if state_changed || offline_timeout {
                events.push(vehicle.clone());
            }
            let stream_healthy = if state_changed || !vehicle.settings.use_streaming_api {
                false
            } else {
                previous.is_some_and(|scheduled| scheduled.stream_healthy)
            };
            let service_mode = previous.is_some_and(|scheduled| scheduled.service_mode);
            let pre_online =
                if !vehicle.is_online() || !vehicle.settings.use_streaming_api || service_mode {
                    PreOnlineCheck::Idle
                } else if newly_online {
                    PreOnlineCheck::Probing {
                        deadline: now + PRE_ONLINE_TIMEOUT,
                    }
                } else {
                    previous.map_or(PreOnlineCheck::Idle, |scheduled| scheduled.pre_online)
                };
            discovered.insert(
                vehicle.id,
                ScheduledVehicle {
                    settings: vehicle.settings.clone(),
                    vehicle,
                    next_poll,
                    failure_backoff,
                    last_phase,
                    state_since,
                    offline_timeout_emitted: if state_changed {
                        false
                    } else {
                        previous.is_some_and(|scheduled| scheduled.offline_timeout_emitted)
                            || offline_timeout
                    },
                    last_used,
                    suspended: if state_changed {
                        false
                    } else {
                        previous.is_some_and(|scheduled| scheduled.suspended)
                    },
                    stream_healthy,
                    stream_outage_started_at: if state_changed {
                        None
                    } else {
                        previous.and_then(|scheduled| scheduled.stream_outage_started_at)
                    },
                    consecutive_stream_failures: if state_changed {
                        0
                    } else {
                        previous.map_or(0, |scheduled| scheduled.consecutive_stream_failures)
                    },
                    pre_online,
                    service_mode,
                    offline_state_fetch_due: if state_changed {
                        None
                    } else {
                        previous.and_then(|scheduled| scheduled.offline_state_fetch_due)
                    },
                },
            );
        }
        self.vehicles = discovered;
        self.vehicle_fuses
            .retain(|id, _| self.vehicles.contains_key(id));
        for id in self.vehicles.keys().copied() {
            self.vehicle_fuses.entry(id).or_default();
        }
        if replace_all {
            self.next_discovery = now + self.cadence.sleeping;
            self.discovery_backoff = self.cadence.sleeping;
        }
        events
    }

    fn discovery_failed(&mut self, now: Instant) -> Duration {
        let delay = self.discovery_backoff;
        self.next_discovery = now + delay;
        self.discovery_backoff = self
            .discovery_backoff
            .saturating_mul(2)
            .min(self.cadence.maximum_backoff);
        delay
    }

    fn discovery_failed_for_error(&mut self, error: &CollectorError, now: Instant) -> Duration {
        if let Some(OwnerApiError::RateLimited {
            retry_after_seconds,
        }) = owner_api_error(error)
        {
            let delay = Duration::from_secs(*retry_after_seconds);
            self.next_discovery = retry_deadline(now, *retry_after_seconds);
            return delay;
        }
        self.discovery_failed(now)
    }

    fn due_vehicles(&mut self, now: Instant) -> Vec<VehicleId> {
        for scheduled in self.vehicles.values_mut() {
            if let PreOnlineCheck::Probing { deadline } = scheduled.pre_online
                && now >= deadline
            {
                // TeslaMate falls back to vehicle_data when a new stream stays
                // silent. A nil-power frame remains a confirmed fake-online
                // signal and deliberately does not take this fallback.
                scheduled.pre_online = PreOnlineCheck::OwnerApiReady;
                scheduled.next_poll = now;
            }
        }
        // A newly online streaming car requires numeric stream power before
        // its first vehicle_data read. Established cars and bounded silent
        // probes use normal Owner API fallback.
        let candidates = self
            .vehicles
            .values()
            .filter(|scheduled| {
                scheduled.vehicle.is_online()
                    && scheduled.settings.enabled
                    && !scheduled.service_mode
                    && (!scheduled.settings.use_streaming_api
                        || matches!(
                            scheduled.pre_online,
                            PreOnlineCheck::ConfirmedReal | PreOnlineCheck::OwnerApiReady
                        ))
                    && now >= scheduled.next_poll
            })
            .map(|scheduled| scheduled.vehicle.id)
            .collect::<Vec<_>>();
        candidates
            .into_iter()
            .filter(|id| self.vehicle_fuse_healthy(*id, now))
            .collect()
    }

    fn has_due_stream_fallback(&mut self, now: Instant) -> bool {
        self.due_vehicles(now).into_iter().any(|id| {
            self.vehicles.get(&id).is_some_and(|scheduled| {
                scheduled.settings.use_streaming_api && !scheduled.stream_healthy
            })
        })
    }

    fn schedule_offline_state_fetch(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            if scheduled.offline_state_fetch_due.is_none() {
                scheduled.offline_state_fetch_due = Some(now);
            }
            scheduled.next_poll = now + GENERIC_OTHER_RETRY;
        }
    }

    fn due_offline_state_vehicles(&mut self, now: Instant) -> Vec<VehicleId> {
        self.vehicles
            .values_mut()
            .filter_map(|scheduled| {
                scheduled
                    .offline_state_fetch_due
                    .filter(|due| now >= *due)
                    .map(|_| {
                        scheduled.offline_state_fetch_due = None;
                        scheduled.vehicle.id
                    })
            })
            .collect()
    }

    fn offline_state_failed_for_error(
        &mut self,
        id: VehicleId,
        error: &CollectorError,
        now: Instant,
    ) {
        let delay = if matches!(
            error,
            CollectorError::LegacyAuthManager(_)
                | CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(_))
                | CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
        ) {
            LEGACY_REFRESH_RETRY
        } else if let Some(OwnerApiError::RateLimited {
            retry_after_seconds,
        }) = owner_api_error(error)
        {
            Duration::from_secs(*retry_after_seconds)
        } else {
            GENERIC_OTHER_RETRY
        };
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            let due = now.checked_add(delay).unwrap_or(now);
            scheduled.offline_state_fetch_due = Some(due);
            scheduled.next_poll = due;
        }
    }

    fn vehicle_succeeded(
        &mut self,
        id: VehicleId,
        phase: PollPhase,
        sleep_eligible: bool,
        now: Instant,
    ) -> Option<Vehicle> {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            if !scheduled.settings.enabled {
                return None;
            }
            if scheduled.service_mode {
                return None;
            }
            // Numeric power protects only the first potentially waking read.
            // Once that read succeeds, later driving/charging/online fallback
            // must remain available when the legacy stream disconnects.
            if matches!(scheduled.pre_online, PreOnlineCheck::ConfirmedReal) {
                scheduled.pre_online = PreOnlineCheck::OwnerApiReady;
            }
            let was_suspended = scheduled.suspended;
            let (idle_suspend_after, suspended_interval) = if scheduled.settings.use_streaming_api
                && scheduled.stream_healthy
            {
                (Duration::from_secs(3 * 60), Duration::from_secs(10 * 60))
            } else {
                (
                    Duration::from_secs((scheduled.settings.suspend_after_idle_min * 60) as u64),
                    Duration::from_secs((scheduled.settings.suspend_min * 60) as u64),
                )
            };
            let interval = match (phase, sleep_eligible) {
                (PollPhase::Driving, _) => {
                    scheduled.last_used = now;
                    scheduled.suspended = false;
                    if scheduled.settings.use_streaming_api && scheduled.stream_healthy {
                        Duration::from_secs(15)
                    } else {
                        self.cadence.driving
                    }
                }
                (PollPhase::Charging, _) => {
                    scheduled.last_used = now;
                    scheduled.suspended = false;
                    self.cadence.charging
                }
                (PollPhase::Updating, _) => {
                    scheduled.last_used = now;
                    scheduled.suspended = false;
                    self.cadence.updating
                }
                (PollPhase::Online, false) => {
                    scheduled.last_used = now;
                    scheduled.suspended = false;
                    self.cadence.online
                }
                (PollPhase::Online, true)
                    if now.saturating_duration_since(scheduled.last_used) >= idle_suspend_after =>
                {
                    scheduled.suspended = true;
                    suspended_interval
                }
                (PollPhase::Online, true) => {
                    scheduled.suspended = false;
                    self.cadence.online
                }
            };
            scheduled.next_poll = now + interval;
            scheduled.failure_backoff = interval;
            scheduled.last_phase = phase;
            if scheduled.suspended != was_suspended {
                let mut event = scheduled.vehicle.clone();
                event.state = if scheduled.suspended {
                    "suspended".to_owned()
                } else {
                    "online".to_owned()
                };
                return Some(event);
            }
        }
        None
    }

    fn vehicle_failed(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            let delay = scheduled.failure_backoff;
            scheduled.next_poll = now + delay;
            scheduled.failure_backoff = scheduled
                .failure_backoff
                .saturating_mul(2)
                .min(self.cadence.maximum_backoff);
        }
    }

    fn vehicle_failed_for_error(&mut self, id: VehicleId, error: &CollectorError, now: Instant) {
        if matches!(
            error,
            CollectorError::LegacyAuthManager(_)
                | CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(_))
                | CollectorError::FleetCredential(_)
        ) {
            self.vehicle_retry_after(id, LEGACY_REFRESH_RETRY, now);
            return;
        }
        if let CollectorError::FleetApi(error) = error {
            match error {
                FleetApiError::RateLimited {
                    retry_after_seconds,
                } => self.vehicle_rate_limited(id, *retry_after_seconds, now),
                FleetApiError::HttpStatus(404)
                | FleetApiError::ProviderHttpStatus { status: 404, .. } => {
                    self.vehicle_not_found(id, now)
                }
                FleetApiError::RequestTimeout
                | FleetApiError::HttpStatus(401 | 403)
                | FleetApiError::ProviderHttpStatus {
                    status: 401 | 403, ..
                } => {
                    self.vehicle_failed(id, now);
                }
                _ => self.vehicle_api_error(id, now),
            }
            return;
        }
        let Some(error) = owner_api_error(error) else {
            self.vehicle_failed(id, now);
            return;
        };
        match error {
            OwnerApiError::RateLimited {
                retry_after_seconds,
            } => self.vehicle_rate_limited(id, *retry_after_seconds, now),
            OwnerApiError::VehicleNotFound => self.vehicle_not_found(id, now),
            OwnerApiError::RequestTimeout
            | OwnerApiError::VehicleInService
            | OwnerApiError::HttpStatus(401) => self.vehicle_failed(id, now),
            OwnerApiError::LegacyAuth => self.vehicle_retry_after(id, LEGACY_REFRESH_RETRY, now),
            OwnerApiError::StreamPowerNotConfirmed => {
                let delay = self
                    .vehicles
                    .get(&id)
                    .map(generic_api_retry_delay)
                    .unwrap_or(GENERIC_OTHER_RETRY);
                self.vehicle_retry_after(id, delay, now);
            }
            _ => self.vehicle_api_error(id, now),
        }
    }

    fn vehicle_retry_after(&mut self, id: VehicleId, delay: Duration, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.next_poll = now + delay;
        }
    }

    fn vehicle_rate_limited(&mut self, id: VehicleId, seconds: u64, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.next_poll = retry_deadline(now, seconds);
        }
    }

    fn vehicle_api_error(&mut self, id: VehicleId, now: Instant) {
        let state = self.vehicle_fuses.entry(id).or_default();
        state
            .api_errors
            .retain(|at| now.saturating_duration_since(*at) < API_ERROR_WINDOW);
        state.api_errors.push(now);
        if state.api_errors.len() >= API_ERROR_LIMIT {
            state.api_blown_until = now.checked_add(API_ERROR_RESET);
        }
        let delay = self
            .vehicles
            .get(&id)
            .map(generic_api_retry_delay)
            .unwrap_or(GENERIC_OTHER_RETRY);
        self.vehicle_retry_after(id, delay, now);
    }

    fn vehicle_not_found(&mut self, id: VehicleId, now: Instant) {
        let state = self.vehicle_fuses.entry(id).or_default();
        state
            .not_found
            .retain(|at| now.saturating_duration_since(*at) < VEHICLE_NOT_FOUND_WINDOW);
        state.not_found.push(now);
        state
            .api_errors
            .retain(|at| now.saturating_duration_since(*at) < API_ERROR_WINDOW);
        state.api_errors.push(now);
        if state.not_found.len() >= VEHICLE_NOT_FOUND_LIMIT {
            state.not_found_blown_until = now.checked_add(VEHICLE_NOT_FOUND_RESET);
        }
        if state.api_errors.len() >= API_ERROR_LIMIT {
            state.api_blown_until = now.checked_add(API_ERROR_RESET);
        }
        self.vehicle_failed(id, now);
    }

    fn vehicle_fuse_healthy(&mut self, id: VehicleId, now: Instant) -> bool {
        let Some(state) = self.vehicle_fuses.get_mut(&id) else {
            return true;
        };
        if state.api_blown_until.is_some_and(|until| now >= until) {
            state.api_blown_until = None;
            state.api_errors.clear();
        }
        if state
            .not_found_blown_until
            .is_some_and(|until| now >= until)
        {
            state.not_found_blown_until = None;
            state.not_found.clear();
        }
        state.api_blown_until.is_none() && state.not_found_blown_until.is_none()
    }

    fn due_service_vehicles(&self, now: Instant) -> Vec<VehicleId> {
        self.vehicles
            .values()
            .filter(|scheduled| {
                scheduled.vehicle.is_online()
                    && scheduled.settings.enabled
                    && scheduled.service_mode
                    && now >= scheduled.next_poll
            })
            .map(|scheduled| scheduled.vehicle.id)
            .collect()
    }

    fn enter_service_mode(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.service_mode = true;
            scheduled.stream_healthy = false;
            scheduled.stream_outage_started_at = None;
            scheduled.consecutive_stream_failures = 0;
            scheduled.suspended = false;
            scheduled.pre_online = PreOnlineCheck::Idle;
            scheduled.failure_backoff = self.cadence.online;
            scheduled.next_poll = now + self.cadence.online;
        }
    }

    fn service_retry(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.service_mode = true;
            scheduled.next_poll = now + self.cadence.online;
            scheduled.failure_backoff = self.cadence.online;
        }
    }

    fn service_exited(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.service_mode = false;
            scheduled.stream_healthy = false;
            scheduled.stream_outage_started_at = None;
            scheduled.consecutive_stream_failures = 0;
            scheduled.suspended = false;
            scheduled.next_poll = now;
            scheduled.failure_backoff = self.cadence.online;
            scheduled.pre_online = if scheduled.settings.use_streaming_api {
                PreOnlineCheck::Probing {
                    deadline: now + PRE_ONLINE_TIMEOUT,
                }
            } else {
                PreOnlineCheck::Idle
            };
        }
    }

    fn stream_healthy(&mut self, id: VehicleId, now: Instant) -> StreamRecovery {
        if let Some(scheduled) = self.vehicles.get_mut(&id)
            && scheduled.settings.use_streaming_api
        {
            let recovered = !scheduled.stream_healthy;
            let recovery =
                scheduled
                    .stream_outage_started_at
                    .take()
                    .map(|started_at| StreamRecoveryStatus {
                        failures: scheduled.consecutive_stream_failures,
                        outage_duration: now.saturating_duration_since(started_at),
                    });
            scheduled.consecutive_stream_failures = 0;
            scheduled.stream_healthy = true;
            scheduled.suspended = false;
            if recovered
                && !matches!(
                    scheduled.pre_online,
                    PreOnlineCheck::Probing { .. } | PreOnlineCheck::ConfirmedFake { .. }
                )
            {
                scheduled.next_poll = scheduled.next_poll.min(now);
            }
            return recovery.map_or(StreamRecovery::Unchanged, StreamRecovery::Recovered);
        }
        StreamRecovery::Unchanged
    }

    fn stream_unhealthy(&mut self, id: VehicleId, now: Instant) -> StreamOutage {
        if let Some(scheduled) = self.vehicles.get_mut(&id)
            && scheduled.settings.use_streaming_api
        {
            let started_at = *scheduled.stream_outage_started_at.get_or_insert(now);
            scheduled.consecutive_stream_failures =
                scheduled.consecutive_stream_failures.saturating_add(1);
            scheduled.stream_healthy = false;
            scheduled.suspended = false;
            if matches!(scheduled.pre_online, PreOnlineCheck::ConfirmedReal) {
                scheduled.pre_online = PreOnlineCheck::Probing {
                    deadline: now + PRE_ONLINE_TIMEOUT,
                };
            }
            if !matches!(
                scheduled.pre_online,
                PreOnlineCheck::Probing { .. } | PreOnlineCheck::ConfirmedFake { .. }
            ) {
                scheduled.next_poll = now;
            }
            return StreamOutage::Active(StreamOutageStatus {
                consecutive_failures: scheduled.consecutive_stream_failures,
                outage_duration: now.saturating_duration_since(started_at),
                owner_api_fallback_scheduled: now >= scheduled.next_poll,
                live_power_gate: matches!(
                    scheduled.pre_online,
                    PreOnlineCheck::Probing { .. }
                        | PreOnlineCheck::ConfirmedFake { .. }
                        | PreOnlineCheck::ConfirmedReal
                ),
                phase: scheduled.last_phase,
            });
        }
        StreamOutage::Ignored
    }

    fn pre_online_power(&mut self, id: VehicleId, power: Option<i64>, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id)
            && matches!(scheduled.pre_online, PreOnlineCheck::Probing { .. })
        {
            observe_pre_online_power(&mut scheduled.pre_online, power, now);
            match scheduled.pre_online {
                PreOnlineCheck::ConfirmedReal | PreOnlineCheck::OwnerApiReady => {
                    scheduled.next_poll = now;
                }
                PreOnlineCheck::ConfirmedFake { deadline } => {
                    scheduled.next_poll = deadline;
                }
                PreOnlineCheck::Idle | PreOnlineCheck::Probing { .. } => {}
            }
        }
    }

    fn should_persist_stream_telemetry(
        &mut self,
        id: VehicleId,
        power: Option<i64>,
        now: Instant,
    ) -> bool {
        let Some(scheduled) = self.vehicles.get_mut(&id) else {
            return false;
        };
        if !matches!(scheduled.vehicle.state.as_str(), "asleep" | "offline") {
            self.pre_online_power(id, power, now);
            return true;
        }

        match (&scheduled.pre_online, power) {
            (PreOnlineCheck::Idle, None) => {
                scheduled.pre_online = PreOnlineCheck::ConfirmedFake {
                    deadline: now + PRE_ONLINE_TIMEOUT,
                };
            }
            (PreOnlineCheck::Idle, Some(_)) => {
                scheduled.pre_online = PreOnlineCheck::ConfirmedReal;
            }
            _ => observe_pre_online_power(&mut scheduled.pre_online, power, now),
        }
        if matches!(scheduled.pre_online, PreOnlineCheck::ConfirmedReal) {
            scheduled.next_poll = now;
        }
        power.is_some()
    }

    fn schedule_stream_charging_poll(
        &mut self,
        id: VehicleId,
        shift_state: Option<&str>,
        power: Option<i64>,
        now: Instant,
    ) {
        let Some(scheduled) = self.vehicles.get_mut(&id) else {
            return;
        };
        let charging_hint = power.is_some_and(|power| power < 0)
            && (shift_state.is_none() || (scheduled.suspended && shift_state == Some("P")));
        if charging_hint
            && scheduled.vehicle.is_online()
            && scheduled.last_phase != PollPhase::Charging
        {
            scheduled.last_phase = PollPhase::Charging;
            scheduled.suspended = false;
            scheduled.last_used = now;
            scheduled.next_poll = now;
        }
    }

    fn should_start_stream(&self, id: VehicleId) -> bool {
        self.vehicles.get(&id).is_some_and(|scheduled| {
            scheduled.settings.use_streaming_api
                && matches!(
                    scheduled.pre_online,
                    PreOnlineCheck::Probing { .. }
                        | PreOnlineCheck::ConfirmedFake { .. }
                        | PreOnlineCheck::ConfirmedReal
                        | PreOnlineCheck::OwnerApiReady
                )
        })
    }

    /// Numeric stream power is the strict no-wake prerequisite. A stream that
    /// stays silent for the bounded startup window instead uses TeslaMate's
    /// Owner API fallback after products has already reported the car online.
    fn requires_live_stream_power_gate(&self, id: VehicleId) -> bool {
        self.vehicles.get(&id).is_some_and(|scheduled| {
            scheduled.settings.use_streaming_api
                && matches!(scheduled.pre_online, PreOnlineCheck::ConfirmedReal)
        })
    }

    fn vehicles(&self) -> Vec<Vehicle> {
        self.vehicles
            .values()
            .map(|scheduled| scheduled.vehicle.clone())
            .collect()
    }

    fn delay_until_next_action(&self, now: Instant) -> Duration {
        let next_offline_state = self
            .vehicles
            .values()
            .filter_map(|scheduled| scheduled.offline_state_fetch_due)
            .min();
        let next_vehicle = self
            .vehicles
            .values()
            .filter(|scheduled| scheduled.vehicle.is_online() && scheduled.settings.enabled)
            .map(|scheduled| match scheduled.pre_online {
                PreOnlineCheck::Probing { deadline } => deadline,
                _ => scheduled.next_poll,
            })
            .min();
        next_vehicle
            .into_iter()
            .chain(next_offline_state)
            .min()
            .unwrap_or(self.next_discovery)
            .min(self.next_discovery)
            .saturating_duration_since(now)
    }
}

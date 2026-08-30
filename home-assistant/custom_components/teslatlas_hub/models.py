"""Integration-side models independent of the unfinished wire protocol."""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field, replace
from datetime import datetime
from types import MappingProxyType


@dataclass(frozen=True, slots=True)
class HubEndpoint:
    """One candidate public Hub endpoint."""

    host: str
    port: int
    use_tls: bool


@dataclass(frozen=True, slots=True)
class HubInfo:
    """Stable public identity and advertised capabilities for one Hub."""

    hub_id: str
    name: str
    protocol_version: str
    capabilities: frozenset[str] = field(default_factory=frozenset)


@dataclass(frozen=True, slots=True)
class PairingResult:
    """A successful public pairing claim."""

    info: HubInfo
    access_token: str = field(repr=False)


@dataclass(frozen=True, slots=True)
class HubStatus:
    """Read-only Hub-level state exposed to Home Assistant."""

    collector_health: str | None = None
    fleet_cost_usd: float | None = None
    backup_age_seconds: int | None = None


@dataclass(frozen=True, slots=True)
class VehicleState:
    """Read-only vehicle projection consumed by entities."""

    vehicle_id: str
    name: str
    state_of_charge: float | None = None
    charging_state: str | None = None
    charging_power_kw: float | None = None
    charge_limit_percent: float | None = None
    estimated_range_km: float | None = None
    odometer_km: float | None = None
    activity_state: str | None = None
    inside_temperature_c: float | None = None
    outside_temperature_c: float | None = None
    access_state: str | None = None
    software_version: str | None = None
    software_update_state: str | None = None
    telemetry_age_seconds: int | None = None
    data_quality: str | None = None


@dataclass(frozen=True, slots=True)
class HubSnapshot:
    """One immutable integration-side view of Hub state."""

    info: HubInfo
    status: HubStatus
    vehicles: Mapping[str, VehicleState]
    received_at: datetime

    @classmethod
    def create(
        cls,
        *,
        info: HubInfo,
        status: HubStatus,
        vehicles: Iterable[VehicleState],
        received_at: datetime,
    ) -> HubSnapshot:
        """Create a snapshot with an owned, read-only vehicle mapping."""
        vehicle_map = {vehicle.vehicle_id: vehicle for vehicle in vehicles}
        return cls(
            info=info,
            status=status,
            vehicles=MappingProxyType(vehicle_map),
            received_at=received_at,
        )

    def with_vehicle(self, vehicle: VehicleState, received_at: datetime) -> HubSnapshot:
        """Return a new snapshot containing one replaced vehicle."""
        vehicles = dict(self.vehicles)
        vehicles[vehicle.vehicle_id] = vehicle
        return replace(
            self,
            vehicles=MappingProxyType(vehicles),
            received_at=received_at,
        )


@dataclass(frozen=True, slots=True)
class HubEvent:
    """One integration-side vehicle update with replay identity."""

    event_id: str
    vehicle: VehicleState
    received_at: datetime

    def apply(self, snapshot: HubSnapshot) -> HubSnapshot:
        """Apply this event without mutating the previous snapshot."""
        return snapshot.with_vehicle(self.vehicle, self.received_at)

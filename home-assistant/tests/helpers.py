"""Fixture builders for integration tests.

These shapes are local test inputs, not released Teslatlas protocol schemas.
"""

from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator
from datetime import datetime
from pathlib import Path
from typing import Any

from custom_components.teslatlas_hub.client import HubClientError
from custom_components.teslatlas_hub.models import (
    HubEvent,
    HubInfo,
    HubSnapshot,
    HubStatus,
    PairingResult,
    VehicleState,
)

FIXTURES = Path(__file__).parent / "fixtures"


def load_fixture(name: str) -> dict[str, Any]:
    """Load one deterministic redacted fixture."""
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


def parse_vehicle(payload: dict[str, Any]) -> VehicleState:
    """Translate the local test fixture shape into the integration model."""
    return VehicleState(
        vehicle_id=payload["id"],
        name=payload["name"],
        state_of_charge=payload["state_of_charge"],
        charging_state=payload["charging_state"],
        charging_power_kw=payload["charging_power_kw"],
        charge_limit_percent=payload["charge_limit_percent"],
        estimated_range_km=payload["estimated_range_km"],
        odometer_km=payload["odometer_km"],
        activity_state=payload["activity_state"],
        inside_temperature_c=payload["inside_temperature_c"],
        outside_temperature_c=payload["outside_temperature_c"],
        access_state=payload["access_state"],
        software_version=payload["software_version"],
        software_update_state=payload["software_update_state"],
        telemetry_age_seconds=payload["telemetry_age_seconds"],
        data_quality=payload["data_quality"],
    )


def parse_snapshot(payload: dict[str, Any]) -> HubSnapshot:
    """Translate the local snapshot fixture into the integration model."""
    hub = payload["hub"]
    status = payload["status"]
    return HubSnapshot.create(
        info=HubInfo(
            hub_id=hub["id"],
            name=hub["name"],
            protocol_version=hub["protocol_version"],
            capabilities=frozenset(hub["capabilities"]),
        ),
        status=HubStatus(
            collector_health=status["collector_health"],
            fleet_cost_usd=status["fleet_cost_usd"],
            backup_age_seconds=status["backup_age_seconds"],
        ),
        vehicles=[parse_vehicle(vehicle) for vehicle in payload["vehicles"]],
        received_at=datetime.fromisoformat(payload["received_at"]),
    )


def parse_event(payload: dict[str, Any]) -> HubEvent:
    """Translate the local update fixture into the integration model."""
    return HubEvent(
        event_id=payload["event_id"],
        vehicle=parse_vehicle(payload["vehicle"]),
        received_at=datetime.fromisoformat(payload["received_at"]),
    )


def initial_snapshot() -> HubSnapshot:
    """Return the canonical initial fixture snapshot."""
    return parse_snapshot(load_fixture("initial-snapshot.json"))


def vehicle_update() -> HubEvent:
    """Return the canonical fixture push event."""
    return parse_event(load_fixture("vehicle-update.json"))


class FixtureHubClient:
    """Complete in-memory implementation of the integration client boundary."""

    def __init__(self) -> None:
        """Initialize deterministic responses and call records."""
        self.snapshot = initial_snapshot()
        self.info = self.snapshot.info
        self.access_token = "fixture-device-bearer"
        self.probe_error: HubClientError | None = None
        self.pair_error: HubClientError | None = None
        self.snapshot_error: HubClientError | None = None
        self.events: list[HubEvent | HubClientError] = []
        self.event_connections: list[list[HubEvent | HubClientError]] = []
        self.pairing_secrets: list[str] = []
        self.event_cursors: list[str | None] = []
        self.snapshot_calls = 0
        self.block_after_events = True
        self.stream_blocked = asyncio.Event()
        self.closed = False

    async def async_probe(self) -> HubInfo:
        """Return fixture identity or the configured failure."""
        if self.probe_error is not None:
            raise self.probe_error
        return self.info

    async def async_pair(self, pairing_secret: str) -> PairingResult:
        """Record one transient secret and return a fixture bearer."""
        self.pairing_secrets.append(pairing_secret)
        if self.pair_error is not None:
            raise self.pair_error
        return PairingResult(info=self.info, access_token=self.access_token)

    async def async_snapshot(self) -> HubSnapshot:
        """Return fixture state or the configured failure."""
        self.snapshot_calls += 1
        if self.snapshot_error is not None:
            raise self.snapshot_error
        return self.snapshot

    async def async_events(self, last_event_id: str | None) -> AsyncIterator[HubEvent]:
        """Yield configured events and failures in order."""
        self.event_cursors.append(last_event_id)
        items = self.event_connections.pop(0) if self.event_connections else self.events
        for item in items:
            if isinstance(item, HubClientError):
                raise item
            yield item
        if self.block_after_events:
            self.stream_blocked.set()
            await asyncio.Event().wait()

    async def async_close(self) -> None:
        """Record resource cleanup."""
        self.closed = True

"""Tests for immutable integration-side data models."""

from __future__ import annotations

import pytest

from tests.helpers import initial_snapshot, vehicle_update


def test_snapshot_preserves_literal_fixture_values() -> None:
    """Catch field loss or cross-vehicle mixing in the adapter model."""
    snapshot = initial_snapshot()

    assert snapshot.info.hub_id == "hub-fixture"
    assert snapshot.info.capabilities == frozenset({"current_state", "events"})
    assert snapshot.status.collector_health == "healthy"
    assert snapshot.status.fleet_cost_usd == 12.34
    assert snapshot.status.backup_age_seconds == 3600
    assert tuple(snapshot.vehicles) == ("vehicle-alpha", "vehicle-beta")
    assert snapshot.vehicles["vehicle-alpha"].state_of_charge == 72.5
    assert snapshot.vehicles["vehicle-alpha"].inside_temperature_c is None
    assert snapshot.vehicles["vehicle-beta"].charging_power_kw == 7.2
    assert snapshot.vehicles["vehicle-beta"].data_quality == "partial"


def test_snapshot_vehicle_mapping_is_read_only() -> None:
    """Catch mutation that could leak one event across entity reads."""
    snapshot = initial_snapshot()

    with pytest.raises(TypeError):
        snapshot.vehicles["vehicle-alpha"] = snapshot.vehicles["vehicle-beta"]  # type: ignore[index]


def test_event_replaces_one_vehicle_without_mutating_prior_snapshot() -> None:
    """Catch event application that mutates or drops unrelated vehicles."""
    before = initial_snapshot()
    after = vehicle_update().apply(before)

    assert before.vehicles["vehicle-alpha"].state_of_charge == 72.5
    assert after.vehicles["vehicle-alpha"].state_of_charge == 71.0
    assert after.vehicles["vehicle-alpha"].activity_state == "driving"
    assert after.vehicles["vehicle-beta"] == before.vehicles["vehicle-beta"]
    assert after.received_at.isoformat() == "2026-08-30T12:05:00+00:00"

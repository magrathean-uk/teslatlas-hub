"""Tests for privacy-preserving diagnostics."""

from __future__ import annotations

import json
from dataclasses import replace
from unittest.mock import patch

from homeassistant.const import CONF_HOST, CONF_LATITUDE, CONF_LONGITUDE
from homeassistant.core import HomeAssistant
from homeassistant.helpers.redact import REDACTED
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PAIRING_SECRET,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
)
from custom_components.teslatlas_hub.coordinator import TeslatlasDataCoordinator
from custom_components.teslatlas_hub.diagnostics import (
    async_get_config_entry_diagnostics,
)
from tests.helpers import FixtureHubClient


async def test_diagnostics_redact_secrets_endpoints_identity_and_location(
    hass: HomeAssistant,
) -> None:
    """Catch diagnostics that expose client, vehicle, or location identity."""
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id="hub-fixture",
        data={
            CONF_HOST: "sensitive-hub.example",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "fixture-device-bearer",
            CONF_PAIRING_SECRET: "must-never-persist",
            CONF_LATITUDE: 51.501,
            CONF_LONGITUDE: -0.142,
        },
    )
    entry.add_to_hass(hass)
    client = FixtureHubClient()

    with patch(
        "custom_components.teslatlas_hub.create_client",
        return_value=client,
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is True
    diagnostics = await async_get_config_entry_diagnostics(hass, entry)
    serialized = json.dumps(diagnostics, sort_keys=True)

    assert diagnostics["entry_data"][CONF_HOST] == REDACTED
    assert diagnostics["entry_data"][CONF_PORT] == REDACTED
    assert diagnostics["entry_data"][CONF_ACCESS_TOKEN] == REDACTED
    assert diagnostics["entry_data"][CONF_HUB_ID] == REDACTED
    assert diagnostics["runtime"]["vehicle_count"] == 2
    assert diagnostics["runtime"]["transport"] == "local_poll"

    for private_value in (
        "sensitive-hub.example",
        "7443",
        "fixture-device-bearer",
        "must-never-persist",
        "hub-fixture",
        "vehicle-alpha",
        "vehicle-beta",
        "Fixture Alpha",
        "Fixture Beta",
        "fixture-event-2",
        "51.501",
        "-0.142",
    ):
        assert private_value not in serialized

    assert await hass.config_entries.async_unload(entry.entry_id) is True


async def test_diagnostics_before_first_refresh_returns_safe_empty_runtime(
    hass: HomeAssistant,
) -> None:
    """A failed initial refresh must still expose useful redacted diagnostics."""
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id="hub-fixture",
        data={
            CONF_HOST: "sensitive-hub.example",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "fixture-device-bearer",
        },
    )
    entry.add_to_hass(hass)
    coordinator = TeslatlasDataCoordinator(hass, entry, FixtureHubClient())
    entry.runtime_data = coordinator

    diagnostics = await async_get_config_entry_diagnostics(hass, entry)

    assert diagnostics["runtime"] == {
        "available": False,
        "transport": "local_poll",
        "protocol_version": None,
        "capabilities": [],
        "vehicle_count": 0,
        "current_read_failure_count": 0,
        "received_at": None,
    }
    await coordinator.async_shutdown()


async def test_diagnostics_without_runtime_data_returns_safe_empty_runtime(
    hass: HomeAssistant,
) -> None:
    """A setup-error entry still exposes redacted, unavailable diagnostics."""
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id="hub-fixture",
        data={
            CONF_HOST: "sensitive-hub.example",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "fixture-device-bearer",
        },
    )
    entry.add_to_hass(hass)

    diagnostics = await async_get_config_entry_diagnostics(hass, entry)

    assert diagnostics["runtime"] == {
        "available": False,
        "transport": "local_poll",
        "protocol_version": None,
        "capabilities": [],
        "vehicle_count": 0,
        "current_read_failure_count": 0,
        "received_at": None,
    }


async def test_diagnostics_after_outage_keeps_last_snapshot_but_marks_unavailable(
    hass: HomeAssistant,
) -> None:
    """An outage keeps aggregate context while reporting unavailable state."""
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id="hub-fixture",
        data={
            CONF_HOST: "sensitive-hub.example",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "fixture-device-bearer",
        },
    )
    entry.add_to_hass(hass)
    client = FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(hass, entry, client)
    coordinator.data = client.snapshot
    coordinator.last_update_success = False
    entry.runtime_data = coordinator

    diagnostics = await async_get_config_entry_diagnostics(hass, entry)

    assert diagnostics["runtime"]["available"] is False
    assert diagnostics["runtime"]["protocol_version"] == "0.0-fixture"
    assert diagnostics["runtime"]["vehicle_count"] == 2
    await coordinator.async_shutdown()


async def test_diagnostics_count_failed_current_reads_without_vehicle_identity(
    hass: HomeAssistant,
) -> None:
    entry = MockConfigEntry(domain=DOMAIN, data={})
    entry.add_to_hass(hass)
    client = FixtureHubClient()
    snapshot = client.snapshot
    failed = replace(snapshot.vehicles["vehicle-alpha"], current_read_failed=True)
    coordinator = TeslatlasDataCoordinator(hass, entry, client)
    coordinator.data = snapshot.with_vehicle(failed, snapshot.received_at)
    coordinator.last_update_success = True
    entry.runtime_data = coordinator

    diagnostics = await async_get_config_entry_diagnostics(hass, entry)
    assert diagnostics["runtime"]["available"] is True
    assert diagnostics["runtime"]["current_read_failure_count"] == 1
    assert diagnostics["runtime"]["vehicle_count"] == 2
    serialized = json.dumps(diagnostics)
    assert "vehicle-alpha" not in serialized
    assert "Fixture Alpha" not in serialized
    await coordinator.async_shutdown()

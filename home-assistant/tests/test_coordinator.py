"""Tests for single-authority current-Hub polling."""

from __future__ import annotations

from dataclasses import replace
from datetime import timedelta

import pytest
from homeassistant.config_entries import ConfigEntryAuthFailed, ConfigEntryState
from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant
from homeassistant.helpers.update_coordinator import UpdateFailed
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.client import HubConnectionError
from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
)
from custom_components.teslatlas_hub.coordinator import TeslatlasDataCoordinator
from tests.helpers import FixtureHubClient, initial_snapshot


def _entry(hass: HomeAssistant) -> MockConfigEntry:
    entry = MockConfigEntry(
        domain=DOMAIN,
        unique_id="hub-fixture",
        data={
            CONF_HOST: "hub-fixture.local",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "fixture-device-bearer",
        },
    )
    entry.add_to_hass(hass)
    entry.mock_state(hass, ConfigEntryState.SETUP_IN_PROGRESS)
    return entry


async def test_initial_refresh_starts_thirty_second_polling(
    hass: HomeAssistant,
) -> None:
    client = FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(hass, _entry(hass), client)
    await coordinator.async_config_entry_first_refresh()

    assert client.snapshot_calls == 1
    assert coordinator.update_interval == timedelta(seconds=30)
    assert coordinator.last_event_id is None

    await coordinator.async_shutdown()
    assert client.closed is True


async def test_outage_backoff_recovers_to_default_interval(
    hass: HomeAssistant,
) -> None:
    client = FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(hass, _entry(hass), client)
    client.snapshot_error = HubConnectionError("offline")

    with pytest.raises(UpdateFailed):
        await coordinator._async_update_data()
    assert coordinator.update_interval == timedelta(seconds=30)
    with pytest.raises(UpdateFailed):
        await coordinator._async_update_data()
    assert coordinator.update_interval == timedelta(seconds=60)

    client.snapshot_error = None
    await coordinator._async_update_data()
    assert coordinator.update_interval == timedelta(seconds=30)
    await coordinator.async_shutdown()


async def test_identity_change_is_authentication_failure_before_publish(
    hass: HomeAssistant,
) -> None:
    client = FixtureHubClient()
    client.snapshot = replace(
        initial_snapshot(),
        info=replace(initial_snapshot().info, hub_id="other-hub"),
    )
    coordinator = TeslatlasDataCoordinator(hass, _entry(hass), client)

    with pytest.raises(ConfigEntryAuthFailed, match="identity"):
        await coordinator._async_update_data()
    await coordinator.async_shutdown()


async def test_vehicle_removal_replaces_prior_mapping(
    hass: HomeAssistant,
) -> None:
    client = FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(hass, _entry(hass), client)
    await coordinator.async_config_entry_first_refresh()
    without_beta = initial_snapshot().create(
        info=initial_snapshot().info,
        status=initial_snapshot().status,
        vehicles=[initial_snapshot().vehicles["vehicle-alpha"]],
        received_at=initial_snapshot().received_at,
    )
    client.snapshot = without_beta

    coordinator.async_set_updated_data(await coordinator._async_update_data())
    assert tuple(coordinator.data.vehicles) == ("vehicle-alpha",)
    await coordinator.async_shutdown()

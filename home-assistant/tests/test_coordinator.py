"""Tests for push-only runtime coordination."""

from __future__ import annotations

import asyncio
from dataclasses import replace
from unittest.mock import patch

from homeassistant.config_entries import ConfigEntryState
from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.client import (
    HubAuthenticationError,
    HubConnectionError,
)
from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
)
from custom_components.teslatlas_hub.coordinator import TeslatlasDataCoordinator
from tests.helpers import FixtureHubClient, vehicle_update


class ImmediateSleeper:
    """Record reconnect delays without using wall-clock time."""

    def __init__(self) -> None:
        self.delays: list[int] = []

    async def __call__(self, delay: int) -> None:
        self.delays.append(delay)


def _entry(hass: HomeAssistant) -> MockConfigEntry:
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
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


async def _coordinator(
    hass: HomeAssistant,
    client: FixtureHubClient,
    sleeper: ImmediateSleeper | None = None,
) -> TeslatlasDataCoordinator:
    coordinator = TeslatlasDataCoordinator(
        hass,
        _entry(hass),
        client,
        sleep=sleeper or ImmediateSleeper(),
    )
    await coordinator.async_config_entry_first_refresh()
    return coordinator


async def test_initial_refresh_is_single_bounded_snapshot(
    hass: HomeAssistant,
) -> None:
    """Catch periodic provider-style polling in the local-push coordinator."""
    client = FixtureHubClient()
    coordinator = await _coordinator(hass, client)

    assert client.snapshot_calls == 1
    assert coordinator.update_interval is None
    assert coordinator.data.vehicles["vehicle-alpha"].state_of_charge == 72.5

    await coordinator.async_shutdown()


async def test_push_event_updates_state_and_replay_cursor(
    hass: HomeAssistant,
) -> None:
    """Catch event delivery that fails to advance state or replay identity."""
    client = FixtureHubClient()
    client.event_connections = [[vehicle_update()]]
    coordinator = await _coordinator(hass, client)

    coordinator.async_start()
    await asyncio.wait_for(client.stream_blocked.wait(), timeout=1)

    assert coordinator.data.vehicles["vehicle-alpha"].state_of_charge == 71.0
    assert coordinator.last_event_id == "fixture-event-2"
    assert client.event_cursors == [None]
    assert coordinator.last_update_success is True

    await coordinator.async_shutdown()
    assert client.closed is True


async def test_disconnect_marks_unavailable_then_recovers_on_event(
    hass: HomeAssistant,
) -> None:
    """Catch stale available state during reconnect or failed recovery."""
    client = FixtureHubClient()
    client.event_connections = [
        [HubConnectionError("offline")],
        [vehicle_update()],
    ]
    sleeper = ImmediateSleeper()
    coordinator = await _coordinator(hass, client, sleeper)
    update_states: list[bool] = []
    coordinator.async_add_listener(
        lambda: update_states.append(coordinator.last_update_success)
    )

    coordinator.async_start()
    await asyncio.wait_for(client.stream_blocked.wait(), timeout=1)

    assert sleeper.delays == [1]
    assert update_states == [False, True]
    assert coordinator.last_update_success is True
    assert coordinator.data.vehicles["vehicle-alpha"].state_of_charge == 71.0

    await coordinator.async_shutdown()


async def test_reconnect_reuses_last_event_id_and_caps_backoff(
    hass: HomeAssistant,
) -> None:
    """Catch lost replay continuity or unbounded reconnect delays."""
    client = FixtureHubClient()
    event = vehicle_update()
    client.event_connections = [
        [event, HubConnectionError("drop-1")],
        [HubConnectionError("drop-2")],
        [HubConnectionError("drop-3")],
        [HubConnectionError("drop-4")],
        [HubConnectionError("drop-5")],
        [HubConnectionError("drop-6")],
        [HubConnectionError("drop-7")],
        [],
    ]
    sleeper = ImmediateSleeper()
    coordinator = await _coordinator(hass, client, sleeper)

    coordinator.async_start()
    await asyncio.wait_for(client.stream_blocked.wait(), timeout=1)

    assert sleeper.delays == [1, 2, 4, 8, 16, 30, 30]
    assert client.event_cursors == [
        None,
        "fixture-event-2",
        "fixture-event-2",
        "fixture-event-2",
        "fixture-event-2",
        "fixture-event-2",
        "fixture-event-2",
        "fixture-event-2",
    ]

    await coordinator.async_shutdown()


async def test_authentication_loss_starts_one_reauth_and_stops_reconnect(
    hass: HomeAssistant,
) -> None:
    """Catch expired bearer loops that never ask the user to repair access."""
    client = FixtureHubClient()
    client.event_connections = [[HubAuthenticationError("expired")]]
    sleeper = ImmediateSleeper()
    coordinator = await _coordinator(hass, client, sleeper)

    with patch.object(coordinator.config_entry, "async_start_reauth") as start_reauth:
        task = coordinator.async_start()
        await asyncio.wait_for(task, timeout=1)

    start_reauth.assert_called_once_with(hass)
    assert coordinator.last_update_success is False
    assert sleeper.delays == []
    assert client.event_cursors == [None]

    await coordinator.async_shutdown()


async def test_newer_event_replaces_only_target_vehicle(
    hass: HomeAssistant,
) -> None:
    """Catch cross-vehicle state loss while consuming multiple events."""
    client = FixtureHubClient()
    second = replace(
        vehicle_update(),
        event_id="fixture-event-3",
        vehicle=replace(vehicle_update().vehicle, state_of_charge=70.0),
    )
    client.event_connections = [[vehicle_update(), second]]
    coordinator = await _coordinator(hass, client)

    coordinator.async_start()
    await asyncio.wait_for(client.stream_blocked.wait(), timeout=1)

    assert coordinator.data.vehicles["vehicle-alpha"].state_of_charge == 70.0
    assert coordinator.data.vehicles["vehicle-beta"].state_of_charge == 44.0
    assert coordinator.last_event_id == "fixture-event-3"

    await coordinator.async_shutdown()

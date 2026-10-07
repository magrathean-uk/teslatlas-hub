"""Tests for single-authority current-Hub polling."""

from __future__ import annotations

import asyncio
from dataclasses import replace
from datetime import UTC, datetime, timedelta
from unittest.mock import patch

import pytest
from homeassistant.config_entries import ConfigEntryAuthFailed, ConfigEntryState
from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant
from homeassistant.helpers.update_coordinator import UpdateFailed
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.client import HubConnectionError
from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_DEVICE_ID,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_TOKEN_EXPIRES_AT_MS,
    CONF_USE_TLS,
    DOMAIN,
)
from custom_components.teslatlas_hub.coordinator import TeslatlasDataCoordinator
from tests.helpers import (
    FIXTURE_ACCESS_TOKEN,
    FIXTURE_DEVICE_ID,
    FIXTURE_ROTATED_ACCESS_TOKEN,
    FixtureHubClient,
    initial_snapshot,
)


def _entry(hass: HomeAssistant) -> MockConfigEntry:
    entry = MockConfigEntry(
        domain=DOMAIN,
        unique_id="hub-fixture",
        data={
            CONF_HOST: "hub-fixture.local",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_DEVICE_ID: FIXTURE_DEVICE_ID,
            CONF_ACCESS_TOKEN: FIXTURE_ACCESS_TOKEN,
        },
    )
    entry.add_to_hass(hass)
    entry.mock_state(hass, ConfigEntryState.SETUP_IN_PROGRESS)
    return entry


def _set_expiry(
    hass: HomeAssistant, entry: MockConfigEntry, expires_at: datetime
) -> None:
    hass.config_entries.async_update_entry(
        entry,
        data={
            **entry.data,
            CONF_TOKEN_EXPIRES_AT_MS: int(expires_at.timestamp() * 1000),
        },
    )


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


async def test_due_bearer_rotation_persists_before_switching_client(
    hass: HomeAssistant,
) -> None:
    now = datetime(2026, 9, 26, tzinfo=UTC)
    entry = _entry(hass)
    _set_expiry(hass, entry, now + timedelta(days=7))
    client = FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(hass, entry, client, now=lambda: now)

    original_update = hass.config_entries.async_update_entry

    def assert_persisted_before_switch(*args: object, **kwargs: object) -> None:
        assert client.bearer_updates == []
        original_update(*args, **kwargs)

    with patch.object(
        hass.config_entries,
        "async_update_entry",
        side_effect=assert_persisted_before_switch,
    ):
        await coordinator._async_update_data()

    assert client.rotation_calls == 1
    assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ROTATED_ACCESS_TOKEN
    assert client.bearer_updates == [(FIXTURE_ROTATED_ACCESS_TOKEN, 2_000_000_000_000)]
    await coordinator.async_shutdown()


async def test_rotation_failure_keeps_existing_credential(
    hass: HomeAssistant,
) -> None:
    now = datetime(2026, 9, 26, tzinfo=UTC)
    entry = _entry(hass)
    _set_expiry(hass, entry, now + timedelta(days=6))
    client = FixtureHubClient()
    client.rotation_error = HubConnectionError("lost response")
    coordinator = TeslatlasDataCoordinator(hass, entry, client, now=lambda: now)

    with pytest.raises(UpdateFailed, match="lost response"):
        await coordinator._async_update_data()

    assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
    assert client.bearer_updates == []
    await coordinator.async_shutdown()


async def test_rotation_rejects_another_paired_device(
    hass: HomeAssistant,
) -> None:
    now = datetime(2026, 9, 26, tzinfo=UTC)
    entry = _entry(hass)
    _set_expiry(hass, entry, now + timedelta(days=6))
    client = FixtureHubClient()
    client.rotated_device_id = "another-device"
    coordinator = TeslatlasDataCoordinator(hass, entry, client, now=lambda: now)

    with pytest.raises(ConfigEntryAuthFailed, match="authentication expired"):
        await coordinator._async_update_data()

    assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
    assert client.bearer_updates == []
    await coordinator.async_shutdown()


async def test_rotation_rejects_out_of_range_expiry_before_persisting(
    hass: HomeAssistant,
) -> None:
    now = datetime(2026, 9, 26, tzinfo=UTC)
    entry = _entry(hass)
    _set_expiry(hass, entry, now + timedelta(days=6))
    client = FixtureHubClient()
    client.rotation_expires_at_ms = 10**100
    coordinator = TeslatlasDataCoordinator(hass, entry, client, now=lambda: now)

    with pytest.raises(UpdateFailed, match="signed64"):
        await coordinator._async_update_data()

    assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
    assert client.bearer_updates == []
    await coordinator.async_shutdown()


@pytest.mark.parametrize("expiry", [0, 1_790_380_800_000])
async def test_rotation_rejects_expired_issued_credential_before_persistence(
    hass: HomeAssistant, expiry: int
) -> None:
    now = datetime(2026, 9, 26, tzinfo=UTC)
    entry = _entry(hass)
    _set_expiry(hass, entry, now + timedelta(days=6))
    client = FixtureHubClient()
    client.rotation_expires_at_ms = expiry
    coordinator = TeslatlasDataCoordinator(hass, entry, client, now=lambda: now)

    with pytest.raises(UpdateFailed, match="future"):
        await coordinator._async_update_data()
    assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
    assert client.bearer_updates == []
    await coordinator.async_shutdown()


async def test_rotation_rejects_control_character_token_before_persistence(
    hass: HomeAssistant,
) -> None:
    now = datetime(2026, 9, 26, tzinfo=UTC)
    entry = _entry(hass)
    _set_expiry(hass, entry, now + timedelta(days=6))
    client = FixtureHubClient()
    client.rotated_access_token = "a" * 62 + "\r\n"
    coordinator = TeslatlasDataCoordinator(hass, entry, client, now=lambda: now)

    with pytest.raises(UpdateFailed, match="access_token"):
        await coordinator._async_update_data()
    assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
    assert client.bearer_updates == []
    await coordinator.async_shutdown()


async def test_signed64_maximum_expiry_schedules_without_datetime_conversion(
    hass: HomeAssistant,
) -> None:
    entry = _entry(hass)
    hass.config_entries.async_update_entry(
        entry, data={**entry.data, CONF_TOKEN_EXPIRES_AT_MS: 2**63 - 1}
    )
    client = FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(hass, entry, client)
    await coordinator._async_update_data()
    assert client.rotation_calls == 0
    assert client.snapshot_calls == 1
    await coordinator.async_shutdown()


async def test_rotated_signed64_maximum_expiry_is_persisted_without_narrowing(
    hass: HomeAssistant,
) -> None:
    now = datetime(2026, 9, 26, tzinfo=UTC)
    entry = _entry(hass)
    _set_expiry(hass, entry, now + timedelta(days=6))
    client = FixtureHubClient()
    client.rotation_expires_at_ms = 2**63 - 1
    coordinator = TeslatlasDataCoordinator(hass, entry, client, now=lambda: now)
    await coordinator._async_update_data()
    assert entry.data[CONF_TOKEN_EXPIRES_AT_MS] == 2**63 - 1
    assert client.bearer_updates == [(FIXTURE_ROTATED_ACCESS_TOKEN, 2**63 - 1)]
    await coordinator.async_shutdown()


async def test_cancelled_shutdown_waiter_rejoins_client_cleanup(
    hass: HomeAssistant,
) -> None:
    client = FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(hass, _entry(hass), client)
    cleanup_started = asyncio.Event()
    finish_cleanup = asyncio.Event()
    original_close = client.async_close

    async def blocked_close() -> None:
        cleanup_started.set()
        await finish_cleanup.wait()
        await original_close()

    with patch.object(client, "async_close", side_effect=blocked_close) as close:
        first = asyncio.create_task(coordinator.async_shutdown())
        await asyncio.wait_for(cleanup_started.wait(), timeout=1)
        first.cancel()
        with pytest.raises(asyncio.CancelledError):
            await first
        retry = asyncio.create_task(coordinator.async_shutdown())
        await asyncio.sleep(0)
        assert not retry.done()
        assert client.closed is False
        finish_cleanup.set()
        await asyncio.wait_for(retry, timeout=1)
        await coordinator.async_shutdown()
        close.assert_awaited_once()
    assert client.closed is True


async def test_due_rotation_is_not_attempted_concurrently(
    hass: HomeAssistant,
) -> None:
    now = datetime(2026, 9, 26, tzinfo=UTC)
    entry = _entry(hass)
    _set_expiry(hass, entry, now + timedelta(days=6))
    client = FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(hass, entry, client, now=lambda: now)
    rotation_started = asyncio.Event()
    finish_rotation = asyncio.Event()
    original_rotate = client.async_rotate

    async def blocked_rotate() -> object:
        rotation_started.set()
        await finish_rotation.wait()
        return await original_rotate()

    with patch.object(client, "async_rotate", side_effect=blocked_rotate):
        first = asyncio.create_task(coordinator._async_update_data())
        await rotation_started.wait()
        second = asyncio.create_task(coordinator._async_update_data())
        await asyncio.sleep(0)
        assert client.rotation_calls == 0
        finish_rotation.set()
        await asyncio.gather(first, second)

    assert client.rotation_calls == 1
    await coordinator.async_shutdown()


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


async def test_outage_backoff_reaches_and_caps_at_five_minutes(
    hass: HomeAssistant,
) -> None:
    """Keep transient polling backoff bounded at the declared five-minute cap."""
    client = FixtureHubClient()
    coordinator = TeslatlasDataCoordinator(hass, _entry(hass), client)
    client.snapshot_error = HubConnectionError("offline")

    for expected_seconds in (30, 60, 120, 300, 300):
        with pytest.raises(UpdateFailed):
            await coordinator._async_update_data()
        assert coordinator.update_interval == timedelta(seconds=expected_seconds)

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

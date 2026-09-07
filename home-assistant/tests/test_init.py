"""Tests for config-entry runtime setup and cleanup."""

from __future__ import annotations

import asyncio
from dataclasses import replace
from unittest.mock import AsyncMock, patch

import pytest
from aiohttp import web
from homeassistant.config_entries import SOURCE_REAUTH, ConfigEntryState
from homeassistant.const import CONF_HOST, Platform
from homeassistant.core import HomeAssistant
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.client import (
    HubAuthenticationError,
    HubConnectionError,
    ProtocolContractUnavailable,
)
from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
)
from custom_components.teslatlas_hub.current_hub_client import CurrentHubClient
from custom_components.teslatlas_hub.models import HubEndpoint
from tests.helpers import FixtureHubClient


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
    return entry


async def test_setup_loads_polling_coordinator_and_unloads_cleanly(
    hass: HomeAssistant,
) -> None:
    """Catch setup that skips platforms or leaks polling resources."""
    entry = _entry(hass)
    client = FixtureHubClient()
    forward = AsyncMock()
    unload = AsyncMock(return_value=True)

    with (
        patch(
            "custom_components.teslatlas_hub.create_client",
            return_value=client,
        ) as factory,
        patch.object(hass.config_entries, "async_forward_entry_setups", forward),
        patch.object(hass.config_entries, "async_unload_platforms", unload),
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is True
        assert entry.state is ConfigEntryState.LOADED
        assert entry.runtime_data.data.info.hub_id == "hub-fixture"
        assert client.snapshot_calls == 1
        factory.assert_called_once_with(
            HubEndpoint(host="hub-fixture.local", port=7443, use_tls=True),
            bearer_token="fixture-device-bearer",
            expected_hub_id="hub-fixture",
            bearer_expires_at_ms=None,
            hass=hass,
        )
        forward.assert_awaited_once_with(entry, (Platform.SENSOR,))

        assert await hass.config_entries.async_unload(entry.entry_id) is True

    unload.assert_awaited_once_with(entry, (Platform.SENSOR,))
    assert client.closed is True
    assert entry.state is ConfigEntryState.NOT_LOADED


async def test_reload_replaces_client_and_closes_previous_runtime(
    hass: HomeAssistant,
) -> None:
    """Catch reloads that retain the prior session or polling schedule."""
    entry = _entry(hass)
    first_client = FixtureHubClient()
    second_client = FixtureHubClient()

    with patch(
        "custom_components.teslatlas_hub.create_client",
        side_effect=(first_client, second_client),
    ) as factory:
        assert await hass.config_entries.async_setup(entry.entry_id) is True
        first_runtime = entry.runtime_data

        await hass.config_entries.async_reload(entry.entry_id)
        await hass.async_block_till_done()

        assert entry.state is ConfigEntryState.LOADED
        assert entry.runtime_data is not first_runtime
        assert first_client.closed is True
        assert second_client.closed is False
        assert first_client.snapshot_calls == 1
        assert second_client.snapshot_calls == 1
        assert factory.call_count == 2

        assert await hass.config_entries.async_unload(entry.entry_id) is True

    assert second_client.closed is True


async def test_reload_cancels_blocked_production_poll_before_new_client_starts(
    hass: HomeAssistant,
    aiohttp_server,
    socket_enabled,
) -> None:
    """Catch reload allowing an old credential-bearing poll to outlive unload."""
    del socket_enabled
    hub_id = "11111111-1111-4111-8111-111111111111"
    vehicle_id = "22222222-2222-4222-8222-222222222222"
    current_requests = 0
    blocked = asyncio.Event()
    blocker = asyncio.Event()

    async def discovery(_request: web.Request) -> web.Response:
        return web.json_response(
            {
                "hub_id": hub_id,
                "protocol": "teslatlas-sync",
                "protocol_major": 1,
                "api_versions": ["1.0"],
                "capabilities": ["query.vehicles", "query.current"],
            }
        )

    async def vehicles(_request: web.Request) -> web.Response:
        return web.json_response(
            {"vehicles": [{"vehicle_id": vehicle_id, "display_name": "Atlas"}]}
        )

    async def current(_request: web.Request) -> web.Response:
        nonlocal current_requests
        current_requests += 1
        if current_requests > 1:
            blocked.set()
            await blocker.wait()
        return web.json_response(
            {
                "vehicle_id": vehicle_id,
                "observed_at_ms": None,
                "battery_level": 50,
            }
        )

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    app.router.add_get("/v1/vehicles/{vehicle_id}/current", current)
    server = await aiohttp_server(app)
    endpoint = HubEndpoint(host=server.host, port=server.port, use_tls=False)
    old_client = CurrentHubClient(
        async_get_clientsession(hass),
        endpoint,
        bearer_token="a" * 64,
        expected_hub_id=hub_id,
    )
    new_client = FixtureHubClient()
    new_client.snapshot = replace(
        new_client.snapshot,
        info=replace(new_client.snapshot.info, hub_id=hub_id),
    )
    new_client.info = new_client.snapshot.info
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id=hub_id,
        data={
            CONF_HOST: server.host,
            CONF_PORT: server.port,
            CONF_USE_TLS: False,
            CONF_HUB_ID: hub_id,
            CONF_ACCESS_TOKEN: "a" * 64,
        },
    )
    entry.add_to_hass(hass)

    with patch(
        "custom_components.teslatlas_hub.create_client",
        side_effect=(old_client, new_client),
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is True
        refresh = asyncio.create_task(entry.runtime_data.async_refresh())
        await asyncio.wait_for(blocked.wait(), timeout=1)

        await hass.config_entries.async_reload(entry.entry_id)
        await hass.async_block_till_done()
        blocker.set()

        with pytest.raises(asyncio.CancelledError):
            await refresh
        assert refresh.done()
        assert refresh.cancelled()
        assert entry.state is ConfigEntryState.LOADED
        assert entry.runtime_data.client is new_client
        assert current_requests == 2
        assert await hass.config_entries.async_unload(entry.entry_id) is True


@pytest.mark.parametrize(
    "error",
    [
        HubConnectionError("offline"),
        ProtocolContractUnavailable("public protocol unavailable"),
    ],
)
async def test_setup_retries_transient_or_pending_protocol_failure(
    hass: HomeAssistant,
    error: Exception,
) -> None:
    """Catch setup failures that require a Home Assistant restart."""
    entry = _entry(hass)
    client = FixtureHubClient()
    client.snapshot_error = error

    with patch(
        "custom_components.teslatlas_hub.create_client",
        return_value=client,
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is False

    assert entry.state is ConfigEntryState.SETUP_RETRY
    assert client.closed is True


async def test_setup_authentication_failure_starts_reauth(
    hass: HomeAssistant,
) -> None:
    """Catch expired bearers that loop setup without user repair."""
    entry = _entry(hass)
    client = FixtureHubClient()
    client.snapshot_error = HubAuthenticationError("expired")

    with patch(
        "custom_components.teslatlas_hub.create_client",
        return_value=client,
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is False
        await hass.async_block_till_done()

    assert client.closed is True
    assert entry.state is ConfigEntryState.SETUP_ERROR
    assert any(
        flow["context"]["source"] == SOURCE_REAUTH
        for flow in hass.config_entries.flow.async_progress_by_handler(DOMAIN)
    )

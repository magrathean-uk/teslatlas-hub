"""Tests for config-entry runtime setup and cleanup."""

from __future__ import annotations

from unittest.mock import AsyncMock, patch

import pytest
from homeassistant.config_entries import SOURCE_REAUTH, ConfigEntryState
from homeassistant.const import CONF_HOST, Platform
from homeassistant.core import HomeAssistant
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


async def test_setup_loads_once_starts_push_and_unloads_cleanly(
    hass: HomeAssistant,
) -> None:
    """Catch setup that polls, skips platforms, or leaks the public client."""
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
        )
        forward.assert_awaited_once_with(entry, (Platform.SENSOR,))

        assert await hass.config_entries.async_unload(entry.entry_id) is True

    unload.assert_awaited_once_with(entry, (Platform.SENSOR,))
    assert client.closed is True
    assert entry.state is ConfigEntryState.NOT_LOADED


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

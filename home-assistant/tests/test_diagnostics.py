"""Tests for privacy-preserving diagnostics."""

from __future__ import annotations

import asyncio
import json
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
from custom_components.teslatlas_hub.diagnostics import (
    async_get_config_entry_diagnostics,
)
from tests.helpers import FixtureHubClient, vehicle_update


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
    client.event_connections = [[vehicle_update()]]

    with patch(
        "custom_components.teslatlas_hub.create_client",
        return_value=client,
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is True
    await asyncio.wait_for(client.stream_blocked.wait(), timeout=1)

    diagnostics = await async_get_config_entry_diagnostics(hass, entry)
    serialized = json.dumps(diagnostics, sort_keys=True)

    assert diagnostics["entry_data"][CONF_HOST] == REDACTED
    assert diagnostics["entry_data"][CONF_PORT] == REDACTED
    assert diagnostics["entry_data"][CONF_ACCESS_TOKEN] == REDACTED
    assert diagnostics["entry_data"][CONF_HUB_ID] == REDACTED
    assert diagnostics["runtime"]["vehicle_count"] == 2
    assert diagnostics["runtime"]["data_quality_counts"] == {
        "complete": 1,
        "partial": 1,
    }
    assert diagnostics["runtime"]["last_event_id_present"] is True

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

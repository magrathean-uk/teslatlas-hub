"""Tests for config-entry schema migration."""

from __future__ import annotations

from unittest.mock import patch

from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub import async_migrate_entry
from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PORT,
    CONF_USE_TLS,
    DOMAIN,
)
from tests.helpers import FixtureHubClient


async def test_minor_zero_entry_advances_without_invented_data_changes(
    hass: HomeAssistant,
) -> None:
    """Catch migration code that fabricates a legacy transport schema."""
    original_data = {CONF_HUB_ID: "hub-fixture", "opaque": "preserved"}
    entry = MockConfigEntry(
        domain=DOMAIN,
        version=1,
        minor_version=0,
        data=original_data,
    )
    entry.add_to_hass(hass)

    result = await async_migrate_entry(hass, entry)

    assert result is True
    assert entry.version == 1
    assert entry.minor_version == 2
    assert entry.data == original_data


async def test_future_major_entry_fails_closed(hass: HomeAssistant) -> None:
    """Catch destructive downgrade of an unknown future entry schema."""
    entry = MockConfigEntry(
        domain=DOMAIN,
        version=2,
        minor_version=0,
        data={CONF_HUB_ID: "hub-fixture"},
    )
    entry.add_to_hass(hass)

    result = await async_migrate_entry(hass, entry)

    assert result is False
    assert entry.version == 2
    assert entry.minor_version == 0


async def test_future_minor_entry_is_preserved_without_downgrade(
    hass: HomeAssistant,
) -> None:
    """Preserve a newer minor schema until this component knows how to read it."""
    original_data = {CONF_HUB_ID: "hub-fixture", "opaque": "preserved"}
    entry = MockConfigEntry(
        domain=DOMAIN,
        version=1,
        minor_version=99,
        data=original_data,
    )
    entry.add_to_hass(hass)

    result = await async_migrate_entry(hass, entry)

    assert result is True
    assert entry.version == 1
    assert entry.minor_version == 99
    assert entry.data == original_data


async def test_older_complete_entry_migrates_and_loads_without_losing_unknown_data(
    hass: HomeAssistant,
) -> None:
    """Exercise metadata migration together with real setup admission."""
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id="hub-fixture",
        version=1,
        minor_version=0,
        data={
            CONF_HOST: "hub-fixture.local",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "fixture-device-bearer",
            "opaque": "preserved",
        },
    )
    entry.add_to_hass(hass)
    client = FixtureHubClient()

    with patch(
        "custom_components.teslatlas_hub.create_client",
        return_value=client,
    ):
        assert await hass.config_entries.async_setup(entry.entry_id) is True

    assert entry.version == 1
    assert entry.minor_version == 2
    assert entry.data["opaque"] == "preserved"
    assert await hass.config_entries.async_unload(entry.entry_id) is True

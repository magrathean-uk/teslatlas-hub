"""Tests for config-entry schema migration."""

from __future__ import annotations

from homeassistant.core import HomeAssistant
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub import async_migrate_entry
from custom_components.teslatlas_hub.const import CONF_HUB_ID, DOMAIN


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
    assert entry.minor_version == 1
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

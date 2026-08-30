"""Teslatlas Hub Home Assistant integration."""

from __future__ import annotations

from typing import TYPE_CHECKING

from homeassistant.config_entries import ConfigEntry
from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant

from .client import create_client
from .const import (
    CONF_ACCESS_TOKEN,
    CONF_PORT,
    CONF_USE_TLS,
    CONFIG_ENTRY_MINOR_VERSION,
    CONFIG_ENTRY_VERSION,
    PLATFORMS,
)
from .coordinator import TeslatlasDataCoordinator
from .models import HubEndpoint

if TYPE_CHECKING:
    from homeassistant.helpers.typing import ConfigType

type TeslatlasConfigEntry = ConfigEntry[TeslatlasDataCoordinator]


async def async_setup(hass: HomeAssistant, config: ConfigType) -> bool:
    """Set up the config-entry-only integration."""
    return True


async def async_setup_entry(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
) -> bool:
    """Validate one Hub, create entities, then start local push."""
    endpoint = HubEndpoint(
        host=entry.data[CONF_HOST],
        port=entry.data[CONF_PORT],
        use_tls=entry.data[CONF_USE_TLS],
    )
    client = create_client(
        endpoint,
        bearer_token=entry.data[CONF_ACCESS_TOKEN],
    )
    coordinator = TeslatlasDataCoordinator(hass, entry, client)
    entry.runtime_data = coordinator
    try:
        await coordinator.async_config_entry_first_refresh()
        await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
    except Exception:
        await coordinator.async_shutdown()
        raise
    coordinator.async_start()
    return True


async def async_unload_entry(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
) -> bool:
    """Unload entities and release the event stream."""
    if unload_ok := await hass.config_entries.async_unload_platforms(entry, PLATFORMS):
        await entry.runtime_data.async_shutdown()
    return unload_ok


async def async_migrate_entry(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
) -> bool:
    """Advance known entry metadata without inventing a legacy wire schema."""
    if entry.version != CONFIG_ENTRY_VERSION:
        return False
    if entry.minor_version < CONFIG_ENTRY_MINOR_VERSION:
        hass.config_entries.async_update_entry(
            entry,
            version=CONFIG_ENTRY_VERSION,
            minor_version=CONFIG_ENTRY_MINOR_VERSION,
        )
    return True

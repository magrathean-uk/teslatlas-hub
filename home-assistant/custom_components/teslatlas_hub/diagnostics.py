"""Privacy-preserving diagnostics for Teslatlas Hub."""

from __future__ import annotations

from typing import Any

from homeassistant.const import CONF_HOST, CONF_LATITUDE, CONF_LONGITUDE
from homeassistant.core import HomeAssistant
from homeassistant.helpers.redact import async_redact_data

from . import TeslatlasConfigEntry
from .const import (
    CONF_ACCESS_TOKEN,
    CONF_DEVICE_ID,
    CONF_HUB_ID,
    CONF_PAIRING_SECRET,
    CONF_PORT,
    CONF_TLS_PIN,
)

TO_REDACT = {
    CONF_ACCESS_TOKEN,
    CONF_HOST,
    CONF_HUB_ID,
    CONF_LATITUDE,
    CONF_LONGITUDE,
    CONF_PAIRING_SECRET,
    CONF_PORT,
    CONF_TLS_PIN,
    CONF_DEVICE_ID,
}


async def async_get_config_entry_diagnostics(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
) -> dict[str, Any]:
    """Return useful aggregate state without client or vehicle identity."""
    coordinator = entry.runtime_data
    snapshot = coordinator.data
    return {
        "entry_data": async_redact_data(dict(entry.data), TO_REDACT),
        "entry_version": {
            "major": entry.version,
            "minor": entry.minor_version,
        },
        "runtime": {
            "available": coordinator.last_update_success,
            "protocol_version": snapshot.info.protocol_version,
            "capabilities": sorted(snapshot.info.capabilities),
            "vehicle_count": len(snapshot.vehicles),
            "transport": "local_poll",
            "received_at": snapshot.received_at.isoformat(),
        },
    }

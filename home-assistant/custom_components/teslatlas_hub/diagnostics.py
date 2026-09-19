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
    runtime: dict[str, Any] = {
        "available": False,
        "transport": "local_poll",
    }
    coordinator = getattr(entry, "runtime_data", None)
    snapshot = coordinator.data if coordinator is not None else None
    if snapshot is not None:
        runtime.update(
            {
                "available": coordinator.last_update_success,
                "protocol_version": snapshot.info.protocol_version,
                "capabilities": sorted(snapshot.info.capabilities),
                "vehicle_count": len(snapshot.vehicles),
                "received_at": snapshot.received_at.isoformat(),
            }
        )
    else:
        runtime.update(
            {
                "protocol_version": None,
                "capabilities": [],
                "vehicle_count": 0,
                "received_at": None,
            }
        )
    return {
        "entry_data": async_redact_data(dict(entry.data), TO_REDACT),
        "entry_version": {
            "major": entry.version,
            "minor": entry.minor_version,
        },
        "runtime": runtime,
    }

"""Privacy-preserving diagnostics for Teslatlas Hub."""

from __future__ import annotations

from collections import Counter
from typing import Any

from homeassistant.const import CONF_HOST, CONF_LATITUDE, CONF_LONGITUDE
from homeassistant.core import HomeAssistant
from homeassistant.helpers.redact import async_redact_data

from . import TeslatlasConfigEntry
from .const import (
    CONF_ACCESS_TOKEN,
    CONF_HUB_ID,
    CONF_PAIRING_SECRET,
    CONF_PORT,
)

TO_REDACT = {
    CONF_ACCESS_TOKEN,
    CONF_HOST,
    CONF_HUB_ID,
    CONF_LATITUDE,
    CONF_LONGITUDE,
    CONF_PAIRING_SECRET,
    CONF_PORT,
}


async def async_get_config_entry_diagnostics(
    hass: HomeAssistant,
    entry: TeslatlasConfigEntry,
) -> dict[str, Any]:
    """Return useful aggregate state without client or vehicle identity."""
    coordinator = entry.runtime_data
    snapshot = coordinator.data
    quality_counts = Counter(
        vehicle.data_quality or "unknown" for vehicle in snapshot.vehicles.values()
    )

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
            "collector_health": snapshot.status.collector_health,
            "vehicle_count": len(snapshot.vehicles),
            "data_quality_counts": dict(sorted(quality_counts.items())),
            "last_event_id_present": coordinator.last_event_id is not None,
            "received_at": snapshot.received_at.isoformat(),
        },
    }

"""Single-authority polling coordination for Teslatlas Hub."""

from __future__ import annotations

import logging
from datetime import timedelta
from typing import override

from homeassistant.config_entries import ConfigEntry, ConfigEntryAuthFailed
from homeassistant.core import HomeAssistant
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed

from .client import (
    HubAuthenticationError,
    HubConnectionError,
    HubContractError,
    TeslatlasHubClient,
)
from .const import CONF_HUB_ID, DOMAIN
from .models import HubSnapshot

_LOGGER = logging.getLogger(__name__)

POLL_INTERVAL_SECONDS = 30
POLL_BACKOFF_SECONDS = (30, 60, 120, 300)


class TeslatlasDataCoordinator(DataUpdateCoordinator[HubSnapshot]):
    """Own the only refresh schedule for one config entry."""

    def __init__(
        self,
        hass: HomeAssistant,
        entry: ConfigEntry,
        client: TeslatlasHubClient,
    ) -> None:
        """Initialize a non-overlapping 30-second polling coordinator."""
        super().__init__(
            hass,
            _LOGGER,
            config_entry=entry,
            name=DOMAIN,
            update_interval=timedelta(seconds=POLL_INTERVAL_SECONDS),
            always_update=False,
        )
        self.client = client
        self._failure_count = 0
        self._closed = False

    @property
    def last_event_id(self) -> None:
        """Confirm that this profile has no event replay cursor."""
        return None

    @override
    async def _async_update_data(self) -> HubSnapshot:
        """Poll one bounded set of independently observed vehicle states."""
        try:
            snapshot = await self.client.async_snapshot()
        except HubAuthenticationError as err:
            raise ConfigEntryAuthFailed("Teslatlas Hub authentication expired") from err
        except (HubConnectionError, HubContractError) as err:
            delay = POLL_BACKOFF_SECONDS[
                min(self._failure_count, len(POLL_BACKOFF_SECONDS) - 1)
            ]
            self._failure_count += 1
            self.update_interval = timedelta(seconds=delay)
            raise UpdateFailed(f"Teslatlas Hub is unavailable: {err}") from err

        if snapshot.info.hub_id != self.config_entry.data[CONF_HUB_ID]:
            raise ConfigEntryAuthFailed("Teslatlas Hub identity changed")
        self._failure_count = 0
        self.update_interval = timedelta(seconds=POLL_INTERVAL_SECONDS)
        return snapshot

    @override
    async def async_shutdown(self) -> None:
        """Cancel coordinator timers and close the client exactly once."""
        if self._closed:
            return
        self._closed = True
        await super().async_shutdown()
        await self.client.async_close()

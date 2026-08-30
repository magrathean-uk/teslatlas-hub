"""Push-only runtime coordination for Teslatlas Hub."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Awaitable, Callable
from contextlib import suppress
from typing import override

from homeassistant.config_entries import ConfigEntry, ConfigEntryAuthFailed
from homeassistant.core import HomeAssistant
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed

from .client import (
    HubAuthenticationError,
    HubConnectionError,
    ProtocolContractUnavailable,
    TeslatlasHubClient,
)
from .const import CONF_HUB_ID, DOMAIN
from .models import HubSnapshot

_LOGGER = logging.getLogger(__name__)

RECONNECT_DELAYS = (1, 2, 4, 8, 16, 30)

type Sleep = Callable[[int], Awaitable[None]]


class TeslatlasDataCoordinator(DataUpdateCoordinator[HubSnapshot]):
    """Own one initial query and the replayable public event stream."""

    def __init__(
        self,
        hass: HomeAssistant,
        entry: ConfigEntry,
        client: TeslatlasHubClient,
        *,
        sleep: Sleep = asyncio.sleep,
    ) -> None:
        """Initialize a coordinator with no polling interval."""
        super().__init__(
            hass,
            _LOGGER,
            config_entry=entry,
            name=DOMAIN,
            update_interval=None,
            always_update=False,
        )
        self.client = client
        self._sleep = sleep
        self._stream_task: asyncio.Task[None] | None = None
        self._last_event_id: str | None = None
        self._closed = False

    @property
    def last_event_id(self) -> str | None:
        """Return the in-memory replay cursor without exposing it as diagnostics."""
        return self._last_event_id

    @override
    async def _async_update_data(self) -> HubSnapshot:
        """Load the single bounded snapshot required before push starts."""
        try:
            snapshot = await self.client.async_snapshot()
        except HubAuthenticationError as err:
            raise ConfigEntryAuthFailed("Teslatlas Hub authentication expired") from err
        except (HubConnectionError, ProtocolContractUnavailable) as err:
            raise UpdateFailed(f"Teslatlas Hub is unavailable: {err}") from err

        if snapshot.info.hub_id != self.config_entry.data[CONF_HUB_ID]:
            raise ConfigEntryAuthFailed("Teslatlas Hub identity changed")
        return snapshot

    def async_start(self) -> asyncio.Task[None]:
        """Start the config-entry-owned event task once."""
        if self._stream_task is not None and not self._stream_task.done():
            return self._stream_task
        self._stream_task = self.config_entry.async_create_background_task(
            self.hass,
            self._async_stream(),
            f"{DOMAIN} event stream",
        )
        return self._stream_task

    async def _async_stream(self) -> None:
        """Consume events and reconnect with bounded exponential delay."""
        reconnect_attempt = 0
        while True:
            try:
                async for event in self.client.async_events(self._last_event_id):
                    reconnect_attempt = 0
                    self._last_event_id = event.event_id
                    self.async_set_updated_data(event.apply(self.data))
                raise HubConnectionError("event stream ended")
            except asyncio.CancelledError:
                raise
            except HubAuthenticationError as err:
                self.async_set_update_error(
                    UpdateFailed(f"Teslatlas Hub authentication expired: {err}")
                )
                self.config_entry.async_start_reauth(self.hass)
                return
            except (HubConnectionError, ProtocolContractUnavailable) as err:
                self.async_set_update_error(
                    UpdateFailed(f"Teslatlas Hub event stream unavailable: {err}")
                )
                delay = RECONNECT_DELAYS[
                    min(reconnect_attempt, len(RECONNECT_DELAYS) - 1)
                ]
                reconnect_attempt += 1
                await self._sleep(delay)

    @override
    async def async_shutdown(self) -> None:
        """Cancel the stream and close the public client exactly once."""
        if self._closed:
            return
        self._closed = True
        task = self._stream_task
        if task is not None and not task.done():
            task.cancel()
            with suppress(asyncio.CancelledError):
                await task
        await self.client.async_close()
        await super().async_shutdown()

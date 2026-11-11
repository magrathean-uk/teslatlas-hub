"""Single-authority polling coordination for Teslatlas Hub."""

from __future__ import annotations

import asyncio
import logging
from collections.abc import Callable
from datetime import UTC, datetime, timedelta
from typing import Any, override

from homeassistant.config_entries import ConfigEntry, ConfigEntryAuthFailed
from homeassistant.core import HomeAssistant
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed

from .client import (
    HubAuthenticationError,
    HubConnectionError,
    HubContractError,
    TeslatlasHubClient,
)
from .const import (
    CONF_ACCESS_TOKEN,
    CONF_DEVICE_ID,
    CONF_HUB_ID,
    CONF_TOKEN_EXPIRES_AT_MS,
    DOMAIN,
)
from .credentials import (
    epoch_milliseconds,
    validate_current_hub_credential,
    validate_epoch_milliseconds,
)
from .models import HubSnapshot

_LOGGER = logging.getLogger(__name__)

POLL_INTERVAL_SECONDS = 30
POLL_BACKOFF_SECONDS = (30, 60, 120, 300)
ROTATION_LEAD_TIME = timedelta(days=7)
ROTATION_LEAD_TIME_MS = ROTATION_LEAD_TIME // timedelta(milliseconds=1)


class TeslatlasDataCoordinator(DataUpdateCoordinator[HubSnapshot]):
    """Own the only refresh schedule for one config entry."""

    def __init__(
        self,
        hass: HomeAssistant,
        entry: ConfigEntry,
        client: TeslatlasHubClient,
        *,
        now: Callable[[], datetime] | None = None,
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
        self._now = now or (lambda: datetime.now(UTC))
        self._rotation_lock = asyncio.Lock()
        self._shutdown_task: asyncio.Task[None] | None = None

    @property
    def last_event_id(self) -> None:
        """Confirm that this profile has no event replay cursor."""
        return None

    @override
    async def _async_update_data(self) -> HubSnapshot:
        """Poll one bounded set of independently observed vehicle states."""
        try:
            await self._async_rotate_bearer_if_due()
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

    async def _async_rotate_bearer_if_due(self) -> None:
        """Persist a replacement bearer before making it active in the client."""
        async with self._rotation_lock:
            expires_at_ms = self.config_entry.data.get(CONF_TOKEN_EXPIRES_AT_MS)
            if expires_at_ms is None:
                return
            expires_at_ms = validate_epoch_milliseconds(
                expires_at_ms, CONF_TOKEN_EXPIRES_AT_MS
            )
            if expires_at_ms - epoch_milliseconds(self._now()) > ROTATION_LEAD_TIME_MS:
                return

            rotated = await self.client.async_rotate()
            if rotated.info.hub_id != self.config_entry.data[CONF_HUB_ID]:
                raise HubAuthenticationError(
                    "Teslatlas Hub identity changed during rotation"
                )
            if rotated.device_id != self.config_entry.data.get(CONF_DEVICE_ID):
                raise HubAuthenticationError(
                    "Teslatlas Hub device identity changed during rotation"
                )
            validate_current_hub_credential(
                rotated.access_token,
                rotated.device_id,
                rotated.expires_at_ms,
                now_ms=epoch_milliseconds(self._now()),
            )

            updated_data: dict[str, Any] = {
                **self.config_entry.data,
                CONF_ACCESS_TOKEN: rotated.access_token,
                CONF_TOKEN_EXPIRES_AT_MS: rotated.expires_at_ms,
            }
            self.hass.config_entries.async_update_entry(
                self.config_entry, data=updated_data
            )
            self.client.set_bearer(rotated.access_token, rotated.expires_at_ms)

    @override
    async def async_shutdown(self) -> None:
        """Cancel coordinator timers and close the client exactly once."""
        if self._shutdown_task is None:
            self._closed = True
            self._shutdown_task = asyncio.create_task(self._async_finalize_shutdown())
        await asyncio.shield(self._shutdown_task)

    async def _async_finalize_shutdown(self) -> None:
        """Keep cleanup joinable after a cancelled shutdown waiter."""
        try:
            await super().async_shutdown()
        finally:
            await self.client.async_close()

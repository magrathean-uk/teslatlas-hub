"""Public Teslatlas Hub client boundary."""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

from .models import HubEndpoint, HubInfo, HubSnapshot, PairingResult

if TYPE_CHECKING:
    from aiohttp import ClientSession
    from homeassistant.core import HomeAssistant


class HubClientError(Exception):
    """Base public-client failure."""


class HubConnectionError(HubClientError):
    """The configured public Hub endpoint is unavailable."""


class HubAuthenticationError(HubClientError):
    """The scoped device bearer is invalid or expired."""


class HubIdentityError(HubAuthenticationError):
    """The endpoint no longer identifies the expected Hub."""


class HubPairingError(HubClientError):
    """The transient pairing secret was rejected."""


class HubContractError(HubClientError):
    """The Hub response violates the selected public contract."""


class ProtocolContractUnavailable(HubContractError):
    """A requested operation is absent from the selected public profile."""


class TeslatlasHubClient(Protocol):
    """Operations required from a released public-protocol adapter."""

    async def async_probe(self) -> HubInfo:
        """Return stable public Hub identity without pairing."""

    async def async_pair(
        self,
        pairing_id: str,
        pairing_secret: str,
        device_name: str,
    ) -> PairingResult:
        """Claim a transient secret and return a scoped device bearer."""

    async def async_snapshot(self) -> HubSnapshot:
        """Return one bounded current-state snapshot."""

    async def async_rotate(self) -> PairingResult:
        """Rotate the current scoped device bearer."""

    async def async_close(self) -> None:
        """Release owned network resources."""


def create_client(
    endpoint: HubEndpoint,
    bearer_token: str | None = None,
    *,
    expected_hub_id: str | None = None,
    bearer_expires_at_ms: int | None = None,
    hass: HomeAssistant | None = None,
    session: ClientSession | None = None,
) -> TeslatlasHubClient:
    """Create the bounded production client through HA's session helpers."""
    from .current_hub_client import (
        CurrentHubClient,
        pinned_request_class,
        tls_pin_bytes,
    )

    owns_session_wrapper = False
    transport_pin_bound = False
    if endpoint.tls_pin is not None:
        if not endpoint.use_tls:
            raise HubContractError("tls_pin requires a TLS endpoint")
        if session is not None:
            raise TypeError("a pinned endpoint requires hass-managed session creation")
        if hass is None:
            raise TypeError("hass is required for a pinned endpoint")
        from homeassistant.helpers.aiohttp_client import async_create_clientsession

        session = async_create_clientsession(
            hass,
            auto_cleanup=False,
            request_class=pinned_request_class(tls_pin_bytes(endpoint.tls_pin)),
        )
        owns_session_wrapper = True
        transport_pin_bound = True
    elif session is None:
        if hass is None:
            raise TypeError("hass or session is required")
        from homeassistant.helpers.aiohttp_client import async_get_clientsession

        session = async_get_clientsession(hass)

    return CurrentHubClient(
        session,
        endpoint,
        bearer_token=bearer_token,
        bearer_expires_at_ms=bearer_expires_at_ms,
        expected_hub_id=expected_hub_id,
        owns_session_wrapper=owns_session_wrapper,
        transport_pin_bound=transport_pin_bound,
    )

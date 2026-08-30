"""Public Teslatlas Hub client boundary.

The protocol repository has not frozen any network routes or payload schemas.
This module therefore defines only the integration-facing interface and a
fail-closed production placeholder.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import Protocol

from .models import HubEndpoint, HubEvent, HubInfo, HubSnapshot, PairingResult

PROTOCOL_UNAVAILABLE = (
    "The Teslatlas public protocol is not frozen; runtime transport is disabled"
)


class HubClientError(Exception):
    """Base public-client failure."""


class HubConnectionError(HubClientError):
    """The configured public Hub endpoint is unavailable."""


class HubAuthenticationError(HubClientError):
    """The scoped device bearer is invalid or expired."""


class HubPairingError(HubClientError):
    """The transient pairing secret was rejected."""


class ProtocolContractUnavailable(HubClientError):
    """Released public routes and schemas are not available yet."""


class TeslatlasHubClient(Protocol):
    """Operations required from a released public-protocol adapter."""

    async def async_probe(self) -> HubInfo:
        """Return stable public Hub identity without pairing."""

    async def async_pair(self, pairing_secret: str) -> PairingResult:
        """Claim a transient secret and return a scoped device bearer."""

    async def async_snapshot(self) -> HubSnapshot:
        """Return one bounded current-state snapshot."""

    def async_events(self, last_event_id: str | None) -> AsyncIterator[HubEvent]:
        """Consume public push events with an optional replay cursor."""

    async def async_close(self) -> None:
        """Release owned network resources."""


@dataclass(slots=True, repr=False)
class _PendingProtocolClient:
    """Fail closed until released protocol artifacts define transport."""

    endpoint: HubEndpoint
    _bearer_token: str | None = field(default=None, repr=False)

    def __repr__(self) -> str:
        """Return a diagnostic representation that never contains credentials."""
        return (
            f"{type(self).__name__}(host={self.endpoint.host!r}, "
            f"port={self.endpoint.port!r}, use_tls={self.endpoint.use_tls!r})"
        )

    async def async_probe(self) -> HubInfo:
        """Refuse to probe using an invented route."""
        raise ProtocolContractUnavailable(PROTOCOL_UNAVAILABLE)

    async def async_pair(self, pairing_secret: str) -> PairingResult:
        """Refuse to claim a secret using an invented route."""
        raise ProtocolContractUnavailable(PROTOCOL_UNAVAILABLE)

    async def async_snapshot(self) -> HubSnapshot:
        """Refuse to query using an invented route."""
        raise ProtocolContractUnavailable(PROTOCOL_UNAVAILABLE)

    async def async_events(self, last_event_id: str | None) -> AsyncIterator[HubEvent]:
        """Refuse to stream using an invented route."""
        raise ProtocolContractUnavailable(PROTOCOL_UNAVAILABLE)
        yield  # pragma: no cover

    async def async_close(self) -> None:
        """Release no resources because no transport was opened."""


def create_client(
    endpoint: HubEndpoint,
    bearer_token: str | None = None,
) -> TeslatlasHubClient:
    """Create the fail-closed production client boundary."""
    return _PendingProtocolClient(endpoint, bearer_token)

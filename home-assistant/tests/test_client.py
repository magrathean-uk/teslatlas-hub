"""Tests for the fail-closed public-client boundary."""

from __future__ import annotations

import pytest

from custom_components.teslatlas_hub.client import (
    ProtocolContractUnavailable,
    create_client,
)
from custom_components.teslatlas_hub.models import HubEndpoint


@pytest.mark.asyncio
async def test_pending_client_fails_closed_for_every_network_operation() -> None:
    """Catch accidental runtime claims before public routes are frozen."""
    client = create_client(
        HubEndpoint(host="hub.invalid", port=443, use_tls=True),
        bearer_token="fixture-device-bearer",
    )

    operations = (
        client.async_probe(),
        client.async_pair("fixture-pairing-secret"),
        client.async_snapshot(),
        anext(client.async_events(None)),
    )
    for operation in operations:
        with pytest.raises(ProtocolContractUnavailable, match="public protocol"):
            await operation

    await client.async_close()


def test_pending_client_repr_never_discloses_bearer() -> None:
    """Catch secret exposure through routine logging or tracebacks."""
    client = create_client(
        HubEndpoint(host="hub.invalid", port=443, use_tls=True),
        bearer_token="fixture-device-bearer",
    )

    assert "fixture-device-bearer" not in repr(client)

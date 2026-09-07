"""Tests for production-client construction and secret-safe diagnostics."""

from __future__ import annotations

from unittest.mock import MagicMock, patch

from aiohttp import ClientRequest
from homeassistant.core import HomeAssistant

from custom_components.teslatlas_hub.client import create_client
from custom_components.teslatlas_hub.current_hub_client import CurrentHubClient
from custom_components.teslatlas_hub.models import HubEndpoint


def test_create_client_returns_current_profile_adapter() -> None:
    """Catch regression to the historical pending placeholder."""
    client = create_client(
        HubEndpoint(host="hub.invalid", port=443, use_tls=True),
        bearer_token="fixture-device-bearer",
        expected_hub_id="fixture-hub",
        session=MagicMock(),
    )

    assert isinstance(client, CurrentHubClient)
    assert "fixture-device-bearer" not in repr(client)


async def test_pinned_client_uses_and_detaches_ha_managed_request_wrapper(
    hass: HomeAssistant,
) -> None:
    """Catch pinning outside the actual HA-created HTTP request connection."""
    session = MagicMock()
    with patch(
        "homeassistant.helpers.aiohttp_client.async_create_clientsession",
        return_value=session,
    ) as create_session:
        client = create_client(
            HubEndpoint(
                host="hub.invalid",
                port=443,
                use_tls=True,
                tls_pin="a" * 64,
            ),
            hass=hass,
        )

        request_class = create_session.call_args.kwargs["request_class"]
        assert issubclass(request_class, ClientRequest)
        assert create_session.call_args.kwargs["auto_cleanup"] is False
        await client.async_close()

    session.detach.assert_called_once_with()

"""Tests for production-client construction and secret-safe diagnostics."""

from __future__ import annotations

import asyncio
from datetime import UTC, datetime, timedelta
from unittest.mock import MagicMock, patch

import pytest
from aiohttp import ClientRequest
from homeassistant.core import HomeAssistant

from custom_components.teslatlas_hub.client import HubContractError, create_client
from custom_components.teslatlas_hub.credentials import (
    SIGNED64_MAX,
    HubCredentialExpiredError,
    epoch_milliseconds,
    validate_current_hub_credential,
)
from custom_components.teslatlas_hub.current_hub_client import (
    CurrentHubClient,
    _telemetry_age,
    tls_pin_bytes,
)
from custom_components.teslatlas_hub.models import HubEndpoint
from tests.helpers import FIXTURE_ACCESS_TOKEN, FIXTURE_DEVICE_ID


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


@pytest.mark.parametrize("pin", ["a" * 62 + "  ", "A" * 64, "g" * 64])
def test_tls_pin_rejects_noncanonical_hexadecimal(pin: str) -> None:
    with pytest.raises(HubContractError, match="tls_pin"):
        tls_pin_bytes(pin)
    assert tls_pin_bytes("a" * 64) == bytes.fromhex("a" * 64)


@pytest.mark.parametrize(
    ("token", "device_id", "expiry"),
    [
        ("g" * 64, FIXTURE_DEVICE_ID, 1),
        ("A" * 64, FIXTURE_DEVICE_ID, 1),
        ("a" * 62 + "\r\n", FIXTURE_DEVICE_ID, 1),
        (FIXTURE_ACCESS_TOKEN, "33333333333343338333333333333333", 1),
        (FIXTURE_ACCESS_TOKEN, FIXTURE_DEVICE_ID, True),
        (FIXTURE_ACCESS_TOKEN, FIXTURE_DEVICE_ID, 2**63),
        (FIXTURE_ACCESS_TOKEN, FIXTURE_DEVICE_ID, 0),
    ],
)
def test_issued_credential_admission_rejects_malformed_or_stale_values(
    token: str, device_id: str, expiry: object
) -> None:
    with pytest.raises(HubContractError):
        validate_current_hub_credential(token, device_id, expiry, now_ms=0)


def test_issued_credential_accepts_full_signed64_future_domain() -> None:
    validate_current_hub_credential(
        FIXTURE_ACCESS_TOKEN, FIXTURE_DEVICE_ID, SIGNED64_MAX, now_ms=0
    )


def test_expired_issued_credential_has_a_distinct_recovery_error() -> None:
    with pytest.raises(HubCredentialExpiredError, match="expires_at_ms"):
        validate_current_hub_credential(
            FIXTURE_ACCESS_TOKEN, FIXTURE_DEVICE_ID, 0, now_ms=0
        )


def test_epoch_and_telemetry_math_preserve_zero_null_and_skew_boundary() -> None:
    epoch = datetime(1970, 1, 1, tzinfo=UTC)
    assert epoch_milliseconds(epoch - timedelta(microseconds=1)) == -1
    assert _telemetry_age(None, epoch) is None
    assert _telemetry_age(0, epoch) == 0
    assert _telemetry_age(-(2**63), epoch) == (2**63) // 1000
    assert _telemetry_age(300_000, epoch) == 0
    with pytest.raises(HubContractError, match="future"):
        _telemetry_age(300_001, epoch)


async def test_pair_rejects_dual_secret_keywords_before_dispatch() -> None:
    session = MagicMock()
    client = CurrentHubClient(
        session, HubEndpoint(host="hub.invalid", port=443, use_tls=True)
    )
    with pytest.raises(TypeError, match="not both"):
        await client.async_pair(
            "44444444-4444-4444-8444-444444444444",
            secret="a" * 64,
            pairing_secret="a" * 64,
            device_name="Home Assistant",
        )
    session.request.assert_not_called()
    await client.async_close()


def test_set_bearer_rejects_invalid_replacement_without_mutation() -> None:
    now = datetime(2026, 9, 26, tzinfo=UTC)
    client = CurrentHubClient(
        MagicMock(),
        HubEndpoint(host="hub.invalid", port=443, use_tls=True),
        bearer_token=FIXTURE_ACCESS_TOKEN,
        bearer_expires_at_ms=2_000_000_000_000,
        now=lambda: now,
    )
    with pytest.raises(HubContractError, match="access_token"):
        client.set_bearer("a" * 62 + "\r\n", 2_000_000_000_000)
    with pytest.raises(HubContractError, match="future"):
        client.set_bearer("b" * 64, epoch_milliseconds(now))
    assert client._bearer_token == FIXTURE_ACCESS_TOKEN
    assert client._bearer_expires_at_ms == 2_000_000_000_000


async def test_cancelled_close_waiter_can_rejoin_owned_session_cleanup() -> None:
    request_started = asyncio.Event()
    request_cancelled = asyncio.Event()
    release_cleanup = asyncio.Event()

    class BlockedRequest:
        async def __aenter__(self):
            request_started.set()
            try:
                await asyncio.Event().wait()
            except asyncio.CancelledError:
                request_cancelled.set()
                await release_cleanup.wait()
                raise

        async def __aexit__(self, *_args: object) -> None:
            return None

    session = MagicMock()
    session.request.return_value = BlockedRequest()
    client = CurrentHubClient(
        session,
        HubEndpoint(host="hub.invalid", port=443, use_tls=True),
        owns_session_wrapper=True,
    )
    probe = asyncio.create_task(client.async_probe())
    await asyncio.wait_for(request_started.wait(), timeout=1)
    first_close = asyncio.create_task(client.async_close())
    await asyncio.wait_for(request_cancelled.wait(), timeout=1)
    first_close.cancel()
    with pytest.raises(asyncio.CancelledError):
        await first_close
    session.detach.assert_not_called()

    retry = asyncio.create_task(client.async_close())
    await asyncio.sleep(0)
    assert not retry.done()
    release_cleanup.set()
    await asyncio.wait_for(retry, timeout=1)
    with pytest.raises(asyncio.CancelledError):
        await probe
    await client.async_close()
    session.detach.assert_called_once_with()

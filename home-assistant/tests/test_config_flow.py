"""Tests for invitation setup, reauthentication, and endpoint identity."""

from __future__ import annotations

from dataclasses import replace
from unittest.mock import patch

import pytest
from aiohttp import web
from homeassistant.config_entries import SOURCE_REAUTH, SOURCE_RECONFIGURE, SOURCE_USER
from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.client import HubConnectionError, HubPairingError
from custom_components.teslatlas_hub.const import (
    CONF_ACCESS_TOKEN,
    CONF_DEVICE_ID,
    CONF_DEVICE_NAME,
    CONF_HUB_ID,
    CONF_PAIRING_ID,
    CONF_PAIRING_SECRET,
    CONF_PORT,
    CONF_TLS_PIN,
    CONF_TOKEN_EXPIRES_AT_MS,
    CONF_USE_TLS,
    DOMAIN,
)
from tests.helpers import FixtureHubClient

PAIRING_INPUT = {
    CONF_PAIRING_ID: "44444444-4444-4444-8444-444444444444",
    CONF_PAIRING_SECRET: "fixture-pairing-secret",
    CONF_DEVICE_NAME: "Home Assistant",
}
LIVE_HUB_ID = "11111111-1111-4111-8111-111111111111"
LIVE_OTHER_HUB_ID = "22222222-2222-4222-8222-222222222222"


def _discovery(hub_id: str) -> dict:
    return {
        "hub_id": hub_id,
        "protocol": "teslatlas-sync",
        "protocol_major": 1,
        "api_versions": ["1.0"],
        "capabilities": ["query.vehicles", "query.current"],
    }


@pytest.fixture
def fixture_client() -> FixtureHubClient:
    return FixtureHubClient()


@pytest.fixture
def client_factory(fixture_client: FixtureHubClient):
    with patch(
        "custom_components.teslatlas_hub.config_flow.create_client",
        return_value=fixture_client,
    ) as factory:
        yield factory


def _entry(hass: HomeAssistant) -> MockConfigEntry:
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id="hub-fixture",
        data={
            CONF_HOST: "old-hub.local",
            CONF_PORT: 443,
            CONF_USE_TLS: True,
            CONF_TLS_PIN: "c" * 64,
            CONF_HUB_ID: "hub-fixture",
            CONF_ACCESS_TOKEN: "old-device-bearer",
            CONF_DEVICE_ID: "old-device",
            CONF_TOKEN_EXPIRES_AT_MS: 1,
        },
    )
    entry.add_to_hass(hass)
    return entry


async def _start_pair(hass: HomeAssistant) -> dict:
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": SOURCE_USER}
    )
    return await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_HOST: "hub-fixture.local",
            CONF_PORT: 7443,
            CONF_USE_TLS: True,
            CONF_TLS_PIN: "c" * 64,
        },
    )


async def test_user_flow_claims_full_invitation_without_storing_it(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Catch incomplete claim inputs or persisted invitation material."""
    result = await _start_pair(hass)
    assert result["step_id"] == "pair"

    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )

    assert result["type"] is FlowResultType.CREATE_ENTRY
    assert result["data"] == {
        CONF_HOST: "hub-fixture.local",
        CONF_PORT: 7443,
        CONF_USE_TLS: True,
        CONF_TLS_PIN: "c" * 64,
        CONF_HUB_ID: "hub-fixture",
        CONF_ACCESS_TOKEN: "fixture-device-bearer",
        CONF_DEVICE_ID: "device-fixture",
        CONF_TOKEN_EXPIRES_AT_MS: 1_788_567_300_000,
    }
    assert fixture_client.pairing_ids == [PAIRING_INPUT[CONF_PAIRING_ID]]
    assert fixture_client.pairing_secrets == [PAIRING_INPUT[CONF_PAIRING_SECRET]]
    assert fixture_client.device_names == ["Home Assistant"]
    assert CONF_PAIRING_ID not in result["data"]
    assert CONF_PAIRING_SECRET not in result["data"]
    assert client_factory.call_args.kwargs["expected_hub_id"] == "hub-fixture"


async def test_pairing_failure_remains_repairable(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    fixture_client.pair_error = HubPairingError("expired")
    result = await _start_pair(hass)
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["type"] is FlowResultType.FORM
    assert result["errors"] == {"base": "invalid_pairing_secret"}


async def test_probe_failure_stays_on_manual_endpoint_form(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    fixture_client.probe_error = HubConnectionError("offline")
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": SOURCE_USER}
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {CONF_HOST: "offline.local", CONF_PORT: 443, CONF_USE_TLS: True},
    )
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "user"
    assert result["errors"] == {"base": "cannot_connect"}


async def test_reauth_replaces_only_device_credential_metadata(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    entry = _entry(hass)
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
        data=entry.data,
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["reason"] == "reauth_successful"
    assert entry.data[CONF_ACCESS_TOKEN] == "fixture-device-bearer"
    assert entry.data[CONF_DEVICE_ID] == "device-fixture"
    assert entry.data[CONF_HOST] == "old-hub.local"
    assert client_factory.call_args.kwargs["expected_hub_id"] == "hub-fixture"


async def test_reauth_rejects_a_different_hub(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    entry = _entry(hass)
    fixture_client.info = replace(fixture_client.info, hub_id="other-hub")
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
        data=entry.data,
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["reason"] == "wrong_hub"
    assert entry.data[CONF_ACCESS_TOKEN] == "old-device-bearer"


async def test_reconfigure_probes_without_saved_bearer_before_endpoint_update(
    hass: HomeAssistant,
    client_factory,
) -> None:
    entry = _entry(hass)
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": entry.entry_id},
        data=entry.data,
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_HOST: "new-hub.local",
            CONF_PORT: 8443,
            CONF_USE_TLS: True,
            CONF_TLS_PIN: "d" * 64,
        },
    )
    assert result["reason"] == "reconfigure_successful"
    assert entry.data[CONF_HOST] == "new-hub.local"
    assert entry.data[CONF_ACCESS_TOKEN] == "old-device-bearer"
    assert client_factory.call_args.kwargs.get("bearer_token") is None


async def test_user_flow_changed_hub_never_receives_pairing_secret(
    hass: HomeAssistant,
    aiohttp_server,
    socket_enabled,
) -> None:
    """Prove the initial flow binds its first discovery before the claim POST."""
    del socket_enabled
    discovery_requests = 0
    claim_requests = 0

    async def discovery(_request: web.Request) -> web.Response:
        nonlocal discovery_requests
        discovery_requests += 1
        hub_id = LIVE_HUB_ID if discovery_requests == 1 else LIVE_OTHER_HUB_ID
        return web.json_response(_discovery(hub_id))

    async def claim(_request: web.Request) -> web.Response:
        nonlocal claim_requests
        claim_requests += 1
        return web.Response(status=500)

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_post("/v1/pairings/{pairing_id}/claim", claim)
    server = await aiohttp_server(app)

    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": SOURCE_USER}
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_HOST: server.host,
            CONF_PORT: server.port,
            CONF_USE_TLS: False,
        },
    )
    assert result["step_id"] == "pair"
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "wrong_hub"
    assert discovery_requests == 2
    assert claim_requests == 0


async def test_reauth_changed_hub_never_receives_pairing_secret(
    hass: HomeAssistant,
    aiohttp_server,
    socket_enabled,
) -> None:
    """Prove reauthentication binds the stored identity before its claim POST."""
    del socket_enabled
    claim_requests = 0

    async def discovery(_request: web.Request) -> web.Response:
        return web.json_response(_discovery(LIVE_OTHER_HUB_ID))

    async def claim(_request: web.Request) -> web.Response:
        nonlocal claim_requests
        claim_requests += 1
        return web.Response(status=500)

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_post("/v1/pairings/{pairing_id}/claim", claim)
    server = await aiohttp_server(app)
    entry = MockConfigEntry(
        domain=DOMAIN,
        title="Fixture Hub",
        unique_id=LIVE_HUB_ID,
        data={
            CONF_HOST: server.host,
            CONF_PORT: server.port,
            CONF_USE_TLS: False,
            CONF_TLS_PIN: None,
            CONF_HUB_ID: LIVE_HUB_ID,
            CONF_ACCESS_TOKEN: "old-device-bearer",
            CONF_DEVICE_ID: "old-device",
            CONF_TOKEN_EXPIRES_AT_MS: 1,
        },
    )
    entry.add_to_hass(hass)

    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
        data=entry.data,
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "wrong_hub"
    assert claim_requests == 0
    assert entry.data[CONF_ACCESS_TOKEN] == "old-device-bearer"

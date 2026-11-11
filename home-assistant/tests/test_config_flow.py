"""Tests for invitation setup, reauthentication, and endpoint identity."""

from __future__ import annotations

import asyncio
from dataclasses import replace
from datetime import UTC, datetime
from unittest.mock import AsyncMock, patch

import pytest
from aiohttp import web
from homeassistant.config_entries import SOURCE_REAUTH, SOURCE_RECONFIGURE, SOURCE_USER
from homeassistant.const import CONF_HOST
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.helpers import device_registry as dr
from homeassistant.helpers import entity_registry as er
from pytest_homeassistant_custom_component.common import MockConfigEntry

from custom_components.teslatlas_hub.client import (
    HubAuthenticationError,
    HubConnectionError,
    HubIdentityError,
    HubPairingError,
    ProtocolContractUnavailable,
    create_client,
)
from custom_components.teslatlas_hub.config_flow import (
    APPROVE_NEW_TLS_PIN,
    TeslatlasHubConfigFlow,
)
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
from tests.helpers import (
    FIXTURE_ACCESS_TOKEN,
    FIXTURE_DEVICE_ID,
    FIXTURE_EXPIRES_AT_MS,
    FixtureHubClient,
)

PAIRING_INPUT = {
    CONF_PAIRING_ID: "44444444-4444-4444-8444-444444444444",
    CONF_PAIRING_SECRET: "fixture-pairing-secret",
    CONF_DEVICE_NAME: "Home Assistant",
}
LIVE_HUB_ID = "11111111-1111-4111-8111-111111111111"
LIVE_OTHER_HUB_ID = "22222222-2222-4222-8222-222222222222"


async def _held_auth_server(aiohttp_server, *, phase=None, failure=None):
    """Exercise production HTTP and manager lifecycle with deterministic gates."""
    started, release = asyncio.Event(), asyncio.Event()
    counts = {"claim": 0, "vehicles": 0}
    control = {"failure": failure}

    async def discovery(_request):
        return web.json_response(
            {
                "hub_id": LIVE_HUB_ID,
                "protocol": "teslatlas-sync",
                "protocol_major": 1,
                "api_versions": ["1.0"],
                "capabilities": ["query.vehicles", "query.current"],
            }
        )

    async def claim(_request):
        counts["claim"] += 1
        if phase == "claim":
            started.set()
            await release.wait()
        return web.json_response(
            {
                "access_token": FIXTURE_ACCESS_TOKEN,
                "device_id": FIXTURE_DEVICE_ID,
                "expires_at_ms": FIXTURE_EXPIRES_AT_MS,
            }
        )

    async def vehicles(_request):
        counts["vehicles"] += 1
        if phase == "vehicles":
            started.set()
            await release.wait()
        if control["failure"] == "unauthorized":
            return web.json_response({"error": "unauthorized"}, status=401)
        return web.json_response({"vehicles": []})

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_post("/v1/pairings/{pairing_id}/claim", claim)
    app.router.add_get("/v1/vehicles", vehicles)
    return await aiohttp_server(app), started, release, counts, control


def _http_auth_entry(hass, server):
    entry = MockConfigEntry(
        domain=DOMAIN,
        unique_id=LIVE_HUB_ID,
        data={
            CONF_HOST: server.host,
            CONF_PORT: server.port,
            CONF_USE_TLS: False,
            CONF_HUB_ID: LIVE_HUB_ID,
            CONF_DEVICE_ID: FIXTURE_DEVICE_ID,
            CONF_ACCESS_TOKEN: "b" * 64,
            CONF_TOKEN_EXPIRES_AT_MS: FIXTURE_EXPIRES_AT_MS,
        },
        version=1,
        minor_version=2,
    )
    entry.add_to_hass(hass)
    return entry


@pytest.mark.parametrize("phase", ["claim", "vehicles"])
@pytest.mark.parametrize("abort", [True, False])
async def test_manager_removed_reauth_cannot_commit_late_response(
    hass, aiohttp_server, socket_enabled, phase, abort
):
    """Public manager removal rejects both late phases; ordinary completion works."""
    del socket_enabled
    server, started, release, counts, _control = await _held_auth_server(
        aiohttp_server, phase=phase
    )
    entry = _http_auth_entry(hass, server)
    original = dict(entry.data)
    clients = []

    def capture(*args, **kwargs):
        client = create_client(*args, **kwargs)
        clients.append(client)
        return client

    with (
        patch("custom_components.teslatlas_hub.config_flow.create_client", new=capture),
        patch.object(hass.config_entries, "async_reload", AsyncMock()) as reload,
    ):
        first = await hass.config_entries.flow.async_init(
            DOMAIN,
            context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
            data=entry.data,
        )
        configure = asyncio.create_task(
            hass.config_entries.flow.async_configure(first["flow_id"], PAIRING_INPUT)
        )
        try:
            await asyncio.wait_for(started.wait(), 5)
            if abort:
                hass.config_entries.flow.async_abort(first["flow_id"])
                assert not hass.config_entries.flow.async_progress_by_handler(DOMAIN)
            assert dict(entry.data) == original
            assert not configure.done()
            release.set()
            result = await asyncio.wait_for(configure, 5)
            await hass.async_block_till_done()
            assert result["type"] is FlowResultType.ABORT
            assert counts["claim"] == 1
            if abort:
                assert dict(entry.data) == original
                reload.assert_not_called()
                assert result["reason"] == "reauth_cancelled"
                assert counts["vehicles"] == (0 if phase == "claim" else 1)
            else:
                assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
                reload.assert_awaited_once_with(entry.entry_id)
                assert result["reason"] == "reauth_successful"
                assert counts["vehicles"] == 1
            assert all(client._closed for client in clients)
            assert all(not client._request_tasks for client in clients)
            assert all(not client._snapshot_tasks for client in clients)
        finally:
            release.set()
            if not configure.done():
                configure.cancel()
            await asyncio.gather(configure, return_exceptions=True)


@pytest.mark.parametrize("source", [SOURCE_USER, SOURCE_REAUTH])
@pytest.mark.parametrize("failure", ["unauthorized", "expired"])
async def test_terminal_auth_returns_editable_invitation_and_accepts_new_claim(
    hass, aiohttp_server, socket_enabled, source, failure
):
    """401 and expiry discard pending access without replacing the saved entry."""
    del socket_enabled
    server, _started, _release, counts, control = await _held_auth_server(
        aiohttp_server,
        failure=failure,
        phase="vehicles" if failure == "expired" else None,
    )
    entry = _http_auth_entry(hass, server) if source == SOURCE_REAUTH else None
    original = dict(entry.data) if entry else None
    context = {"source": source}
    if entry:
        context["entry_id"] = entry.entry_id
    with patch.object(hass.config_entries, "async_reload", AsyncMock()):
        result = await hass.config_entries.flow.async_init(
            DOMAIN, context=context, data=entry.data if entry else None
        )
        if source == SOURCE_USER:
            result = await hass.config_entries.flow.async_configure(
                result["flow_id"],
                {CONF_HOST: server.host, CONF_PORT: server.port, CONF_USE_TLS: False},
            )
        configure = asyncio.create_task(
            hass.config_entries.flow.async_configure(result["flow_id"], PAIRING_INPUT)
        )
        try:
            if failure == "expired":
                await asyncio.wait_for(_started.wait(), 5)
                with patch(
                    "custom_components.teslatlas_hub.config_flow.datetime"
                ) as clock:
                    clock.now.return_value = datetime(2034, 1, 1, tzinfo=UTC)
                    _release.set()
                    result = await asyncio.wait_for(configure, 5)
            else:
                result = await asyncio.wait_for(configure, 5)
        finally:
            _release.set()
            if not configure.done():
                configure.cancel()
            await asyncio.gather(configure, return_exceptions=True)
        assert result["type"] is FlowResultType.FORM
        assert result["step_id"] == (
            "pair" if source == SOURCE_USER else "reauth_confirm"
        )
        assert result["errors"] == {"base": "invalid_auth"}
        assert len(result["data_schema"].schema) == 3
        if entry:
            assert dict(entry.data) == original
        else:
            assert not hass.config_entries.async_entries(DOMAIN)
        assert counts["claim"] == 1
        assert counts["vehicles"] == 1
        control["failure"] = None
        result = await hass.config_entries.flow.async_configure(
            result["flow_id"], PAIRING_INPUT | {CONF_PAIRING_SECRET: "fresh-invitation"}
        )
        assert counts["claim"] == 2
        assert result["type"] is (
            FlowResultType.CREATE_ENTRY
            if source == SOURCE_USER
            else FlowResultType.ABORT
        )
        if entry:
            assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN


@pytest.mark.parametrize(
    "old_error", [None, HubIdentityError("old identity"), HubConnectionError("offline")]
)
async def test_superseded_reauth_claim_keeps_newer_candidate_retryable(hass, old_error):
    """An older completion cannot remove the manager's newer transient retry."""
    started, release = asyncio.Event(), asyncio.Event()
    claims = 0
    validation_available = False
    clients = []

    class ConcurrentClient(StrictFlowClient):
        async def async_pair(self, *args, **kwargs):
            nonlocal claims
            claims += 1
            result = await super().async_pair(*args, **kwargs)
            if claims == 1:
                started.set()
                await release.wait()
                if old_error:
                    raise old_error
            return result

        async def async_snapshot(self):
            if not validation_available:
                raise HubConnectionError("transient")
            return await super().async_snapshot()

    def create(*args, **kwargs):
        client = ConcurrentClient()
        clients.append(client)
        return client

    entry = _entry(hass)
    original = dict(entry.data)
    with (
        patch("custom_components.teslatlas_hub.config_flow.create_client", new=create),
        patch.object(hass.config_entries, "async_reload", AsyncMock()) as reload,
    ):
        first = await hass.config_entries.flow.async_init(
            DOMAIN,
            context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
            data=entry.data,
        )
        old = asyncio.create_task(
            hass.config_entries.flow.async_configure(first["flow_id"], PAIRING_INPUT)
        )
        try:
            await asyncio.wait_for(started.wait(), 5)
            newer = await hass.config_entries.flow.async_configure(
                first["flow_id"],
                PAIRING_INPUT | {CONF_PAIRING_SECRET: "newer-invitation"},
            )
            assert newer["step_id"] == "reauth_validate"
            assert newer["errors"] == {"base": "cannot_connect"}
            release.set()
            result = await asyncio.wait_for(old, 5)
            assert result["type"] is FlowResultType.FORM
            assert result["step_id"] == "reauth_validate"
            assert result["errors"] == newer["errors"]
            assert dict(entry.data) == original
            reload.assert_not_called()
            assert hass.config_entries.flow.async_progress_by_handler(DOMAIN)
            validation_available = True
            final = await hass.config_entries.flow.async_configure(newer["flow_id"], {})
            assert final["reason"] == "reauth_successful"
            assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
            assert claims == 2
            await hass.async_block_till_done()
            reload.assert_awaited_once_with(entry.entry_id)
            assert all(client.closed for client in clients)
        finally:
            release.set()
            if not old.done():
                old.cancel()
            await asyncio.gather(old, return_exceptions=True)


REPLACEMENT_INPUT = {
    CONF_HOST: "replacement.local",
    CONF_PORT: 8443,
    CONF_USE_TLS: True,
    CONF_TLS_PIN: "d" * 64,
}


class StrictFlowClient(FixtureHubClient):
    """Fail immediately if a flow reuses a closed client."""

    async def async_probe(self):
        assert not self.closed
        return await super().async_probe()

    async def async_pair(self, *args, **kwargs):
        assert not self.closed
        return await super().async_pair(*args, **kwargs)

    async def async_snapshot(self):
        assert not self.closed
        return await super().async_snapshot()


@pytest.fixture
def replacement_factory(hass: HomeAssistant):
    """Keep construction, closure and claims observable across distinct clients."""
    clients: list[StrictFlowClient] = []
    calls: list[dict] = []
    controls: dict = {}

    def create(_endpoint, **kwargs):
        assert not clients or clients[-1].closed
        client = StrictFlowClient()
        for key, value in controls.items():
            setattr(client, key, value)
        clients.append(client)
        calls.append(kwargs)
        return client

    with (
        patch("custom_components.teslatlas_hub.config_flow.create_client", new=create),
        patch.object(hass.config_entries, "async_reload", AsyncMock(return_value=True)),
    ):
        yield clients, calls, controls


async def _start_replacement(hass: HomeAssistant, entry: MockConfigEntry) -> dict:
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": entry.entry_id},
        data=entry.data,
    )
    return await hass.config_entries.flow.async_configure(
        result["flow_id"], REPLACEMENT_INPUT
    )


async def _approve_replacement(hass: HomeAssistant, result: dict) -> dict:
    assert result["step_id"] == "reconfigure_tls"
    return await hass.config_entries.flow.async_configure(
        result["flow_id"], {APPROVE_NEW_TLS_PIN: True}
    )


def _assert_replacement_clients(clients, calls, claims: int) -> None:
    assert all(client.closed for client in clients)
    assert sum(len(client.pairing_ids) for client in clients) == claims
    assert all(call.get("bearer_token") != "old-device-bearer" for call in calls)
    for call in calls:
        if "expected_hub_id" in call:
            assert call["expected_hub_id"] == "hub-fixture"


@pytest.mark.parametrize("saved_pin", ["c" * 64, None])
async def test_approved_pin_replacement_is_atomic_and_preserves_identity(
    hass: HomeAssistant, replacement_factory, saved_pin: str | None
) -> None:
    clients, calls, controls = replacement_factory
    entry = _entry(hass)
    hass.config_entries.async_update_entry(
        entry, data=dict(entry.data) | {CONF_TLS_PIN: saved_pin, "opaque": "retained"}
    )
    original = dict(entry.data)
    original_id, unique_id = entry.entry_id, entry.unique_id
    device_registry = dr.async_get(hass)
    device = device_registry.async_get_or_create(
        config_entry_id=entry.entry_id,
        identifiers={(DOMAIN, "hub-fixture_vehicle-alpha")},
    )
    entity_registry = er.async_get(hass)
    entity = entity_registry.async_get_or_create(
        "sensor",
        DOMAIN,
        "hub-fixture_vehicle-alpha_state_of_charge",
        config_entry=entry,
        device_id=device.id,
    )
    controls["pairing_expires_at_ms"] = 2**63 - 1
    result = await _start_replacement(hass, entry)
    assert dict(entry.data) == original
    assert result["description_placeholders"] == {
        "host": "replacement.local",
        "port": "8443",
        "tls_pin": "d" * 64,
    }
    result = await _approve_replacement(hass, result)
    assert result["step_id"] == "reconfigure_pair"
    assert dict(entry.data) == original
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["reason"] == "reconfigure_successful"
    assert dict(entry.data) == original | REPLACEMENT_INPUT | {
        CONF_ACCESS_TOKEN: FIXTURE_ACCESS_TOKEN,
        CONF_DEVICE_ID: FIXTURE_DEVICE_ID,
        CONF_TOKEN_EXPIRES_AT_MS: 2**63 - 1,
    }
    assert (entry.entry_id, entry.unique_id) == (original_id, unique_id)
    assert device_registry.async_get(device.id) == device
    assert entity_registry.async_get(entity.entity_id) == entity
    _assert_replacement_clients(clients, calls, 1)
    assert len(clients) == 3
    assert calls[0]["bearer_token"] is None
    assert "bearer_token" not in calls[1]
    assert calls[2]["bearer_token"] == FIXTURE_ACCESS_TOKEN


@pytest.mark.parametrize(
    "approval", [{}, {APPROVE_NEW_TLS_PIN: False}, {APPROVE_NEW_TLS_PIN: "true"}]
)
async def test_pin_replacement_requires_explicit_boolean_approval(
    hass: HomeAssistant, replacement_factory, approval: dict
) -> None:
    clients, calls, _controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await _start_replacement(hass, entry)
    flow = hass.config_entries.flow._progress[result["flow_id"]]
    result = await flow.async_step_reconfigure_tls(approval)
    assert result["reason"] == "replacement_not_approved"
    assert dict(entry.data) == original
    _assert_replacement_clients(clients, calls, 0)


@pytest.mark.parametrize("step", ["reconfigure_pair", "reconfigure_validate"])
async def test_replacement_direct_steps_have_controlled_guards(hass, step) -> None:
    flow = TeslatlasHubConfigFlow()
    flow.hass = hass
    result = await getattr(flow, "async_step_" + step)(PAIRING_INPUT)
    assert result["reason"] == "replacement_not_approved"


@pytest.mark.parametrize(
    "candidate",
    [
        {CONF_USE_TLS: False},
        {CONF_TLS_PIN: ""},
        {CONF_TLS_PIN: "d" * 63},
        {CONF_TLS_PIN: "d " * 32},
    ],
)
async def test_replacement_rejects_unpinned_or_noncanonical_channels(
    hass, replacement_factory, candidate
) -> None:
    clients, calls, _controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": entry.entry_id},
        data=entry.data,
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], REPLACEMENT_INPUT | candidate
    )
    assert result["step_id"] == "reconfigure_confirm"
    assert result["errors"] == {"base": "invalid_contract"}
    assert dict(entry.data) == original
    _assert_replacement_clients(clients, calls, 0)


async def test_replacement_validation_retry_reuses_one_claim(
    hass, replacement_factory
) -> None:
    clients, calls, controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await _approve_replacement(hass, await _start_replacement(hass, entry))
    controls["snapshot_error"] = HubConnectionError("timeout")
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["step_id"] == "reconfigure_validate"
    assert result["errors"] == {"base": "cannot_connect"}
    assert dict(entry.data) == original
    controls.clear()
    result = await hass.config_entries.flow.async_configure(result["flow_id"], {})
    assert result["reason"] == "reconfigure_successful"
    _assert_replacement_clients(clients, calls, 1)
    assert calls[2]["bearer_token"] == calls[3]["bearer_token"] == FIXTURE_ACCESS_TOKEN


async def test_replacement_terminal_auth_requires_an_explicit_new_claim(
    hass, replacement_factory
) -> None:
    clients, calls, controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await _approve_replacement(hass, await _start_replacement(hass, entry))
    controls["snapshot_error"] = HubAuthenticationError("rejected")
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["step_id"] == "reconfigure_pair"
    assert result["errors"] == {"base": "invalid_auth"}
    assert dict(entry.data) == original
    _assert_replacement_clients(clients, calls, 1)
    controls.clear()
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["reason"] == "reconfigure_successful"
    _assert_replacement_clients(clients, calls, 2)


@pytest.mark.parametrize(
    "invalid",
    [
        {"access_token": "A" * 64},
        {"device_id": "device"},
        {"pairing_expires_at_ms": True},
        {"pairing_expires_at_ms": 1},
        {"pairing_expires_at_ms": 2**63},
    ],
)
async def test_initial_pairing_rejects_invalid_issued_metadata_before_storage(
    hass, fixture_client, client_factory, invalid
) -> None:
    for key, value in invalid.items():
        setattr(fixture_client, key, value)
    result = await _start_pair(hass)
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["step_id"] == (
        "pair"
        if type(invalid.get("pairing_expires_at_ms")) is int
        and invalid["pairing_expires_at_ms"] == 1
        else "pair_validate"
    )
    expected = (
        "invalid_auth"
        if type(invalid.get("pairing_expires_at_ms")) is int
        and invalid["pairing_expires_at_ms"] == 1
        else "invalid_contract"
    )
    assert result["errors"] == {"base": expected}
    assert not hass.config_entries.async_entries(DOMAIN)
    assert fixture_client.snapshot_calls == 0


@pytest.mark.parametrize("phase", ["probe", "pair", "validate"])
async def test_replacement_identity_mismatch_preserves_saved_entry(
    hass, replacement_factory, phase
) -> None:
    clients, calls, controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    other = replace(FixtureHubClient().info, hub_id="other-hub")
    if phase == "probe":
        controls["info"] = other
    result = await _start_replacement(hass, entry)
    if phase != "probe":
        result = await _approve_replacement(hass, result)
        if phase == "pair":
            controls["info"] = other
        else:
            snapshot = FixtureHubClient().snapshot
            controls["snapshot"] = replace(snapshot, info=other)
        result = await hass.config_entries.flow.async_configure(
            result["flow_id"], PAIRING_INPUT
        )
    assert result["reason"] == "wrong_hub"
    assert dict(entry.data) == original
    _assert_replacement_clients(clients, calls, 0 if phase == "probe" else 1)


@pytest.mark.parametrize(
    ("pair_error", "expected_error"),
    [
        (HubPairingError("rejected"), "invalid_pairing_secret"),
        (ProtocolContractUnavailable("not ready"), "protocol_not_ready"),
    ],
)
async def test_replacement_invitation_rejection_is_repairable(
    hass, replacement_factory, pair_error, expected_error
):
    clients, calls, controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await _approve_replacement(hass, await _start_replacement(hass, entry))
    controls["pair_error"] = pair_error
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["step_id"] == "reconfigure_pair"
    assert result["errors"] == {"base": expected_error}
    assert dict(entry.data) == original
    controls.clear()
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["reason"] == "reconfigure_successful"
    _assert_replacement_clients(clients, calls, 2)


async def test_new_endpoint_input_invalidates_previous_approval(
    hass, replacement_factory
):
    clients, calls, _controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await _approve_replacement(hass, await _start_replacement(hass, entry))
    flow = hass.config_entries.flow._progress[result["flow_id"]]
    result = await flow.async_step_reconfigure_confirm(
        REPLACEMENT_INPUT | {CONF_HOST: "another.local", CONF_TLS_PIN: "e" * 64}
    )
    assert result["step_id"] == "reconfigure_tls"
    assert result["description_placeholders"]["host"] == "another.local"
    result = await flow.async_step_reconfigure_pair(PAIRING_INPUT)
    assert result["reason"] == "replacement_not_approved"
    assert dict(entry.data) == original
    _assert_replacement_clients(clients, calls, 0)


@pytest.mark.parametrize("phase", ["probe", "pair", "validate"])
async def test_cancelled_replacement_closes_client_without_mutating_entry(
    hass, replacement_factory, phase
) -> None:
    clients, calls, _controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    started = asyncio.Event()

    async def blocked(_client, *args, **kwargs):
        started.set()
        await asyncio.Event().wait()

    if phase == "probe":
        with patch.object(StrictFlowClient, "async_probe", new=blocked):
            task = asyncio.create_task(_start_replacement(hass, entry))
            await asyncio.wait_for(started.wait(), timeout=1)
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
    else:
        result = await _approve_replacement(hass, await _start_replacement(hass, entry))
        method = "async_pair" if phase == "pair" else "async_snapshot"
        with patch.object(StrictFlowClient, method, new=blocked):
            task = asyncio.create_task(
                hass.config_entries.flow.async_configure(
                    result["flow_id"], PAIRING_INPUT
                )
            )
            await asyncio.wait_for(started.wait(), timeout=1)
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
    assert dict(entry.data) == original
    _assert_replacement_clients(clients, calls, 1 if phase == "validate" else 0)


async def test_replacement_token_expiring_during_retry_returns_to_pairing(
    hass, replacement_factory
) -> None:
    clients, calls, controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await _approve_replacement(hass, await _start_replacement(hass, entry))
    controls["snapshot_error"] = HubConnectionError("timeout")
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["step_id"] == "reconfigure_validate"
    with patch(
        "custom_components.teslatlas_hub.config_flow.epoch_milliseconds",
        return_value=FIXTURE_EXPIRES_AT_MS,
    ):
        result = await hass.config_entries.flow.async_configure(result["flow_id"], {})
    assert result["step_id"] == "reconfigure_pair"
    assert result["errors"] == {"base": "invalid_auth"}
    assert dict(entry.data) == original
    _assert_replacement_clients(clients, calls, 1)
    assert len(clients) == 3
    controls.clear()
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )
    assert result["reason"] == "reconfigure_successful"
    _assert_replacement_clients(clients, calls, 2)


async def test_replacement_token_expiring_during_read_is_not_persisted(
    hass, replacement_factory
) -> None:
    clients, calls, _controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await _approve_replacement(hass, await _start_replacement(hass, entry))
    with patch(
        "custom_components.teslatlas_hub.config_flow.epoch_milliseconds",
        side_effect=[FIXTURE_EXPIRES_AT_MS - 1, FIXTURE_EXPIRES_AT_MS],
    ):
        result = await hass.config_entries.flow.async_configure(
            result["flow_id"], PAIRING_INPUT
        )
    assert result["step_id"] == "reconfigure_pair"
    assert result["errors"] == {"base": "invalid_auth"}
    assert dict(entry.data) == original
    _assert_replacement_clients(clients, calls, 1)
    assert len(clients) == 3


@pytest.mark.parametrize("change", ["approval", "endpoint", "pending", "hub_id"])
async def test_replacement_rechecks_approval_and_identity_after_validation(
    hass, replacement_factory, change
) -> None:
    clients, calls, _controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await _approve_replacement(hass, await _start_replacement(hass, entry))
    flow = hass.config_entries.flow._progress[result["flow_id"]]
    started = asyncio.Event()
    release = asyncio.Event()

    async def held_snapshot(client):
        started.set()
        await release.wait()
        return client.snapshot

    with patch.object(StrictFlowClient, "async_snapshot", new=held_snapshot):
        task = asyncio.create_task(
            hass.config_entries.flow.async_configure(result["flow_id"], PAIRING_INPUT)
        )
        await asyncio.wait_for(started.wait(), timeout=1)
        if change == "approval":
            flow._replacement_approved = False
        elif change == "endpoint":
            flow._replacement_endpoint = replace(
                flow._replacement_endpoint, host="other.local"
            )
        elif change == "pending":
            flow._pending_pairing_result = None
        else:
            hass.config_entries.async_update_entry(
                entry, data=original | {CONF_HUB_ID: "other-hub"}
            )
        release.set()
        result = await task
    assert result["reason"] == (
        "wrong_hub" if change == "hub_id" else "replacement_not_approved"
    )
    expected = original | ({CONF_HUB_ID: "other-hub"} if change == "hub_id" else {})
    assert dict(entry.data) == expected
    _assert_replacement_clients(clients, calls, 1)


@pytest.mark.parametrize("phase", ["pair", "close"])
@pytest.mark.parametrize("wrong_hub", [False, True])
async def test_stale_pair_completion_never_uses_or_clears_new_candidate(
    hass, replacement_factory, phase, wrong_hub
) -> None:
    clients, calls, _controls = replacement_factory
    entry = _entry(hass)
    original = dict(entry.data)
    result = await _approve_replacement(hass, await _start_replacement(hass, entry))
    flow = hass.config_entries.flow._progress[result["flow_id"]]
    started = asyncio.Event()
    release = asyncio.Event()
    newer_endpoint = replace(flow._replacement_endpoint, host="newer.local")
    newer_result = await FixtureHubClient().async_pair(
        PAIRING_INPUT[CONF_PAIRING_ID],
        PAIRING_INPUT[CONF_PAIRING_SECRET],
        PAIRING_INPUT[CONF_DEVICE_NAME],
    )

    async def held_pair(client, *args, **kwargs):
        issued = await FixtureHubClient.async_pair(client, *args, **kwargs)
        if phase == "pair":
            started.set()
            await release.wait()
        if wrong_hub:
            raise HubIdentityError("changed")
        return issued

    async def held_close(client):
        if phase == "close":
            started.set()
            await release.wait()
        await FixtureHubClient.async_close(client)

    with (
        patch.object(StrictFlowClient, "async_pair", new=held_pair),
        patch.object(StrictFlowClient, "async_close", new=held_close),
    ):
        task = asyncio.create_task(
            hass.config_entries.flow.async_configure(result["flow_id"], PAIRING_INPUT)
        )
        await asyncio.wait_for(started.wait(), timeout=1)
        flow._replacement_endpoint = newer_endpoint
        flow._replacement_approved = True
        flow._pending_pairing_result = newer_result
        release.set()
        result = await task
    assert result["reason"] == "replacement_not_approved"
    assert flow._replacement_endpoint is newer_endpoint
    assert flow._replacement_approved is True
    assert flow._pending_pairing_result is newer_result
    assert dict(entry.data) == original
    assert len(clients) == 2
    _assert_replacement_clients(clients, calls, 1)


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
        CONF_ACCESS_TOKEN: FIXTURE_ACCESS_TOKEN,
        CONF_DEVICE_ID: FIXTURE_DEVICE_ID,
        CONF_TOKEN_EXPIRES_AT_MS: FIXTURE_EXPIRES_AT_MS,
    }
    assert fixture_client.pairing_ids == [PAIRING_INPUT[CONF_PAIRING_ID]]
    assert fixture_client.pairing_secrets == [PAIRING_INPUT[CONF_PAIRING_SECRET]]
    assert fixture_client.device_names == ["Home Assistant"]
    assert CONF_PAIRING_ID not in result["data"]
    assert CONF_PAIRING_SECRET not in result["data"]
    assert client_factory.call_count == 3
    assert client_factory.call_args.kwargs == {
        "bearer_token": FIXTURE_ACCESS_TOKEN,
        "expected_hub_id": "hub-fixture",
        "bearer_expires_at_ms": FIXTURE_EXPIRES_AT_MS,
        "hass": hass,
    }
    assert fixture_client.snapshot_calls == 1


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


async def test_user_flow_retries_issued_bearer_without_duplicate_claim(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """A transient validation failure retains one claim only in flow memory."""
    fixture_client.snapshot_error = HubConnectionError("offline")
    result = await _start_pair(hass)
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "pair_validate"
    assert result["errors"] == {"base": "cannot_connect"}
    assert hass.config_entries.async_entries(DOMAIN) == []
    assert client_factory.call_args.kwargs["bearer_token"] == FIXTURE_ACCESS_TOKEN
    assert fixture_client.pairing_ids == [PAIRING_INPUT[CONF_PAIRING_ID]]

    fixture_client.snapshot_error = None
    result = await hass.config_entries.flow.async_configure(result["flow_id"], {})

    assert result["type"] is FlowResultType.CREATE_ENTRY
    assert result["data"][CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
    assert fixture_client.pairing_ids == [PAIRING_INPUT[CONF_PAIRING_ID]]
    assert fixture_client.snapshot_calls == 2


async def test_user_flow_validation_identity_change_aborts_without_entry(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Never persist a claimed bearer after authenticated identity divergence."""
    del client_factory
    fixture_client.snapshot = replace(
        fixture_client.snapshot,
        info=replace(fixture_client.snapshot.info, hub_id="other-hub"),
    )
    result = await _start_pair(hass)
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "wrong_hub"
    assert hass.config_entries.async_entries(DOMAIN) == []
    assert fixture_client.pairing_ids == [PAIRING_INPUT[CONF_PAIRING_ID]]


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
    assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
    assert entry.data[CONF_DEVICE_ID] == FIXTURE_DEVICE_ID
    assert entry.data[CONF_HOST] == "old-hub.local"
    assert client_factory.call_args.kwargs["expected_hub_id"] == "hub-fixture"
    assert fixture_client.closed is True
    assert fixture_client.snapshot_calls == 1


async def test_reauth_retries_issued_bearer_without_duplicate_claim(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Keep the old entry and retry one issued replacement entirely in memory."""
    entry = _entry(hass)
    fixture_client.snapshot_error = HubConnectionError("offline")
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
        data=entry.data,
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"], PAIRING_INPUT
    )

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "reauth_validate"
    assert result["errors"] == {"base": "cannot_connect"}
    assert entry.data[CONF_ACCESS_TOKEN] == "old-device-bearer"
    assert entry.data[CONF_DEVICE_ID] == "old-device"
    assert client_factory.call_args.kwargs["bearer_token"] == FIXTURE_ACCESS_TOKEN
    assert fixture_client.pairing_ids == [PAIRING_INPUT[CONF_PAIRING_ID]]

    fixture_client.snapshot_error = None
    result = await hass.config_entries.flow.async_configure(result["flow_id"], {})

    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "reauth_successful"
    assert entry.data[CONF_ACCESS_TOKEN] == FIXTURE_ACCESS_TOKEN
    assert entry.data[CONF_DEVICE_ID] == FIXTURE_DEVICE_ID
    assert fixture_client.pairing_ids == [PAIRING_INPUT[CONF_PAIRING_ID]]
    assert fixture_client.snapshot_calls == 2


async def test_reauth_validation_identity_change_keeps_old_entry(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Authenticated identity divergence cannot update a reauth entry."""
    del client_factory
    entry = _entry(hass)
    fixture_client.snapshot = replace(
        fixture_client.snapshot,
        info=replace(fixture_client.snapshot.info, hub_id="other-hub"),
    )
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
    assert entry.data[CONF_ACCESS_TOKEN] == "old-device-bearer"
    assert entry.data[CONF_DEVICE_ID] == "old-device"
    assert fixture_client.pairing_ids == [PAIRING_INPUT[CONF_PAIRING_ID]]


async def test_reauth_rejects_a_different_hub(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    entry = _entry(hass)
    fixture_client.info = replace(fixture_client.info, hub_id="other-hub")
    fixture_client.pair_error = HubIdentityError("wrong hub")
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
    assert fixture_client.closed is True


async def test_cancelled_reauth_closes_owned_client(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Canceling a credential claim must release its transient client."""
    del client_factory
    entry = _entry(hass)
    started = asyncio.Event()

    async def blocked_pair(
        _pairing_id: str,
        _pairing_secret: str,
        _device_name: str,
    ) -> object:
        started.set()
        await asyncio.Event().wait()
        raise AssertionError("unreachable")

    fixture_client.async_pair = blocked_pair  # type: ignore[method-assign]
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_REAUTH, "entry_id": entry.entry_id},
        data=entry.data,
    )
    configure = asyncio.create_task(
        hass.config_entries.flow.async_configure(result["flow_id"], PAIRING_INPUT)
    )
    await asyncio.wait_for(started.wait(), timeout=1)
    configure.cancel()
    with pytest.raises(asyncio.CancelledError):
        await configure
    assert fixture_client.closed is True


async def test_cancelled_probe_closes_owned_client(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Canceling discovery must release the client it created."""
    del client_factory
    started = asyncio.Event()

    async def blocked_probe() -> object:
        started.set()
        await asyncio.Event().wait()
        raise AssertionError("unreachable")

    fixture_client.async_probe = blocked_probe  # type: ignore[method-assign]
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": SOURCE_USER}
    )
    configure = asyncio.create_task(
        hass.config_entries.flow.async_configure(
            result["flow_id"],
            {
                CONF_HOST: "hub-fixture.local",
                CONF_PORT: 7443,
                CONF_USE_TLS: True,
            },
        )
    )
    await asyncio.wait_for(started.wait(), timeout=1)
    configure.cancel()
    with pytest.raises(asyncio.CancelledError):
        await configure
    assert fixture_client.closed is True


async def test_cancelled_reconfigure_closes_probed_client(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Canceling endpoint identity binding must release the probed client."""
    del client_factory
    entry = _entry(hass)
    started = asyncio.Event()

    async def blocked_unique_id(_flow, _hub_id: str) -> None:
        started.set()
        await asyncio.Event().wait()

    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": entry.entry_id},
        data=entry.data,
    )
    with patch(
        "custom_components.teslatlas_hub.config_flow.TeslatlasHubConfigFlow.async_set_unique_id",
        new=blocked_unique_id,
    ):
        configure = asyncio.create_task(
            hass.config_entries.flow.async_configure(
                result["flow_id"],
                {
                    CONF_HOST: "new-hub.local",
                    CONF_PORT: 8443,
                    CONF_USE_TLS: True,
                },
            )
        )
        await asyncio.wait_for(started.wait(), timeout=1)
        configure.cancel()
        with pytest.raises(asyncio.CancelledError):
            await configure
    assert fixture_client.closed is True


async def test_reconfigure_same_pinned_origin_authenticates_before_atomic_update(
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
            CONF_TLS_PIN: "c" * 64,
        },
    )
    assert result["reason"] == "reconfigure_successful"
    assert entry.data[CONF_HOST] == "new-hub.local"
    assert entry.data[CONF_ACCESS_TOKEN] == "old-device-bearer"
    assert client_factory.call_args.kwargs["bearer_token"] == "old-device-bearer"
    assert client_factory.call_count == 2


async def test_reconfigure_lookalike_hub_id_never_receives_saved_bearer(
    hass: HomeAssistant,
    client_factory,
) -> None:
    """A matching public Hub ID cannot substitute for the saved TLS binding."""
    entry = _entry(hass)
    result = await hass.config_entries.flow.async_init(
        DOMAIN,
        context={"source": SOURCE_RECONFIGURE, "entry_id": entry.entry_id},
        data=entry.data,
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_HOST: "lookalike-hub.local",
            CONF_PORT: 8443,
            CONF_USE_TLS: True,
            CONF_TLS_PIN: "d" * 64,
        },
    )

    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "reconfigure_tls"
    assert result["description_placeholders"]["tls_pin"] == "d" * 64
    assert entry.data[CONF_HOST] == "old-hub.local"
    assert entry.data[CONF_TLS_PIN] == "c" * 64
    assert client_factory.call_count == 1
    assert client_factory.call_args.kwargs["bearer_token"] is None


async def test_reconfigure_keeps_old_endpoint_when_saved_bearer_cannot_read_new_one(
    hass: HomeAssistant,
    fixture_client: FixtureHubClient,
    client_factory,
) -> None:
    """Identity discovery alone must not strand an entry on an unusable endpoint."""
    entry = _entry(hass)
    fixture_client.snapshot_error = HubAuthenticationError("not admitted")
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
            CONF_TLS_PIN: "c" * 64,
        },
    )

    assert result["type"] is FlowResultType.FORM
    assert result["errors"] == {"base": "invalid_auth"}
    assert entry.data[CONF_HOST] == "old-hub.local"
    assert entry.data[CONF_TLS_PIN] == "c" * 64
    assert client_factory.call_args.kwargs["bearer_token"] == "old-device-bearer"


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

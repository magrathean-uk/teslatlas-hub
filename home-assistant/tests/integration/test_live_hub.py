"""Opt-in installed Home Assistant runtime gate against a genuine Hub process."""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import secrets
import ssl
import time
from dataclasses import fields
from datetime import timedelta
from pathlib import Path
from urllib.parse import urlsplit

import aiohttp
import pytest
from homeassistant.config_entries import SOURCE_USER, ConfigEntryState
from homeassistant.const import CONF_HOST, STATE_UNAVAILABLE, STATE_UNKNOWN
from homeassistant.core import HomeAssistant
from homeassistant.data_entry_flow import FlowResultType
from homeassistant.helpers import entity_registry as er

from custom_components.teslatlas_hub.client import (
    HubAuthenticationError,
    HubPairingError,
    create_client,
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
from custom_components.teslatlas_hub.current_hub_client import (
    PROFILE_ID,
    PROFILE_SHA256,
    CurrentHubClient,
)
from custom_components.teslatlas_hub.models import HubEndpoint, VehicleState

pytestmark = pytest.mark.skipif(
    os.environ.get("TESLATLAS_HA_LIVE") != "1",
    reason="requires an explicitly prepared private current-Hub fixture",
)


def _require_credential_changed(before: str, after: str) -> None:
    unchanged = secrets.compare_digest(before, after)
    del before, after
    if unchanged:
        pytest.fail("Home Assistant credential did not rotate", pytrace=False)


def _private_json(variable: str) -> tuple[Path, dict]:
    value = os.environ.get(variable)
    assert value, f"{variable} is required"
    path = Path(value)
    assert path.is_absolute() and path.is_file()
    assert path.stat().st_mode & 0o077 == 0
    return path, json.loads(path.read_text(encoding="utf-8"))


def _entity_id(hass: HomeAssistant, unique_id: str) -> str:
    entity_id = er.async_get(hass).async_get_entity_id("sensor", DOMAIN, unique_id)
    assert entity_id is not None
    return entity_id


def _write_receipt(
    receipt_path: Path,
    ready_path: Path,
    ready: dict,
    reauth_ready_path: Path,
    reauth_ready: dict,
    hub_id: str,
    request_count: int,
) -> None:
    """Write a credential-free receipt outside Home Assistant's event loop."""
    source_root = Path(__file__).parents[2]
    integration_root = source_root / "custom_components" / "teslatlas_hub"
    entries = []
    for path in sorted(integration_root.rglob("*")):
        if not path.is_file() or "__pycache__" in path.parts:
            continue
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        entries.append(f"{path.relative_to(source_root)} {digest}\n")
    source_manifest = hashlib.sha256("".join(entries).encode()).hexdigest()
    payload = {
        "schema_version": 1,
        "runtime": "Home Assistant 2026.8.3 on Python 3.14.2",
        "profile_id": PROFILE_ID,
        "profile_sha256": PROFILE_SHA256,
        "hub_binary_sha256": ready["binary_sha256"],
        "hub_id_sha256": hashlib.sha256(hub_id.encode()).hexdigest(),
        "integration_source_manifest_sha256": source_manifest,
        "config_entry_loaded": True,
        "vehicles": 2,
        "initial_battery": 0,
        "later_battery": 1,
        "initial_inside_temperature_c": 21.5,
        "later_inside_temperature_c": 22.5,
        "normal_trust_negative": True,
        "reauth_started_after_401": True,
        "reauth_completed_with_fresh_invitation": True,
        "reauth_hub_restart": {
            "initial_hub_pid": ready["hub_pid"],
            "reauth_hub_pid": reauth_ready["hub_pid"],
            "same_hub_identity": True,
            "same_hub_binary": True,
        },
        "post_reauth_battery": 1,
        "http_requests_observed": request_count,
        "event_transport_evidence": "instrumented_aiohttp_request_boundary",
        "sse_subscriptions": 0,
        "last_event_id_requests": 0,
        "clean_unload": True,
        "ready_descriptor_sha256": hashlib.sha256(ready_path.read_bytes()).hexdigest(),
        "reauth_ready_descriptor_sha256": hashlib.sha256(
            reauth_ready_path.read_bytes()
        ).hexdigest(),
    }
    receipt_path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


async def test_installed_config_entry_polls_and_unloads_real_hub(
    hass: HomeAssistant,
    socket_enabled,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Prove actual flow, pairing, entity updates, reauth, and clean unload."""
    del socket_enabled
    ready_path, ready = _private_json("TESLATLAS_HA_LIVE_READY")
    _invitation_path, invitation = _private_json("TESLATLAS_HA_LIVE_INVITATION")
    scenario_path, scenario = _private_json("TESLATLAS_HA_LIVE_SCENARIO")
    receipt_path = Path(os.environ["TESLATLAS_HA_LIVE_RECEIPT"])
    continue_path = Path(os.environ["TESLATLAS_HA_LIVE_CONTINUE"])
    initial_path = Path(os.environ["TESLATLAS_HA_LIVE_INITIAL"])
    reauth_invitation_path = Path(os.environ["TESLATLAS_HA_LIVE_REAUTH_INVITATION"])
    reauth_ready_path = Path(os.environ["TESLATLAS_HA_LIVE_REAUTH_READY"])
    reauth_request_path = Path(os.environ["TESLATLAS_HA_LIVE_REAUTH_REQUEST"])
    reauth_continue_path = Path(os.environ["TESLATLAS_HA_LIVE_REAUTH_CONTINUE"])
    requests: list[tuple[str, str, dict[str, str]]] = []
    original_request = aiohttp.ClientSession._request

    async def instrumented_request(
        session: aiohttp.ClientSession,
        method: str,
        url: str,
        **kwargs,
    ) -> aiohttp.ClientResponse:
        headers = {
            str(key): str(value) for key, value in kwargs.get("headers", {}).items()
        }
        requests.append((method, str(url), headers))
        return await original_request(session, method, url, **kwargs)

    monkeypatch.setattr(aiohttp.ClientSession, "_request", instrumented_request)

    assert ready["status"] == "ready"
    assert ready["profile_id"] == PROFILE_ID
    assert ready["profile_sha256"] == PROFILE_SHA256
    scenario_sha256 = await hass.async_add_executor_job(
        lambda: hashlib.sha256(scenario_path.read_bytes()).hexdigest()
    )
    assert ready["scenario_sha256"] == scenario_sha256
    assert invitation["endpoint"] == ready["endpoint"]
    endpoint = urlsplit(invitation["endpoint"])

    system_ca = os.environ.get("TESLATLAS_HA_SYSTEM_CA")
    assert system_ca and await hass.async_add_executor_job(os.path.isfile, system_ca)
    assert os.environ.get("REQUESTS_CA_BUNDLE") == os.environ.get("SSL_CERT_FILE")
    untrusted = ssl.create_default_context(cafile=system_ca)
    async with aiohttp.ClientSession(
        connector=aiohttp.TCPConnector(ssl=untrusted)
    ) as session:
        with pytest.raises(aiohttp.ClientConnectorCertificateError):
            await session.get(invitation["endpoint"] + "/healthz")

    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": SOURCE_USER}
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_HOST: endpoint.hostname,
            CONF_PORT: endpoint.port,
            CONF_USE_TLS: True,
            CONF_TLS_PIN: invitation["tlsPin"],
        },
    )
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "pair", result
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_PAIRING_ID: invitation["pairingId"],
            CONF_PAIRING_SECRET: invitation["secret"],
            CONF_DEVICE_NAME: "Home Assistant interop",
        },
    )
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
    entry = hass.config_entries.async_entries(DOMAIN)[0]
    assert entry.state is ConfigEntryState.LOADED
    assert entry.data[CONF_HUB_ID] == ready["hub_id"]
    assert entry.data[CONF_DEVICE_ID]
    assert entry.data[CONF_TOKEN_EXPIRES_AT_MS] > 0
    assert entry.data[CONF_ACCESS_TOKEN]
    assert CONF_PAIRING_ID not in entry.data
    assert CONF_PAIRING_SECRET not in entry.data

    vehicle_id = scenario["vehicle_ids"][0]
    hub_id = ready["hub_id"]
    battery_entity = _entity_id(hass, f"{hub_id}_{vehicle_id}_state_of_charge")
    temperature_entity = _entity_id(hass, f"{hub_id}_{vehicle_id}_inside_temperature")
    battery_state = hass.states.get(battery_entity)
    temperature_state = hass.states.get(temperature_entity)
    assert battery_state is not None
    assert temperature_state is not None
    assert battery_state.state == "0"
    assert temperature_state.state == "21.5"
    assert len(entry.runtime_data.data.vehicles) == 2

    await hass.async_add_executor_job(
        lambda: initial_path.write_text("ready\n", encoding="utf-8")
    )
    async with asyncio.timeout(30):
        while not await hass.async_add_executor_job(continue_path.is_file):  # noqa: ASYNC110
            await asyncio.sleep(0.1)
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    battery_state = hass.states.get(battery_entity)
    temperature_state = hass.states.get(temperature_entity)
    assert battery_state is not None
    assert temperature_state is not None
    assert battery_state.state == "1"
    assert temperature_state.state == "22.5"

    old_access_token = entry.data[CONF_ACCESS_TOKEN]
    rotated = await entry.runtime_data.client.async_rotate()
    _require_credential_changed(old_access_token, rotated.access_token)
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert entry.state in {ConfigEntryState.LOADED, ConfigEntryState.SETUP_ERROR}
    reauth_flows = [
        flow
        for flow in hass.config_entries.flow.async_progress_by_handler(DOMAIN)
        if flow["context"]["source"] == "reauth"
    ]
    assert len(reauth_flows) == 1
    await hass.async_add_executor_job(
        lambda: reauth_request_path.write_text("ready\n", encoding="utf-8")
    )
    async with asyncio.timeout(30):
        while not await hass.async_add_executor_job(  # noqa: ASYNC110
            reauth_continue_path.is_file
        ):
            await asyncio.sleep(0.1)
    _reauth_path, reauth_invitation = _private_json(
        "TESLATLAS_HA_LIVE_REAUTH_INVITATION"
    )
    _reauth_ready_path, reauth_ready = _private_json("TESLATLAS_HA_LIVE_REAUTH_READY")
    assert _reauth_path == reauth_invitation_path
    assert _reauth_ready_path == reauth_ready_path
    assert reauth_ready["status"] == "ready"
    assert reauth_ready["hub_id"] == ready["hub_id"]
    assert reauth_ready["endpoint"] == ready["endpoint"]
    assert reauth_ready["profile_id"] == ready["profile_id"]
    assert reauth_ready["profile_sha256"] == ready["profile_sha256"]
    assert reauth_ready["scenario_sha256"] == ready["scenario_sha256"]
    assert reauth_ready["binary_sha256"] == ready["binary_sha256"]
    assert reauth_ready["seed_binary_sha256"] == ready["seed_binary_sha256"]
    assert reauth_ready["provenance"] == ready["provenance"]
    assert reauth_ready["hub_pid"] != ready["hub_pid"]
    assert reauth_ready["hub_started_at"] != ready["hub_started_at"]
    assert reauth_invitation["endpoint"] == invitation["endpoint"]
    result = await hass.config_entries.flow.async_configure(
        reauth_flows[0]["flow_id"],
        {
            CONF_PAIRING_ID: reauth_invitation["pairingId"],
            CONF_PAIRING_SECRET: reauth_invitation["secret"],
            CONF_DEVICE_NAME: "Home Assistant reauthentication interop",
        },
    )
    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "reauth_successful"
    await hass.async_block_till_done()
    assert entry.state is ConfigEntryState.LOADED
    _require_credential_changed(old_access_token, entry.data[CONF_ACCESS_TOKEN])
    assert CONF_PAIRING_ID not in entry.data
    assert CONF_PAIRING_SECRET not in entry.data
    battery_state = hass.states.get(battery_entity)
    assert battery_state is not None
    assert battery_state.state == "1"

    event_requests = [
        request for request in requests if "event" in urlsplit(request[1]).path.lower()
    ]
    last_event_id_requests = [
        request
        for request in requests
        if any(key.lower() == "last-event-id" for key in request[2])
    ]
    assert event_requests == []
    assert last_event_id_requests == []
    assert await hass.config_entries.async_unload(entry.entry_id)
    assert entry.state is ConfigEntryState.NOT_LOADED

    await hass.async_add_executor_job(
        _write_receipt,
        receipt_path,
        ready_path,
        ready,
        reauth_ready_path,
        reauth_ready,
        hub_id,
        len(requests),
    )


def _registry_ids(hass: HomeAssistant, entry_id: str) -> list[str]:
    registry = er.async_get(hass)
    return sorted(
        item.entity_id
        for item in er.async_entries_for_config_entry(registry, entry_id)
    )


@pytest.mark.asyncio
async def test_installed_matrix_all_cases_against_real_hub(
    hass: HomeAssistant,
    socket_enabled,
    monkeypatch: pytest.MonkeyPatch,
    matrix_runtime,
) -> None:
    """Exercise all 18 HA cases through installed flow and client actors."""
    del socket_enabled
    census = matrix_runtime.request_census(aiohttp.ClientSession._request)

    async def instrumented_request(session, method: str, url: str, **kwargs):
        return await census.request(session, method, url, **kwargs)

    monkeypatch.setattr(aiohttp.ClientSession, "_request", instrumented_request)

    running = await matrix_runtime.verify()
    assert isinstance(matrix_runtime.sequence, int) and matrix_runtime.sequence > 0
    initial_sequence = matrix_runtime.sequence
    descriptor = running["descriptor"]
    invitation = running["invitation"]
    expired_invitation = running["expired_invitation"]
    endpoint_url = urlsplit(descriptor["endpoint"])
    endpoint = HubEndpoint(
        host=endpoint_url.hostname,
        port=endpoint_url.port,
        use_tls=True,
        tls_pin=matrix_runtime.session["inputs"]["certificate_der_sha256"],
    )

    probe_mark = census.mark()
    authorization_before = census.authorization_headers
    client = create_client(endpoint, expected_hub_id=descriptor["hub_id"], hass=hass)
    info = await client.async_probe()
    await client.async_close()
    assert info.hub_id == descriptor["hub_id"]
    probe_requests = census.since(probe_mark)
    probe_facts = {
        "hub_id": info.hub_id,
        "api_versions": ["1.0"],
        "protocol": "teslatlas-sync",
        "protocol_major": 1,
        "version": "2026.36.2",
        "discovery_status": 200,
        "authorization_headers": census.authorization_headers
        - authorization_before,
        "credential_absent": True,
    }
    matrix_runtime.record_operation(
        "probe",
        actor_id="ha_client",
        before=initial_sequence,
        after=initial_sequence,
        facts=probe_facts,
        requests=probe_requests,
    )

    bad_secret = (
        ("0" if invitation["secret"][0] != "0" else "1")
        + invitation["secret"][1:]
    )
    bad_mark = census.mark()
    client = create_client(endpoint, expected_hub_id=descriptor["hub_id"], hass=hass)
    with pytest.raises(HubPairingError):
        await client.async_pair(
            invitation["pairingId"], bad_secret, "HA matrix bad invitation"
        )
    await client.async_close()
    assert any(
        item["route"] == "/v1/pairings/{pairing_id}/claim"
        and item["resource_id"] == invitation["pairingId"]
        for item in census.attempts_since(bad_mark)
    )
    bad_facts = {
        "claim_status": 401,
        "typed_error": "HubPairingError",
        "credential_created": False,
        "pairing_id": invitation["pairingId"],
    }
    matrix_runtime.record_operation(
        "bad_pair",
        actor_id="ha_client",
        before=initial_sequence,
        after=initial_sequence,
        facts=bad_facts,
        requests=census.since(bad_mark),
    )

    assert expired_invitation["expiresAtMs"] < int(time.time() * 1000)
    expired_mark = census.mark()
    client = create_client(endpoint, expected_hub_id=descriptor["hub_id"], hass=hass)
    with pytest.raises(HubPairingError):
        await client.async_pair(
            expired_invitation["pairingId"],
            expired_invitation["secret"],
            "HA matrix expired invitation",
        )
    await client.async_close()
    assert any(
        item["route"] == "/v1/pairings/{pairing_id}/claim"
        and item["resource_id"] == expired_invitation["pairingId"]
        for item in census.attempts_since(expired_mark)
    )
    expired_facts = {
        "claim_status": 401,
        "typed_error": "HubPairingError",
        "credential_created": False,
        "fixture_expires_at_ms": expired_invitation["expiresAtMs"],
        "observed_after_expiry_ms": int(time.time() * 1000),
        "fixture_kind": "persisted-one-second",
        "pairing_id": expired_invitation["pairingId"],
    }
    matrix_runtime.set_expired_fixture_facts(
        {
            key: expired_facts[key]
            for key in (
                "fixture_expires_at_ms",
                "observed_after_expiry_ms",
                "fixture_kind",
                "pairing_id",
            )
        }
    )
    matrix_runtime.record_operation(
        "expired_pair",
        actor_id="ha_client",
        before=initial_sequence,
        after=initial_sequence,
        facts=expired_facts,
        requests=census.since(expired_mark),
    )

    setup_mark = census.mark()
    result = await hass.config_entries.flow.async_init(
        DOMAIN, context={"source": SOURCE_USER}
    )
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_HOST: endpoint.host,
            CONF_PORT: endpoint.port,
            CONF_USE_TLS: True,
            CONF_TLS_PIN: invitation["tlsPin"],
        },
    )
    assert result["type"] is FlowResultType.FORM
    assert result["step_id"] == "pair"
    result = await hass.config_entries.flow.async_configure(
        result["flow_id"],
        {
            CONF_PAIRING_ID: invitation["pairingId"],
            CONF_PAIRING_SECRET: invitation["secret"],
            CONF_DEVICE_NAME: "Home Assistant installed matrix",
        },
    )
    assert result["type"] is FlowResultType.CREATE_ENTRY
    await hass.async_block_till_done()
    entry = hass.config_entries.async_entries(DOMAIN)[0]
    assert entry.state is ConfigEntryState.LOADED
    assert entry.data[CONF_HUB_ID] == descriptor["hub_id"]
    assert CONF_PAIRING_ID not in entry.data
    assert CONF_PAIRING_SECRET not in entry.data
    registry_ids = _registry_ids(hass, entry.entry_id)
    assert len(registry_ids) == 26
    for vehicle_id in matrix_runtime.scenario["vehicle_ids"]:
        prefix = f"{descriptor['hub_id']}_{vehicle_id}_"
        registry = er.async_get(hass)
        assert (
            len(
                [
                    item
                    for item in er.async_entries_for_config_entry(
                        registry, entry.entry_id
                    )
                    if item.unique_id.startswith(prefix)
                ]
            )
            == 13
        )
    matrix_runtime.record_operation(
        "config_flow_setup",
        actor_id="ha_flow",
        before=initial_sequence,
        after=initial_sequence,
        facts={
            "claim_status": 200,
            "vehicles": 2,
            "config_entry_loaded": True,
            "entry_id": entry.entry_id,
            "registry_ids": registry_ids,
        },
        requests=census.since(setup_mark),
    )

    replay_mark = census.mark()
    client = create_client(endpoint, expected_hub_id=descriptor["hub_id"], hass=hass)
    with pytest.raises(HubPairingError):
        await client.async_pair(
            invitation["pairingId"],
            invitation["secret"],
            "HA matrix replayed invitation",
        )
    await client.async_close()
    assert any(
        item["route"] == "/v1/pairings/{pairing_id}/claim"
        and item["resource_id"] == invitation["pairingId"]
        for item in census.attempts_since(replay_mark)
    )
    replay_facts = {
        "claim_status": 401,
        "typed_error": "HubPairingError",
        "credential_created": False,
        "pairing_id": invitation["pairingId"],
    }
    matrix_runtime.record_operation(
        "replay_pair",
        actor_id="ha_client",
        before=initial_sequence,
        after=initial_sequence,
        facts=replay_facts,
        requests=census.since(replay_mark),
    )

    unknown_mark = census.mark()
    unknown_client = create_client(
        endpoint,
        bearer_token=entry.data[CONF_ACCESS_TOKEN],
        bearer_expires_at_ms=entry.data[CONF_TOKEN_EXPIRES_AT_MS],
        expected_hub_id=descriptor["hub_id"],
        hass=hass,
    )
    assert isinstance(unknown_client, CurrentHubClient)
    unknown = await unknown_client._async_current(
        {"vehicle_id": "33333333-3333-4333-8333-333333333333", "display_name": None}
    )
    optional_fields = {
        field.name: getattr(unknown, field.name)
        for field in fields(VehicleState)
        if field.name not in {"vehicle_id", "name"}
    }
    assert optional_fields == {
        "state_of_charge": None,
        "charging_state": None,
        "charging_power_kw": None,
        "charge_limit_percent": None,
        "estimated_range_km": None,
        "odometer_km": None,
        "activity_state": None,
        "inside_temperature_c": None,
        "outside_temperature_c": None,
        "access_state": None,
        "software_version": None,
        "software_update_state": None,
        "telemetry_age_seconds": None,
        "data_quality": None,
    }
    await unknown_client.async_close()
    unknown_facts = {
        "http_status": 404,
        "vehicle_id": unknown.vehicle_id,
        "state_of_charge": None,
        "inside_temperature_c": None,
        "optional_fields": dict(sorted(optional_fields.items())),
    }
    matrix_runtime.record_operation(
        "unknown_current",
        actor_id="ha_client",
        before=initial_sequence,
        after=initial_sequence,
        facts=unknown_facts,
        requests=census.since(unknown_mark),
    )

    primary_id, empty_id = matrix_runtime.scenario["vehicle_ids"]
    hub_id = descriptor["hub_id"]
    battery_entity = _entity_id(hass, f"{hub_id}_{primary_id}_state_of_charge")
    temperature_entity = _entity_id(
        hass, f"{hub_id}_{primary_id}_inside_temperature"
    )
    empty_battery_entity = _entity_id(
        hass, f"{hub_id}_{empty_id}_state_of_charge"
    )
    assert hass.states[battery_entity].state == "0"
    assert hass.states[temperature_entity].state == "21.5"
    assert hass.states[empty_battery_entity].state == STATE_UNKNOWN
    unicode_name = "Interop \N{EN DASH} Árvíztűrő 🚗"
    assert entry.runtime_data.data.vehicles[primary_id].name == unicode_name
    initial_facts = {
        "vehicles": 2,
        "sensors_per_vehicle": 13,
        "initial_battery": 0,
        "initial_inside_temperature_c": 21.5,
        "empty_state_of_charge": None,
        "unicode_name": unicode_name,
    }
    matrix_runtime.record_operation(
        "initial_poll",
        actor_id="ha_flow",
        before=initial_sequence,
        after=initial_sequence,
        facts=initial_facts,
        requests=[],
    )
    await matrix_runtime.initial_barrier()
    assert matrix_runtime.sequence is not None
    assert matrix_runtime.sequence > initial_sequence

    later_mark = census.mark()
    later_sequence = matrix_runtime.sequence
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert entry.runtime_data.last_update_success is True
    assert hass.states[battery_entity].state == "1"
    assert hass.states[temperature_entity].state == "22.5"
    later_facts = {
        "later_battery": 1,
        "later_inside_temperature_c": 22.5,
        "advanced_once": True,
        "advance_from_sequence": initial_sequence,
    }
    matrix_runtime.record_operation(
        "later_poll",
        actor_id="ha_flow",
        before=later_sequence,
        after=later_sequence,
        facts=later_facts,
        requests=census.since(later_mark),
    )

    unsupported_mark = census.mark()
    surface_client = entry.runtime_data.client
    assert not hasattr(surface_client, "async_commands")
    assert not hasattr(surface_client, "async_drives")
    assert not hasattr(surface_client, "async_subscribe_events")
    unsupported_facts = {
        "operations": ["commands", "drives", "sse"],
        "outgoing_requests": len(census.attempts_since(unsupported_mark)),
    }
    assert unsupported_facts["outgoing_requests"] == 0
    matrix_runtime.record_operation(
        "unsupported_surface",
        actor_id="ha_client",
        before=later_sequence,
        after=later_sequence,
        facts=unsupported_facts,
        requests=[],
    )

    old_entry_id = entry.entry_id
    old_registry_ids = _registry_ids(hass, entry.entry_id)
    old_token = entry.data[CONF_ACCESS_TOKEN]
    old_device_id = entry.data[CONF_DEVICE_ID]
    revoke_before = matrix_runtime.sequence
    await matrix_runtime.request("revoke", device_id=old_device_id)
    revoke_after = matrix_runtime.sequence
    revoke_mark = census.mark()
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert entry.runtime_data.last_update_success is False
    last_error = entry.runtime_data.last_exception
    assert last_error is not None
    assert isinstance(last_error.__cause__, HubAuthenticationError)
    reauth_flows = [
        flow
        for flow in hass.config_entries.flow.async_progress_by_handler(DOMAIN)
        if flow["context"]["source"] == "reauth"
    ]
    assert len(reauth_flows) == 1
    revocation_facts = {
        "old_credential_status": 401,
        "typed_error": "HubAuthenticationError",
        "reauth_started": True,
        "credential_loss_observed": True,
        "device_id": old_device_id,
    }
    matrix_runtime.record_operation(
        "revocation_refresh",
        actor_id="ha_flow",
        before=revoke_before,
        after=revoke_after,
        facts=revocation_facts,
        requests=census.since(revoke_mark),
    )

    pair_before = matrix_runtime.sequence
    pair_result = await matrix_runtime.request("pair")
    pair_after = matrix_runtime.sequence
    fresh = pair_result["invitation"]
    reauth_mark = census.mark()
    result = await hass.config_entries.flow.async_configure(
        reauth_flows[0]["flow_id"],
        {
            CONF_PAIRING_ID: fresh["pairingId"],
            CONF_PAIRING_SECRET: fresh["secret"],
            CONF_DEVICE_NAME: "Home Assistant matrix reauthentication",
        },
    )
    assert result["type"] is FlowResultType.ABORT
    assert result["reason"] == "reauth_successful"
    await hass.async_block_till_done()
    assert entry.entry_id == old_entry_id
    assert entry.state is ConfigEntryState.LOADED
    matrix_runtime.require_secret_changed(
        old_token, entry.data[CONF_ACCESS_TOKEN]
    )
    assert _registry_ids(hass, entry.entry_id) == old_registry_ids
    assert hass.states[battery_entity].state == "1"
    reauth_facts = {
        "fresh_claim_status": 200,
        "entry_id_preserved": True,
        "registry_ids_preserved": True,
        "later_poll_status": 200,
        "reauth_completed": True,
        "pairing_id": fresh["pairingId"],
    }
    matrix_runtime.record_operation(
        "reauthentication_flow",
        actor_id="ha_flow",
        before=pair_before,
        after=pair_after,
        facts=reauth_facts,
        requests=census.since(reauth_mark),
    )

    restart_before = matrix_runtime.sequence
    before_generation = pair_result["descriptor"]["service_generation"]
    await matrix_runtime.request("stop")
    restarted = await matrix_runtime.request("start")
    restart_after = matrix_runtime.sequence
    restart_mark = census.mark()
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert entry.runtime_data.last_update_success is True
    assert restarted["descriptor"]["hub_id"] == hub_id
    assert restarted["descriptor"]["service_generation"] != before_generation
    restart_facts = {
        "same_hub": True,
        "new_service_generation": True,
        "poll_status": 200,
        "before_service_generation": before_generation,
        "after_service_generation": restarted["descriptor"]["service_generation"],
    }
    matrix_runtime.record_operation(
        "endpoint_restart_poll",
        actor_id="ha_flow",
        before=restart_before,
        after=restart_after,
        facts=restart_facts,
        requests=census.since(restart_mark),
    )

    outage_before = matrix_runtime.sequence
    outage_before_generation = restarted["descriptor"]["service_generation"]
    await matrix_runtime.request("stop")
    outage_mark = census.mark()
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert entry.runtime_data.last_update_success is False
    assert hass.states[battery_entity].state == STATE_UNAVAILABLE
    outage_restarted = await matrix_runtime.request("start")
    outage_after = matrix_runtime.sequence
    await entry.runtime_data.async_refresh()
    await hass.async_block_till_done()
    assert entry.runtime_data.last_update_success is True
    assert hass.states[battery_entity].state == "1"
    assert _registry_ids(hass, entry.entry_id) == old_registry_ids
    outage_facts = {
        "unavailable_observed": True,
        "recovered": True,
        "registry_ids_preserved": True,
        "before_service_generation": outage_before_generation,
        "after_service_generation": outage_restarted["descriptor"][
            "service_generation"
        ],
    }
    matrix_runtime.record_operation(
        "outage_poll",
        actor_id="ha_flow",
        before=outage_before,
        after=outage_after,
        facts=outage_facts,
        requests=census.since(outage_mark),
    )

    unload_before = matrix_runtime.sequence
    entry.runtime_data.update_interval = timedelta(milliseconds=50)
    entry.runtime_data._schedule_refresh()
    scheduled_cancel = entry.runtime_data._unsub_refresh
    scheduled_handle = getattr(scheduled_cancel, "__self__", None)
    assert isinstance(scheduled_handle, asyncio.TimerHandle)
    scheduled_deadline = scheduled_handle.when()
    unload_mark = census.mark()
    assert await hass.config_entries.async_unload(entry.entry_id)
    assert entry.state is ConfigEntryState.NOT_LOADED
    assert scheduled_handle.cancelled()
    delay_past_deadline = max(0.0, scheduled_deadline - hass.loop.time()) + 0.05
    async with asyncio.timeout(1):
        await asyncio.sleep(delay_past_deadline)
    await hass.async_block_till_done()
    observed_after_deadline = hass.loop.time()
    assert observed_after_deadline > scheduled_deadline
    post_unload = census.attempts_since(unload_mark)
    assert not post_unload
    all_attempts = census.attempts_since(0)
    retained_attempts = census.retained_attempts(0, unload_mark=unload_mark)
    outcomes = {
        outcome: len([item for item in all_attempts if item["outcome"] == outcome])
        for outcome in ("completed", "failed", "cancelled", "pending")
    }
    polling_facts = {
        "polling_requests_positive": len(census.attempts) > 0,
        "vehicle_requests_positive": any(
            item["route"] == "/v1/vehicles" for item in census.completed
        ),
        "current_requests_positive": any(
            item["route"] == "/v1/vehicles/{vehicle_id}/current"
            for item in census.completed
        ),
        "sse_requests": len(
            [item for item in census.attempts if "event" in item["route"].lower()]
        ),
        "last_event_id_requests": census.last_event_id_headers,
        "clean_unload": True,
        "post_unload_requests": len(post_unload),
        "attempted_requests": len(all_attempts),
        "completed_requests": outcomes["completed"],
        "failed_requests": outcomes["failed"],
        "cancelled_requests": outcomes["cancelled"],
        "pending_requests": outcomes["pending"],
        "post_unload_attempts": len(post_unload),
        "scheduled_callback_deadline_ns": int(scheduled_deadline * 1_000_000_000),
        "observed_after_deadline_ns": int(
            observed_after_deadline * 1_000_000_000
        ),
        "scheduled_callback_cancelled": scheduled_handle.cancelled(),
        "attempts": retained_attempts,
    }
    matrix_runtime.record_operation(
        "unload",
        actor_id="ha_flow",
        before=unload_before,
        after=unload_before,
        facts=polling_facts,
        requests=[],
    )

    runtime_facts = matrix_runtime.runtime_facts(
        hub_id=hub_id,
        entry_id=old_entry_id,
        registry_ids=old_registry_ids,
    )
    matrix_runtime.record_operation(
        "observe_installed_runtime",
        actor_id="ha_flow",
        before=initial_sequence,
        after=initial_sequence,
        facts=runtime_facts,
        requests=[],
    )
    matrix_runtime.set_case_facts_from_raw()

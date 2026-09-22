"""Production HTTP adapter tests for the frozen current-Hub profile."""

from __future__ import annotations

import asyncio
import hashlib
import json
import ssl
from datetime import UTC, datetime, timedelta
from ipaddress import ip_address
from pathlib import Path
from typing import Any

import pytest
from aiohttp import ClientSession, ClientTimeout, TCPConnector, web
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID
from homeassistant.core import HomeAssistant
from homeassistant.helpers.aiohttp_client import async_get_clientsession

import custom_components.teslatlas_hub.current_hub_client as current_hub_client_module
from custom_components.teslatlas_hub.client import (
    HubAuthenticationError,
    HubConnectionError,
    HubContractError,
)
from custom_components.teslatlas_hub.current_hub_client import (
    MAX_RESPONSE_BYTES,
    CurrentHubClient,
    pinned_request_class,
)
from custom_components.teslatlas_hub.models import HubEndpoint

HUB_ID = "11111111-1111-4111-8111-111111111111"
OTHER_HUB_ID = "22222222-2222-4222-8222-222222222222"
VEHICLE_ID = "33333333-3333-4333-8333-333333333333"
DYNAMIC_VEHICLE_ID = "66666666-6666-4666-8666-666666666666"
PAIRING_ID = "44444444-4444-4444-8444-444444444444"
TOKEN = "a" * 64


def _write_tls_chain(root: Path) -> tuple[Path, Path, Path, bytes, bytes]:
    """Create one private CA and two separately pinned localhost leaves."""
    now = datetime.now(UTC)
    ca_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    ca_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Task 7 CA")])
    ca = (
        x509.CertificateBuilder()
        .subject_name(ca_name)
        .issuer_name(ca_name)
        .public_key(ca_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(minutes=1))
        .not_valid_after(now + timedelta(days=1))
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .add_extension(
            x509.KeyUsage(
                digital_signature=True,
                content_commitment=False,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=True,
                crl_sign=True,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
        .add_extension(
            x509.SubjectKeyIdentifier.from_public_key(ca_key.public_key()),
            critical=False,
        )
        .sign(ca_key, hashes.SHA256())
    )
    ca_path = root / "ca.pem"
    ca_path.write_bytes(ca.public_bytes(serialization.Encoding.PEM))

    leaves: list[tuple[Path, Path, bytes]] = []
    for label in ("a", "b"):
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        certificate = (
            x509.CertificateBuilder()
            .subject_name(
                x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "localhost")])
            )
            .issuer_name(ca.subject)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - timedelta(minutes=1))
            .not_valid_after(now + timedelta(days=1))
            .add_extension(
                x509.BasicConstraints(ca=False, path_length=None), critical=True
            )
            .add_extension(
                x509.AuthorityKeyIdentifier.from_issuer_public_key(ca_key.public_key()),
                critical=False,
            )
            .add_extension(
                x509.SubjectKeyIdentifier.from_public_key(key.public_key()),
                critical=False,
            )
            .add_extension(
                x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]),
                critical=False,
            )
            .add_extension(
                x509.SubjectAlternativeName([x509.IPAddress(ip_address("127.0.0.1"))]),
                critical=False,
            )
            .sign(ca_key, hashes.SHA256())
        )
        cert_path = root / f"leaf-{label}.pem"
        key_path = root / f"leaf-{label}-key.pem"
        cert_path.write_bytes(certificate.public_bytes(serialization.Encoding.PEM))
        key_path.write_bytes(
            key.private_bytes(
                serialization.Encoding.PEM,
                serialization.PrivateFormat.PKCS8,
                serialization.NoEncryption(),
            )
        )
        leaves.append(
            (
                cert_path,
                key_path,
                certificate.public_bytes(serialization.Encoding.DER),
            )
        )
    return ca_path, leaves[1][0], leaves[1][1], leaves[0][2], leaves[1][2]


@pytest.fixture(autouse=True)
def _enable_local_http(socket_enabled) -> None:
    """Permit the real loopback server used by these transport tests."""


def _discovery(hub_id: str = HUB_ID) -> dict[str, Any]:
    return {
        "hub_id": hub_id,
        "protocol": "teslatlas-sync",
        "protocol_major": 1,
        "api_versions": ["1.0"],
        "capabilities": ["query.vehicles", "query.current"],
        "version": "2026.36.2",
        "sourceUrl": "https://example.invalid/source",
        "pack_format": "sqlite-zstd",
    }


def _current(
    *, observed_at_ms: int | None, battery_level: int | None = 0
) -> dict[str, Any]:
    return {
        "vehicle_id": VEHICLE_ID,
        "observed_at_ms": observed_at_ms,
        "battery_level": battery_level,
        "charging_state": "Stopped",
        "charger_power": 0.0,
        "charge_limit_soc": 80,
        "est_battery_range_km": 321.25,
        "odometer": 12345.5,
        "state": "asleep",
        "inside_temp": None,
        "outside_temp": 12.5,
        "locked": None,
        "version": "2026.26.3",
        "update_status": None,
    }


def _endpoint(server: Any) -> HubEndpoint:
    return HubEndpoint(host=server.host, port=server.port, use_tls=False)


async def test_probe_is_unauthenticated_and_wrong_identity_blocks_bearer(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch a saved bearer being sent before replacement-Hub identity is known."""
    authenticated_requests = 0

    async def discovery(request: web.Request) -> web.Response:
        assert "Authorization" not in request.headers
        return web.json_response(_discovery(OTHER_HUB_ID))

    async def vehicles(request: web.Request) -> web.Response:
        nonlocal authenticated_requests
        authenticated_requests += 1
        return web.json_response({"vehicles": []})

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    with pytest.raises(HubAuthenticationError, match="identity"):
        await client.async_snapshot()
    assert authenticated_requests == 0


async def test_pair_claims_exact_invitation_and_returns_device_credential(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch a wrong claim route/body or loss of returned credential metadata."""
    requests: list[tuple[str, dict[str, Any]]] = []

    async def discovery(request: web.Request) -> web.Response:
        assert "Authorization" not in request.headers
        return web.json_response(_discovery())

    async def claim(request: web.Request) -> web.Response:
        assert "Authorization" not in request.headers
        requests.append((request.path, await request.json()))
        return web.json_response(
            {
                "device_id": "55555555-5555-4555-8555-555555555555",
                "access_token": TOKEN,
                "expires_at_ms": 1_788_567_300_000,
            }
        )

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_post("/v1/pairings/{pairing_id}/claim", claim)
    server = await aiohttp_server(app)
    client = CurrentHubClient(async_get_clientsession(hass), _endpoint(server))

    result = await client.async_pair(PAIRING_ID, "b" * 64, "Home Assistant")

    assert requests == [
        (
            f"/v1/pairings/{PAIRING_ID}/claim",
            {"secret": "b" * 64, "device_name": "Home Assistant"},
        )
    ]
    assert result.info.hub_id == HUB_ID
    assert result.access_token == TOKEN
    assert result.device_id == "55555555-5555-4555-8555-555555555555"
    assert result.expires_at_ms == 1_788_567_300_000


async def test_snapshot_maps_current_fields_preserving_zero_null_and_no_sse(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch unit remapping, zero/null collapse, lock guesses, or event traffic."""
    paths: list[str] = []
    observed_at_ms = int(datetime.now(UTC).timestamp() * 1000) - 2_000

    async def handler(request: web.Request) -> web.Response:
        paths.append(request.path)
        if request.path == "/.well-known/teslatlas-hub":
            assert "Authorization" not in request.headers
            return web.json_response(_discovery())
        assert request.headers["Authorization"] == f"Bearer {TOKEN}"
        if request.path == "/v1/vehicles":
            return web.json_response(
                {"vehicles": [{"vehicle_id": VEHICLE_ID, "display_name": "Atlas"}]}
            )
        return web.json_response(_current(observed_at_ms=observed_at_ms))

    app = web.Application()
    app.router.add_get("/{tail:.*}", handler)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    snapshot = await client.async_snapshot()
    vehicle = snapshot.vehicles[VEHICLE_ID]

    assert vehicle.name == "Atlas"
    assert vehicle.state_of_charge == 0
    assert vehicle.charging_power_kw == 0.0
    assert vehicle.inside_temperature_c is None
    assert vehicle.access_state is None
    assert vehicle.telemetry_age_seconds in {2, 3}
    assert paths == [
        "/.well-known/teslatlas-hub",
        "/v1/vehicles",
        f"/v1/vehicles/{VEHICLE_ID}/current",
    ]
    assert all("event" not in path for path in paths)


async def test_snapshot_tracks_vehicle_add_remove_and_return_from_public_list(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Use each current vehicle list without caching or inventing mutation APIs."""
    stable_vehicle_ids = (
        VEHICLE_ID,
        "55555555-5555-4555-8555-555555555555",
        DYNAMIC_VEHICLE_ID,
    )
    listed_vehicle_ids = [
        stable_vehicle_ids,
        stable_vehicle_ids[:2],
        stable_vehicle_ids,
    ]
    list_requests = 0
    current_requests: list[str] = []

    async def discovery(_request: web.Request) -> web.Response:
        return web.json_response(_discovery())

    async def vehicles(_request: web.Request) -> web.Response:
        nonlocal list_requests
        vehicle_ids = listed_vehicle_ids[list_requests]
        list_requests += 1
        return web.json_response(
            {
                "vehicles": [
                    {"vehicle_id": vehicle_id, "display_name": f"Vehicle {index}"}
                    for index, vehicle_id in enumerate(vehicle_ids, start=1)
                ]
            }
        )

    async def current(request: web.Request) -> web.Response:
        vehicle_id = request.match_info["vehicle_id"]
        current_requests.append(vehicle_id)
        payload = _current(observed_at_ms=None)
        payload["vehicle_id"] = vehicle_id
        return web.json_response(payload)

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    app.router.add_get("/v1/vehicles/{vehicle_id}/current", current)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    added = await client.async_snapshot()
    omitted = await client.async_snapshot()
    returned = await client.async_snapshot()

    assert tuple(added.vehicles) == stable_vehicle_ids
    assert tuple(omitted.vehicles) == stable_vehicle_ids[:2]
    assert tuple(returned.vehicles) == stable_vehicle_ids
    assert returned.vehicles[DYNAMIC_VEHICLE_ID].name == "Vehicle 3"
    assert current_requests == [
        *stable_vehicle_ids,
        *stable_vehicle_ids[:2],
        *stable_vehicle_ids,
    ]


async def test_snapshot_rejects_non_finite_numeric_telemetry(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Do not publish JSON exponent overflow as live numeric telemetry."""

    async def handler(request: web.Request) -> web.Response:
        if request.path == "/.well-known/teslatlas-hub":
            return web.json_response(_discovery())
        if request.path == "/v1/vehicles":
            return web.json_response(
                {"vehicles": [{"vehicle_id": VEHICLE_ID, "display_name": "Atlas"}]}
            )
        payload = _current(observed_at_ms=None)
        raw = json.dumps(payload, separators=(",", ":")).replace(
            '"charger_power":0.0', '"charger_power":1e999'
        )
        return web.Response(
            body=raw.encode(),
            headers={"Content-Type": "application/json"},
        )

    app = web.Application()
    app.router.add_get("/{tail:.*}", handler)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    with pytest.raises(HubContractError, match="charger_power"):
        await client.async_snapshot()


async def test_probe_rejects_non_json_content_type(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Reject a response body that is JSON-shaped but not typed as JSON."""

    async def discovery(_request: web.Request) -> web.Response:
        return web.Response(
            text=json.dumps(_discovery()),
            content_type="text/plain",
        )

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    server = await aiohttp_server(app)
    client = CurrentHubClient(async_get_clientsession(hass), _endpoint(server))

    with pytest.raises(HubContractError, match="not JSON"):
        await client.async_probe()


async def test_probe_rejects_non_object_json(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Reject a valid JSON value that cannot satisfy a profile object schema."""

    async def discovery(_request: web.Request) -> web.Response:
        return web.Response(
            body=b"[]",
            headers={"Content-Type": "application/json"},
        )

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    server = await aiohttp_server(app)
    client = CurrentHubClient(async_get_clientsession(hass), _endpoint(server))

    with pytest.raises(HubContractError, match="JSON object"):
        await client.async_probe()


async def test_request_timeout_becomes_connection_failure(
    hass: HomeAssistant,
    aiohttp_server,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Keep a slow public request on the retryable connection-failure path."""
    monkeypatch.setattr(
        current_hub_client_module,
        "REQUEST_TIMEOUT",
        ClientTimeout(total=0.01, connect=0.01, sock_read=0.01),
    )

    async def slow_discovery(_request: web.Request) -> web.Response:
        await asyncio.sleep(0.05)
        return web.json_response(_discovery())

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", slow_discovery)
    server = await aiohttp_server(app)
    client = CurrentHubClient(async_get_clientsession(hass), _endpoint(server))

    with pytest.raises(HubConnectionError, match="request failed"):
        await client.async_probe()


async def test_current_reads_are_capped_at_four(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch unbounded fan-out when a Hub exposes many vehicles."""
    in_flight = 0
    maximum_in_flight = 0
    vehicle_ids = [f"00000000-0000-4000-8000-{index:012d}" for index in range(8)]

    async def discovery(_request: web.Request) -> web.Response:
        return web.json_response(_discovery())

    async def vehicles(_request: web.Request) -> web.Response:
        return web.json_response(
            {
                "vehicles": [
                    {"vehicle_id": vehicle_id, "display_name": None}
                    for vehicle_id in vehicle_ids
                ]
            }
        )

    async def current(request: web.Request) -> web.Response:
        nonlocal in_flight, maximum_in_flight
        in_flight += 1
        maximum_in_flight = max(maximum_in_flight, in_flight)
        await asyncio.sleep(0.01)
        in_flight -= 1
        payload = _current(observed_at_ms=None)
        payload["vehicle_id"] = request.match_info["vehicle_id"]
        return web.json_response(payload)

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    app.router.add_get("/v1/vehicles/{vehicle_id}/current", current)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    snapshot = await client.async_snapshot()

    assert len(snapshot.vehicles) == 8
    assert maximum_in_flight == 4


async def test_snapshot_rejects_duplicate_vehicle_identity(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Reject duplicate vehicle IDs before publishing a collapsed snapshot."""
    current_requests = 0

    async def handler(request: web.Request) -> web.Response:
        nonlocal current_requests
        if request.path == "/.well-known/teslatlas-hub":
            return web.json_response(_discovery())
        if request.path == "/v1/vehicles":
            return web.json_response(
                {
                    "vehicles": [
                        {"vehicle_id": VEHICLE_ID, "display_name": "Atlas"},
                        {
                            "vehicle_id": VEHICLE_ID,
                            "display_name": "Atlas duplicate",
                        },
                    ]
                }
            )
        current_requests += 1
        return web.json_response(_current(observed_at_ms=None))

    app = web.Application()
    app.router.add_get("/{tail:.*}", handler)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    with pytest.raises(HubContractError, match="duplicate vehicle_id"):
        await client.async_snapshot()
    assert current_requests == 0


async def test_overlapping_snapshots_are_serialized(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Prevent a second refresh from dispatching while the first is active."""
    discovery_requests = 0
    current_requests = 0
    in_flight = 0
    maximum_in_flight = 0
    first_current_started = asyncio.Event()
    release_first_current = asyncio.Event()

    async def discovery(_request: web.Request) -> web.Response:
        nonlocal discovery_requests
        discovery_requests += 1
        return web.json_response(_discovery())

    async def vehicles(_request: web.Request) -> web.Response:
        return web.json_response(
            {"vehicles": [{"vehicle_id": VEHICLE_ID, "display_name": "Roadster"}]}
        )

    async def current(_request: web.Request) -> web.Response:
        nonlocal current_requests, in_flight, maximum_in_flight
        current_requests += 1
        in_flight += 1
        maximum_in_flight = max(maximum_in_flight, in_flight)
        try:
            if current_requests == 1:
                first_current_started.set()
                await release_first_current.wait()
            return web.json_response(
                _current(observed_at_ms=None, battery_level=current_requests)
            )
        finally:
            in_flight -= 1

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    app.router.add_get("/v1/vehicles/{vehicle_id}/current", current)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    first_snapshot = asyncio.create_task(client.async_snapshot())
    await asyncio.wait_for(first_current_started.wait(), timeout=1)
    second_snapshot = asyncio.create_task(client.async_snapshot())
    await asyncio.sleep(0.05)
    assert discovery_requests == 1
    assert current_requests == 1
    assert maximum_in_flight == 1

    release_first_current.set()
    first = await first_snapshot
    second = await second_snapshot
    assert discovery_requests == 2
    assert current_requests == 2
    assert first.vehicles[VEHICLE_ID].state_of_charge == 1
    assert second.vehicles[VEHICLE_ID].state_of_charge == 2


async def test_unauthorized_vehicle_list_is_typed_authentication_failure(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch an expired credential being treated as a retryable outage."""

    async def discovery(_request: web.Request) -> web.Response:
        return web.json_response(_discovery())

    async def vehicles(_request: web.Request) -> web.Response:
        return web.Response(status=401)

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    with pytest.raises(HubAuthenticationError):
        await client.async_snapshot()


async def test_future_observation_outside_clock_skew_is_rejected(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch future telemetry being presented as freshly observed."""
    future_ms = int(datetime.now(UTC).timestamp() * 1000) + 301_000

    async def handler(request: web.Request) -> web.Response:
        if request.path == "/.well-known/teslatlas-hub":
            return web.json_response(_discovery())
        if request.path == "/v1/vehicles":
            return web.json_response(
                {"vehicles": [{"vehicle_id": VEHICLE_ID, "display_name": None}]}
            )
        return web.json_response(_current(observed_at_ms=future_ms))

    app = web.Application()
    app.router.add_get("/{tail:.*}", handler)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    with pytest.raises(HubContractError, match="future"):
        await client.async_snapshot()


async def test_expired_device_credential_fails_before_authenticated_request(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch known-expired credentials being dispatched to the Hub."""
    authenticated_requests = 0

    async def discovery(_request: web.Request) -> web.Response:
        return web.json_response(_discovery())

    async def vehicles(_request: web.Request) -> web.Response:
        nonlocal authenticated_requests
        authenticated_requests += 1
        return web.json_response({"vehicles": []})

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        bearer_expires_at_ms=1,
        expected_hub_id=HUB_ID,
    )

    with pytest.raises(HubAuthenticationError, match="expired"):
        await client.async_snapshot()
    assert authenticated_requests == 0


async def test_missing_current_observation_keeps_vehicle_with_unknown_values(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch a defined 404 observation being promoted to a global outage."""

    async def handler(request: web.Request) -> web.Response:
        if request.path == "/.well-known/teslatlas-hub":
            return web.json_response(_discovery())
        if request.path == "/v1/vehicles":
            return web.json_response(
                {"vehicles": [{"vehicle_id": VEHICLE_ID, "display_name": None}]}
            )
        return web.Response(status=404)

    app = web.Application()
    app.router.add_get("/{tail:.*}", handler)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    snapshot = await client.async_snapshot()
    assert snapshot.vehicles[VEHICLE_ID].state_of_charge is None
    assert snapshot.vehicles[VEHICLE_ID].telemetry_age_seconds is None


async def test_one_vehicle_current_failure_keeps_other_observations(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Keep healthy vehicles updating when one current read is unavailable."""
    healthy_vehicle_id = VEHICLE_ID
    unavailable_vehicle_id = "66666666-6666-4666-8666-666666666666"

    async def handler(request: web.Request) -> web.Response:
        if request.path == "/.well-known/teslatlas-hub":
            return web.json_response(_discovery())
        if request.path == "/v1/vehicles":
            return web.json_response(
                {
                    "vehicles": [
                        {"vehicle_id": healthy_vehicle_id, "display_name": "Atlas"},
                        {
                            "vehicle_id": unavailable_vehicle_id,
                            "display_name": "Roadster",
                        },
                    ]
                }
            )
        if request.match_info.get("vehicle_id") == unavailable_vehicle_id:
            return web.Response(status=503)
        payload = _current(observed_at_ms=None, battery_level=42)
        payload["vehicle_id"] = healthy_vehicle_id
        return web.json_response(payload)

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", handler)
    app.router.add_get("/v1/vehicles", handler)
    app.router.add_get("/v1/vehicles/{vehicle_id}/current", handler)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    snapshot = await client.async_snapshot()

    assert snapshot.vehicles[healthy_vehicle_id].state_of_charge == 42
    assert snapshot.vehicles[unavailable_vehicle_id].state_of_charge is None
    assert snapshot.vehicles[unavailable_vehicle_id].name == "Roadster"


async def test_fragmented_discovery_body_is_read_through_eof(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch decoding a temporarily short stream read as a complete response."""
    raw = json.dumps(_discovery(), separators=(",", ":")).encode()

    async def discovery(request: web.Request) -> web.StreamResponse:
        response = web.StreamResponse(headers={"Content-Type": "application/json"})
        await response.prepare(request)
        await response.write(raw[:1])
        await asyncio.sleep(0.05)
        await response.write(raw[1:])
        await response.write_eof()
        return response

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    server = await aiohttp_server(app)
    client = CurrentHubClient(async_get_clientsession(hass), _endpoint(server))

    assert (await client.async_probe()).hub_id == HUB_ID


@pytest.mark.parametrize("extra_bytes", [0, 1])
async def test_chunked_response_enforces_exact_body_limit(
    hass: HomeAssistant,
    aiohttp_server,
    extra_bytes: int,
) -> None:
    """Accept exactly one MiB and reject the first byte beyond it."""
    payload = _discovery() | {"padding": ""}
    base = json.dumps(payload, separators=(",", ":")).encode()
    payload["padding"] = "x" * (MAX_RESPONSE_BYTES + extra_bytes - len(base))
    raw = json.dumps(payload, separators=(",", ":")).encode()
    assert len(raw) == MAX_RESPONSE_BYTES + extra_bytes

    async def discovery(request: web.Request) -> web.StreamResponse:
        response = web.StreamResponse(headers={"Content-Type": "application/json"})
        await response.prepare(request)
        for offset in range(0, len(raw), 4093):
            await response.write(raw[offset : offset + 4093])
        await response.write_eof()
        return response

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    server = await aiohttp_server(app)
    client = CurrentHubClient(async_get_clientsession(hass), _endpoint(server))

    if extra_bytes:
        with pytest.raises(HubContractError, match="size limit"):
            await client.async_probe()
    else:
        assert (await client.async_probe()).hub_id == HUB_ID


async def test_complete_valid_prefix_does_not_hide_trailing_response_bytes(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch accepting a valid JSON prefix without consuming the entire body."""
    raw = json.dumps(_discovery(), separators=(",", ":")).encode()

    async def discovery(request: web.Request) -> web.StreamResponse:
        response = web.StreamResponse(headers={"Content-Type": "application/json"})
        await response.prepare(request)
        await response.write(raw)
        await asyncio.sleep(0.05)
        await response.write(b"{}")
        await response.write_eof()
        return response

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    server = await aiohttp_server(app)
    client = CurrentHubClient(async_get_clientsession(hass), _endpoint(server))

    with pytest.raises(HubContractError, match="invalid JSON"):
        await client.async_probe()


async def test_pair_rejects_changed_identity_before_sending_secret(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch a one-use invitation secret being sent to a replacement Hub."""
    claim_requests = 0

    async def discovery(_request: web.Request) -> web.Response:
        return web.json_response(_discovery(OTHER_HUB_ID))

    async def claim(_request: web.Request) -> web.Response:
        nonlocal claim_requests
        claim_requests += 1
        return web.Response(status=500)

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_post("/v1/pairings/{pairing_id}/claim", claim)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        expected_hub_id=HUB_ID,
    )

    with pytest.raises(HubAuthenticationError, match="identity"):
        await client.async_pair(PAIRING_ID, "secret-must-not-leave", "Home Assistant")
    assert claim_requests == 0


async def test_pin_is_checked_on_actual_normally_trusted_http_connection(
    aiohttp_server,
    tmp_path: Path,
) -> None:
    """Catch a trusted peer switch between a disconnected pin check and HTTP."""
    ca_path, cert_b, key_b, der_a, der_b = _write_tls_chain(tmp_path)
    requests_seen = 0

    async def discovery(_request: web.Request) -> web.Response:
        nonlocal requests_seen
        requests_seen += 1
        return web.json_response(_discovery())

    server_context = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
    server_context.load_cert_chain(cert_b, key_b)
    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    server = await aiohttp_server(app, ssl=server_context)
    endpoint = HubEndpoint(
        host=server.host,
        port=server.port,
        use_tls=True,
        tls_pin=hashlib.sha256(der_a).hexdigest(),
    )

    trusted_context = ssl.create_default_context(cafile=str(ca_path))
    async with ClientSession(
        connector=TCPConnector(ssl=trusted_context),
        request_class=pinned_request_class(hashlib.sha256(der_a).digest()),
    ) as session:
        client = CurrentHubClient(session, endpoint, transport_pin_bound=True)
        with pytest.raises(HubConnectionError):
            await client.async_probe()
    assert requests_seen == 0

    endpoint = HubEndpoint(
        host=server.host,
        port=server.port,
        use_tls=True,
        tls_pin=hashlib.sha256(der_b).hexdigest(),
    )
    async with ClientSession(
        connector=TCPConnector(ssl=trusted_context),
        request_class=pinned_request_class(hashlib.sha256(der_b).digest()),
    ) as session:
        client = CurrentHubClient(session, endpoint, transport_pin_bound=True)
        assert (await client.async_probe()).hub_id == HUB_ID
    assert requests_seen == 1


@pytest.mark.parametrize("first_status", [401, 418])
async def test_failed_snapshot_cancels_queued_and_in_flight_current_reads(
    hass: HomeAssistant,
    aiohttp_server,
    first_status: int,
) -> None:
    """Catch detached sibling reads continuing after one current read fails."""
    vehicle_ids = [f"00000000-0000-4000-8000-{index:012d}" for index in range(8)]
    started = 0
    four_started = asyncio.Event()
    blocker = asyncio.Event()

    async def discovery(_request: web.Request) -> web.Response:
        return web.json_response(_discovery())

    async def vehicles(_request: web.Request) -> web.Response:
        return web.json_response(
            {
                "vehicles": [
                    {"vehicle_id": vehicle_id, "display_name": None}
                    for vehicle_id in vehicle_ids
                ]
            }
        )

    async def current(request: web.Request) -> web.Response:
        nonlocal started
        started += 1
        if started == 4:
            four_started.set()
        if request.match_info["vehicle_id"] == vehicle_ids[0]:
            await four_started.wait()
            return web.Response(status=first_status)
        await blocker.wait()
        return web.json_response(_current(observed_at_ms=None))

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    app.router.add_get("/v1/vehicles/{vehicle_id}/current", current)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    error = HubAuthenticationError if first_status == 401 else HubContractError
    with pytest.raises(error):
        await client.async_snapshot()
    observed_after_failure = started
    await client.async_close()
    await asyncio.sleep(0.05)
    assert observed_after_failure == 4
    assert started == observed_after_failure


async def test_close_cancels_active_snapshot_and_prevents_new_dispatch(
    hass: HomeAssistant,
    aiohttp_server,
) -> None:
    """Catch unload returning while the old client can still send requests."""
    vehicle_ids = [f"00000000-0000-4000-8000-{index:012d}" for index in range(8)]
    started = 0
    four_started = asyncio.Event()
    blocker = asyncio.Event()

    async def discovery(_request: web.Request) -> web.Response:
        return web.json_response(_discovery())

    async def vehicles(_request: web.Request) -> web.Response:
        return web.json_response(
            {
                "vehicles": [
                    {"vehicle_id": vehicle_id, "display_name": None}
                    for vehicle_id in vehicle_ids
                ]
            }
        )

    async def current(_request: web.Request) -> web.Response:
        nonlocal started
        started += 1
        if started == 4:
            four_started.set()
        await blocker.wait()
        return web.json_response(_current(observed_at_ms=None))

    app = web.Application()
    app.router.add_get("/.well-known/teslatlas-hub", discovery)
    app.router.add_get("/v1/vehicles", vehicles)
    app.router.add_get("/v1/vehicles/{vehicle_id}/current", current)
    server = await aiohttp_server(app)
    client = CurrentHubClient(
        async_get_clientsession(hass),
        _endpoint(server),
        bearer_token=TOKEN,
        expected_hub_id=HUB_ID,
    )

    snapshot_task = asyncio.create_task(client.async_snapshot())
    await asyncio.wait_for(four_started.wait(), timeout=1)
    await client.async_close()
    assert snapshot_task.done()
    with pytest.raises(asyncio.CancelledError):
        await snapshot_task
    observed_after_close = started
    await asyncio.sleep(0.05)
    assert observed_after_close == 4
    assert started == observed_after_close
    with pytest.raises(HubConnectionError, match="closed"):
        await client.async_snapshot()

"""Bounded asynchronous client for ``hub-http-v1@1.0.0``."""

from __future__ import annotations

import asyncio
import hashlib
import json
import math
from datetime import UTC, datetime
from typing import Any
from urllib.parse import quote
from uuid import UUID

from aiohttp import (
    ClientError,
    ClientRequest,
    ClientResponse,
    ClientSession,
    ClientTimeout,
    ServerFingerprintMismatch,
)
from yarl import URL

from .client import (
    HubAuthenticationError,
    HubConnectionError,
    HubContractError,
    HubIdentityError,
    HubPairingError,
)
from .models import (
    HubEndpoint,
    HubInfo,
    HubSnapshot,
    HubStatus,
    PairingResult,
    VehicleState,
)

PROFILE_ID = "hub-http-v1@1.0.0"
PROFILE_SHA256 = "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
MAX_RESPONSE_BYTES = 1_048_576
MAX_CURRENT_READS = 4
CLOCK_SKEW_SECONDS = 300
REQUEST_TIMEOUT = ClientTimeout(total=10, connect=5, sock_read=8)
READ_CHUNK_BYTES = 64 * 1024


def tls_pin_bytes(pin: str) -> bytes:
    """Decode one canonical SHA-256 leaf-certificate pin."""
    if len(pin) != 64:
        raise HubContractError("tls_pin must be a SHA-256 hex digest")
    try:
        expected = bytes.fromhex(pin)
    except ValueError as err:
        raise HubContractError("tls_pin must be a SHA-256 hex digest") from err
    if pin != pin.lower():
        raise HubContractError("tls_pin must use lowercase hexadecimal")
    return expected


def pinned_request_class(expected_pin: bytes) -> type[ClientRequest]:
    """Bind a leaf pin to the actual normally verified HTTP connection."""

    class PinnedClientRequest(ClientRequest):
        async def send(self, conn: Any) -> ClientResponse:
            transport = conn.transport
            ssl_object = (
                transport.get_extra_info("ssl_object")
                if transport is not None
                else None
            )
            certificate = (
                ssl_object.getpeercert(binary_form=True)
                if ssl_object is not None
                else None
            )
            actual_pin = hashlib.sha256(certificate or b"").digest()
            if certificate is None or actual_pin != expected_pin:
                conn.close()
                raise ServerFingerprintMismatch(
                    expected_pin,
                    actual_pin,
                    self.url.host or "",
                    self.url.port or 443,
                )
            return await super().send(conn)

    return PinnedClientRequest


def _reject_json_constant(value: str) -> None:
    raise ValueError(f"invalid JSON constant {value}")


def _uuid(value: Any, field_name: str) -> str:
    if not isinstance(value, str):
        raise HubContractError(f"{field_name} must be a UUID string")
    try:
        parsed = UUID(value)
    except ValueError as err:
        raise HubContractError(f"{field_name} must be a UUID") from err
    if str(parsed) != value:
        raise HubContractError(f"{field_name} must use canonical lowercase UUID form")
    return value


def _optional_number(payload: dict[str, Any], key: str) -> float | int | None:
    value = payload.get(key)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise HubContractError(f"{key} must be numeric or null")
    if isinstance(value, float) and not math.isfinite(value):
        raise HubContractError(f"{key} must be finite")
    return value


def _optional_string(payload: dict[str, Any], key: str) -> str | None:
    value = payload.get(key)
    if value is not None and not isinstance(value, str):
        raise HubContractError(f"{key} must be text or null")
    return value


def _vehicle_summary(summary: Any) -> tuple[str, str]:
    if not isinstance(summary, dict):
        raise HubContractError("vehicle summary must be an object")
    vehicle_id = _uuid(summary.get("vehicle_id"), "vehicle_id")
    display_name = summary.get("display_name")
    if display_name is not None and not isinstance(display_name, str):
        raise HubContractError("display_name must be text or null")
    return vehicle_id, display_name or "Tesla vehicle"


def _telemetry_age(observed_at_ms: Any, now: datetime) -> int | None:
    if observed_at_ms is None:
        return None
    if isinstance(observed_at_ms, bool) or not isinstance(observed_at_ms, int):
        raise HubContractError("observed_at_ms must be an integer or null")
    age = now.timestamp() - observed_at_ms / 1000
    if age < -CLOCK_SKEW_SECONDS:
        raise HubContractError("observed_at_ms is too far in the future")
    return max(0, int(age))


class CurrentHubClient:
    """Production current-Hub HTTP adapter."""

    def __init__(
        self,
        session: ClientSession,
        endpoint: HubEndpoint,
        *,
        bearer_token: str | None = None,
        bearer_expires_at_ms: int | None = None,
        expected_hub_id: str | None = None,
        owns_session_wrapper: bool = False,
        transport_pin_bound: bool = False,
        now: Any = None,
    ) -> None:
        """Store transport inputs."""
        self._session = session
        self.endpoint = endpoint
        self._bearer_token = bearer_token
        self._bearer_expires_at_ms = bearer_expires_at_ms
        self._expected_hub_id = expected_hub_id
        if endpoint.tls_pin is not None and not transport_pin_bound:
            raise HubContractError("TLS pin is not bound to the HTTP transport")
        self._owns_session_wrapper = owns_session_wrapper
        self._now = now or (lambda: datetime.now(UTC))
        self._current_semaphore = asyncio.Semaphore(MAX_CURRENT_READS)
        self._snapshot_lock = asyncio.Lock()
        self._request_tasks: set[asyncio.Task[Any]] = set()
        self._snapshot_tasks: set[asyncio.Task[Any]] = set()
        self._closed = False

    @property
    def _base_url(self) -> str:
        scheme = "https" if self.endpoint.use_tls else "http"
        try:
            return str(
                URL.build(
                    scheme=scheme,
                    host=self.endpoint.host,
                    port=self.endpoint.port,
                )
            ).rstrip("/")
        except ValueError as err:
            raise HubContractError("Invalid Teslatlas Hub endpoint") from err

    def _ensure_open(self) -> None:
        if self._closed:
            raise HubConnectionError("Teslatlas Hub client is closed")

    async def _async_json(
        self,
        method: str,
        path: str,
        *,
        authenticated: bool = False,
        pairing: bool = False,
        allow_not_found: bool = False,
        body: dict[str, Any] | None = None,
    ) -> dict[str, Any] | None:
        if authenticated and self._bearer_token is None:
            raise HubAuthenticationError("No device credential is configured")
        if (
            authenticated
            and self._bearer_expires_at_ms is not None
            and int(self._now().timestamp() * 1000) >= self._bearer_expires_at_ms
        ):
            raise HubAuthenticationError("The device credential has expired")
        self._ensure_open()
        headers = {"Accept": "application/json"}
        if authenticated:
            headers["Authorization"] = f"Bearer {self._bearer_token}"
        task = asyncio.current_task()
        if task is not None:
            self._request_tasks.add(task)
        try:
            self._ensure_open()
            async with self._session.request(
                method,
                self._base_url + path,
                headers=headers,
                json=body,
                allow_redirects=False,
                timeout=REQUEST_TIMEOUT,
            ) as response:
                return await self._async_decode_response(
                    response,
                    pairing=pairing,
                    allow_not_found=allow_not_found,
                )
        except HubAuthenticationError:
            raise
        except (ClientError, TimeoutError, OSError) as err:
            raise HubConnectionError("Teslatlas Hub request failed") from err
        finally:
            if task is not None:
                self._request_tasks.discard(task)

    async def _async_decode_response(
        self,
        response: ClientResponse,
        *,
        pairing: bool,
        allow_not_found: bool,
    ) -> dict[str, Any] | None:
        if 300 <= response.status < 400:
            raise HubContractError("Redirect responses are not accepted")
        if pairing and response.status in {400, 401, 404, 415, 422}:
            raise HubPairingError("Teslatlas Hub rejected the pairing invitation")
        if response.status == 401:
            error_type = HubPairingError if pairing else HubAuthenticationError
            raise error_type("Teslatlas Hub rejected the credential")
        if response.status == 404 and allow_not_found:
            return None
        if response.status in {404, 503}:
            raise HubConnectionError(f"Teslatlas Hub returned HTTP {response.status}")
        if response.status < 200 or response.status >= 300:
            raise HubContractError(f"Unexpected Teslatlas Hub HTTP {response.status}")
        if (
            response.content_length is not None
            and response.content_length > MAX_RESPONSE_BYTES
        ):
            raise HubContractError("Teslatlas Hub response exceeds the size limit")
        body = bytearray()
        async for chunk in response.content.iter_chunked(READ_CHUNK_BYTES):
            if len(body) + len(chunk) > MAX_RESPONSE_BYTES:
                raise HubContractError("Teslatlas Hub response exceeds the size limit")
            body.extend(chunk)
        raw = bytes(body)
        content_type = response.headers.get("Content-Type", "").split(";", 1)[0]
        if content_type.lower() != "application/json":
            raise HubContractError("Teslatlas Hub response is not JSON")
        try:
            payload = json.loads(raw, parse_constant=_reject_json_constant)
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as err:
            raise HubContractError("Teslatlas Hub returned invalid JSON") from err
        if not isinstance(payload, dict):
            raise HubContractError("Teslatlas Hub response must be a JSON object")
        return payload

    async def async_probe(self) -> HubInfo:
        """Read and validate stable public identity without a bearer."""
        payload = await self._async_json("GET", "/.well-known/teslatlas-hub")
        assert payload is not None
        hub_id = _uuid(payload.get("hub_id"), "hub_id")
        if payload.get("protocol") != "teslatlas-sync":
            raise HubContractError("Unsupported discovery protocol")
        if payload.get("protocol_major") != 1 or payload.get("api_versions") != ["1.0"]:
            raise HubContractError("Unsupported current-Hub API version")
        capabilities = payload.get("capabilities")
        if not isinstance(capabilities, list) or not all(
            isinstance(value, str) for value in capabilities
        ):
            raise HubContractError("Discovery capabilities must be strings")
        required = {"query.vehicles", "query.current"}
        if not required.issubset(capabilities):
            raise HubContractError("Hub lacks required current-state capabilities")
        return HubInfo(
            hub_id=hub_id,
            name="Teslatlas Hub",
            protocol_version=PROFILE_ID,
            capabilities=frozenset(capabilities),
        )

    async def async_snapshot(self) -> HubSnapshot:
        """Poll identity, vehicles and bounded per-vehicle observations."""
        self._ensure_open()
        task = asyncio.current_task()
        if task is not None:
            self._snapshot_tasks.add(task)
        try:
            async with self._snapshot_lock:
                self._ensure_open()
                info = await self.async_probe()
                if (
                    self._expected_hub_id is not None
                    and info.hub_id != self._expected_hub_id
                ):
                    raise HubIdentityError("Teslatlas Hub identity changed")
                payload = await self._async_json(
                    "GET", "/v1/vehicles", authenticated=True
                )
                assert payload is not None
                vehicles = payload.get("vehicles")
                if not isinstance(vehicles, list):
                    raise HubContractError("vehicles must be an array")
                seen_vehicle_ids: set[str] = set()
                for summary in vehicles:
                    vehicle_id, _ = _vehicle_summary(summary)
                    if vehicle_id in seen_vehicle_ids:
                        raise HubContractError("vehicles contains duplicate vehicle_id")
                    seen_vehicle_ids.add(vehicle_id)
                observations: list[VehicleState] = []
                for offset in range(0, len(vehicles), MAX_CURRENT_READS):
                    reads = [
                        asyncio.create_task(self._async_current(item))
                        for item in vehicles[offset : offset + MAX_CURRENT_READS]
                    ]
                    try:
                        results = await asyncio.gather(*reads)
                    except HubConnectionError:
                        results = await asyncio.gather(*reads, return_exceptions=True)
                    except BaseException:
                        for read in reads:
                            read.cancel()
                        await asyncio.gather(*reads, return_exceptions=True)
                        raise
                    for summary, result in zip(
                        vehicles[offset : offset + MAX_CURRENT_READS],
                        results,
                        strict=True,
                    ):
                        if isinstance(result, HubConnectionError):
                            vehicle_id, name = _vehicle_summary(summary)
                            observations.append(
                                VehicleState(vehicle_id=vehicle_id, name=name)
                            )
                        elif isinstance(result, BaseException):
                            raise result
                        else:
                            observations.append(result)
                return HubSnapshot.create(
                    info=info,
                    status=HubStatus(),
                    vehicles=observations,
                    received_at=self._now(),
                )
        finally:
            if task is not None:
                self._snapshot_tasks.discard(task)

    async def async_pair(
        self,
        pairing_id: str,
        secret: str,
        device_name: str,
    ) -> PairingResult:
        """Claim a pairing invitation."""
        info = await self.async_probe()
        if self._expected_hub_id is not None and info.hub_id != self._expected_hub_id:
            raise HubIdentityError("Teslatlas Hub identity changed")
        pairing_id = _uuid(pairing_id, "pairing_id")
        payload = await self._async_json(
            "POST",
            f"/v1/pairings/{quote(pairing_id, safe='')}/claim",
            pairing=True,
            body={"secret": secret, "device_name": device_name},
        )
        assert payload is not None
        token = payload.get("access_token")
        expires_at_ms = payload.get("expires_at_ms")
        if not isinstance(token, str) or len(token) != 64:
            raise HubContractError("Claim returned an invalid access_token")
        if isinstance(expires_at_ms, bool) or not isinstance(expires_at_ms, int):
            raise HubContractError("Claim returned an invalid expires_at_ms")
        return PairingResult(
            info=info,
            access_token=token,
            device_id=_uuid(payload.get("device_id"), "device_id"),
            expires_at_ms=expires_at_ms,
        )

    async def async_rotate(self) -> PairingResult:
        """Rotate the active device credential."""
        info = await self.async_probe()
        if self._expected_hub_id is not None and info.hub_id != self._expected_hub_id:
            raise HubIdentityError("Teslatlas Hub identity changed")
        payload = await self._async_json(
            "POST", "/v1/device/rotate", authenticated=True
        )
        assert payload is not None
        token = payload.get("access_token")
        expires_at_ms = payload.get("expires_at_ms")
        if not isinstance(token, str) or len(token) != 64:
            raise HubContractError("Rotation returned an invalid access_token")
        if isinstance(expires_at_ms, bool) or not isinstance(expires_at_ms, int):
            raise HubContractError("Rotation returned an invalid expires_at_ms")
        return PairingResult(
            info=info,
            access_token=token,
            device_id=_uuid(payload.get("device_id"), "device_id"),
            expires_at_ms=expires_at_ms,
        )

    async def _async_current(self, summary: Any) -> VehicleState:
        vehicle_id, name = _vehicle_summary(summary)
        async with self._current_semaphore:
            payload = await self._async_json(
                "GET",
                f"/v1/vehicles/{quote(vehicle_id, safe='')}/current",
                authenticated=True,
                allow_not_found=True,
            )
        if payload is None:
            return VehicleState(
                vehicle_id=vehicle_id,
                name=name,
            )
        if _uuid(payload.get("vehicle_id"), "vehicle_id") != vehicle_id:
            raise HubContractError("Current observation vehicle identity changed")
        locked = payload.get("locked")
        if locked is not None and not isinstance(locked, bool):
            raise HubContractError("locked must be boolean or null")
        name = _optional_string(payload, "display_name") or name
        return VehicleState(
            vehicle_id=vehicle_id,
            name=name,
            state_of_charge=_optional_number(payload, "battery_level"),
            charging_state=_optional_string(payload, "charging_state"),
            charging_power_kw=_optional_number(payload, "charger_power"),
            charge_limit_percent=_optional_number(payload, "charge_limit_soc"),
            estimated_range_km=_optional_number(payload, "est_battery_range_km"),
            odometer_km=_optional_number(payload, "odometer"),
            activity_state=_optional_string(payload, "state"),
            inside_temperature_c=_optional_number(payload, "inside_temp"),
            outside_temperature_c=_optional_number(payload, "outside_temp"),
            access_state=None
            if locked is None
            else ("locked" if locked else "unlocked"),
            software_version=_optional_string(payload, "version"),
            software_update_state=_optional_string(payload, "update_status"),
            telemetry_age_seconds=_telemetry_age(
                payload.get("observed_at_ms"), self._now()
            ),
        )

    async def async_close(self) -> None:
        """Cancel owned reads and detach any HA-managed session wrapper."""
        if self._closed:
            return
        self._closed = True
        current = asyncio.current_task()
        pending = {
            task
            for task in self._request_tasks | self._snapshot_tasks
            if task is not current
        }
        for task in pending:
            task.cancel()
        if pending:
            await asyncio.gather(*pending, return_exceptions=True)
        if self._owns_session_wrapper:
            self._session.detach()

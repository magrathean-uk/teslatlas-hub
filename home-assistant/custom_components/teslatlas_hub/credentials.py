"""Admission helpers for the packaged Current Hub credential contract."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from uuid import UUID

from .client import HubContractError

SIGNED64_MIN = -(2**63)
SIGNED64_MAX = 2**63 - 1
_EPOCH = datetime(1970, 1, 1, tzinfo=UTC)
_MILLISECOND = timedelta(milliseconds=1)
_HEX_DIGITS = frozenset("0123456789abcdef")


class HubCredentialExpiredError(HubContractError):
    """An otherwise valid issued credential is no longer operationally fresh."""


def epoch_milliseconds(now: datetime) -> int:
    """Convert an aware clock reading without floating-point rounding."""
    if now.tzinfo is None or now.utcoffset() is None:
        raise HubContractError("The current clock must be timezone-aware")
    return (now - _EPOCH) // _MILLISECOND


def validate_epoch_milliseconds(value: object, field_name: str) -> int:
    """Accept the full signed64 epoch-millisecond wire domain."""
    if isinstance(value, bool) or not isinstance(value, int):
        raise HubContractError(f"{field_name} must be a signed64 integer")
    if not SIGNED64_MIN <= value <= SIGNED64_MAX:
        raise HubContractError(f"{field_name} must be a signed64 integer")
    return value


def validate_access_token(access_token: object) -> None:
    """Reject malformed scoped credentials before header construction."""
    if (
        not isinstance(access_token, str)
        or len(access_token) != 64
        or not _HEX_DIGITS.issuperset(access_token)
    ):
        raise HubContractError(
            "access_token must be 64 lowercase hexadecimal characters"
        )


def validate_bearer(
    access_token: object, expires_at_ms: object, *, now_ms: int
) -> None:
    """Validate a bearer before activating a durably stored replacement."""
    validate_access_token(access_token)
    expiry = validate_epoch_milliseconds(expires_at_ms, "expires_at_ms")
    if expiry <= now_ms:
        raise HubCredentialExpiredError("expires_at_ms must be in the future")


def validate_current_hub_credential(
    access_token: object,
    device_id: object,
    expires_at_ms: object,
    *,
    now_ms: int,
) -> None:
    """Admit one issued Current Hub credential before persistence or use."""
    validate_bearer(access_token, expires_at_ms, now_ms=now_ms)
    if not isinstance(device_id, str):
        raise HubContractError("device_id must be a canonical lowercase UUID")
    try:
        parsed = UUID(device_id)
    except ValueError as err:
        raise HubContractError("device_id must be a canonical lowercase UUID") from err
    if str(parsed) != device_id:
        raise HubContractError("device_id must be a canonical lowercase UUID")

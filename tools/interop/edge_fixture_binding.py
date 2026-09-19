# SPDX-License-Identifier: AGPL-3.0-only
"""Fail-closed validation for a sealed Edge binding and its Hub fixture store."""

from __future__ import annotations

import hashlib
import os
import sqlite3
import stat
from pathlib import Path
from typing import Any, Mapping


class FixtureBindingError(RuntimeError):
    """Raised when a binding is not exactly anchored to its created fixture."""


def _text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not 1 <= len(value.encode("utf-8")) <= 256:
        raise FixtureBindingError(f"{label} must contain 1 to 256 UTF-8 bytes")
    return value


def _positive_integer(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise FixtureBindingError(f"{label} must be a positive integer")
    return value


def _canonical_private_database(database: Path) -> Path:
    if not database.is_absolute() or database.resolve(strict=False) != database:
        raise FixtureBindingError("database path must be absolute and canonical")
    try:
        metadata = database.lstat()
        parent = database.parent.lstat()
    except OSError as error:
        raise FixtureBindingError(f"cannot inspect fixture database: {error}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise FixtureBindingError("database must be a non-symlink regular file")
    if metadata.st_uid != os.getuid() or metadata.st_nlink != 1:
        raise FixtureBindingError("database must be singly linked and owned by the validator user")
    if stat.S_IMODE(metadata.st_mode) & 0o077:
        raise FixtureBindingError("database must be owner-only")
    if not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid():
        raise FixtureBindingError("database parent must be owned by the validator user")
    if stat.S_IMODE(parent.st_mode) & 0o077:
        raise FixtureBindingError("database parent must be owner-only")
    return database


def _equal(actual: Any, expected: Any, label: str) -> None:
    if actual != expected:
        raise FixtureBindingError(f"{label} mismatch")


def _immutable_fingerprint(database: Path) -> tuple[int, str]:
    for suffix, label in (("-wal", "pending WAL"), ("-shm", "active SHM")):
        sidecar = Path(str(database) + suffix)
        try:
            metadata = sidecar.lstat()
        except FileNotFoundError:
            continue
        except OSError as error:
            raise FixtureBindingError(f"cannot inspect {label} sidecar: {error}") from error
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise FixtureBindingError(f"{label} sidecar is not a regular file")
        raise FixtureBindingError(f"{label} sidecar exists")
    try:
        content = database.read_bytes()
    except OSError as error:
        raise FixtureBindingError(f"cannot fingerprint fixture database: {error}") from error
    return len(content), hashlib.sha256(content).hexdigest()


def validate_fixture_binding(
    fixture: Mapping[str, Any], binding: Mapping[str, Any], database: Path
) -> dict[str, Any]:
    """Validate all sealed identities against the fixture receipt and SQLite store.

    The function has no filesystem write path. Call it before PKI, credentials,
    guest creation, listeners, or runtime startup.
    """
    if not isinstance(fixture, Mapping) or not isinstance(binding, Mapping):
        raise FixtureBindingError("fixture and binding must be mappings")
    _equal(fixture.get("schema_version"), 2, "fixture schema_version")
    _equal(binding.get("schema_version"), 1, "binding schema_version")

    database = _canonical_private_database(Path(database))
    fixture_data_dir = Path(_text(fixture.get("data_dir"), "fixture data_dir"))
    if not fixture_data_dir.is_absolute() or fixture_data_dir.resolve(strict=False) != fixture_data_dir:
        raise FixtureBindingError("fixture data_dir must be absolute and canonical")
    _equal(fixture_data_dir, database.parent, "fixture data_dir")

    relationships = {
        "hub_installation_id": (
            _text(fixture.get("hub_id"), "fixture hub_id"),
            _text(binding.get("hub_installation_id"), "binding hub_installation_id"),
        ),
        "edge_installation_id": (
            _text(fixture.get("installation_id"), "fixture installation_id"),
            _text(binding.get("edge_installation_id"), "binding edge_installation_id"),
        ),
        "edge_lineage": (
            _text(fixture.get("lineage"), "fixture lineage"),
            _text(binding.get("edge_lineage"), "binding edge_lineage"),
        ),
        "source_id": (
            _text(fixture.get("source_id"), "fixture source_id"),
            _text(binding.get("source_id"), "binding source_id"),
        ),
        "vehicle_id": (
            _text(fixture.get("vehicle_id"), "fixture vehicle_id"),
            _text(binding.get("vehicle_id"), "binding vehicle_id"),
        ),
        "vehicle_identity": (
            _text(fixture.get("vin"), "fixture vin"),
            _text(binding.get("vehicle_identity"), "binding vehicle_identity"),
        ),
        "car_id": (
            _positive_integer(fixture.get("car_id"), "fixture car_id"),
            _positive_integer(binding.get("car_id"), "binding car_id"),
        ),
    }
    for label, (fixture_value, binding_value) in relationships.items():
        _equal(binding_value, fixture_value, label)

    database_fingerprint = _immutable_fingerprint(database)

    try:
        connection = sqlite3.connect(
            f"{database.as_uri()}?mode=ro&immutable=1", uri=True, timeout=1.0
        )
    except sqlite3.Error as error:
        raise FixtureBindingError(f"cannot open fixture database read-only: {error}") from error
    try:
        connection.execute("PRAGMA query_only=ON")
        query_only = connection.execute("PRAGMA query_only").fetchone()[0] == 1
        if not query_only:
            raise FixtureBindingError("SQLite query_only mode was not enabled")
        store_ids = connection.execute(
            "SELECT value FROM hub_metadata WHERE key='installation_id'"
        ).fetchall()
        if len(store_ids) != 1 or not isinstance(store_ids[0][0], str):
            raise FixtureBindingError("actual store hub_installation_id is unavailable")
        _equal(store_ids[0][0], relationships["hub_installation_id"][0], "actual store hub_installation_id")

        source_rows = connection.execute(
            "SELECT source_id FROM sources WHERE source_id=?", (relationships["source_id"][0],)
        ).fetchall()
        if source_rows != [(relationships["source_id"][0],)]:
            raise FixtureBindingError("actual store source_id mismatch")

        vehicle_rows = connection.execute(
            "SELECT vehicle_id, source_id, source_vehicle_key, vin FROM vehicles WHERE vehicle_id=?",
            (relationships["vehicle_id"][0],),
        ).fetchall()
        expected_vehicle = (
            relationships["vehicle_id"][0],
            relationships["source_id"][0],
            str(relationships["car_id"][0]),
            relationships["vehicle_identity"][0],
        )
        if vehicle_rows != [expected_vehicle]:
            raise FixtureBindingError("actual store vehicle identity tuple mismatch")
    except sqlite3.Error as error:
        raise FixtureBindingError(f"fixture database query failed: {error}") from error
    finally:
        connection.close()
    _equal(_immutable_fingerprint(database), database_fingerprint, "immutable database fingerprint")

    return {
        "schema_version": 1,
        "kind": "teslatlas.edge_fixture_binding_validation",
        "status": "passed",
        "database_query_only": True,
        "database_immutable": True,
        "checked_relationships": len(relationships),
        "hub_installation_id": relationships["hub_installation_id"][0],
        "edge_installation_id": relationships["edge_installation_id"][0],
        "edge_lineage": relationships["edge_lineage"][0],
        "source_id": relationships["source_id"][0],
        "vehicle_id": relationships["vehicle_id"][0],
        "vehicle_identity": relationships["vehicle_identity"][0],
        "car_id": relationships["car_id"][0],
    }

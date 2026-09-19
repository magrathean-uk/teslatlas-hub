# SPDX-License-Identifier: AGPL-3.0-only
"""Bounded same-owner, read-only observer for one Hub Edge ledger lineage."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sqlite3
import stat
import sys
from pathlib import Path
from typing import Any


REQUIRED_SCHEMA_VERSION = 58
REQUIRED_TABLES = frozenset(
    {
        "hub_metadata",
        "edge_lineages",
        "edge_applications",
        "edge_sequence_dispositions",
        "edge_accumulator_states",
        "edge_pending_publications",
    }
)


class LedgerObservationError(RuntimeError):
    """Raised when a ledger cannot be observed without weakening the boundary."""


def _private_database(database: Path) -> Path:
    if not database.is_absolute() or database.resolve(strict=False) != database:
        raise LedgerObservationError("database path must be absolute and canonical")
    try:
        metadata = database.lstat()
    except FileNotFoundError as error:
        raise LedgerObservationError("database does not exist") from error
    except OSError as error:
        raise LedgerObservationError(f"cannot stat database: {error}") from error
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise LedgerObservationError("database must be a non-symlink regular file")
    if metadata.st_nlink != 1:
        raise LedgerObservationError("database must have one hard link")
    if metadata.st_uid != os.getuid():
        raise LedgerObservationError("database must be owned by the observer user")
    if stat.S_IMODE(metadata.st_mode) & 0o077:
        raise LedgerObservationError("database must be owner-only")
    parent = database.parent.lstat()
    if not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid():
        raise LedgerObservationError("database parent must be an observer-owned directory")
    if stat.S_IMODE(parent.st_mode) & 0o077:
        raise LedgerObservationError("database parent must be owner-only")
    return database


def _selector(value: str, label: str) -> str:
    if not isinstance(value, str) or not 1 <= len(value.encode("utf-8")) <= 128:
        raise LedgerObservationError(f"{label} must contain 1 to 128 UTF-8 bytes")
    return value


def _rows(connection: sqlite3.Connection, sql: str, parameters: tuple[str, str]) -> list[dict[str, Any]]:
    return [dict(row) for row in connection.execute(sql, parameters).fetchall()]


def observe_ledger(database: Path, installation_id: str, lineage: str) -> dict[str, Any]:
    """Read one exact lineage without opening or creating the Hub database read-write."""
    database = _private_database(Path(database))
    installation_id = _selector(installation_id, "installation_id")
    lineage = _selector(lineage, "lineage")
    parameters = (installation_id, lineage)
    uri = f"{database.as_uri()}?mode=ro"
    try:
        connection = sqlite3.connect(uri, uri=True, timeout=1.0)
    except sqlite3.Error as error:
        raise LedgerObservationError(f"cannot open database read-only: {error}") from error
    connection.row_factory = sqlite3.Row
    try:
        connection.execute("PRAGMA query_only=ON")
        query_only = connection.execute("PRAGMA query_only").fetchone()[0] == 1
        if not query_only:
            raise LedgerObservationError("SQLite query_only mode was not enabled")
        user_version = int(connection.execute("PRAGMA user_version").fetchone()[0])
        if user_version < REQUIRED_SCHEMA_VERSION:
            raise LedgerObservationError(
                f"Hub schema {user_version} predates Edge ledger version {REQUIRED_SCHEMA_VERSION}"
            )
        tables = {
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_schema WHERE type='table' AND name IN "
                "('hub_metadata','edge_lineages','edge_applications',"
                "'edge_sequence_dispositions','edge_accumulator_states',"
                "'edge_pending_publications')"
            )
        }
        missing = sorted(REQUIRED_TABLES - tables)
        if missing:
            raise LedgerObservationError("missing Edge ledger tables: " + ", ".join(missing))
        store_rows = connection.execute(
            "SELECT value FROM hub_metadata WHERE key='installation_id'"
        ).fetchall()
        if len(store_rows) != 1 or not isinstance(store_rows[0][0], str):
            raise LedgerObservationError("Hub installation identity is unavailable")
        lineage_rows = _rows(
            connection,
            "SELECT source_id, vehicle_id, car_id, first_spool_seq, ack_frontier, "
            "created_at_ms, updated_at_ms FROM edge_lineages "
            "WHERE installation_id=? AND lineage=?",
            parameters,
        )
        if len(lineage_rows) > 1:
            raise LedgerObservationError("Edge lineage selector returned multiple rows")
        applications = _rows(
            connection,
            "SELECT stable_record_id, payload_sha256, disposition, first_spool_seq, "
            "observation_id, applied_at_ms FROM edge_applications "
            "WHERE installation_id=? AND lineage=? ORDER BY first_spool_seq, stable_record_id",
            parameters,
        )
        sequences = _rows(
            connection,
            "SELECT spool_seq, item_kind, item_id, legacy_record_id, payload_sha256, "
            "category, reason, occurred_at_ms, evidence_sha256, committed_at_ms "
            "FROM edge_sequence_dispositions WHERE installation_id=? AND lineage=? "
            "ORDER BY spool_seq",
            parameters,
        )
        pending = _rows(
            connection,
            "SELECT stable_record_id, vehicle_id, status, attempts, next_attempt_ms, "
            "last_error, created_at_ms, completed_at_ms FROM edge_pending_publications "
            "WHERE installation_id=? AND lineage=? AND status!='complete' "
            "ORDER BY stable_record_id",
            parameters,
        )
        accumulator_rows = connection.execute(
            "SELECT vehicle_id, state_version, state_json, through_spool_seq, updated_at_ms "
            "FROM edge_accumulator_states WHERE installation_id=? AND lineage=?",
            parameters,
        ).fetchall()
        accumulators = [
            {
                "vehicle_id": row[0],
                "state_version": row[1],
                "state_sha256": hashlib.sha256(bytes(row[2])).hexdigest(),
                "state_bytes": len(row[2]),
                "through_spool_seq": row[3],
                "updated_at_ms": row[4],
            }
            for row in accumulator_rows
        ]
        return {
            "schema_version": 1,
            "kind": "teslatlas.edge_ledger_read_only_observation",
            "database": {
                "path": str(database),
                "mode": f"{stat.S_IMODE(database.stat().st_mode):04o}",
                "uid": database.stat().st_uid,
                "query_only": query_only,
                "user_version": user_version,
                "store_installation_id_sha256": hashlib.sha256(
                    store_rows[0][0].encode("utf-8")
                ).hexdigest(),
            },
            "selector": {
                "installation_id_sha256": hashlib.sha256(
                    installation_id.encode("utf-8")
                ).hexdigest(),
                "lineage_sha256": hashlib.sha256(lineage.encode("utf-8")).hexdigest(),
            },
            "counts": {
                "lineages": len(lineage_rows),
                "applications": len(applications),
                "sequences": len(sequences),
                "pending_publications": len(pending),
                "accumulator_states": len(accumulators),
            },
            "lineage": lineage_rows[0] if lineage_rows else None,
            "applications": applications,
            "sequences": sequences,
            "pending_publications": pending,
            "accumulator_states": accumulators,
        }
    except sqlite3.Error as error:
        raise LedgerObservationError(f"ledger query failed: {error}") from error
    finally:
        connection.close()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True, type=Path)
    parser.add_argument("--installation-id", required=True)
    parser.add_argument("--lineage", required=True)
    arguments = parser.parse_args(argv)
    try:
        observation = observe_ledger(
            arguments.database, arguments.installation_id, arguments.lineage
        )
    except LedgerObservationError as error:
        print(json.dumps({"status": "failed", "error": str(error)}), file=sys.stderr)
        return 2
    print(json.dumps(observation, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

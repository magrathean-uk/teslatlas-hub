# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for sealing an Edge binding to the actual Hub fixture store."""

from __future__ import annotations

import copy
import hashlib
import json
import os
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path

from .edge_fixture_binding import FixtureBindingError, validate_fixture_binding


HUB_ID = "8b5dfd0a-a326-47d8-9981-49d3b4d424c6"
SOURCE_ID = "b424a2bb-2fc9-42e4-8155-45687dbf0a57"
VEHICLE_ID = "2e611641-94e9-44c2-a036-5d34d9dd1e69"
EDGE_INSTALLATION = "edge-r9-installation"
LINEAGE = "edge-r9-lineage"
VIN = "5YJ3E1EA7KF000009"
CAR_ID = 49


class EdgeFixtureBindingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)
        data_dir = self.root / "hub"
        data_dir.mkdir(mode=0o700)
        self.database = data_dir / "hub.sqlite"
        connection = sqlite3.connect(self.database)
        connection.executescript(
            """
            PRAGMA journal_mode=WAL;
            CREATE TABLE hub_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE sources (
                source_id TEXT PRIMARY KEY, source_kind TEXT NOT NULL,
                generation INTEGER NOT NULL, created_at_ms INTEGER NOT NULL
            );
            CREATE TABLE vehicles (
                vehicle_id TEXT PRIMARY KEY, source_id TEXT NOT NULL,
                source_vehicle_key TEXT NOT NULL, vin TEXT,
                display_name TEXT, created_at_ms INTEGER NOT NULL,
                last_seen_at_ms INTEGER NOT NULL
            );
            """
        )
        connection.execute(
            "INSERT INTO hub_metadata(key,value) VALUES ('installation_id',?)", (HUB_ID,)
        )
        connection.execute(
            "INSERT INTO sources VALUES (?,?,?,?)",
            (SOURCE_ID, "owner_api_compat", 1, 1),
        )
        connection.execute(
            "INSERT INTO vehicles VALUES (?,?,?,?,?,?,?)",
            (VEHICLE_ID, SOURCE_ID, str(CAR_ID), VIN, "Empty Edge binding", 1, 1),
        )
        connection.commit()
        checkpoint = connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
        self.assertEqual(checkpoint[0], 0)
        self.assertEqual(checkpoint[1], checkpoint[2])
        connection.close()
        for suffix in ("-wal", "-shm"):
            sidecar = Path(str(self.database) + suffix)
            if sidecar.exists():
                if suffix == "-wal":
                    self.assertEqual(sidecar.stat().st_size, 0)
                sidecar.unlink()
        self.database.chmod(0o600)
        self.fixture = {
            "schema_version": 2,
            "data_dir": str(data_dir),
            "hub_id": HUB_ID,
            "installation_id": EDGE_INSTALLATION,
            "lineage": LINEAGE,
            "source_id": SOURCE_ID,
            "vehicle_id": VEHICLE_ID,
            "vin": VIN,
            "car_id": CAR_ID,
        }
        self.binding = {
            "schema_version": 1,
            "hub_installation_id": HUB_ID,
            "edge_installation_id": EDGE_INSTALLATION,
            "edge_lineage": LINEAGE,
            "source_id": SOURCE_ID,
            "vehicle_id": VEHICLE_ID,
            "vehicle_identity": VIN,
            "car_id": CAR_ID,
        }

    def test_binding_matches_fixture_result_and_actual_store_without_writes(self) -> None:
        """Catches trusting generated JSON instead of the canonical SQLite identities."""
        before = self.database.read_bytes()

        result = validate_fixture_binding(self.fixture, self.binding, self.database)

        self.assertEqual(self.database.read_bytes(), before)
        self.assertTrue(result["database_query_only"])
        self.assertTrue(result["database_immutable"])
        self.assertEqual(self.database.read_bytes()[18:20], b"\x02\x02")
        self.assertEqual(result["checked_relationships"], 7)
        self.assertEqual(result["hub_installation_id"], HUB_ID)
        self.assertEqual(result["edge_installation_id"], EDGE_INSTALLATION)

    def test_in_memory_hub_id_mismatch_fails_closed(self) -> None:
        """Catches sealing a proposed Hub ID before the fixture creates its store."""
        mismatched = copy.deepcopy(self.binding)
        mismatched["hub_installation_id"] = "0b4c45ae-9bd4-4444-af30-832683e0ca27"

        with self.assertRaisesRegex(FixtureBindingError, "hub_installation_id"):
            validate_fixture_binding(self.fixture, mismatched, self.database)

        self.assertEqual(list(self.root.iterdir()), [self.database.parent])

    def test_pending_wal_is_rejected_before_immutable_open(self) -> None:
        """Catches ignoring uncheckpointed state when taking an immutable snapshot."""
        pending_wal = Path(str(self.database) + "-wal")
        pending_wal.write_bytes(b"pending")
        pending_wal.chmod(0o600)

        with self.assertRaisesRegex(FixtureBindingError, "pending WAL"):
            validate_fixture_binding(self.fixture, self.binding, self.database)

    def test_accepted_interop_fixture_store_validates_without_sidecars(self) -> None:
        """Catches divergence from the actual Rust fixture's checkpointed WAL state."""
        fixture_binary = os.environ.get("TESLATLAS_INTEROP_FIXTURE")
        if fixture_binary is None:
            self.skipTest("TESLATLAS_INTEROP_FIXTURE is not configured")
        actual_root = self.root / "actual-empty-edge-binding"
        completed = subprocess.run(
            [
                fixture_binary,
                "--output",
                str(actual_root),
                "--scenario",
                "empty-edge-binding",
                "--source-id",
                SOURCE_ID,
                "--vehicle-id",
                VEHICLE_ID,
                "--vin",
                VIN,
                "--car-id",
                str(CAR_ID),
                "--installation-id",
                EDGE_INSTALLATION,
                "--lineage",
                LINEAGE,
            ],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        fixture = json.loads(completed.stdout)
        database = actual_root / "hub/hub.sqlite"
        binding = copy.deepcopy(self.binding)
        binding["hub_installation_id"] = fixture["hub_id"]
        self.assertFalse(Path(str(database) + "-wal").exists())
        self.assertFalse(Path(str(database) + "-shm").exists())

        def tree_fingerprint() -> list[tuple[str, int, str]]:
            return [
                (
                    str(path.relative_to(actual_root)),
                    path.stat().st_size,
                    hashlib.sha256(path.read_bytes()).hexdigest(),
                )
                for path in sorted(actual_root.rglob("*"))
                if path.is_file()
            ]

        before = tree_fingerprint()
        result = validate_fixture_binding(fixture, binding, database)

        self.assertTrue(result["database_immutable"])
        self.assertTrue(result["database_query_only"])
        self.assertEqual(result["checked_relationships"], 7)
        self.assertEqual(tree_fingerprint(), before)
        self.assertFalse(Path(str(database) + "-wal").exists())
        self.assertFalse(Path(str(database) + "-shm").exists())


if __name__ == "__main__":
    unittest.main()

# SPDX-License-Identifier: AGPL-3.0-only
"""Focused tests for the bounded read-only Edge ledger observer."""

from __future__ import annotations

import hashlib
import sqlite3
import tempfile
import unittest
from pathlib import Path

from .edge_ledger_observer import LedgerObservationError, observe_ledger


INSTALLATION = "fresh-installation"
LINEAGE = "fresh-lineage"
DIGEST_A = "a" * 64
DIGEST_B = "b" * 64
DIGEST_C = "c" * 64


class EdgeLedgerObserverTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)
        self.database = self.root / "hub.sqlite"

    def _create_fixture(self) -> None:
        connection = sqlite3.connect(self.database)
        connection.executescript(
            """
            CREATE TABLE hub_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE edge_lineages (
                installation_id TEXT NOT NULL, lineage TEXT NOT NULL,
                source_id TEXT NOT NULL, vehicle_id TEXT NOT NULL, vin TEXT NOT NULL,
                car_id INTEGER NOT NULL, first_spool_seq INTEGER, ack_frontier INTEGER,
                created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (installation_id, lineage)
            );
            CREATE TABLE edge_applications (
                installation_id TEXT NOT NULL, lineage TEXT NOT NULL,
                stable_record_id TEXT NOT NULL, payload_sha256 TEXT NOT NULL,
                disposition TEXT NOT NULL, first_spool_seq INTEGER NOT NULL,
                observation_id INTEGER, applied_at_ms INTEGER NOT NULL
            );
            CREATE TABLE edge_sequence_dispositions (
                installation_id TEXT NOT NULL, lineage TEXT NOT NULL,
                spool_seq INTEGER NOT NULL, item_kind TEXT NOT NULL,
                item_id TEXT NOT NULL, legacy_record_id TEXT, payload_sha256 TEXT,
                category TEXT NOT NULL, reason TEXT NOT NULL, occurred_at_ms INTEGER,
                evidence_sha256 TEXT, committed_at_ms INTEGER NOT NULL
            );
            CREATE TABLE edge_accumulator_states (
                installation_id TEXT NOT NULL, lineage TEXT NOT NULL,
                vehicle_id TEXT NOT NULL, state_version INTEGER NOT NULL,
                state_json BLOB NOT NULL, through_spool_seq INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE edge_pending_publications (
                installation_id TEXT NOT NULL, lineage TEXT NOT NULL,
                stable_record_id TEXT NOT NULL, vehicle_id TEXT NOT NULL,
                status TEXT NOT NULL, attempts INTEGER NOT NULL,
                next_attempt_ms INTEGER NOT NULL, last_error TEXT,
                created_at_ms INTEGER NOT NULL, completed_at_ms INTEGER
            );
            PRAGMA user_version = 58;
            """
        )
        connection.execute(
            "INSERT INTO hub_metadata(key,value) VALUES ('installation_id',?)",
            (INSTALLATION,),
        )
        connection.execute(
            "INSERT INTO edge_lineages VALUES (?,?,?,?,?,?,?,?,?,?)",
            (INSTALLATION, LINEAGE, "source", "vehicle", "1" * 17, 1, 1, 3, 10, 30),
        )
        for sequence, category, reason, digest, observation in (
            (1, "durable_non_projection_event", "projection_unsupported", DIGEST_A, None),
            (2, "projected_telemetry", "projected", DIGEST_B, 7),
            (3, "durable_non_projection_event", "projection_unsupported", DIGEST_C, None),
        ):
            connection.execute(
                "INSERT INTO edge_applications VALUES (?,?,?,?,?,?,?,?)",
                (INSTALLATION, LINEAGE, digest, digest, category, sequence, observation, 100 + sequence),
            )
            connection.execute(
                "INSERT INTO edge_sequence_dispositions VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    INSTALLATION,
                    LINEAGE,
                    sequence,
                    "record",
                    digest,
                    digest,
                    digest,
                    category,
                    reason,
                    None,
                    None,
                    200 + sequence,
                ),
            )
        connection.execute(
            "INSERT INTO edge_accumulator_states VALUES (?,?,?,?,?,?,?)",
            (INSTALLATION, LINEAGE, "vehicle", 1, b"{}", 3, 300),
        )
        connection.execute(
            "INSERT INTO edge_pending_publications VALUES (?,?,?,?,?,?,?,?,?,?)",
            (INSTALLATION, LINEAGE, DIGEST_B, "vehicle", "complete", 1, 0, None, 200, 301),
        )
        connection.commit()
        connection.close()
        self.database.chmod(0o600)

    def test_observer_classifies_every_sequence_without_writing_database(self) -> None:
        """Catches dropping lifecycle rows or opening the Hub ledger read-write."""
        self._create_fixture()
        before = hashlib.sha256(self.database.read_bytes()).hexdigest()

        observed = observe_ledger(self.database, INSTALLATION, LINEAGE)

        self.assertEqual(hashlib.sha256(self.database.read_bytes()).hexdigest(), before)
        self.assertEqual(list(self.root.iterdir()), [self.database])
        self.assertTrue(observed["database"]["query_only"])
        self.assertEqual(observed["database"]["user_version"], 58)
        self.assertEqual(
            observed["counts"],
            {
                "lineages": 1,
                "applications": 3,
                "sequences": 3,
                "pending_publications": 0,
                "accumulator_states": 1,
            },
        )
        self.assertEqual(observed["lineage"]["ack_frontier"], 3)
        self.assertEqual(
            [row["category"] for row in observed["sequences"]],
            [
                "durable_non_projection_event",
                "projected_telemetry",
                "durable_non_projection_event",
            ],
        )
        self.assertEqual(
            [row["observation_id"] for row in observed["applications"]],
            [None, 7, None],
        )

    def test_observer_rejects_an_absent_database_without_creating_it(self) -> None:
        """Catches sqlite3 silently creating a missing path instead of failing closed."""
        with self.assertRaisesRegex(LedgerObservationError, "does not exist"):
            observe_ledger(self.database, INSTALLATION, LINEAGE)
        self.assertFalse(self.database.exists())


if __name__ == "__main__":
    unittest.main()

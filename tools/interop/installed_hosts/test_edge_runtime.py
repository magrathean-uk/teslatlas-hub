# SPDX-License-Identifier: AGPL-3.0-only
"""Tests for the fail-closed Edge runtime evidence interface."""
import copy
import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from .edge_runtime import EdgeRuntimeError, validate_edge_runtime_evidence


SESSION_ID = "11111111-1111-4111-8111-111111111111"
CELL_ID = "edge_v2__debian13_amd64"


class EdgeRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)

    def binding(self, name):
        path = self.root / name
        raw = json.dumps({"evidence": name}, sort_keys=True, separators=(",", ":")).encode()
        path.write_bytes(raw)
        path.chmod(0o600)
        return {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()}

    def evidence(self):
        lanes = []
        for lane in ("primary", "reference", "fault", "negative"):
            lanes.append({
                "id": lane,
                "hub_generation": self.binding(lane + "-hub-generation.json"),
                "producer_generation": self.binding(lane + "-producer-generation.json"),
                "namespace": self.binding(lane + "-namespace.json"),
                "config": self.binding(lane + "-config.json"),
                "store": self.binding(lane + "-store.json"),
                "spool": None if lane == "negative" else self.binding(lane + "-spool.json"),
                "transport": self.binding(lane + "-transport.json"),
                "witnesses": [self.binding(lane + "-witness.json")],
                "cleanup": self.binding(lane + "-cleanup.json"),
            })
        return {
            "schema_version": 1, "kind": "edge-installed-runtime-evidence",
            "run_id": "edge-run", "cell_id": CELL_ID, "session_id": SESSION_ID,
            "instance_nonce": "b" * 64, "fixture_sha256": "c" * 64,
            "lanes": lanes,
        }

    def test_runtime_interface_requires_every_bound_lane_obligation(self):
        value = self.evidence()
        checked = validate_edge_runtime_evidence(
            value, run_id="edge-run", cell_id=CELL_ID, session_id=SESSION_ID,
            instance_nonce="b" * 64, fixture_sha256="c" * 64,
        )
        self.assertEqual([lane["id"] for lane in checked["lanes"]], ["primary", "reference", "fault", "negative"])

    def test_runtime_interface_rejects_missing_cleanup_or_process_binding(self):
        for mutate, pattern in (
            (lambda value: value["lanes"][2].pop("cleanup"), "missing fields"),
            (lambda value: value["lanes"][0].update(producer_generation=None), "producer generation"),
            (lambda value: value["lanes"][3].update(producer_generation=None), "producer generation"),
            (lambda value: value["lanes"][3].update(spool=self.binding("forged-spool.json")), "negative lane"),
        ):
            with self.subTest(pattern=pattern):
                value = self.evidence()
                mutate(value)
                with self.assertRaisesRegex(EdgeRuntimeError, pattern):
                    validate_edge_runtime_evidence(
                        value, run_id="edge-run", cell_id=CELL_ID, session_id=SESSION_ID,
                        instance_nonce="b" * 64, fixture_sha256="c" * 64,
                    )

    def test_runtime_interface_rechecks_bound_bytes_and_private_modes(self):
        value = self.evidence()
        path = Path(value["lanes"][0]["transport"]["path"])
        path.write_text('{"changed":true}', encoding="utf-8")
        with self.assertRaisesRegex(EdgeRuntimeError, "digest"):
            validate_edge_runtime_evidence(
                value, run_id="edge-run", cell_id=CELL_ID, session_id=SESSION_ID,
                instance_nonce="b" * 64, fixture_sha256="c" * 64,
            )


if __name__ == "__main__":
    unittest.main()

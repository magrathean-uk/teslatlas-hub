# SPDX-License-Identifier: AGPL-3.0-only
"""Contract tests for the private Edge session-v2 fixture boundary."""
import copy
import hashlib
import json
import os
import tempfile
import unittest
from pathlib import Path

from .contract import (
    ContractError,
    MAX_JSON_BYTES,
    read_edge_session_config,
    validate_edge_session_config,
)
from .edge_fixture import (
    EdgeFixtureError,
    build_edge_fixture_input,
    write_edge_fixture_input,
)


DIGEST = "a" * 64
SESSION_ID = "11111111-1111-4111-8111-111111111111"
CELL_ID = "edge_v2__debian13_amd64"


class EdgeFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.root.chmod(0o700)

    def binding(self, name, value=None):
        path = self.root / name
        raw = json.dumps(value if value is not None else {"fixed": True}, sort_keys=True, separators=(",", ":")).encode()
        path.write_bytes(raw)
        path.chmod(0o600)
        return {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()}

    def owner(self):
        primary = self.root / "runs" / CELL_ID
        certificate_roles = {
            "edge_server_ca", "edge_server_leaf", "edge_client_ca",
            "edge_client_leaf", "negative_server_ca", "negative_server_leaf",
            "untrusted_client_leaf",
        }
        roles = (
            "edge_server_ca", "edge_server_leaf", "edge_server_key",
            "edge_client_ca", "edge_client_leaf", "edge_client_key",
            "edge_delivery_bearer", "edge_receiver_bearer", "edge_spool_key",
            "edge_credential_store", "negative_server_ca", "negative_server_leaf",
            "negative_server_key", "untrusted_client_leaf", "untrusted_client_key",
            "bad_delivery_bearer",
        )
        files = []
        for role in roles:
            lane = "negative" if role.startswith(("negative_", "untrusted_", "bad_")) else "primary"
            root_binding = self.binding("credential-root-" + role)
            local_binding = self.binding("credential-local-" + role)
            self.assertEqual(root_binding["sha256"], local_binding["sha256"])
            files.append({
                "id": lane + "." + role, "role": role,
                "root": root_binding, "local": local_binding, "uid": 997,
                "mode": 384,
                "bytes": Path(root_binding["path"]).stat().st_size,
                "der_sha256": "d" * 64 if role in certificate_roles else None,
            })
        credentials = self.binding("credentials.json", {
            "schema_version": 1, "session_id": SESSION_ID, "cell_id": CELL_ID,
            "instance_nonce": "b" * 64, "files": files,
        })
        return {
            "schema_version": 1, "kind": "edge-fixture-owner-config",
            "run_id": "edge-run", "cell_id": CELL_ID, "session_id": SESSION_ID,
            "instance_nonce": "b" * 64,
            "host_registration": self.binding("host.json"),
            "recipe": self.binding("recipe.json"),
            "vectors": self.binding("vectors.json"),
            "edge_profile": {
                "manifest": self.binding("edge-profile-sums"),
                "members": [self.binding("edge-profile-member-{:02d}".format(index)) for index in range(19)],
            },
            "producer_registration": self.binding("producer.json"),
            "launch_inventory": self.binding("launch.json"),
            "credentials": credentials,
            "private_root": str(self.root / "private"),
            "primary_hub_root": str(primary),
        }

    def reservation(self):
        primary = self.root / "runs" / CELL_ID
        lanes = []
        values = (
            ("primary", "installed", 18480, 18500, 18510, primary),
            ("reference", "normal_aux", 18490, 18501, 18511, primary.parent / (CELL_ID + "-edge-reference")),
            ("fault", "fault_aux", 18491, 18502, 18512, primary.parent / (CELL_ID + "-edge-fault")),
            ("negative", "normal_aux", 18492, 18503, 18513, primary.parent / (CELL_ID + "-edge-negative")),
        )
        for lane, role, hub, receiver, delivery, hub_root in values:
            lanes.append({
                "id": lane, "hub_role": role, "hub_root": str(hub_root),
                "producer_root": str(self.root / ("producer-" + lane)),
                "hub_port": hub, "receiver_port": receiver, "delivery_port": delivery,
            })
        return {
            "schema_version": 1, "kind": "edge-fixture-reservation",
            "run_id": "edge-run", "cell_id": CELL_ID, "session_id": SESSION_ID,
            "instance_nonce": "b" * 64, "lease": self.binding("lease.json"),
            "lanes": lanes,
        }

    def test_edge_v2_session_is_additive_and_other_clients_remain_v1_only(self):
        value = {
            "schema_version": 2, "run_id": "edge-run",
            "edge_fixture": {"path": "/private/edge-fixture.json", "sha256": DIGEST},
            "scenario": {"path": "/private/scenario.json", "sha256": DIGEST},
        }
        self.assertEqual(
            validate_edge_session_config(value, adapter_id="edge_v2", client_id="edge_v2", cell_id=CELL_ID),
            value,
        )
        with self.assertRaisesRegex(ContractError, "Edge-only"):
            validate_edge_session_config(value, adapter_id="viewer", client_id="viewer", cell_id="viewer__debian13_amd64")
        with self.assertRaisesRegex(ContractError, "unexpected fields"):
            validate_edge_session_config({**value, "argv": ["/bin/sh"]}, adapter_id="edge_v2", client_id="edge_v2", cell_id=CELL_ID)

    def test_edge_v2_reader_rechecks_exact_private_config_bytes(self):
        value = {
            "schema_version": 2, "run_id": "edge-run",
            "edge_fixture": {"path": "/private/edge-fixture.json", "sha256": DIGEST},
            "scenario": {"path": "/private/scenario.json", "sha256": DIGEST},
        }
        bound = self.binding("edge-session.json", value)
        self.assertEqual(
            read_edge_session_config(bound, adapter_id="edge_v2", client_id="edge_v2", cell_id=CELL_ID),
            value,
        )
        Path(bound["path"]).write_text("{}", encoding="utf-8")
        with self.assertRaisesRegex(ContractError, "digest"):
            read_edge_session_config(bound, adapter_id="edge_v2", client_id="edge_v2", cell_id=CELL_ID)

    def test_edge_v2_reader_rejects_duplicate_keys(self):
        path = self.root / "duplicate-session.json"
        raw = (
            '{"schema_version":2,"run_id":"edge-run","run_id":"other",'
            '"edge_fixture":{"path":"/private/edge-fixture.json","sha256":"' + DIGEST + '"},'
            '"scenario":{"path":"/private/scenario.json","sha256":"' + DIGEST + '"}}'
        ).encode()
        path.write_bytes(raw)
        path.chmod(0o600)
        bound = {"path": str(path), "sha256": hashlib.sha256(raw).hexdigest()}
        with self.assertRaisesRegex(ContractError, "duplicate"):
            read_edge_session_config(bound, adapter_id="edge_v2", client_id="edge_v2", cell_id=CELL_ID)

    def test_fixture_producer_binds_exact_reserved_four_lane_topology(self):
        fixture = build_edge_fixture_input(self.owner(), self.reservation())
        self.assertEqual([lane["id"] for lane in fixture["lanes"]], ["primary", "reference", "fault", "negative"])
        self.assertEqual(
            [(lane["hub_port"], lane["receiver_port"], lane["delivery_port"]) for lane in fixture["lanes"]],
            [(18480, 18500, 18510), (18490, 18501, 18511), (18491, 18502, 18512), (18492, 18503, 18513)],
        )
        self.assertNotIn("executable", json.dumps(fixture, sort_keys=True))
        output = self.root / "sealed" / "edge-fixture.json"
        binding = write_edge_fixture_input(output, fixture)
        self.assertEqual(binding["sha256"], hashlib.sha256(output.read_bytes()).hexdigest())
        self.assertEqual(os.stat(output).st_mode & 0o777, 0o600)
        with self.assertRaisesRegex(EdgeFixtureError, "fresh"):
            write_edge_fixture_input(output, fixture)

    def test_fixture_permits_a_public_profile_member_larger_than_private_metadata(self):
        owner, reservation = self.owner(), self.reservation()
        path = self.root / "public-profile-member.json"
        raw = b'{"payload":"' + (b"x" * (MAX_JSON_BYTES + 1)) + b'"}\n'
        path.write_bytes(raw)
        path.chmod(0o644)
        owner["edge_profile"]["members"][0] = {
            "path": str(path),
            "sha256": hashlib.sha256(raw).hexdigest(),
        }

        fixture = build_edge_fixture_input(owner, reservation)

        self.assertEqual(owner["edge_profile"]["members"][0], fixture["edge_profile"]["members"][0])

    def test_fixture_rejects_unreserved_ports_aliases_and_identity_drift(self):
        for mutate, pattern in (
            (lambda owner, reservation: reservation["lanes"][0].update(delivery_port=19999), "topology"),
            (lambda owner, reservation: reservation["lanes"][1].update(producer_root=reservation["lanes"][0]["producer_root"]), "alias"),
            (lambda owner, reservation: owner.update(run_id="foreign-run"), "identity"),
        ):
            with self.subTest(pattern=pattern):
                owner, reservation = self.owner(), self.reservation()
                mutate(owner, reservation)
                with self.assertRaisesRegex(EdgeFixtureError, pattern):
                    build_edge_fixture_input(owner, reservation)

    def test_fixture_rejects_symlink_bound_private_input(self):
        owner, reservation = self.owner(), self.reservation()
        target = self.root / "real-recipe.json"
        target.write_text("{}", encoding="utf-8")
        target.chmod(0o600)
        link = self.root / "linked-recipe.json"
        link.symlink_to(target)
        owner["recipe"] = {"path": str(link), "sha256": hashlib.sha256(target.read_bytes()).hexdigest()}
        with self.assertRaisesRegex(EdgeFixtureError, "canonical|regular"):
            build_edge_fixture_input(owner, reservation)

    def test_fixture_rejects_incomplete_profile_or_credential_custody(self):
        owner, reservation = self.owner(), self.reservation()
        owner["edge_profile"]["members"].pop()
        with self.assertRaisesRegex(EdgeFixtureError, "19"):
            build_edge_fixture_input(owner, reservation)

        owner, reservation = self.owner(), self.reservation()
        credentials_path = Path(owner["credentials"]["path"])
        credentials = json.loads(credentials_path.read_text())
        credentials["files"].pop()
        owner["credentials"] = self.binding("credentials-incomplete.json", credentials)
        with self.assertRaisesRegex(EdgeFixtureError, "credential custody"):
            build_edge_fixture_input(owner, reservation)


if __name__ == "__main__":
    unittest.main()

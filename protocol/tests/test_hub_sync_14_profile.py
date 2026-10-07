import base64
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from jsonschema import Draft202012Validator
from openapi_spec_validator import validate as validate_openapi
import zstandard

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.4.0"


class PhysicalChangedSetProfileTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("hub_sync_14", ROOT / "conformance/hub_sync_14.py")
        cls.contract = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.contract)
        cls.request = json.loads((PROFILE / "examples/changes-since-request-physical.json").read_text())["request"]
        cls.receipt = json.loads((PROFILE / "examples/changes-since-physical-changed-set.json").read_text())["receipt"]
        cls.pack = (PROFILE / "packs/physical-changed-set-v1.sqlite.zst").read_bytes()

    def fixture_signed(self, value):
        value = copy.deepcopy(value)
        # Published RFC 8032 fixture seed, never a live signing credential.
        key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex(
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"))
        payload = self.contract._canonical_unsigned(value)
        value["signature"] = {
            "algorithm": "ed25519", "key_id": self.receipt["signature"]["key_id"],
            "signed_payload_sha256": hashlib.sha256(payload).hexdigest(),
            "signature": base64.b64encode(key.sign(payload)).decode("ascii"),
        }
        return value

    def edited_pack(self, edit, *, content_size=True):
        raw = zstandard.ZstdDecompressor().decompress(self.pack)
        receipt = copy.deepcopy(self.receipt)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "edited.sqlite"
            path.write_bytes(raw)
            connection = sqlite3.connect(path)
            try:
                changed = edit(connection)
                if isinstance(changed, sqlite3.Cursor):
                    self.assertGreater(changed.rowcount, 0, "mutation must change a fixture row")
                receipt["total_rows"] = connection.execute("SELECT COUNT(*) FROM row_roles").fetchone()[0] + connection.execute("SELECT COUNT(*) FROM tombstones").fetchone()[0]
                for table, columns, prefix in (
                        ("affected_projected_ids", "table_name, entity_id, effect", "affected_projected_ids"),
                        ("impacted_roots", "root_type, root_id, base_state, target_state, target_child_count", "impacted_roots")):
                    rows = list(connection.execute(f"SELECT {columns} FROM {table}"))
                    receipt[prefix + "_count"] = len(rows)
                    receipt[prefix + "_sha256"] = self.contract._rows_digest(rows)
                connection.commit()
            finally:
                connection.close()
            raw = path.read_bytes()
        compressed = zstandard.ZstdCompressor(write_content_size=content_size, write_checksum=True).compress(raw)
        receipt["chunks"][0]["pack"].update(sha256=hashlib.sha256(compressed).hexdigest(), compressed_bytes=len(compressed))
        receipt["total_compressed_bytes"] = len(compressed)
        receipt["total_uncompressed_bytes"] = len(raw)
        return self.fixture_signed(receipt), compressed

    def admit_pack_bindings(self, receipt):
        self.contract.verify_receipt(self.request, receipt, self.receipt["vehicle_id"])
        self.contract.verify_signed_manifest_bindings(
            receipt, (PROFILE / "examples/signed-full-manifest-42.json").read_bytes(),
            (PROFILE / "examples/signed-full-manifest-43.json").read_bytes())

    def edited_rebase(self, edit, *, resign_child=True):
        child = json.loads((PROFILE / "examples/signed-full-manifest-43.json").read_bytes())
        edit(child)
        if resign_child:
            child = self.fixture_signed(child)
        raw = (json.dumps(child, indent=2, sort_keys=True) + "\n").encode("ascii")
        hint = json.loads((PROFILE / "examples/changes-since-physical-rebase.json").read_text())["hint"]
        digest = hashlib.sha256(raw).hexdigest()
        hint["replacement"].update(manifest_sha256=digest, chunks=child["chunks"])
        hint["retry_request"]["base_manifest_sha256"] = digest
        return self.fixture_signed(hint), raw

    def test_generated_bundle_and_machine_schemas(self):
        check = subprocess.run([sys.executable, str(ROOT / "tools/build_hub_sync_14_profile.py"), "--check"], capture_output=True)
        self.assertEqual(check.returncode, 0, check.stderr.decode())
        self.assertEqual(self.contract.verify_bundle()["profile_id"], "hub-sync-v1@1.4.0")
        for name in ("changes-since-request.schema.json", "physical-changed-set-receipt.schema.json", "physical-rebase-hint.schema.json"):
            Draft202012Validator.check_schema(json.loads((PROFILE / name).read_text()))
        validate_openapi(json.loads((PROFILE / "openapi.json").read_text()), base_uri=PROFILE.as_uri() + "/")

    def test_positive_signed_changed_set_and_complete_pack(self):
        self.contract.verify_receipt(self.request, self.receipt, self.receipt["vehicle_id"])
        self.assertTrue(self.contract.verify_signed_manifest_bindings(
            self.receipt, (PROFILE / "examples/signed-full-manifest-42.json").read_bytes(),
            (PROFILE / "examples/signed-full-manifest-43.json").read_bytes()))
        self.assertTrue(self.contract.verify_pack(self.receipt, self.pack))
        catalog = json.loads((PROFILE / "physical-field-catalog.json").read_text())
        self.assertEqual(catalog["mapped_source_fields"], 168)
        self.assertEqual(catalog["wire_columns"], 205)
        self.assertEqual(len(catalog["tables"]), 11)
        self.assertEqual(catalog["excluded_source_fields"], ["addresses.raw"])

    def test_unadvertised_capability_and_wrong_base_fail_closed(self):
        unadvertised = dict(self.request)
        unadvertised.pop("accepted_changed_set_formats")
        with self.assertRaisesRegex(self.contract.ContractError, "request violates"):
            self.contract.verify_receipt(unadvertised, self.receipt, self.receipt["vehicle_id"])
        other_base = dict(self.request, base_receipt_id="receipt_other")
        with self.assertRaisesRegex(self.contract.ContractError, "base checkpoint mismatch"):
            self.contract.verify_receipt(other_base, self.receipt, self.receipt["vehicle_id"])

    def test_tampering_and_pack_hash_fail_closed(self):
        tampered = json.loads(json.dumps(self.receipt))
        tampered["target"]["sequence"] += 1
        with self.assertRaises(self.contract.ContractError):
            self.contract.verify_receipt(self.request, tampered, self.receipt["vehicle_id"])
        with self.assertRaisesRegex(self.contract.ContractError, "pack digest mismatch"):
            self.contract.verify_pack(self.receipt, self.pack + b"x")

    def test_physical_binary64_lengths_and_nan_pairs_fail_after_valid_bindings(self):
        cases = (
            ("short binary64", "UPDATE drives SET distance_f64_be=x'01'"),
            ("long binary64", "UPDATE drives SET end_km_f64_be=x'000000000000000000'"),
            ("empty binary64", "UPDATE positions SET odometer_f64_be=x''"),
            ("nonboolean NaN flag", "UPDATE positions SET latitude_e6=1, latitude_e6_is_nan=2"),
            ("negative NaN flag", "UPDATE drives SET outside_temp_avg_e1_is_nan=-1"),
            ("NaN with finite value", "UPDATE charging_processes SET cost_e2=1, cost_e2_is_nan=1"),
        )
        for label, statement in cases:
            with self.subTest(case=label):
                receipt, compressed = self.edited_pack(lambda connection: connection.execute(statement))
                self.admit_pack_bindings(receipt)
                with self.assertRaisesRegex(self.contract.ContractError, "physical value representation mismatch"):
                    self.contract.verify_pack(receipt, compressed)
        def add_car(connection):
            connection.execute("INSERT INTO cars(id,eid,vid,efficiency,display_priority,inserted_at_pg_us,updated_at_pg_us,settings_id) VALUES(1,1,1,x'01',0,0,0,1)")
            connection.execute("INSERT INTO row_roles VALUES('cars',1,'context')")
        receipt, compressed = self.edited_pack(add_car)
        self.admit_pack_bindings(receipt)
        with self.assertRaisesRegex(self.contract.ContractError, "physical value representation mismatch"):
            self.contract.verify_pack(receipt, compressed)

    def test_physical_binary64_bits_nan_null_and_signed64_are_lossless(self):
        catalog = json.loads((PROFILE / "physical-field-catalog.json").read_text())
        bits = ("0000000000000000", "8000000000000000", "7ff0000000000000",
                "fff0000000000000", "7ff8000000000001", "fff0000000000001")
        for pattern in bits:
            with self.subTest(binary64=pattern):
                def edit(connection):
                    for table in catalog["tables"]:
                        for field in table["columns"]:
                            if field["sqlite_type"] == "BLOB":
                                connection.execute(f"UPDATE {table['name']} SET {field['name']}=?", (bytes.fromhex(pattern),))
                    connection.execute("UPDATE drives SET outside_temp_avg_e1=?, outside_temp_avg_e1_is_nan=0", (2**63 - 1,))
                    connection.execute("UPDATE positions SET latitude_e6=?, latitude_e6_is_nan=0, longitude_e6=NULL, longitude_e6_is_nan=1, outside_temp_e1=NULL, outside_temp_e1_is_nan=0", (-2**63,))
                receipt, compressed = self.edited_pack(edit)
                self.admit_pack_bindings(receipt)
                self.assertTrue(self.contract.verify_pack(receipt, compressed))

    def test_bounded_zstd_known_unknown_and_multiple_complete_frames(self):
        raw = zstandard.ZstdDecompressor().decompress(self.pack)
        frames = {
            "unknown size": zstandard.ZstdCompressor(write_content_size=False).compress(raw),
            "two complete frames": zstandard.ZstdCompressor().compress(raw[:len(raw)//2]) + zstandard.ZstdCompressor(write_content_size=False).compress(raw[len(raw)//2:]),
            "skippable frame": bytes.fromhex("502a4d18") + (3).to_bytes(4, "little") + b"abc" + self.pack,
        }
        for label, compressed in frames.items():
            with self.subTest(case=label):
                receipt = copy.deepcopy(self.receipt)
                receipt["chunks"][0]["pack"].update(sha256=hashlib.sha256(compressed).hexdigest(), compressed_bytes=len(compressed))
                receipt["total_compressed_bytes"] = len(compressed)
                receipt = self.fixture_signed(receipt)
                self.admit_pack_bindings(receipt)
                self.assertTrue(self.contract.verify_pack(receipt, compressed))
        for content_size in (True, False):
            with self.subTest(content_size=content_size), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "bounded.sqlite"
                compressed = zstandard.ZstdCompressor(write_content_size=content_size).compress(b"x" * 4096)
                with self.assertRaisesRegex(self.contract.ContractError, "uncompressed byte limit"):
                    self.contract._decompress_pack(compressed, path, 1024, max_output_bytes=1024)
                self.assertLessEqual(path.stat().st_size if path.exists() else 0, 1024)
                if content_size:
                    self.assertFalse(path.exists(), "advertised expansion fails before output allocation")

    def test_zstd_truncation_corruption_and_trailing_garbage_fail_closed(self):
        raw = zstandard.ZstdDecompressor().decompress(self.pack)
        complete = zstandard.ZstdCompressor(write_checksum=True).compress(raw)
        for label, compressed in (
                ("truncated checksum", complete[:-2]),
                ("truncated block", complete[:12]),
                ("corrupt checksum", complete[:-1] + bytes([complete[-1] ^ 1])),
                ("trailing garbage", complete + b"garbage")):
            with self.subTest(case=label):
                receipt = copy.deepcopy(self.receipt)
                receipt["chunks"][0]["pack"].update(sha256=hashlib.sha256(compressed).hexdigest(), compressed_bytes=len(compressed))
                receipt["total_compressed_bytes"] = len(compressed)
                receipt = self.fixture_signed(receipt)
                self.admit_pack_bindings(receipt)
                with self.assertRaises(self.contract.ContractError):
                    self.contract.verify_pack(receipt, compressed)

    def test_negative_signed_fixtures_bindings_closure_and_bounds(self):
        for label, expected in (("missing-closure", "incomplete impacted root"),
                                ("wrong-binding", "physical pack binding mismatch"),
                                ("wrong-open-state", "impacted root open/closed state mismatch")):
            with self.subTest(label=label):
                receipt = json.loads((PROFILE / f"examples/changes-since-physical-{label}.json").read_text())["receipt"]
                pack_name = receipt["chunks"][0]["pack"]["object_name"]
                self.contract.verify_receipt(self.request, receipt, receipt["vehicle_id"])
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_pack(receipt, (PROFILE / pack_name).read_bytes())
        wrong_source = json.loads((PROFILE / "examples/changes-since-physical-wrong-source.json").read_text())["receipt"]
        with self.assertRaisesRegex(self.contract.ContractError, "source vehicle mismatch"):
            self.contract.verify_receipt(self.request, wrong_source, self.receipt["vehicle_id"])
        wrong_generation = json.loads((PROFILE / "examples/changes-since-physical-wrong-generation.json").read_text())["receipt"]
        with self.assertRaisesRegex(self.contract.ContractError, "source binding mismatch"):
            self.contract.verify_receipt(self.request, wrong_generation, self.receipt["vehicle_id"])
        over_limit = json.loads((PROFILE / "examples/changes-since-physical-over-limit.json").read_text())["receipt"]
        with self.assertRaisesRegex(self.contract.ContractError, "receipt violates 1.4 schema"):
            self.contract.verify_receipt(self.request, over_limit, self.receipt["vehicle_id"])

    def test_previous_profile_noop_is_still_admitted(self):
        prior = ROOT / "profiles/hub-sync-v1/1.3.0"
        noop = json.loads((prior / "examples/sync-noop-signed.json").read_text())
        self.assertEqual(noop["receipt"]["kind"], "no_op")
        self.assertEqual(noop["receipt"]["manifest_schema"], "2.2")
        self.assertEqual(self.contract.verify_bundle()["previous_profile"], "hub-sync-v1@1.3.0")

    def test_signed_rebase_carries_exact_14_retry(self):
        hint = json.loads((PROFILE / "examples/changes-since-physical-rebase.json").read_text())["hint"]
        target = (PROFILE / "examples/signed-full-manifest-43.json").read_bytes()
        self.assertTrue(self.contract.verify_rebase(self.request, hint, self.receipt["vehicle_id"], target))
        tampered = json.loads(json.dumps(hint))
        tampered["retry_request"].pop("accepted_changed_set_formats")
        with self.assertRaisesRegex(self.contract.ContractError, "rebase violates"):
            self.contract.verify_rebase(self.request, tampered, self.receipt["vehicle_id"], target)

    def test_rebase_admits_selected_schema_vehicle_kind_chunks_and_signature(self):
        cases = (
            ("wrong vehicle", lambda child: child.update(vehicle_id="44444444-4444-4444-8444-444444444444"), True, "vehicle mismatch"),
            ("wrong selected schema", lambda child: child.update(schema_version="2.1"), True, "violates schema 2.2"),
            ("wrong kind", lambda child: child.update(kind="no_op"), True, "violates schema 2.2"),
            ("missing required member", lambda child: child.pop("vehicle_id"), True, "violates schema 2.2"),
            ("noncontiguous chunks", lambda child: child["chunks"][0].update(chunk_index=1), True, "not contiguous"),
            ("invalid child signature", lambda child: child["signature"].update(signature=base64.b64encode(bytes(64)).decode("ascii")), False, "signature invalid"),
        )
        for label, edit, resign, expected in cases:
            with self.subTest(case=label):
                hint, raw = self.edited_rebase(edit, resign_child=resign)
                schema = json.loads((PROFILE / "physical-rebase-hint.schema.json").read_text())
                self.assertFalse(list(Draft202012Validator({**schema, "$ref": "#/$defs/hint"}).iter_errors(hint)))
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_rebase(self.request, hint, self.receipt["vehicle_id"], raw)

    def test_rebase_rejects_duplicate_members_under_valid_outer_binding(self):
        target = (PROFILE / "examples/signed-full-manifest-43.json").read_bytes()
        for name, duplicate in (
                ("vehicle_id", '"44444444-4444-4444-8444-444444444444"'),
                ("schema_version", '"2.1"')):
            with self.subTest(member=name):
                raw = ("{" + json.dumps(name) + ":" + duplicate + ",").encode("ascii") + target.lstrip()[1:]
                self.assertEqual(json.loads(target), json.loads(raw), "last-value view preserves the child signature")
                hint = copy.deepcopy(json.loads((PROFILE / "examples/changes-since-physical-rebase.json").read_text())["hint"])
                digest = hashlib.sha256(raw).hexdigest()
                hint["replacement"]["manifest_sha256"] = digest
                hint["retry_request"]["base_manifest_sha256"] = digest
                hint = self.fixture_signed(hint)
                with self.assertRaisesRegex(self.contract.ContractError, "not valid JSON"):
                    self.contract.verify_rebase(self.request, hint, self.receipt["vehicle_id"], raw)

    def test_child_closure_work_does_not_repeat_for_each_impacted_root(self):
        def workload(connection, root_count):
            drive = list(connection.execute("SELECT * FROM drives LIMIT 1").fetchone())
            position = list(connection.execute("SELECT * FROM positions LIMIT 1").fetchone())
            connection.execute("DELETE FROM drives")
            connection.execute("DELETE FROM positions")
            connection.execute("DELETE FROM row_roles WHERE table_name IN ('drives','positions')")
            connection.execute("DELETE FROM impacted_roots WHERE root_type='drive'")
            connection.execute("DELETE FROM affected_projected_ids WHERE table_name IN ('drives','positions')")
            for index in range(64):
                drive[0], drive[3] = index, None
                connection.execute("INSERT INTO drives VALUES(" + ",".join("?" for _ in drive) + ")", drive)
                connection.execute("INSERT INTO row_roles VALUES('drives',?,'context')", (index,))
                connection.execute("INSERT INTO affected_projected_ids VALUES('drives',?,'recompute')", (index,))
                if index < root_count:
                    connection.execute("INSERT INTO impacted_roots VALUES('drive',?,'closed','open',64)", (index,))
            for index in range(4096):
                position[0], position[2] = index, index % 64
                connection.execute("INSERT INTO positions VALUES(" + ",".join("?" for _ in position) + ")", position)
                connection.execute("INSERT INTO row_roles VALUES('positions',?,'context')", (index,))
                connection.execute("INSERT INTO affected_projected_ids VALUES('positions',?,'recompute')", (index,))

        instruction_batches = []
        connect = sqlite3.connect
        class MeasuredConnection(sqlite3.Connection):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, **kwargs)
                self.set_progress_handler(lambda: instruction_batches.append(1) or 0, 1000)
        def measured_connect(*args, **kwargs):
            return connect(*args, **kwargs, factory=MeasuredConnection)
        work = []
        for root_count in (16, 64):
            receipt, compressed = self.edited_pack(lambda connection: workload(connection, root_count))
            self.admit_pack_bindings(receipt)
            instruction_batches.clear()
            with patch.object(self.contract.sqlite3, "connect", measured_connect):
                self.assertTrue(self.contract.verify_pack(receipt, compressed))
            work.append(len(instruction_batches))
        self.assertLess(work[1], work[0] * 2, "four times the roots must not cause four child-table scans per root")

    def test_full_rebase_over_delta_cap_and_full_boundary(self):
        fixture = json.loads((PROFILE / "examples/changes-since-physical-rebase-large.json").read_text())
        hint = fixture["hint"]
        manifest = (PROFILE / "examples/signed-full-manifest-43-large.json").read_bytes()
        self.assertEqual(len(hint["replacement"]["chunks"]), 65)
        self.assertTrue(self.contract.verify_rebase(self.request, hint, self.receipt["vehicle_id"], manifest))
        schema = json.loads((PROFILE / "physical-rebase-hint.schema.json").read_text())
        validation = {**schema, "$ref": "#/$defs/hint"}
        at_full_limit = json.loads(json.dumps(hint))
        one_pack = at_full_limit["replacement"]["chunks"][0]["pack"]
        at_full_limit["replacement"]["chunks"] = [
            {"chunk_index": index, "pack": one_pack} for index in range(1771)
        ]
        self.assertFalse(list(Draft202012Validator(validation).iter_errors(at_full_limit)))
        beyond_full_limit = json.loads(json.dumps(at_full_limit))
        beyond_full_limit["replacement"]["chunks"].append({"chunk_index": 1771, "pack": one_pack})
        self.assertTrue(list(Draft202012Validator(validation).iter_errors(beyond_full_limit)))
        delta_schema = json.loads((PROFILE / "physical-changed-set-receipt.schema.json").read_text())
        delta_validation = {**delta_schema, "$ref": "#/$defs/receipt"}
        over_delta_limit = json.loads(json.dumps(self.receipt))
        over_delta_limit["chunks"] = hint["replacement"]["chunks"]
        self.assertTrue(list(Draft202012Validator(delta_validation).iter_errors(over_delta_limit)))

    def test_absent_drive_retains_lossless_soft_fk_and_requires_detach_scope(self):
        positive = json.loads((PROFILE / "examples/changes-since-physical-absent-drive-surviving-position.json").read_text())["receipt"]
        self.contract.verify_receipt(self.request, positive, positive["vehicle_id"])
        pack = (PROFILE / positive["chunks"][0]["pack"]["object_name"]).read_bytes()
        self.assertTrue(self.contract.verify_pack(positive, pack))
        for name, expected in (
            ("absent-drive-missing-surviving-position", "incomplete impacted root"),
            ("absent-drive-missing-detach-scope", "missing surviving-child recomputation scope"),
        ):
            with self.subTest(name=name):
                receipt = json.loads((PROFILE / f"examples/changes-since-physical-{name}.json").read_text())["receipt"]
                self.contract.verify_receipt(self.request, receipt, receipt["vehicle_id"])
                bad_pack = (PROFILE / receipt["chunks"][0]["pack"]["object_name"]).read_bytes()
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_pack(receipt, bad_pack)

    def test_delta_row_bound_admits_normal_30_day_cut(self):
        definition = json.loads((PROFILE / "physical-changed-set-receipt.schema.json").read_text())
        receipt_schema = {**definition, "$ref": "#/$defs/receipt"}
        accepted = json.loads(json.dumps(self.receipt))
        accepted["total_rows"] = 311_288
        self.assertFalse(list(Draft202012Validator(receipt_schema).iter_errors(accepted)))
        rejected = json.loads(json.dumps(self.receipt))
        rejected["total_rows"] = 2_000_001
        self.assertTrue(list(Draft202012Validator(receipt_schema).iter_errors(rejected)))

    def test_changed_set_manifest_binding_rejects_malformed_complete_heads(self):
        baseline = (PROFILE / "examples/signed-full-manifest-43.json").read_bytes()
        base = (PROFILE / "examples/signed-full-manifest-42.json").read_bytes()
        cases = (
            ("wrong kind", lambda child: child.update(kind="no_op"), "violates schema 2.2"),
            ("empty chunks", lambda child: child.update(chunks=[]), "violates schema 2.2"),
            ("wrong chunk index", lambda child: child['chunks'][0].update(chunk_index=1), "not contiguous"),
        )
        for label, edit, expected in cases:
            with self.subTest(case=label):
                child = json.loads(baseline)
                edit(child)
                raw = json.dumps(self.fixture_signed(child), separators=(",", ":")).encode()
                digest = hashlib.sha256(raw).hexdigest()
                receipt, compressed = self.edited_pack(lambda c: c.execute(
                    "UPDATE delta_metadata SET value=? WHERE key='target_manifest_sha256'", (digest,)))
                receipt['target']['manifest_sha256'] = digest
                receipt = self.fixture_signed(receipt)
                self.contract.verify_receipt(self.request, receipt, receipt['vehicle_id'])
                self.assertTrue(self.contract.verify_pack(receipt, compressed), "pack binding is independently valid")
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_signed_manifest_bindings(receipt, base, raw)
        duplicate = b'{"kind":"no_op",' + baseline.lstrip()[1:]
        receipt = copy.deepcopy(self.receipt)
        receipt['target']['manifest_sha256'] = hashlib.sha256(duplicate).hexdigest()
        receipt = self.fixture_signed(receipt)
        self.contract.verify_receipt(self.request, receipt, receipt['vehicle_id'])
        with self.assertRaisesRegex(self.contract.ContractError, "not valid JSON"):
            self.contract.verify_signed_manifest_bindings(receipt, base, duplicate)

    def test_direct_operations_and_root_children_require_signed_scope(self):
        for statement, expected in (
            ("DELETE FROM affected_projected_ids; DELETE FROM impacted_roots", "missing changed-row"),
            ("DELETE FROM impacted_roots", "missing impacted root"),
            ("DELETE FROM affected_projected_ids WHERE table_name='car_states'", "missing tombstone"),
            ("DELETE FROM affected_projected_ids WHERE table_name='charge_samples'", "missing root-child"),
            ("DELETE FROM affected_projected_ids WHERE table_name='positions'", "missing root-child"),
        ):
            with self.subTest(statement=statement):
                def omit(c):
                    c.executescript(statement)
                receipt, compressed = self.edited_pack(omit)
                self.admit_pack_bindings(receipt)
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_pack(receipt, compressed)
        def changed_child(c):
            c.execute("UPDATE row_roles SET role='changed' WHERE table_name='positions'")
            c.execute("DELETE FROM impacted_roots WHERE root_type='drive'")
            c.execute("UPDATE row_roles SET role='context' WHERE table_name='drives'")
        receipt, compressed = self.edited_pack(changed_child)
        self.admit_pack_bindings(receipt)
        with self.assertRaisesRegex(self.contract.ContractError, "missing changed-child"):
            self.contract.verify_pack(receipt, compressed)
        # Unchanged context outside roots/scope remains legal at this admission layer.
        def context(c):
            c.execute("UPDATE row_roles SET role='context' WHERE table_name='positions'")
            c.execute("UPDATE positions SET drive_id=NULL")
            c.execute("UPDATE impacted_roots SET target_child_count=0 WHERE root_type='drive'")
            c.execute("DELETE FROM affected_projected_ids WHERE table_name='positions'")
        receipt, compressed = self.edited_pack(context)
        self.admit_pack_bindings(receipt)
        self.assertTrue(self.contract.verify_pack(receipt, compressed))

    def test_absent_root_requires_explicit_raw_deletion(self):
        def absent(c):
            c.execute("DELETE FROM drives WHERE id=10")
            c.execute("DELETE FROM row_roles WHERE table_name='drives' AND entity_id=10")
            c.execute("UPDATE impacted_roots SET target_state='absent' WHERE root_type='drive'")
            c.execute("UPDATE affected_projected_ids SET effect='delete' WHERE table_name='drives'")
        receipt, compressed = self.edited_pack(absent)
        self.admit_pack_bindings(receipt)
        with self.assertRaisesRegex(self.contract.ContractError, "missing raw tombstone"):
            self.contract.verify_pack(receipt, compressed)
        def invented(c):
            absent(c)
            c.execute("UPDATE impacted_roots SET base_state='absent' WHERE root_type='drive'")
        receipt, compressed = self.edited_pack(invented)
        self.admit_pack_bindings(receipt)
        with self.assertRaisesRegex(self.contract.ContractError, "invents deletion"):
            self.contract.verify_pack(receipt, compressed)

    def test_integral_json_number_encodings_preserve_existing_jcs_signatures(self):
        from conformance import hub_sync
        noop_raw = (ROOT / 'profiles/hub-sync-v1/1.3.0/examples/sync-noop-signed.json').read_text()
        baseline = json.loads(noop_raw)['receipt']
        for lexeme in ('45.0', '4.5e1', '450e-1'):
            value = json.loads(noop_raw.replace('"sequence": 45,', '"sequence": ' + lexeme + ','))['receipt']
            self.assertEqual(hub_sync.canonical_fixture_payload(value), hub_sync.canonical_fixture_payload(baseline))
            self.assertEqual(hub_sync.validate_response(200, value, value['vehicle_id']), [])
        negative_zero = {'sequence': -0.0}
        self.assertEqual(hub_sync.canonical_fixture_payload(negative_zero), b'{"sequence":0}')
        # A publisher signing Python's decimal spelling must no longer pass JCS admission.
        value = dict(baseline, sequence=45.0)
        payload = json.dumps({k:v for k,v in value.items() if k != 'signature'}, sort_keys=True, separators=(',', ':')).encode()
        key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex('9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60'))
        value['signature'] = {**value['signature'], 'signed_payload_sha256': hashlib.sha256(payload).hexdigest(),
                              'signature': base64.b64encode(key.sign(payload)).decode()}
        self.assertIn('signature payload digest mismatch', hub_sync.validate_response(200, value, value['vehicle_id']))
        value = dict(baseline, sequence=9007199254740992)
        self.assertIn('no-op receipt violates schema', hub_sync.validate_response(200, value, value['vehicle_id']))
        receipt = copy.deepcopy(self.receipt)
        receipt['base']['sequence'] = float(receipt['base']['sequence'])
        receipt['target']['sequence'] = float(receipt['target']['sequence'])
        self.admit_pack_bindings(receipt)
        self.assertTrue(self.contract.verify_pack(receipt, self.pack))

    def test_raw_physical_request_and_control_byte_bounds(self):
        encode = lambda value: json.dumps(value, separators=(',', ':')).encode()
        request_raw, receipt_raw = encode(self.request), encode(self.receipt)
        hint = json.loads((PROFILE/'examples/changes-since-physical-rebase.json').read_text())['hint']
        target = (PROFILE/'examples/signed-full-manifest-43.json').read_bytes()
        self.contract.verify_receipt_body(request_raw, receipt_raw, self.receipt['vehicle_id'])
        self.assertTrue(self.contract.verify_rebase_body(request_raw, encode(hint), self.receipt['vehicle_id'], target))
        for raw, call, limit in (
            (request_raw, self.contract.validate_request_body, 8192),
            (receipt_raw, lambda raw: self.contract.verify_receipt_body(request_raw, raw, self.receipt['vehicle_id']), 2097152),
            (encode(hint), lambda raw: self.contract.verify_rebase_body(request_raw, raw, self.receipt['vehicle_id'], target), 2097152),
        ):
            self.assertTrue(call(raw + b' ' * (limit-len(raw))))
            with self.assertRaisesRegex(self.contract.ContractError, 'exceeds byte limit'):
                call(raw + b' ' * (limit-len(raw)+1))
        with self.assertRaisesRegex(self.contract.ContractError, 'exceeds byte limit'):
            self.contract._verify_replacement_manifest(target + b' ' * 2097152, self.receipt['vehicle_id'])
        oversized = copy.deepcopy(hint)
        oversized['retry_request'].update(base_manifest_id='x'*4096, base_receipt_id='y'*4096)
        with self.assertRaisesRegex(self.contract.ContractError, 'request exceeds byte limit'):
            self.contract.verify_rebase(self.request, oversized, self.receipt['vehicle_id'], target)

    def test_catalog_text_is_strict_utf8_and_preserves_unicode_and_null(self):
        catalog = json.loads((PROFILE/'physical-field-catalog.json').read_text())
        for table in catalog['tables']:
            fields = [field for field in table['columns'] if field['sqlite_type'] == 'TEXT']
            for field in fields:
                def prepare(c):
                    if not c.execute(f"SELECT 1 FROM {table['name']} LIMIT 1").fetchone():
                        values = [100 if col['name']=='id' else None if col['nullable'] else
                                  'valid' if col['sqlite_type']=='TEXT' else bytes(8) if col['sqlite_type']=='BLOB' else 0
                                  for col in table['columns']]
                        c.execute(f"INSERT INTO {table['name']} VALUES("+','.join('?' for _ in values)+')', values)
                        c.execute("INSERT INTO row_roles VALUES(?,100,'context')", (table['name'],))
                with self.subTest(table=table['name'], field=field['name']):
                    def invalid(c):
                        prepare(c)
                        c.execute(f"UPDATE {table['name']} SET {field['name']}=CAST(x'80' AS TEXT)")
                    receipt, compressed = self.edited_pack(invalid)
                    self.admit_pack_bindings(receipt)
                    with self.assertRaisesRegex(self.contract.ContractError, 'TEXT is not valid UTF-8'):
                        self.contract.verify_pack(receipt, compressed)
                    def valid(c):
                        prepare(c)
                        c.execute(f"UPDATE {table['name']} SET {field['name']}=?", ('München 東京 🚗',))
                    receipt, compressed = self.edited_pack(valid)
                    self.admit_pack_bindings(receipt)
                    self.assertTrue(self.contract.verify_pack(receipt, compressed))
                    if field['nullable']:
                        def nullable(c):
                            prepare(c)
                            c.execute(f"UPDATE {table['name']} SET {field['name']}=NULL")
                        receipt, compressed = self.edited_pack(nullable)
                        self.assertTrue(self.contract.verify_pack(receipt, compressed))

    def test_largest_jointly_admitted_full_rebase_and_exact_retry(self):
        encode = lambda value: json.dumps(value, separators=(',', ':'), sort_keys=True).encode()
        request = copy.deepcopy(self.request)
        request.update(base_manifest_id='b'*4096, base_receipt_id='r'*4096,
                       from_sequence=9007199254740991)
        request['source'].update(generation=9007199254740991, selected_car_id=9007199254740991)
        excess = len(encode(request)) - 8192
        request['base_receipt_id'] = request['base_receipt_id'][:-excess]
        self.assertEqual(len(encode(request)), 8192)
        hint = copy.deepcopy(json.loads((PROFILE/'examples/changes-since-physical-rebase.json').read_text())['hint'])
        hint['requested_base'].update(manifest_id=request['base_manifest_id'], receipt_id=request['base_receipt_id'],
                                      sequence=request['from_sequence'])
        hint['replacement'].update(manifest_id='m'*4096, receipt_id='t'*len(request['base_receipt_id']),
                                  sequence=9007199254740991)
        hint['replacement']['chunks'] = [
            {'chunk_index':i, 'pack':{'object_name':f'{i:04d}'+'p'*1020,
                                     'sha256':'f'*64, 'compressed_bytes':16777216}}
            for i in range(1771)]
        hint['retry_request'] = {**request, 'base_manifest_id':hint['replacement']['manifest_id'],
                                 'base_receipt_id':hint['replacement']['receipt_id']}
        excess = len(encode(hint)) - 2097152
        self.assertGreater(excess, 0, 'individual maxima must exercise the joint bound')
        for chunk in reversed(hint['replacement']['chunks']):
            reduction = min(excess, len(chunk['pack']['object_name'])-1)
            if reduction:
                chunk['pack']['object_name'] = chunk['pack']['object_name'][:-reduction]
                excess -= reduction
            if not excess:
                break
        self.assertEqual(excess, 0)
        child = self.fixture_signed({'manifest_id':hint['replacement']['manifest_id'],
                                     'receipt_id':hint['replacement']['receipt_id'], 'vehicle_id':hint['vehicle_id'],
                                     'kind':'snapshot', 'schema_version':'2.2', 'sequence':hint['replacement']['sequence'],
                                     'chunks':hint['replacement']['chunks']})
        child_raw = encode(child)
        digest = hashlib.sha256(child_raw).hexdigest()
        hint['replacement']['manifest_sha256'] = digest
        hint['retry_request']['base_manifest_sha256'] = digest
        hint = self.fixture_signed(hint)
        hint_raw = encode(hint)
        self.assertEqual(len(hint['replacement']['chunks']), 1771)
        self.assertEqual(len(hint_raw), 2097152)
        self.assertEqual(len(encode(hint['retry_request'])), 8192)
        self.assertLessEqual(len(child_raw), 2097152)
        self.assertTrue(self.contract.verify_rebase_body(encode(request), hint_raw, hint['vehicle_id'], child_raw))
        self.assertTrue(self.contract.validate_request_body(encode(hint['retry_request'])))
        with self.assertRaisesRegex(self.contract.ContractError, 'rebase exceeds byte limit'):
            self.contract.verify_rebase_body(encode(request), hint_raw+b' ', hint['vehicle_id'], child_raw)

    def test_pack_rejects_every_extra_schema_object_kind(self):
        for statement in (
            "CREATE VIEW extra_view AS SELECT 'extra' AS raw",
            "CREATE TRIGGER extra_trigger AFTER INSERT ON drives BEGIN SELECT 'extra'; END",
            "CREATE INDEX extra_index ON drives(car_id)",
            "CREATE TABLE extra_table (raw TEXT)",
        ):
            with self.subTest(statement=statement):
                def add_object(c):
                    c.execute(statement)
                receipt, compressed = self.edited_pack(add_object)
                self.admit_pack_bindings(receipt)
                with self.assertRaisesRegex(self.contract.ContractError, 'physical pack schema mismatch'):
                    self.contract.verify_pack(receipt, compressed)


if __name__ == "__main__":
    unittest.main()

import ast
import base64
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from jsonschema import Draft202012Validator, FormatChecker
from openapi_spec_validator import validate as validate_openapi
import zstandard

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.5.0"
PREVIOUS = ROOT / "profiles/hub-sync-v1/1.4.0"
MAX_INTEGER = 9007199254740991
SCHEMA_FILES = ("changes-since-request.schema.json", "physical-changed-set-receipt.schema.json",
                "physical-rebase-hint.schema.json", "live-overlay.schema.json")
SPEC_CASES = ("live-advance", "live-no-op-when-current", "live-gap-rejected", "live-wrong-head-rejected",
              "live-rebase-after-import", "live-rebase-after-retention", "live-request-1-4-compatible",
              "live-bootstrap-then-overlay")
EXTRA_CASES = ("live-rebase-after-head-expiry", "live-rebase-when-client-ahead", "live-step-reissue-rejected",
               "live-checkpoint-upgrade-from-1-4", "live-checkpoint-upgrade-other-head-rejected",
               "live-checkpoint-upgrade-from-1-4-changed-set", "live-checkpoint-upgrade-changed-set-other-head-rejected")
EXAMPLES = (
    "changes-since-live-advance", "changes-since-live-gap", "changes-since-live-no-op", "changes-since-live-rebase-ahead",
    "changes-since-live-rebase-head-changed", "changes-since-live-rebase-head-expired", "changes-since-live-rebase-retention",
    "changes-since-live-reissued", "changes-since-live-wrong-head", "changes-since-request-live",
    "changes-since-request-live-ahead", "changes-since-request-live-current", "changes-since-request-live-expired-head",
    "changes-since-request-live-retained", "changes-since-request-live-stale-head", "signed-full-manifest-live",
    "signed-full-manifest-live-next", "signed-full-manifest-live-previous")
RESPONSE_REFS = {
    "200": ["physical-changed-set-receipt.schema.json#/$defs/receipt", "live-overlay.schema.json#/$defs/no_op",
            "../1.4.0/physical-changed-set-receipt.schema.json#/$defs/receipt", "../1.3.0/noop.schema.json#/$defs/receipt"],
    "409": ["physical-rebase-hint.schema.json#/$defs/hint", "../1.4.0/physical-rebase-hint.schema.json#/$defs/hint"]}
REQUEST_REFS = ["changes-since-request.schema.json#/$defs/request", "../1.4.0/changes-since-request.schema.json#/$defs/request"]
MANIFEST_REF = "live-overlay.schema.json#/$defs/manifest"
REASONS = ["head_changed", "live_retention", "live_ahead"]
INHERITED_KEYS = sorted([
    *(f"../1.4.0/{name}" for name in (
        "changes-since-request.schema.json", "examples/changes-since-physical-changed-set.json",
        "examples/changes-since-request-physical.json", "examples/signed-full-manifest-42.json",
        "examples/signed-full-manifest-43.json", "fixture-signing-keys.json", "packs/physical-changed-set-v1.sqlite.zst",
        "physical-changed-set-receipt.schema.json", "physical-delta-pack-v1-contract.json", "physical-delta-pack-v1.sql",
        "physical-field-catalog.json", "physical-rebase-hint.schema.json", "physical-value-semantics.json",
        "signing-keys.schema.json")),
    *(f"../1.3.0/{name}" for name in ("noop.schema.json", "signing-keys.schema.json", "sync-error.schema.json",
                                      "sync-manifest.schema.json"))])
# Every case in full: id, validator, every fixture binding, status and expected errors. The published cases are the
# contract a second implementation runs, so a dropped binding (a manifest_fixture, a second fixture) is a change to see.
EXPECTED_CASES = [
    {"id": "live-advance", "validator": "live_receipt", "request_fixture": "changes-since-request-live-v1",
     "response_fixture": "changes-since-live-advance-v1", "manifest_fixture": "signed-full-manifest-live",
     "status": 200, "expected_errors": []},
    {"id": "live-no-op-when-current", "validator": "live_no_op", "request_fixture": "changes-since-request-live-current-v1",
     "response_fixture": "changes-since-live-no-op-v1", "status": 200, "expected_errors": []},
    {"id": "live-gap-rejected", "validator": "live_receipt", "request_fixture": "changes-since-request-live-v1",
     "response_fixture": "changes-since-live-gap-v1", "status": 200, "expected_errors": ["live sequence gap"]},
    {"id": "live-wrong-head-rejected", "validator": "live_receipt", "request_fixture": "changes-since-request-live-v1",
     "response_fixture": "changes-since-live-wrong-head-v1", "status": 200, "expected_errors": ["base checkpoint mismatch"]},
    {"id": "live-rebase-after-import", "validator": "live_rebase", "request_fixture": "changes-since-request-live-stale-head-v1",
     "response_fixture": "changes-since-live-rebase-head-changed-v1", "manifest_fixture": "signed-full-manifest-live",
     "status": 409, "expected_errors": []},
    {"id": "live-rebase-after-retention", "validator": "live_rebase", "request_fixture": "changes-since-request-live-retained-v1",
     "response_fixture": "changes-since-live-rebase-retention-v1", "manifest_fixture": "signed-full-manifest-live-next",
     "status": 409, "expected_errors": []},
    {"id": "live-request-1-4-compatible", "validator": "legacy_exchange",
     "request_file": "../1.4.0/examples/changes-since-request-physical.json",
     "response_file": "../1.4.0/examples/changes-since-physical-changed-set.json", "status": 200, "expected_errors": []},
    {"id": "live-bootstrap-then-overlay", "validator": "bootstrap_then_overlay", "manifest_fixture": "signed-full-manifest-live",
     "request_fixture": "changes-since-request-live-v1", "response_fixture": "changes-since-live-advance-v1",
     "status": 200, "expected_errors": []},
    {"id": "live-rebase-after-head-expiry", "validator": "live_rebase",
     "request_fixture": "changes-since-request-live-expired-head-v1",
     "response_fixture": "changes-since-live-rebase-head-expired-v1", "manifest_fixture": "signed-full-manifest-live-next",
     "status": 409, "expected_errors": []},
    {"id": "live-rebase-when-client-ahead", "validator": "live_rebase",
     "request_fixture": "changes-since-request-live-ahead-v1",
     "response_fixture": "changes-since-live-rebase-ahead-v1", "manifest_fixture": "signed-full-manifest-live-next",
     "status": 409, "expected_errors": []},
    {"id": "live-step-reissue-rejected", "validator": "live_step_unique", "request_fixture": "changes-since-request-live-v1",
     "response_fixture": "changes-since-live-advance-v1", "second_fixture": "changes-since-live-reissued-v1", "status": 200,
     "expected_errors": ["live step reissued with different content"]},
    {"id": "live-checkpoint-upgrade-from-1-4", "validator": "checkpoint_upgrade",
     "checkpoint_file": "../1.4.0/examples/signed-full-manifest-43.json", "manifest_fixture": "signed-full-manifest-live",
     "status": 200, "expected_errors": []},
    {"id": "live-checkpoint-upgrade-other-head-rejected", "validator": "checkpoint_upgrade",
     "checkpoint_file": "../1.4.0/examples/signed-full-manifest-42.json", "manifest_fixture": "signed-full-manifest-live",
     "status": 200, "expected_errors": ["checkpoint upgrade does not match the bootstrap manifest"]},
    {"id": "live-checkpoint-upgrade-from-1-4-changed-set", "validator": "checkpoint_upgrade",
     "checkpoint_file": "../1.4.0/examples/changes-since-physical-changed-set.json", "checkpoint_member": "target",
     "manifest_fixture": "signed-full-manifest-live", "status": 200, "expected_errors": []},
    {"id": "live-checkpoint-upgrade-changed-set-other-head-rejected", "validator": "checkpoint_upgrade",
     "checkpoint_file": "../1.4.0/examples/changes-since-physical-changed-set.json", "checkpoint_member": "base",
     "manifest_fixture": "signed-full-manifest-live", "status": 200,
     "expected_errors": ["checkpoint upgrade does not match the bootstrap manifest"]},
]
# The live-journal retention obligation, as the profile and the reference say it (squashed whitespace for the reference).
RETENTION_KEY = "every_live_step_of_the_current_head_from_live_sequence_1_or_fold_into_a_new_head_before_pruning"
PROFILE_RETENTION_SENTENCE = ("A Hub MUST retain every live changed set of its current head from live_sequence 1, or fold the "
                              "live rows into a new head before pruning them, so that a retention rebase cannot loop on an "
                              "unchanged head.")
PROFILE_WATERMARK_SENTENCE = ("A watermark over paired devices is not enough: a new, reinstalled or rebased device and a 1.4 "
                              "upgrade start at live_sequence 0 and need step 1, which no paired device's acknowledgement protects.")
DOC_RETENTION_SENTENCE = ("A Hub therefore keeps every live changed set of its current head from `live_sequence` 1, or folds the "
                          "live rows into a new head before pruning them.")
DOC_WATERMARK_SENTENCE = ("A watermark over paired devices is not enough: a new, reinstalled or rebased device and a 1.4 upgrade "
                          "start at `live_sequence` 0 and need step 1, which no paired device's acknowledgement protects.")
# The capability qualification: the token is advertised only under a discovery profile that defines it.
PROFILE_CAPABILITY_SENTENCES = (
    "if and only if it serves this profile's live overlay (a hub-sync-v1@1.5.0 bootstrap and a changes-since request carrying "
    "base_live_sequence) and the discovery profile it names defines the token.",
    "A Hub that does not serve the overlay MUST NOT advertise it and answers the 1.5 selector with an empty 406 no-store.",
    "hub-http-v1@1.2.0 and hub-http-v1@1.1.0 do not define sync.live, and a Hub that names a discovery profile advertises only "
    "the tokens that profile lists, so a Hub that serves this profile under either of them does not advertise sync.live.",
    "A later hub-http-v1 minor adds the token; the exact capability equality check of the hub-http-v1@1.1.0 actual-Hub matrix "
    "is adjusted then.")
DOC_CAPABILITY_SENTENCE = ("`hub-http-v1@1.2.0` and `1.1.0` do not define `sync.live`, so a Hub that serves 1.5 under either of "
                           "them does not advertise it; a later `hub-http-v1` minor adds the token, and the exact capability "
                           "equality of the 1.1.0 actual-Hub matrix is adjusted then.")
UNSIGNED_422 = {"status": 422, "error": "unknown_base_receipt", "signed": False}
TRANSIENT = {"transient_failure": True, "defined_by_this_profile": False}
RESOLUTION_ORDER = ["base_identity_not_recorded_or_differs", "base_head_is_not_the_head_to_serve",
                    "head_to_serve_live_service_paused_for_rotation", "base_live_sequence_above_head_high_water_mark",
                    "base_live_sequence_above_head_position_and_not_above_high_water_mark",
                    "base_live_sequence_equals_head_position", "base_live_sequence_behind_head_position"]


def encode(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode()


class LiveOverlayProfileTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("hub_sync_15", ROOT / "conformance/hub_sync_15.py")
        cls.contract = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.contract)
        cls.request = cls.example("changes-since-request-live", "request")
        cls.current = cls.example("changes-since-request-live-current", "request")
        cls.stale = cls.example("changes-since-request-live-stale-head", "request")
        cls.retained = cls.example("changes-since-request-live-retained", "request")
        cls.receipt = cls.example("changes-since-live-advance", "receipt")
        cls.no_op = cls.example("changes-since-live-no-op", "no_op")
        cls.head_changed = cls.example("changes-since-live-rebase-head-changed", "hint")
        cls.retention = cls.example("changes-since-live-rebase-retention", "hint")
        cls.expired = cls.example("changes-since-request-live-expired-head", "request")
        cls.expired_hint = cls.example("changes-since-live-rebase-head-expired", "hint")
        cls.ahead = cls.example("changes-since-request-live-ahead", "request")
        cls.ahead_hint = cls.example("changes-since-live-rebase-ahead", "hint")
        cls.vehicle = cls.receipt["vehicle_id"]
        cls.head_raw = (PROFILE / "examples/signed-full-manifest-live.json").read_bytes()
        cls.next_raw = (PROFILE / "examples/signed-full-manifest-live-next.json").read_bytes()

    @staticmethod
    def example(name, member):
        return json.loads((PROFILE / f"examples/{name}.json").read_text())[member]

    def fixture_signed(self, value):
        value = copy.deepcopy(value)
        # Published RFC 8032 fixture seed, never a live signing credential.
        key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex(
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"))
        payload = self.contract.inherited._canonical_unsigned(value)
        value["signature"] = {
            "algorithm": "ed25519", "key_id": self.receipt["signature"]["key_id"],
            "signed_payload_sha256": hashlib.sha256(payload).hexdigest(),
            "signature": base64.b64encode(key.sign(payload)).decode("ascii")}
        return value

    def edited(self, source, edit, resign=True):
        value = copy.deepcopy(source)
        edit(value)
        return self.fixture_signed(value) if resign else value

    def reseal(self, root):
        names = sorted(path.relative_to(root).as_posix() for path in Path(root).rglob("*")
                       if path.is_file() and path.name != "SHA256SUMS")
        (Path(root) / "SHA256SUMS").write_text("".join(
            hashlib.sha256((Path(root) / name).read_bytes()).hexdigest() + "  " + name + "\n" for name in names))

    def copied_profile(self, directory):
        root = Path(directory) / "1.5.0"
        shutil.copytree(PROFILE, root)
        # The profile refers to ../1.4.0/ and ../1.3.0/ by relative path; the siblings resolve to the committed profiles.
        for name in ("1.4.0", "1.3.0"):
            (Path(directory) / name).symlink_to(ROOT / "profiles/hub-sync-v1" / name, target_is_directory=True)
        return root

    def matching(self, ref, value):
        """True when value validates against the schema a profile-relative $ref names."""
        path, _, pointer = ref.partition("#/$defs/")
        schema = json.loads((PROFILE / path).read_text())
        validator = Draft202012Validator({**schema, "$ref": "#/$defs/" + pointer}, format_checker=FormatChecker())
        return not list(validator.iter_errors(value))

    def run_generator(self, name):
        return subprocess.run([sys.executable, "-B", str(ROOT / "tools" / name), "--check"], capture_output=True)

    # Generator, bundle, chain

    def test_generator_check_is_byte_identical_and_prints_the_profile_digest(self):
        check = self.run_generator("build_hub_sync_15_profile.py")
        self.assertEqual(check.returncode, 0, check.stderr.decode())
        digest = hashlib.sha256((PROFILE / "SHA256SUMS").read_bytes()).hexdigest()
        self.assertEqual(check.stdout.decode(), f"hub-sync-v1@1.5.0 sha256={digest}\n")

    def test_retained_generators_and_profiles_stay_green(self):
        # Reported as they stand: an SQLite-version drift (audit P20) would fail these, not the 1.5 generator.
        for name in ("build_hub_sync_profile.py", "build_hub_sync_14_profile.py"):
            with self.subTest(generator=name):
                check = self.run_generator(name)
                self.assertEqual(check.returncode, 0, check.stderr.decode())
        self.assertEqual(self.contract.inherited.verify_bundle()["profile_id"], "hub-sync-v1@1.4.0")

    def test_generator_builds_no_sqlite_pack(self):
        tree = ast.parse((ROOT / "tools/build_hub_sync_15_profile.py").read_text())
        imported = {alias.name.split(".")[0] for node in ast.walk(tree) if isinstance(node, ast.Import) for alias in node.names}
        imported |= {node.module.split(".")[0] for node in ast.walk(tree) if isinstance(node, ast.ImportFrom)}
        self.assertFalse(imported & {"sqlite3", "zstandard", "tempfile", "build_hub_sync_14_profile"})

    def test_bundle_closure_chain_candidate_status_and_inherited_pins(self):
        profile = self.contract.verify_bundle()
        self.assertEqual(profile["profile_id"], "hub-sync-v1@1.5.0")
        self.assertEqual(profile["status"], "candidate")
        self.assertEqual(profile["previous_profile"], "hub-sync-v1@1.4.0")
        self.assertEqual(profile["previous_profile_sha256"], hashlib.sha256((PREVIOUS / "SHA256SUMS").read_bytes()).hexdigest())
        self.assertEqual(sorted(path.name for path in PROFILE.iterdir()), sorted((
            "SHA256SUMS", "cases.json", "examples", "openapi.json", "profile.json", *SCHEMA_FILES)))
        # The pin set is exact: dropping any one of the 18 references is a change the tests must see.
        self.assertEqual(sorted(profile["inherited_artifacts"]), INHERITED_KEYS)
        for reference, digest in profile["inherited_artifacts"].items():
            self.assertTrue(reference.startswith(("../1.4.0/", "../1.3.0/")), reference)
            self.assertEqual(hashlib.sha256((PROFILE / reference).read_bytes()).hexdigest(), digest)
        for key in ("field_catalog", "value_semantics", "pack_contract", "pack_sql", "fixture_signing_keys", "signing_keys_schema"):
            self.assertTrue(profile[key].startswith("../1.4.0/"), key)
            self.assertIn(profile[key], profile["inherited_artifacts"])
        previous = json.loads((PREVIOUS / "profile.json").read_text())
        for key in ("max_request_body_bytes", "max_control_body_bytes", "max_delta_chunks", "max_pack_compressed_bytes",
                    "max_delta_compressed_bytes", "max_delta_uncompressed_bytes", "max_delta_rows", "max_impacted_roots"):
            self.assertEqual(profile[key], previous[key], key)
        self.assertEqual(profile["changed_set_format"], previous["changed_set_format"])
        self.assertEqual(profile["bootstrap_selector"], {**previous["bootstrap_selector"], "value": "hub-sync-v1@1.5.0"})

    def test_chain_closure_and_pin_mutants_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied_profile(directory)
            profile = json.loads((root / "profile.json").read_text())
            profile["previous_profile_sha256"] = "0" * 64
            (root / "profile.json").write_text(json.dumps(profile, indent=2, sort_keys=True) + "\n")
            self.reseal(root)
            with self.assertRaisesRegex(self.contract.ContractError, "previous profile digest mismatch"):
                self.contract.verify_bundle(root)
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied_profile(directory)
            profile = json.loads((root / "profile.json").read_text())
            pinned = "../1.4.0/physical-field-catalog.json"
            profile["inherited_artifacts"][pinned] = "0" * 64
            (root / "profile.json").write_text(json.dumps(profile, indent=2, sort_keys=True) + "\n")
            self.reseal(root)
            with self.assertRaisesRegex(self.contract.ContractError, "inherited artefact digest mismatch"):
                self.contract.verify_bundle(root)
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied_profile(directory)
            profile = json.loads((root / "profile.json").read_text())
            profile["profile_id"] = "hub-sync-v1@1.4.0"
            (root / "profile.json").write_text(json.dumps(profile, indent=2, sort_keys=True) + "\n")
            self.reseal(root)
            with self.assertRaisesRegex(self.contract.ContractError, "wrong profile"):
                self.contract.verify_bundle(root)
        with tempfile.TemporaryDirectory() as directory:
            # A reference that is not relative (an extra leading directory) must not be read as '../1.4.0/...'.
            root = self.copied_profile(directory)
            profile = json.loads((root / "profile.json").read_text())
            digest = profile["inherited_artifacts"]["../1.4.0/physical-field-catalog.json"]
            profile["inherited_artifacts"]["xx/1.4.0/physical-field-catalog.json"] = digest
            (root / "profile.json").write_text(json.dumps(profile, indent=2, sort_keys=True) + "\n")
            self.reseal(root)
            with self.assertRaisesRegex(self.contract.ContractError, "inherited artefact digest mismatch"):
                self.contract.verify_bundle(root)
        with tempfile.TemporaryDirectory() as directory:
            # The owning profile's own SHA256SUMS line must agree with the pin, not only the file bytes.
            root = Path(directory) / "1.5.0"
            shutil.copytree(PROFILE, root)
            (Path(directory) / "1.4.0").symlink_to(PREVIOUS, target_is_directory=True)
            shutil.copytree(ROOT / "profiles/hub-sync-v1/1.3.0", Path(directory) / "1.3.0")
            self.assertEqual(self.contract.verify_bundle(root)["profile_id"], "hub-sync-v1@1.5.0")
            sums = Path(directory) / "1.3.0/SHA256SUMS"
            lines = sums.read_text().splitlines()
            index = next(i for i, line in enumerate(lines) if line.endswith("  sync-error.schema.json"))
            lines[index] = "0" * 64 + "  sync-error.schema.json"
            sums.write_text("\n".join(lines) + "\n")
            with self.assertRaisesRegex(self.contract.ContractError, "inherited artefact digest mismatch"):
                self.contract.verify_bundle(root)
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied_profile(directory)
            (root / "examples/changes-since-live-advance.json").write_text("{}\n")
            with self.assertRaisesRegex(self.contract.ContractError, "profile file digest mismatch"):
                self.contract.verify_bundle(root)
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied_profile(directory)
            (root / "stray.json").write_text("{}\n")
            with self.assertRaisesRegex(self.contract.ContractError, "profile manifest is incomplete"):
                self.contract.verify_bundle(root)

    def test_inherited_pins_are_checked_against_the_owning_profile_and_its_bytes(self):
        # An inherited file whose bytes drift while its owner's SHA256SUMS is left alone must still be refused.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "1.5.0"
            shutil.copytree(PROFILE, root)
            shutil.copytree(PREVIOUS, Path(directory) / "1.4.0")
            (Path(directory) / "1.3.0").symlink_to(ROOT / "profiles/hub-sync-v1/1.3.0", target_is_directory=True)
            self.assertEqual(self.contract.verify_bundle(root)["profile_id"], "hub-sync-v1@1.5.0")
            catalog = Path(directory) / "1.4.0/physical-field-catalog.json"
            catalog.write_bytes(catalog.read_bytes() + b" ")
            with self.assertRaisesRegex(self.contract.ContractError, "inherited artefact digest mismatch"):
                self.contract.verify_bundle(root)
        for label, reference in (("another profile directory", "../1.0.0/profile.json"),
                                 ("a path that climbs out", "../1.4.0/../1.3.0/profile.json"),
                                 ("not relative", "1.4.0/profile.json")):
            with self.subTest(case=label), tempfile.TemporaryDirectory() as directory:
                root = self.copied_profile(directory)
                profile = json.loads((root / "profile.json").read_text())
                profile["inherited_artifacts"][reference] = "0" * 64
                (root / "profile.json").write_text(json.dumps(profile, indent=2, sort_keys=True) + "\n")
                self.reseal(root)
                with self.assertRaisesRegex(self.contract.ContractError, "inherited artefact digest mismatch"):
                    self.contract.verify_bundle(root)

    def test_every_relative_reference_of_the_profile_is_pinned_by_digest(self):
        pinned = set(json.loads((PROFILE / "profile.json").read_text())["inherited_artifacts"])
        found = set()

        def walk(value, skip=()):
            if isinstance(value, dict):
                for key, item in value.items():
                    if key not in skip:
                        walk(item)
            elif isinstance(value, list):
                for item in value:
                    walk(item)
            elif isinstance(value, str) and value.startswith("../"):
                found.add(value.partition("#")[0])
        walk(json.loads((PROFILE / "profile.json").read_text()), skip=("inherited_artifacts",))
        walk(json.loads((PROFILE / "openapi.json").read_text()))
        walk(json.loads((PROFILE / "cases.json").read_text()))
        self.assertTrue(found)
        self.assertEqual(sorted(found - pinned), [])
        # Every such reference resolves to a file whose bytes are the pinned bytes.
        for reference in sorted(found):
            self.assertEqual(hashlib.sha256((PROFILE / reference).read_bytes()).hexdigest(),
                             json.loads((PROFILE / "profile.json").read_text())["inherited_artifacts"][reference], reference)

    def test_a_sums_file_with_a_duplicate_or_climbing_name_is_refused(self):
        for label, extra in (("climbing name", "0" * 64 + "  ../escape.json\n"), ("bad digest", "xyz  other.json\n")):
            with self.subTest(case=label), tempfile.TemporaryDirectory() as directory:
                root = self.copied_profile(directory)
                sums = (root / "SHA256SUMS").read_text()
                (root / "SHA256SUMS").write_text(sums + extra)
                with self.assertRaisesRegex(self.contract.ContractError, "invalid profile manifest"):
                    self.contract.verify_bundle(root)
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied_profile(directory)
            sums = (root / "SHA256SUMS").read_text()
            (root / "SHA256SUMS").write_text(sums + sums.splitlines()[0] + "\n")
            with self.assertRaisesRegex(self.contract.ContractError, "invalid profile manifest"):
                self.contract.verify_bundle(root)

    # Schemas, OpenAPI, examples, signatures

    def test_machine_schemas_and_openapi_are_valid(self):
        for name in SCHEMA_FILES:
            Draft202012Validator.check_schema(json.loads((PROFILE / name).read_text()))
        spec = json.loads((PROFILE / "openapi.json").read_text())
        validate_openapi(spec, base_uri=PROFILE.as_uri() + "/")
        self.assertEqual(spec["info"]["version"], "1.5.0")
        route = spec["paths"]["/v1/vehicles/{vehicle_id}/sync/changes-since"]["post"]
        self.assertEqual(set(route["responses"]), {"200", "409"})
        self.assertEqual(route["requestBody"]["x-max-body-bytes"], 8192)
        self.assertEqual([item["$ref"] for item in route["requestBody"]["content"]["application/json"]["schema"]["oneOf"]],
                         REQUEST_REFS)
        for status, refs in RESPONSE_REFS.items():
            with self.subTest(status=status):
                self.assertEqual([item["$ref"] for item in route["responses"][status]["content"]["application/json"]["schema"]["oneOf"]],
                                 refs)
        manifest = spec["paths"]["/v1/vehicles/{vehicle_id}/sync/manifest"]["get"]
        self.assertEqual(set(manifest["responses"]), {"200", "406"})
        self.assertEqual(manifest["responses"]["200"]["content"]["application/json"]["schema"], {"$ref": MANIFEST_REF})
        selector = {item["name"]: item["schema"] for item in manifest["parameters"]}
        self.assertEqual(selector["x-teslatlas-sync-profile"], {"const": "hub-sync-v1@1.5.0"})
        self.assertEqual(selector["x-teslatlas-supported-schemas"], {"const": "2.1,2.2"})

    def test_openapi_alternatives_are_mutually_exclusive(self):
        legacy = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-request-physical.json")["request"]
        legacy_receipt = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-physical-changed-set.json")["receipt"]
        noop_13 = self.contract.inherited.read_json(ROOT / "profiles/hub-sync-v1/1.3.0/examples/sync-noop-signed.json")["receipt"]
        violations = self.contract._violations
        self.assertEqual(self.contract.classify_request(self.request), "live")
        self.assertEqual(self.contract.classify_request(legacy), "legacy")
        self.assertTrue(violations("changes-since-request.schema.json", "request", legacy))
        self.assertTrue(violations("changes-since-request.schema.json", "request", self.request, PREVIOUS))
        self.assertTrue(violations("physical-changed-set-receipt.schema.json", "receipt", legacy_receipt))
        self.assertTrue(violations("physical-changed-set-receipt.schema.json", "receipt", self.receipt, PREVIOUS))
        self.assertTrue(violations("live-overlay.schema.json", "no_op", noop_13))
        self.assertTrue(violations("physical-rebase-hint.schema.json", "hint", self.head_changed, PREVIOUS))
        self.assertTrue(violations("physical-rebase-hint.schema.json", "hint",
                                   self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-physical-rebase.json")["hint"]))

    def test_every_example_matches_exactly_the_openapi_alternative_for_its_status(self):
        legacy = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-request-physical.json")["request"]
        legacy_receipt = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-physical-changed-set.json")["receipt"]
        legacy_hint = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-physical-rebase.json")["hint"]
        noop_13 = self.contract.inherited.read_json(ROOT / "profiles/hub-sync-v1/1.3.0/examples/sync-noop-signed.json")["receipt"]
        manifest_14 = json.loads((PREVIOUS / "examples/signed-full-manifest-43.json").read_text())

        def alternatives(refs, value):
            return [index for index, ref in enumerate(refs) if self.matching(ref, value)]
        # Requests: a 1.5 request is the first alternative only, a 1.4 request the second only.
        for name in ("changes-since-request-live", "changes-since-request-live-current", "changes-since-request-live-stale-head",
                     "changes-since-request-live-retained", "changes-since-request-live-expired-head",
                     "changes-since-request-live-ahead"):
            with self.subTest(request=name):
                self.assertEqual(alternatives(REQUEST_REFS, self.example(name, "request")), [0])
        self.assertEqual(alternatives(REQUEST_REFS, legacy), [1])
        # 200: live changed sets, the live no-op, the 1.4 changed set and the 1.3 no-op each have exactly one home.
        for name in ("changes-since-live-advance", "changes-since-live-gap", "changes-since-live-reissued",
                     "changes-since-live-wrong-head"):
            with self.subTest(receipt=name):
                self.assertEqual(alternatives(RESPONSE_REFS["200"], self.example(name, "receipt")), [0])
        self.assertEqual(alternatives(RESPONSE_REFS["200"], self.no_op), [1])
        self.assertEqual(alternatives(RESPONSE_REFS["200"], legacy_receipt), [2])
        self.assertEqual(alternatives(RESPONSE_REFS["200"], noop_13), [3])
        # 409: each 1.5 hint is the first alternative only, the 1.4 hint the second only.
        for name in ("changes-since-live-rebase-head-changed", "changes-since-live-rebase-retention",
                     "changes-since-live-rebase-head-expired", "changes-since-live-rebase-ahead"):
            with self.subTest(hint=name):
                self.assertEqual(alternatives(RESPONSE_REFS["409"], self.example(name, "hint")), [0])
        self.assertEqual(alternatives(RESPONSE_REFS["409"], legacy_hint), [1])
        # Manifest 200: the 1.5 manifests match, the 1.4 manifest of the same head does not.
        for name in ("signed-full-manifest-live", "signed-full-manifest-live-next", "signed-full-manifest-live-previous"):
            with self.subTest(manifest=name):
                self.assertTrue(self.matching(MANIFEST_REF, json.loads((PROFILE / f"examples/{name}.json").read_text())))
        self.assertFalse(self.matching(MANIFEST_REF, manifest_14))

    def test_every_example_validates_and_every_signature_verifies(self):
        seen = set()
        for path in sorted((PROFILE / "examples").glob("*.json")):
            document = json.loads(path.read_text())
            if "fixture_id" in document:
                self.assertNotIn(document["fixture_id"], seen)
                seen.add(document["fixture_id"])
                self.assertEqual(document["fixture_id"], path.stem + "-v1")
                (member,) = [key for key in document if key != "fixture_id"]
            else:
                member = None
            with self.subTest(example=path.name):
                if member == "request":
                    self.contract.validate_request(document["request"])
                elif member == "receipt":
                    self.assertFalse(self.contract._violations("physical-changed-set-receipt.schema.json", "receipt", document["receipt"]))
                    self.contract._signature(document["receipt"], self.vehicle, "signature")
                elif member == "no_op":
                    self.assertFalse(self.contract._violations("live-overlay.schema.json", "no_op", document["no_op"]))
                    self.contract._signature(document["no_op"], self.vehicle, "signature")
                elif member == "hint":
                    self.assertFalse(self.contract._violations("physical-rebase-hint.schema.json", "hint", document["hint"]))
                    self.contract._signature(document["hint"], self.vehicle, "signature")
                else:
                    self.assertIsNone(member)
                    self.contract.verify_manifest(path.read_bytes(), self.vehicle)
        self.assertEqual(len(seen), 15)
        self.assertEqual(sorted(path.stem for path in (PROFILE / "examples").glob("*.json")), sorted(EXAMPLES))

    def test_the_previous_head_manifest_is_the_head_the_stale_request_and_hint_name(self):
        digest = hashlib.sha256((PROFILE / "examples/signed-full-manifest-live-previous.json").read_bytes()).hexdigest()
        self.assertEqual(self.stale["base_manifest_sha256"], digest)
        self.assertEqual(self.head_changed["requested_base"]["manifest_sha256"], digest)
        self.assertTrue(self.contract.verify_head_manifest(
            (PROFILE / "examples/signed-full-manifest-live-previous.json").read_bytes(),
            self.head_changed["requested_base"], self.vehicle))
        # Head 40 stands for a superseded head whose packs have expired and has no published manifest.
        self.assertEqual(self.expired["from_sequence"], 40)

    def test_signed_examples_fail_closed_when_tampered_or_signed_by_another_key(self):
        tampered = copy.deepcopy(self.receipt)
        tampered["total_rows"] += 1
        with self.assertRaisesRegex(self.contract.ContractError, "signature payload mismatch"):
            self.contract.verify_live_receipt(self.request, tampered, self.vehicle)
        bad = copy.deepcopy(self.receipt)
        bad["signature"]["signature"] = base64.b64encode(bytes(64)).decode("ascii")
        with self.assertRaisesRegex(self.contract.ContractError, "signature invalid"):
            self.contract.verify_live_receipt(self.request, bad, self.vehicle)
        unknown = copy.deepcopy(self.receipt)
        unknown["signature"]["key_id"] = "ed25519-sha256-" + "0" * 64
        with self.assertRaisesRegex(self.contract.ContractError, "unknown signing key"):
            self.contract.verify_live_receipt(self.request, unknown, self.vehicle)
        noop = copy.deepcopy(self.no_op)
        noop["signature"]["signed_payload_sha256"] = "0" * 64
        with self.assertRaisesRegex(self.contract.ContractError, "no-op signature payload mismatch"):
            self.contract.verify_no_op(self.current, noop, self.vehicle)
        # A hint is verified through verify_rebase itself, after every structural check has passed.
        hint = copy.deepcopy(self.head_changed)
        hint["signature"]["signature"] = base64.b64encode(bytes(64)).decode("ascii")
        with self.assertRaisesRegex(self.contract.ContractError, "rebase signature invalid"):
            self.contract.verify_rebase(self.stale, hint, self.vehicle, self.head_raw)
        changed = self.edited(self.head_changed, lambda value: value.update(reason="live_retention"), resign=False)
        with self.assertRaisesRegex(self.contract.ContractError, "rebase signature payload mismatch"):
            self.contract.verify_rebase(self.stale, changed, self.vehicle, self.head_raw)
        foreign = copy.deepcopy(self.head_changed)
        foreign["signature"]["key_id"] = "ed25519-sha256-" + "0" * 64
        with self.assertRaisesRegex(self.contract.ContractError, "unknown signing key"):
            self.contract.verify_rebase(self.stale, foreign, self.vehicle, self.head_raw)

    def test_cases_are_complete_and_executable(self):
        cases = json.loads((PROFILE / "cases.json").read_text())["cases"]
        self.assertEqual([case["id"] for case in cases], [*SPEC_CASES, *EXTRA_CASES])
        # The whole tuple of each case, not only its id and outcome: fixture bindings, files, members and status.
        self.assertEqual(cases, EXPECTED_CASES)
        self.assertEqual(len(EXPECTED_CASES), 15)
        by_id = {case["id"]: case for case in cases}
        self.assertEqual(by_id["live-advance"]["manifest_fixture"], "signed-full-manifest-live")
        # Every case that names a fixture names one the profile ships, so a binding cannot point at nothing.
        shipped = {json.loads(path.read_text()).get("fixture_id", path.stem) for path in (PROFILE / "examples").glob("*.json")}
        for case in cases:
            for key in ("request_fixture", "response_fixture", "second_fixture"):
                if key in case:
                    self.assertIn(case[key], shipped, (case["id"], key))
            if "manifest_fixture" in case:
                self.assertIn(case["manifest_fixture"], shipped, (case["id"], "manifest_fixture"))
        results = {item["case_id"]: item for item in self.contract.run_cases()}
        self.assertEqual(set(results), {*SPEC_CASES, *EXTRA_CASES})
        for case in cases:
            with self.subTest(case=case["id"]):
                result = results[case["id"]]
                self.assertTrue(result["passed"], result)
                self.assertEqual(bool(result["errors"]), bool(case["expected_errors"]))
        self.assertEqual(results["live-gap-rejected"]["errors"], ["live sequence gap"])
        self.assertEqual(results["live-wrong-head-rejected"]["errors"], ["base checkpoint mismatch"])
        self.assertEqual(results["live-step-reissue-rejected"]["errors"], ["live step reissued with different content"])
        self.assertEqual(results["live-checkpoint-upgrade-other-head-rejected"]["errors"],
                         ["checkpoint upgrade does not match the bootstrap manifest"])
        self.assertEqual(results["live-checkpoint-upgrade-changed-set-other-head-rejected"]["errors"],
                         ["checkpoint upgrade does not match the bootstrap manifest"])
        self.assertEqual({case["status"] for case in cases}, {200, 409})
        # Each rebase reason has a case, and every case names fixtures that exist.
        reasons = {self.example(case["response_fixture"].removesuffix("-v1"), "hint")["reason"]
                   for case in cases if case["validator"] == "live_rebase"}
        self.assertEqual(sorted(reasons), sorted(REASONS))

    def test_a_case_with_the_wrong_expectation_or_status_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied_profile(directory)
            document = json.loads((root / "cases.json").read_text())
            by_id = {case["id"]: case for case in document["cases"]}
            by_id["live-advance"]["expected_errors"] = ["live sequence gap"]
            by_id["live-gap-rejected"]["expected_errors"] = []
            by_id["live-rebase-after-import"]["status"] = 200
            # A negative case with the wrong status must not pass because its own expected error still occurs.
            by_id["live-wrong-head-rejected"]["status"] = 409
            # An unexpected extra error fails a negative case as well.
            by_id["live-checkpoint-upgrade-other-head-rejected"]["expected_errors"] = ["a different error"]
            # Cases without a response fixture are 200 answers too: a 1.4 exchange and a bootstrap manifest.
            by_id["live-request-1-4-compatible"]["status"] = 409
            by_id["live-checkpoint-upgrade-from-1-4"]["status"] = 409
            by_id["live-checkpoint-upgrade-from-1-4-changed-set"]["status"] = 409
            (root / "cases.json").write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
            results = {item["case_id"]: item for item in self.contract.run_cases(root)}
        self.assertFalse(results["live-advance"]["passed"])
        self.assertFalse(results["live-gap-rejected"]["passed"])
        self.assertFalse(results["live-rebase-after-import"]["passed"])
        self.assertFalse(results["live-wrong-head-rejected"]["passed"])
        self.assertEqual(results["live-wrong-head-rejected"]["errors"],
                         ["case status does not match the response kind", "base checkpoint mismatch"])
        self.assertFalse(results["live-checkpoint-upgrade-other-head-rejected"]["passed"])
        for name in ("live-request-1-4-compatible", "live-checkpoint-upgrade-from-1-4", "live-checkpoint-upgrade-from-1-4-changed-set"):
            with self.subTest(case=name):
                self.assertFalse(results[name]["passed"])
                self.assertEqual(results[name]["errors"], ["case status does not match the response kind"])
        self.assertTrue(results["live-no-op-when-current"]["passed"])

    def test_case_guards_refuse_an_unknown_validator_and_a_fixture_outside_the_14_profile(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied_profile(directory)
            document = json.loads((root / "cases.json").read_text())
            by_id = {case["id"]: case for case in document["cases"]}
            by_id["live-advance"]["validator"] = "bogus"
            by_id["live-request-1-4-compatible"]["request_file"] = "../1.3.0/examples/sync-noop-signed.json"
            by_id["live-checkpoint-upgrade-from-1-4"]["checkpoint_file"] = "../../1.3.0/profile.json"
            (root / "cases.json").write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
            results = {item["case_id"]: item for item in self.contract.run_cases(root)}
        self.assertEqual(results["live-advance"]["errors"], ["unknown case validator"])
        self.assertEqual(results["live-request-1-4-compatible"]["errors"], ["fixture path leaves the inherited profile"])
        self.assertEqual(results["live-checkpoint-upgrade-from-1-4"]["errors"], ["fixture path leaves the inherited profile"])
        self.assertFalse(any(results[name]["passed"] for name in (
            "live-advance", "live-request-1-4-compatible", "live-checkpoint-upgrade-from-1-4")))
        self.assertEqual(sum(not item["passed"] for item in results.values()), 3)

    def test_a_missing_fixture_fails_only_its_own_case(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied_profile(directory)
            (root / "examples/changes-since-live-wrong-head.json").unlink()
            results = {item["case_id"]: item for item in self.contract.run_cases(root)}
        self.assertFalse(results["live-wrong-head-rejected"]["passed"])
        self.assertEqual(len(results["live-wrong-head-rejected"]["errors"]), 1)
        self.assertIn("missing fixture", results["live-wrong-head-rejected"]["errors"][0])
        self.assertEqual([case for case, item in results.items() if not item["passed"]], ["live-wrong-head-rejected"])

    # Request

    def test_request_requires_base_live_sequence_and_stays_closed(self):
        self.assertEqual(self.request["base_live_sequence"], 0)
        self.assertTrue(self.contract.validate_request(self.request))
        missing = copy.deepcopy(self.request)
        del missing["base_live_sequence"]
        with self.assertRaisesRegex(self.contract.ContractError, "request violates 1.5 schema"):
            self.contract.validate_request(missing)
        for label, edit in (
                ("unknown member", lambda value: value.update(unexpected=1)),
                ("unknown source member", lambda value: value["source"].update(unexpected=1)),
                ("unknown range member", lambda value: value["schema_version_range"].update(unexpected=1)),
                ("negative live sequence", lambda value: value.update(base_live_sequence=-1)),
                ("live sequence beyond I-JSON", lambda value: value.update(base_live_sequence=MAX_INTEGER + 1)),
                ("string live sequence", lambda value: value.update(base_live_sequence="0")),
                ("fractional live sequence", lambda value: value.update(base_live_sequence=0.5)),
                ("null live sequence", lambda value: value.update(base_live_sequence=None)),
                ("other format", lambda value: value.update(accepted_changed_set_formats=["teslatlas-physical-v3-delta-v2"])),
                ("two formats", lambda value: value.update(accepted_changed_set_formats=["teslatlas-physical-v3-delta-v1"] * 2))):
            with self.subTest(case=label):
                with self.assertRaisesRegex(self.contract.ContractError, "request violates 1.5 schema"):
                    self.contract.validate_request(self.edited(self.request, edit, resign=False))
        ceiling = self.edited(self.request, lambda value: value.update(base_live_sequence=MAX_INTEGER), resign=False)
        self.assertTrue(self.contract.validate_request(ceiling))

    def test_a_14_request_gets_14_semantics_and_never_a_live_overlay(self):
        legacy = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-request-physical.json")["request"]
        receipt = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-physical-changed-set.json")["receipt"]
        vehicle = legacy["source"]["vehicle_id"]
        self.assertTrue(self.contract.verify_legacy_exchange(legacy, receipt, vehicle))
        with self.assertRaisesRegex(self.contract.ContractError, "request violates 1.5 schema"):
            self.contract.verify_live_receipt(legacy, receipt, vehicle)
        with self.assertRaisesRegex(self.contract.ContractError, "legacy request received live overlay"):
            self.contract.verify_legacy_exchange(legacy, self.receipt, vehicle)
        with self.assertRaisesRegex(self.contract.ContractError, "legacy request received live overlay"):
            self.contract.verify_legacy_exchange(legacy, self.no_op, vehicle)
        with self.assertRaisesRegex(self.contract.ContractError, "not a 1.4 request"):
            self.contract.verify_legacy_exchange(self.request, receipt, vehicle)
        # A 1.4 request whose base is the head gets the 1.3 signed no-op, bound to that request.
        no_op = self.fixture_signed({
            "kind": "no_op", "vehicle_id": vehicle, "base_receipt_id": legacy["base_receipt_id"],
            "base_manifest_schema": "2.2", "sequence": legacy["from_sequence"], "manifest_schema": "2.2"})
        self.assertTrue(self.contract.verify_legacy_exchange(legacy, no_op, vehicle))
        for label, edit, expected in (
                ("other base", lambda value: value.update(base_receipt_id="receipt_demo_000099"), "no-op base mismatch"),
                ("other sequence", lambda value: value.update(sequence=legacy["from_sequence"] + 1), "no-op base mismatch"),
                ("other vehicle", lambda value: value.update(vehicle_id="44444444-4444-4444-8444-444444444444"), "no-op vehicle mismatch"),
                ("unknown member", lambda value: value.update(unexpected=1), "no-op violates 1.3 schema")):
            with self.subTest(case=label):
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_legacy_exchange(legacy, self.edited(no_op, edit), vehicle)
        tampered = copy.deepcopy(no_op)
        tampered["signature"]["signed_payload_sha256"] = "0" * 64
        with self.assertRaisesRegex(self.contract.ContractError, "no-op signature payload mismatch"):
            self.contract.verify_legacy_exchange(legacy, tampered, vehicle)
        invalid = copy.deepcopy(no_op)
        invalid["signature"]["signature"] = base64.b64encode(bytes(64)).decode("ascii")
        with self.assertRaisesRegex(self.contract.ContractError, "no-op signature invalid"):
            self.contract.verify_legacy_exchange(legacy, invalid, vehicle)
        noop_13 = self.contract.inherited.read_json(ROOT / "profiles/hub-sync-v1/1.3.0/examples/sync-noop-signed.json")["receipt"]
        with self.assertRaisesRegex(self.contract.ContractError, "no-op vehicle mismatch"):
            self.contract.verify_legacy_exchange(legacy, noop_13, vehicle)
        with self.assertRaisesRegex(self.contract.ContractError, "request violates 1.5 schema"):
            self.contract.classify_request({**legacy, "unexpected": 1})

    # Live changed set

    def test_live_changed_set_binds_the_exact_head_and_advances_one_step(self):
        self.assertTrue(self.contract.verify_live_receipt(self.request, self.receipt, self.vehicle, self.head_raw))
        self.assertEqual(self.receipt["base"], self.receipt["target"])
        self.assertEqual((self.receipt["from_live_sequence"], self.receipt["to_live_sequence"], self.receipt["live_sequence"]), (0, 1, 1))
        self.assertEqual(self.receipt["kind"], "physical_changed_set")
        self.assertEqual(self.receipt["payload_format"], "teslatlas-physical-v3-delta-v1")
        for label, edit, expected in (
                ("gap", lambda value: value.update(to_live_sequence=3, live_sequence=3), "live sequence gap"),
                ("repeat", lambda value: value.update(to_live_sequence=0, live_sequence=0), "receipt violates 1.5 schema"),
                ("backwards", lambda value: value.update(from_live_sequence=2, to_live_sequence=1), "live base sequence mismatch"),
                ("not the request's position", lambda value: value.update(from_live_sequence=1, to_live_sequence=2, live_sequence=2), "live base sequence mismatch"),
                ("live_sequence is not the target", lambda value: value.update(live_sequence=2), "live sequence member mismatch"),
                ("target moves the head", lambda value: value["target"].update(sequence=44, receipt_id="receipt_demo_000044"), "live target is not the base head"),
                ("target digest differs", lambda value: value["target"].update(manifest_sha256="0" * 64), "live target is not the base head"),
                ("different head", lambda value: (value["base"].update(sequence=44, receipt_id="receipt_demo_000044", manifest_id="manifest_demo_000044"),
                                                  value["target"].update(sequence=44, receipt_id="receipt_demo_000044", manifest_id="manifest_demo_000044")),
                 "base checkpoint mismatch"),
                ("different manifest digest", lambda value: (value["base"].update(manifest_sha256="0" * 64), value["target"].update(manifest_sha256="0" * 64)),
                 "base checkpoint mismatch"),
                ("other vehicle", lambda value: value.update(vehicle_id="44444444-4444-4444-8444-444444444444"), "source vehicle mismatch"),
                ("other generation", lambda value: value["source"].update(generation=2), "source binding mismatch"),
                ("noncontiguous chunks", lambda value: value["chunks"][0].update(chunk_index=1), "delta chunks are not contiguous"),
                ("byte total", lambda value: value.update(total_compressed_bytes=value["total_compressed_bytes"] + 1), "aggregate compressed byte count mismatch"),
                ("unknown member", lambda value: value.update(unexpected=1), "receipt violates 1.5 schema"),
                ("missing from member", lambda value: value.pop("from_live_sequence"), "receipt violates 1.5 schema"),
                ("missing to member", lambda value: value.pop("to_live_sequence"), "receipt violates 1.5 schema"),
                ("missing live member", lambda value: value.pop("live_sequence"), "receipt violates 1.5 schema"),
                ("negative from", lambda value: value.update(from_live_sequence=-1, to_live_sequence=0, live_sequence=0), "receipt violates 1.5 schema"),
                ):
            with self.subTest(case=label):
                mutant = self.edited(self.receipt, edit)
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_live_receipt(self.request, mutant, self.vehicle)
        # The step rule is not specific to the first step: it holds at every position of the head.
        def step(request_position, from_sequence, to_sequence):
            request = self.edited(self.request, lambda value: value.update(base_live_sequence=request_position), resign=False)
            receipt = self.edited(self.receipt, lambda value: value.update(
                from_live_sequence=from_sequence, to_live_sequence=to_sequence, live_sequence=to_sequence))
            return self.contract.verify_live_receipt(request, receipt, self.vehicle)
        for position in (1, 2, 41, MAX_INTEGER - 1):
            with self.subTest(position=position, step="advance"):
                self.assertTrue(step(position, position, position + 1))
            with self.subTest(position=position, step="repeat"):
                with self.assertRaisesRegex(self.contract.ContractError, "live sequence gap"):
                    step(position, position, position)
            if position + 2 <= MAX_INTEGER:
                with self.subTest(position=position, step="skip"):
                    with self.assertRaisesRegex(self.contract.ContractError, "live sequence gap"):
                        step(position, position, position + 2)
            with self.subTest(position=position, step="from another position"):
                with self.assertRaisesRegex(self.contract.ContractError, "live base sequence mismatch"):
                    step(position, position - 1, position)
        beyond = copy.deepcopy(self.receipt)
        beyond.update(from_live_sequence=MAX_INTEGER, to_live_sequence=MAX_INTEGER + 1, live_sequence=MAX_INTEGER + 1)
        self.assertTrue(self.contract._violations("physical-changed-set-receipt.schema.json", "receipt", beyond))
        ceiling = copy.deepcopy(self.receipt)
        ceiling.update(from_live_sequence=MAX_INTEGER - 1, to_live_sequence=MAX_INTEGER, live_sequence=MAX_INTEGER)
        self.assertFalse(self.contract._violations("physical-changed-set-receipt.schema.json", "receipt", ceiling))

    def test_live_fixtures_for_gap_and_wrong_head_fail_client_validation(self):
        gap = self.example("changes-since-live-gap", "receipt")
        self.assertNotEqual(gap["to_live_sequence"], gap["from_live_sequence"] + 1)
        self.assertFalse(self.contract._violations("physical-changed-set-receipt.schema.json", "receipt", gap))
        self.contract._signature(gap, self.vehicle, "signature")
        with self.assertRaisesRegex(self.contract.ContractError, "live sequence gap"):
            self.contract.verify_live_receipt(self.request, gap, self.vehicle)
        wrong = self.example("changes-since-live-wrong-head", "receipt")
        self.assertEqual(wrong["base"], wrong["target"])
        self.assertNotEqual(wrong["base"]["receipt_id"], self.request["base_receipt_id"])
        self.contract._signature(wrong, self.vehicle, "signature")
        with self.assertRaisesRegex(self.contract.ContractError, "base checkpoint mismatch"):
            self.contract.verify_live_receipt(self.request, wrong, self.vehicle)

    def test_head_manifest_binding_of_a_live_receipt(self):
        other = (PROFILE / "examples/signed-full-manifest-live-next.json").read_bytes()
        with self.assertRaisesRegex(self.contract.ContractError, "head signed manifest digest mismatch"):
            self.contract.verify_live_receipt(self.request, self.receipt, self.vehicle, other)
        child = json.loads(self.head_raw)
        child["chunks"][0]["chunk_index"] = 1
        raw = (json.dumps(self.fixture_signed(child), indent=2, sort_keys=True) + "\n").encode()
        receipt = copy.deepcopy(self.receipt)
        digest = hashlib.sha256(raw).hexdigest()
        receipt["base"]["manifest_sha256"] = receipt["target"]["manifest_sha256"] = digest
        request = {**self.request, "base_manifest_sha256": digest}
        with self.assertRaisesRegex(self.contract.ContractError, "bootstrap manifest chunks are not contiguous"):
            self.contract.verify_live_receipt(request, self.fixture_signed(receipt), self.vehicle, raw)
        duplicate = b'{"kind":"no_op",' + self.head_raw.lstrip()[1:]
        receipt["base"]["manifest_sha256"] = receipt["target"]["manifest_sha256"] = hashlib.sha256(duplicate).hexdigest()
        request = {**self.request, "base_manifest_sha256": hashlib.sha256(duplicate).hexdigest()}
        with self.assertRaisesRegex(self.contract.ContractError, "not valid JSON"):
            self.contract.verify_live_receipt(request, self.fixture_signed(receipt), self.vehicle, duplicate)
        # A correctly signed manifest whose digest the checkpoint carries, but which names another head.
        for key, value in (("manifest_id", "manifest_demo_other"), ("receipt_id", "receipt_demo_other"),
                           ("sequence", 44)):
            with self.subTest(identity=key):
                other = self.fixture_signed({**json.loads(self.head_raw), key: value})
                raw = (json.dumps(other, indent=2, sort_keys=True) + "\n").encode()
                digest = hashlib.sha256(raw).hexdigest()
                bound = copy.deepcopy(self.receipt)
                bound["base"]["manifest_sha256"] = bound["target"]["manifest_sha256"] = digest
                request = {**self.request, "base_manifest_sha256": digest}
                with self.assertRaisesRegex(self.contract.ContractError, "head signed manifest identity mismatch"):
                    self.contract.verify_live_receipt(request, self.fixture_signed(bound), self.vehicle, raw)

    # Manifest

    def test_manifest_carries_a_constant_live_sequence_and_checkpoints_are_profile_specific(self):
        manifest = json.loads(self.head_raw)
        self.assertEqual(manifest["live_sequence"], 0)
        self.assertTrue(self.contract.verify_head_manifest(self.head_raw, self.receipt["base"], self.vehicle))
        legacy_raw = (PREVIOUS / "examples/signed-full-manifest-43.json").read_bytes()
        legacy = json.loads(legacy_raw)
        self.assertEqual({k: v for k, v in manifest.items() if k not in ("live_sequence", "signature")},
                         {k: v for k, v in legacy.items() if k != "signature"})
        self.assertNotEqual(hashlib.sha256(self.head_raw).hexdigest(), hashlib.sha256(legacy_raw).hexdigest())
        self.assertEqual(self.receipt["base"]["manifest_sha256"], hashlib.sha256(self.head_raw).hexdigest())
        self.assertEqual(self.request["base_manifest_sha256"], hashlib.sha256(self.head_raw).hexdigest())
        with self.assertRaisesRegex(self.contract.ContractError, "violates live schema 2.2"):
            self.contract.verify_manifest(legacy_raw, self.vehicle)
        for label, edit, expected in (
                ("later position", lambda value: value.update(live_sequence=1), "violates live schema 2.2"),
                ("missing position", lambda value: value.pop("live_sequence"), "violates live schema 2.2"),
                ("unknown member", lambda value: value.update(unexpected=1), "violates live schema 2.2"),
                ("wrong kind", lambda value: value.update(kind="no_op"), "violates live schema 2.2"),
                ("other vehicle", lambda value: value.update(vehicle_id="44444444-4444-4444-8444-444444444444"), "vehicle mismatch"),
                ("noncontiguous chunks", lambda value: value["chunks"][0].update(chunk_index=1), "chunks are not contiguous")):
            with self.subTest(case=label):
                raw = json.dumps(self.edited(manifest, edit)).encode()
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_manifest(raw, self.vehicle)
        resigned_wrongly = self.edited(manifest, lambda value: value["signature"].update(signature=base64.b64encode(bytes(64)).decode()), resign=False)
        with self.assertRaisesRegex(self.contract.ContractError, "signature invalid"):
            self.contract.verify_manifest(json.dumps(resigned_wrongly).encode(), self.vehicle)

    def test_manifest_schema_is_the_13_snapshot_plus_exactly_the_live_member(self):
        base = json.loads((ROOT / "profiles/hub-sync-v1/1.3.0/sync-manifest.schema.json").read_text())["$defs"]["schema_2_2"]
        live = json.loads((PROFILE / "live-overlay.schema.json").read_text())["$defs"]["manifest"]
        self.assertEqual({k: v for k, v in live["properties"].items() if k != "live_sequence"}, base["properties"])
        self.assertEqual(live["properties"]["live_sequence"], {"const": 0})
        self.assertEqual(live["required"], [*base["required"], "live_sequence"])
        self.assertIs(live["additionalProperties"], False)

    def test_request_receipt_and_hint_are_the_14_definitions_plus_exactly_the_live_members(self):
        def load(directory, name, kind):
            return json.loads((directory / name).read_text())["$defs"][kind]
        natural = {"type": "integer", "minimum": 0, "maximum": MAX_INTEGER}
        positive = {"type": "integer", "minimum": 1, "maximum": MAX_INTEGER}
        request = load(PROFILE, "changes-since-request.schema.json", "request")
        expected = copy.deepcopy(load(PREVIOUS, "changes-since-request.schema.json", "request"))
        expected["properties"]["base_live_sequence"] = natural
        expected["required"].append("base_live_sequence")
        self.assertEqual(request, expected)
        receipt = load(PROFILE, "physical-changed-set-receipt.schema.json", "receipt")
        expected = copy.deepcopy(load(PREVIOUS, "physical-changed-set-receipt.schema.json", "receipt"))
        expected["properties"].update(live_sequence=positive, from_live_sequence=natural, to_live_sequence=positive)
        expected["required"].extend(["live_sequence", "from_live_sequence", "to_live_sequence"])
        self.assertEqual(receipt, expected)
        hint = load(PROFILE, "physical-rebase-hint.schema.json", "hint")
        expected = copy.deepcopy(load(PREVIOUS, "physical-rebase-hint.schema.json", "hint"))
        retry = copy.deepcopy(request)
        retry["properties"]["base_live_sequence"] = {"const": 0}
        expected["properties"].update(reason={"enum": REASONS}, requested_live_sequence=natural, retry_request=retry)
        expected["required"].insert(expected["required"].index("requested_base") + 1, "requested_live_sequence")
        self.assertEqual(hint, expected)
        no_op = load(PROFILE, "live-overlay.schema.json", "no_op")
        noop_13 = load(ROOT / "profiles/hub-sync-v1/1.3.0", "noop.schema.json", "receipt")
        # The whole live no-op definition: the 1.3 no-op members that stay as they were (kind, the vehicle UUID and the
        # opaque base receipt), the 1.3 schema enum tightened to the one schema this profile serves (2.2) on both
        # manifest members, a positive sequence, the 1.5 receipt signature, and the two live additions.
        hex_digest = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
        expected = {
            "type": "object", "additionalProperties": False,
            "properties": {
                **{name: copy.deepcopy(noop_13["properties"][name]) for name in ("kind", "vehicle_id", "base_receipt_id")},
                "base_manifest_sha256": hex_digest,
                "base_manifest_schema": {"const": "2.2"}, "manifest_schema": {"const": "2.2"},
                "sequence": positive, "live_sequence": natural,
                "signature": copy.deepcopy(receipt["properties"]["signature"])}}
        self.assertEqual(sorted(no_op["properties"]), sorted([*noop_13["properties"], "base_manifest_sha256", "live_sequence"]))
        self.assertEqual({key: value for key, value in no_op.items() if key != "required"}, expected)
        self.assertEqual(sorted(no_op["required"]), sorted(expected["properties"]))
        self.assertEqual(len(no_op["required"]), len(set(no_op["required"])))
        # The 1.3 no-op allowed 2.1 and 2.2 on both members; the live no-op allows 2.2 only.
        self.assertEqual(noop_13["properties"]["manifest_schema"]["enum"], ["2.1", "2.2"])
        self.assertEqual(noop_13["properties"]["base_manifest_schema"]["enum"], ["2.1", "2.2"])
        self.assertEqual(noop_13["properties"]["vehicle_id"]["format"], "uuid")
        self.assertEqual(noop_13["properties"]["base_receipt_id"]["pattern"], "^[A-Za-z0-9._~-]+$")

    def test_no_object_in_any_1_5_schema_is_open(self):
        def objects(value, path):
            """(path, closed) for every object schema, nested ones included."""
            found = []
            if isinstance(value, dict):
                if "properties" in value:
                    found.append((path, value.get("additionalProperties") is False))
                for key, item in value.items():
                    found += objects(item, f"{path}.{key}")
            elif isinstance(value, list):
                for index, item in enumerate(value):
                    found += objects(item, f"{path}[{index}]")
            return found
        # The walk reaches the nested objects: source and range, checkpoints, chunks, replacement and retry_request.
        for name, count in (("changes-since-request.schema.json", 3), ("physical-changed-set-receipt.schema.json", 7),
                            ("physical-rebase-hint.schema.json", 9), ("live-overlay.schema.json", 6)):
            with self.subTest(schema=name):
                found = objects(json.loads((PROFILE / name).read_text()), name)
                self.assertEqual(len(found), count)
                self.assertEqual([path for path, closed in found if not closed], [])
        hint = json.loads((PROFILE / "physical-rebase-hint.schema.json").read_text())["$defs"]["hint"]["properties"]
        for member in ("requested_base", "replacement", "retry_request"):
            self.assertIs(hint[member]["additionalProperties"], False, member)
        # A schema-valid hint with a stray member anywhere in the nested objects is refused end to end.
        for label, edit in (("replacement", lambda value: value["replacement"].update(extra=1)),
                            ("requested base", lambda value: value["requested_base"].update(extra=1)),
                            ("chunk", lambda value: value["replacement"]["chunks"][0].update(extra=1)),
                            ("retry request", lambda value: value["retry_request"].update(extra=1)),
                            ("retry source", lambda value: value["retry_request"]["source"].update(extra=1))):
            with self.subTest(hint=label):
                with self.assertRaisesRegex(self.contract.ContractError, "rebase violates 1.5 schema|request violates 1.5 schema"):
                    self.contract.verify_rebase(self.stale, self.edited(self.head_changed, edit), self.vehicle, self.head_raw)
        for label, edit in (("base", lambda value: value["base"].update(extra=1)),
                            ("target", lambda value: value["target"].update(extra=1)),
                            ("source", lambda value: value["source"].update(extra=1)),
                            ("chunk pack", lambda value: value["chunks"][0]["pack"].update(extra=1)),
                            ("signature", lambda value: value["signature"].update(extra=1))):
            with self.subTest(receipt=label):
                with self.assertRaisesRegex(self.contract.ContractError, "receipt violates 1.5 schema"):
                    self.contract.verify_live_receipt(self.request, self.edited(self.receipt, edit, resign=False), self.vehicle)

    def test_bootstrap_then_overlay_requests_from_the_manifest_position(self):
        self.assertTrue(self.contract.verify_bootstrap_then_overlay(self.head_raw, self.request, self.receipt, self.vehicle))
        with self.assertRaisesRegex(self.contract.ContractError, "overlay request does not start from the bootstrap manifest"):
            self.contract.verify_bootstrap_then_overlay(self.head_raw, self.current, self.example("changes-since-live-advance", "receipt"), self.vehicle)
        with self.assertRaisesRegex(self.contract.ContractError, "overlay request does not start from the bootstrap manifest"):
            self.contract.verify_bootstrap_then_overlay(self.next_raw, self.request, self.receipt, self.vehicle)

    # No-op

    def test_live_no_op_proves_currency_of_head_and_live_sequence(self):
        self.assertTrue(self.contract.verify_no_op(self.current, self.no_op, self.vehicle))
        self.assertEqual(self.no_op["kind"], "no_op")
        self.assertEqual((self.no_op["base_receipt_id"], self.no_op["live_sequence"]), (self.current["base_receipt_id"], 1))
        for label, request, edit, expected in (
                ("behind", self.request, lambda value: None, "no-op live sequence mismatch"),
                ("ahead", {**self.current, "base_live_sequence": 2}, lambda value: None, "no-op live sequence mismatch"),
                ("other head", self.stale, lambda value: None, "no-op base mismatch"),
                ("other digest", {**self.current, "base_manifest_sha256": "0" * 64}, lambda value: None, "no-op base mismatch"),
                ("other sequence", {**self.current, "from_sequence": 44}, lambda value: None, "no-op base mismatch"),
                ("other vehicle", self.current, lambda value: value.update(vehicle_id="44444444-4444-4444-8444-444444444444"), "no-op vehicle mismatch"),
                ("missing live member", self.current, lambda value: value.pop("live_sequence"), "no-op violates 1.5 schema"),
                ("unknown member", self.current, lambda value: value.update(unexpected=1), "no-op violates 1.5 schema"),
                ("wrong kind", self.current, lambda value: value.update(kind="physical_changed_set"), "no-op violates 1.5 schema"),
                # The 1.3 no-op allowed schema 2.1 on both manifest members; the live no-op serves schema 2.2 only.
                ("manifest schema 2.1", self.current, lambda value: value.update(manifest_schema="2.1"), "no-op violates 1.5 schema"),
                ("base manifest schema 2.1", self.current, lambda value: value.update(base_manifest_schema="2.1"),
                 "no-op violates 1.5 schema"),
                ("sequence 0", self.current, lambda value: value.update(sequence=0), "no-op violates 1.5 schema"),
                ("base digest not hex", self.current, lambda value: value.update(base_manifest_sha256="G" * 64),
                 "no-op violates 1.5 schema"),
                ("base digest opaque", self.current, lambda value: value.update(base_manifest_sha256="a.b_c~d-" * 8),
                 "no-op violates 1.5 schema"),
                ("vehicle not a uuid", self.current, lambda value: value.update(vehicle_id="vehicle_demo"),
                 "no-op violates 1.5 schema")):
            with self.subTest(case=label):
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_no_op(request, self.edited(self.no_op, edit), self.vehicle)
        noop_13 = self.contract.inherited.read_json(ROOT / "profiles/hub-sync-v1/1.3.0/examples/sync-noop-signed.json")["receipt"]
        with self.assertRaisesRegex(self.contract.ContractError, "no-op violates 1.5 schema"):
            self.contract.verify_no_op(self.current, noop_13, self.vehicle)

    # Rebase

    def test_rebase_after_import_and_after_retention_name_a_newer_head_and_retry_from_zero(self):
        self.assertTrue(self.contract.verify_rebase(self.stale, self.head_changed, self.vehicle, self.head_raw))
        self.assertTrue(self.contract.verify_rebase(self.retained, self.retention, self.vehicle, self.next_raw))
        self.assertEqual((self.head_changed["reason"], self.retention["reason"]), ("head_changed", "live_retention"))
        for hint, request in ((self.head_changed, self.stale), (self.retention, self.retained)):
            self.assertEqual(hint["retry_request"]["base_live_sequence"], 0)
            self.assertEqual(hint["requested_live_sequence"], request["base_live_sequence"])
            self.assertNotEqual(hint["replacement"]["receipt_id"], hint["requested_base"]["receipt_id"])
            self.assertGreater(hint["replacement"]["sequence"], hint["requested_base"]["sequence"])
        # The retention rebase is for the head the client asked about, which a replacement then supersedes.
        self.assertEqual(self.retention["requested_base"]["receipt_id"], self.retained["base_receipt_id"])

    def test_rebase_rejects_loops_bad_retries_reasons_and_other_requests(self):
        cases = (
            ("reason outside the enum", lambda value: value.update(reason="compacted"), "rebase violates 1.5 schema"),
            ("missing requested position", lambda value: value.pop("requested_live_sequence"), "rebase violates 1.5 schema"),
            ("requested position differs", lambda value: value.update(requested_live_sequence=2), "rebase requested live sequence mismatch"),
            ("retry without the live member", lambda value: value["retry_request"].pop("base_live_sequence"), "rebase violates 1.5 schema"),
            ("retry from a later position", lambda value: value["retry_request"].update(base_live_sequence=1), "rebase violates 1.5 schema"),
            ("retry to another head", lambda value: value["retry_request"].update(from_sequence=44), "rebase retry request mismatch"),
            ("retry source differs", lambda value: value["retry_request"]["source"].update(generation=2), "rebase source mismatch"),
            ("requested head differs", lambda value: value["requested_base"].update(sequence=41), "rebase requested base mismatch"),
            ("unknown member", lambda value: value.update(unexpected=1), "rebase violates 1.5 schema"),
            ("other vehicle", lambda value: value.update(vehicle_id="44444444-4444-4444-8444-444444444444"), "rebase source mismatch"),
        )
        for label, edit, expected in cases:
            with self.subTest(case=label):
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_rebase(self.stale, self.edited(self.head_changed, edit), self.vehicle, self.head_raw)

        def same_head(value):
            head = self.retained["base_receipt_id"]
            value["replacement"].update(manifest_id=self.retained["base_manifest_id"], receipt_id=head,
                                        manifest_sha256=self.retained["base_manifest_sha256"], sequence=43,
                                        chunks=json.loads(self.head_raw)["chunks"])
            value["retry_request"].update(base_manifest_id=self.retained["base_manifest_id"], base_receipt_id=head,
                                          base_manifest_sha256=self.retained["base_manifest_sha256"], from_sequence=43)
        with self.assertRaisesRegex(self.contract.ContractError, "rebase replacement is not a newer head"):
            self.contract.verify_rebase(self.retained, self.edited(self.retention, same_head), self.vehicle, self.head_raw)

        def older_head(value):
            value["replacement"].update(sequence=41)
            value["retry_request"].update(from_sequence=41)
        with self.assertRaisesRegex(self.contract.ContractError, "rebase replacement is not a newer head"):
            self.contract.verify_rebase(self.retained, self.edited(self.retention, older_head), self.vehicle, self.next_raw)
        # An equal sequence under another receipt_id is not newer either; the manifest and digest agree with the hint.
        child = self.fixture_signed({**json.loads(self.next_raw), "sequence": 43})
        raw = (json.dumps(child, indent=2, sort_keys=True) + "\n").encode()
        digest = hashlib.sha256(raw).hexdigest()

        def equal_sequence(value):
            value["replacement"].update(sequence=43, manifest_sha256=digest)
            value["retry_request"].update(from_sequence=43, base_manifest_sha256=digest)
        with self.assertRaisesRegex(self.contract.ContractError, "rebase replacement is not a newer head"):
            self.contract.verify_rebase(self.retained, self.edited(self.retention, equal_sequence), self.vehicle, raw)
        # A greater sequence under the requested receipt_id is not a different head either; the manifest, its digest
        # and the exact retry all agree with the hint, so only the receipt_id comparison can refuse it.
        reused = self.fixture_signed({**json.loads(self.next_raw), "receipt_id": self.retained["base_receipt_id"]})
        reused_raw = (json.dumps(reused, indent=2, sort_keys=True) + "\n").encode()
        reused_digest = hashlib.sha256(reused_raw).hexdigest()

        def same_receipt_id(value):
            value["replacement"].update(receipt_id=self.retained["base_receipt_id"], manifest_sha256=reused_digest)
            value["retry_request"].update(base_receipt_id=self.retained["base_receipt_id"], base_manifest_sha256=reused_digest)
        self.assertGreater(self.retention["replacement"]["sequence"], self.retained["from_sequence"])
        with self.assertRaisesRegex(self.contract.ContractError, "rebase replacement is not a newer head"):
            self.contract.verify_rebase(self.retained, self.edited(self.retention, same_receipt_id), self.vehicle, reused_raw)
        with self.assertRaisesRegex(self.contract.ContractError, "rebase requested base mismatch"):
            self.contract.verify_rebase(self.retained, self.head_changed, self.vehicle, self.head_raw)
        for reason in REASONS:
            with self.subTest(reason=reason):
                self.assertTrue(self.contract.verify_rebase(
                    self.stale, self.edited(self.head_changed, lambda value: value.update(reason=reason)), self.vehicle, self.head_raw))

    def test_rebase_admits_the_replacement_manifest_only_when_it_is_the_signed_live_manifest(self):
        legacy_raw = (PREVIOUS / "examples/signed-full-manifest-43.json").read_bytes()
        with self.assertRaisesRegex(self.contract.ContractError, "rebase replacement manifest digest mismatch"):
            self.contract.verify_rebase(self.stale, self.head_changed, self.vehicle, legacy_raw)

        def replace_manifest(edit, resign=True, keep_hint_chunks=False):
            child = json.loads(self.head_raw)
            edit(child)
            if resign:
                child = self.fixture_signed(child)
            raw = (json.dumps(child, indent=2, sort_keys=True) + "\n").encode()
            digest = hashlib.sha256(raw).hexdigest()
            hint = copy.deepcopy(self.head_changed)
            hint["replacement"].update(manifest_sha256=digest)
            if not keep_hint_chunks:
                hint["replacement"]["chunks"] = child["chunks"]
            hint["retry_request"]["base_manifest_sha256"] = digest
            return self.fixture_signed(hint), raw
        for label, edit, resign, expected in (
                ("legacy shape", lambda child: child.pop("live_sequence"), True, "replacement manifest violates live schema 2.2"),
                ("later position", lambda child: child.update(live_sequence=1), True, "replacement manifest violates live schema 2.2"),
                ("other vehicle", lambda child: child.update(vehicle_id="44444444-4444-4444-8444-444444444444"), True, "replacement manifest vehicle mismatch"),
                ("noncontiguous chunks", lambda child: child["chunks"][0].update(chunk_index=1), True, "replacement manifest chunks are not contiguous"),
                ("invalid child signature", lambda child: child["signature"].update(signature=base64.b64encode(bytes(64)).decode()), False,
                 "replacement manifest signature invalid"),
                # The child is correctly signed and its digest is in the hint, but it names another head.
                ("other manifest id", lambda child: child.update(manifest_id="manifest_demo_other"), True,
                 "rebase replacement manifest mismatch"),
                ("other receipt id", lambda child: child.update(receipt_id="receipt_demo_other"), True,
                 "rebase replacement manifest mismatch"),
                ("other sequence", lambda child: child.update(sequence=45), True, "rebase replacement manifest mismatch")):
            with self.subTest(case=label):
                hint, raw = replace_manifest(edit, resign)
                with self.assertRaisesRegex(self.contract.ContractError, expected):
                    self.contract.verify_rebase(self.stale, hint, self.vehicle, raw)
        # The hint lists chunks that differ from the signed manifest it carries the digest of.
        hint, raw = replace_manifest(lambda child: child["chunks"][0]["pack"].update(compressed_bytes=4097), keep_hint_chunks=True)
        with self.assertRaisesRegex(self.contract.ContractError, "rebase replacement manifest mismatch"):
            self.contract.verify_rebase(self.stale, hint, self.vehicle, raw)
        duplicate = b'{"live_sequence":5,' + self.head_raw.lstrip()[1:]
        hint = copy.deepcopy(self.head_changed)
        digest = hashlib.sha256(duplicate).hexdigest()
        hint["replacement"]["manifest_sha256"] = digest
        hint["retry_request"]["base_manifest_sha256"] = digest
        with self.assertRaisesRegex(self.contract.ContractError, "not valid JSON"):
            self.contract.verify_rebase(self.stale, self.fixture_signed(hint), self.vehicle, duplicate)

    def test_an_expired_head_and_a_client_ahead_of_the_hub_get_a_signed_rebase_never_an_error(self):
        # Head 40 was superseded long ago and its packs have expired: only its identity is retained, so no
        # manifest is published for it and the replacement jumps over several heads.
        self.assertTrue(self.contract.verify_rebase(self.expired, self.expired_hint, self.vehicle, self.next_raw))
        self.assertEqual(self.expired_hint["reason"], "head_changed")
        self.assertGreater(self.expired_hint["replacement"]["sequence"] - self.expired_hint["requested_base"]["sequence"], 1)
        published = {json.loads(path.read_text())["receipt_id"] for path in (PROFILE / "examples").glob("signed-full-manifest-*.json")}
        self.assertNotIn(self.expired["base_receipt_id"], published)
        self.assertEqual(self.expired_hint["retry_request"]["base_live_sequence"], 0)
        with self.assertRaisesRegex(self.contract.ContractError, "rebase requested base mismatch"):
            self.contract.verify_rebase(self.stale, self.expired_hint, self.vehicle, self.next_raw)
        # The client is ahead of the Hub's position for the exact current head: the Hub rotated to a new head.
        self.assertTrue(self.contract.verify_rebase(self.ahead, self.ahead_hint, self.vehicle, self.next_raw))
        self.assertEqual((self.ahead_hint["reason"], self.ahead_hint["requested_live_sequence"]), ("live_ahead", 9))
        self.assertEqual(self.ahead["base_receipt_id"], self.ahead_hint["requested_base"]["receipt_id"])
        self.assertNotEqual(self.ahead_hint["replacement"]["receipt_id"], self.ahead["base_receipt_id"])
        with self.assertRaisesRegex(self.contract.ContractError, "rebase requested live sequence mismatch"):
            self.contract.verify_rebase(self.request, self.ahead_hint, self.vehicle, self.next_raw)

    def test_a_live_step_is_signed_for_one_changed_set_only(self):
        reissued = self.example("changes-since-live-reissued", "receipt")
        # Each receipt is valid on its own, which is why a client must compare them.
        self.assertTrue(self.contract.verify_live_receipt(self.request, reissued, self.vehicle, self.head_raw))
        self.contract._signature(reissued, self.vehicle, "signature")
        self.assertEqual(self.contract.live_step_key(reissued), self.contract.live_step_key(self.receipt))
        with self.assertRaisesRegex(self.contract.ContractError, "live step reissued with different content"):
            self.contract.verify_live_step_unique(self.receipt, reissued)
        with self.assertRaisesRegex(self.contract.ContractError, "live step reissued with different content"):
            self.contract.verify_live_step_unique(reissued, self.receipt)
        # Any content difference is a reissue, not only a different total: every member of the receipt that is not part
        # of the step's identity is varied alone, and each second receipt is a correctly re-signed receipt.
        def differing(first, second):
            return sorted(key for key in first if key != "signature" and first[key] != second[key])
        variants = (
            ("chunk pack digest", ["chunks"], lambda value: value["chunks"][0]["pack"].update(sha256="b" * 64)),
            ("chunk object name", ["chunks"], lambda value: value["chunks"][0]["pack"].update(object_name="packs/other.sqlite.zst")),
            ("chunk compressed bytes", ["chunks"], lambda value: value["chunks"][0]["pack"].update(
                compressed_bytes=value["chunks"][0]["pack"]["compressed_bytes"] + 1)),
            ("chunk count", ["chunks"], lambda value: value["chunks"].append(
                {**value["chunks"][0], "chunk_index": 1})),
            ("target commitment", ["target_raw_sha256"], lambda value: value.update(target_raw_sha256="5" * 64)),
            ("affected months", ["affected_months"], lambda value: value.update(affected_months=value["affected_months"][:-1])),
            ("affected months order", ["affected_months"], lambda value: value["affected_months"].reverse()),
            ("total rows", ["total_rows"], lambda value: value.update(total_rows=value["total_rows"] + 1)),
            ("total compressed bytes", ["total_compressed_bytes"], lambda value: value.update(
                total_compressed_bytes=value["total_compressed_bytes"] + 1)),
            ("total uncompressed bytes", ["total_uncompressed_bytes"], lambda value: value.update(
                total_uncompressed_bytes=value["total_uncompressed_bytes"] + 1)),
            ("affected ids digest", ["affected_projected_ids_sha256"], lambda value: value.update(
                affected_projected_ids_sha256="6" * 64)),
            ("affected ids count", ["affected_projected_ids_count"], lambda value: value.update(
                affected_projected_ids_count=value["affected_projected_ids_count"] + 1)),
            ("impacted roots digest", ["impacted_roots_sha256"], lambda value: value.update(impacted_roots_sha256="7" * 64)),
            ("impacted roots count", ["impacted_roots_count"], lambda value: value.update(
                impacted_roots_count=value["impacted_roots_count"] + 1)))
        self.assertGreater(len(self.receipt["affected_months"]), 1)
        for label, members, edit in variants:
            with self.subTest(variant=label):
                second = self.edited(self.receipt, edit)
                # Same step, one member different, and a signature that is valid for the second content.
                self.assertEqual(self.contract.live_step_key(second), self.contract.live_step_key(self.receipt))
                self.assertEqual(differing(self.receipt, second), members)
                self.assertNotEqual(second["signature"]["signed_payload_sha256"], self.receipt["signature"]["signed_payload_sha256"])
                self.contract._signature(second, self.vehicle, "signature")
                with self.assertRaisesRegex(self.contract.ContractError, "live step reissued with different content"):
                    self.contract.verify_live_step_unique(self.receipt, second)
                with self.assertRaisesRegex(self.contract.ContractError, "live step reissued with different content"):
                    self.contract.verify_live_step_unique(second, self.receipt)
        # A replay is byte-for-byte the same content, a re-signature included; another step is not compared.
        self.assertTrue(self.contract.verify_live_step_unique(self.receipt, copy.deepcopy(self.receipt)))
        self.assertTrue(self.contract.verify_live_step_unique(self.receipt, self.fixture_signed(self.receipt)))
        other = self.edited(self.receipt, lambda value: value.update(from_live_sequence=1, to_live_sequence=2, live_sequence=2,
                                                                     total_rows=value["total_rows"] + 5))
        self.assertTrue(self.contract.verify_live_step_unique(self.receipt, other))
        elsewhere = self.edited(self.receipt, lambda value: (value["base"].update(receipt_id="receipt_demo_000044"),
                                                             value["target"].update(receipt_id="receipt_demo_000044"),
                                                             value.update(total_rows=value["total_rows"] + 5)))
        self.assertTrue(self.contract.verify_live_step_unique(self.receipt, elsewhere))

    def test_a_14_checkpoint_upgrades_by_adopting_the_15_manifest_of_the_same_head(self):
        legacy_raw = (PREVIOUS / "examples/signed-full-manifest-43.json").read_bytes()
        legacy = json.loads(legacy_raw)
        members = self.contract.UPGRADE_MEMBERS
        checkpoint = {key: legacy[key] for key in members}
        adopted = self.contract.verify_checkpoint_upgrade(checkpoint, self.head_raw, self.vehicle)
        self.assertEqual(adopted, {"manifest_id": legacy["manifest_id"], "receipt_id": legacy["receipt_id"], "sequence": 43,
                                   "schema_version": "2.2", "manifest_sha256": hashlib.sha256(self.head_raw).hexdigest(),
                                   "live_sequence": 0})
        self.assertNotEqual(adopted["manifest_sha256"], hashlib.sha256(legacy_raw).hexdigest())
        # The adopted checkpoint is exactly the base of the first overlay request, with the staged packs kept.
        self.assertEqual((self.request["base_manifest_sha256"], self.request["base_live_sequence"]),
                         (adopted["manifest_sha256"], adopted["live_sequence"]))
        self.assertTrue(self.contract.verify_bootstrap_then_overlay(self.head_raw, self.request, self.receipt, self.vehicle))
        # Another head, another chunk list or an unsigned or legacy-shaped manifest is not an upgrade.
        for label, change in (("vehicle", {"vehicle_id": "44444444-4444-4444-8444-444444444444"}),
                              ("manifest id", {"manifest_id": "manifest_demo_000042"}),
                              ("receipt id", {"receipt_id": "receipt_demo_000042"}), ("sequence", {"sequence": 42}),
                              ("schema", {"schema_version": "2.1"}),
                              ("chunk list", {"chunks": [{**checkpoint["chunks"][0], "pack": {
                                  **checkpoint["chunks"][0]["pack"], "sha256": "a" * 64}}]})):
            with self.subTest(case=label):
                with self.assertRaisesRegex(self.contract.ContractError, "checkpoint upgrade does not match the bootstrap manifest"):
                    self.contract.verify_checkpoint_upgrade({**checkpoint, **change}, self.head_raw, self.vehicle)
        with self.assertRaisesRegex(self.contract.ContractError, "checkpoint upgrade does not match the bootstrap manifest"):
            self.contract.verify_checkpoint_upgrade(checkpoint, self.next_raw, self.vehicle)
        with self.assertRaisesRegex(self.contract.ContractError, "upgrade manifest violates live schema 2.2"):
            self.contract.verify_checkpoint_upgrade(checkpoint, legacy_raw, self.vehicle)
        with self.assertRaisesRegex(self.contract.ContractError, "upgrade manifest vehicle mismatch"):
            self.contract.verify_checkpoint_upgrade(checkpoint, self.head_raw, "44444444-4444-4444-8444-444444444444")
        invalid = self.edited(json.loads(self.head_raw), lambda value: value["signature"].update(
            signature=base64.b64encode(bytes(64)).decode()), resign=False)
        with self.assertRaisesRegex(self.contract.ContractError, "upgrade manifest signature invalid"):
            self.contract.verify_checkpoint_upgrade(checkpoint, json.dumps(invalid).encode(), self.vehicle)
        # The 1.4 digest is not a 1.5 base: a request or receipt carrying it fails against the 1.5 manifest of that head,
        # and the Hub answers it with an unsigned 422 unknown_base_receipt.
        digest = hashlib.sha256(legacy_raw).hexdigest()
        bound = copy.deepcopy(self.receipt)
        bound["base"]["manifest_sha256"] = bound["target"]["manifest_sha256"] = digest
        request = {**self.request, "base_manifest_sha256": digest}
        with self.assertRaisesRegex(self.contract.ContractError, "head signed manifest digest mismatch"):
            self.contract.verify_live_receipt(request, self.fixture_signed(bound), self.vehicle, self.head_raw)
        upgrade = json.loads((PROFILE / "profile.json").read_text())["checkpoint_upgrade"]
        self.assertEqual(upgrade["from"], "hub-sync-v1@1.4.0")
        self.assertEqual(upgrade["mechanism"], "client_side_adoption")
        self.assertEqual(members, ("vehicle_id", *self.contract.UPGRADE_IDENTITY, "chunks"))
        self.assertEqual(upgrade["adopt_when"], {
            "always": ["vehicle_id", *self.contract.UPGRADE_IDENTITY],
            "when_the_checkpoint_holds_the_bootstrap_manifest": ["chunks"]})
        self.assertEqual(sorted(upgrade["checkpoint_kinds"]), ["bootstrap_manifest", "changed_set_target"])
        self.assertEqual(upgrade["adopted_live_sequence"], 0)
        self.assertIs(upgrade["reuses_materialised_state"], True)
        self.assertNotIn("reuses_staged_packs", upgrade)
        self.assertIn("The route vehicle binds vehicle_id", upgrade["vehicle_binding"])
        self.assertEqual(upgrade["old_digest_under_a_1_5_request"], {"status": 422, "error": "unknown_base_receipt", "signed": False})
        self.assertIn("MUST NOT send the manifest_sha256 of a 1.4 manifest in a 1.5 request", upgrade["prose"])
        for phrase in ("keeps its materialised state", "reached the head through a 1.4 changed set", "carries no chunk list",
                       "compares the identity members alone", "keeps the 1.4 manifest_sha256 beside its 1.5 checkpoint",
                       "performs a full 1.4 bootstrap"):
            self.assertIn(phrase, upgrade["prose"])
        self.assertNotIn("staged packs", upgrade["prose"])
        self.assertEqual(upgrade["when_selector_later_unserved"], {
            "keep_beside_the_1_5_checkpoint": "manifest_sha256_of_the_1_4_checkpoint_until_the_head_changes",
            "at_live_sequence_0": "resume_1_4_requests_from_the_kept_1_4_checkpoint",
            "at_live_sequence_0_without_a_kept_1_4_checkpoint":
                "fetch_the_1_4_manifest_of_the_same_head_and_adopt_it_when_identity_and_chunks_equal_else_full_1_4_bootstrap",
            "above_live_sequence_0": "full_1_4_bootstrap"})
        # A client that reached its head under 1.5 never held a 1.4 checkpoint to keep.
        for phrase in ("has no kept 1.4 checkpoint", "fetches the 1.4 manifest of the same head under the 1.4 selector",
                       "chunks equal those of its 1.5 manifest", "otherwise performs a full 1.4 bootstrap"):
            self.assertIn(phrase, upgrade["prose"])

    def test_a_14_checkpoint_from_a_changed_set_upgrades_on_identity_alone(self):
        receipt = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-physical-changed-set.json")["receipt"]
        legacy_raw = (PREVIOUS / "examples/signed-full-manifest-43.json").read_bytes()
        target, identity = receipt["target"], self.contract.UPGRADE_IDENTITY
        # The usual 1.4 path after an import: the client holds the receipt's target, which carries no chunk list.
        self.assertNotIn("chunks", target)
        self.assertNotIn("vehicle_id", target)
        self.assertEqual(target["manifest_sha256"], hashlib.sha256(legacy_raw).hexdigest())
        checkpoint = {key: target[key] for key in identity}
        adopted = self.contract.verify_checkpoint_upgrade(checkpoint, self.head_raw, self.vehicle)
        self.assertEqual(adopted["live_sequence"], 0)
        self.assertEqual(adopted["manifest_sha256"], hashlib.sha256(self.head_raw).hexdigest())
        self.assertNotEqual(adopted["manifest_sha256"], target["manifest_sha256"])
        self.assertEqual((adopted["receipt_id"], adopted["sequence"]), (self.request["base_receipt_id"], self.request["from_sequence"]))
        # The base of that changed set is head 42, not the served head, and every identity member is compared.
        with self.assertRaisesRegex(self.contract.ContractError, "checkpoint upgrade does not match the bootstrap manifest"):
            self.contract.verify_checkpoint_upgrade({key: receipt["base"][key] for key in identity}, self.head_raw, self.vehicle)
        for key, value in (("manifest_id", "manifest_demo_000042"), ("receipt_id", "receipt_demo_000042"),
                           ("sequence", 42), ("schema_version", "2.1")):
            with self.subTest(member=key):
                with self.assertRaisesRegex(self.contract.ContractError, "checkpoint upgrade does not match the bootstrap manifest"):
                    self.contract.verify_checkpoint_upgrade({**checkpoint, key: value}, self.head_raw, self.vehicle)
        for key in identity:
            with self.subTest(missing=key):
                with self.assertRaisesRegex(self.contract.ContractError, "checkpoint upgrade is incomplete"):
                    self.contract.verify_checkpoint_upgrade({k: v for k, v in checkpoint.items() if k != key}, self.head_raw, self.vehicle)
        # The route vehicle binds the vehicle when the checkpoint carries none, and the manifest must verify.
        with self.assertRaisesRegex(self.contract.ContractError, "upgrade manifest vehicle mismatch"):
            self.contract.verify_checkpoint_upgrade(checkpoint, self.head_raw, "44444444-4444-4444-8444-444444444444")
        with self.assertRaisesRegex(self.contract.ContractError, "upgrade manifest violates live schema 2.2"):
            self.contract.verify_checkpoint_upgrade(checkpoint, legacy_raw, self.vehicle)

    def test_the_profile_pins_how_a_hub_resolves_a_base(self):
        profile = json.loads((PROFILE / "profile.json").read_text())
        rules = profile["live_rules"]
        rows = rules["base_resolution"]
        table = {entry["when"]: entry["answer"] for entry in rows}
        # First match wins, so the order is part of the rule. An unrecorded identity is refused before anything is
        # rebuilt; a superseded head is answered with a rebase whatever the position (the replacement already exists);
        # a pending rotation pauses the head to serve before any position is judged; only then are positions compared.
        self.assertEqual(rules["base_resolution_order"], "first_match")
        self.assertEqual([entry["when"] for entry in rows], RESOLUTION_ORDER)
        self.assertEqual(table["base_identity_not_recorded_or_differs"], UNSIGNED_422)
        self.assertEqual(table["base_head_is_not_the_head_to_serve"],
                         {"status": 409, "kind": "rebase_required", "reasons": REASONS})
        self.assertEqual(table["head_to_serve_live_service_paused_for_rotation"], TRANSIENT)
        self.assertEqual(table["base_live_sequence_above_head_high_water_mark"], UNSIGNED_422)
        self.assertEqual(table["base_live_sequence_above_head_position_and_not_above_high_water_mark"],
                         {**TRANSIENT, "starts_rotation_outside_request": True})
        self.assertEqual(table["base_live_sequence_equals_head_position"], {"status": 200, "kind": "no_op"})
        self.assertEqual(table["base_live_sequence_behind_head_position"], {"status": 200, "kind": "physical_changed_set"})
        # No row answers a live changed set or a no-op before a pending rotation or a regression is excluded, and no row
        # asks for a replacement that cannot exist yet: a 409 is only ever the answer for a head that is not the one to serve.
        order = [entry["when"] for entry in rows]
        for answered in ("base_live_sequence_equals_head_position", "base_live_sequence_behind_head_position"):
            for guard in ("head_to_serve_live_service_paused_for_rotation",
                          "base_live_sequence_above_head_position_and_not_above_high_water_mark"):
                self.assertLess(order.index(guard), order.index(answered), (guard, answered))
        self.assertEqual([entry["when"] for entry in rows if entry["answer"].get("status") == 409],
                         ["base_head_is_not_the_head_to_serve"])
        self.assertEqual([entry["when"] for entry in rows if entry["answer"].get("transient_failure")],
                         ["head_to_serve_live_service_paused_for_rotation",
                          "base_live_sequence_above_head_position_and_not_above_high_water_mark"])
        self.assertEqual(sorted(reason for entry in rows for reason in entry["answer"].get("reasons", [])), sorted(REASONS))
        # Only a regression the Hub signed past starts a rotation, and it never happens inside a request; the
        # high-water mark lives with the head identity for the life of the vehicle.
        self.assertEqual(rules["head_live_members"], ["live_position", "live_high_water_mark"])
        self.assertEqual(rules["live_high_water_mark"], "highest_live_sequence_ever_signed_for_the_head")
        self.assertIs(rules["live_rotation_inside_request"], False)
        # The example ahead request names a position the Hub could have signed for head 43, which was rotated away.
        self.assertEqual((self.ahead["base_live_sequence"], self.ahead_hint["reason"]), (9, "live_ahead"))
        enum = json.loads((PROFILE / "physical-rebase-hint.schema.json").read_text())["$defs"]["hint"]["properties"]["reason"]["enum"]
        self.assertEqual(enum, REASONS)
        # Only the 409 rebase answers carry a replacement; the 422 is unsigned and every 409 reason has an example hint.
        self.assertEqual({self.example(name, "hint")["reason"] for name in (
            "changes-since-live-rebase-head-changed", "changes-since-live-rebase-retention",
            "changes-since-live-rebase-head-expired", "changes-since-live-rebase-ahead")}, set(REASONS))
        self.assertEqual(rules["head_identity_retention"], "vehicle_lifetime")
        self.assertEqual(rules["head_identity_members"], ["manifest_id", "receipt_id", "manifest_sha256", "sequence", "source"])
        self.assertIs(rules["live_step_content_is_unique"], True)
        self.assertIs(rules["live_regression_rotates_head"], True)
        resolution = profile["checkpoint_resolution"]
        for phrase in ("immediate predecessor", "head whose packs have expired", "did not record for the vehicle",
                       "unsigned 422 unknown_base_receipt", "above the highest live position the Hub recorded as signed for it",
                       "without rebuilding anything", "whatever its base_live_sequence", "so answering costs no rebuild",
                       "drops its checkpoint, performs a full bootstrap under the 1.5 selector"):
            self.assertIn(phrase, resolution)
        errors = profile["error_responses"]
        self.assertEqual(errors["schema"], "../1.3.0/sync-error.schema.json#/$defs/error")
        self.assertIn("../1.3.0/sync-error.schema.json", profile["inherited_artifacts"])
        for phrase in ("Error statuses are inherited from 1.3.0 and are unsigned", "lists only 200 and 409 for changes-since",
                       "a base_live_sequence above the high-water mark of the head to serve", "A base the Hub did not record"):
            self.assertIn(phrase, errors["rule"])
        self.assertEqual(errors["on_unknown_base_receipt"]["client_action"],
                         "drop_the_checkpoint_do_a_full_bootstrap_under_the_1_5_selector_then_request_with_base_live_sequence_0")
        self.assertIs(errors["on_unknown_base_receipt"]["retries_the_same_request"], False)
        for phrase in ("drops its checkpoint, performs a full bootstrap", "it never retries the same request",
                       "A backup restore rolls back the head ledger"):
            self.assertIn(phrase, errors["on_unknown_base_receipt"]["rule"])
        self.assertIn("unknown_base_receipt", json.loads((ROOT / "profiles/hub-sync-v1/1.3.0/sync-error.schema.json").read_text())
                      ["$defs"]["error"]["properties"]["code"]["enum"])
        text = " ".join(rules["prose"])
        for phrase in ("including for a head whose packs have expired", "at most one changed set", "MUST NOT re-issue a live_sequence",
                       "MUST rotate the head before live service resumes", "live_ahead",
                       "The reason is diagnostic", "is answered by folding the live rows into a new head and answering head_changed",
                       "has detected a regression it did not rotate on", "MUST NOT build the replacement inside the request",
                       "a fold is O(history) and a client chooses base_live_sequence", "the high-water mark",
                       "is not recorded by this Hub as issued",
                       "so a client cannot make a Hub rotate by naming a position the Hub holds no record of issuing",
                       "starts the rotation outside the request and pauses live service for the head until the rotation completes",
                       "A 409 live_ahead can therefore only name a replacement that already exists",
                       "a client keeps its checkpoint and retries the same request later",
                       "A restore also rolls back the head ledger"):
            self.assertIn(phrase, text)
        self.assertNotIn("never signed by this Hub", text)
        self.assertNotIn("answers 409 live_ahead naming a replacement head", text)

    def test_the_base_resolution_table_is_executable_and_never_answers_from_a_head_it_must_not(self):
        rules = json.loads((PROFILE / "profile.json").read_text())["live_rules"]
        base = self.request

        def ledger(position, mark, serve_head, paused, other_position=2):
            return {"heads": {
                base["base_receipt_id"]: {"manifest_id": base["base_manifest_id"], "manifest_sha256": base["base_manifest_sha256"],
                                          "sequence": base["from_sequence"], "position": position, "high_water_mark": mark},
                "receipt_demo_000044": {"manifest_id": "manifest_demo_000044", "manifest_sha256": "a" * 64, "sequence": 44,
                                        "position": other_position, "high_water_mark": other_position}},
                "serve": base["base_receipt_id"] if serve_head else "receipt_demo_000044", "paused": paused}
        answers = {"unsigned_422": UNSIGNED_422, "rebase": rules["base_resolution"][1]["answer"]}
        seen = set()
        for position in range(0, 4):
            for mark in range(position, position + 3):
                for served in (True, False):
                    for paused in (False, True):
                        for live in range(0, 7):
                            for identity in ("recorded", "unknown", "digest", "manifest_id", "sequence"):
                                request = {**base, "base_live_sequence": live}
                                if identity == "unknown":
                                    request["base_receipt_id"] = "receipt_demo_000099"
                                if identity == "digest":
                                    request["base_manifest_sha256"] = "0" * 64
                                if identity == "manifest_id":
                                    request["base_manifest_id"] = "manifest_demo_other"
                                if identity == "sequence":
                                    request["from_sequence"] = 44
                                answer = self.contract.resolve_base(rules, ledger(position, mark, served, paused), request)
                                label = (position, mark, served, paused, live, identity)
                                seen.add(json.dumps(answer, sort_keys=True))
                                if identity != "recorded":
                                    self.assertEqual(answer, UNSIGNED_422, label)
                                elif not served:
                                    # A superseded head is answered with a rebase whatever the position or a pending rotation.
                                    self.assertEqual(answer, answers["rebase"], label)
                                elif paused:
                                    self.assertEqual(answer, TRANSIENT, label)
                                elif live > mark:
                                    self.assertEqual(answer, UNSIGNED_422, label)
                                elif live > position:
                                    self.assertEqual(answer, {**TRANSIENT, "starts_rotation_outside_request": True}, label)
                                elif live == position:
                                    self.assertEqual(answer, {"status": 200, "kind": "no_op"}, label)
                                else:
                                    self.assertEqual(answer, {"status": 200, "kind": "physical_changed_set"}, label)
                                # Safety, stated without the table: a 200 is only ever given for the head to serve, with
                                # no rotation pending and a position the Hub holds; a 409 never for the head to serve.
                                if answer.get("status") == 200:
                                    self.assertTrue(served and not paused and live <= position and identity == "recorded", label)
                                if answer.get("status") == 409:
                                    self.assertTrue(not served and identity == "recorded", label)
                                if answer.get("transient_failure"):
                                    self.assertTrue(served and identity == "recorded", label)
        self.assertEqual(len(seen), 6)
        # A table that has lost a row, or names a condition the model does not know, is refused.
        with self.assertRaisesRegex(self.contract.ContractError, "unknown base resolution condition"):
            self.contract.resolve_base({"base_resolution": [{"when": "other", "answer": {}}]}, ledger(1, 1, True, False), base)
        with self.assertRaisesRegex(self.contract.ContractError, "base resolution is not total"):
            self.contract.resolve_base({"base_resolution": rules["base_resolution"][:2]}, ledger(1, 1, True, False), base)

    def test_a_backup_restore_turns_no_legitimate_base_into_a_dead_end(self):
        profile = json.loads((PROFILE / "profile.json").read_text())
        rules = profile["live_rules"]
        base = self.request
        # The restored ledger rolled head 43 back to position 3 and mark 5, and the Hub rotated to head 44 before resuming.
        restored = {"heads": {
            base["base_receipt_id"]: {"manifest_id": base["base_manifest_id"], "manifest_sha256": base["base_manifest_sha256"],
                                      "sequence": 43, "position": 3, "high_water_mark": 5},
            "receipt_demo_000044": {"manifest_id": "manifest_demo_000044", "manifest_sha256": "a" * 64, "sequence": 44,
                                    "position": 0, "high_water_mark": 0}},
            "serve": "receipt_demo_000044", "paused": False}
        # A client of the superseded head, above even the restored mark, is told the head changed: a signed rebase.
        for live in (2, 3, 5, 9, 4000):
            with self.subTest(live=live):
                self.assertEqual(self.contract.resolve_base(rules, restored, {**base, "base_live_sequence": live}),
                                 {"status": 409, "kind": "rebase_required", "reasons": REASONS})
        # A client on a head published after the backup is unknown to the restored ledger: the unsigned 422, from which
        # the profile defines the recovery, a full 1.5 bootstrap and then base_live_sequence 0.
        later = {**base, "base_receipt_id": "receipt_demo_000045", "base_manifest_id": "manifest_demo_000045",
                 "from_sequence": 45, "base_live_sequence": 4}
        self.assertEqual(self.contract.resolve_base(rules, restored, later), UNSIGNED_422)
        unknown = profile["error_responses"]["on_unknown_base_receipt"]
        self.assertEqual(unknown["client_action"].split("_then_")[0],
                         "drop_the_checkpoint_do_a_full_bootstrap_under_the_1_5_selector")
        self.assertTrue(unknown["client_action"].endswith("request_with_base_live_sequence_0"))

    def test_the_live_target_commitment_is_the_head_commitment_and_is_constant_for_the_head(self):
        previous = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-physical-changed-set.json")["receipt"]
        commitment = previous["target_raw_sha256"]
        # Head 43 is the target of the 1.4 example changed set, so its commitment is that receipt's value.
        self.assertEqual(previous["target"]["receipt_id"], self.request["base_receipt_id"])
        self.assertEqual(self.receipt["target_raw_sha256"], commitment)
        rules = json.loads((PROFILE / "profile.json").read_text())["live_rules"]
        self.assertEqual(rules["target_raw_sha256"], "the_head_own_commitment_constant_for_the_head")
        self.assertTrue(self.contract.verify_live_receipt(self.request, self.receipt, self.vehicle, self.head_raw,
                                                          head_commitment=commitment))
        later = self.edited(self.receipt, lambda value: value.update(from_live_sequence=1, to_live_sequence=2, live_sequence=2))
        self.assertTrue(self.contract.verify_live_receipt(self.current, later, self.vehicle, head_commitment=commitment))
        for label, request, receipt in (("first step", self.request, self.receipt), ("later step", self.current, later)):
            with self.subTest(step=label):
                other = self.edited(receipt, lambda value: value.update(target_raw_sha256="5" * 64))
                with self.assertRaisesRegex(self.contract.ContractError, "live target commitment mismatch"):
                    self.contract.verify_live_receipt(request, other, self.vehicle, head_commitment=commitment)
                # A client that bootstrapped does not hold the commitment, so the member is not compared.
                self.assertTrue(self.contract.verify_live_receipt(request, other, self.vehicle))
        text = " ".join(rules["prose"])
        for phrase in ("is the head's own all-history target commitment", "identical in every live step of the head",
                       "is bound by the signed (receipt_id, manifest_sha256, from_live_sequence, to_live_sequence)",
                       "a client is not required to reproduce it"):
            self.assertIn(phrase, text)

    def test_apply_split_fold_and_head_transition_rules_are_machine_readable(self):
        rules = json.loads((PROFILE / "profile.json").read_text())["live_rules"]
        self.assertEqual(rules["live_apply"], {
            "precondition": "stored_receipt_id_manifest_sha256_live_sequence_equal_base_and_from_live_sequence",
            "activates": ["receipt_id", "manifest_sha256", "to_live_sequence"],
            "replay_identity": ["receipt_id", "manifest_sha256", "to_live_sequence"],
            "inherited_replay_clause_applies": False})
        self.assertEqual(rules["live_over_limit"], "split_into_consecutive_live_steps")
        self.assertEqual(rules["live_fold_only_for"], "a_single_root_whose_closure_alone_exceeds_the_1_4_limits")
        self.assertEqual(rules["head_transition"], {"base": "head_packs_at_live_sequence_0", "tombstones_live_rows": False,
                                                    "carries_folded_live_rows_as": "changed"})
        text = " ".join(rules["prose"])
        for phrase in ("and then atomically activates (receipt_id, manifest_sha256, to_live_sequence)",
                       "the 1.4 apply clause that replay of an already activated target is idempotent does not apply",
                       "replay identity is (receipt_id, manifest_sha256, to_live_sequence)",
                       "is split into consecutive live steps", "a root is never split across steps",
                       "Only a single root whose closure alone exceeds the 1.4 limits is answered by folding",
                       "a live step is never answered with a 1.4 complete replacement of the same head",
                       "is computed from head N's packs at live_sequence 0", "never tombstones a live row",
                       "reaches no client as a tombstone",
                       "a 1.5 client above live_sequence 0 never receives a head transition"):
            self.assertIn(phrase, text)
        # The retired wording, which made the head transition tombstone a live duplicate, is gone.
        self.assertNotIn("may tombstone a live duplicate", text)
        self.assertNotIn("in favour of the imported row inside that import's own head transition", text)
        self.assertNotIn("folds the live rows into a new head and answers head_changed", text)
        # The wire key document is the 1.3.0 route and schema; the 1.4.0 key schema covers only the fixture key set.
        keys = json.loads((PROFILE / "profile.json").read_text())["signing_keys"]
        self.assertEqual((keys["wire_key_document"], keys["route"]),
                         ("../1.3.0/signing-keys.schema.json#/$defs/document", "/v1/vehicles/{vehicle_id}/sync/signing-keys"))
        self.assertIn("../1.3.0/signing-keys.schema.json", json.loads((PROFILE / "profile.json").read_text())["inherited_artifacts"])
        for phrase in ("profile_id hub-sync-v1@1.3.0", "validates against the 1.3.0 schema",
                       "validate no wire document", "The 1.5 OpenAPI defines no key route"):
            self.assertIn(phrase, keys["rule"])
        self.assertEqual(json.loads((ROOT / "profiles/hub-sync-v1/1.3.0/signing-keys.schema.json").read_text())
                         ["$defs"]["document"]["properties"]["profile_id"], {"const": "hub-sync-v1@1.3.0"})

    def test_cases_run_from_a_relocated_bundle_with_copied_siblings(self):
        # The retained 1.3.0 and 1.4.0 profiles are siblings of the bundle being checked, copied here, not linked.
        with tempfile.TemporaryDirectory() as directory:
            for name in ("1.3.0", "1.4.0", "1.5.0"):
                shutil.copytree(ROOT / "profiles/hub-sync-v1" / name, Path(directory) / name)
            root = Path(directory) / "1.5.0"
            self.assertEqual(self.contract.verify_bundle(root)["profile_id"], "hub-sync-v1@1.5.0")
            results = self.contract.run_cases(root)
        self.assertEqual(len(results), 15)
        self.assertEqual([item["case_id"] for item in results if not item["passed"]], [])
        self.assertTrue(all(item["errors"] or not item["expected_errors"] for item in results))

    def test_the_chain_is_checked_against_the_1_4_0_sibling_of_the_bundle_it_is_given(self):
        # A relocated bundle next to an altered copy of 1.4.0 must not pass because the committed 1.4.0 still matches
        # the digest. The copies are real copies (a symlink would alter the committed tree) and the altered line
        # belongs to a file the profile does not pin, so only the chain digest can tell.
        with tempfile.TemporaryDirectory() as directory:
            for name in ("1.3.0", "1.4.0", "1.5.0"):
                shutil.copytree(ROOT / "profiles/hub-sync-v1" / name, Path(directory) / name)
            root = Path(directory) / "1.5.0"
            self.assertEqual(self.contract.verify_bundle(root)["profile_id"], "hub-sync-v1@1.5.0")
            profile = json.loads((root / "profile.json").read_text())
            self.assertNotIn("../1.4.0/cases.json", profile["inherited_artifacts"])
            committed = hashlib.sha256((PREVIOUS / "SHA256SUMS").read_bytes()).hexdigest()
            self.assertEqual(profile["previous_profile_sha256"], committed)
            sums = Path(directory) / "1.4.0/SHA256SUMS"
            lines = sums.read_text().splitlines()
            index = next(i for i, line in enumerate(lines) if line.endswith("  cases.json"))
            lines[index] = "0" * 64 + "  cases.json"
            sums.write_text("\n".join(lines) + "\n")
            self.assertNotEqual(hashlib.sha256(sums.read_bytes()).hexdigest(), committed)
            self.assertFalse(Path(directory, "1.4.0").is_symlink())
            with self.assertRaisesRegex(self.contract.ContractError, "previous profile digest mismatch"):
                self.contract.verify_bundle(root)
        # The committed tree is untouched by the altered copy.
        self.assertEqual(hashlib.sha256((PREVIOUS / "SHA256SUMS").read_bytes()).hexdigest(), committed)
        self.assertEqual(self.contract.verify_bundle()["profile_id"], "hub-sync-v1@1.5.0")

    # Byte bounds

    def test_request_bound_is_8192_bytes_including_base_live_sequence(self):
        def largest(with_live):
            request = copy.deepcopy(self.request)
            request.update(base_manifest_id="b" * 4096, base_receipt_id="r" * 4096, from_sequence=MAX_INTEGER)
            request["source"].update(generation=MAX_INTEGER, selected_car_id=MAX_INTEGER)
            if with_live:
                request["base_live_sequence"] = MAX_INTEGER
            else:
                del request["base_live_sequence"]
            excess = len(encode(request)) - 8192
            self.assertGreater(excess, 0)
            request["base_receipt_id"] = request["base_receipt_id"][:-excess]
            self.assertEqual(len(encode(request)), 8192)
            return request
        exact = largest(True)
        raw = encode(exact)
        self.assertTrue(self.contract.validate_request_body(raw))
        with self.assertRaisesRegex(self.contract.ContractError, "request exceeds byte limit"):
            self.contract.validate_request_body(raw + b" ")
        # A 1.4-sized request that fills the bound exactly has no room for the new member.
        legacy_sized = largest(False)
        self.assertEqual(len(encode(legacy_sized)), 8192)
        legacy_sized["base_live_sequence"] = 0
        with self.assertRaisesRegex(self.contract.ContractError, "request exceeds byte limit"):
            self.contract.validate_request(legacy_sized)
        # Raw whitespace and numeric spellings count before parsing.
        padded = encode(self.request)
        self.assertTrue(self.contract.validate_request_body(padded + b" " * (8192 - len(padded))))
        with self.assertRaisesRegex(self.contract.ContractError, "request exceeds byte limit"):
            self.contract.validate_request_body(padded + b" " * (8192 - len(padded) + 1))
        with self.assertRaisesRegex(self.contract.ContractError, "duplicate JSON member|not valid JSON"):
            self.contract.validate_request_body(b'{"base_live_sequence":0,' + encode(self.request)[1:])

    def test_raw_control_bodies_are_bounded_before_parsing(self):
        receipt_raw, no_op_raw, hint_raw = encode(self.receipt), encode(self.no_op), encode(self.head_changed)
        request_raw, current_raw, stale_raw = encode(self.request), encode(self.current), encode(self.stale)
        vehicle = self.vehicle
        self.contract.verify_live_receipt_body(request_raw, receipt_raw, vehicle, self.head_raw)
        self.contract.verify_no_op_body(current_raw, no_op_raw, vehicle)
        self.assertTrue(self.contract.verify_rebase_body(stale_raw, hint_raw, vehicle, self.head_raw))
        for raw, call in (
                (receipt_raw, lambda value: self.contract.verify_live_receipt_body(request_raw, value, vehicle)),
                (no_op_raw, lambda value: self.contract.verify_no_op_body(current_raw, value, vehicle)),
                (hint_raw, lambda value: self.contract.verify_rebase_body(stale_raw, value, vehicle, self.head_raw))):
            self.assertTrue(call(raw + b" " * (2097152 - len(raw))))
            with self.assertRaisesRegex(self.contract.ContractError, "exceeds byte limit"):
                call(raw + b" " * (2097152 - len(raw) + 1))
        with self.assertRaisesRegex(self.contract.ContractError, "exceeds byte limit"):
            self.contract.verify_manifest(self.head_raw + b" " * 2097152, vehicle)
        oversized = copy.deepcopy(self.head_changed)
        oversized["retry_request"].update(base_manifest_id="x" * 4096, base_receipt_id="y" * 4096)
        with self.assertRaisesRegex(self.contract.ContractError, "request exceeds byte limit"):
            self.contract.verify_rebase(self.stale, oversized, self.vehicle, self.head_raw)

    def test_largest_jointly_admitted_rebase_and_exact_retry_with_live_members(self):
        request = copy.deepcopy(self.stale)
        request.update(base_manifest_id="b" * 4096, base_receipt_id="r" * 4096,
                       from_sequence=MAX_INTEGER - 1, base_live_sequence=MAX_INTEGER)
        request["source"].update(generation=MAX_INTEGER, selected_car_id=MAX_INTEGER)
        excess = len(encode(request)) - 8192
        request["base_receipt_id"] = request["base_receipt_id"][:-excess]
        self.assertEqual(len(encode(request)), 8192)
        hint = copy.deepcopy(self.head_changed)
        hint["requested_base"].update(manifest_id=request["base_manifest_id"], receipt_id=request["base_receipt_id"],
                                      sequence=request["from_sequence"])
        hint["requested_live_sequence"] = request["base_live_sequence"]
        hint["replacement"].update(manifest_id="m" * 4096, receipt_id="t" * len(request["base_receipt_id"]), sequence=MAX_INTEGER)
        hint["replacement"]["chunks"] = [
            {"chunk_index": index, "pack": {"object_name": f"{index:04d}" + "p" * 1020, "sha256": "f" * 64,
                                            "compressed_bytes": 16777216}} for index in range(1771)]
        hint["retry_request"] = {**request, "base_manifest_id": hint["replacement"]["manifest_id"],
                                 "base_receipt_id": hint["replacement"]["receipt_id"],
                                 "from_sequence": hint["replacement"]["sequence"], "base_live_sequence": 0}
        excess = len(encode(hint)) - 2097152
        self.assertGreater(excess, 0, "individual maxima must exercise the joint bound")
        for chunk in reversed(hint["replacement"]["chunks"]):
            reduction = min(excess, len(chunk["pack"]["object_name"]) - 1)
            if reduction:
                chunk["pack"]["object_name"] = chunk["pack"]["object_name"][:-reduction]
                excess -= reduction
            if not excess:
                break
        self.assertEqual(excess, 0)
        child = self.fixture_signed({
            "manifest_id": hint["replacement"]["manifest_id"], "receipt_id": hint["replacement"]["receipt_id"],
            "vehicle_id": hint["vehicle_id"], "kind": "snapshot", "schema_version": "2.2",
            "sequence": hint["replacement"]["sequence"], "live_sequence": 0, "chunks": hint["replacement"]["chunks"]})
        child_raw = encode(child)
        digest = hashlib.sha256(child_raw).hexdigest()
        hint["replacement"]["manifest_sha256"] = digest
        hint["retry_request"]["base_manifest_sha256"] = digest
        hint = self.fixture_signed(hint)
        hint_raw = encode(hint)
        self.assertEqual(len(hint["replacement"]["chunks"]), 1771)
        self.assertEqual(len(hint_raw), 2097152)
        self.assertLessEqual(len(encode(hint["retry_request"])), 8192)
        self.assertLessEqual(len(child_raw), 2097152)
        self.assertTrue(self.contract.verify_rebase_body(encode(request), hint_raw, hint["vehicle_id"], child_raw))
        self.assertTrue(self.contract.validate_request_body(encode(hint["retry_request"])))
        with self.assertRaisesRegex(self.contract.ContractError, "rebase exceeds byte limit"):
            self.contract.verify_rebase_body(encode(request), hint_raw + b" ", hint["vehicle_id"], child_raw)

    # Pack reference and derived live pack

    def derived_live_pack(self, edit=None, keep_tombstone=False, keep_open=False):
        """A live pack derived at run time from the committed 1.4 pack; never written to the tree.

        The 1.4 pack carries a tombstone and an open charge root; a live pack has neither unless asked to keep them.
        """
        raw = zstandard.ZstdDecompressor().decompress((PREVIOUS / "packs/physical-changed-set-v1.sqlite.zst").read_bytes())
        receipt = copy.deepcopy(self.receipt)
        head = receipt["base"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "live.sqlite"
            path.write_bytes(raw)
            connection = sqlite3.connect(path)
            try:
                for side in ("base", "target"):
                    for key in ("manifest_id", "receipt_id", "manifest_sha256", "sequence"):
                        connection.execute("UPDATE delta_metadata SET value=? WHERE key=?", (str(head[key]), f"{side}_{key}"))
                if not keep_tombstone:
                    connection.execute("DELETE FROM tombstones")
                    connection.execute("DELETE FROM affected_projected_ids WHERE table_name='car_states'")
                if not keep_open:
                    connection.execute("UPDATE charging_processes SET end_date_pg_us=2000000 WHERE id=30")
                    connection.execute("UPDATE impacted_roots SET target_state='closed' WHERE root_type='charge' AND root_id=30")
                if edit:
                    edit(connection)
                receipt["total_rows"] = (connection.execute("SELECT COUNT(*) FROM row_roles").fetchone()[0]
                                         + connection.execute("SELECT COUNT(*) FROM tombstones").fetchone()[0])
                for table, columns, prefix in (
                        ("affected_projected_ids", "table_name, entity_id, effect", "affected_projected_ids"),
                        ("impacted_roots", "root_type, root_id, base_state, target_state, target_child_count", "impacted_roots")):
                    rows = list(connection.execute(f"SELECT {columns} FROM {table}"))
                    receipt[prefix + "_count"] = len(rows)
                    receipt[prefix + "_sha256"] = self.contract.inherited._rows_digest(rows)
                connection.commit()
            finally:
                connection.close()
            raw = path.read_bytes()
        compressed = zstandard.ZstdCompressor(write_checksum=True).compress(raw)
        receipt["chunks"][0]["pack"].update(sha256=hashlib.sha256(compressed).hexdigest(), compressed_bytes=len(compressed))
        receipt["total_compressed_bytes"], receipt["total_uncompressed_bytes"] = len(compressed), len(raw)
        return self.fixture_signed(receipt), compressed

    def test_example_pack_reference_is_the_committed_14_descriptor_and_a_placeholder(self):
        descriptor = self.receipt["chunks"][0]["pack"]
        pack = (PREVIOUS / "packs/physical-changed-set-v1.sqlite.zst").read_bytes()
        self.assertEqual(descriptor, {"object_name": "packs/physical-changed-set-v1.sqlite.zst",
                                      "sha256": hashlib.sha256(pack).hexdigest(), "compressed_bytes": len(pack)})
        listed = dict(line.split("  ", 1)[::-1] for line in (PREVIOUS / "SHA256SUMS").read_text().splitlines())
        self.assertEqual(listed["packs/physical-changed-set-v1.sqlite.zst"], descriptor["sha256"])
        previous = self.contract.inherited.read_json(PREVIOUS / "examples/changes-since-physical-changed-set.json")["receipt"]
        for key in ("total_rows", "total_compressed_bytes", "total_uncompressed_bytes", "affected_projected_ids_sha256",
                    "impacted_roots_sha256", "affected_months", "target_raw_sha256"):
            self.assertEqual(self.receipt[key], previous[key], key)
        self.assertIn("placeholder", json.loads((PROFILE / "profile.json").read_text())["live_pack"]["fixture_status"])
        # Those bytes bind head 42 to head 43 and carry a tombstone and an open charge root: not a valid live pack.
        with self.assertRaisesRegex(self.contract.ContractError, "physical pack binding mismatch"):
            self.contract.verify_live_pack(self.receipt, pack)

    def test_derived_live_pack_has_equal_head_sides_and_never_deletes(self):
        receipt, compressed = self.derived_live_pack()
        self.assertTrue(self.contract.verify_live_receipt(self.request, receipt, self.vehicle, self.head_raw))
        self.assertTrue(self.contract.verify_live_pack(receipt, compressed))
        deleting, compressed = self.derived_live_pack(keep_tombstone=True)
        self.assertTrue(self.contract.verify_live_receipt(self.request, deleting, self.vehicle))
        self.assertTrue(self.contract.inherited.verify_pack(deleting, compressed, PREVIOUS), "valid 1.4 pack, but it deletes")
        with self.assertRaisesRegex(self.contract.ContractError, "live changed set deletes rows"):
            self.contract.verify_live_pack(deleting, compressed)
        moving = self.edited(receipt, lambda value: value["target"].update(sequence=44))
        with self.assertRaisesRegex(self.contract.ContractError, "live target is not the base head"):
            self.contract.verify_live_pack(moving, compressed)
        # An open root could later need a delete, so only closed roots are published live. The 1.4 checker accepts both.
        opened, compressed = self.derived_live_pack(keep_open=True)
        self.assertTrue(self.contract.verify_live_receipt(self.request, opened, self.vehicle))
        self.assertTrue(self.contract.inherited.verify_pack(opened, compressed, PREVIOUS), "valid 1.4 pack, but a root is open")
        with self.assertRaisesRegex(self.contract.ContractError, "live changed set publishes an open or absent root"):
            self.contract.verify_live_pack(opened, compressed)
        absent, compressed = self.derived_live_pack(lambda connection: connection.execute(
            "INSERT INTO impacted_roots VALUES ('drive', 99, 'absent', 'absent', 0)"))
        self.assertTrue(self.contract.inherited.verify_pack(absent, compressed, PREVIOUS), "valid 1.4 pack, but a root is absent")
        with self.assertRaisesRegex(self.contract.ContractError, "live changed set publishes an open or absent root"):
            self.contract.verify_live_pack(absent, compressed)
        roots = self.derived_live_pack()
        self.assertTrue(self.contract.verify_live_pack(*roots))
        stale_binding, stale_pack = self.derived_live_pack(lambda connection: connection.execute(
            "UPDATE delta_metadata SET value='42' WHERE key='target_sequence'"))
        with self.assertRaisesRegex(self.contract.ContractError, "physical pack binding mismatch"):
            self.contract.verify_live_pack(stale_binding, stale_pack)

    # Profile document and prose

    def test_docs_say_the_profile_is_a_candidate_that_no_hub_serves_and_state_the_new_rules(self):
        def squashed(path):
            return " ".join((ROOT / path).read_text().split())
        readme, doc = squashed("README.md"), squashed("docs/reference/hub-sync.md")
        self.assertIn("The candidate `hub-sync-v1@1.5.0` adds a live overlay", readme)
        self.assertIn("`sync.live` capability) to a PhysicalV3 head; no Hub serves it yet.", readme)
        self.assertIn("`profiles/hub-sync-v1/1.5.0/` — candidate live overlay", readme)
        self.assertIn("is a candidate that adds a live overlay to a PhysicalV3 head. No Hub serves it yet. A Hub lists the "
                      "discovery capability `sync.live` if and only if it serves the overlay and the discovery profile it "
                      "names defines that token; a Hub that does not serve the overlay answers the 1.5 selector with an "
                      "empty `406`.", doc)
        self.assertIn(DOC_CAPABILITY_SENTENCE, doc)
        self.assertIn(DOC_RETENTION_SENTENCE, doc)
        self.assertIn(DOC_WATERMARK_SENTENCE, doc)
        for phrase in ("the highest `live_sequence` it ever signed for it (the high-water mark)",
                       "`base_live_sequence` above the head's high-water mark fails closed as the unsigned `422 unknown_base_receipt`",
                       "first match wins", "detected a regression it did not rotate on",
                       "never builds the replacement inside the request, because a fold is O(history)",
                       "above the high-water mark of the head to serve is not recorded by this Hub as issued and is answered "
                       "at once, without rebuilding anything, with the unsigned `422`",
                       "so a client cannot make a Hub rotate by naming a position it holds no record of issuing",
                       "only closed impacted roots", "A synthesised stationary position for a live charge is an ordinary `changed` row",
                       "compares the identity members alone", "keeps its materialised state",
                       "does a full 1.4 bootstrap", "Error statuses are inherited from 1.3.0 and are unsigned",
                       "whatever its `base_live_sequence`", "a head that already exists, so answering costs no rebuild",
                       "A client answered that way drops its checkpoint, does a full bootstrap under the 1.5 selector and then "
                       "requests with `base_live_sequence` 0; it never retries the same request",
                       "then whether the head is still the one to serve, then a paused rotation, then the high-water mark",
                       "a head the restored ledger does not record is answered with the unsigned `422`",
                       "it answers with a transient failure that this profile does not define, starts the rotation outside the "
                       "request and pauses live service for the head until the rotation completes",
                       "A `409` `live_ahead` therefore names a replacement that already exists",
                       "is split into consecutive live steps", "Only a single root whose closure alone exceeds the 1.4 limits",
                       "a live step is never answered with a replacement of the same head",
                       "and then atomically activates `(receipt_id, manifest_sha256, to_live_sequence)`",
                       "the 1.4 rule that replay of an already activated target is idempotent does not apply",
                       "is the same in every live step of the head",
                       "is computed from head N's packs at `live_sequence` 0, because a 1.4 client never holds live rows",
                       "never tombstones a live row", "never receives a head transition and always receives a complete replacement",
                       "A client that reached its head under 1.5 has no kept 1.4 checkpoint",
                       "The wire key document is the 1.3.0 signing-keys route and schema"):
            self.assertIn(phrase, doc)
        for retired in ("never signed by this Hub", "position it never issued", "that replacement inside the request",
                        "tombstone a live duplicate", "answers `live_ahead` with a replacement head"):
            self.assertNotIn(retired, doc)
        self.assertNotIn("staged packs", doc)

    def test_profile_states_the_live_rules_and_the_capability_in_machine_readable_keys(self):
        profile = json.loads((PROFILE / "profile.json").read_text())
        rules = profile["live_rules"]
        self.assertEqual(rules["request_member"], "base_live_sequence")
        self.assertIs(rules["request_member_required"], True)
        self.assertEqual(rules["receipt_members"], ["live_sequence", "from_live_sequence", "to_live_sequence"])
        self.assertEqual((rules["manifest_member"], rules["manifest_live_sequence"]), ("live_sequence", 0))
        self.assertEqual((rules["live_step"], rules["bootstrap_base_live_sequence"], rules["rebase_retry_base_live_sequence"]), (1, 0, 0))
        self.assertIs(rules["target_equals_base"], True)
        self.assertIs(rules["live_changed_sets_delete"], False)
        self.assertIs(rules["live_roots_closed_only"], True)
        self.assertEqual(rules["rebase_reasons"], REASONS)
        pack = profile["live_pack"]
        for phrase in ("tombstones table is empty", "every row of its impacted_roots table has target_state closed",
                       "ordinary changed row"):
            self.assertIn(phrase, pack["binding"])
        self.assertIn("an open charge root", pack["fixture_status"])
        self.assertIs(rules["rebase_reason_is_diagnostic"], True)
        self.assertEqual(profile["capability"]["token"], "sync.live")
        self.assertIn("if and only if it serves", profile["capability"]["rule"])
        self.assertIn("MUST NOT advertise", profile["capability"]["rule"])
        self.assertEqual(profile["bootstrap_selector"]["value"], "hub-sync-v1@1.5.0")
        self.assertEqual(profile["bootstrap_selector"]["required_schema_value"], "2.1,2.2")
        self.assertEqual(profile["changed_set_format"], "teslatlas-physical-v3-delta-v1")
        for key in ("request", "receipt", "hint"):
            schema = json.loads((PROFILE / {"request": "changes-since-request.schema.json",
                                            "receipt": "physical-changed-set-receipt.schema.json",
                                            "hint": "physical-rebase-hint.schema.json"}[key]).read_text())
            definition = schema["$defs"][key]
            self.assertIs(definition["additionalProperties"], False)
            self.assertEqual(schema["$id"], f"urn:teslatlas:hub-sync-v1:1.5.0:{key}")
        text = " ".join(rules["prose"])
        for phrase in ("exact head", "to_live_sequence equals from_live_sequence plus 1", "None of them produces an error",
                       "immediately requests the overlay with base_live_sequence 0", "Live changed sets never delete",
                       "Every impacted root of a live changed set has target_state closed",
                       "only closed drives and charges are published live", "an open or absent root is never a live root",
                       "A client rejects a live pack with an open or absent impacted root",
                       "Live changed sets never delete: their packs carry no tombstones."):
            self.assertIn(phrase, text)

    # Live-journal retention and the capability qualification

    def squashed_text(self, relative):
        return " ".join((ROOT / relative).read_text().split())

    def test_the_live_journal_retention_obligation_is_machine_readable_and_pinned(self):
        # A Hub that prunes the journal by a watermark over paired devices breaks base resolution row 7 for every
        # client that starts at live_sequence 0 (a new, reinstalled or rebased device and a 1.4 upgrade), so the
        # obligation is a key, a profile sentence and a reference sentence, each pinned.
        rules = json.loads((PROFILE / "profile.json").read_text())["live_rules"]
        self.assertEqual(rules.get("live_journal_retention"), RETENTION_KEY)
        prose = rules["prose"]
        for sentence in (PROFILE_RETENTION_SENTENCE, PROFILE_WATERMARK_SENTENCE):
            with self.subTest(sentence=sentence[:40]):
                self.assertEqual(" ".join(prose).count(sentence), 1)
        # Both sentences are one prose entry, and only that entry states the obligation.
        entries = [item for item in prose if PROFILE_RETENTION_SENTENCE in item]
        self.assertEqual(len(entries), 1)
        self.assertIn(PROFILE_WATERMARK_SENTENCE, entries[0])
        self.assertEqual([item for item in prose if "live_sequence 1" in item and "MUST retain" in item], entries)
        doc = self.squashed_text("docs/reference/hub-sync.md")
        for sentence in (DOC_RETENTION_SENTENCE, DOC_WATERMARK_SENTENCE):
            with self.subTest(sentence=sentence[:40]):
                self.assertEqual(doc.count(sentence), 1)
        self.assertLess(doc.index(DOC_RETENTION_SENTENCE), doc.index(DOC_WATERMARK_SENTENCE))
        # The obligation backs the retention rebase: it is the same reason the profile and the reference name.
        self.assertIn("live_retention", rules["rebase_reasons"])
        self.assertIn("live_retention after a fold that pruned the live journal of the requested head", " ".join(prose))

    def test_sync_live_is_advertised_only_under_a_discovery_profile_that_defines_it(self):
        profile = json.loads((PROFILE / "profile.json").read_text())
        capability = profile["capability"]
        self.assertEqual({key: value for key, value in capability.items() if key != "rule"}, {
            "token": "sync.live",
            "advertised_only_under_a_discovery_profile_that_defines_it": True,
            "discovery_profiles_without_the_token": ["hub-http-v1@1.1.0", "hub-http-v1@1.2.0"],
            "when_the_discovery_profile_lacks_the_token": "do_not_advertise",
            "token_added_by": "a_later_hub_http_v1_minor",
            "adjusted_when_the_token_is_added": "the_hub_http_v1_1_1_0_actual_hub_matrix_exact_capability_equality_check"})
        for sentence in PROFILE_CAPABILITY_SENTENCES:
            with self.subTest(sentence=sentence[:48]):
                self.assertEqual(capability["rule"].count(sentence), 1)
        doc = self.squashed_text("docs/reference/hub-sync.md")
        self.assertEqual(doc.count(DOC_CAPABILITY_SENTENCE), 1)
        # The statement is true of the committed discovery profiles, which this profile does not edit.
        for identity in capability["discovery_profiles_without_the_token"]:
            with self.subTest(discovery_profile=identity):
                if identity == "hub-http-v1@1.1.0":
                    # The tree was retired in P2; the digest-bound identity text above stays under D-P4.
                    continue
                family, _, version = identity.partition("@")
                document = json.loads((ROOT / "profiles" / family / version / "profile.json").read_text())
                self.assertEqual(document["profile_id"], identity)
                self.assertNotIn(capability["token"], document["capabilities"])
        # And the conflict it avoids is real: hub-http-v1@1.2.0 conformance refuses a token outside its list.
        http = importlib.import_module("conformance.hub_http_12")
        http_profile = json.loads((ROOT / "profiles/hub-http-v1/1.2.0/profile.json").read_text())
        discovery = json.loads((ROOT / "profiles/hub-http-v1/1.2.0/examples/discovery.json").read_text())
        tokens = http_profile["capabilities"]
        self.assertEqual(http.check_discovery(discovery, profile_id=http_profile["profile_id"], tokens=tokens), [])
        advertised = {**discovery, "capabilities": [*discovery["capabilities"], capability["token"]]}
        self.assertEqual(http.check_discovery(advertised, profile_id=http_profile["profile_id"], tokens=tokens),
                         ["discovery advertises a token outside the profile"])


if __name__ == "__main__":
    unittest.main()

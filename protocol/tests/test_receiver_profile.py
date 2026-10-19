"""Independent receiver-v1 contract tests; no Hub imports."""

import copy
import hashlib
import json
from pathlib import Path
import re
import unittest

from jsonschema import Draft202012Validator


ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/receiver-v1/1.0.0"
PROFILE_ID = "receiver-v1@1.0.0"

# Pinned from the retired envelope contract before it was removed; the
# profile must keep these constraints without needing the retired files.
PIN_SCHEMA = "d6768c18ff3e8c8aeae8a4e7a79181141be6f0bd0f81b27d974eb81fc3bd2847"
PIN_GROUPS = {
    "identity": "d4409b7300a2a3a3f218d1eff484222b4cb2a01c0594c2791ec984e66cf0a3a3",
    "sequence": "4e079d349787f9a5b7aeed00edf093a375a9e4e2fbbf131008702ae369acc746",
    "unicode_key_order": "9ff44b0e0211292007ad34911f956ab211274c05628fe242a43978e5c54a6db3",
    "non_projection_envelopes": "4063e7e622dfc42d031b85674e6de430f00bbbb24f60efe7dd8e600b94e45062",
    "negative_cases": "43c86bb1bb4f77f5e0909d2275125a3f441ce44e0b3e8a2f2dbbb79ddb547bb3",
}
ORIGINAL_NEGATIVE_COUNT = 12
BASE_ID = "8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9"
BASE_ALIAS = "ac89a19968e0d88fe632e2cf59046dd333213da1e4ecd34f97430341bc70a0bd"
ACCEPTED_TX_TYPES = {"v", "data", "vehicledata", "connectivity", "alerts", "errors"}
EVENT_CLOCK_SLACK_MS = 300000


def pin_digest(value):
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False
        ).encode("utf-8")
    ).hexdigest()


def fixture_canonical(value):
    # Bounded integer/string serializer with UTF-8 key order.
    if isinstance(value, dict):
        return b"{" + b",".join(
            fixture_canonical(key) + b":" + fixture_canonical(value[key])
            for key in sorted(value, key=lambda item: item.encode("utf-8"))
        ) + b"}"
    if isinstance(value, list):
        return b"[" + b",".join(fixture_canonical(item) for item in value) + b"]"
    if value is None or type(value) in (str, bool, int):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    raise TypeError("fixture canonicalizer supports only the bounded integer/string corpus")


def normalise_tx_type(value):
    return re.sub(r"[^A-Za-z0-9]", "", value).lower()


def profile_rules(envelope):
    """Annotated rules a conforming receiver enforces beyond the schema."""
    if normalise_tx_type(envelope["tx_type"]) not in ACCEPTED_TX_TYPES:
        return "accepted-tx-type"
    if envelope["timestamp_ms"] > envelope["received_at_ms"] + EVENT_CLOCK_SLACK_MS:
        return "event-clock"
    return None


def load(name):
    return json.loads((PROFILE / name).read_text())


class ReceiverProfileTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.profile = load("profile.json")
        cls.vectors = load("vectors.json")
        cls.schema = load("receiver-envelope.schema.json")
        cls.validator = Draft202012Validator(cls.schema)
        cls.examples = {
            path.stem: json.loads(path.read_text())
            for path in sorted((PROFILE / "examples").glob("*.json"))
        }

    def test_bundle_matches_sha256sums(self):
        files = sorted(
            str(path.relative_to(PROFILE))
            for path in PROFILE.rglob("*")
            if path.is_file() and path.name not in ("SHA256SUMS", ".DS_Store")
        )
        listed = {}
        for line in (PROFILE / "SHA256SUMS").read_text().splitlines():
            digest, name = line.split("  ", 1)
            listed[name] = digest
        self.assertEqual(sorted(listed), files)
        for name, digest in listed.items():
            with self.subTest(name=name):
                self.assertEqual(hashlib.sha256((PROFILE / name).read_bytes()).hexdigest(), digest)
        self.assertEqual(list(listed), sorted(listed))

    def test_profile_identity_and_route(self):
        profile = self.profile
        self.assertEqual(profile["profile_id"], PROFILE_ID)
        self.assertEqual(profile["profile_name"], "receiver-v1")
        self.assertEqual(profile["revision"], "1.0.0")
        self.assertEqual(profile["status"], "served")
        self.assertEqual(
            profile["routes"],
            [{"method": "POST", "path": "/v1/internal/fleet-telemetry", "success_status": 204}],
        )
        self.assertEqual(profile["receiver_body_bytes"], 262144)
        self.assertEqual(profile["schemas"], ["receiver-envelope.schema.json"])
        self.assertEqual(
            [item["kind"] for item in profile["examples"]], ["receiver-envelope"] * 3
        )
        self.assertIn(
            "A conforming receiver enforces these rules; JSON Schema validation alone does not.",
            json.dumps(profile),
        )
        self.assertEqual(self.schema["x-teslatlas-event-clock-rule"],
                         "timestamp_ms <= received_at_ms + 300000")
        self.assertEqual(
            self.schema["properties"]["tx_type"]["x-teslatlas-accepted-normalized-values"],
            ["v", "data", "vehicledata", "connectivity", "alerts", "errors"],
        )
        self.assertEqual(self.vectors["profile_id"], PROFILE_ID)
        for retired in ("batches", "acks", "delivery", "selection", "spool_format", "sequence"):
            self.assertNotIn(retired, profile)
        self.assertNotIn("Edge", json.dumps(profile["routes"]))

    def test_schema_constraints_equal_the_retired_envelope(self):
        schema = {k: v for k, v in self.schema.items() if k not in ("$id", "title")}
        self.assertEqual(pin_digest(schema), PIN_SCHEMA)
        self.assertEqual(
            self.schema["$id"],
            "https://protocol.teslatlas.example/profiles/receiver-v1/1.0.0/"
            "receiver-envelope.schema.json",
        )
        self.assertEqual(self.schema["title"], "Receiver envelope")
        Draft202012Validator.check_schema(self.schema)

    def test_carried_groups_are_verbatim(self):
        for group in ("identity", "sequence", "unicode_key_order", "non_projection_envelopes"):
            with self.subTest(group=group):
                self.assertEqual(pin_digest(self.vectors[group]), PIN_GROUPS[group])
        negatives = self.vectors["negative_cases"]
        self.assertEqual(len(negatives), ORIGINAL_NEGATIVE_COUNT + 1)
        self.assertEqual(pin_digest(negatives[:ORIGINAL_NEGATIVE_COUNT]), PIN_GROUPS["negative_cases"])
        added = negatives[ORIGINAL_NEGATIVE_COUNT]
        self.assertEqual(added["expected_status"], 400)
        self.assertEqual(added["envelope"]["vin"], added["envelope"]["vin"].lower())
        self.assertFalse(list(self.validator.iter_errors(added["envelope"])) == [])

    def test_carried_groups_are_marked_historical(self):
        carried = self.profile["carried_vectors"]
        self.assertEqual(carried["status"], "historical-informative")
        self.assertEqual(
            carried["groups"],
            ["identity", "negative_cases", "non_projection_envelopes", "sequence", "unicode_key_order"],
        )
        for group in carried["groups"]:
            self.assertIn(group, self.vectors)
        # The conformance corpus is the receiver-only group, never a carried one.
        self.assertEqual(carried["normative_groups"], ["receiver_negative_envelopes"])
        self.assertIn("receiver_negative_envelopes", self.vectors)
        self.assertTrue(set(carried["normative_groups"]).isdisjoint(carried["groups"]))

    def test_positive_envelopes_validate(self):
        self.assertEqual(
            sorted(self.examples), ["alert-envelope", "error-envelope", "projected-envelope"]
        )
        candidates = dict(self.examples)
        for index, item in enumerate(self.vectors["non_projection_envelopes"]):
            candidates[f"non-projection-{index}"] = item["envelope"]
        candidates["identity-base"] = self.vectors["identity"]["base_envelope"]
        candidates["identity-changed-arrival"] = self.vectors["identity"]["changed_arrival_envelope"]
        candidates["identity-changed-payload"] = self.vectors["identity"]["changed_payload_envelope"]
        candidates["unicode"] = self.vectors["unicode_key_order"]["envelope"]
        for index, item in enumerate(self.vectors["sequence"]["batch"]["records"]):
            candidates[f"sequence-record-{index}"] = item["envelope"]
        for name, envelope in candidates.items():
            with self.subTest(name=name):
                self.assertEqual(list(self.validator.iter_errors(envelope)), [])
                self.assertIsNone(profile_rules(envelope))

    def test_negative_envelopes_are_refused(self):
        entries = self.vectors["receiver_negative_envelopes"]
        ids = [entry["id"] for entry in entries]
        self.assertEqual(len(ids), len(set(ids)))
        self.assertEqual(len(entries), 14)
        for entry in entries:
            with self.subTest(id=entry["id"]):
                self.assertEqual(entry["expected_status"], 400)
                errors = list(self.validator.iter_errors(entry["envelope"]))
                if entry["schema_valid"]:
                    self.assertEqual(errors, [])
                    self.assertEqual(profile_rules(entry["envelope"]), entry["rule"])
                else:
                    self.assertEqual(entry["rule"], "schema")
                    self.assertNotEqual(errors, [])
        by_id = {entry["id"]: entry for entry in entries}
        clock = by_id["event-clock-beyond-five-minutes"]["envelope"]
        self.assertEqual(clock["timestamp_ms"], clock["received_at_ms"] + 300001)
        on_edge = copy.deepcopy(clock)
        on_edge["timestamp_ms"] = on_edge["received_at_ms"] + 300000
        self.assertIsNone(profile_rules(on_edge))
        self.assertEqual(list(self.validator.iter_errors(on_edge)), [])

    def test_identity_vectors_recompute(self):
        identity = self.vectors["identity"]
        domains = self.profile["hash_parameters"]["domains"]
        stable_domain = bytes.fromhex(domains["stable_record_id"]["bytes_hex"])
        legacy_domain = bytes.fromhex(domains["legacy_record_id"]["bytes_hex"])
        self.assertEqual(identity["stable_record_id"], BASE_ID)
        self.assertEqual(identity["legacy_record_id"], BASE_ALIAS)

        def stable_id(envelope):
            stable = copy.deepcopy(envelope)
            del stable["received_at_ms"]
            stable["version"] = 2
            return hashlib.sha256(stable_domain + fixture_canonical(stable)).hexdigest()

        def legacy_id(envelope):
            return hashlib.sha256(legacy_domain + fixture_canonical(envelope)).hexdigest()

        base = identity["base_envelope"]
        self.assertEqual(stable_id(base), identity["stable_record_id"])
        self.assertEqual(stable_id(identity["changed_arrival_envelope"]), BASE_ID)
        self.assertEqual(stable_id(identity["changed_payload_envelope"]),
                         identity["changed_payload_stable_record_id"])
        self.assertEqual(legacy_id(base), identity["legacy_record_id"])
        self.assertEqual(legacy_id(identity["changed_arrival_envelope"]),
                         identity["changed_arrival_legacy_record_id"])
        self.assertNotEqual(identity["legacy_record_id"], identity["changed_arrival_legacy_record_id"])
        unicode_vector = self.vectors["unicode_key_order"]
        self.assertEqual(
            fixture_canonical(unicode_vector["envelope"]),
            bytes.fromhex(unicode_vector["legacy_canonical_utf8_hex"]),
        )
        self.assertEqual(
            legacy_id(unicode_vector["envelope"]), unicode_vector["legacy_record_id"]
        )


if __name__ == "__main__":
    unittest.main()

"""Independent admission checks for the hub-sync-v1@1.5.0 live-overlay candidate.

Offline only: bundle closure and chain digest, closed schemas, signed examples,
and the live rules a client must enforce (exact head, contiguous live sequence,
one changed set per live step, live no-op currency, rebase retry to a newer head,
adoption of a 1.4 checkpoint, 1.4 compatibility). Unchanged 1.4.0
artefacts (fixture keys, pack contract, packs) are used from their own
directory, never from a copy. Errors are ContractError with stable substrings.
"""
import hashlib
import json
from pathlib import Path
import re
import sqlite3
import tempfile

from jsonschema import Draft202012Validator, FormatChecker

from conformance import hub_sync_14 as inherited

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.5.0"
PREVIOUS = ROOT / "profiles/hub-sync-v1/1.4.0"
PROFILE_ID = "hub-sync-v1@1.5.0"
PREVIOUS_ID = "hub-sync-v1@1.4.0"
MAX_REQUEST_BYTES = inherited.MAX_REQUEST_BYTES
MAX_CONTROL_BYTES = inherited.MAX_CONTROL_BYTES
LIVE_MEMBERS = ("base_live_sequence", "live_sequence", "from_live_sequence", "to_live_sequence",
                "requested_live_sequence")
# A 1.4 checkpoint from a bootstrap holds the whole manifest; one from a changed set holds the receipt's target
# (identity members only, no vehicle_id and no chunk list). The route vehicle binds vehicle_id in either case.
UPGRADE_IDENTITY = ("manifest_id", "receipt_id", "sequence", "schema_version")
UPGRADE_MEMBERS = ("vehicle_id", *UPGRADE_IDENTITY, "chunks")
ContractError = inherited.ContractError
read_json = inherited.read_json


def _violations(name, kind, value, root=PROFILE):
    schema = {**read_json(Path(root) / name), "$ref": "#/$defs/" + kind}
    return list(Draft202012Validator(schema, format_checker=FormatChecker()).iter_errors(value))


def _sums(root):
    listed = {}
    for line in (Path(root) / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        if not re.fullmatch("[0-9a-f]{64}", digest) or name in listed or ".." in Path(name).parts:
            raise ContractError("invalid profile manifest")
        listed[name] = digest
    return listed


def verify_bundle(root=PROFILE):
    root = Path(root)
    family = root.parent  # the retained 1.3.0 and 1.4.0 profiles are siblings of the 1.5.0 directory
    listed = _sums(root)
    for name, digest in listed.items():
        if hashlib.sha256((root / name).read_bytes()).hexdigest() != digest:
            raise ContractError("profile file digest mismatch")
    actual = {path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file() and path.name != "SHA256SUMS"}
    if set(listed) != actual:
        raise ContractError("profile manifest is incomplete")
    profile = read_json(root / "profile.json")
    if profile["profile_id"] != PROFILE_ID or profile["previous_profile"] != PREVIOUS_ID:
        raise ContractError("wrong profile")
    if hashlib.sha256((family / "1.4.0" / "SHA256SUMS").read_bytes()).hexdigest() != profile["previous_profile_sha256"]:
        raise ContractError("previous profile digest mismatch")
    # Referenced, not copied: each inherited artefact is pinned by digest and must still match its own profile's sums.
    for reference, digest in profile["inherited_artifacts"].items():
        directory, _, name = reference[3:].partition("/")
        if not reference.startswith("../") or not name or ".." in Path(name).parts or directory not in ("1.3.0", "1.4.0"):
            raise ContractError("inherited artefact digest mismatch")
        owner = family / directory
        if _sums(owner).get(name) != digest or hashlib.sha256((owner / name).read_bytes()).hexdigest() != digest:
            raise ContractError("inherited artefact digest mismatch")
    return profile


def validate_request(request, root=PROFILE):
    if _violations("changes-since-request.schema.json", "request", request, root):
        raise ContractError("request violates 1.5 schema")
    inherited._check_encoded_size(request, MAX_REQUEST_BYTES, "request")
    return request


def validate_request_body(raw, root=PROFILE):
    return validate_request(inherited._bounded_json(raw, MAX_REQUEST_BYTES, "request"), root)


def classify_request(request, root=PROFILE):
    """'live' for a 1.5 request, 'legacy' for a 1.4 request (which never receives an overlay)."""
    if not _violations("changes-since-request.schema.json", "request", request, root):
        return "live"
    if not _violations("changes-since-request.schema.json", "request", request, PREVIOUS):
        return "legacy"
    raise ContractError("request violates 1.5 schema")


def _signature(value, vehicle_id, label):
    # The 1.5 fixtures are signed with the 1.4.0 fixture keys, used from that directory.
    inherited._verify_fixture_signature(value, vehicle_id, PREVIOUS, label)


def verify_manifest(raw, vehicle_id, root=PROFILE, label="bootstrap manifest"):
    manifest = inherited._bounded_json(raw, MAX_CONTROL_BYTES, label)
    if _violations("live-overlay.schema.json", "manifest", manifest, root):
        raise ContractError(f"{label} violates live schema 2.2")
    if manifest["vehicle_id"] != vehicle_id:
        raise ContractError(f"{label} vehicle mismatch")
    if [item["chunk_index"] for item in manifest["chunks"]] != list(range(len(manifest["chunks"]))):
        raise ContractError(f"{label} chunks are not contiguous")
    _signature(manifest, vehicle_id, f"{label} signature")
    return manifest


def _bind_manifest(raw, manifest, checkpoint, label):
    if hashlib.sha256(raw).hexdigest() != checkpoint["manifest_sha256"]:
        raise ContractError(f"{label} signed manifest digest mismatch")
    if any(manifest[key] != checkpoint[key] for key in ("manifest_id", "receipt_id", "sequence", "schema_version")):
        raise ContractError(f"{label} signed manifest identity mismatch")


def verify_head_manifest(raw, checkpoint, vehicle_id, root=PROFILE):
    manifest = verify_manifest(raw, vehicle_id, root)
    _bind_manifest(raw, manifest, checkpoint, "head")
    return manifest


def verify_live_receipt(request, receipt, route_vehicle_id, head_manifest_raw=None, root=PROFILE, head_commitment=None):
    """head_commitment is the head's target_raw_sha256 for a client that holds it (it reached the head through a
    1.4 changed set); every live step of a head carries that same value."""
    validate_request(request, root)
    if _violations("physical-changed-set-receipt.schema.json", "receipt", receipt, root):
        raise ContractError("receipt violates 1.5 schema")
    inherited._check_encoded_size(receipt, MAX_CONTROL_BYTES, "receipt")
    if receipt["vehicle_id"] != route_vehicle_id or receipt["source"]["vehicle_id"] != route_vehicle_id:
        raise ContractError("source vehicle mismatch")
    if receipt["source"] != request["source"]:
        raise ContractError("source binding mismatch")
    base = receipt["base"]
    if (base["receipt_id"] != request["base_receipt_id"] or base["manifest_id"] != request["base_manifest_id"]
            or base["manifest_sha256"] != request["base_manifest_sha256"] or base["sequence"] != request["from_sequence"]):
        raise ContractError("base checkpoint mismatch")
    if receipt["target"] != base:
        raise ContractError("live target is not the base head")
    if receipt["from_live_sequence"] != request["base_live_sequence"]:
        raise ContractError("live base sequence mismatch")
    if receipt["to_live_sequence"] != receipt["from_live_sequence"] + 1:
        raise ContractError("live sequence gap")
    if receipt["live_sequence"] != receipt["to_live_sequence"]:
        raise ContractError("live sequence member mismatch")
    if head_commitment is not None and receipt["target_raw_sha256"] != head_commitment:
        raise ContractError("live target commitment mismatch")
    chunks = receipt["chunks"]
    if [entry["chunk_index"] for entry in chunks] != list(range(len(chunks))):
        raise ContractError("delta chunks are not contiguous")
    if sum(entry["pack"]["compressed_bytes"] for entry in chunks) != receipt["total_compressed_bytes"]:
        raise ContractError("aggregate compressed byte count mismatch")
    _signature(receipt, route_vehicle_id, "signature")
    if head_manifest_raw is not None:
        verify_head_manifest(head_manifest_raw, base, route_vehicle_id, root)
    return receipt


def verify_live_receipt_body(request_raw, receipt_raw, route_vehicle_id, head_manifest_raw=None, root=PROFILE):
    return verify_live_receipt(validate_request_body(request_raw, root),
                               inherited._bounded_json(receipt_raw, MAX_CONTROL_BYTES, "receipt"),
                               route_vehicle_id, head_manifest_raw, root)


def verify_live_pack(receipt, compressed):
    """A live pack is a 1.4 pack bound to one head on both sides, with no tombstones and only closed roots."""
    if receipt["target"] != receipt["base"]:
        raise ContractError("live target is not the base head")
    inherited.verify_pack(receipt, compressed, PREVIOUS)
    with tempfile.TemporaryDirectory() as temp:
        path = Path(temp) / "pack.sqlite"
        inherited._decompress_pack(compressed, path, receipt["total_uncompressed_bytes"])
        connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        try:
            if connection.execute("SELECT COUNT(*) FROM tombstones").fetchone()[0]:
                raise ContractError("live changed set deletes rows")
            # Only closed drives and charges are published live, so a live root never needs a later delete.
            if connection.execute("SELECT COUNT(*) FROM impacted_roots WHERE target_state <> 'closed'").fetchone()[0]:
                raise ContractError("live changed set publishes an open or absent root")
        finally:
            connection.close()
    return True


def live_step_key(receipt):
    """The identity of a live step: the exact head and the position it advances from."""
    return (receipt["base"]["receipt_id"], receipt["base"]["manifest_sha256"], receipt["from_live_sequence"])


def verify_live_step_unique(first, second):
    """A live step is signed for one changed set only; a replay must carry identical content."""
    if live_step_key(first) == live_step_key(second) and inherited._canonical_unsigned(first) != inherited._canonical_unsigned(second):
        raise ContractError("live step reissued with different content")
    return True


def resolve_base(rules, ledger, request):
    """The answer live_rules.base_resolution gives a request: first matching row wins.

    ledger: {"heads": {receipt_id: {manifest_id, manifest_sha256, sequence, position, high_water_mark}},
             "serve": the receipt_id of the head to serve, "paused": True while a rotation is pending}.
    An unknown condition name is refused, so the table cannot drift from the model unnoticed.
    """
    head = ledger["heads"].get(request["base_receipt_id"])
    recorded = head is not None and all(head[key] == request[name] for key, name in (
        ("manifest_id", "base_manifest_id"), ("manifest_sha256", "base_manifest_sha256"), ("sequence", "from_sequence")))
    served = request["base_receipt_id"] == ledger["serve"]
    live = request["base_live_sequence"]
    conditions = {
        "base_identity_not_recorded_or_differs": lambda: not recorded,
        "base_head_is_not_the_head_to_serve": lambda: not served,
        "head_to_serve_live_service_paused_for_rotation": lambda: bool(ledger["paused"]),
        "base_live_sequence_above_head_high_water_mark": lambda: live > head["high_water_mark"],
        "base_live_sequence_above_head_position_and_not_above_high_water_mark":
            lambda: head["position"] < live <= head["high_water_mark"],
        "base_live_sequence_equals_head_position": lambda: live == head["position"],
        "base_live_sequence_behind_head_position": lambda: live < head["position"],
    }
    for entry in rules["base_resolution"]:
        if entry["when"] not in conditions:
            raise ContractError("unknown base resolution condition")
        if conditions[entry["when"]]():
            return entry["answer"]
    raise ContractError("base resolution is not total")


def verify_checkpoint_upgrade(checkpoint, manifest_raw, route_vehicle_id, root=PROFILE):
    """A 1.4 checkpoint becomes a 1.5 checkpoint at live_sequence 0 when the 1.5 manifest names the same head.

    The identity members are always compared. vehicle_id and chunks are compared when the checkpoint carries them,
    which only a checkpoint holding the head's bootstrap manifest does; the route vehicle binds the vehicle either way.
    """
    manifest = verify_manifest(manifest_raw, route_vehicle_id, root, "upgrade manifest")
    if any(key not in checkpoint for key in UPGRADE_IDENTITY):
        raise ContractError("checkpoint upgrade is incomplete")
    compared = (*UPGRADE_IDENTITY, *(key for key in ("vehicle_id", "chunks") if key in checkpoint))
    if any(manifest[key] != checkpoint[key] for key in compared):
        raise ContractError("checkpoint upgrade does not match the bootstrap manifest")
    return {**{key: manifest[key] for key in UPGRADE_IDENTITY},
            "manifest_sha256": hashlib.sha256(manifest_raw).hexdigest(), "live_sequence": 0}


def verify_no_op(request, no_op, route_vehicle_id, root=PROFILE):
    validate_request(request, root)
    if _violations("live-overlay.schema.json", "no_op", no_op, root):
        raise ContractError("no-op violates 1.5 schema")
    inherited._check_encoded_size(no_op, MAX_CONTROL_BYTES, "no-op")
    if no_op["vehicle_id"] != route_vehicle_id:
        raise ContractError("no-op vehicle mismatch")
    if (no_op["base_receipt_id"] != request["base_receipt_id"] or no_op["base_manifest_sha256"] != request["base_manifest_sha256"]
            or no_op["sequence"] != request["from_sequence"]):
        raise ContractError("no-op base mismatch")
    if no_op["live_sequence"] != request["base_live_sequence"]:
        raise ContractError("no-op live sequence mismatch")
    _signature(no_op, route_vehicle_id, "no-op signature")
    return no_op


def verify_no_op_body(request_raw, no_op_raw, route_vehicle_id, root=PROFILE):
    return verify_no_op(validate_request_body(request_raw, root),
                        inherited._bounded_json(no_op_raw, MAX_CONTROL_BYTES, "no-op"), route_vehicle_id, root)


def verify_rebase(request, hint, route_vehicle_id, replacement_manifest_raw, root=PROFILE):
    validate_request(request, root)
    if _violations("physical-rebase-hint.schema.json", "hint", hint, root):
        raise ContractError("rebase violates 1.5 schema")
    inherited._check_encoded_size(hint, MAX_CONTROL_BYTES, "rebase")
    validate_request(hint["retry_request"], root)
    if hint["vehicle_id"] != route_vehicle_id or hint["retry_request"]["source"] != request["source"]:
        raise ContractError("rebase source mismatch")
    requested = hint["requested_base"]
    if any(requested[key] != request[request_key] for key, request_key in
           (("manifest_id", "base_manifest_id"), ("receipt_id", "base_receipt_id"),
            ("manifest_sha256", "base_manifest_sha256"), ("sequence", "from_sequence"))):
        raise ContractError("rebase requested base mismatch")
    if hint["requested_live_sequence"] != request["base_live_sequence"]:
        raise ContractError("rebase requested live sequence mismatch")
    replacement = hint["replacement"]
    # A replacement that is not a newer head would send the client back to the head it asked about.
    if replacement["receipt_id"] == requested["receipt_id"] or replacement["sequence"] <= requested["sequence"]:
        raise ContractError("rebase replacement is not a newer head")
    expected_retry = {**request, "base_manifest_id": replacement["manifest_id"],
                      "base_receipt_id": replacement["receipt_id"],
                      "base_manifest_sha256": replacement["manifest_sha256"],
                      "from_sequence": replacement["sequence"], "base_live_sequence": 0}
    if hint["retry_request"] != expected_retry:
        raise ContractError("rebase retry request mismatch")
    if hashlib.sha256(replacement_manifest_raw).hexdigest() != replacement["manifest_sha256"]:
        raise ContractError("rebase replacement manifest digest mismatch")
    manifest = verify_manifest(replacement_manifest_raw, route_vehicle_id, root, "replacement manifest")
    if any(manifest[key] != replacement[key] for key in ("manifest_id", "receipt_id", "sequence", "schema_version")) \
            or manifest["chunks"] != replacement["chunks"]:
        raise ContractError("rebase replacement manifest mismatch")
    _signature(hint, route_vehicle_id, "rebase signature")
    return True


def verify_rebase_body(request_raw, hint_raw, route_vehicle_id, replacement_manifest_raw, root=PROFILE):
    return verify_rebase(validate_request_body(request_raw, root),
                         inherited._bounded_json(hint_raw, MAX_CONTROL_BYTES, "rebase"), route_vehicle_id,
                         replacement_manifest_raw, root)


def verify_bootstrap_then_overlay(manifest_raw, request, receipt, route_vehicle_id, root=PROFILE):
    """After a bootstrap the client asks for the overlay at once, from the manifest's live_sequence."""
    manifest = verify_manifest(manifest_raw, route_vehicle_id, root)
    validate_request(request, root)
    if (hashlib.sha256(manifest_raw).hexdigest() != request["base_manifest_sha256"]
            or manifest["manifest_id"] != request["base_manifest_id"] or manifest["receipt_id"] != request["base_receipt_id"]
            or manifest["sequence"] != request["from_sequence"] or manifest["live_sequence"] != request["base_live_sequence"]):
        raise ContractError("overlay request does not start from the bootstrap manifest")
    return verify_live_receipt(request, receipt, route_vehicle_id, manifest_raw, root)


def verify_legacy_exchange(request, response, route_vehicle_id):
    """A request without base_live_sequence gets 1.4 semantics and no live member."""
    if classify_request(request) != "legacy":
        raise ContractError("not a 1.4 request")
    inherited.validate_request(request, PREVIOUS)
    if any(member in response for member in LIVE_MEMBERS):
        raise ContractError("legacy request received live overlay")
    if response.get("kind") == "physical_changed_set":
        inherited.verify_receipt(request, response, route_vehicle_id, PREVIOUS)
    elif response.get("kind") == "no_op":
        if _violations("noop.schema.json", "receipt", response, ROOT / "profiles/hub-sync-v1/1.3.0"):
            raise ContractError("no-op violates 1.3 schema")
        if response["vehicle_id"] != route_vehicle_id:
            raise ContractError("no-op vehicle mismatch")
        if response["base_receipt_id"] != request["base_receipt_id"] or response["sequence"] != request["from_sequence"]:
            raise ContractError("no-op base mismatch")
        _signature(response, route_vehicle_id, "no-op signature")
    else:
        raise ContractError("unexpected legacy response")
    return True


def _fixtures(root):
    found = {}
    for path in sorted((Path(root) / "examples").glob("*.json")):
        document = read_json(path)
        found[document.get("fixture_id", path.stem)] = (path, document)
    return found


def _file_fixture(root, relative, raw=False):
    # As in verify_bundle, the retained 1.4.0 profile is the sibling of the directory being checked.
    path = (Path(root) / relative).resolve()
    if not path.is_relative_to((Path(root).parent / "1.4.0").resolve()):
        raise ContractError("fixture path leaves the inherited profile")
    return path.read_bytes() if raw else read_json(path)


def _run_case(case, root, fixtures):
    def fixture(key, member):
        return fixtures[case[key]][1][member]

    def manifest_raw():
        return fixtures[case["manifest_fixture"]][0].read_bytes()

    kind = case["validator"]
    if kind == "checkpoint_upgrade":
        document = _file_fixture(root, case["checkpoint_file"])
        if "checkpoint_member" in case:
            vehicle, held = document["receipt"]["vehicle_id"], document["receipt"][case["checkpoint_member"]]
        else:
            vehicle, held = document["vehicle_id"], document
        return verify_checkpoint_upgrade({key: held[key] for key in UPGRADE_MEMBERS if key in held}, manifest_raw(), vehicle, root)
    if kind == "legacy_exchange":
        request = _file_fixture(root, case["request_file"])["request"]
        response = _file_fixture(root, case["response_file"])["receipt"]
        return verify_legacy_exchange(request, response, request["source"]["vehicle_id"])
    request = fixture("request_fixture", "request")
    vehicle = request["source"]["vehicle_id"]
    if kind == "live_receipt":
        return verify_live_receipt(request, fixture("response_fixture", "receipt"), vehicle,
                                   manifest_raw() if "manifest_fixture" in case else None, root)
    if kind == "live_no_op":
        return verify_no_op(request, fixture("response_fixture", "no_op"), vehicle, root)
    if kind == "live_rebase":
        return verify_rebase(request, fixture("response_fixture", "hint"), vehicle, manifest_raw(), root)
    if kind == "live_step_unique":
        return verify_live_step_unique(fixture("response_fixture", "receipt"), fixture("second_fixture", "receipt"))
    if kind == "bootstrap_then_overlay":
        return verify_bootstrap_then_overlay(manifest_raw(), request, fixture("response_fixture", "receipt"), vehicle, root)
    raise ContractError("unknown case validator")


def run_cases(root=PROFILE):
    """Execute every case in cases.json.

    A case passes only when every expected error substring appears in an error and every error contains an
    expected substring, so an unexpected failure (a wrong status, a missing fixture) fails the case.
    """
    root = Path(root)
    fixtures = _fixtures(root)
    results = []
    for case in read_json(root / "cases.json")["cases"]:
        errors = []
        try:
            if "response_fixture" in case:
                response = fixtures[case["response_fixture"]][1]
                body = next(response[key] for key in ("receipt", "no_op", "hint") if key in response)
                if (case["status"] == 409) != (body["kind"] == "rebase_required"):
                    errors.append("case status does not match the response kind")
            elif case["validator"] in ("legacy_exchange", "checkpoint_upgrade") and case["status"] != 200:
                # A 1.4 receipt or no-op and a bootstrap manifest are 200 answers; there is no response fixture to compare.
                errors.append("case status does not match the response kind")
            _run_case(case, root, fixtures)
        except KeyError as error:
            errors.append(f"missing fixture or member {error}")
        except (ContractError, ValueError) as error:
            errors.append(str(error) or error.__class__.__name__)
        expected = case["expected_errors"]
        passed = (all(any(item in error for error in errors) for item in expected)
                  and all(any(item in error for item in expected) for error in errors))
        results.append({"case_id": case["id"], "passed": passed, "errors": errors, "expected_errors": expected})
    return results

#!/usr/bin/env python3
"""Build the additive hub-sync-v1@1.5.0 live-overlay candidate profile.

The profile is chained on the committed 1.4.0 profile: it pins the SHA-256 of
that profile's SHA256SUMS, derives its closed schemas from the committed 1.4.0
and 1.3.0 schema bytes, and refers to every unchanged 1.4.0 artefact by relative
path instead of copying it. Nothing here builds a SQLite pack, so the output is
independent of the SQLite library version; the one example pack descriptor is
read from the committed 1.4.0 pack bytes.
"""
import argparse
import base64
import copy
import hashlib
import json
from pathlib import Path

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

ROOT = Path(__file__).resolve().parents[1]
PROFILE = ROOT / "profiles/hub-sync-v1/1.5.0"
PREVIOUS_DIR = ROOT / "profiles/hub-sync-v1/1.4.0"
PREVIOUS = PREVIOUS_DIR / "SHA256SUMS"
MANIFEST_DIR = ROOT / "profiles/hub-sync-v1/1.3.0"
PROFILE_ID = "hub-sync-v1@1.5.0"
PREVIOUS_ID = "hub-sync-v1@1.4.0"
FORMAT = "teslatlas-physical-v3-delta-v1"
# Published RFC 8032 test vector 1, the 1.4.0 fixture signing seed. Never a live credential.
SEED = bytes.fromhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
MAX_INTEGER = 9007199254740991
UUID = {"type": "string", "format": "uuid", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"}
OPAQUE = {"type": "string", "minLength": 1, "maxLength": 4096, "pattern": "^[A-Za-z0-9._~-]+$"}
HEX = {"type": "string", "pattern": "^[0-9a-f]{64}$"}
POSITIVE = {"type": "integer", "minimum": 1, "maximum": MAX_INTEGER}
NATURAL = {"type": "integer", "minimum": 0, "maximum": MAX_INTEGER}
SCHEMA = "https://json-schema.org/draft/2020-12/schema"
# Unchanged artefacts the 1.5.0 profile refers to instead of copying. Each is pinned by digest.
INHERITED_FROM_14 = (
    "physical-field-catalog.json", "physical-value-semantics.json",
    "physical-delta-pack-v1-contract.json", "physical-delta-pack-v1.sql",
    "signing-keys.schema.json", "fixture-signing-keys.json",
    "changes-since-request.schema.json", "physical-changed-set-receipt.schema.json",
    "physical-rebase-hint.schema.json", "packs/physical-changed-set-v1.sqlite.zst",
    "examples/changes-since-request-physical.json", "examples/changes-since-physical-changed-set.json",
    "examples/signed-full-manifest-42.json", "examples/signed-full-manifest-43.json",
)
INHERITED_FROM_13 = ("sync-manifest.schema.json", "noop.schema.json", "sync-error.schema.json", "signing-keys.schema.json")
# Diagnostic only: a client takes the same action (a full bootstrap, then base_live_sequence 0) for every reason.
REBASE_REASONS = ("head_changed", "live_retention", "live_ahead")
EXPIRED_HEAD = 40


def obj(properties, required=None):
    return {"type": "object", "additionalProperties": False, "properties": properties,
            "required": required or list(properties)}


def schema(name, definitions):
    return {"$schema": SCHEMA, "$id": f"urn:teslatlas:hub-sync-v1:1.5.0:{name}", "$defs": definitions}


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def read_json(path):
    return json.loads(Path(path).read_text())


def lock_sums(directory):
    """Digest map of a retained profile's own SHA256SUMS."""
    result = {}
    for line in (Path(directory) / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        result[name] = digest
    return result


def inherited_digests():
    """Digest of each referenced artefact, cross-checked against its owning SHA256SUMS."""
    result = {}
    for directory, names in ((PREVIOUS_DIR, INHERITED_FROM_14), (MANIFEST_DIR, INHERITED_FROM_13)):
        listed = lock_sums(directory)
        for name in names:
            digest = hashlib.sha256((directory / name).read_bytes()).hexdigest()
            assert listed[name] == digest, f"retained artefact drifted: {directory.name}/{name}"
            result[f"../{directory.name}/{name}"] = digest
    return result


def signed(payload):
    data = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    key = Ed25519PrivateKey.from_private_bytes(SEED)
    public = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    result = dict(payload)
    result["signature"] = {"algorithm": "ed25519", "key_id": "ed25519-sha256-" + hashlib.sha256(public).hexdigest(),
                           "signed_payload_sha256": hashlib.sha256(data).hexdigest(),
                           "signature": base64.b64encode(key.sign(data)).decode()}
    return result


def request_definition():
    """The 1.4.0 request plus a required base_live_sequence; still closed."""
    value = copy.deepcopy(read_json(PREVIOUS_DIR / "changes-since-request.schema.json")["$defs"]["request"])
    assert "base_live_sequence" not in value["properties"]
    value["properties"]["base_live_sequence"] = NATURAL
    value["required"].append("base_live_sequence")
    return value


def receipt_definition():
    """The 1.4.0 physical_changed_set receipt plus the three live members; still closed."""
    value = copy.deepcopy(read_json(PREVIOUS_DIR / "physical-changed-set-receipt.schema.json")["$defs"]["receipt"])
    for name in ("live_sequence", "from_live_sequence", "to_live_sequence"):
        assert name not in value["properties"]
    value["properties"].update(live_sequence=POSITIVE, from_live_sequence=NATURAL, to_live_sequence=POSITIVE)
    value["required"].extend(["live_sequence", "from_live_sequence", "to_live_sequence"])
    return value


def hint_definition(request):
    """The 1.4.0 rebase hint with the live reasons, the requested live position and a live retry."""
    value = copy.deepcopy(read_json(PREVIOUS_DIR / "physical-rebase-hint.schema.json")["$defs"]["hint"])
    retry = copy.deepcopy(request)
    retry["properties"]["base_live_sequence"] = {"const": 0}
    value["properties"]["reason"] = {"enum": list(REBASE_REASONS)}
    value["properties"]["requested_live_sequence"] = NATURAL
    value["properties"]["retry_request"] = retry
    value["required"].insert(value["required"].index("requested_base") + 1, "requested_live_sequence")
    return value


def overlay_definitions(receipt):
    """The 1.5.0 bootstrap manifest and the live no-op."""
    manifest = copy.deepcopy(read_json(MANIFEST_DIR / "sync-manifest.schema.json")["$defs"]["schema_2_2"])
    assert "live_sequence" not in manifest["properties"]
    manifest["properties"]["live_sequence"] = {"const": 0}
    manifest["required"].append("live_sequence")
    no_op = obj({"kind": {"const": "no_op"}, "vehicle_id": UUID, "base_receipt_id": OPAQUE,
                 "base_manifest_sha256": HEX, "base_manifest_schema": {"const": "2.2"},
                 "sequence": POSITIVE, "manifest_schema": {"const": "2.2"}, "live_sequence": NATURAL,
                 "signature": copy.deepcopy(receipt["properties"]["signature"])})
    return {"manifest": manifest, "no_op": no_op}


def control_schemas():
    request, receipt = request_definition(), receipt_definition()
    return {"changes-since-request.schema.json": schema("request", {"request": request}),
            "physical-changed-set-receipt.schema.json": schema("receipt", {"receipt": receipt}),
            "physical-rebase-hint.schema.json": schema("hint", {"hint": hint_definition(request)}),
            "live-overlay.schema.json": schema("live-overlay", overlay_definitions(receipt))}


def profile_document(inherited):
    previous = read_json(PREVIOUS_DIR / "profile.json")
    profile = {
        "profile_id": PROFILE_ID, "status": "candidate", "previous_profile": PREVIOUS_ID,
        "previous_profile_sha256": hashlib.sha256(PREVIOUS.read_bytes()).hexdigest(),
        "snapshot_schema": "2.2", "changed_set_format": FORMAT,
        "bootstrap_selector": {"header": "x-teslatlas-sync-profile", "value": PROFILE_ID,
                               "required_schema_header": "x-teslatlas-supported-schemas", "required_schema_value": "2.1,2.2"},
        "bootstrap_rule": "The 1.5 selector succeeds only with a signed schema 2.2 PhysicalV3 snapshot that carries live_sequence. If no schema 2.2 PhysicalV3 head is available, return an empty 406 no-store. The 2.1 and 2.2 header spelling is retained for selector compatibility; it does not permit a 2.1 base. A 1.5 bootstrap manifest differs from the 1.4 bootstrap manifest of the same head by the signed live_sequence member, so the exact-bytes SHA-256 that a checkpoint stores as manifest_sha256 is profile-specific: a checkpoint taken under 1.4 is never sent as a 1.5 base and is upgraded by checkpoint_upgrade.",
        "checkpoint_resolution": "The Hub verifies base manifest ID, exact signed-manifest byte SHA-256, receipt, sequence, source binding and base_live_sequence before any live changed set or signed no-op. A base that is not the head to serve receives a signed 409 rebase_required whatever its base_live_sequence. That covers a superseded head, the immediate predecessor (the 1.4 single-step head transition is not offered to a 1.5 client) and a head whose packs have expired, because the Hub keeps the identity of every head it has published for the vehicle (live_rules.head_identity_retention) and the replacement is a head that already exists, so answering costs no rebuild. A receipt this Hub did not record for the vehicle, a known receipt with a different manifest_id, manifest digest or sequence (for example the digest of the 1.4 manifest of the same head), and, for the head to serve, a base_live_sequence above the highest live position the Hub recorded as signed for it (a position no 1.5 client can legitimately hold) all fail closed, without rebuilding anything, as an unsigned 422 unknown_base_receipt, as in 1.3 and 1.4. A client answered that way drops its checkpoint, performs a full bootstrap under the 1.5 selector and then requests with base_live_sequence 0 (error_responses.on_unknown_base_receipt).",
        "compatibility": "A request without base_live_sequence keeps 1.4 semantics and never receives a live overlay. A request carrying base_live_sequence is invalid under the closed 1.4 request schema, and a 1.5 request without it is invalid under the closed 1.5 schema, so the two are mutually exclusive. A live receipt and a live no-op each carry live members that the closed 1.4 and 1.3 schemas forbid. Schema 2.2 snapshots are unchanged except for the live_sequence member of the 1.5 bootstrap manifest.",
        "first_delta_base": "exact_current_head_and_live_sequence", "fallback": "signed_409_complete_schema_2_2_replacement",
        "capability": {
            "token": "sync.live",
            "rule": "A Hub advertises sync.live in discovery capabilities if and only if it serves this profile's live overlay (a hub-sync-v1@1.5.0 bootstrap and a changes-since request carrying base_live_sequence) and the discovery profile it names defines the token. A Hub that does not serve the overlay MUST NOT advertise it and answers the 1.5 selector with an empty 406 no-store. hub-http-v1@1.2.0 and hub-http-v1@1.1.0 do not define sync.live, and a Hub that names a discovery profile advertises only the tokens that profile lists, so a Hub that serves this profile under either of them does not advertise sync.live. A later hub-http-v1 minor adds the token; the exact capability equality check of the hub-http-v1@1.1.0 actual-Hub matrix is adjusted then.",
            "advertised_only_under_a_discovery_profile_that_defines_it": True,
            "discovery_profiles_without_the_token": ["hub-http-v1@1.1.0", "hub-http-v1@1.2.0"],
            "when_the_discovery_profile_lacks_the_token": "do_not_advertise",
            "token_added_by": "a_later_hub_http_v1_minor",
            "adjusted_when_the_token_is_added": "the_hub_http_v1_1_1_0_actual_hub_matrix_exact_capability_equality_check",
        },
        "live_rules": {
            "request_member": "base_live_sequence",
            "request_member_required": True,
            "receipt_members": ["live_sequence", "from_live_sequence", "to_live_sequence"],
            "manifest_member": "live_sequence",
            "manifest_live_sequence": 0,
            "live_step": 1,
            "bootstrap_base_live_sequence": 0,
            "target_equals_base": True,
            "live_changed_sets_delete": False,
            "rebase_reasons": list(REBASE_REASONS),
            "rebase_reason_is_diagnostic": True,
            "rebase_retry_base_live_sequence": 0,
            "rebase_replacement_is_newer_head": True,
            "head_identity_retention": "vehicle_lifetime",
            "head_identity_members": ["manifest_id", "receipt_id", "manifest_sha256", "sequence", "source"],
            "head_live_members": ["live_position", "live_high_water_mark"],
            "live_high_water_mark": "highest_live_sequence_ever_signed_for_the_head",
            "live_step_content_is_unique": True,
            "live_regression_rotates_head": True,
            "live_rotation_inside_request": False,
            "live_roots_closed_only": True,
            "target_raw_sha256": "the_head_own_commitment_constant_for_the_head",
            "live_apply": {
                "precondition": "stored_receipt_id_manifest_sha256_live_sequence_equal_base_and_from_live_sequence",
                "activates": ["receipt_id", "manifest_sha256", "to_live_sequence"],
                "replay_identity": ["receipt_id", "manifest_sha256", "to_live_sequence"],
                "inherited_replay_clause_applies": False,
            },
            "live_journal_retention": "every_live_step_of_the_current_head_from_live_sequence_1_or_fold_into_a_new_head_before_pruning",
            "live_over_limit": "split_into_consecutive_live_steps",
            "live_fold_only_for": "a_single_root_whose_closure_alone_exceeds_the_1_4_limits",
            "head_transition": {"base": "head_packs_at_live_sequence_0", "tombstones_live_rows": False,
                                "carries_folded_live_rows_as": "changed"},
            "base_resolution_order": "first_match",
            "base_resolution": [
                {"when": "base_identity_not_recorded_or_differs",
                 "answer": {"status": 422, "error": "unknown_base_receipt", "signed": False}},
                {"when": "base_head_is_not_the_head_to_serve",
                 "answer": {"status": 409, "kind": "rebase_required", "reasons": list(REBASE_REASONS)}},
                {"when": "head_to_serve_live_service_paused_for_rotation",
                 "answer": {"transient_failure": True, "defined_by_this_profile": False}},
                {"when": "base_live_sequence_above_head_high_water_mark",
                 "answer": {"status": 422, "error": "unknown_base_receipt", "signed": False}},
                {"when": "base_live_sequence_above_head_position_and_not_above_high_water_mark",
                 "answer": {"transient_failure": True, "defined_by_this_profile": False,
                            "starts_rotation_outside_request": True}},
                {"when": "base_live_sequence_equals_head_position", "answer": {"status": 200, "kind": "no_op"}},
                {"when": "base_live_sequence_behind_head_position", "answer": {"status": 200, "kind": "physical_changed_set"}},
            ],
            "prose": [
                "A live changed set, live no-op or live rebase is valid only for the exact head (receipt_id, manifest_sha256) its request names. The base and target of a live changed set are that head, byte-equal; the receipt does not move the head.",
                "to_live_sequence equals from_live_sequence plus 1, live_sequence equals to_live_sequence, and from_live_sequence equals the request's base_live_sequence. A client rejects a receipt that breaks any of these.",
                "A client applies a live changed set only when its stored (receipt_id, manifest_sha256, live_sequence) equals the receipt's (base.receipt_id, base.manifest_sha256, from_live_sequence), and then atomically activates (receipt_id, manifest_sha256, to_live_sequence). The target of a live receipt is the head the client already has active, so the 1.4 apply clause that replay of an already activated target is idempotent does not apply: a live receipt is a replay only when the client's stored live_sequence already equals to_live_sequence, and replay identity is (receipt_id, manifest_sha256, to_live_sequence).",
                "When the head matches and the client's live sequence is behind, the Hub returns the next live changed set, one step per response. When it is current, the Hub returns the signed live no-op, which names the exact receipt_id, manifest_sha256, sequence and live_sequence it proves current. The 1.3 no-op is never a live answer.",
                "rebase_required is returned only when the base head is no longer the head to serve: reason head_changed after a later import, reason live_retention after a fold that pruned the live journal of the requested head (a client offline beyond the live-journal retention), and reason live_ahead when the head was rotated away because of a regression and the client's base_live_sequence is above the position the Hub recorded for that head but not above its high-water mark (see the regression rule below). The reason is diagnostic: a client takes the same action for every reason. None of them produces an error, including for a head whose packs have expired, because the Hub keeps the identity of every head it has published for the vehicle (manifest_id, receipt_id, manifest_sha256, sequence and source binding) together with the position it holds for the head and the highest live_sequence it ever signed for it (the high-water mark) for the life of the vehicle. Each names a replacement head with a different receipt_id and a greater sequence.",
                "A Hub MUST retain every live changed set of its current head from live_sequence 1, or fold the live rows into a new head before pruning them, so that a retention rebase cannot loop on an unchanged head. A watermark over paired devices is not enough: a new, reinstalled or rebased device and a 1.4 upgrade start at live_sequence 0 and need step 1, which no paired device's acknowledgement protects.",
                "A (receipt_id, manifest_sha256, live_sequence) is signed for at most one changed set. A Hub that answers the same step twice returns identical content, so crash replay is idempotent, and MUST NOT re-issue a live_sequence of a head with different content. An operation that can lower a head's live position (a backup restore, a lost journal tail) MUST rotate the head before live service resumes. A restore also rolls back the head ledger: a head the restored ledger does not record is answered with the unsigned 422 unknown_base_receipt, and the client recovers by a full 1.5 bootstrap. A client that holds two receipts for one step with different content rejects the second.",
                "A Hub that finds base_live_sequence above the position it holds for the head to serve but not above the head's high-water mark has detected a regression it did not rotate on: it MUST NOT answer a live changed set or a no-op from that head, and it MUST NOT build the replacement inside the request, because a fold is O(history) and a client chooses base_live_sequence. It answers with a transient failure that this profile does not define, starts the rotation outside the request and pauses live service for the head until the rotation completes; every request for the head to serve is answered the same way meanwhile, and a client keeps its checkpoint and retries the same request later. A 409 live_ahead can therefore only name a replacement that already exists: once the rotation has completed the head is no longer the head to serve, and a request that names it is answered 409 rebase_required, with reason live_ahead when its base_live_sequence is above the position the Hub recorded for that head and not above its high-water mark. A base_live_sequence above the high-water mark of the head to serve is not recorded by this Hub as issued: it is answered at once, without rebuilding anything, as the unsigned 422 unknown_base_receipt, so a client cannot make a Hub rotate by naming a position the Hub holds no record of issuing.",
                "A live publication that cannot fit the 1.4 admission limits or closure in one step is split into consecutive live steps, each admitted by the 1.4 limits and closed over its own roots: a root is never split across steps and live_sequence advances by 1 per step. Only a single root whose closure alone exceeds the 1.4 limits is answered by folding the live rows into a new head and answering head_changed; a live publication does not otherwise take the head-rotation path, and a live step is never answered with a 1.4 complete replacement of the same head.",
                "A new head resets live_sequence to 0. After a bootstrap or a rebase the client immediately requests the overlay with base_live_sequence 0; the exact retry_request of a rebase carries base_live_sequence 0.",
                "The manifest's live_sequence is the overlay position already contained in its packs. Head packs are never rewritten, so a Hub emits 0, and a client starts from it. The member is signed but constant for the life of the head, so the head's manifest_sha256 does not change as live changed sets are published; the current overlay position is learned through changes-since, not from the manifest.",
                "The target_raw_sha256 of a live changed set is the head's own all-history target commitment, the value that the receipt which published the head carries, and it is identical in every live step of the head. The step itself is bound by the signed (receipt_id, manifest_sha256, from_live_sequence, to_live_sequence), not by this member. As in 1.4 a client is not required to reproduce it, a client that holds the head's commitment (it reached the head through a 1.4 changed set) compares it with the receipt's, and the pack's delta_metadata must carry the same value.",
                "Every impacted root of a live changed set has target_state closed: only closed drives and charges are published live, so a live changed set never needs to delete a root it published while open, and an open or absent root is never a live root. A client rejects a live pack with an open or absent impacted root.",
                "Live changed sets never delete: their packs carry no tombstones.",
                "An import folds live rows into the new stage before publication. The 1.4 head transition from head N to head N+1 stays as in 1.4 and is computed from head N's packs at live_sequence 0, because a 1.4 client never holds live rows. It carries every folded live row of N+1 as a changed row and never tombstones a live row. A live duplicate that an import replaces is absent from N+1 and from that transition and reaches no client as a tombstone: a 1.5 client above live_sequence 0 never receives a head transition, it receives a complete replacement of the new head.",
            ],
        },
        "checkpoint_upgrade": {
            "from": PREVIOUS_ID, "mechanism": "client_side_adoption",
            "checkpoint_kinds": {
                "bootstrap_manifest": "A 1.4 client that reached its head by a bootstrap holds the signed 1.4 manifest of the head.",
                "changed_set_target": "A 1.4 client that reached its head by a 1.4 physical_changed_set holds the receipt's target (manifest_id, receipt_id, manifest_sha256, sequence, schema_version) and no chunk list.",
            },
            "adopt_when": {
                "always": ["vehicle_id", "manifest_id", "receipt_id", "sequence", "schema_version"],
                "when_the_checkpoint_holds_the_bootstrap_manifest": ["chunks"],
            },
            "vehicle_binding": "The route vehicle binds vehicle_id: the verified 1.5 manifest must name it, and a checkpoint that carries a vehicle_id must equal it.",
            "adopted_live_sequence": 0, "adopted_manifest_sha256": "sha256_of_the_1_5_manifest_bytes",
            "reuses_materialised_state": True, "otherwise": "full_bootstrap_under_the_1_5_selector",
            "when_selector_unserved": "keep_the_1_4_checkpoint_and_1_4_requests",
            "when_selector_later_unserved": {
                "keep_beside_the_1_5_checkpoint": "manifest_sha256_of_the_1_4_checkpoint_until_the_head_changes",
                "at_live_sequence_0": "resume_1_4_requests_from_the_kept_1_4_checkpoint",
                "at_live_sequence_0_without_a_kept_1_4_checkpoint": "fetch_the_1_4_manifest_of_the_same_head_and_adopt_it_when_identity_and_chunks_equal_else_full_1_4_bootstrap",
                "above_live_sequence_0": "full_1_4_bootstrap",
            },
            "old_digest_under_a_1_5_request": {"status": 422, "error": "unknown_base_receipt", "signed": False},
            "prose": "A client holding a 1.4 checkpoint upgrades without a rebase. It fetches the 1.5 bootstrap manifest of the head the Hub serves (one signed document, no packs), verifies it, and adopts it as its 1.5 checkpoint with live_sequence 0 when the manifest's vehicle_id, manifest_id, receipt_id, sequence and schema_version equal those of its 1.4 checkpoint. Both kinds of 1.4 checkpoint qualify: a client that holds the signed 1.4 bootstrap manifest of the head also compares the chunk list, and a client that reached the head through a 1.4 changed set holds only the receipt's target, which carries no chunk list, so it compares the identity members alone; the signed 1.5 manifest binds that identity and a head identity names the head's content. The client then stores the SHA-256 of the 1.5 manifest bytes as manifest_sha256, keeps its materialised state, and requests the overlay with base_live_sequence 0. If any compared member differs, because the head moved on, it performs a full bootstrap under the 1.5 selector. If the Hub answers the 1.5 selector with 406, the client keeps its 1.4 checkpoint and sends 1.4 requests. The client keeps the 1.4 manifest_sha256 beside its 1.5 checkpoint until the head changes. If the Hub later stops serving 1.5 (a 406 on the selector or an invalid_request answer to a 1.5 request), a client at live_sequence 0 holds exactly the head's packs and resumes 1.4 requests from the kept 1.4 checkpoint, and a client that applied any live changed set (live_sequence above 0) holds rows that no 1.4 head contains and performs a full 1.4 bootstrap. A client that reached its head under 1.5 (a 1.5 bootstrap or rebase) has no kept 1.4 checkpoint: at live_sequence 0 it fetches the 1.4 manifest of the same head under the 1.4 selector and adopts it as its 1.4 checkpoint when vehicle_id, manifest_id, receipt_id, sequence, schema_version and chunks equal those of its 1.5 manifest, and otherwise performs a full 1.4 bootstrap. A client MUST NOT send the manifest_sha256 of a 1.4 manifest in a 1.5 request: the Hub answers it, like any known receipt with a different digest, with unsigned 422 unknown_base_receipt.",
        },
        "error_responses": {
            "schema": "../1.3.0/sync-error.schema.json#/$defs/error",
            "rule": "Error statuses are inherited from 1.3.0 and are unsigned. The 1.5 OpenAPI lists only 200 and 409 for changes-since and 200 and 406 for the manifest, as 1.4 does. A base the Hub did not record, and a base_live_sequence above the high-water mark of the head to serve, are answered with 422 unknown_base_receipt.",
            "on_unknown_base_receipt": {
                "client_action": "drop_the_checkpoint_do_a_full_bootstrap_under_the_1_5_selector_then_request_with_base_live_sequence_0",
                "retries_the_same_request": False,
                "rule": "A 1.5 client answered with 422 unknown_base_receipt drops its checkpoint, performs a full bootstrap under the 1.5 selector and then requests the overlay with base_live_sequence 0; it never retries the same request. A backup restore rolls back the head ledger, so it can answer a checkpoint the client legitimately holds this way, and the client recovers by itself.",
            },
        },
        "signing_keys": {
            "wire_key_document": "../1.3.0/signing-keys.schema.json#/$defs/document",
            "route": "/v1/vehicles/{vehicle_id}/sync/signing-keys",
            "rule": "A 1.5 client obtains the vehicle-bound signing keys from the 1.3.0 signing-keys route, whose document carries profile_id hub-sync-v1@1.3.0 and validates against the 1.3.0 schema. The signing_keys_schema and fixture_signing_keys referenced by this profile are the 1.4.0 fixture key set only and validate no wire document. The 1.5 OpenAPI defines no key route.",
        },
        "live_pack": {
            "contract": "../1.4.0/physical-delta-pack-v1-contract.json",
            "binding": "The pack is a 1.4 pack whose delta_metadata base and target both name the exact head. Its tombstones table is empty and every row of its impacted_roots table has target_state closed. A synthesised stationary position for a live charge is an ordinary changed row (the 1.4 row_roles role changed); the 1.4 pack contract has no other role for it.",
            "fixture_status": "The changes-since-live-advance example reuses the 1.4 positive pack digest as a descriptor placeholder so that no SQLite pack is generated. Those bytes bind head 42 to head 43, carry a tombstone and an open charge root, so they are not a valid live pack. No live pack fixture is committed; conformance derives one at run time to exercise the pack-level rules.",
        },
        "max_request_body_bytes": previous["max_request_body_bytes"], "max_control_body_bytes": previous["max_control_body_bytes"],
        **{key: previous[key] for key in ("max_delta_chunks", "max_pack_compressed_bytes", "max_delta_compressed_bytes",
                                          "max_delta_uncompressed_bytes", "max_delta_rows", "max_impacted_roots")},
        "joint_body_admission": "Individual schema member maxima are not independently combinable. Every request, including its base_live_sequence member, and the exact retry_request MUST fit 8192 raw UTF-8 JSON bytes; each signed control response and exact replacement manifest, including its live_sequence member, MUST fit 2097152 raw UTF-8 JSON bytes. A replacement MUST satisfy these limits jointly before it is published. Producers MUST choose bounded checkpoint IDs and chunk references so the complete signed rebase and mandatory exact retry both fit. Receivers MUST enforce raw limits before parsing, including whitespace and numeric lexemes. A schema-valid over-bound value is not an admitted wire document.",
        "field_catalog": "../1.4.0/physical-field-catalog.json", "value_semantics": "../1.4.0/physical-value-semantics.json",
        "pack_contract": "../1.4.0/physical-delta-pack-v1-contract.json", "pack_sql": "../1.4.0/physical-delta-pack-v1.sql",
        "signature": "Ed25519 over RFC 8785 canonical JSON without signature",
        "fixture_signing_keys": "../1.4.0/fixture-signing-keys.json", "signing_keys_schema": "../1.4.0/signing-keys.schema.json",
        "live_overlay_schema": "live-overlay.schema.json",
        "inherited_artifacts": inherited,
    }
    return profile


def build_files(inherited):
    files = {"profile.json": profile_document(inherited), **control_schemas()}
    previous_request = read_json(PREVIOUS_DIR / "examples/changes-since-request-physical.json")["request"]
    previous_receipt = read_json(PREVIOUS_DIR / "examples/changes-since-physical-changed-set.json")["receipt"]
    source = previous_request["source"]
    vehicle_id = source["vehicle_id"]
    # Same head identities as the 1.4 manifests; the signed live_sequence member makes the bytes differ.
    manifests = {}
    # Head 40 stands for a superseded head whose packs have expired: only its identity is retained, so no
    # manifest example is published for it.
    for name, sequence in ((None, EXPIRED_HEAD), ("signed-full-manifest-live-previous", 42),
                           ("signed-full-manifest-live", 43), ("signed-full-manifest-live-next", 44)):
        manifests[sequence] = signed({
            "manifest_id": f"manifest_demo_{sequence:06}", "receipt_id": f"receipt_demo_{sequence:06}",
            "vehicle_id": vehicle_id, "kind": "snapshot", "schema_version": "2.2", "sequence": sequence,
            "live_sequence": 0,
            "chunks": [{"chunk_index": 0, "pack": {"object_name": f"packs/demo-full-{sequence}.sqlite.zst",
                                                   "sha256": f"{sequence % 10}" * 64, "compressed_bytes": 4096}}]})
        if name:
            files[f"examples/{name}.json"] = manifests[sequence]
    heads = {sequence: {"manifest_id": value["manifest_id"], "receipt_id": value["receipt_id"],
                        "manifest_sha256": hashlib.sha256(json_bytes(value)).hexdigest(),
                        "sequence": sequence, "schema_version": "2.2"} for sequence, value in manifests.items()}

    def request(head, live_sequence):
        return {"source": source, "base_receipt_id": head["receipt_id"], "base_manifest_id": head["manifest_id"],
                "base_manifest_sha256": head["manifest_sha256"], "base_manifest_schema": "2.2",
                "from_sequence": head["sequence"], "schema_version_range": {"minimum": "2.2", "maximum": "2.2"},
                "accepted_changed_set_formats": [FORMAT], "base_live_sequence": live_sequence}

    requests = {"live": request(heads[43], 0), "live-current": request(heads[43], 1),
                "live-stale-head": request(heads[42], 3), "live-retained": request(heads[43], 2),
                "live-expired-head": request(heads[EXPIRED_HEAD], 7), "live-ahead": request(heads[43], 9)}
    for name, value in requests.items():
        files[f"examples/changes-since-request-{name}.json"] = {"fixture_id": f"changes-since-request-{name}-v1", "request": value}

    def live_receipt(head, from_sequence, to_sequence, extra_rows=0):
        # Pack descriptor, totals and witnesses are the committed 1.4 example values (see live_pack.fixture_status).
        unsigned = {key: previous_receipt[key] for key in (
            "payload_format", "vehicle_id", "source", "target_raw_sha256", "chunks", "total_compressed_bytes",
            "total_uncompressed_bytes", "total_rows", "affected_projected_ids_sha256", "affected_projected_ids_count",
            "impacted_roots_sha256", "impacted_roots_count", "affected_months")}
        unsigned.update(kind="physical_changed_set", base=head, target=head, live_sequence=to_sequence,
                        from_live_sequence=from_sequence, to_live_sequence=to_sequence)
        unsigned["total_rows"] += extra_rows
        return signed(unsigned)

    files["examples/changes-since-live-advance.json"] = {
        "fixture_id": "changes-since-live-advance-v1", "receipt": live_receipt(heads[43], 0, 1)}
    files["examples/changes-since-live-gap.json"] = {
        "fixture_id": "changes-since-live-gap-v1", "receipt": live_receipt(heads[43], 0, 2)}
    files["examples/changes-since-live-reissued.json"] = {
        "fixture_id": "changes-since-live-reissued-v1", "receipt": live_receipt(heads[43], 0, 1, extra_rows=1)}
    files["examples/changes-since-live-wrong-head.json"] = {
        "fixture_id": "changes-since-live-wrong-head-v1", "receipt": live_receipt(heads[44], 0, 1)}
    files["examples/changes-since-live-no-op.json"] = {
        "fixture_id": "changes-since-live-no-op-v1",
        "no_op": signed({"kind": "no_op", "vehicle_id": vehicle_id, "base_receipt_id": heads[43]["receipt_id"],
                         "base_manifest_sha256": heads[43]["manifest_sha256"], "base_manifest_schema": "2.2",
                         "sequence": 43, "manifest_schema": "2.2", "live_sequence": 1})}

    def rebase(requested, requested_live, reason, replacement_sequence):
        replacement = heads[replacement_sequence]
        return signed({"kind": "rebase_required", "vehicle_id": vehicle_id, "requested_base": requested,
                       "requested_live_sequence": requested_live, "reason": reason,
                       "replacement": {**replacement, "chunks": manifests[replacement_sequence]["chunks"]},
                       "retry_request": request(replacement, 0)})

    files["examples/changes-since-live-rebase-head-changed.json"] = {
        "fixture_id": "changes-since-live-rebase-head-changed-v1", "hint": rebase(heads[42], 3, "head_changed", 43)}
    files["examples/changes-since-live-rebase-retention.json"] = {
        "fixture_id": "changes-since-live-rebase-retention-v1", "hint": rebase(heads[43], 2, "live_retention", 44)}
    files["examples/changes-since-live-rebase-head-expired.json"] = {
        "fixture_id": "changes-since-live-rebase-head-expired-v1",
        "hint": rebase(heads[EXPIRED_HEAD], 7, "head_changed", 44)}
    files["examples/changes-since-live-rebase-ahead.json"] = {
        "fixture_id": "changes-since-live-rebase-ahead-v1", "hint": rebase(heads[43], 9, "live_ahead", 44)}
    cases = [
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
        {"id": "live-step-reissue-rejected", "validator": "live_step_unique",
         "request_fixture": "changes-since-request-live-v1", "response_fixture": "changes-since-live-advance-v1",
         "second_fixture": "changes-since-live-reissued-v1", "status": 200,
         "expected_errors": ["live step reissued with different content"]},
        {"id": "live-checkpoint-upgrade-from-1-4", "validator": "checkpoint_upgrade",
         "checkpoint_file": "../1.4.0/examples/signed-full-manifest-43.json", "manifest_fixture": "signed-full-manifest-live",
         "status": 200, "expected_errors": []},
        {"id": "live-checkpoint-upgrade-other-head-rejected", "validator": "checkpoint_upgrade",
         "checkpoint_file": "../1.4.0/examples/signed-full-manifest-42.json", "manifest_fixture": "signed-full-manifest-live",
         "status": 200, "expected_errors": ["checkpoint upgrade does not match the bootstrap manifest"]},
        # A 1.4 client that advanced by a changed set holds the receipt's target, which has no chunk list.
        {"id": "live-checkpoint-upgrade-from-1-4-changed-set", "validator": "checkpoint_upgrade",
         "checkpoint_file": "../1.4.0/examples/changes-since-physical-changed-set.json", "checkpoint_member": "target",
         "manifest_fixture": "signed-full-manifest-live", "status": 200, "expected_errors": []},
        {"id": "live-checkpoint-upgrade-changed-set-other-head-rejected", "validator": "checkpoint_upgrade",
         "checkpoint_file": "../1.4.0/examples/changes-since-physical-changed-set.json", "checkpoint_member": "base",
         "manifest_fixture": "signed-full-manifest-live", "status": 200,
         "expected_errors": ["checkpoint upgrade does not match the bootstrap manifest"]},
    ]
    files["cases.json"] = {"cases": cases}
    uuid_parameter = {"name": "vehicle_id", "in": "path", "required": True, "schema": UUID}
    request_refs = [{"$ref": "changes-since-request.schema.json#/$defs/request"},
                    {"$ref": "../1.4.0/changes-since-request.schema.json#/$defs/request"}]
    files["openapi.json"] = {
        "openapi": "3.1.0", "info": {"title": "Teslatlas PhysicalV3 live overlay extension", "version": "1.5.0"},
        "paths": {
            "/v1/vehicles/{vehicle_id}/sync/changes-since": {"post": {
                "operationId": "changesSincePhysicalV15", "parameters": [uuid_parameter],
                "requestBody": {"required": True, "content": {"application/json": {"schema": {"oneOf": request_refs}}},
                                "x-max-body-bytes": 8192},
                "responses": {
                    "200": {"description": "signed live changed set or live no-op for a 1.5 request; the 1.4 changed set or 1.3 no-op for a request without base_live_sequence",
                            "content": {"application/json": {"schema": {"oneOf": [
                                {"$ref": "physical-changed-set-receipt.schema.json#/$defs/receipt"},
                                {"$ref": "live-overlay.schema.json#/$defs/no_op"},
                                {"$ref": "../1.4.0/physical-changed-set-receipt.schema.json#/$defs/receipt"},
                                {"$ref": "../1.3.0/noop.schema.json#/$defs/receipt"}]}}}},
                    "409": {"description": "signed complete replacement with exact retry request; 1.5 carries base_live_sequence 0",
                            "content": {"application/json": {"schema": {"oneOf": [
                                {"$ref": "physical-rebase-hint.schema.json#/$defs/hint"},
                                {"$ref": "../1.4.0/physical-rebase-hint.schema.json#/$defs/hint"}]}}}}}}},
            "/v1/vehicles/{vehicle_id}/sync/manifest": {"get": {
                "operationId": "bootstrapPhysicalV15",
                "parameters": [uuid_parameter,
                               {"name": "x-teslatlas-sync-profile", "in": "header", "required": True, "schema": {"const": PROFILE_ID}},
                               {"name": "x-teslatlas-supported-schemas", "in": "header", "required": True, "schema": {"const": "2.1,2.2"}}],
                "responses": {"200": {"description": "signed schema 2.2 PhysicalV3 full snapshot carrying live_sequence",
                                      "content": {"application/json": {"schema": {"$ref": "live-overlay.schema.json#/$defs/manifest"}}}},
                              "406": {"description": "selector unavailable; empty body, Cache-Control: no-store"}}}}}}
    return files


def build(check):
    inherited = inherited_digests()
    files = build_files(inherited)
    rendered = {name: value if isinstance(value, bytes) else json_bytes(value) for name, value in files.items()}
    rendered["SHA256SUMS"] = "".join(f"{hashlib.sha256(rendered[name]).hexdigest()}  {name}\n"
                                     for name in sorted(files)).encode()
    if check:
        for name, raw in rendered.items():
            path = PROFILE / name
            if not path.is_file() or path.read_bytes() != raw:
                raise SystemExit(f"outdated 1.5 profile artifact: {path.relative_to(ROOT)}")
        present = {path.relative_to(PROFILE).as_posix() for path in PROFILE.rglob("*") if path.is_file()}
        if present != set(rendered):
            raise SystemExit("unexpected 1.5 profile artifact: " + ", ".join(sorted(present - set(rendered))))
    else:
        for name, raw in rendered.items():
            (PROFILE / name).parent.mkdir(parents=True, exist_ok=True)
            (PROFILE / name).write_bytes(raw)
    print(PROFILE_ID + " sha256=" + hashlib.sha256(rendered["SHA256SUMS"]).hexdigest())


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    build(parser.parse_args().check)

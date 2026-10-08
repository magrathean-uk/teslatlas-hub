# Commands and metadata

These are rich-profile write surfaces. The candidate current-Hub HTTP profile
has no public command or metadata routes; see [`current-hub.md`](current-hub.md).

Commands and metadata are the two write surfaces. Both are capability-gated,
audited, bounded, and independent of provider-specific implementation details.

## Command catalogue

Use only commands advertised inside the stable `commands.async` discovery
capability. Each descriptor supplies:

- command name and class;
- required authorization scope;
- retry policy;
- whether human confirmation is required;
- JSON Schemas for `parameters` and `expected_state`.

The catalogue is the machine-readable command vocabulary. A command absent
from it is unsupported even if another Hub exposes a similarly named action.
Credential provisioning and scope assignment are deployment concerns; the
public contract only declares the required scope. The generic command schema
validates common shape; the server also validates `parameters`,
`expected_state`, and conditional confirmation against the advertised
descriptor schemas.

## Asynchronous command jobs

`POST /v1/commands` requires `Idempotency-Key` and a body conforming to the
discovered command descriptor plus `schemas/command.schema.json`. Acceptance
returns `202`, a `Location` for the job, an ETag, and an initial `accepted`
job. It never claims that the vehicle has already reached the requested state.

Jobs move through the closed state set:

```text
accepted -> authorising -> sent -> provider_acknowledged -> verifying
         -> succeeded | failed | indeterminate | expired | cancelled
```

Intermediate states may be skipped when the provider cannot expose them.
`succeeded` requires a result; `failed` requires a problem detail. Every state
change appends an audit event with time and actor. Clients poll the job using
conditional GET or consume `command.changed` events.

`expected_state` is the observable postcondition, not a provider reply. A Hub
reports `succeeded` only after verification. If it cannot prove success or
failure, it reports `indeterminate`.

## Idempotency and retries

An idempotency key is a UUID and is retained for at least 86,400 seconds.
Within that window:

- the same principal, key, and canonical request return the same command job;
- the same key with a different request returns `409`
  `idempotency_conflict`;
- a client does not reuse one key for a different intent.

Retry behaviour comes from the discovery descriptor and is repeated on the
job. `none` forbids automatic reissue. `state_verified` permits a retry only
after state verification proves the intent is still unmet. Nuisance and
access-class actions are never blindly retried. Confirmation-required commands
carry the confirming actor and timestamp.

## Mutable metadata

Metadata is separate from immutable observations. Supported kinds and targets
are closed enums in `schemas/metadata.schema.json`.

- `POST /v1/vehicles/{vehicle_id}/metadata` creates revision 1 and returns
  `201`, `Location`, and a strong ETag.
- `GET /v1/metadata/{metadata_id}` returns the record and strong ETag.
- `PUT /v1/metadata/{metadata_id}` replaces only the value.
- `DELETE /v1/metadata/{metadata_id}` returns `200` with a persistent deletion
  tombstone and new strong ETag.

PUT and DELETE require the exact current ETag in `If-Match`. Missing it returns
`428` `precondition_required`. A stale value returns `409`
`metadata_revision_conflict`. A successful replacement increments `revision`,
changes the ETag, and appends an audit event.

Live audit entries contain revision, action, exact UTC time, actor identity,
and previous/new hashes. Created entries have no previous hash;
updated entries have both. A tombstone retains identity and target, stores the
unchanged live history in `audit.history`, and records the terminal revision,
time, and actor once in `audit.deletion`. The final history hash identifies the
deleted value. GET continues to return the tombstone, list operations exclude
it, and `metadata.changed` distributes it. Deletion never rewrites prior
entries or the provider observation log.

The selected rich profile defines digest spelling and chain linkage but does
not establish a reproducible byte preimage. Existing digest examples are
illustrative. Their values MUST NOT be recalculated against a guessed canonical
domain, and current or historical emitter compatibility has not been established.
The final history digest identifies the deleted value within that same history;
deletion adds no hash field or sentinel.

The unselected candidate `metadata-value-sha256-jcs-v1` defines lowercase
SHA-256 of the RFC 8785 canonical JSON UTF-8 bytes of `value` alone. It includes
no newline, identity, revision, audit or timestamp. Candidate values require
finite binary64 JSON numbers and valid Unicode strings, including recursively
inside objects and arrays. Object ordering follows RFC 8785 UTF-16 ordering;
array order is retained. These prerequisites do not narrow today's unrestricted
metadata `value` schema. Candidate byte/digest vectors are in
[`metadata-value-sha256-jcs-v1.json`](../../examples/candidate/metadata-value-sha256-jcs-v1.json).
They verify the candidate only. Adoption requires a versioned compatibility
decision and evidence from the actual emitter and consumers; historical digest
rewrites remain excluded.

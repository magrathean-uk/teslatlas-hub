# R1 named-source read-only evidence plan — 2026-09-09

## Purpose and boundary

This is the next H7 evidence plan, not a source admission, import, migration,
or collection result. It is based on the current `teslamate-check` and
read-only source code paths. No named TeslaMate source, backup, database
credential, vehicle, collector, or Hub service was accessed while preparing
it.

The current static source reference remains TeslaMate v4.2-compatible source
revision `e8d24886f97f22469c2675f89be843f6d401c76a`, with 105 migrations and
migration-set SHA-256
`ea850d1b038c4af950db32e7a0939aa5ebe8f1dcefe5e56dcd592f3451038868`.
The database can establish compatibility with that reviewed schema; the source
owner must separately confirm that the running TeslaMate application is 4.2.0
or later.

## Required owner-supplied input before any source command

| Input | Custody and read-only boundary | Receipt content | Missing now |
| --- | --- | --- | --- |
| Named preserved TeslaMate source | Owner identifies one source and creates or identifies one consistent, immutable backup before inspection. Hub never modifies the original volume. | Redacted source label, snapshot time, backup format/size, content digest, custody owner, and selected car ID. | Yes. |
| PostgreSQL read identity | One database user has `CONNECT` and `SELECT` only for the reviewed source scope. The URL names exactly one host and database, has no password, query, or fragment; its password is supplied only through one owner-only regular file or stdin. | Redacted source authority/database fingerprint, account privilege attestation, password-file mode/ownership check, and no credential value. | Yes. |
| Source operator confirmation | Owner confirms TeslaMate 4.2.0+ and selects one positive car ID. It also confirms whether the selected car has open drive, charging, or state sessions. | Version confirmation, selected car ID, and a redacted open-session count/shape. | Yes. |
| Network and TLS route | The selected host, Hub runner, and database owner agree the one read route and TLS trust. No password is embedded in the URL and no multi-host routing is allowed. | Redacted route/trust identity and the exact refusal reason if unavailable. | Yes. |

Phase A creates no Hub data root, import target, backup restore, service, or collector. A comparison target is a later offline-import input, not an admission input.

The current parser rejects embedded credentials, URL parameters/fragments, and
multiple hosts. Its session begins with `SET SESSION CHARACTERISTICS AS
TRANSACTION READ ONLY`, then `BEGIN ISOLATION LEVEL REPEATABLE READ, READ
ONLY`, and fixes the session time zone to UTC. The source reader has no source
write or credential-transport path.

## Phase A — read-only admission

After the owner has provided the above inputs, run only the current check from
the approved runner. Do not start a Hub service or create an import target.

```sh
teslatlas-hub teslamate-check \
  --source "$PASSWORD_FREE_POSTGRES_URL" \
  --car-id "$TESLAMATE_CAR_ID" \
  --postgres-password-file "$OWNER_ONLY_POSTGRES_PASSWORD_FILE" \
  --acknowledge-v4-2-compatible-schema
```

The raw JSON and password file stay owner-only. The redacted receipt records:

- compatibility status/reason, pinned source revision, observed migration
  version/count, and reviewed bounds;
- selected-car inventory, open-session shape, selected-car and source totals,
  and token-relation validity without token ciphertext;
- the check's `sourceNeverMutated=true` outcome and
  `versionEvidence=database_schema_only` limitation; and
- process exit status, source-backup digest, runner identity, and a before/after
  no-source-mutation observation supplied by the source owner.

An `incompatible`, `unavailable`, or version-confirmation result is a stop.
It must be recorded with the redacted reason code. It does not authorize a
schema change, source repair, source downgrade, token refresh, or retry against
a different source.

## Phase B — bounded source-only inventory

Only after Phase A passes and the coordinator authorizes the selected snapshot, the source owner performs bounded inspection in an explicit transaction. This phase creates no Hub target and makes no source-to-Hub comparison:

```sql
BEGIN READ ONLY;
-- reviewed schema, selected-car, counts, NULL/zero/absent semantics,
-- time-bound, and digest queries only
COMMIT;
```

The inventory receipt binds the preserved backup and source fingerprint. It covers the
17-domain ledger in `teslamate-parity-evidence-2026-09-09.md`, plus the H7
domains that ledger does not claim: drive/session boundaries and duration,
energy, positions/geocoding, charge processes/samples, state/update events,
ordering, and derived values. For each selected domain it records counts,
stable identifiers, units/conversions, time bounds, and `NULL` versus zero,
absent, and inapplicable behavior. Raw locations, tokens, VINs, endpoint
credentials, and unrelated address payloads remain out of the receipt.

The source backup is preserved unchanged. A count-only inventory, a synthetic fixture, or a successful `teslamate-check` is insufficient for import or source-to-Hub parity.

## Phase C — offline import and source-to-Hub comparison

This phase requires separate explicit authorization after Phases A and B. The preferred first lane restores the preserved TeslaMate backup into a fresh, isolated local PostgreSQL instance owned by the test lane; it never changes the original backup or a production source. Before import, the coordinator must approve the restored source identity and custody path, its one read-only database identity, a fresh stopped collector-free Hub destination, and import credential custody. Only then may the offline import populate that destination and compare source/backup/Hub identities, stable IDs, counts, units, time bounds, and NULL/zero/absent/inapplicable semantics.

## Later gates requiring separate authority

| Gate | Why it is not included in Phase A/B | Exact additional authority/evidence required |
| --- | --- | --- |
| Offline import, migration, and open-session cutover | `migrate` writes a Hub target, requires Hub admission, and needs either the TeslaMate encryption key or fresh legacy token files. The source remains read-only but Hub state and credential custody change. | A coordinator-approved restored local PostgreSQL source from the immutable backup, stopped new Hub target, migration credential custody, explicit 4.2+ acknowledgement, selected open-session/successor-delta policy, before/after identity comparison, and rollback/restore plan. |
| Passive collection | Watermark commands inspect durable Hub observations, but a meaningful later observation requires an admitted collector and vehicle/source provenance. | Vehicle/collector owner approval, a no-wake/no-command window, starting and ending watermarks, source provenance, reconnect/duplicate observations, and a pending result if no event arrives. |
| Recovery after real import | The existing synthetic data-only recovery receipt deliberately excludes pairing, keys, configuration, service state, and collector authority. | A separately authorized stopped post-import Hub backup/verify/restore lane, data identity comparison, explicit pairing/TLS treatment, and original-volume preservation. |
| Soak | Restart, reconnect, retention, duplicate delivery, storage growth, and clean shutdown require time and active components. | A bounded duration, selected components, storage/retention limits, no-extra-mutation guard, redacted durable logs/metrics, and final child/listener cleanup evidence. |

## Completion rule

Phase A is only a redacted source-admission receipt. Phase B is only a bounded source inventory. Phase C is the first possible source-to-Hub comparison and requires its own authorization. No phase alone completes H7, D1, or X1.

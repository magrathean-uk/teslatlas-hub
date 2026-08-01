# TeslaMate migration reconciliation v1

Reconciliation binds one exported source snapshot to one sealed Hub stage and
one candidate manifest. It has no source write path and produces a canonical,
checksummed report before publication.

| Proof | Source summary | Destination comparison |
| --- | --- | --- |
| Exact rows | per selected-table count, first/last source key, and ordered canonical keyed hash | typed stage count/key/hash, with every declared mapping loss removed only by its reason code |
| Relationships | selected-car parent/child, endpoint, process/sample, state/update reference counts and orphan counts | stage and projected foreign-key checks; expected omissions must match the loss ledger |
| Time/lifecycle | min/max timestamp, interval count/open count, stable `(time,id)` order, completed/open drive and charge summaries | same normalized milliseconds and completed/open classification; no changed closed interval or unexplained skip |
| Numeric aggregates | canonical decimal sums/min/max for distance, energy, ranges, SOC, power, duration and source cost | source values or documented derived fallback, with formula/version and exact difference classification |
| Spatial | coordinate count, invalid count, min/max and deterministic keyed coordinate samples | WGS84 stage/pack values and matching sample values; invalid input is never silently projected |
| Deep values | deterministic boundary rows and keyed samples selected from the snapshot digest | every captured field, parent chain, null, enum, and mapped value compared field by field |
| Published output | candidate pack hash/size/SQLite integrity and manifest binding | signed manifest totals, chunk order, sequence, identity and cursor verify exactly |

Any difference without a mapping-loss, accepted-anomaly, or declared derived
formula reason is a fatal reconciliation failure. The report names both source
and destination keys, field, expected/actual canonical value hashes, reason,
and severity without exposing credentials or unrelated payloads.

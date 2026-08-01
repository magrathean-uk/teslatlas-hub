# Representative TeslaMate corpus v1

The repeatable corpus is declared in
`fixtures/teslamate-corpus/v1/manifest.json`. Fixtures are deterministic typed
source rows plus a pinned schema/migration fingerprint, expected normalized
fact graph, validation classification, and SHA-256 digest. A fixture materializes
only into a disposable local harness database or binary-COPY source; production
TeslaMate data is never a runtime dependency.

Synthetic fixtures use a recorded generator version and seed, UTC timestamps,
fictional identities/locations, and no credentials. Optional redacted fixtures
need a recorded lawful provenance class, a sanitizer version/digest, a stable
replacement identity map, coordinate/time perturbation policy, and a review
that no direct identifier, secret, address, or raw payload remains. Redaction
may not change the fixture's expected validation result without creating a new
fixture ID.

Every accepted fixture runs the source schema probe, read-only/repeatable-read
capture, typed decoding, stage validation, reconciliation, pack verification,
and differential normalizer. Rejected fixtures prove the exact failure reason
and no publication. The large fixture is deterministic and approximately
ten-million rows; it is generated locally for benchmark/rehearsal instead of
being stored as a production database image.

Fixture changes, generator changes, sanitization changes, or a TeslaMate/Hub
adapter revision create a new corpus version and invalidate prior baseline or
conformance results.

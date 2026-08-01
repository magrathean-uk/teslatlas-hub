# Final parity signoff v1

Only the named Hub release authority may sign a final parity record. The signer
is an accountable operator, not an automated test, package host, or agent. The
record binds the exact Hub release/provenance, pinned TeslaMate reference,
corpus, normalizer, host/platform evidence, runbook/rehearsal records, and
complete artifact index. It signs `approved`, `rejected`, or `superseded`; it
does not execute cutover or grant authority to mutate TeslaMate.

Approval requires every row of `docs/COMPLETION_EVIDENCE_MATRIX.md` to be
current for those exact inputs: clean reference lock; differential parity with
zero unexplained facts/calculations/lifecycle differences; crash/corruption and
restore proof with zero lost durable acknowledgements and duplicate projections;
no-wake/security proof; read-only representative migration/reconciliation;
under-ten-minute baseline evidence with no thirty-minute run; signed phone sync;
one-minute owner-authorized live proof; native macOS/amd64/arm64 proof;
operator-owned cutover/rollback rehearsal; and release/supply-chain evidence.

An accepted deviation must name its user-approved scope, Teslatlas-visible
impact, reason, replacement proof, expiry/review trigger, and signer. It cannot
cover source mutation, secret exposure, data loss, duplicate projection,
unexplained data difference, invalid signature, missing platform evidence,
failed restore, missing owner proof, missed hard limit, unsupported source
schema, or a required compatibility surface. Excluded MQTT/Phoenix/Grafana/web
UI are declared scope boundaries, not silent deviations.

Any source/reference, Hub binary, dependency/vendor, corpus/normalizer,
configuration/profile, platform, release key, protocol/schema, evidence
artifact, or accepted-deviation change invalidates approval until affected gates
are rerun. A later failure, revocation, expiry, corruption, or unexplained
difference supersedes approval immediately. The final record lists every gate
and artifact digest, timestamps, signer identity, remaining non-release notes,
and manual cutover prerequisites without secrets.

Until an `approved` record exists, Hub is not a complete TeslaMate backend
replacement and may not be presented as one. A signed approval is release
readiness only; production cutover remains a separate operator decision and
fresh signed plan.

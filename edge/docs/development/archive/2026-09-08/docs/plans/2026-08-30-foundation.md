# Edge foundation plan

## Goal

Deliver an optional self-hosted Fleet Telemetry ingress without expanding Hub's trust boundary.

## Dependencies

- Stable Edge-to-Hub authentication, batch, acknowledgement, and deduplication protocol.
- Hub ingestion, data-quality, and Fleet continuity policies.
- Tesla Fleet Telemetry receiver requirements.

## Delivery sequence

1. Specify deployment model, identity lifecycle, mTLS trust model, credential rotation, and supported platforms.
2. Specify bounded encrypted spool, acknowledgement, replay, retention, and deletion semantics.
3. Implement receiver admission and batch delivery against a deterministic Hub test double.
4. Test duplicates, out-of-order bursts, restart during spool, full disk, revoked Hub credential, and network recovery.
5. Publish user-operated installation, upgrade, backup, recovery, and observability guidance.
6. Prove that Hub marks unresolved gaps rather than silently inventing continuity.

## Acceptance

- Edge can lose connectivity and later deliver duplicate-safe batches.
- Edge contains neither Tesla credentials nor command paths.
- Spool limits, deletion-after-acknowledgement, and privacy-safe metrics are verifiable.

## Out of scope

Mandatory hosted relay, consumer dashboard, TeslaMate migration, or vehicle command proxy.

# Fleet Telemetry ingress implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:test-driven-development and implement this plan task-by-task. Keep one writer. Read-only research and review may use bounded subagents.

**Goal:** Build an optional user-operated Fleet Telemetry ingress that durably accepts Tesla receiver records, stores them in a bounded encrypted spool, and exposes an authenticated mTLS pull-and-ack contract for a home Hub.

**Architecture:** Tesla's pinned Apache-2.0 Fleet Telemetry v0.9.4 receiver remains a separately built native sidecar and terminates vehicle mTLS. Its reliable-ack HTTP dispatcher posts strict decoded envelopes to a loopback Rust service. The Rust service acknowledges the receiver only after atomic encrypted spool persistence; a home Hub connects outbound over mTLS plus a scoped rotating bearer, pulls bounded batches, durably deduplicates by `record_id`, and acknowledges exact records before Edge deletes them.

**Tech stack:** Rust 1.98, Tokio, Axum, rustls, XChaCha20-Poly1305, SHA-256/JCS canonical JSON, TOML, Prometheus text format; Tesla Fleet Telemetry v0.9.4 sidecar built with Go 1.27.0 and `CGO_ENABLED=0`.

**Spec:** `docs/architecture.md`, `docs/plans/2026-08-30-foundation.md`, and `docs/hub-delivery-contract.md` created by this plan.

## Global constraints

- `#![forbid(unsafe_code)]` in every first-party Rust target.
- Edge stores no Tesla account access token, refresh token, client secret, command key, or vehicle-command path.
- Vehicle mTLS terminates only in the pinned official receiver sidecar; its reliable acknowledgement follows successful loopback durable admission.
- Loopback receiver bodies are at most 256 KiB and reject unknown envelope fields.
- Spool defaults: 512 MiB, 100,000 records, seven-day retention, 1 MiB/256-record Hub batches; every bound is configurable downward or upward within documented hard maxima.
- Spool payloads are encrypted at rest with a mode-0600 32-byte key. File names, logs, health, and metric labels contain no VIN, coordinates, raw payload, secret, or unbounded identifier.
- Hub pull requires both a trusted client certificate and an active Hub-scoped bearer. Credentials are generated locally, shown once, hashed at rest, rotatable with bounded overlap, and revocable immediately.
- Delivery is at-least-once. A disconnect or restart before acknowledgement returns records again. Deletion occurs only after exact acknowledgement or explicit retention expiry; expiry is counted and makes readiness degraded so loss is not silent.
- Hub deduplicates before acknowledgement using the deterministic `record_id` contract.
- No GitHub Actions, hosted CI, Dependabot, release automation, mandatory hosted relay, consumer API, Tesla command support, or real VPS deployment.
- Preserve the dirty-state boundary and do not touch `/Users/bolyki/dev/source/teslatlas-service/app`.
- Commit and push in one coherent batch after fresh verification.

---

### Task 1: Rust skeleton and strict receiver envelope

**Files:**

- Create: `Cargo.toml`
- Create: `Cargo.lock`
- Create: `.gitignore`
- Create: `src/lib.rs`
- Create: `src/protocol.rs`
- Create: `tests/protocol_contract.rs`

**Interfaces:**

- Produces: `ReceiverEnvelope::parse(&[u8]) -> Result<Self, ProtocolError>`.
- Produces: `ReceiverEnvelope::record_id() -> RecordId` where `RecordId` is lowercase SHA-256 hex.
- Produces: strict `HubBatchV1`, `HubBatchRecordV1`, `HubAckV1`, and `HubAckResultV1` wire types.

- [ ] Write protocol tests first for a valid Hub-compatible envelope, unknown-field rejection, 256 KiB body limit, VIN/txid/type/timestamp bounds, JSON duplicate-key rejection, and stable IDs across payload key order.
- [ ] Run `cargo test --test protocol_contract` and confirm missing types/functions fail.
- [ ] Add the minimal strict envelope parser and JCS-based ID calculation:

  ```text
  record_id = hex(sha256(
    "teslatlas-edge-record-v1\0" ||
    canonical_json({version, vin, txid, tx_type, received_at_ms,
                    timestamp_ms, payload, optional_versions})
  ))
  ```

- [ ] Re-run the focused test and then `cargo test --lib`.

### Task 2: Atomic encrypted bounded spool

**Files:**

- Create: `src/crypto.rs`
- Create: `src/spool.rs`
- Create: `tests/spool_contract.rs`
- Create: `tests/support/mod.rs`

**Interfaces:**

- Consumes: `ReceiverEnvelope`, `RecordId`.
- Produces: `Spool::open(SpoolConfig, SpoolKey) -> Result<Spool, SpoolError>`.
- Produces: `enqueue`, `next_batch`, `acknowledge`, `snapshot`, `expire_due`, and `recover` operations.
- Produces: `SpoolSnapshot { pending_records, pending_bytes, corrupt_records, expired_records, oldest_age_seconds }`.

- [ ] Write failing tests proving plaintext VIN/payload bytes never appear in pending files, duplicate enqueue is idempotent, queue byte/count limits reject before acknowledgement, and acknowledgement deletes only named records.
- [ ] Run `cargo test --test spool_contract` and confirm the expected missing-spool failures.
- [ ] Implement XChaCha20-Poly1305 records with a fixed magic/version, random 24-byte nonce, domain-separated associated data, canonical encrypted payload, mode-0600 create-new temp file, `sync_all`, atomic rename, and directory `sync_all`.
- [ ] Add failing restart tests proving valid pending records reappear, orphan temporary files are quarantined, and a corrupt ciphertext is never delivered or deleted silently.
- [ ] Implement deterministic startup scan, quarantine, degraded snapshot state, retention expiry counters, and stable oldest-first selection.
- [ ] Add a filesystem fault boundary and failing ENOSPC test using raw OS storage-full error 28; implement `SpoolError::StorageFull` without retry loops or eviction.
- [ ] Run `cargo test --test spool_contract` after every red/green cycle.

### Task 3: Receiver admission, health, and privacy-safe metrics

**Files:**

- Create: `src/admission.rs`
- Create: `src/health.rs`
- Create: `src/metrics.rs`
- Create: `tests/admission_contract.rs`

**Interfaces:**

- Consumes: loopback bearer file and `Spool::enqueue`.
- Produces: loopback routes `POST /v1/internal/fleet-telemetry`, `GET /healthz`, `GET /readyz`, `GET /metrics`.

- [ ] Write failing router tests for missing/bad bearer (401), malformed/oversized envelope (400/413), queue capacity or ENOSPC (507), corruption-degraded readiness (503), and durable/idempotent admission (204).
- [ ] Implement fixed-body, fixed-concurrency, fixed-timeout middleware and constant-time bearer comparison. Do not log request bodies, VINs, txids, or token material.
- [ ] Write failing metric tests that assert only fixed names/labels and numeric aggregate values; mutation target is any VIN, record ID, secret, or raw payload entering output.
- [ ] Implement Prometheus text for aggregate queue bytes/records/age/corruption/expiry, admissions by fixed reason, deliveries, acknowledgements, and build info.
- [ ] Run `cargo test --test admission_contract`.

### Task 4: Hub credential lifecycle and mTLS pull/ack API

**Files:**

- Create: `src/credentials.rs`
- Create: `src/tls.rs`
- Create: `src/delivery.rs`
- Create: `tests/credential_contract.rs`
- Create: `tests/delivery_contract.rs`
- Create: `tests/mtls_contract.rs`

**Interfaces:**

- Produces: `CredentialStore::{enrol, rotate, revoke, verify}` with token format `tte1.<credential_id>.<32-byte-secret>`.
- Produces: `GET /v1/hub/batches/next` and `POST /v1/hub/acks`.
- Produces: rustls server configuration that requires a certificate rooted in the configured Hub client CA.

- [ ] Write failing credential tests for one-time secret output, no plaintext secret at rest, active verification, zero-overlap rotation, bounded overlap rotation, expiry, immediate revocation, and atomic restart persistence.
- [ ] Implement SHA-256 domain-separated secret digests, constant-time comparison, UUID credential IDs, fixed label bounds, atomic mode-0600 JSON state, and reload-on-request revocation.
- [ ] Write failing delivery tests for bounded oldest-first batch output, stable `batch_id`, disconnect-before-ack replay, duplicate delivery, reordered acknowledgement IDs, unknown IDs, and delete-after-ack.
- [ ] Implement Hub authorization requiring credential ID plus bearer and fixed-size bodies. Return stable machine-readable errors without secret or telemetry content.
- [ ] Generate ephemeral test CA/server/client certificates and write failing network tests proving no-client-cert TLS failure, wrong-CA failure, bad-bearer 401, and trusted-cert plus bearer success.
- [ ] Implement rustls TLS 1.3/1.2 server configuration with mandatory client authentication and no plaintext public Hub listener.
- [ ] Run the three focused test targets.

### Task 5: Configuration, CLI, runtime, and bounded shutdown

**Files:**

- Create: `src/config.rs`
- Create: `src/runtime.rs`
- Create: `src/main.rs`
- Create: `tests/config_contract.rs`
- Create: `tests/runtime_contract.rs`

**Interfaces:**

- Produces CLI commands: `init`, `serve`, `doctor`, `credential enrol`, `credential rotate`, `credential revoke`, and `receiver-token rotate`.
- Produces strict TOML configuration with absolute paths, loopback-only receiver/metrics binds, non-loopback mTLS Hub bind, and validated hard bounds.

- [ ] Write failing config tests for unknown keys, relative/symlink secret paths, non-loopback internal binds, missing TLS/key files, unsafe permissions, zero/unbounded limits, and receiver/public bind collisions.
- [ ] Implement strict configuration and secret-file admission. `init` creates directories, spool key, receiver bearer, and empty credential store with private permissions; it never creates Tesla account or command material.
- [ ] Write failing runtime tests proving cancellation stops all listeners within five seconds while an idle connection exists and a completed admission remains restart-visible.
- [ ] Implement signal-driven cancellation, graceful listener shutdown, five-second join deadline, forced task abort after the deadline, and final spool sync.
- [ ] Run `cargo test --test config_contract --test runtime_contract` and `cargo run -- --help`.

### Task 6: Pinned Tesla Fleet Telemetry mTLS receiver sidecar

**Files:**

- Create: `packaging/fleet-telemetry-bridge/fleet-telemetry-bridge-lock.json`
- Create: `packaging/fleet-telemetry-bridge/0001-teslatlas-http-dispatcher.patch`
- Create: `scripts/build-fleet-telemetry-bridge.sh`
- Create: `packaging/fleet-telemetry.json.example`
- Create: `docs/legal/third-party-notices.md`
- Create: `scripts/test-fleet-telemetry-bridge.sh`

**Interfaces:**

- Produces a CGO-free native `teslatlas-fleet-telemetry` sidecar from exact upstream tag `v0.9.4`, revision `d64c73ab65e7c5fb5fc12b35fe507e2c6054227b`, archive SHA-256 `a30818d9d832cf6dcec7cf0d61b780d4bea52cc7c9f8edb31a111bc0f25cd6b9`.
- Sidecar posts only to `http://127.0.0.1:8080/v1/internal/fleet-telemetry` and reliably acknowledges vehicle records only after HTTP 2xx.

- [ ] Reuse the already reviewed Hub bridge patch byte-for-byte so Edge does not reimplement Tesla's WebSocket/protobuf/mTLS protocol.
- [ ] Add a hermetic Go bridge contract test, applied only inside a temporary source copy, that serves a controlled loopback receiver, proves non-2xx never enters the reliable ack channel, and proves 204 does. Run behavior, not a source grep.
- [ ] Adapt the existing pinned build script to verify archive and patch digests, apply only to a temporary source copy, build with Go 1.27.0 and `CGO_ENABLED=0`, and never modify the checkout.
- [ ] Add the minimal Apache-2.0 origin/revision/modification notice and public certificate/key runtime boundary.
- [ ] Run `scripts/test-fleet-telemetry-bridge.sh` and one local sidecar build from the existing verified source cache.

### Task 7: Deterministic Hub double and fault acceptance suite

**Files:**

- Create: `tests/support/hub_double.rs`
- Create: `tests/fault_matrix.rs`

**Interfaces:**

- Produces a deterministic Hub double that persists seen `record_id` values before acknowledging and exposes applied-envelope count/order.

- [ ] Write failing end-to-end tests for duplicate receiver delivery, delayed/reordered records, Hub disconnect before ack, Edge restart before ack, capacity/full-disk rejection, corrupted ciphertext quarantine, revoked credential, rotation overlap, and network recovery.
- [ ] Implement only test support needed to drive the real routers/spool/TLS paths; use literal hand-derived expectations and no assertions on mocks.
- [ ] For each test, mentally mutate the corresponding production branch and confirm the test would fail.
- [ ] Run `cargo test --test fault_matrix` serially.

### Task 8: Contract and native operations documentation

**Files:**

- Modify: `README.md`
- Modify: `docs/architecture.md`
- Create: `docs/hub-delivery-contract.md`
- Create: `docs/operations/native-installation.md`
- Create: `docs/operations/upgrade-backup-recovery.md`
- Create: `packaging/linux/teslatlas-edge.service`
- Create: `packaging/linux/teslatlas-fleet-telemetry.service`
- Create: `packaging/macos/uk.co.magrathean.teslatlas-edge.plist`

- [ ] Document the exact Hub record-ID/dedupe-before-ack contract, batch/ack/status schemas, retryable status mapping, credential lifecycle, retention-loss visibility, and beta version negotiation.
- [ ] Document native source install for macOS 13+/Apple silicon and Debian/Ubuntu amd64/arm64, dedicated user/file modes, TCP pass-through requirement, systemd/launchd supervision, upgrade preflight, clean stop, tested backup, key/credential restore, corruption recovery, and rollback boundary.
- [ ] Document that backups require the encrypted spool plus spool key and credential state; losing the key makes pending records unrecoverable. Never place private keys in ordinary telemetry/log archives.
- [ ] Add units that stop both processes with a bounded timeout and keep all private/internal listeners on loopback.
- [ ] Run examples through `cargo run -- doctor` against a temporary installation instead of testing prose text.

### Task 9: Verification, review, commit, and bulk push

- [ ] Run `cargo fmt --check`.
- [ ] Run `cargo clippy --all-targets --all-features -- -D warnings`.
- [ ] Run `cargo test --all-targets --all-features -- --test-threads=1`.
- [ ] Run `cargo build --locked --release`.
- [ ] Run the pinned sidecar build/contract test and inspect its recorded digest.
- [ ] Run `rg -n 'unsafe\s*\{' src tests` and confirm none; confirm every first-party target has `forbid(unsafe_code)`.
- [ ] Run `git diff --check`, inspect `git status --short`, and review the full diff against every global constraint.
- [ ] Dispatch one independent read-only Luna review over the final working-tree diff; fix every valid Critical/Important finding with a failing regression test first.
- [ ] Repeat the full verification after review fixes.
- [ ] Create one coherent commit and push `main` once. Do not add hosted automation.

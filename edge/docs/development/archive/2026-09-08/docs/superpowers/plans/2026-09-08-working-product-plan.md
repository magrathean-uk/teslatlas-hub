# Teslatlas Edge Working Product Execution Plan

**Latest user model policy (2026-09-08):** This coordinator and delegation use `gpt-6-astra` with `thinking=high`. This product task, coding, goal execution and all development workers use `gpt-5.6-terra` with `thinking=high`. This supersedes every earlier model instruction in this plan and its historical goal snapshot. Preserve checkpoints at model transitions and verify the actual new turn model.

> **Evidence checkpoint:** 2026-09-08T17:46:09Z. Edge `main` is at
> `67650cd989f25835bd185d0d8c0ff4e3c8ff1bb2` with the authorized unfinished
> Edge changes preserved. The saved execution goal is currently `blocked`; it
> has no token or elapsed-time budget. Its native Resume control is separate;
> ordinary authorized Edge development continues under the current model policy.

## Current continuation control

Planning and coordination now use `gpt-5.6-terra` with `thinking=max`; this Edge product turn and its development subagents now use `gpt-5.6-terra` with `thinking=high`. The native goal remains blocked. With the available tools, resumption requires the user's supported Resume goal control. The parent can dispatch ordinary authorized development turns now; this does not activate the native goal. Do not replace an unfinished goal, mark it falsely complete, or edit internal app state. Continue READY owned work without waiting for native resumption; retain the full proposed objective and acceptance gates.

## Remaining objective

Deliver Teslatlas Edge as the optional working Fleet Telemetry ingress and Hub
bridge. Preserve public Edge delivery wire v2 and supported v1 behavior while
finishing occurrence-bound ACK deletion and guarded storage migration,
single-writer and monotonic-lineage safety, bounded batching/capacity/shutdown,
receiver retransmission, and mTLS/bearer recovery. Then establish real local and
installed Edge/Hub acceptance, validate the simple Edge/receiver Docker path,
reconcile current Edge documentation and AGENTS guidance, obtain product review,
and commit/push the final Edge source and documentation only.

The finished product must keep Tesla account credentials and vehicle-command
capability out of Edge, use encrypted bounded at-least-once spooling, preserve
unrelated work in every independent `main` checkout, and report local tests,
installed acceptance, Docker acceptance, and passive vehicle evidence as
separate claims.

## Current evidence

| Boundary | Current evidence | Exact limit |
| --- | --- | --- |
| Edge correctness | 73 Rust tests passed on Rust 1.98. Format 3 binds ACK receipts to the original occurrence, guards format-2 migration, excludes a second writer, keeps sequence lineage monotonic, bounds delivery selection, distinguishes capacity readiness, and gives service shutdown margin. | Refresh affected gates after review changes. A prior green run is not installed acceptance. |
| Edge matrix package | `tools/interop/client_lanes/edge.py` plus `edge_contract/` publishes 16 cases, six actors, 31 phases, six closed schemas, and literal ten-record/gap vectors. Focused coordinator and predicate tests passed 6 + 7. | Hub owns runtime launch, observers, fixed registry admission, and installed receipts. |
| Matrix bindings | Coordinator `e8af024c7e18534f2fe7f8f422a07db6295a0b75eaad79df1a45f0bada89be45`; manifest `328c62d724c96011ba06c4d1fae6b362dae4e0e740323bf17e8ad3d8cf8915`; validator `bf1219b1766e3f2af0f90d45903e961117bcce3275fc2e0424dd6e146851605f`; phases `15000c3bfdc02578426c8484f2cc96f2152f9a89d66cb70a1862ef675a98dbf5`; vectors `d40a853cd02111ff200083be81588526dc7d2cf28c3daa168d2ea7dd9dda6eb1`. | Recompute and coordinate any binding change; never edit Hub ledger from Edge. |
| Protocol and Hub | Edge profile review accepted manifest `e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224`. Hub Task 9 is approved with its report correction. Current Hub normal and `edge-test-faults` `edge_delivery_e2e` lanes each passed 2/2 against the Edge release binary. | These are real local processes and SQLite, not installed packages or the complete target matrix. |
| Receiver bridge | An exact Go 1.27.0 disposable run of the pinned v0.9.4 dispatcher admitted one record after duplicate submission and after a dropped response followed by explicit retransmission. | It starts after receiver decoding. Current host Go 1.27.1 fails the exact pin; vehicle TLS/parser provenance remains pending. |
| Packaging | Native units, format guard, Dockerfile, Compose file, operations guide, and pinned bridge build inputs exist. `docker-compose 5.5.1` parsed the Compose source. | No Docker daemon was available, so no image or container lifecycle passed. |
| Installed integration | Hub shared I1-I18 source rereview passed with no material source residual. Hub installed source gates report 176 interop and 132 installed-host tests. Edge fault binaries exist. | `task10_edge_fixture_design` is `design_accepted_bounded_implementation_pending`; no required installed Edge row has passed. ARM64 remains quarantined after protected-config observation failed. |

Hub's current ledger is `hub/docs/compatibility/execution-state.json`:
top-level status `active`, candidate `2026.36.2`, publication `false`; Hub
execution is `bounded_source_and_local_gate_execution_complete_installed_live_and_publication_pending`.
The ledger is read-only input to this Edge plan.

## Ownership and execution rules

- Edge execution owns only `/Users/bolyki/dev/source/teslatlas-service/teslatlas-edge`.
  Hub owns its consumer/store, installed fixture, registry, target hosts, shared
  evidence, and compatibility ledger. Protocol owns public schemas and vectors.
- Use the existing independent `main` checkouts. Before every edit or Git phase,
  recheck HEAD and status. Do not reset, clean, stash, or overwrite unrelated
  changes.
- Do not operate a live Tesla account, register/reconfigure a vehicle, send a
  vehicle command, mutate a VPS, or use production credentials. Passive vehicle
  proof proceeds only through an already authorized source.
- Keep Hub-link and receiver credentials private and separate. Do not commit
  tokens, private keys, raw telemetry, private runtime state, or evidence bundles.
- GitHub is source storage. The last milestone may commit and push reviewed
  source/docs only; it must not add CI, create a release/tag, or upload artifacts.
- Do not search `vendor/`, `upstream/`, `repository-roots/`,
  `fixtures/teslamate-postgres/`, or `TeslatlasCore.xcframework/`. App is outside
  this plan and outside AGENTS reconciliation.

## Milestones

| ID | Status | Owner task | Deliverable and verification |
| --- | --- | --- | --- |
| M1 | **DONE** | Edge goal `01a07f89-57a9-7831-9162-c1dede446ee9` | Edge Tasks 2a-2d, matrix contract publication, dispatcher-to-Edge bridge lane, and current Hub normal/fault local lanes. Evidence is bounded as described above. |
| M2 | **DONE** | Edge goal `01a07f89-57a9-7831-9162-c1dede446ee9` | Independent current-tree product review and immutable handoff manifest completed without a new source defect. Exact identities and all untested boundaries are recorded under `.superpowers/sdd/2026-09-08-working-product-plan/`. |
| M3 | **WAITING** | Hub task `01a07f89-45cb-7ea2-b96e-aa89de18fd14`, coordinated by parent `01a07048-a05c-72a2-9c7a-87dd59f52e9b` | Implement/review the accepted Edge installed fixture, recover usable targets, pass one installed Linux core gate, then run required macOS arm64 and Debian 13 arm64/amd64 Hub target cells with normal and fault actors. |
| M4 | **WAITING** | Edge goal plus parent `01a07048-a05c-72a2-9c7a-87dd59f52e9b` | Build and run the existing simple Docker solution on a daemon-equipped Linux host; prove permissions, ingress, Hub drain, restart/replacement, rotation, capacity, and shutdown with persistent state. |
| M5 | **WAITING** | Edge goal, final review by parent | Reconcile README/operations/compatibility/AGENTS claims to accepted evidence, complete source and product review, stage exact owned files, then commit and push Edge source/docs only as the final action. |

M2 completed on 2026-09-08 with a Terra/high independent review: specification
compliance passed, task quality was approved, and no handoff-blocking finding was
reported. M3 fixture work can proceed under Hub ownership once the Hub task is
active. Documentation drafting in M5 may use confirmed M1/M2 evidence, but
installed/Docker support claims remain placeholders until M3/M4 receipts exist.

## Dependency ledger

| Dependency | Owner task ID | Missing input | Local work possible now | Unblock event |
| --- | --- | --- | --- | --- |
| Saved goal dispatch | Parent `01a07048-a05c-72a2-9c7a-87dd59f52e9b` | Goal status is `blocked`; its native Resume control is unavailable to ordinary development turns. | M2 source review and evidence handoff can proceed on Terra/high without native-goal mutation. | User resumes the native goal separately; this does not gate M2. |
| Exact receiver toolchain | Edge goal | Current host provides Go 1.27.1, while the bridge pins 1.27.0. | Source review and retained exact-1.27.0 evidence remain usable; Rust/Python gates can run locally. | Exact Go 1.27.0 is available or Hub accepts an immutable already-built bridge identity for the installed lane. |
| Hub Edge fixture | Hub `01a07f89-45cb-7ea2-b96e-aa89de18fd14` | Accepted design has no reviewed `hub/tests/interop/edge_installed` runtime implementation/entry point. | Edge can finish M2 and provide its fixed contract, release binary, and fault actors. | Hub fixture implementation and independent review pass, with exact invocation and observers published. |
| Installed targets | Hub task and parent | No required installed row has passed; ARM64 guest is quarantined; runtime identities and service access are absent. | Local process gates and package inspection can continue without changing a host. | At least one clean installed Linux pair is ready, then every required target exposes reviewed runtime inventory and least-privilege observation. |
| Docker runtime | Parent and Edge goal | Local daemon is unavailable. | Static Docker/Compose review and documentation corrections can proceed. | A daemon-equipped isolated Linux amd64/arm64 environment is supplied and the final source tree is staged there. |
| Passive vehicle ingress | Parent | No authorized passive vehicle-wire source and provenance capture. | Dispatcher-entry and installed normalized-envelope lanes can finish. | An already authorized passive route produces receiver TLS/parser evidence without vehicle commands or registration changes. This does not block synthetic working-product completion when reported separately. |
| Final source publication | Edge goal and parent | M2-M4 review/acceptance receipts and final diff are incomplete. | Draft docs can track confirmed evidence; no Git write is needed. | Required reviews pass, mandatory installed and Docker gates are accepted, exact staged diff is clean, and remote/upstream state is rechecked. |

## M2 - current-tree review and handoff

### Files and interfaces

Review the occurrence and migration seams in `src/spool.rs`, `src/delivery.rs`,
`src/admission.rs`, `src/health.rs`, and their focused tests. Review the whole
process bound in `src/runtime.rs`, native service descriptors, and
`scripts/run-with-spool-format-guard.sh`. Review `tools/interop/client_lanes/edge.py`
and `edge_contract/` as an immutable Hub-facing publication. Review Docker and
docs for contradictions that could invalidate later runtime acceptance.

The public interface remains `edge-delivery-v2@2.0.0`; ACK arrays keep independent
256 record-ID and 256 gap-ID caps. Format 3 receipts must delete only the original
`(spool_seq, stable_id, legacy_id)` occurrence. An ambiguous format-2 receipt must
fail visibly and preserve state. A second writer must fail before spool mutation.
An established spool with missing/corrupt sequence metadata must never restart at
sequence 1. Valid batch settings must drain 257 and 1,024 entries in ordered
prefixes without decoding every pending payload.

### Work

1. Recheck HEAD/status and classify every changed path as inherited, M1-owned, or
   unrelated. Do not edit a sibling checkout.
2. Inspect implementation and tests for interrupted receipt deletion, duplicate
   stable IDs at later sequences, wrong-key/corrupt/missing state, lock release,
   record/gap ACK mixtures, admission saturation, readiness recovery, and forced
   versus graceful stop.
3. Review configuration and docs together: default and hard budgets, lazy
   retention, auxiliary receipt/gap/quarantine headroom, ten-second initial
   manager allowance, startup-loaded credentials, and backup/rollback lineage.
4. Run focused tests first. If review changes code, run affected package and
   contract gates; then run `rustup run 1.98.0 cargo test --locked`, formatting,
   six coordinator tests, seven predicate tests, and the release build. Run the
   bridge script only with exact Go 1.27.0. Keep a missing exact toolchain as a
   refresh gap rather than weakening the pin.
5. Recompute the five Edge matrix digests. Any change requires Hub coordination
   before installed execution. Produce a manifest with Edge HEAD, dirty source
   hashes, Rust/Go identities, release/fault binary hashes, profile digest,
   commands/results, and explicit untested boundaries. Keep machine-specific
   evidence outside Git.
6. Obtain an independent Terra/high review of occurrence deletion, migration,
   single-writer/lineage behavior, bounded batch/capacity/shutdown, and the
   coordinator. Resolve critical/important findings before M3.

M2 passes when the reviewed current tree has no unresolved critical/important
product finding, affected local gates pass, and Hub receives immutable identities
for the exact binaries and Edge matrix package. Local success does not change an
installed or vehicle-wire status.

## M3 - installed core and target matrix

Hub first implements the accepted root-owned synthetic fixture rather than using
the generic API-only seed. It must configure a real `collector.edge`, launch
normal and `edge-test-faults` actors separately, retain encrypted producer
checkpoints, observe Hub store/frontier/receipt state, and emit fail-closed
receipts bound to runtime, source, binary, case, and phase.

On the first supported installed Linux Edge/Hub pair, verify:

- dispatcher submission, durable Edge ACK boundary, Hub mTLS/bearer pull, applied
  telemetry, disposition, receipt, and contiguous frontier;
- replay, dropped ACK response, Edge-only restart, Hub-only restart, receiver
  restart, process crash boundaries, and retry without spool deletion;
- re-enqueued stable event at sequence 2 surviving an old sequence-1 ACK, its
  duplicate disposition advancing the frontier, and sequence 3 then applying;
- 256/257/1,024 record and record/gap prefixes, queue full/recovery, corruption
  and ordered loss evidence, missing/corrupt sequence refusal, and second-writer
  rejection;
- absent/untrusted/expired client certificate, wrong server trust/name, revoked
  bearer, bounded rotation overlap, and recovery without frontier movement on
  failure;
- service-manager stop/start, receiver-first stop and Edge-first start, measured
  graceful stop, forced-stop recovery, no remaining listener/child, and durable
  inspection of every acknowledged admission.

Use the accepted fixture's 16 case IDs and add only required occurrence/batch
boundary cases. Preserve normal checkpoint counts `1,1,2,5,6,7` at phases
`1,2,3,6,8,10`, with final Soc 82, InsideTemp 22.75, and power -4.0. Keep the
historical sparse-parity Soc 80 dataset separate.

After the first installed Linux gate passes, repeat meaningful cells against
macOS arm64 and Debian 13 arm64/amd64 Hub targets. Record native versus emulated
execution, normal/fault identities, expected/actual values, shutdown time, and
every pending or blocked row. Do not promote source tests or a prepared package
into an installed pass.

## M4 - Docker acceptance

Use the existing one-image, two-service design. Edge runs as UID/GID
`10001:10001`; receiver shares Edge's network namespace but not PID or filesystem
namespaces. Keep spool/key/Hub credentials, receiver bearer, vehicle TLS, and
configuration in separate owner-only mounts. Publish only selected Hub-link and
raw TCP vehicle ports; never publish admission `8080` or put secrets in build
arguments, environment values, image layers, history, or logs.

On isolated Linux, run Compose configuration, image build, fresh initialization,
`doctor`, and startup. Verify linkage, provenance/notices, ownership/modes, format
guard, ports, receiver inability to read Edge private mounts, Edge liveness and
readiness, and receiver process/listener state. Then pass dispatcher-to-Edge-to-
real-Hub delivery, outage/backoff drain, Edge and receiver restarts, coordinated
replacement with persistent mounts, full stop/start, token rotation, old ACK
replay, capacity recovery, wrong-key/format refusal, and bounded stop. Repeat on
amd64 and arm64, naming emulation. Compose parsing alone does not pass M4.

## M5 - documentation, review, and source-only Git phase

Reconcile `README.md`, architecture, delivery contract, native/Docker installation,
upgrade/backup/recovery, versioning, compatibility metadata, examples, and Edge
AGENTS guidance. State verified defaults/hard caps, independent ACK caps, body
limits, headroom, intended VIN/consumer scope, credential reload/restart rules,
lineage/rollback limits, health meanings, and the boundaries between receiver
decoding, durable Edge admission, Hub application, installed acceptance, Docker
acceptance, and passive vehicle proof.

Run link/command/provenance checks and affected tests. Obtain separate review of
receipt occurrence/migration, storage recovery, raw installed failure evidence,
Docker lifecycle, privacy, and support claims. Recheck `main`, status, remote, and
upstream; stage exact owned files/hunks and inspect for secrets/artifacts/unrelated
edits. Only after M2-M4 acceptance and review, commit and push Edge source/docs to
the verified Edge repository. Do not force-push, configure CI, tag/release, or
upload binaries/evidence.

## Completion gate

The goal is complete only when M2-M5 are accepted, every mandatory installed
target and Docker architecture row has an evidence-bound result, final docs match
those results, critical/important review findings are closed, and the final
source/docs-only Git result is recorded. Passive real-vehicle evidence may remain
explicitly pending only as a separately scoped provenance claim; it must never be
reported as proven by dispatcher or normalized-envelope tests.

# Teslatlas Edge Implementation Plan

> Active model: **gpt-5.6-luna / max**. The owner has started development; old G0/hold paragraphs below are historical.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a usable Fleet Telemetry Edge service, receiver and durable Hub forwarding path on Debian 13 ARM64 and supported Apple-silicon macOS through the existing Hub bootstrap. Complete the minimum bootstrap/install/service integration and active-target reliability/lifecycle work first; polished packaging follows. x86, Intel Mac, Azure and the complete cross-architecture matrix are deferred and non-blocking.

**Architecture:** Keep Rust Edge and the pinned Fleet Telemetry receiver sidecar. The receiver owns vehicle mTLS and posts strict envelopes over loopback; Edge owns encrypted bounded spooling and Hub mTLS/bearer pull/ack; Hub owns deduplication, durable commit ordering, and gap disposition. Edge never holds Tesla account credentials or vehicle-command capability.

**Tech Stack:** Rust `1.98`, edition 2024, Tokio/Axum/rustls, XChaCha20-Poly1305 encrypted spool, Go `1.27.0` bridge, systemd, launchd, Debian packages, Docker Compose, Debian 13 slim.

**Spec:** `/Users/bolyki/dev/source/teslatlas-service/docs/development/PRODUCT_SPEC.md` and `/Users/bolyki/dev/source/teslatlas-service/docs/development/MASTER_PLAN.md`.

## Active execution scope — owner restart 2026-09-12

The 2026-09-10 global stop is lifted. The workspace
[ACTIVE_SCOPE.md](../../../docs/development/ACTIVE_SCOPE.md) supersedes older
strict R1-first, full-matrix and platform-order wording for execution. Active
targets are the existing Debian 13 ARM64 and supported Apple-silicon macOS
lanes. The first product aim is a usable Edge service, receiver and forwarding
path, followed by the minimum install/service and reliability work needed on
those targets.

The existing native Edge goal is preserved; its current `get_goal` control
state is paused and no duplicate goal was created. Product execution resumes
under this owner restart for the active targets. The earlier read-only guest
preflight was unavailable, but Hub has now independently verified the exact
macOS 13 receiver candidate on the host and supported macOS 13.7.4 ARM64 guest,
then stopped the guest with no receiver process or heavy-build lock. Edge has
prepared a fresh source-validated readiness-only proposal bound to that
candidate. Hub completed the bounded receiver-only readiness run after the
startup-bearer correction: `/status` returned `200 ok`, only loopback
`21444`/`21445` were owned, no event or Edge process ran, and cleanup completed.
The receiver-only handoff and run are complete. Edge has now drafted and
corrected the complete source-bound forwarding contract at
`docs/development/edge-macos-forwarding-contract-proposal-2026-09-12-r1.json`;
its fresh source-to-darwin-arm64 receipt records the exact dirty-tree
compilation manifest and locked Rust build. Hub review, fresh runtime-generated
inputs and separate submit-once authorization remain outstanding.

The coordinator's Viewer scope change is effective for this Edge plan:
Viewer development, packaging, integration, acceptance and dependencies are
excluded from current Edge execution. The native Edge objective was verified
unchanged and contains no Viewer clause. The current goal controls expose no
supported amend/cancel operation, so this exclusion is recorded as an
effective planning/execution override rather than a native-goal mutation.

Deferred targets remain paused for every Edge milestone: x86, x86_64, amd64,
Intel Mac and Azure development, compilation, image acquisition, VM
provisioning, runtime/lifecycle tests, cleanup and acceptance. The complete
cross-architecture matrix is also deferred. Existing x86 code, schemas,
candidate identities, receipts, the `debian13-x86_64-20260909` VM and its
artifacts remain preserved and unadmitted; they must not block active work or
be used until a new owner direction.

Named-source TeslaMate admission, parity, import and passive provenance remain
explicit acceptance gaps, but missing source inputs do not stall independent
ARM64/Mac working-product tasks. Use a fresh Hub-owned bounded handoff only
when an actual remaining active-target integration gap requires it.

## Prepared development VMs

Use [VM_ACCESS.md](../../../docs/development/VM_ACCESS.md) for the shared login, SSH, transfer and lifecycle instructions, and [ENVIRONMENT.md](../../../docs/development/ENVIRONMENT.md) for storage policy. The active guests are `debian13-arm64`: Debian 13.6 ARM64, 4 CPUs, 4 GiB RAM, 32 GiB disk; user `bolyki`, SSH alias `teslatlas-debian13-arm64`, endpoint `127.0.0.1:60022`; and `macos13-arm64`: macOS 13.7.4 ARM64, 4 CPUs, 4 GiB RAM, 50 GB disk; user `admin`, alias `teslatlas-macos13-arm64`. The wrapper resolves the Mac guest's changing NAT address.

From the workspace root, use `scripts/dev/vm.sh debian ssh uname -m` or `scripts/dev/vm.sh mac ssh sw_vers -productVersion`. Both use the private lab key. The SSH config is `~/dev/teslatlas-lab/access/ssh_config`; the Mac GUI password is in private `access/credentials.json`, outside Git. Work on the two active ARM64/Mac guests first and keep polished distribution late. There is no active Colima VM; provision Docker in the primary Debian guest only when an active ARM64 step needs it. Do not use or alter the preserved x86 guest/artifacts. Retained baseline/quarantine disks are not current development targets.

## Global Constraints

- Product development and tests are authorized; follow [START_AUTHORIZATION.md](../../../docs/development/START_AUTHORIZATION.md) and the current task model/resource policy.
- Preserve this independent `main` checkout and all unrelated dirty paths; no branch, reset, clean, stash, commit, push, publication, or binary upload without later authorization.
- Active environments are Debian 13 ARM64 with the existing Hub bootstrap and supported Apple-silicon macOS 13 ARM64. x86/x86_64/amd64, Intel Mac and Azure rows are paused and cannot block this plan.
- Pin Rust `1.98` and Go `1.27.0`; record compiler, libc, image, architecture, service-manager, package, and bridge identities.
- Keep `edge-delivery-v2@2.0.0`, wire versions 1/2, spool format 3, occurrence-bound receipts, bounded resources, and no automatic v1 downgrade.
- No vehicle commands, account credentials, production receiver credentials, registration/reconfiguration, or passive-vehicle claim from synthetic envelopes.
- Hub owns bootstrap/catalog/fixture/registry/runner/observers/aggregate installer/ledger. Edge requests those changes through a bounded handoff.

## Current state and purpose

The source-only synthetic vehicle identity generator now emits a fresh
`Tesla Motors Products CA` client profile with the VIN as leaf common name
and the required client-auth extensions. An exact pinned-verifier harness
covers accepted, wrong-issuer and missing-identity-OID cases. This remains
source/application evidence only; the consumed r12 live authorization cannot
be retried.

The coordinator-authorised r12 path is closed as a bounded receiver-identity failure and is retained only as evidence; it cannot be replayed. A fresh r13 cohort then used a corrected `Tesla Motors Products CA`/VIN client profile and completed exactly one `VehicleName=synthetic-b2` submission on topic V, transaction `edge-r13-authorized-20260909-v-001`, timestamp `1788933999000` ms. The receiver ACKed, Edge drained its encrypted queue, Hub committed the lineage and reached ACK frontier 3, and HA observed only the temporary device display name with no numeric sensor/value/units. The r13 fixture and temporary HA entry were cleaned in the required order while primary lanes remained untouched.

Edge `main` is `67650cd989f25835bd185d0d8c0ff4e3c8ff1bb2` with 51 dirty paths. Rust Edge, the pinned v0.9.4 bridge, guarded spool migration, occurrence-bound ACKs, native units, Docker source packaging, and matrix schemas exist. Current evidence includes the Hub-coordinated Debian 13 ARM64 primary process path, the successful r13 receiver/Edge/Hub/HA path, an installed ARM64 Debian package lifecycle, an ARM64 Docker one-image/two-service runtime, a live Docker bearer expiry/rotation/revocation subset, and a macOS 13 ARM64 package/lifecycle subset: real receiver mTLS WebSocket, loopback admission, encrypted spool, duplicate disposition, Hub outage, Edge restart, Hub drain/ACK, package upgrade/stop-order, Docker restart, macOS LaunchAgent restart, mount isolation, secret scanning, and data-preserving uninstall. The fresh r9 Docker-to-Hub/same-Hub HA cohort reached trusted Hub/HA readiness, but its one authorized receiver attempt failed before admission because the selected helper carried a stale producer identity and did not present the required client certificate; its disposable runtime is stopped with no queue, ledger mutation, frontier advance, or HA value. The Edge-owned identity-bound helper invocation repair now derives VIN from the sealed producer manifest, rejects embedded VINs, requires distinct TLS paths, and passes source/build tests without launching a runtime. The source-only Hub consumer-input preparer now requires the issuing server CA and an explicit expected server IPv4 address, accepts exactly one matching server IP SAN while retaining DNS hostname verification, rejects wildcard/loopback/other IP SANs for the non-loopback target, and emits owner-only outputs; its focused contract passes on the host's LibreSSL toolchain. ARM64 and Apple-silicon Mac working-product integration is the active next lane. Named-source parity/passive provenance and the remaining active-target lifecycle gaps remain open acceptance work; x86/Intel/Azure/full-matrix rows are preserved and deferred, not blockers. The Docker bridge build retains target mapping but only ARM64 execution is active.

Owned paths are `src/**`, `tests/**`, `tools/interop/client_lanes/**`, `packaging/**`, `scripts/**`, `Dockerfile`, `.dockerignore`, `compose.yaml`, and Edge docs. Hub owns fixture, runner, registry, aggregate package/wrapper, target observers, and ledger.

## Milestones

The first two local work packages belong to the master's B2 stage; the Hub
prerequisite completes its own master B1 work, with no Viewer dependency in
Edge. Under the owner restart, active Edge
work is the usable Debian ARM64 and Apple-silicon Mac product path, followed by
its minimum service/install and reliability work. Independent owned corrections
can proceed once the owner starts development. Cross-product checks consume
the other product owner's receipts; this task does not implement or rerun
sibling suites. Deferred x86/Intel/Azure/full-matrix work is not an active
dependency.


### B2.1 — Existing bootstrap: Debian 13 ARM64 Hub + Edge actual user path

**Purpose:** Get the selected Edge service working in the same primary environment as Hub using the existing bootstrap and minimal compatibility correction.

**Dependency:** Hub supplies a minimal local-candidate/accepted cohort for Edge, a disposable `collector.edge` fixture with `edge.delivery.v2`, and private mTLS/bearer/session inputs. Do not wait for polished `.deb`, `.pkg`, Docker matrix, or 21-cell schema.

- [x] Recheck status/HEAD and record Rust/Go/Debian ARM64 identities, Edge/profile/bridge/contract hashes, and private state paths.
- [x] Extend the existing Hub companion selection with `edge` only when explicitly selected; build the Rust binary and pinned receiver for native ARM64, copy only approved support files, and initialize a private state root.
- [x] Start Edge and receiver with separate owner-only credentials/config/TLS paths. Prove loopback admission, encrypted spool write, Hub mTLS/bearer pull, durable Hub application, exact ACK, and one synthetic record through the real process path.
- [ ] Keep Hub changes limited to selection/admission, fixture/session values, and the smallest collector.edge integration needed by this path.

Run: `rustup run 1.98.0 cargo test --locked --all-features`, exact `scripts/test-fleet-telemetry-bridge.sh` with Go `1.27.0`, and Hub-coordinated primary Edge smoke. Expected: ARM64 receiver binary matches host, admission ACK follows durable spool write, Hub ACK follows durable commit, no secret/payload appears in logs, and deselected Edge does not start.

### B2.2 — Selected HA/Edge helpers and SDK resources in the primary environment

**Purpose:** Work with selected Home Assistant and Edge helpers in the active ARM64/Mac product lanes without coupling their credentials or storage.

**Dependency:** B2.1 bootstrap and current-Hub fixture. HA is an explicitly selected HA OS/Container or existing HA instance; Edge remains the only service in this root.

- [ ] Record selected Protocol/SDK resources only when a selected companion needs them; Edge itself consumes the Edge delivery profile, not the browser SDK.
- [ ] Coordinate with HA so its read-only integration sees the same Hub current state while Edge telemetry arrives through its own mTLS/bearer path. Verify no cross-read of state, bearer, TLS key, or spool.
- [x] Run Edge receiver-first stop and Edge-first start on the primary host; preserve the same state root and Hub frontier.
- [ ] Add only the minimum contract/fixture correction necessary for selected helpers; leave package/distribution design for D1.

Named checks: Edge `tests/admission_contract.rs`, `tests/delivery_contract.rs`, `tests/mtls_contract.rs`, `tests/packaging_contract.rs`; HA selected smoke; Hub receipt validator. Expected: selected helpers coexist and unselected roles remain absent.

### R1 — TLS, ACK, crash, durable data, and parity/recovery reliability

**Purpose:** Prove real Edge behavior and data semantics on the active ARM64/Mac product lanes before polishing packaging.

**Dependency:** B2.1/B2.2 actual primary runtime.

- [ ] Exercise absent/untrusted/expired client certificate, wrong server trust/name, revoked/expired bearer, rotation overlap, duplicate receiver submission, dropped ACK response/retransmission, Hub outage/backoff, Edge crash, receiver crash, Hub restart, and clean five-second drain under the ten-second supervisor margin.
- [ ] Prove encrypted spool/key ownership, exact `(spool_seq, stable_id, legacy_id)` ACK occurrence, later duplicate safety, contiguous v2 prefix/gap behavior, v1 receipt pruning, format-2 migration/refusal, wrong key, missing/corrupt sequence, orphan temp, second writer, capacity/readiness recovery, and backup/restore lineage.
- [ ] Keep the named-source TeslaMate comparison, import, passive provenance and derived-value evidence explicit when the required source inputs arrive. This acceptance gap does not block independent ARM64/Mac working-product usability.
- [ ] Keep passive vehicle evidence separate and use only an already authorized route; synthetic receiver input proves the dispatcher boundary only.

Run: full Rust suite, exact pinned bridge test, Hub normal/fault Edge E2E, and receipt validation. Expected: no ACK/frontier advance occurs before durable commit, no pending occurrence is deleted by an old ACK, and every crash/recovery result is hash-bound.

### D1 — Debian ARM64 package, selectable Apple-silicon Mac services, and ARM64 Compose

**Purpose:** Make the active ARM64 and Apple-silicon Mac products installable and serviceable after the working paths are usable. Keep polished distribution late.

**Dependency:** Active ARM64/Mac working-product evidence and the package-selection contract from Hub. Named-source parity and deferred x86/Intel/Azure/full-matrix rows are not prerequisites.

- [ ] Add Debian ARM64 metadata for Edge and receiver, configs, format guard, systemd units, modes/owners, preflight/init/doctor, upgrade/rollback, and service ordering. The x86/amd64 package row is paused and non-blocking.
- [ ] Add a visibly deselectable Apple-silicon macOS `.pkg` with independent Edge-core and Fleet-receiver LaunchAgent components, fixed state/config paths, development-only isolation warning, and safe start/stop/upgrade/uninstall. Intel Mac packaging is deferred. Never silently add public ingress or credentials.
- [x] Fix the Docker bridge target: replace the hard-coded `--target linux-amd64` with an architecture mapping/build argument that produces a receiver binary matching the image. Preserve upstream/patch/Go/CGo/checksum locks; execute only the ARM64 target under the current scope.
- [x] Preserve one image/two services, UID/GID 10001, separate state/receiver-secret/Hub-TLS/vehicle-TLS/config mounts, loopback-only 8080, selected Hub/raw TCP ports, health/readiness, bounded stop, and no secret in image layers/logs.
- [ ] Request Hub aggregate installer/profile changes only for selection, wrapper, and package orchestration; Edge owns payloads and support docs.

Run: `rustup run 1.98.0 cargo test --test packaging_contract`; `sh -n scripts/build-fleet-telemetry-bridge.sh scripts/test-fleet-telemetry-bridge.sh scripts/run-with-spool-format-guard.sh`; `docker compose config --quiet`; ARM64 bridge/build checks and the supported Apple-silicon package/service checks. Do not run x86/amd64 bridge builds or acquire an amd64 image under this scope. Expected: active package/service choices, file modes, image architecture, receiver linkage, mount isolation, and ports agree.

### X1 — Active ARM64/Mac lifecycle and later final acceptance

**Purpose:** Validate the usable Debian 13 ARM64 and supported Apple-silicon macOS lifecycle, then deliver source only. The complete cross-architecture matrix is deferred.

**Dependency:** D1 active-target packaging, Hub registry/fixture/observers where needed, existing active guests, and the applicable product inputs. Deferred x86/Intel/Azure targets are not dependencies.

> Deferred and preserved: Debian 13 x86_64 native/package/Docker, Intel Mac,
> Azure clean hosts, and complete cross-architecture matrix rows. Do not build,
> install, test, clean up or accept them during this scope.

- [ ] Record OS/kernel/architecture, native/emulated status, Rust/Go, package/image digests, UID/GID, mount/file modes, service-manager state, listener topology, config/profile, and empty state before init/doctor for Debian ARM64 and Apple-silicon Mac only.
- [ ] Exercise normal/fault delivery, receiver/Edge/Hub restart, package/image replacement, upgrade, interrupted upgrade, rollback, receiver-first stop, Edge-first start, forced stop, clean stop, credential rotation, wrong key/format, capacity, old ACK replay, and cleanup.
- [ ] Verify passive vehicle provenance only through an authorized route; never send vehicle commands or register/reconfigure a vehicle.
- [ ] Reconcile docs/compatibility to receipts. After explicit authorization, stage only Edge source/docs/package/image recipe paths for clean-host delivery; no CI, release, tag, registry, or binary upload.

## Shared gates

G0 is the planning/environment hold. G1 is minimal Hub + Protocol contract and
fixture admission for B1. G2 is actual Edge runtime in Debian ARM64. G3 is
selectable active-target distributions and ARM64 Compose in D1. G4 is the
active ARM64/Mac lifecycle subset in X1. G5 is passive/parity/recovery
evidence. G6 is clean-host review and source-only delivery. x86/Intel/Azure and
the complete cross-architecture matrix are deferred backlog rows and are not
early or active prerequisites.

## Ready versus waiting

Current restart state (2026-09-12): execution is resumed for Debian ARM64 and
Apple-silicon Mac under the owner restart. The actual native goal-control state
is paused, with no duplicate goal. Hub's exact c2ace receiver loader gate and
receiver-only readiness gate have passed on host and macOS 13.7.4 ARM64 guest.
Edge has drafted the complete source-bound forwarding contract; the immediate
blocker is now the rejected and closed producer cohort, not Hub preparation.
Hub accepted the contract, prepared one fresh empty baseline, and cleaned the
runtime after the coordinator rejected the cohort before producer invocation.
Edge has not created inputs, started or mutated either guest.

The redacted certificate-profile reconciliation is recorded in
`docs/development/edge-macos-producer-certificate-profile-verification-2026-09-12-r1.json`
(SHA-256 `b8e64d764252581c50a13b6e046399729aa3a755e317dfc64a8f856862658407`).
The immutable public leaf is PEM SHA-256
`fcf01266e005309904d36d67a407303497db4280723f261a024cae165fb0c6ba` and DER
SHA-256 `3f7f6207e7716bca25cd2dafad5ca17df617d2c432a09e1f6f2cf856167d0f58`;
the earlier `fcf012de...` handoff value was a recording error. Issuer/profile,
clientAuth EKU, key correspondence, exact receiver CA trust and pinned-parser
identity checks passed. The pinned parser uses Subject.CommonName for this
issuer and does not require/read a VIN SAN, so the earlier SAN failure was an
inapplicable expectation rather than a parser mismatch. The cohort remains
closed and cannot be retried or replaced; no producer event was sent. The
mutable handoff digests `4fd3ed7237f7ae15e83176d611d76e0761fb4abcb4170f462cfa562bfb5a52eb`,
`a9007816bcbee9f63751600a0869acd0218153ce863bdde52d14f7f07019df52` and
`8aadc454966d9cf692fe32260ddf531d8594e5a26c56902424901f8873c278fc` are
historical only. Edge directly verified and bound the reconciliation to the
immutable snapshot
`hub/docs/development/edge-macos-empty-baseline-handoff-2026-09-12-r1-final.json`
(SHA-256 `df9f19a5f8368beafa88068f34c4239cf9ee4294bb6885fb97de76707b07939b`).
Coordinator has authorized Hub to prepare one wholly fresh bounded
empty-baseline cohort later. Before any fresh runtime launch, Edge must
machine-hash the exact staged PEM and DER bytes and independently verify the
sealed CommonName identity, issuer, clientAuth EKU, key correspondence and
exact receiver CA. No fresh credential or runtime is being started now.

The fresh r2 source, identity/profile and zero-event baseline package is
recorded in
`docs/development/edge-macos-forwarding-contract-proposal-2026-09-12-r2.json`
(SHA-256 `cc21ae3f0480f0f574502f7a7d69aa1f4fcbeb50d2255946471b6026d5288928`)
and
`docs/development/edge-macos-r2-source-candidate-inventory-2026-09-12.json`
(SHA-256 `1a46d6057d52c82198393bfba2d9908c44edd136216af6b57f49abd1a33e2584`).
It binds the fresh Edge arm64 build receipt
`docs/development/edge-macos-arm64-source-binary-receipt-2026-09-12-r2.json`
(SHA-256 `aead774fcec533eb65f71e48bd0f38d141aa7be63cd9beb195420d304406a309`),
fresh receiver candidate receipt
`docs/development/edge-macos-r2-receiver-candidate-receipt-2026-09-12.json`
(SHA-256 `45a7e4b14fae9f6caade094831abd1a760299116b97a5c76b129de61aa71c2f9`),
and the redacted pre-runtime certificate/profile receipt
`docs/development/edge-macos-r2-certificate-profile-receipt-2026-09-12.json`
(SHA-256 `6897a292b5a0d11081319be7bc53b1d13272bc81b3800d8748fe1805b421740e`).
The exact staged public certificate is PEM SHA-256
`44a7c93772422f29cab0574fb9a224a07228fd94a4f0f2b71f9095f7e49cd58a` and DER
SHA-256 `27a772da81b8d73f135e8234a005f2b6368a4b92f79bebf327842c0138ba1e3c`;
the exact receiver CA is SHA-256
`0ab9ee01765c8d72d0b180948ebc4823f48d160c0c2d100b951ce97e2932e5f6`, and the
sealed binding is SHA-256
`245d96da6a34f671759de0296d24efd33b7244ca6df24a9c5486f78bdb020091`. The
pinned CommonName parser policy, issuer/profile, key correspondence and
sslclient verification all passed; SAN is absent and not required by that
policy. The source-candidate manifest is SHA-256
`a74718fb5f99db1f22004b155e503886cfd2ced0528fa18dbef7e2c3a4ab55d0`, the
staging manifest is SHA-256
`807e3d5fe305f4b2a387c29232dc8a7c7be72377787a6abbf8445b0f91d8a93d`, and the
Hub source receipt is SHA-256
`d7d3d2499370c4c344e3db77e06cd5db14ff954d86e29bd1ef68745a17767103`.
The Hub/coordinator-managed r2 empty baseline was independently reviewed over
five stable seconds, then closed with final handoff
`hub/docs/development/edge-macos-empty-baseline-handoff-2026-09-12-r2.json`
(SHA-256 `21f46025eb51344e5403b94e37aa937eb2e7f86c2ecf301ddd72821d1b352867`);
ports 8081/21844/21446/21447/18526 are closed, the guest is stopped and the
shared heavy-build lock is absent. No producer was invoked or event sent, and
the reserved transaction remains an identifier only. The r2 cohort is closed
and must not be restarted or reused.

Hub then rejected the source-only r3 actor contract for independent-review
failure: both claimed helper source paths under `/private/tmp` were absent, and
r3 named only a historical stopped-guest wrapper rather than stable wrapper
source and tests. The rejection is preserved in
`hub/docs/development/edge-macos-one-event-actor-contract-review-2026-09-12-r3.json`
(SHA-256 `d4dc413d276e0df6f9a0ef8e628a0e6b79127788822e0ad42b6667348a289f12`).
Edge has added the stable superseding r4 source-only package in
`docs/development/edge-macos-one-event-actor-r4/` and proposal
`docs/development/edge-macos-one-event-actor-contract-proposal-2026-09-12-r4.json`.
The fresh producer source/test hashes are
`d59c5e38245113bebdfe2635f1f3e42cfa4052f269d45d5fbd0b392a12697df2` and
`a60bdba6bb71da8af5e4ede33627affda55efbd813efe2705108211391eca40d`; the
fresh wrapper source/test hashes are
`4b072a2bc51d27c78fe1db84c9535d0fae7b4dc413b5e4c1eb9dc76a0e28084a` and
`1a48c87c90eedd32c7027dd6c06621a546b9d421274ac207fcddd931481e4fff`.
The producer pair passed its focused tests in an isolated overlay of the
pinned source snapshot, and the wrapper pair passed 6/6 browser-free,
no-network tests. The producer pair is explicitly a new review candidate, not
a byte-identical recovery of the historical binary; the old binary, r1/r2
roots, identities, credentials and transactions remain non-reusable. This r4
package grants no runtime or event authority: Hub must independently rehash,
inspect and stage it, build a fresh helper, create a fresh runtime and obtain a
separate coordinator submit-once decision before any actor action.

Hub's independent r4 review then found a guard defect: pre-existing result,
output or atomic-result temporary files, and output/input aliases, were not
rejected until after the guard/one timestamp and one child spawn. The r4
rejection is preserved in
`hub/docs/development/edge-macos-one-event-actor-contract-review-2026-09-12-r4.json`
(SHA-256 `932bde29461221aabf99a2e5dc00990c3f882923c380530846329e9086d3cf0a`).
Edge has kept the r4 producer source/test pair unchanged and added the
source-only r5 guard correction at
`docs/development/edge-macos-one-event-actor-r5/`, with proposal
`docs/development/edge-macos-one-event-actor-contract-proposal-2026-09-12-r5.json`
(SHA-256 `d205d56076798696715d501252f5f67f83fc6e6e07143ed2683ecc4f495ff49f`)
and receipt
`docs/development/edge-macos-one-event-actor-guard-correction-receipt-2026-09-12-r5.json`
(SHA-256 `26d93e0085e52dfca25517a7b8c613c846a3f64dc98e1beff0f7dae35a9bad2b`).
The r5 wrapper/test hashes are
`889d79fff2ec6154993a72a65c2161240df999666cd97bee9c34e1b9ecb51d1f` and
`ab95b8f1eb2bc8ed922209c3e648fdfd4b28d4646ce6d5a18ed405b0c296a983`;
focused local preflight tests pass 7/7 with no browser, network or runtime.
Existing output/guard/temp files and all tested aliases now fail before the
clock, guard claim or spawn while preserving sentinels. r4 remains rejected
history; no r4/r5 package grants event authority, and Hub must independently
rehash the additive correction before proposing any fresh runtime.

Hub's independent r5 review accepted the additive correction as source-only in
`hub/docs/development/edge-macos-one-event-actor-contract-review-2026-09-12-r5.json`
(SHA-256 `5dcbebee590d1e334abe8d2eaa27a5fb78aa6d62233206af2296bb09971202b0`).
It accepted the r5 guard/test hashes
`889d79fff2ec6154993a72a65c2161240df999666cd97bee9c34e1b9ecb51d1f` and
`ab95b8f1eb2bc8ed922209c3e648fdfd4b28d4646ce6d5a18ed405b0c296a983`, with the
r4 producer pair unchanged. Hub then completed the separately authorized
fresh source/test/build step under a fresh root and the shared heavy-build
lock. The source-to-binary receipt is
`hub/docs/development/edge-macos-one-event-actor-source-binary-receipt-2026-09-12-r5.json`
(SHA-256 `e5e8e87c5ecbf191ef30a46de4e5f55796b22d7b193a84da6784a9e6e28ec4d5`);
it binds the new darwin-arm64 Mach-O helper
`/Users/bolyki/dev/teslatlas-lab/candidates/edge-macos-one-event-actor-r5-20260912/bin/synthetic-vehicle-darwin-arm64`
(SHA-256 `4b7c0170888d9f45fcd145bf3d34eecf7363898036daca6332b67354fc47ea59`,
13,673,794 bytes, mode 0500, uid 501, one hard link, macOS 13.0 minimum).
The host source/test/build and artifact checks passed and the lock was absent
afterward. This remains host source/build evidence only: no PKI, guest,
runtime, producer invocation or event was authorized. The next gate is Edge's
independent binding review, followed by one complete fresh inactive
actor/guard/profile/runtime proposal and a separate coordinator submit-once
decision.

Edge independently accepted Hub's complete inactive r6 proposal as a
proposal-only document:
`hub/docs/development/edge-macos-one-event-inactive-runtime-proposal-2026-09-12-r6.json`
(SHA-256 `873e046f50b498045c83ed5dcc8e03012c252ae052a6bc3f055b29f8c09fe56a`).
It binds the accepted r5 source-to-binary receipt and helper, reserves the
fresh r6 roots and ports, leaves Hub/source/vehicle/transaction identities
unassigned, and defines the source/staging, distinct vehicle/receiver PKI,
five-second empty-baseline, one-shot, post-event durability/projection and
ordered-cleanup gates. The proposal records no PKI, credential, guest,
listener, process, producer or event action. The next gate is a separate
coordinator decision for fresh staging/profile verification and a zero-event
runtime; submit-once authorization remains a later decision after both-owner
empty-baseline acceptance.

Ready: Rust runtime/spool/ACK contracts, bridge lock/scripts, native units,
Docker source shape, active ARM64 package/service shape, the source-only Hub
consumer-input preparer with explicit server addressing and concrete
Edge-issued bearer validation, the tested receiver-key ownership correction, the
pinned-verifier compatible synthetic identity generator, the successful r13
VehicleName-only receiver/Edge/Hub/HA receipt with ordered cleanup, and the
Hub-owned immutable Edge D1 source binding. The accepted r13 source regression
and bounded mTLS path satisfy the generic collector transport gate, but r13 is
a distinct synthetic cohort and does not authorize or replace the prepared
R1 producer handoff. The c2ace loader and receiver-only readiness gates are
complete, but they do not prove Edge forwarding. Edge has now drafted, corrected
and sent the complete source-bound forwarding contract. Hub then accepted it,
prepared the fresh empty baseline, and cleaned that runtime. The coordinator
rejected and closed the cohort before producer invocation after the public leaf
hash reconciliation; the redacted receipt above preserves the hash mismatch,
the corrected parser interpretation, and the no-retry boundary. The fresh r2
certificate/profile gate and zero-event baseline closure are complete; the
final Hub handoff is bound above, cleanup is complete, and no producer event
was authorized or sent. Neither closed cohort may be restarted or reused, and a
separate coordinator decision remains required before any future producer
event.
Edge's new
source-bound forwarding contract is
`docs/development/edge-macos-forwarding-contract-proposal-2026-09-12-r1.json`
(SHA-256 `9bf4c2f85911f94a35a19353b024f6a59f6eb39635f8a89b8159aa2b64ee4bd8`);
the source-to-binary receipt is
`docs/development/edge-macos-arm64-source-binary-receipt-2026-09-12-r1.json`
(SHA-256 `2344d42e1d43199eca57263aae50e7ffa7f68abdd85c7d7fb37d234f26202e5f`).
The contract records the exact staged binary path, source/artifact/config/state/
spool/key shapes and leaves runtime-generated values pending Hub. The current receiver-only readiness
proposal is
`docs/development/edge-macos13-receiver-readiness-proposal-2026-09-12-c2ace-r1.json`
(SHA-256 `d629e3dfc55bbe750ec416a56889175fd7efdca782fcc541eeec3daaf8348aa6`);
it binds receiver `127.0.0.1:21444`, receiver status
`127.0.0.1:21445/status`, and the pinned receiver JSON field names without
creating or reading credentials; its root and redacted readiness receipts are
retained after the bounded run. The receiver-only proposal has no Edge process
or upstream requirement, but the pinned receiver still requires a separate
startup bearer environment binding, which Hub supplied only for that bounded
readiness run. The prior
explicit-address and concrete-bearer Hub consumer-input repairs are
source-tested, and the prior disposable Mac handoff consumed its bearer and
completed the historical fresh empty
receiver/Hub baseline with four consecutive health/ready 200 responses and
zero pending/ACK state, and performed ordered disposable cleanup; its direct
SQLite count limitation is recorded. The amended
`docs/development/edge-macos-forwarding-handoff-proposal-2026-09-09.json`
is a prior preparation receipt that bound the coordinator-authorized
`macos13-arm64` preparation at
`192.168.64.10:21443` and the fresh Hub-owned 0700 root
`/Users/admin/teslatlas-edge-macos-forwarding-fresh-20260909`; Hub staged that
fresh root, PKI/config, init/doctor and completed a bounded empty receiver/Hub
baseline under its separately recorded authority. That prior handoff
remains producer-launch-free until the coordinator gives explicit submit-once
authorization for the fresh synthetic VehicleName-only event and Hub records
the exact darwin-arm64 producer/helper and receiver-identity bindings. The
prior baseline recorded stable health and a trusted Hub empty control poll. The
frozen proposal intentionally omits
the private identity path and digest; the separately versioned Hub runtime
handoff binds that private identity after the proposal digest is frozen. The
handoff also binds the helper digest, transaction/name/timestamp, issuer CA
versus leaf, SAN/key correspondence, process UID/readability and bounded stop
receipts. The receiver JSON follows the pinned template's exact `tls` keys
(`server_cert`, `server_key`, `ca_file`); the bearer environment and fixed
loopback dispatcher are separate process bindings, not JSON fields. Exactly
one VehicleName projection is expected;
unexplained or numeric projections fail, and the historical r13 three-sequence
caveat is not an acceptance expectation. The D1 binding is source-only and
does not promote any package, image or isolated runtime subset to installed
aggregate acceptance. x86/Intel/Azure/full-matrix rows are paused and cannot
block ARM64/Mac work. No r13 root, credential, actor, record or HA entry may
be reused.

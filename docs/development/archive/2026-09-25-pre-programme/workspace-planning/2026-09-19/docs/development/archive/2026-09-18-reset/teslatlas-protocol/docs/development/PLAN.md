# Teslatlas Protocol Development Plan

> Active model: **gpt-5.6-luna / max**. The owner has started development; old G0/hold paragraphs below are historical.

> **Development authorized 2026-09-08; execution scope amended 2026-09-09; owner restart 2026-09-18.** Execute ready owned work under [ACTIVE_SCOPE.md](../../../docs/development/ACTIVE_SCOPE.md), which takes precedence over older stage-order and platform-matrix wording. Native goal `01a082b7-86b3-72d2-b173-dbad9073ec2c` completed its ARM64/Mac Protocol resource objective on 2026-09-09 and is preserved; the restart reopens only concrete ARM64/Mac follow-on corrections and handoffs. Viewer development, packaging, integration and acceptance are excluded from the current Protocol scope.

**Goal:** Deliver usable, source-neutral current-Hub contracts, developer assets and conformance support for Debian 13 ARM64 and Apple-silicon Mac consumers. Keep formal parity gaps distinct; x86, amd64, Intel Mac, Azure and full cross-architecture acceptance are deferred and are not prerequisites for the current usable resources. Viewer development, packaging, integration and acceptance are excluded from the current goal; retained Viewer records are historical only. Add no Protocol daemon.

**Native goal completion (2026-09-09).** The Protocol-owned ARM64/Mac resource
goal is complete: the current-Hub, rich-profile and Edge contracts, public
documentation, conformance runner/cases, bounded matrix boundary and immutable
D1 developer-resource handoff are present and locally verified. The focused
generators are current and `./tools/check` passes 134 tests plus 31/31 rich
profile runs across three profiles. This completion does not promote the
candidate current-Hub profile, install or activate companions, or claim named
TeslaMate parity, active-target runtime receipts, deferred architecture rows or
X1 lifecycle acceptance.

**Owner restart (2026-09-12).** The global pause is lifted for the active
ARM64/Mac scope. Protocol has no in-flight runtime or owned lock to reconcile;
its completed resource goal remains complete, and no replacement goal or
invented work is authorized. Viewer development, packaging, integration and
acceptance remain excluded; retained Viewer records are historical only.
Continue only when a concrete Hub/companion handoff or current-Hub contract
correction exists. x86/amd64/Intel Mac, Azure and full cross-architecture work
remain paused or deferred.

**Owner restart revalidation (2026-09-18).** The owner resumed the six
in-scope products. The completed ARM64/Mac Protocol developer-resource
objective remains complete. A narrow post-pause audit found no new Hub or
companion contract correction: the checked sibling status/plan metadata remains
at the 2026-09-12 checkpoint, and no post-pause commits were present. The
currently pending sibling items are runtime or handoff dependencies rather
than Protocol-owned wire-contract changes. No Protocol action is ready; remain
idle until a concrete correction or fresh handoff arrives. Viewer remains
excluded, and x86/amd64/Intel Mac, Azure and full cross-architecture work
remain paused or deferred.

**Architecture:** Protocol owns schemas, OpenAPI, profiles, redacted fixtures, conformance cases, and bounded adapters. It does not own Hub Rust implementation, service lifecycle, bootstrap, packages, credentials, or generated SDK implementations. hub-http-v1@1.0.0 remains separate from rich profiles 1.0.0–1.2.0 and Edge delivery 2.0.0.

**Tech Stack:** Python >=3.11; uv; JSON Schema 2020-12; OpenAPI; jsonschema[format-nongpl]>=4.26,<5; JSONL adapters; optional pinned Python 3.13.13/uv 0.12.9 checker image.

**Spec:** [PRODUCT_SPEC](../../../docs/development/PRODUCT_SPEC.md), [MASTER_PLAN](../../../docs/development/MASTER_PLAN.md), and [AGENTS](../../AGENTS.md).

## Status and non-goals

HEAD is 05225bd5b2f56885025180d68fedf3a42baaa90b with 35 preserved dirty entries. The current profile has eight routes and 24 profile cases; the broader installed matrix is deferred beyond the active working-product aim. Recorded local evidence is 134 Protocol unit tests plus 31 rich-profile runs and 133 offline checker tests on Linux/arm64. A fresh Hub-owned Debian 13 ARM64 real-process fixture passed 24/24 current-profile cases for product 2026.36.2 with profile hash b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926, the Swift public macOS consumer passed its native B2 journey against a separate fresh fixture with the same profile identity, the TypeScript helpers are source-ready and the packed Node consumer passed a fresh selected 51-drive journey with 25/25/1 paging, conditional-304/cursor continuation, claim, rotation and default Fetch, the selected Debian ARM64 Home Assistant Container supplied bounded outage/recovery, config-flow negative/reauth, same-ID reconfigure/clean-restart, diagnostics-redaction and HA/Edge coexistence observations against separate current-Hub fixtures, and the sealed Edge B2.1/B2.2 ARM64 receiver/spool/Hub path passed duplicate, outage/restart, recovery and receiver-first restart checks in isolated disposable lanes. The Edge macOS 13 ARM64 package subset also passed clean install/init, independent core/receiver LaunchAgent lifecycle, real receiver-wire ACK, restart continuity and data-preserving uninstall. The installed Viewer Recovery7 journey passed current projection, Hub outage/restore, stale/offline presentation, revoke/401 session clearing, fresh re-pair and clean process cleanup. The first same-Hub Docker attempt reached receiver ACK but retained one encrypted record because the r5 collector reported transport unavailable; direct mTLS succeeded and Hub cleanup completed, so it remains a bounded integration failure requiring a fresh corrected handoff. A separate fresh r9 cohort reached trusted Hub/HA readiness; its Edge helper failed before receiver admission on client-certificate presentation, while the Hub collector emitted only safe connect categories before any ledger row. The r9 cohort was cleaned without a queue record or ledger value, and Hub identified the helper identity as stale, so it is not a valid producer transport result. These prove local contract/package behavior and bounded ARM64/Mac fixture, consumer, recovery and runtime behavior only.

The profile and compatibility/hub.json remain candidate pending formal admission; the native fixture and bounded HA recovery result do not admit an installed profile. Active product rows are Debian 13 ARM64 and macOS on Apple silicon. Debian amd64, x86, Intel Mac, Azure and full cross-architecture closure are deferred and cannot block the active aim. Edge's identity-bound receiver actor correction now has source/build, loopback mutual-TLS and exact pinned-verifier evidence; the fresh r10 Docker runtime handoff was healthy with an empty encrypted queue, but its single authorized non-submitting Hub GET failed before HTTP because the Hub server-CA slot contained the Edge leaf instead of the issuing CA trust anchor. Edge then published a fresh test-backed r11 issuing-CA bundle and sealed empty-runtime handoff on 20643/20644; the one authorized non-submitting poll returned an empty batch with zero ledger mutation and the disposable pair was cleaned receiver-first. Coordinator task `01a0829e-751f-7d93-9c1f-4b1ffdbfc22e` authorized the prior bounded r12 synthetic Edge-to-Hub-to-HA journey. Its first staging attempt failed before runtime, and the repaired receiver then failed before empty readiness because the mounted `vehicle-tls.key` was unreadable. Edge supplied source/control and tested custody evidence, but explicitly marks the 20743/20744 root diagnostic-only because it was repaired in place; the one empty pull on disposable 18491 was cleaned and is likewise non-admissible. Hub and HA accepted the exact name-only `VehicleName=synthetic-b2` contract and primary-lane boundary. Edge staged the distinct 20843/20844 replacement with fresh empty readiness; the one authorized producer call then failed at the old actor's receiver application identity boundary, and the replacement was cleaned without retry. After the pinned-verifier review passed, the same coordinator task separately authorized one wholly new fresh sealed one-record journey; its runtime handoff, bounded one-record result and cleanup are recorded below. The active first working slice is one real Debian 13 ARM64 Hub plus its non-Viewer companion paths and an Apple-silicon Mac consumer path. Viewer records remain historical and are excluded from current Protocol work. The former B1/B2/R1/D1/X1 sequence remains useful as evidence labels, but its x86/full-matrix prerequisite ordering is superseded by ACTIVE_SCOPE. G0–G6 are evidence labels only.

The forward-looking r12 staging sentence above is now historical. The coordinator-authorized sequence reached its fresh 18493 pre-record gate, consumed one producer attempt that failed at receiver application identity authorization after TLS, and completed Edge/Hub/HA cleanup; no retry is authorized.

**Current r12 status (2026-09-09).** The distinct Edge replacement passed fresh ARM64 custody/empty-readiness; Hub/HA reached the required pre-record ordering on fresh fixture 18493, and the single producer invocation was consumed. The receiver rejected the actor after TLS at application identity authorization because its custom r12 issuer had no public X509 extensions/EKU/SAN, so no Edge admission, receiver ACK, Hub ledger/frontier mutation or post-record HA value exists. Edge stopped the pair receiver-first without retry and retained the sealed empty state. Hub stopped 18493 and HA removed the temporary entry, restoring the primary 18483 lane; r12 is closed as bounded non-acceptance. Edge subsequently supplied a source-only generator and exact pinned-verifier regression, and Hub independently reviewed it: the corrected issuer/VIN/client-auth profile is accepted, while wrong-issuer and missing-identity-OID profiles are rejected. The old r12 producer call remains consumed and cannot be retried. The separately authorized fresh r13 journey is recorded below as a bounded B2 result. Full R1/P5, D1 and X1 remain open.

**Current r13 status (2026-09-09).** The separately authorized fresh r13 journey passed its bounded same-Hub B2 path. Edge was empty-ready on fresh 20943/20944 state; Hub bound fixture 18494 with zero pre-record durable Edge state; and HA paired one temporary entry with a stable `synthetic-b2` name-only baseline. Exactly one actor invocation was admitted and ACKed for topic `V` and transaction `edge-r13-authorized-20260909-v-001`. Edge drained to zero queued records with no gaps or restarts. Hub advanced the ACK frontier to 3 with no pending publication: sequence 2 was the sole projected `VehicleName` item and sequences 1 and 3 were durable connection/state lifecycle artifacts, while the accumulator contained no numeric telemetry. HA observed the name-only projection through a normal scheduler interval. Hub, HA and Edge then completed ordered disposable cleanup, retained the encrypted Edge state, and restored the primary 18483/HA lanes. This is bounded synthetic B2 evidence; named passive TeslaMate R1/P5 parity and recovery, active ARM64/Mac D1 installed SessionInputs/receipts and formal profile admission remain separate open items. x86/amd64/Intel Mac lifecycle and full cross-architecture closure are deferred, not blockers.

**Current R1 source preparation (2026-09-09).** Hub recorded a v4.2.0-compatible TeslaMate source mapping with a 17-domain fail-closed denominator and passed source-only recovery, observation-command and watermark regressions. HA added and passed an overlapping-snapshot serialization regression alongside its existing source checks. Hub then completed one fresh isolated synthetic data-only backup, immutable verification, restore, post-watermark observation check and truncated-copy rejection; the redacted receipt is `hub/docs/development/r1-synthetic-data-recovery-2026-09-09.json`. These are bounded synthetic/consumer results only: no named TeslaMate database, passive collection, migration, live-source backup or soak receipt has been admitted. Previously recorded Viewer data-state results remain in verification evidence as historical only and are excluded from this current Protocol plan.

**Current H7/D1 handoff (2026-09-09).** Hub prepared `hub/docs/development/r1-named-source-read-only-evidence-plan-2026-09-09.md`, which separates source admission, source-only inventory, offline import/comparison, passive collection, recovery and soak authority. It is not an admission or operational receipt and still needs an owner-supplied preserved source/backup, read-only PostgreSQL identity, version/selected-car confirmation and isolated comparison destination; this R1 gap must not stall independent ARM64/Mac usability work. The D1 aggregate source boundary is now present and independently verified in Hub `packaging/components.json` (SHA256 `37b6fc64fd804813d82053c7cf8d12e89cb9ebfc69ce569c4d1c84d645bed0aa`): HA supplies the frozen 31-file component-root payload and its bounded Debian 13 ARM64 Container selection receipt (SHA256 `2f7b2b933f1f970fad49786530286fe3dadd463d3b063c4a6ffa50327e4f2be2), and Protocol supplies `docs/development/d1-component-manifest-handoff-2026-09-09.json` with an external read-only 177-file source snapshot (handoff SHA256 `54907340ce8dd04ea93403d8ebf317783faa190a43c68745e3d470962df924bd`, source SHA256 `efda1c9d31556871fe5be9965889083a7037b0c311945280631ce9dd21ce994f`, snapshot manifest SHA256 `e89ff7ecf4b9e0ddeebc6801ca4bf7a865b8c0561e12bb69e1c5d14f55b7ec24`). Hub independently rechecked the Protocol sealed record twice, bound its current-Hub/profile/compatibility/matrix/offline-checker identities, and the aggregate verifier passes 9/9; the aggregate also records source/candidate identities for TypeScript, Swift and Edge while Hub core and Fleet helpers remain blocked. Previously recorded Viewer snapshot/package identities are retained as historical evidence only and are excluded from current Protocol D1 work. The earlier manifest-gap audit is retained as historical evidence; this current record is source/package identity preparation only, not installed composition, a native package, an image, a service, or final platform acceptance. Active ARM64/Mac component slots and runtime selectors remain pending their immutable candidate and runtime receipts; x86/amd64/Intel Mac, Azure and full cross-architecture slots are deferred and cannot block this scope.

**Current D1 source guard and disposable installer transaction (2026-09-09).** Hub's source-only `d1-plan` and explicit `d1-install` paths admit only the Home Assistant ARM64 Container selector from `packaging/components.json`, emit `activation_authorized=false` with a required runtime receipt, reject unbound Hub core and Fleet helper selections before manifest access, and carry the HA payload/selection identities through install, status and repair provenance checks. The disposable local-candidate transaction exercised the existing HA staging/link path, preserved its existing sentinel, and reported `runtime_acceptance=false` without starting Home Assistant. Fresh Hub source gates passed 80/80 companion tests, 9/9 aggregate-manifest checks, one bootstrap-catalog check and 11 Rust companion-delegation tests including five D1-specific cases; `cargo fmt --check` and the Hub binary build also passed. This remains a source-only fail-closed guard and staging rehearsal for the active ARM64 lane: it does not provide installed composition, native artifacts, a service start, runtime receipts, H7 named-source parity, active Mac lifecycle acceptance or formal profile admission. Deferred x86/amd64/Intel Mac and Azure/full-matrix rows are not prerequisites.

Protocol does not implement Hub internals, collection, storage, service managers, bootstrap, .deb/pkg/Docker Hub runtime, installer choices, vehicle commands, credential creation, or GitHub/registry publication. Reference fixtures and compiler results never establish installed acceptance.

## Prepared development VMs

The single source of detailed access instructions is [VM_ACCESS](../../../docs/development/VM_ACCESS.md); [ENVIRONMENT](../../../docs/development/ENVIRONMENT.md) records shared environment context. From the workspace root, use the shared wrapper: `scripts/dev/vm.sh debian ssh [COMMAND]` for the primary Debian guest or `scripts/dev/vm.sh mac ssh [COMMAND]` for the later macOS guest, for example `scripts/dev/vm.sh debian ssh uname -m` and `scripts/dev/vm.sh mac ssh sw_vers -productVersion`. The private SSH configuration is `~/dev/teslatlas-lab/access/ssh_config`; `credentials.json` may contain the Mac GUI password and must never be copied into this repository or any receipt.

The active B1/B2 path uses `debian13-arm64` (Debian 13.6, aarch64, 4 vCPU, 4 GiB RAM, 32 GiB disk), SSH alias `teslatlas-debian13-arm64`, user `bolyki`, forwarded endpoint `127.0.0.1:60022`, and `macos13-arm64` (macOS 13.7.4, arm64, 4 vCPU, 4 GiB RAM, 50 GB disk), SSH alias `teslatlas-macos13-arm64`, user `admin`, NAT with dynamic address currently observed as `192.168.64.10`. The Mac guest is an active working-product target and later ARM64/Mac lifecycle target. These are environment details, not product acceptance.

## Floors and source authority

| Requirement | Source | Floor |
| --- | --- | --- |
| Python | pyproject.toml | >=3.11 |
| Dev dependencies | pyproject.toml, uv.lock | jsonschema[format-nongpl]>=4.26,<5; OpenAPI validator >=0.7,<1 |
| Checker | Dockerfile | Python 3.13.13/uv 0.12.9, digest-pinned, non-root |
| Rich profiles | compatibility/manifest.json and compatibility/{1.0.0,1.1.0,1.2.0} | Current plus previous two minors |
| Current profile | profiles/hub-http-v1/1.0.0/** | hub-http-v1@1.0.0 |
| Gates | tools/check, tools/check-current-hub, conformance/run | Read-only |

Machine-readable artifacts and SHA256SUMS are wire authority. Keep product, wire, schema and storage versions separate.

## Shared gates

The evidence labels G0–G6 classify the planning hold, contract correction, real-runtime integration, developer composition, installed lifecycle, passive parity/recovery and clean-host closure. They are not a mandatory platform sequence: the active order is working Debian ARM64, working Apple-silicon Mac, applicable companions, then relevant reliability and delivery. R1 named-source parity remains conditional on its owner inputs; D1/X1 active ARM64/Mac receipts follow usable product paths. x86/amd64/Intel Mac, Azure and full cross-architecture closure are paused. G1 covers the minimal Hub plus Protocol contract corrections and fixture needed by the active working slice. Protocol never becomes a daemon, and publication remains separately authorized.

## Milestones

### P1 — planning and environment hold

**depends_on:** Owner start; no Hub runtime handoff.

**Files/components:** docs/development/PLAN.md; pyproject.toml; uv.lock; Dockerfile; tools/check; tools/check-current-hub.

Record Python/uv and lock identity, private mode-700 evidence storage, and preserved dirty files. The planning hold has ended; run the meaningful checks for the current owned development step.

**Verify:** python3 -c 'import sys; assert sys.version_info >= (3,11)' and uv lock --check. Expected: supported interpreter and unchanged lock. An absent tool or library is recorded as a product-owned provisioning task; it does not create an immediate permission blocker or lower the floor.

### P2 — B1 minimal contract corrections and fixture

**depends_on:** P1; Hub supplies current source facts, product identity, route behavior and an isolated Debian 13 ARM64 fixture.

**Files/components:** profiles/hub-http-v1/1.0.0/{profile.json,openapi.json,*.schema.json,cases.json,field-semantics.json,SHA256SUMS}; profile examples; tests/test_hub_http_profile.py; conformance/hub_http.py; conformance/hub_matrix.py; Hub-owned hub/tools/interop/fixture.py.

Compare raw discovery, health/readiness, vehicles, current, drives, claim and rotation. Freeze pairingId/expiresAtMs/tlsPin spellings, signed-64-bit values, cursor/time-window binding, ETag/304, content types, TLS and unsupported operations. Require deterministic two-vehicle/five-drive data over the real transport; Hub owns process and cleanup. Update schemas, OpenAPI, examples, cases, generated files and checksums together.

**Verify:** uv run python tools/build_hub_http_profile.py --check; uv run python -m unittest tests.test_hub_http_profile tests.test_current_hub_probe; ./conformance/run --profile hub-http-v1@1.0.0 --adapter ./conformance/adapters/actual-hub --config <Hub-private-descriptor> --json. Expected: generated bytes/checksums agree and raw fixture responses validate. Missing fixture or review is blocked evidence.

**Done/failure:** One B1-ready profile revision and fixture identity are documented. Formal profile admission and broader matrix receipt acceptance remain separate later obligations and do not block the active usable resources. Raw mismatch restarts P2; SDK changes cannot bypass it.

### P3 — B1 first working Debian ARM64 slice

**depends_on:** P2; Hub supplies selectable Debian ARM64 core/bootstrap and a private descriptor; TypeScript and applicable consumers consume the exact profile.

**Files/components:** conformance/adapters/actual-hub; conformance/run; conformance/hub_matrix.py; docs/current-hub.md; docs/conformance.md; docs/verification.md; Hub-owned bootstrap/fixture files.

Run only the ordinary Debian ARM64 path: discovery, readiness, claim, vehicles, current, one bounded drives page and documented errors/ETag. Keep raw-wire validation and read-only behavior. Do not require all 21 cases, x86/amd64, Intel Mac, Azure or full browser trust here; Apple-silicon Mac is a separate active working-product path.

**Verify:** ./conformance/run --profile hub-http-v1@1.0.0 --adapter "$PWD/conformance/adapters/actual-hub" --config "$TESLATLAS_HUB_SESSION_INPUT" --json. Expected: B1 cases pass against a running Hub and exact profile/source/runtime identity is retained; missing input fails closed.

**Done/failure:** Hub, Protocol and applicable consumers demonstrate one ordinary flow. Bad TLS, invitation, profile hash or cleanup restarts P3.

### P4 — B2 applicable companion helper support

**depends_on:** P3; TypeScript and Swift owners provide their public consumer entrypoints; Hub keeps the same fixture and profile identity.

**Files/components:** conformance/adapters/actual-hub; tools/matrix-contract.json; tools/matrix_contract.py; docs/verification.md; consumer-owned protocol locks/bindings as read-only inputs.

Run the TypeScript Node consumer and supported Linux ARM64 helper against the real Hub. Swift may use its native macOS consumer on the current Apple-silicon host for applicable transport and working-product proof. Keep browser/Swift credentials private; x86/amd64 and Intel Mac helpers are deferred.

**Verify:** uv run python tools/build_hub_http_profile.py --check; ./tools/check; each consumer’s own real-runtime check. Expected: all consumers use the same profile hash and no helper invents a route. Companion failures remain companion-owned unless wire evidence changes.

**Done/failure:** Each active-target helper has one primary-slice result. No 21-cell or cross-architecture claim is made; a changed helper or profile restarts P4 or P2.

### P5 — R1 passive data parity and recovery

**depends_on:** P3/P4 for applicable contract context; Hub supplies named TeslaMate passive observations when the owner provides the required source inputs. P5 is a separate acceptance track and is not a prerequisite for active ARM64/Mac usability.

**Files/components:** docs/data-model.md; docs/commands-and-metadata.md; docs/verification.md; profiles/hub-http-v1/1.0.0/field-semantics.json; compatibility/hub.json.

Reconcile units, null/zero semantics, drive ordering/cursors, gaps, duplicate/reconnect behavior and retention. Record supported, absent and inapplicable fields without adding commands or wake behavior.

**Verify:** uv run python tools/build_hub_http_profile.py --check; ./conformance/run --profile hub-http-v1@1.0.0 --adapter ./conformance/adapters/actual-hub --config <accepted-B1-descriptor> --json. Expected: field semantics match raw responses and recovery evidence is linked with stated limits.

**Done/failure:** R1 accepted by Hub and consumers when its owner-controlled source inputs exist. Missing R1 inputs do not block independent ARM64/Mac working-product work. Changed units or reference restarts P5 and dependent bindings.

### P6 — D1 selectable developer contract

**depends_on:** P2; P3 working slice; P4 helper results; Hub owns installer selection and runtime composition.

**Files/components:** VERSION; compatibility/hub.json; README.md; docs/compatibility.md; tools/build_hub_http_profile.py; generated/checksum maps; Hub selection records.

Export the exact Protocol bundle as a developer resource alongside the first stack. Keep product 2026.36.2, wire hub-http-v1@1.0.0, rich profiles and Edge identities separate. Keep candidate status until installed receipts; add no daemon/package service.

**Verify:** uv run python tools/build_hub_http_profile.py --check; uv run python tools/build_openapi.py --check; uv run python tools/build_conformance.py --check; git diff --check. Expected: generated artifacts/checksums agree and no service-manager requirement exists.

**Done/failure:** Hub can present the developer resource with exact identity. Profile change invalidates and restarts affected work.

### P7 — ARM64/Mac working-product lifecycle and clean closure

**depends_on:** P3/P4/P6; Hub supplies fixed runner admission and target SessionInputs for the active Debian ARM64 and Apple-silicon Mac lanes after the working slice is usable. P5 is linked where source evidence exists but does not gate this milestone.

**Files/components:** conformance/adapters/actual-hub; tools/matrix-contract.json; tools/matrix_contract.py; docs/verification.md; compatibility/hub.json; Hub receipts.

Run the applicable ordinary Hub and companion routes on macOS arm64 and Debian 13 ARM64 against exact active-target releases. Capture setup, source/profile/runtime, TLS, raw/normalized evidence, pairing, current/history display, restart/outage/revocation/re-pair, process lifecycle, data preservation, cleanup and close acknowledgement. Then run the active-target clean-host gates and prepare source-only delivery. Do not wait for x86, amd64, Intel Mac, Azure or the complete cross-architecture matrix.

**Verify:** Hub runner reports the applicable ARM64/Mac Protocol cells; ./tools/check; ./conformance/run --profile hub-http-v1@1.0.0 --adapter ./conformance/adapters/actual-hub --config <accepted-private-descriptor> --json; git diff --check. Expected: active-target receipts, accepted metadata and no unexplained skips or credentials. Missing owner inputs fail closed.

**Done/failure:** Active-target G4/G5/G6 rows complete. Changed Hub binary/profile/package/reference or interrupted upgrade restarts only the affected boundary; partial rows never become a whole-product claim. Deferred platforms do not block this milestone.

### P8 — Deferred x86/Azure/full cross-architecture closure

**status:** PAUSED by [ACTIVE_SCOPE.md](../../../docs/development/ACTIVE_SCOPE.md); resume only on new owner direction.

**depends_on:** A later owner scope change after the ARM64/Mac working-product milestone; fixed target inputs, artifacts and runner admission supplied at that time.

Pause x86, x86_64, amd64, Intel Mac, Azure and full cross-architecture matrix development, compilation, image acquisition, VM provisioning, runtime/lifecycle tests, cleanup and acceptance. Preserve existing code, schemas, historical receipts and unadmitted target state. No row in this milestone may block P3, P4, P6 or P7 active ARM64/Mac usability.

**Verify later:** Only after explicit owner resumption, use fresh target-specific SessionInputs and receipts. Do not reuse the preserved Debian x86_64 VM or old diagnostic inputs as active evidence.

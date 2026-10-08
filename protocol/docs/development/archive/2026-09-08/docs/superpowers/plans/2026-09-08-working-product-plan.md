# Teslatlas Protocol Working Product Development Plan

**Latest user model policy (2026-09-08):** This coordinator and delegation use `gpt-6-astra` with `thinking=high`. This product task, coding, goal execution and all development workers use `gpt-5.6-terra` with `thinking=high`. This supersedes every earlier model instruction in this plan and its historical goal snapshot. Preserve checkpoints at model transitions and verify the actual new turn model.

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to execute the full remaining goal across the milestones below. Execution is already authorized. A phase boundary is neither whole-goal completion nor a blocker while Protocol-owned `READY` work remains.

**Goal:** Complete the independently usable current-Hub Protocol product: finish every authorized Protocol-owned source, Docker/probe and documentation change; execute two fresh actual-network sessions and the three required installed Protocol cells from Hub-owned inputs; bind truthful compatibility evidence; then review and publish the verified source-only `main` without CI, releases, tags, binaries or artifact uploads.

**Architecture:** Keep `hub-http-v1@1.0.0` as the canonical current-Hub profile and retain the existing deterministic generator, raw-response validator, native adapter and v2 installed contract. Protocol owns its public contract, adapter, probe, docs and compatibility record; Hub owns runtime sessions, installed-host control, registry admission and the shared ledger. Rich profiles and Edge delivery remain separate contracts.

**Tech Stack:** JSON Schema 2020-12, OpenAPI 3.1, Python >=3.11, locked uv development dependencies, unittest, shell entrypoints, curl and a small Docker checker image.

**Spec:** [Workspace authority](../../../../WORKSPACE_AUTHORITY.md), [ecosystem specification](../../../../docs/superpowers/specs/2026-09-05-hub-ecosystem-compatibility.md), [ecosystem plan](../../../../docs/superpowers/plans/2026-09-05-hub-ecosystem-compatibility.md), and the Hub-owned [execution state](../../../../hub/docs/compatibility/execution-state.json). These workspace-relative inputs do not become public-product dependencies.

## Current continuation control

Coordination uses `gpt-6-astra` with `thinking=high`; all Protocol product development, goal execution, review and publication work uses `gpt-5.6-terra` with `thinking=high`. The native goal remains blocked. With the available tools, resumption requires the user's supported Resume goal control. The parent can dispatch ordinary authorized development turns now; this does not activate the native goal. Do not replace an unfinished goal, mark it falsely complete, or edit internal app state. Continue READY owned work without waiting for native resumption; retain the full proposed objective and acceptance gates.

## Current checkpoint — 2026-09-08 18:24 UTC

Execution remains authorized. Protocol `main` is at `05225bd5b2f56885025180d68fedf3a42baaa90b` with preserved task work. The canonical current-Hub profile is `hub-http-v1@1.0.0`, manifest SHA-256 `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`; the older `b3914d35d28374f6423af789e9ed6a4a4c82196a068c041946e24d609db0b05b` appears only in the explicitly historical pre-execution snapshot below.

Protocol-owned source is locally verified: the v2 manifest, validator and raw schema hashes are `23cacc78e7289009eb4b9b78ab5b1569aa5a905eb604f23955ec44b803590933`, `e934eb2a03ae364dde2decf5456a7f19a18163064459bf5667e8f1642acf6fa7`, and `3a3dfd8408b5ae243e908c83908228bd96a87355886f55c569048dfbc5839e08`. The profile generator check passes; the settled full local gate recorded 128 unittest tests and 31 rich-profile case runs, and the later focused current-Hub/matrix/v2 run passed 51 tests. M2's new probe and current-Hub profile suite passed 29 tests, `py_compile`, `uv lock --check`, Dockerfile lint and local Markdown-link checks. Do not rerun these unchanged gates merely to create activity.

Hub task `01a07f89-45cb-7ea2-b96e-aa89de18fd14` has selected the `b80d…` profile and records the TypeScript source/registry gates as passed while `installed_capability_claimed` remains false. The wider completion audit remains `0/21` accepted cells, `0/444` accepted slots and `0/10` accepted cohorts. Protocol still lacks fresh owned network receipts, all three installed Protocol receipts, Docker runtime evidence, an accepted compatibility transition and source publication.

### Model policy

- Coordination and delegation use `gpt-6-astra` with `high` reasoning.
- Every implementation, runtime, review and publication agent for this Protocol product uses `gpt-5.6-terra` with `high` reasoning.
- The goal spans the full remaining product. Do not replace it with a short task, stop after one milestone, or mark the whole goal complete when a single phase passes.

### Remaining end-to-end objective

Finish all Protocol-local implementation and review that can proceed without Hub runtime inputs. Then use two separate fresh Hub sessions for the public walkthrough and native acceptance, followed by Hub-generated v2 SessionInput for `protocol_actual_hub__macos_arm64`, `protocol_actual_hub__debian13_arm64`, and `protocol_actual_hub__debian13_amd64`. Accept only exact content bindings, all 21 required cases per installed cell, normal child exit, evidence-ready/close-completed acknowledgement, and retained cleanup/stopped evidence. Populate compatibility metadata only from accepted receipts, finish the public docs and Docker/read-only-smoke evidence, and publish reviewed Protocol source on `main` after Hub and sibling owners confirm the exact prepublication content.

### Milestone status

| Milestone | Status | Owned files and interfaces | Evidence / exit condition |
| --- | --- | --- | --- |
| M1 — Canonical contract and v2 source | `DONE` | `profiles/hub-http-v1/1.0.0/`, `tools/build_hub_http_profile.py`, `conformance/hub_http.py`, `conformance/hub_matrix.py`, `tools/matrix-contract.json`, `tools/matrix_contract.py`, `tools/protocol-http-v1.schema.json`, focused tests | Canonical `b80d…`; deterministic generator passes; v2 hashes above; settled local and focused gates pass. Actual network and installed evidence are explicitly outside this source milestone. |
| M2 — Protocol-local product finish | `LOCAL SOURCE DONE; RUNTIME EVIDENCE WAITING` | `AGENTS.md`; `Dockerfile`, `.dockerignore`; `tools/check-current-hub`; probe tests; README and `docs/{architecture,client-quickstart,current-hub,http,conformance,product-versioning,versioning,compatibility,docker,verification}.md` | Scoped AGENTS, bounded probe, static image definition and truthful docs are complete and locally checked. The Docker daemon socket is unavailable, so image build, offline-container, architecture and synthetic remote-smoke evidence remain waiting. |
| M3 — Fresh actual-network walkthrough and native acceptance | `WAITING FOR OWNER/RESOURCE` | Protocol consumes two Hub-private descriptors through `docs/current-hub.md` and `conformance/run`; Hub owns fixture launcher, invitations, TLS identity, controller and cleanup | One isolated walkthrough receipt and one separate native acceptance receipt against the exact `b80d…` profile, including 4 KiB/extractor observations, lifecycle, pagination, rotation and stopped-owned-resource evidence. |
| M4 — Three installed Protocol cells and compatibility transition | `WAITING FOR OWNER/RESOURCE` | Protocol adapter `protocol_actual_hub`; Hub fixed registry, staged v2 contract, host sessions, controller/receipt admission; Protocol `compatibility/hub.json` and `docs/verification.md` | Each macOS arm64, Debian 13 arm64 and Debian 13 amd64 cell passes all 21 cases with exact source/artifact/runtime bindings, normal exit, close acknowledgement and cleanup. Hub approves the candidate-record transition before Protocol fills tested fields. |
| M5 — Final review and source-only publication | `LOCAL REVIEW COMPLETE; PUBLICATION WAITING` | Protocol task-owned diff on existing `main`; Hub/SDK owner content confirmation; Git remote `https://github.com/magrathean-uk/teslatlas-protocol.git` | The local review found and corrected equal drive-bound acceptance and an implicit curl-retry setting. M2–M4 evidence, owner confirmation and runtime receipts remain required before explicit staging, source/docs commit, push and remote verification. No CI, release, tag, binary, image or artifact publication. |

M2 source and documentation are complete. A missing Docker daemon blocks only Docker runtime evidence, not M5 local diff review preparation. M3/M4 remain waiting on their owned Hub inputs.

### Dependency table

| Owner / task ID | Exact missing input | Work still possible locally | Event that unblocks dependent work |
| --- | --- | --- | --- |
| Hub — `01a07f89-45cb-7ea2-b96e-aa89de18fd14` | Two distinct owner-only synthetic sessions: exact Hub executable/source and seed hashes, profile binding `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`, separate fresh normal/disposable/expired invitations, endpoint plus CA and connected-leaf DER identity, scenario bytes/hash, private output directories, live launcher/controller and cleanup contract | Finish M2; validate descriptor parsing and fail-closed behavior with existing local fixtures; do not fabricate a live receipt | Hub delivers both fresh private descriptors and confirms each launcher/controller is live and exclusively owned for the named run |
| Hub — `01a07f89-45cb-7ea2-b96e-aa89de18fd14` | Reviewed fixed registry entry for `protocol_actual_hub`; staged manifest `23cacc78e7289009eb4b9b78ab5b1569aa5a905eb604f23955ec44b803590933`, validator `e934eb2a03ae364dde2decf5456a7f19a18163064459bf5667e8f1642acf6fa7` and raw schema `3a3dfd8408b5ae243e908c83908228bd96a87355886f55c569048dfbc5839e08`; three fresh v2 SessionInput files with host-session registration, exact runtime inventory, broker endpoint, profile members, Hub/seed artifacts, actor input manifest, output reservations and cleanup deadline | Keep Protocol v2 source immutable except for a demonstrated integration defect; finish public docs and review the compatibility transition format | Hub admits the source binding and supplies one live descriptor for each of the three named installed cells; quarantined ARM is explicitly cleared or replaced by Hub before use |
| Local Docker resource — no task ID | Reachable Docker daemon plus supported arm64/amd64 build/runtime targets and a synthetic Hub endpoint whose certificate covers the container-visible hostname | Dockerfile, ignore rules, probe, unit/failure tests and static docs are complete; validate without claiming container runtime success | `docker info` succeeds and the selected synthetic endpoint is ready for the bounded read-only smoke |
| Parent orchestrator — `01a07048-a05c-72a2-9c7a-87dd59f52e9b` | Final Hub/TypeScript/Swift owner confirmation of the exact Protocol content identity and authorization sequencing for the shared compatibility transition | Freeze and review the Protocol-owned content, prepare exact staging list and publication report | Parent reports all required owner bindings/evidence accepted and hands publication back to the Terra high Protocol agent |

## Global constraints

- Own only this product repository. All product-relative paths below are under `teslatlas-protocol/`; sibling paths are read-only references or dependencies assigned to their owners.
- Keep the existing independent `main` checkout. Preserve unrelated edits; no branch, worktree, reset, clean, or stash. Reinspect status before each change set.
- This planning pass edits only this plan and its goal-checkpoint JSON. The already authorized Terra high execution continues through every ready milestone; complete owned local work before waiting for an unavailable owner input or runtime.
- GitHub is source storage only. Source/docs publication is the final execution step; no CI, releases, tags, binaries, images or artifact uploads.
- App development and every App `AGENTS.md` are excluded. No model-default changes.
- Keep Apache-2.0 contracts source-neutral; inspect Hub behavior but never copy AGPL implementation or proprietary App source.
- Product `2026.36.2`, API `1.0`, profile `hub-http-v1@1.0.0`, rich profiles `1.0.0`/`1.1.0`/`1.2.0`, and Edge delivery `2.0.0` are different identities. Coordinate any product-version change with Hub; do not bump wire versions merely to match a product version.
- Do not search the excluded vendor, upstream, historical repository-root, TeslaMate fixture database, or XCFramework trees. Keep secrets and real cursors out of committed examples and reports.

---

## Current evidence and gaps

Inspection date: 2026-09-08. Protocol HEAD was `05225bd` (`Add Hub HTTP and edge-delivery profiles plus live Hub conformance adapters`), with no short-status entries before this plan. The wider ecosystem is unfinished and has preserved work; its dirty-state description must not be mistaken for a dirty Protocol checkout at this observation.

| Evidence level | Observed state | Remaining gap |
| --- | --- | --- |
| Source present | `profiles/hub-http-v1/1.0.0/` includes discovery/auth/resources/errors, OpenAPI, field semantics, cases, examples, hashes and sync appendix. `tools/build_hub_http_profile.py` authors the bundle. The candidate now declares the 4,096-byte claim request bound and extractor text media types. | Confirm the candidate against a fresh Hub runtime before treating the wire correction as accepted. |
| Hub source inspected | `hub/src/api/server.rs` exposes discovery, health/readiness, claim, rotation, vehicles/current/drives and sync routes. `public_query.rs` binds cursors to vehicle and exact millisecond bounds. | Source inspection is not live endpoint verification. |
| Fresh local evidence | On 2026-09-08, the current Protocol tree passed 128 unittest tests and 31 rich-profile case runs through `./tools/check`; focused current-Hub/profile/Edge/v2 contract modules passed. The profile generator check passed with manifest digest `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`. The current-Hub transports reject serialized claim bodies over the profile's 4,096-byte bound before network I/O. The v2 contract manifest binds `tools/protocol-http-v1.schema.json` and `tools/matrix_contract.py` by content hash. | These are local contract and reference checks; they do not prove a running Hub or installed target. |
| Historical source review | Task-3 correction review approved empty drive 503, native provenance, and drive no-store corrections. Task-10 correction-2 review is C0/I0/M0 for bounded integration. | Earlier I6/M2 and later I1 counts in the ledger are history, not outstanding Protocol findings. Preserve deadline, TLS and private-file fixes. |
| Real/installed acceptance | Ledger retains Task-3 native acceptance evidence; Task-10 Protocol status is `approved_bounded_adapter_final_installed_integration_pending`. Protocol now supplies the reviewed v2 SessionInput/evidence/close-ack surface and its 21-case pure predicate. | All three actual installed Protocol rows remain pending. Runner registry admission, host-session/receipt checks, controller binding and cleanup still depend on Hub integration. |
| Compatibility claim | `compatibility/hub.json` is candidate, profile hash null, tested versions/fingerprints/receipts empty. | It cannot substantiate a supported Hub claim. Populate only from verified evidence. |
| User documentation | README and product-versioning now distinguish rich `1.2.0`, current-Hub `hub-http-v1@1.0.0`, and Edge delivery. `docs/current-hub.md` contains private-file discovery, connected-leaf claim trust, bounded reads, cursor handling and rotation examples. | Execute the walkthrough against a dedicated Hub fixture before calling its runtime claims verified. |
| Container support | No Dockerfile or bounded read-only probe exists in Protocol. | Implement both during ready Milestone M2. Runtime validation waits for a Docker daemon and a container-reachable synthetic Hub; native/installed proof remains on its supported host path. |

Relevant retained reports are under `hub/.superpowers/sdd/2026-09-05-hub-ecosystem-compatibility/`: `task-3-fix-1-review.md`, `task-10-protocol-adapter-fix-2-report.md`, and `task-10-protocol-adapter-fix-2-review.md`. They are historical evidence, not freshly rerun results.

Current external dependencies for M3–M5 are fresh Hub sessions, installed-row admission/controller integration, the guarded ARM64 recovery and final immutable sibling pins. Hub owns those runtime and quarantine decisions. They do not block M2. Missing package dependencies use the existing locked environment, not a new harness. Protocol's synthetic acceptance needs no Tesla credentials, private vehicle history, VPS access or vehicle commands.

## Detailed plan review, 2026-09-08

The second read-only review confirmed Protocol `main` at `05225bd5b2f56885025180d68fedf3a42baaa90b`, origin `https://github.com/magrathean-uk/teslatlas-protocol.git`, with only this untracked plan. Hub source was at `7fe8cb202c0eb7e291901a9b719d3fbe65c675bc`; the inspected server/query/pairing paths had no tracked diff. This does not attest to the rest of Hub's working tree or a running Hub.

Historical pre-execution snapshot: checksums of `conformance/hub_matrix.py` and `conformance/hub_http.py` matched the retained correction-2 report. At that snapshot, the exact `profiles/hub-http-v1/1.0.0/SHA256SUMS` bytes had SHA-256 `b3914d35d28374f6423af789e9ed6a4a4c82196a068c041946e24d609db0b05b`, also matching that report. These are targeted source matches, not a complete reproduction of its 116-test gate; the canonical current-Hub profile is now `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`.

| Review finding | Why the first plan needed correction | Required disposition |
| --- | --- | --- |
| Current-Hub OpenAPI omits claim extractor media types | `hub_http.validate_raw` accepts nonempty `text/plain` for claim 400/415/422, while the generator declares no response content for these statuses. | Reconcile OpenAPI, validator, examples and observed Hub responses in Task 1. This artifact inconsistency is visible statically. |
| Claim byte limit was missing from the plan | Hub's public router has `DefaultBodyLimit::max(4 * 1024)`; the profile allows request strings up to 65,536 characters and does not list 413. | Observe the exact over-limit response and document the 4,096-byte serialized UTF-8 body limit. Character lengths alone cannot encode that limit. Treat 413's exact wire form as unverified until execution. |
| Request permissiveness and response strictness were conflated | Hub's claim struct has no `deny_unknown_fields`, while the profile request schema sets `additionalProperties: false`; discovery schemas also admit both two-capability and four-capability forms. | Distinguish the bounded client-emission contract from Hub's actual request acceptance and from strict fixture acceptance. Confirm unknown-field behavior on a disposable invitation; do not silently weaken response validators. |
| Profile validity checks were insufficiently explicit | `tests/test_openapi.py` targets `openapi/teslatlas-v1.openapi.json`; `tests/support.py` loads root `schemas/`. Deterministic generation and example validation alone do not establish current-Hub OpenAPI validity. | Add focused current-Hub schema meta-validation, OpenAPI validation and local reference-resolution checks using installed libraries, in existing profile tests. |
| Native and installed case coverage differed | Native `run_network` covers query/claim/replay/rotation and fixture advancement. Matrix mode adds wrong-secret, expiry refusal, revoke, re-pair, restart and outage cases. | Separate both invocations and their prerequisites. The matrix's expiry case proves local zero-request refusal, not server-side expiry rejection. |
| Walkthrough and adapter would consume the same invitation/state | Native acceptance consumes its invitation and requests one fixture advance using an exclusive marker. | Give walkthrough and each native run fresh owned fixture sessions/invitations. Never delete old evidence markers to make reruns pass. |
| Docker requirements and example were underspecified | Existing tests invoke `cc`, `ps` and `openssl`; native proof and Unix controller access cannot be assumed across container namespaces. | Include actual prerequisites and an offline local gate; use a small curl-backed read-only probe for the external example. Preserve the full adapter's host admission. |
| Final compatibility/pin transition needed an owner | Hub's current version checker still requires candidate records with empty tested arrays; TypeScript's pin is a local-content source binding. | Hub must approve the final record transition before Protocol fills tested fields. Freeze by content for acceptance; final Git commit/pins become available only at the publication step. |

These findings amended the plan; completed source corrections and still-missing runtime observations are classified separately below. They do not overturn the bounded correction-2 review.

TypeScript and Swift consume a frozen Protocol bundle and can do their own work from that content identity without waiting for a Git push. Viewer consumes TypeScript; Home Assistant owns its independent polling client. Their broader installed products and Edge's consumer are not prerequisites for editing Protocol docs or running its native conformance. Hub owns the wider ecosystem claim; Protocol reports only its own acceptance and any specifically received downstream results.

## Working-product acceptance criteria

- [ ] An unaffiliated client can choose the correct profile and complete discovery, trusted invitation claim, authenticated vehicles/current/drives reads, cursor continuation and credential rotation using public files alone.
- [ ] Current-Hub discovery describes `teslatlas-sync`, integer `protocol_major`, `api_versions` as an array of strings, capability strings, UUID identity, product version, `sourceUrl`, pack format and optional `manifestPublicKey` exactly. Both documented capability sets are represented; a fixture requiring drive support rejects the reduced set. It does not require rich version headers or discovery ETags.
- [ ] Invitation `pairingId`/`expiresAtMs`/`tlsPin` and claim `secret`/`device_name` remain distinct actual wire fields, despite general snake_case conventions. TLS identity is checked before credentials; invitation expiry/reuse and old-token invalidation are explicit.
- [ ] Query schemas preserve nullable fields, units, signed-64 integer meaning and lossless/rejected JavaScript IDs. Cursors remain opaque, percent-encoded once and bound to Hub installation, vehicle and exact `from_ms`/`to_ms`; ordering and termination are specified.
- [ ] Endpoint-specific statuses, typed error envelopes, bare errors, drive ETag/304/no-store and unsupported operations match actual Hub behavior. No invented public SSE, command, metadata or charge-query routes.
- [ ] Generated artifacts reproduce; rich profiles and Edge delivery keep their separate gates and semantics; raw-response mutations are rejected.
- [ ] Actual-Hub tests pass against the selected source/artifact and synthetic scenario. Installed claims require the three mandated installed Protocol rows and retained cleanup results; local/reference success cannot substitute.
- [ ] Compatibility metadata names the verified profile hash, Hub identity/version/source and redacted evidence. Unverified combinations stay explicitly unsupported or candidate.
- [ ] `/healthz` describes process health and `/readyz` serving readiness; neither promises recent vehicle data. All field units and null/zero distinctions are explicit. Known response-validation limits are labelled as profile/client limits rather than undocumented Hub guarantees.
- [ ] Public schemas/OpenAPI resolve independently from the profile directory, including an offline environment. Missing files, malformed requests, wrong types, absent prerequisites and unexecuted required cases cannot produce acceptance.

## Ordered core tasks

### 1. Reconcile the existing contract with Hub

**Files:** `tools/build_hub_http_profile.py`; generated `profiles/hub-http-v1/1.0.0/{profile.json,openapi.json,discovery.schema.json,auth.schema.json,resources.schema.json,errors.schema.json,field-semantics.json,sync-regression.json,cases.json,SHA256SUMS}` and affected examples; `tests/test_hub_http_profile.py`.

**Dependency/interface:** Read Hub `src/api/server.rs`, `src/api/public_query.rs`, `src/collection/current_state.rs`, `src/application/main/pairing.rs` and relevant API tests. Hub owns endpoint changes. Produce a reviewed profile bundle and its existing manifest hash for sibling consumers.

- [x] Recheck branch/status, current bundle and selected Hub source. Review all eight profile routes against handlers, extraction errors and authorization branches. Identify any actual drift before changing anything.
- [x] Trace field units/nullability, invitation URI consistency, token expiry, query limits (default 100, range 1–500), half-open time ranges and sync appendix claims to source. Confirm no public revoke route is invented: owner revoke is a lifecycle action outside this HTTP profile.
- [x] Resolve the static claim issues: declare 400/415/422 text media types consistently, distinguish client-emission restrictions from Hub acceptance of extra JSON fields, and write the aggregate 4,096-byte serialized request limit explicitly in the profile/OpenAPI documentation rather than treating `maxLength` as a byte limit.
- [ ] Observe the exact 4 KiB failure and extractor status/media types through the fresh M3 walkthrough session using disposable synthetic invitations. Keep the observed wire result separate from the completed static contract correction.
- [x] For each real mismatch, add an independent failing vector to the existing profile tests, correct the authoring script, then regenerate only affected artifacts. A contract revision requires an explicit compatibility decision and coordinated sibling pins; do not silently rewrite a frozen semantic promise.
- [x] Retain these concrete checks: absent discovery ETag accepted; rich discovery/URN identity rejected; null current retained; signed-64 overflow rejected; `limit=0` and reversed time bounds yield the correct 400 codes; wrong vehicle/filter cursor fails; drive 304 is bodyless with ETag/no-store; drive 503 accepts only its documented empty or typed forms; reused claim returns 401.
- [x] Add current-Hub schema `Draft202012Validator.check_schema` checks and OpenAPI validation through the existing `openapi-spec-validator` dependency, resolving relative schema references from the bundle directory. Check every operation's request/response reference and status/media type against the intended contract. A structural validator does not replace the independent response assertions.
- [x] Keep `SHA256SUMS` as sorted per-file hashes. The public profile digest is SHA-256 of that manifest's exact bytes, not the hash of `profile.json`, a JSON inventory, a tarball or a Git commit. Recompute it after any generated bundle change. Use the existing generator without `--check` to regenerate and with `--check` to verify; never hand-edit the manifest.
- [x] Record Hub's ruling that this is a documented repair to the still-candidate `hub-http-v1@1.0.0` profile and that `b80d…` is the canonical current-Hub content identity. Retain any future incompatible wire change under a new revision rather than rewriting this accepted candidate identity.

**Verification when invalidated by a source change:** `uv run python -m unittest tests.test_hub_http_profile -v`, `uv run python tools/build_hub_http_profile.py --check`. Existing tests already cover these cases; add only uncovered regressions. Exit: every profile change has an independently justified behavior and deterministic generated output.

The implementation review must cover this concrete behavior table. Reuse existing vectors first; rows marked “observe” require a real synthetic Hub request before deciding the final contract.

| Surface | Positive/boundary evidence | Negative evidence and important distinction |
| --- | --- | --- |
| Discovery | Both capability sets, UUID identity, optional absent signing key; health product version agrees | Rich object-valued protocol/capabilities rejected; missing required fields rejected; unsupported capability refused before authenticated use. Profile revision is not advertised as the product version. |
| Readiness | 200 `ready`; 503 `not_ready` with the existing reason enum | Wrong status/body pair rejected; stale or absent current observation can coexist with process health. |
| Invitation/claim | Exact CLI fields and URI agreement; one successful claim; Unicode device name within total body bytes | Missing/wrong-type JSON → observe extractor status/body; wrong Content-Type → observe 415; syntactically bad JSON → observe 400; over-limit UTF-8 body → observe 413; wrong/reused secret → 401. Do not require implementation-specific extractor wording. |
| Expiry/trust | Valid unexpired invitation and correct trusted endpoint | Wrong CA, hostname, connected-leaf pin and URI mismatch refuse before credentials. A coherent expired invitation refuses locally; separately observe server 401 using a directly sent synthetic expired claim. Keep these two proofs distinct. |
| Vehicles/current | Nullable display name, selected vehicle UUID matches response; null remains null, zero remains zero | Unknown current vehicle is bare 404; missing/bad bearer is bare 401. Do not add vehicles pagination or current ETags that are not required. |
| Numeric semantics | Signed-64 IDs; `observed_at_ms` in milliseconds; `scheduled_charging_start_time` in seconds; current speed in integer km/h, odometer in km, route arrival distance in miles | Float/bool/overflow where integer required is rejected; JS consumers reject or preserve values beyond the safe integer range. Drive `efficiency` has no established producer/unit claim: retain null semantics. |
| Drive parameters | Limits 1/500; default 100; nonnegative `from_ms < to_ms`; default `[0, INT64_MAX)` | 0/501 → `invalid_limit`; negative/equal/reversed time bounds → `invalid_time_range`; unknown/duplicate/noninteger/overflowed query fields → observe extraction outcome, normally `invalid_query`. |
| Drive continuation | Five rows, equal-start tie broken by descending ID, `[105,104]`, `[103,102]`, `[101]` at limit 2; final `next_cursor: null`; changing limit retains the same window binding | Altered token, other vehicle/window/Hub key rejected; stop on repeated cursor or exhausted client bound rather than loop forever. Cross-Hub rejection needs a second Hub fixture or a separately retained Hub test; another vehicle is not equivalent proof. |
| Drive conditionals/errors | Repeat identical page with its ETag → bodyless 304 and no-store; preserve in-memory page/continuation across 304 | Missing/invalid ETag, nonempty 304 or absent no-store rejected. Unknown drive vehicle → typed 404; auth failure → bare 401; auth-store and query-store 503 have distinct allowed bodies. |
| Rotation/revocation | Same device with new token after rotation; new token works; revoked device can re-pair as new device | Old rotated/revoked token → 401. Owner revoke is a broker/CLI action, not an invented public DELETE route. |

Byte limits such as the validator's 1,048,576-byte response ceiling and schema array/string bounds describe the bounded profile. Confirm whether each is enforced by Hub before claiming it is a server limit; document a clear client failure for a response beyond the profile ceiling.

### 2. Make the current-Hub walkthrough usable

**Files:** create `docs/current-hub.md`; modify `docs/client-quickstart.md`, `docs/http.md`, `docs/conformance.md`, `docs/product-versioning.md`, README, and affected profile examples.

**Dependency/interface:** Task 1's exact schemas and routes; no SDK implementation dependency.

- [x] Make README choose between current-Hub `hub-http-v1@1.0.0`, richer semantic profiles, and Edge delivery. Add an explicit rich-profile scope label to the existing walkthrough instead of deleting useful rich-contract examples.
- [x] Write the current-Hub flow: discover; obtain private `teslatlas-hub --config PRIVATE_CONFIG pair --json` output from the owner; verify endpoint/trust/expiry; POST claim; store bearer privately; GET vehicles and selected current; iterate drives; rotate and reject old credentials. Explain null observations and permission/authentication failures.
- [x] Include executable examples using protected local files for claim bodies, credential responses and authorization headers; initialize their directory with owner-only permissions. Use `curl --get --data-urlencode` for query values and quote paths. Do not print bearer/invitation responses, place their values in process arguments, or paste them into shell history.
- [x] Make the TLS example implement the actual trust rule. `tlsPin` is the SHA-256 of leaf-certificate DER; curl's `--pinnedpubkey` pins a public key and cannot consume that hex value as an equivalent pin. A bare curl claim using only `--cacert` is insufficient to demonstrate the declared connected-leaf check. For the independent-client walkthrough, document a small Python HTTPS example that enables CA/hostname verification, compares the connected leaf DER hash, then sends the claim on that same connection; reuse existing profile validation helpers. Its endpoint comes from the validated invitation, and response output goes to a private file. Do not turn the loopback-only matrix transport into a general-purpose client. Curl may illustrate already-provisioned read-only requests to an owner-trusted CA/hostname. No preliminary-socket pin followed by an unchecked credential connection, and no `-k` workaround.
- [x] Document per-route ETags/errors and retry boundaries, single-use invitations, no automatic claim retries, no unsupported SSE/commands/metadata, and product-versus-wire versions. Document the existing `--config` argument, required private fixture descriptor and evidence levels.
- [x] For lost claim/rotation responses, document that the operation may already have consumed the invitation or invalidated the old token. Recover through the supported owner pairing flow; do not promise a refresh endpoint, safe blind POST retry or credential rollback that Hub lacks. Do not change Tesla account credentials.
- [x] Explain that ETag/304 support does not override the wire's no-store directive. Distinguish transient conformance comparison bytes from a reusable HTTP cache; do not suggest durable/offline HTTP caching. Stop pagination at null, keep filters stable, and bound repeated pages/cursors.

**M3 validation:** Walk the examples against a dedicated owned synthetic Hub in Task 4; compare each payload to public schemas and verify local links during M2 documentation work. Exit: current-Hub instructions do not require reading sibling implementation or private matrix internals.

### 3. Close conformance integration without replacing it

**Files:** only if required, `conformance/{hub_http.py,hub_matrix.py,hub_control.py,hub_native_evidence.py,runner.py}`, `conformance/adapters/actual-hub`, `tests/{test_hub_http_profile.py,test_hub_matrix.py,test_hub_control.py}`.

**Dependency/interface:** Hub owns shared runner, installed-controller broker, host-session and receipt admission. Preserve existing native/matrix dispatch and private descriptors; reuse the approved five broker operations rather than add another control API.

- [x] Compare current Protocol files with the approved correction-2 boundary; identify changes that invalidate retained evidence. Do not reopen resolved findings without new evidence.
- [ ] Consume Hub's reviewed fixed registry binding and v2 SessionInput for `protocol_actual_hub`. Hub must supply the host/runtime registration, whole-operation budget, exact outage transcript admission and cleanup contract listed in the dependency table. Limit further Protocol edits to a demonstrated integration failure.
- [x] Preserve one absolute HTTP deadline, CA/hostname validation plus connected-leaf pin before request bytes, bounded private files, redacted errors and generation-aware pair/revoke transitions. Retain tests that reject endpoint/profile/fixture omissions; never synthesize success when prerequisites are missing.
- [x] Add only missing direct regression witnesses when touching these seams. The retained review notes that older endpoint/pin invitation mutations also changed URI consistency, so they may refuse before reaching the intended endpoint/pin predicate. Keep mutated invitation fields and URI coherent to test that predicate directly. Keep existing real local TLS and deadline tests; injected clocks do not prove multi-minute installed lifecycle behavior. The native and matrix transports now enforce the profile's serialized 4,096-byte claim bound before network I/O; the focused matrix regression covers that preflight.
- [x] Keep native and matrix modes explicit. Native descriptors come from the fixture's private `ready.json`; matrix descriptors have `kind: "protocol-actual-hub-matrix"`, adapter `protocol_actual_hub`, content-bound profile/source/artifact identities, runtime details and `host_session`. Hub supplies them through its existing runner. Do not handcraft proof records or point either mode at an arbitrary endpoint and call it acceptance.
- [x] If Hub selects a new product cohort, update Protocol `VERSION`, `pyproject.toml`, the root lock entry, compatibility metadata and the matrix's current `PRODUCT_VERSION` expectation with its owner. Audit relevant tests for that exact fixture expectation. Keep product-cohort assertions separate from generic runtime wire compatibility; a later calendar patch is neither automatically compatible nor automatically incompatible. No new cohort was selected.
- [x] Run focused affected unittest modules, then `./tools/check` once the changes settle. Use `uv sync --locked` to restore missing dependencies; report environment failure separately from test failure.
- [x] Add the Protocol-owned v2 installed surface required by the reviewed Hub contract: content-bound `matrix-contract.json`/raw schema/pure predicate, closed SessionInput admission, file-bound normalized/actor/raw evidence, and the evidence-ready/close-completed acknowledgement lifetime. Preserve the v1 config and stdout path. Focused v2 tests and the full local gate pass; Hub registry admission and installed rows remain pending.

**Exit:** artifact generation, local unit checks and 31 rich-profile reference runs are explicitly local evidence. Actual-Hub invocation still fails closed without a valid descriptor. Hub must accept its own integration before installed claims.

Commands to run only when the named source or contract is invalidated, from the Protocol repository root:

```sh
uv sync --locked
uv run python -m unittest tests.test_hub_http_profile tests.test_hub_matrix tests.test_hub_control -v
./tools/check
```

Run the focused modules only when their code or contract is affected; the final full gate already includes them. Do not create a new test runner. `tools/check` also validates rich generated OpenAPI, fixtures, cases and Edge artifacts: retain those checks even though the new task targets current Hub.

### 4. Practical end-to-end acceptance and binding

**Files:** `compatibility/hub.json`; create `docs/compatibility.md` and, if needed, a short redacted `docs/verification.md`. Use existing private evidence storage for raw results; no secret fixtures in Git.

**Dependencies:** Hub supplies the owned TLS fixture with two vehicles/five drives, exact source/artifact identity and the existing private connection descriptor; later, three installed targets (macOS, Debian ARM64, Debian AMD64). SDK TypeScript and Swift consume the same profile; Viewer and Home Assistant consume those public semantics. Edge owns its separate v2 delivery acceptance.

- [x] Add a candidate-only compatibility guide and redacted verification table. Keep `compatibility/hub.json` unpromoted with a null profile digest and empty tested fields until Hub owns the accepted transition.
- [ ] Have Hub prepare two separate owned native sessions: one for the public walkthrough and one for native acceptance. Each gets fresh synthetic data, a fresh CLI invitation, TLS identity and independent output directory. Native acceptance creates a single-use `advance.request` marker and consumes a single-use invitation; do not reuse the walkthrough's invitation or delete retained markers to reset a run.
- [ ] Execute the walkthrough on its session. Send malformed/oversized claims only with synthetic credentials and disposable invitations; record status/media type without body or secret disclosure. If proving server-side expiry rejection, use a dedicated expired invitation and a direct raw request: the matrix's `expired_invitation` case intentionally sends zero requests.
- [ ] On the second session, run native acceptance with the fixture-provided private `ready.json`. The launcher must remain alive and owns cleanup. Confirm query values, null handling, tie ordering, three pages/terminal cursor, conditional drive response, wrong-window/vehicle cursors, replay refusal, fixture advancement and token rotation.

```sh
cd /Users/bolyki/dev/source/teslatlas-service/teslatlas-protocol
# TESLATLAS_HUB_HTTP_CONFIG is the absolute path supplied by Hub for this fresh run.
./conformance/run --profile hub-http-v1@1.0.0 \
  --adapter "$PWD/conformance/adapters/actual-hub" \
  --config "$TESLATLAS_HUB_HTTP_CONFIG" --json
```

- [ ] Use the same entrypoint separately with each Hub-generated matrix descriptor for installed acceptance. Require all 21 current matrix case IDs, exact expected/actual values and the runner's lifecycle/receipt admission. Required groups are identity/service evidence; discovery/health/readiness; bad/replayed invitation and local expiry refusal; real auth/rotation/revoke/re-pair; unknown vehicle/current values; bounded drive order/cursors/304; unsupported-operation zero-request refusal; restart; outage recovery. A unit-test stub executing these names is not an installed row.
- [ ] Record selected Hub build/source, profile hash, case results, evidence type and cleanup outcome. Require the normal child exit, runner/controller completion and stopped-owned-resource evidence, not only a returned JSON status. Missing prerequisites or an unexecuted case is blocked/untested, never passed. Do not operate the quarantined guest independently.
- [ ] Track the three installed rows separately: native macOS arm64; Debian 13 arm64; Debian 13 amd64. Record actual native/emulated execution and tool versions for each. The current 21 cases across three targets make 63 required installed case outcomes; if the case set intentionally changes, update the explicit expected set with Hub rather than use a hard-coded count as the sole gate.
- [ ] Keep sync regression evidence separate. Hub owns existing manifest signature/hash, Range/If-Range, schema-2.2 full-snapshot/no-op and negotiation checks from `tests/tls_import_e2e.rs`, `src/api/server/tests.rs` and `src/sync/updates_delivery.rs`. Obtain relevant results when the selected Hub changes invalidate them. Protocol neither changes the App nor claims query conformance proves App migration, sync import or real vehicle collection.
- [ ] Freeze the profile content/hash for both SDK owners before their final binding checks. TypeScript's current `protocol/lock.json` records a `local-content` source and file map; Swift's current-Hub binding resources also carry the profile. Owners regenerate/copy their own affected artifacts, including downstream references that include `compatibility/hub.json`. Do not edit sibling pins from Protocol.
- [ ] Agree on the compatibility-record transition with Hub before filling tested arrays: its inspected `_check_candidate_records` in `scripts/sync-ecosystem-versions.py` currently requires `status: candidate` and empty tested fields. Hub owns the checker/schema change admitting a verified record. Until then, keep results in the redacted verification document and retain the candidate manifest; do not invent a new status string or falsely empty successful evidence to make a check green.
- [ ] Populate the accepted record with the exact profile-manifest hash, selected product/version/source binding, required query capabilities and verified receipt references. State that sync capability advertisement does not mean an SDK imports sync packs. Use repository-local redacted summaries or clearly labelled private evidence references; no public docs that rely on inaccessible absolute artifact paths. Unknown later products remain unverified; do not publish an untested broad version range.
- [ ] Reconcile unresolved product-review findings and acceptance limits. Core is accepted only when the declared criteria are evidenced; retain candidate status if installed proof remains missing.

**Completion record:** Use one concise table in `docs/verification.md` listing each evidence level/target, exact source/profile identity, executed case set, pass/fail/blocked status and any scope limitation. Keep raw descriptors/invitations/cursors in the existing private storage. Do not introduce a new evidence registry, signing scheme or generic orchestration framework.

## Remaining Protocol-owned product work

Tasks 5–7 are ready local work under M2 and proceed while M3/M4 wait for Hub inputs. Task 8 prepares its review locally and performs publication only after the accepted runtime evidence and owner-binding event in the dependency table.

### 5. Update repository AGENTS guidance

**Milestone/status:** M2 local source — `DONE`.

**Files:** every `AGENTS.md` within this repository (inspection found only root `AGENTS.md`).

- [x] Re-read the [official GPT-6 Astra guidance](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-6-astra), inspected during planning on 2026-09-08. Keep the existing useful section short: finish authorized scope, preserve session authority, resolve routine choices, explain material blockers, use bounded ownership for useful parallel work, and perform proportionate verification with plain outcome-first reports.
- [x] Remove conflicting stale rules; clarify actual wire spellings are preserved even where general naming advice differs. Do not change model defaults or add elaborate agent machinery.
- [x] Give Hub the list of reviewed Protocol AGENTS paths. Hub coordinates workspace-level files and coverage across Hub, Protocol, TypeScript SDK, Swift SDK, Viewer, Home Assistant and Edge. Each owner edits its own repo; nobody edits App AGENTS files. Check the local diff for scope and duplicated guidance.

### 6. Add a small reproducible Docker solution

**Milestone/status:** M2 source, tests and static documentation — `DONE`; Docker runtime evidence is `WAITING FOR OWNER/RESOURCE` because the daemon socket is unavailable.

**Files:** create `Dockerfile`, `.dockerignore`, `docs/docker.md`, `tools/check-current-hub` and `tests/test_current_hub_probe.py`. The probe is one small executable Python wrapper around installed curl and existing profile validation, not a new HTTP client library. Modify `tools/check` only if necessary to reuse a preinstalled locked environment.

- [x] Define one image with an explicit Python version compatible with `>=3.11`, pinned base/uv digests, the existing locked dependencies, C compiler/libc headers, `procps`, `openssl`, `curl` and CA certificates. `tests/test_hub_http_profile.py` invokes `cc` and native process inspection; `tests/test_hub_matrix.py` invokes `openssl` for local TLS fixtures. No Rust, Hub binary, Node, Swift, Docker socket, host PID namespace, privileged mode or new service is required for the local Protocol gate.
- [x] Configure `uv sync --locked` during the image build, including the dev dependency group, an image-owned environment, and `UV_OFFLINE=1` after dependency installation. Exclude host `.venv`, `.git`, caches, `.DS_Store`, credentials and private evidence from the build context; include all profile/schema/example/fixture/test/tool files the gate needs. Prefer explicit source COPY paths to copying a developer's entire directory.
- [x] Use `WORKDIR /workspace/teslatlas-protocol` and a default `CMD ["./tools/check"]`, allowing the external command below to replace it. Do not use a fixed entrypoint that accidentally passes the probe as an argument to `tools/check`. Put the installed Python environment on PATH for direct executable scripts.
- [x] Document these commands from the Protocol repository root, with no source or sibling repository mounts:

```sh
docker build -t teslatlas-protocol-check .
docker run --rm --network none teslatlas-protocol-check
```

- [x] Implement the read-only probe CLI as `./tools/check-current-hub --config /absolute/private/probe.json`. It loads the embedded profile using `hub_http.load_profile` and validates raw bytes with `hub_http.validate_raw`. It uses curl through an argument array with bounded process I/O and a total deadline; it does not duplicate the matrix transport's socket/deadline machinery.
- [x] Define a closed private probe configuration with the following fields. The synthetic sample values below illustrate the runnable shape; Hub supplies the actual endpoint and identity. `authorization_header_path` contains exactly one private `Authorization: Bearer ...` line, read through curl's header-file option, never expanded into command arguments. The JSON contains no bearer value.

```json
{
  "endpoint": "https://host.docker.internal:8443",
  "expected_hub_id": "11111111-1111-4111-8111-111111111111",
  "ca_path": "/run/teslatlas/ca.pem",
  "authorization_header_path": "/run/teslatlas/authorization.header",
  "vehicle_id": "22222222-2222-4222-8222-222222222222",
  "from_ms": 0,
  "to_ms": 9223372036854775807,
  "limit": 2,
  "max_pages": 3,
  "timeout_seconds": 30
}
```

- [x] Reject unknown config fields, non-HTTPS/userinfo/query/fragment endpoints, invalid UUIDs, out-of-range filters/limits, nonprivate secret files and unbounded budgets before credential use. Support integer `max_pages` 1–10 and total `timeout_seconds` 1–120; these are probe limits, not Hub limits. Resolve secrets only from the configured private directory; accept no arbitrary curl options or shell text.
- [x] Probe unauthenticated discovery and expected UUID/profile/capability checks; authenticated vehicles with the explicitly selected vehicle present; selected current; the first bounded drive page; the same page with its ETag for 304; and at most `max_pages` drive pages total while retaining exact filters and opaque cursor values. Refuse repeated cursors. If the page bound is reached with a non-null cursor, report `partial` and a nonzero exit for this verification command; never report complete-history success.
- [x] Require normal CA and hostname verification on every connection. Supply no credential on discovery, disable curl's implicit config/proxy behavior, explicitly disable retries, do not follow redirects, and reject unexpected statuses. Apply curl's transfer timeout using the remaining total budget and cap captured body/header bytes before parsing. Retain the existing 1 MiB profile response ceiling, a small header cap, and fixed redacted errors. HTTP bodies, cursor values, authorization headers and private paths stay out of the public result.
- [x] Keep the probe limited to an owner-provisioned bearer and trusted endpoint/CA; it does not implement invitation trust or validate `tlsPin`. Its result says `evidence_kind: remote-wire-smoke` and lists only executed read operations. It cannot satisfy full current-Hub pairing/lifecycle or installed acceptance. It does not repurpose curl's public-key pin option as a DER leaf pin.
- [ ] Document this invocation after validating the implemented CLI and mount permissions:

```sh
docker run --rm \
  --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$TESLATLAS_PRIVATE_DIR,dst=/run/teslatlas,readonly" \
  teslatlas-protocol-check ./tools/check-current-hub \
  --config /run/teslatlas/probe.json
```

- [ ] The direct probe uses the image's installed interpreter without attempting uv sync as the mounted-file owner's UID. It needs writable temporary space inside the disposable container, not write access to mounted secrets or the environment. On platforms that translate bind-mount ownership, verify the observed permissions and document the supported user mapping; do not chmod private inputs world-readable.
- [ ] For Docker Desktop, `host.docker.internal` reaches the host; the Hub URL certificate must cover that hostname and the service must be reachable at the selected address. On native Linux, add `--add-host host.docker.internal=host-gateway` for the documented gateway example and have Hub prepare a listener reachable through it; this flag does not make a loopback-only listener reachable. Host networking is an optional native Linux example only if deliberately selected. No `-p` port publication is needed for an outbound-only checker.
- [ ] The existing full adapter requires same-host executable/controller evidence and loopback HTTPS. Docker Desktop's VM separation remains incompatible with assuming native process identity from this container. Keep full native/installed acceptance on the supported host. No new orchestration, proxy service or relaxed proof admission to make the Docker example pass.
- [ ] Validate the image's local gate with network disabled, then the probe against a separately running synthetic Hub from a supported Docker host. Check wrong CA/hostname, changed UUID, invalid bearer, malformed/oversize response, redirect, repeated cursor, page exhaustion and unavailable endpoint; verify no secret appears on stdout/stderr and that child processes exit. Reuse the existing small TLS test fixture where practical; do not require a sibling Hub checkout for local probe unit tests.
- [ ] Record tested image architecture/host explicitly. Validate arm64 and amd64 before claiming both container targets; emulated success stays labelled emulated and does not establish native installed Hub acceptance. Missing targets remain pending, with no multi-platform registry publication.
- [ ] No persistent volume is required for the checker. Private input mounts are read-only; optional redacted results can be redirected by the host; all Hub configuration, credentials and vehicle data remain with the separately managed Hub. A failed check changes none of that state.

Implementation references verified during this review: [uv offline environment setting](https://docs.astral.sh/uv/reference/environment/#uv_offline), [curl transfer deadlines and header-file options](https://curl.se/docs/manpage.html), and [Docker host access](https://docs.docker.com/desktop/features/networking/networking-how-tos/). Verify selected image/tool versions during development; this plan does not invent an image digest or claim an unbuilt image works.

### 7. Reconcile all current product documentation

**Milestone/status:** M2 documentation — `DONE`; final receipt values remain waiting on M3/M4.

**Files:** README; `docs/{architecture.md,client-quickstart.md,current-hub.md,http.md,conformance.md,product-versioning.md,versioning.md,compatibility.md,docker.md,verification.md}` where created; also explicitly review `docs/{data-model.md,events.md,commands-and-metadata.md,standards.md}`, `docs/plans/2026-08-30-foundation.md`, root AGENTS and all new current documentation. Inventory again at execution so newly added docs are not missed.

- [x] Document actual prerequisites, locked setup, profile selection, invitation/auth flow, API limitations, conformance versus installed proof, Docker commands/networking/private mounts, and troubleshooting for dependency setup, TLS, expiry, 401, cursor mismatch and missing fixture/controller evidence.
- [x] Reconcile stale profile-export language and unsupported-current-Hub claims. Label rich events/commands/metadata documents by profile; retain historical plans as historical rather than rewriting their recorded outcomes. Review external links and verify all repository-relative links resolve.
- [x] Correct architecture claims that all OpenAPI is self-contained: the rich document embeds schemas, while current-Hub `profiles/hub-http-v1/1.0.0/openapi.json` uses adjacent schema files. Document distributing the whole profile bundle. Clarify the current-Hub OpenAPI version actually present (`3.1.0`) separately from the rich document (`3.1.1`); do not bump either just for cosmetic alignment.
- [x] Explain that Protocol has no package build/server daemon: `pyproject.toml` uses `package = false`. Its supported installation is a source checkout or Hub-coordinated source bootstrap plus locked tooling. Hub owns fresh/update/rollback bootstrap validation and source catalogs; Protocol supplies the standalone CLI/profile layout and does not implement another installer.
- [x] Re-run documented commands only where earlier evidence does not already cover the final command/source. Confirm no docs imply CI, hosted deployment or a Protocol server. Hub owns shared ecosystem documentation; the Protocol AGENTS-path/status summary was sent without editing shared files.
- [x] Ensure every recommended command is classified as local validation, owner pairing, synthetic native acceptance, installed acceptance or read-only remote smoke. New planned filenames/options exist before README links them as supported. Review public examples for real identity/location/credential content and broken links; preserve historical documents with explicit context.

**Exit:** A new reader can install the tooling, select the correct contract, understand all command prerequisites, and interpret its results from public repository files alone. No claim depends on a private workspace report or a hosted release asset.

### 8. Final review and source-only GitHub update

**Milestone/status:** M5 local diff review — `DONE`; commit/push and remote verification are `WAITING FOR OWNER/RESOURCE` until M3/M4 and cross-owner binding are accepted.

**Files:** final task-owned source/docs changes in this repository only.

- [x] Inspect branch, remote and full working/index diff. Identify task-owned paths and any pre-existing/concurrent edits; review only the intended change set. Correct the equal drive-bound preflight and explicitly disable curl retries before publication.
- [x] Verify deterministic artifacts, meaningful local tests, documentation, Docker and current-Hub evidence applicable to the final source. State missing installed combinations explicitly; do not promote compatibility status beyond proof. The focused probe/current-Hub suite passed 29 tests; Docker image/runtime evidence remains unavailable because its daemon is down.
- [ ] Close the pin ordering without a circular prerequisite: freeze task-owned source/profile content for prepublication acceptance; Hub and SDK owners verify that exact content locally; Protocol's final commit/push then provides the immutable public revision. Owners finalize their Git-ref bindings after receiving that revision. The final source commit must contain the reviewed content; a future commit ID is not written into its own files, and a hash-only local candidate is not described as publicly downloadable.
- [ ] Coordinate product version with Hub before the final gate. Stage explicit task-owned paths/hunks, inspect the staged diff for secrets, unintended files and binaries, then commit and push the verified branch/remote as the final execution operation. Reconfirm `main` and the expected Protocol origin rather than relying on this plan's snapshot. Preserve unrelated staged/unstaged work; never use blanket staging or force push. If a concurrent edit overlaps a task-owned hunk, resolve ownership/content before staging rather than reset, stash or include it silently.
- [ ] Verify the remote source commit and report the published source/docs and acceptance limits. Do not create CI, release pages, tags, binaries, images or artifact uploads. This planning pass performs no Git publication; the already authorized Terra high execution performs it only after the M5 unblock event.

**Execution note:** The plan was previously planning-only; the user's explicit “execute plan” instruction superseded that pause. Checkboxes track evidence, not worker lifetime. Continue through all ready milestones and finish every meaningful authorized local implementation/review item before waiting on an unavailable runtime or owner input. A later phase boundary, a passing sub-gate or a short elapsed time is not whole-goal completion. Report an unavailable resource only for the milestone it actually blocks.

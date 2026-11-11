# Teslatlas Home Assistant Working Product Development Plan

**Latest user model policy (2026-09-08):** This coordinator and delegation use `gpt-6-astra` with `thinking=high`. This product task, coding, goal execution and all development workers use `gpt-5.6-terra` with `thinking=high`. This supersedes every earlier model instruction in this plan and its historical goal snapshot. Preserve checkpoints at model transitions and verify the actual new turn model.

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` to implement this plan task by task. Track work with the existing checkboxes and evidence boundaries.

**Goal:** Deliver an installable, read-only Home Assistant custom integration that pairs with a real Hub, polls current vehicle state reliably, survives authentication and runtime lifecycle changes, and reaches evidence-backed source-only publication.

**Architecture:** Finish and retain the existing current-Hub client, config flow, coordinator and sensors. Reuse the existing installed acceptance adapter and Hub-owned runner. Keep `hub-http-v1@1.0.0`, one bounded polling authority and the existing entity identities; do not add event streaming or another common runner.

**Tech Stack:** Python >=3.14.2, uv lockfile, Home Assistant 2026.8.3 baseline, aiohttp, pytest Home Assistant fixtures, Ruff, official Home Assistant Container and minimal Compose.

**Spec and authority:** User's 2026-09-08 planning and execution requests; workspace `WORKSPACE_AUTHORITY.md`; `docs/superpowers/plans/2026-09-05-hub-ecosystem-compatibility.md`, especially Tasks 7, 10 and 12; current Hub compatibility ledger and reports below. The authorized objective spans the full remaining product scope; a phase boundary or owner wait does not complete or block the whole goal while useful HA-owned work remains.

**Status: Execution in progress. HA-owned source, documentation, static packaging and M2 packet reconciliation are complete; installed acceptance is waiting on Hub-owned admission and runtime resources.**

**Detailed review:** Revised 2026-09-08 after a second source/report/requirement check. This is an execution plan with unresolved product and environment gates, not a declaration that the integration is ready to install in production.

The user explicitly authorized execution after this plan was written. Local implementation, regression testing, and bounded adapter checks have now run in the existing `main` checkout. No branches, resets, cleans, commits, pushes, containers, services, credentials, or Hub storage access were used.

**Current checkpoint (2026-09-08, M2 source-manifest refresh and final Azure contract):** the independent HA checkout remains on `main` at `f650331a1af0cf33cec271bc7eefc0f1201ebd4e`, with all 40 dirty paths classified in the handoff and unrelated `.DS_Store`/historical documentation preserved. The canonical embedded profile is `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`; the refreshed HA-to-Hub handoff is `5d496de004faeb1880a78746a429823e7300d4597147ef9f36a7c7457c37648b`, binding its HA-owned ten-file source manifest `e74a955374003be9820a450174715412b4a9c1d504b47eb33d1f93e6b2173832`; its 31-member component manifest is `0a4ec2d743a3f181423c24bbf7c6d9d7e0feef5dc0f0fc3e5d3d69150945e896`; and the refreshed provenance binding is `33cda376023bfff2b1f2361abc52bc110bdd21387b15a712c2d7dafebc49415e`. The final Azure Debian v1 contract is `d9617354...`, ordered after the existing Mac and VPS verification. The locked HA suite remains `115 passed, 2 skipped`; after the one-file source-manifest drift, its changed matrix module is `49 passed` and Ruff passes. The skips are the two opt-in live tests. The static Compose check remains passed. The TypeScript sibling profile and validator bindings match the canonical profile and pass their source gate. HA still has no fixed registry entry or admitted installed runtime. The Hub completion audit remains `0/21` accepted installed cells, `0/444` accepted slot decisions and `0/10` accepted cohorts. No installed, UI, normal-scheduler, replacement, Docker-runtime or source-publication acceptance is claimed.

**Execution model policy:** the latest policy at the top of this plan controls this checkpoint and every subsequent HA development or verification worker: `gpt-5.6-terra` with `thinking=high`. Hub-owned controller, registry and host work remains in the Hub task rather than being copied into this repository.

**Full remaining end-to-end objective:** carry the existing integration from its source-bound handoff through Hub admission and real installed acceptance. Require the fixed HA adapter/runtime registry entry, pinned HA 2026.8.3 resources, three Hub targets, normal 30-second scheduler evidence, UI pairing, reauthentication, replacement, reload and unload evidence, plus a supported Linux Home Assistant Container run. Then bind only admitted evidence into compatibility metadata, review the exact final diff and publish the authorized source-only commit. Local tests, source review, static Compose validation and historical receipts remain supporting evidence rather than substitutes for those runtime results.

| Milestone | Status | Owned files or interface | Completion evidence |
| --- | --- | --- | --- |
| M1. Canonical HA source and handoff | DONE | `custom_components/teslatlas_hub/`, `tests/`, `tools/`, `docs/hub-admission-handoff-2026-09-08.json` | Profile `b80d940e...`, refreshed handoff `5d496de0...`, source manifest `e74a9553...`, component manifest `0a4ec2d...`, provenance `33cda376...`, `115 passed, 2 skipped`, changed module `49 passed`, Ruff pass |
| M2. Reconcile final HA-owned diff and admission packet | DONE | HA checkout status/diff; refreshed source manifest/handoff; final Azure contract; plan; `compatibility/hub.json`; README/docs; no Hub mutation | All 40 paths classified; private ten-file source manifest superseded after one matrix-test change; new handoff `5d496de0...`; final Azure ARM64/x86_64 contract is ordered after Mac/VPS verification; all current source/profile/component/provenance identities and public candidate claims reconciled |
| M3. Admit the HA adapter and pinned runtime | WAITING FOR OWNER | Hub task `01a07f89-45cb-7ea2-b96e-aa89de18fd14`: fixed registry, runtime inventory, controller and ledger | Exact HA manifest/launcher/profile accepted; pinned HA 2026.8.3/Python 3.14.2 actor paths and usable macOS ARM64, Debian ARM64 and Debian AMD64 targets admitted |
| M4. Run installed HA lifecycle acceptance | WAITING FOR RESOURCE | Existing HA matrix adapter plus Hub SessionInput/controller; [Azure Debian v1 evidence contract](../../azure-debian-v1-evidence-contract-2026-09-08.md); no new common runner | Three admitted 18-case cells, including owner-provided disposable Azure Debian ARM64/x86_64 lanes, plus separate UI, normal-scheduler, reauth, replacement, reload and unload records; G1/G1/G2 and final stopped/cleanup evidence |
| M5. Validate Linux Container and publish source | WAITING FOR RESOURCE | `compose.yaml`, `docs/docker.md`, README/docs, `compatibility/hub.json`, explicit owned Git paths | Supported Linux Docker run and recreation pass; metadata contains only admitted receipts; final review passes; source-only commit/push verified without CI, release, tag, HACS or artifact upload |

The current product-only coverage run (excluding the correction-3 matrix harness and opt-in live module) passed `66` tests at `89%` line coverage under Python 3.14.7. The full suite separately reports 2 opt-in live skips. A whole-tree coverage attempt exposed one timing race in the matrix test's private acknowledgement writer: the reader observed the file between the write and its `chmod(0600)`; the unchanged barrier test passed five consecutive isolated reruns, so no production code was changed or relabeled as a product failure.

Workspace references resolve under `/Users/bolyki/dev/source/teslatlas-service`, not under this product's `docs/`. Shared review filenames without another prefix resolve under `/Users/bolyki/dev/source/teslatlas-service/hub/.superpowers/sdd/2026-09-05-hub-ecosystem-compatibility/`. The correction-3 report's exceptional private path is given explicitly below. `component/` in task tables is shorthand for this repository's `custom_components/teslatlas_hub/` directory; it is not a new directory to create.

## Current continuation control

The latest model policy at the top of this plan controls all coordination and HA development. The native goal remains blocked. With the available tools, resumption requires the user's supported Resume goal control. The parent can dispatch ordinary authorized development turns now; this does not activate the native goal. Do not replace an unfinished goal, mark it falsely complete, or edit internal app state. M2 is complete; retain the full proposed objective and acceptance gates while waiting for M3's Hub-owned unblock event.

## Global Constraints and ownership

- Work in the existing independent `main` checkout at `/Users/bolyki/dev/source/teslatlas-service/teslatlas-home-assistant`. Preserve unrelated edits; no branch, worktree, reset, clean or stash.
- All file paths below are relative to this product repository unless explicitly qualified. This task edits only this repository, including its own AGENTS files. Hub owns shared state, shared runner/controller and ecosystem documentation.
- Do not edit the App or any App AGENTS.md. GitHub is source storage only: no CI, tags, releases, HACS submission, binaries or artifact uploads.
- No Tesla credentials, vehicle commands, private APIs, Hub storage access or unsupported `/v1/events`. Synthetic data belongs in isolated acceptance fixtures managed by Hub.
- Keep product `2026.36.2` separate from wire profile identity. Recheck version authority before future edits; do not invent release or compatibility claims.
- Use existing tooling. Resolve an actual runtime/dependency problem directly; do not add another generic validation framework. Record concise commands, versions, outcomes and exact tested inputs; keep secrets/private data outside source control.
- Checkboxes reflect current evidence. Continue the authorized execution from the first unmet milestone; do not restart completed phases or repeat green checks unless later bytes or evidence invalidate them.
- The prohibition on Hub storage access applies to the integration. Hub's existing isolated fixture controller may prepare synthetic data under its own authority. This task must not bypass it, weaken host verification, or reuse a quarantined guest just to make an acceptance cell pass.

## Detailed plan-review corrections

The second review found and resolved these planning defects in this file. Execution checkboxes below reflect only work with current evidence; external review, installed acceptance and final packaging remain unchecked until their owners provide the required evidence.

| Original planning gap | Revision and reason |
| --- | --- |
| Tasks 1–2 invoked tools before environment setup in Task 3 | Task 0 now establishes runtime and locked dependencies first; review-only work can continue if runtime setup is blocked |
| Abbreviated source paths and shared references were ambiguous | Explicit repository/workspace/component path rules and source responsibility table |
| Supported-version claims were insufficiently bounded | `hacs.json` says HA 2026.8.0 while the locked test baseline is 2026.8.3; choose the verified minimum explicitly and never infer support from a manifest |
| Cleanup work covered only the already reviewed branch | Add bounded cancellation reproductions for adjacent ownership paths, clearly labelled unverified candidates; no broad refactor |
| Natural polling was assumed from live tests | Separate manual-refresh, shortened-timer, real scheduler and browser/UI evidence |
| Upgrade/migration and retired sensors lacked a deliverable | Task 3b specifies saved-entry admission, actual registry preservation, retirement and rollback checks |
| The 18-case gate was opaque | List all 18 identifiers, the runner-owned case, required inputs and the distinct legacy live launcher |
| Host blockage was generic | Identify the recorded ARM64 quarantine, accepted payload-hash correction and still-pending fresh runtime retry, owned by Hub |
| TLS bootstrap and invitation field mapping were vague | Explicit trusted-TLS prerequisite, private field mapping and no plaintext authentication claim |
| Docker example omitted shutdown/operational checks | Add 60-second grace, mount checks, persistence/recreation and update/rollback behavior |
| Final Git step omitted dependency/review invalidation detail | Recheck final component bytes, stage owned paths, keep source publication last and preserve unrelated work |

## Source responsibility map

| Existing files | Responsibility to retain |
| --- | --- |
| `component/config_flow.py`, `component/strings.json`, `component/translations/en.json` | Manual endpoint probe, invitation claim, duplicate prevention, reauth/reconfigure and user errors |
| `component/client.py`, `component/current_hub_client.py` | Client protocol/factory, HA session ownership, trusted/pinned HTTP and current-Hub decoding |
| `component/coordinator.py`, `component/__init__.py` | Single refresh authority, availability/auth transitions, setup/unload and migration |
| `component/models.py`, `component/sensor.py`, `component/entity.py` | Immutable current-state projection, stable registry identities and only bound sensors |
| `component/diagnostics.py` | Redacted aggregate diagnostic output |
| `component/manifest.json`, `hacs.json`, `pyproject.toml`, `uv.lock` | Component packaging, version declarations and development runtime |
| `component/profile/hub-http-v1/1.0.0/`, `compatibility/hub.json` | Embedded approved public contract and evidence-backed compatibility declarations |
| `tools/matrix_live.py`, `tools/matrix_wire.py`, `tools/matrix_contract.py`, `tools/matrix-contract.json`, four `tools/ha-*-v1.schema.json` files | Existing installed test adapter and semantic admission; no new common framework |

## Current evidence and gaps

Inspection date: 2026-09-08. This table preserves the pre-execution review; current execution results are recorded in the status update above.

| Area | Observed source or retained evidence | Remaining gap |
| --- | --- | --- |
| Working tree | `main`; existing changes in `tests/test_matrix_live.py`, `tools/matrix_contract.py`, `tools/matrix-contract.json`; unrelated untracked `.DS_Store` and `tools/.DS_Store` | Preserve these inputs and distinguish inherited correction work from new fixes |
| Product runtime | `config_flow.py`, `current_hub_client.py`, `coordinator.py`, `__init__.py`, `sensor.py` implement pairing, identity checking, polling and lifecycle | Working source is not final installed acceptance |
| Discovery | Manual endpoint probe is implemented; README explicitly disables Zeroconf because Hub lacks required advertised identity/TLS fields | Accept manual discovery; automatic discovery requires a separate verified Hub capability |
| Polling | 30-second schedule, 30/60/120/300-second backoff, four current reads maximum, serialized snapshots, 10-second request timeout, 1 MiB response bound | Verify real outage/reconnect/unload behavior on final inputs |
| Earlier Task 7 | `hub/.superpowers/sdd/2026-09-05-hub-ecosystem-compatibility/task-7-fix-1-review.md`: scoped PASS WITH MINOR; recorded local/Linux 50 passed plus one disabled live test; one real macOS-Hub live lane passed | Earlier installed HA 2026.8.3/Python 3.14.2 evidence does not cover the final three-target matrix |
| Product review M1 | The earlier review found that wrong-Hub reauthentication returned before closing its owned client. The bounded implementation now closes on success, error and cancellation, and the cancellation regression passes. | Installed reauthentication and same-entry continuity remain part of M4. |
| Adapter review | `task-10-ha-adapter-fix-2-review.md` historically recorded C0/I1/M0 because I3 admitted an unchanged service generation. Correction 3 now enforces and tests G1/G1/G2. | The source defect is closed; registry and installed acceptance remain pending. |
| Correction 3 | Historical reports retain the earlier 46-pass and 49-pass source snapshots. The current handoff binds the refreshed 31-member component manifest `0a4ec2d...`, provenance `33cda376...`, independent review `e34992c...`, canonical profile `b80d940e...`, and full `115 passed, 2 skipped` suite. | Source semantics and provenance pass. No pinned container, installed lifecycle or registry admission exists yet. |
| Ledger freshness | `hub/docs/compatibility/execution-state.json` records the current HA handoff as `source_bound_canonical_profile_refreshed_installed_runtime_pending`, the TypeScript sibling refresh as complete, Hub local gates including 176 interop, 132 installed-host, 28 Node-lane and 66 companion-bootstrap tests, and the broader Rust gates. | Hub must admit the exact HA adapter/runtime and execute installed acceptance; no source review authorizes that promotion. |

The exact current HA-to-Hub handoff is recorded in [`docs/hub-admission-handoff-2026-09-08.json`](../../hub-admission-handoff-2026-09-08.json), SHA-256 `5d496de004faeb1880a78746a429823e7300d4597147ef9f36a7c7457c37648b` after the M2 source-manifest refresh and final Azure-contract preparation. Its HA-owned ten-file source manifest is `e74a955374003be9820a450174715412b4a9c1d504b47eb33d1f93e6b2173832`: it supersedes the private `bff35f13...` manifest after the one changed `tests/test_matrix_live.py` hash, with the other nine files revalidated byte-for-byte. The handoff binds that source manifest, the 31-member component manifest, four raw schemas, G1/G1/G2 rule, reauth cancellation cleanup, launcher/barrier digests, static Compose boundary, all 40 path classifications, the final Azure-contract digest, and the precise Hub-owned registry seam. The embedded HA profile and refreshed TypeScript sibling match Protocol's canonical current-Hub working-tree snapshot `SHA256SUMS` `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926` (profile `5b939b06...`, OpenAPI `af46fba3...`), including the bounded 4096-byte claim contract. No installed container ID has been claimed. Hub still must admit HA's own adapter and installed runtime independently.
| Compatibility/docs | `compatibility/hub.json` is candidate, with the embedded profile hash but empty tested Hub versions/fingerprints/receipts; product-versioning and README now describe that status accurately | Candidate remains untested and unpublished; admitted Hub fingerprints, receipts and final installation documentation remain pending |
| Version declaration | `hacs.json` declares minimum HA `2026.8.0`; `pyproject.toml`/`uv.lock` pin HA `2026.8.3`; lock records aiohttp `3.14.3`, pytest `9.0.3`, HA custom-component test plugin `0.13.357` | Align declared minimum with tested evidence; actual runtime versions must be observed, not copied from a receipt label |
| Live-test reach | Both existing live tests call HA flow APIs and explicit `async_refresh()`; the matrix unload check schedules an actual timer at 50 ms | Neither is a browser walkthrough or proof of the normal 30-second schedule by itself |
| Migration source | `async_migrate_entry` advances known minor metadata without checking required credential/endpoint data; setup indexes these fields directly | Exercise incomplete fixture-era entries through setup and provide controlled recovery; metadata migration alone is not usable pairing |
| Adjacent cleanup candidates | `_async_probe` has no cancellation cleanup around its network await; reconfigure awaits unique-ID assignment before its cleanup branch; setup catches `Exception`, which does not include `asyncio.CancelledError` | Source-observed ownership concerns, not reproduced bugs in this planning run; add focused cancellation reproductions before changing them |
| Installed ARM64 prerequisite | Ledger `task10_actual_installed_prerequisites` records the stopped/quarantined guest and `payload_hash_correction.status=accepted_C0_I0_M0_fresh_guest_runtime_retry_pending` | Hub must provide a freshly admitted usable target; no current runtime recovery is claimed |

Correction 3 separates stable scenario/seed/store/schema/Hub identity from service generation: initial/pre-advance must be G1/G1, then post-advance must be G2. Existing tests admit G1/G1/G2, reject G1/G1/G1 and keep a missing independent observation pending. Do not recreate this fix or treat its synthetic composition test as real HA execution.

## Working-product acceptance criteria

- [ ] A clean supported HA instance discovers the installed custom component and completes its real UI config flow using endpoint, TLS trust/pin and an isolated Hub invitation. Duplicate Hub entries are prevented; invitation secrets are not persisted.
- [ ] Public discovery validates the expected Hub before any saved bearer or claim material is dispatched. Wrong identity, wrong pin, untrusted CA, redirect, replayed/invalid invitation and unsupported profile fail with useful errors and no credential disclosure.
- [ ] Manual endpoint discovery is documented and works. No Zeroconf or streaming claim is made without an independently verified public Hub contract.
- [ ] Real vehicle sensors match known Hub observations and units, including zero versus missing/null values. Later observations appear on the next successful scheduled poll. New vehicles get one sensor set; removed vehicles become unavailable without changing matching registry IDs.
- [ ] One refresh authority runs per entry at 30 seconds, at most four current reads run concurrently, failed/cancelled batches are drained, and transient failures back off no further than 300 seconds. A recovered connection returns to the normal interval.
- [ ] Outage marks entities unavailable; absent values remain unknown. Revoked/expired authentication enters reauth, a fresh invitation recovers the same entry, and reload preserves registry identity.
- [ ] Reload/unload leaves no old requests or scheduled dispatch after the cancelled deadline; cleanup preserves HA's shared connector. Reauthentication closes its owned wrapper on every exit.
- [ ] Diagnostics/logs omit credentials, pairing material, endpoints and sensitive identities/location values. No commands, SSE requests or replay headers are emitted.
- [ ] Existing supported config entries survive a component replacement with their entry ID, entity IDs, custom names, disabled flags and valid device credential intact. Missing legacy credential/endpoint fields produce controlled repair or a clear unsupported-entry result, never an unhandled missing-key failure or invented live pairing.
- [x] Manifest/HACS/README runtime requirements agree with measured support. The locked/runtime-tested baseline is Python 3.14.7 with Home Assistant 2026.8.3, HACS metadata now names 2026.8.3, and local sensor/coordinator tests cover empty lists plus later vehicle discovery; installed scheduler timing remains external.
- [ ] Final Linux HA runtime passes against Hub macOS ARM64, Debian ARM64 and Debian AMD64 with exact product/profile/runtime inputs and independently admitted lifecycle evidence. Failed/pending/skipped cells stay explicit.
- [ ] Final Compose installation and documentation work from a fresh persistent configuration; source-only publication happens only after the final review phase.

## Ordered implementation tasks

### 0. Establish execution inputs and a usable development runtime

**Read:** `pyproject.toml`, `uv.lock`, current Git diff and the reports above. **Modify:** no dependency or toolchain declarations unless a demonstrated incompatibility requires a separately reviewed bounded change.

- [x] After explicit start, run from the product root and record branch/status before edits. Confirm that the existing three-file correction remains the intended baseline; no reset or blanket staging.
- [x] Inspect the available interpreter and use Python 3.14.2 for the exact historical baseline when available. A later Python allowed by `>=3.14.2` is a separate measured local lane, not proof of the pinned runtime. Establish `uv sync --locked --group dev` before any task that invokes pytest/Ruff.
- [x] Confirm runtime identity using `uv run --locked python --version` and `uv run --locked python -c 'from importlib.metadata import version; print({name: version(name) for name in ("homeassistant", "aiohttp", "pytest", "pytest-homeassistant-custom-component")})'`. Record actual outputs alongside the run, not a hardcoded HA version string.
- [ ] If a required wheel/system dependency is unavailable on the Mac, use Hub's existing supported Linux development/runtime lane after it is admitted. Record the specific interpreter/package failure and continue independent source/review work; do not loosen the lock, substitute mocked HA modules, or create another validation framework as a workaround.
- [x] Keep opt-in live variables absent during local checks. With `TESLATLAS_HA_LIVE` unset, the existing two live tests skip by design; a local green run must state this limitation. No unprepared live command is a meaningful verification attempt.

**Dependencies/exit:** The verified locked local runtime has completed Tasks 1–3b test execution. Existing installed host/container ownership remains with Hub. HA-owned static Container configuration and documentation are already prepared; runtime claims wait for the supported Linux execution.

### 1. Reconfirm baseline and close correction-3 review

**Files:** Read the three dirty adapter files above, `tools/matrix_live.py`, `tools/matrix_wire.py`, `tests/integration/test_live_hub.py`, and the correction-2 review/correction-3 report. Modify adapter files only if the fresh review finds a concrete defect.

- [x] Recheck branch, status and relevant diff; retain a small task-owned baseline inventory before touching inherited edits.
- [x] Have the correction reviewed against its complete G1/G1/G2 test and the actual Hub controller semantics. The current-source review records semantic/provenance PASS against the exact ten-file bundle and preserves sequence ordering, operation kinds, invocation anchors, all four advance hashes and pending admission for absent independent observations. Hub admission remains a separate unchecked gate.
- [x] Read `test_real_projection_admits_only_changed_advance_generation` and `test_advance_admission_is_pending_without_post_advance_observation` in `tests/test_matrix_live.py`. The dedicated new positive currently exercises `exact_current_values`; verify `polling_transport_zero_sse`'s later-poll admission uses the same corrected path, adding one focused regression only if its coverage is absent. Do not turn a missing root observation into fabricated success.
- [x] Resolve only new review findings. For a changed predicate, run `uv run --locked pytest -q --no-showlocals -o asyncio_mode=auto tests/test_matrix_live.py tests/integration/test_live_hub.py`; keep the two opt-in live skips explicitly separate.
- [ ] Have Hub reconcile the report pointer and shared ledger and admit/register the adapter only through its existing independent review process. A local pass cannot authorize registration or installed acceptance.

**Exit:** Independent correction review accepted is required for Task 4 adapter admission. A recorded blocker is a handoff state, not completion of this task. Product cleanup below can proceed independently of shared runner admission.

### 2. Close reauthentication cleanup defect

**Files:** Modify `custom_components/teslatlas_hub/config_flow.py`; extend `tests/test_config_flow.py`. Read `client.py` for wrapper ownership.

- [x] Extend the actual wrong-Hub reauth test with a close spy; assert zero claim requests and client closure. Include cancellation and success/reload ordering cases because the fix changes ownership, not merely an error string.
- [x] Use `test_reauth_changed_hub_never_receives_pairing_secret` for the real HTTP identity mismatch, and `test_pinned_client_uses_and_detaches_ha_managed_request_wrapper` in `tests/test_client.py` for the HA ownership boundary. Observe each newly created client separately; the shared `FixtureHubClient` fixture is reused across calls and a prior probe close must not accidentally satisfy the reauth close assertion.
- [x] Restructure reauth pairing/identity validation with unconditional `finally: await client.async_close()`. Perform `async_update_reload_and_abort` after cleanup; retain existing error mapping and stable entry identity. Remove redundant branch-specific closes rather than adding another abstraction.
- [x] Reproduce cancellation while `_async_probe` awaits discovery, while reconfigure owns a probed wrapper, and during first refresh in `__init__.py`. Require cancelled tasks to finish, wrappers to detach and no later dispatch. If a case already cleans up through HA, record that; otherwise extend the same small ownership fix. Do not swallow cancellation, catch unrelated exceptions as success or close HA's shared connector.
- [x] Run `uv run --locked pytest -q tests/test_config_flow.py tests/test_init.py`, then focused real-client tests if wrapper ownership changes. Confirm no unclosed-session warning and no shared connector close.

**Exit:** Reviewed M1 is closed with a meaningful failing-before/passing-after regression and successful same-entry reauthentication.

### 3a. Verify and finish user-facing runtime behavior

**Likely files:** under `custom_components/teslatlas_hub/`: `current_hub_client.py`, `coordinator.py`, `sensor.py`, `entity.py`, `models.py`, `__init__.py`, `config_flow.py`, `diagnostics.py`, `strings.json`, `translations/en.json`; existing corresponding tests and `tests/integration/test_current_hub_client.py`. Packaging declarations: `hacs.json`, `component/manifest.json`, `tests/test_package.py`. Edit only where a requirement fails.

- [x] Use Task 0's established runtime. Default the claimed HA baseline/minimum to `2026.8.3` and align `hacs.json` plus its package assertion unless existing accepted evidence establishes `2026.8.0`. Retain the required 2026.8.3 lane; test any additional claimed version separately. Do not upgrade dependencies just because a newer version exists.
- [x] Exercise existing tests for pairing, duplicate prevention, reconfigure identity, bearer expiry/revocation, null/zero semantics, vehicle additions/removals, stale/future observations, backoff and reload/unload. Add targeted tests only for missing acceptance behavior or a found bug.
- [x] Preserve asynchronous bounded transport and the current entity IDs. Ensure reconfiguration never sends saved credentials to a different Hub, and labels unsupported discovery/profile capabilities truthfully. Local proof covers the no-bearer replacement-Hub path, stable entity IDs, manual discovery and the `local_poll`/no-SSE contract.
- [x] Distinguish transport bounds from snapshot freshness: a response is limited to 1 MiB and 10 seconds (connect 5, socket read 8), but a multi-vehicle refresh includes sequential discovery/list calls and current batches. Do not advertise an atomic global snapshot or a guaranteed update within exactly 30 seconds under failures. Keep the existing four-read cap and no-overlap rule. Source constants and the focused current-client/coordinator tests establish these limits; installed scheduler timing remains a separate gate.
- [x] Verify the acceptance table below with targeted extensions to existing tests. Fix only a demonstrated mismatch; unused historical `HubEvent`/Hub-status model types do not justify an event feature or a cleanup refactor. The current local suite passes `115 passed, 2 skipped`; the two skips are opt-in live tests.
- [x] Run `uv run --locked pytest`, `uv run --locked ruff check .` and the product-owned `uv run --locked ruff format --check ...` once final product edits settle. Record skipped live tests as unexecuted. The whole-repository format check reports five inherited correction-3 files would be reformatted; those files were preserved and this is not relabeled as a product runtime failure.

**Exit:** Local product checks pass; real installed runtime remains the separate next gate.

| Behavior to verify | Existing test seam | Required observation or bounded addition |
| --- | --- | --- |
| Pairing/duplicate/reconfigure | `tests/test_config_flow.py` | Same Hub is rejected as a duplicate without an extra claim; changed Hub receives no saved bearer/secret; successful reconfigure preserves credentials, updates TLS pin/endpoint and reloads once |
| Endpoint errors | `tests/test_config_flow.py`, `tests/integration/test_current_hub_client.py` | Host field accepts hostname/IP rather than a pasted full URL; invalid port/pin/profile yields a repairable form. No sensitive remote exception text appears in UI/logs |
| Trust and redirects | `test_pin_is_checked_on_actual_normally_trusted_http_connection`; bounded-body tests | Normal CA + hostname validation remains required in addition to the pin; mismatched pin sends no claim/auth headers on that connection; redirects are not followed |
| Polling/error transitions | `tests/test_coordinator.py` and blocked-read tests in `tests/test_init.py` | 30-second default; backoff 30/60/120/300 capped at 300; success resets 30; 401/expiry produces one reauth flow and unavailable sensors rather than retrying forever as a transport error |
| Sensor field/units | `test_snapshot_maps_current_fields_preserving_zero_null_and_no_sse`, `tests/test_sensor.py` | Preserve the 13 current vehicle sensors below; numeric zero is not missing; no fixture-only Hub/data-quality sensor returns |
| Vehicle population | `tests/test_sensor.py`, `tests/test_coordinator.py` | Zero vehicles loads; first/new vehicle adds one 13-sensor set; removal retains registry entries as unavailable; reappearance restores those IDs without duplicates |
| Telemetry age | Future/missing-observation tests in `tests/integration/test_current_hub_client.py` | Age derives only from `observed_at_ms`; missing time is unknown; >300 seconds future skew is rejected; old observations remain old even while Hub connectivity succeeds |
| Lifecycle | `tests/test_init.py`, `tests/test_client.py`, `tests/test_config_flow.py` | Blocked reads cancel/drain; failed setup and cancelled flows release ownership; unload returning false does not falsely claim cleanup; successful reload does not overlap old/new client dispatch |
| Diagnostics | `tests/test_diagnostics.py` | Check loaded, disconnected and not-yet-successfully-loaded entry paths that HA actually exposes; no `snapshot.info` error from missing data; no endpoint/token/pin/pairing/vehicle/location leakage |

The current sensor keys and source mapping are: `state_of_charge ← battery_level (%)`; `charging_state ← charging_state`; `charging_power ← charger_power (kW)`; `charge_limit ← charge_limit_soc (%)`; `estimated_range ← est_battery_range_km (km)`; `odometer ← odometer (km)`; `activity_state ← state`; `inside_temperature ← inside_temp (°C)`; `outside_temperature ← outside_temp (°C)`; `access_state ← locked (true/false/null → locked/unlocked/unknown)`; `software_version ← version`; `software_update_state ← update_status`; `telemetry_age ← observed_at_ms (derived seconds)`. Retain the profile's field semantics; no new conversions or guessed values.

### 3b. Prove config-entry migration and source-update preservation

**Files:** `component/__init__.py`, `component/sensor.py`, `component/const.py` only if behavior needs a fix; `tests/test_migration.py`, `tests/test_sensor.py`, `tests/test_init.py` for meaningful regressions. Consume the final behavior from Tasks 2 and 3a.

- [x] Extend the existing minor-zero test beyond `async_migrate_entry`'s return value: run setup for a legitimate older stored entry and for the current fixture-shaped entry containing only Hub identity/opaque data. Verify required host/port/TLS/Hub/device-bearer fields before use. Preserve unknown data; missing credential uses a controlled repair path, and missing endpoint cannot be treated as successful pairing. Choose existing HA error/flow mechanisms without a second entry store.
- [x] Keep unknown future major versions rejected without rewriting them. Preserve forward-compatible minor data; do not bump schema version for a cleanup fix or equate product calendar version with entry schema version 1/minor 2. Follow the [HA config-entry migration contract](https://developers.home-assistant.io/docs/core/integration/config_flow/).
- [x] Prepopulate this entry's real entity registry with retained sensor IDs, user-renamed IDs/names and disabled flags. Exercise reload/reauth and source replacement; assert these survive. Verify retirement removes only this entry's exact old unsupported sensor meanings (`hub_collector_health`, `hub_fleet_cost`, `hub_backup_age`, `data_quality`) and cannot remove unrelated entries. Use real historical unique-ID shapes from the existing foundation source/fixtures before changing matching logic.
- [x] Check existing local tags/distribution records with Hub to determine whether a real prior HA artifact exists. This checkout has no tags, packaged distribution files or prior published HA artifact in its local history; source replacement remains a local development rehearsal, with no predecessor invented.
- [ ] With a privately backed-up HA config, replace only the custom component, restart HA and verify entry/registry continuity. For a real predecessor where available, also verify supported Hub/client ordering on Hub's retained predecessor/candidate targets. Unsupported profiles must fail clearly before credential use. Record untested mixed-version paths explicitly.
- [ ] Schedule that installed replacement when Task 4's real HA runtime is available; the local migration/registry regressions can run earlier. Do not make ordinary source cleanup wait for a nonexistent historical release.
- [x] Document rollback as restoring the matching backed-up configuration and prior component bytes while HA is stopped; do not promise that an older component can load an unknown newer major config schema. README now contains the bounded backup/rollback instruction; installed replacement evidence remains pending.

**Exit:** Saved-state and registry checks pass locally; the exact installed replacement is verified in Task 4 or the final Compose phase. No broad historical support claim follows from synthetic migration fixtures.

### 4. Run practical end-to-end installed acceptance

**Files:** Reuse `tools/test-live-hub`, `tools/matrix_live.py`, existing schemas and `tests/integration/test_live_hub.py`. Update `compatibility/hub.json` only after genuine admitted results. Hub supplies final fixture descriptors, CA, installed hosts and lifecycle controller; this task does not build another runner.

**Dependencies:** Accepted correction-3 adapter; final component bytes from Tasks 2–3b; admitted Linux HA runtime; three usable Hub targets. Only the Hub task operates the shared installed-host controller/registry. This task supplies its owned adapter, component and exact acceptance requirements and inspects the resulting evidence.

- [ ] Obtain Hub's accepted current pinned-runtime preflight and isolated test resources. Keep the pinned HA 2026.8.3 lane. Do not silently claim latest-HA support; each additional version needs its own run.
- [ ] Before staging, reconcile the current product member inventory with Task 2/3 edits. The accepted existing adapter requires 31 component members and a separate five-file execution harness. Preserve that layout where possible; changes to members/hashes require reviewed manifest updates, not disabling exact-inventory checks. No bytecode, symlink or cached-source substitution may satisfy installed identity.
- [ ] Supply normal trusted TLS: Hub's reachable public URL, certificate whose hostname matches that URL, trusted CA chain, leaf SHA-256 pin when supplied and a short-lived invitation. On a separate host, use the real reachable Hub address rather than localhost. Plaintext loopback has pairing/authentication disabled in current Hub and cannot satisfy this step; HA's optional `use_tls=false` test path does not imply supported plaintext pairing.
- [ ] Map invitation `endpoint` to Host (hostname/IP only), Port and Use TLS; `tlsPin` to TLS fingerprint; `pairingId` to Pairing ID; `secret` to Pairing secret; choose the HA device name. Use Hub's supported local `pair` command to create the invitation, with Hub's actual configuration/account. Never paste secrets into the plan, shell history, test assertions or receipts. The integration persists device credential/expiry/identity, not the invitation.
- [ ] Install the exact component into a clean Linux HA configuration using the existing acceptance lane. Execute its actual config-flow API, then complete one Settings → Devices & services → Add integration → Teslatlas Hub walkthrough in a normally running HA instance with a fresh invitation. Record API and UI results separately. Existing pytest `hass` fixtures alone cannot establish browser/UI installation. Confirm matching device and entity registries; preserve the same user-renamed IDs after reauth/reload.
- [ ] Observe known battery/temperature values from Hub; use Hub's authorized fixture advance to change them; wait for a real scheduled poll and assert new values. Verify retained zero/null semantics and zero SSE traffic.
- [ ] For the two-vehicle scenario, retain initial battery 0 and inside temperature 21.5 °C, then later 1 and 22.5 °C, plus 26 registered sensor entities if all current defaults are enabled. For the natural-schedule check, keep the 30-second interval unchanged and do not call `async_refresh`, `_schedule_refresh` or a time-travel helper after the fixture advance. Observe the next timer-driven request and new HA state with a bounded 90-second allowance, including network/dispatch time. Record a timeout as failure; the interval is measured separately from end-to-end request duration.
- [ ] Have the Hub controller perform isolated outage/restart and revoke actions. Observe unavailable/recovery, then reauth with a fresh invitation and the same entry/IDs. Test reload during a blocked read and unload through the cancelled timer deadline with no late dispatch.
- [ ] Keep existing manual-refresh outage/revocation and 50 ms unload-timer checks as focused tests. Add the normal-schedule observation as a small product acceptance check alongside them; do not redesign or widen the common 18-case wire contract just to claim different scheduler evidence. Confirm one automatic recovery after an outage and observe the reset to the 30-second cadence. Local clock-controlled tests can establish all backoff steps without waiting through repeated five-minute intervals on each platform.
- [ ] Run the existing 18-case HA adapter across all three Hub targets. Require real actor execution, G1/G1/G2 admission for advance, exact installed member identity, actual stopped result and child/transport cleanup. The synthetic composed launcher is supporting evidence only.
- [ ] Retain a concise redacted acceptance record with runtime/image version, source and component/profile fingerprints, target, exact outcomes and receipt pointers. Hub owns the shared matrix and registry. Populate this repository's compatibility metadata only with admitted evidence.

**Existing launcher distinction:** `./tools/test-live-hub` runs only `test_installed_config_entry_polls_and_unloads_real_hub`. It requires private absolute paths for `TESLATLAS_HA_LIVE_READY`, `INVITATION`, `SCENARIO`, `RECEIPT`, `INITIAL`, `CONTINUE`, `REAUTH_INVITATION`, `REAUTH_READY`, `REAUTH_REQUEST`, `REAUTH_CONTINUE` (each with the `TESLATLAS_HA_LIVE_` prefix), plus `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE` and `TESLATLAS_HA_SYSTEM_CA`; the two CA-bundle variables must match. The shell entrypoint sets `TESLATLAS_HA_LIVE=1`. Existing fixture files must meet private-file mode checks. Running the script without this handoff is expected to fail closed, not exercise HA.

The full matrix instead launches `tools/matrix_live.py` with exactly one runner-staged private SessionInput path and a live stdio broker attachment. It owns the selector `tests/integration/test_live_hub.py::test_installed_matrix_all_cases_against_real_hub`, `-o asyncio_mode=auto`, framework log isolation and final Ready/Ack/close exchange. Do not execute its test directly with invented `matrix_runtime` fixtures and call that installed acceptance. Hub must supply the SessionInput, local installed paths, authenticated descriptors, profile/CA inputs, bounded output paths and retained independent controller views. Source review does not give this task authority to fabricate them.

| Required case IDs from `tools/matrix-contract.json` | Acceptance ownership and purpose |
| --- | --- |
| `candidate_artifact_identity`, `installed_service_runtime`, `installed_home_assistant_runtime` | Component/actor identity plus actual HA and Hub runtime; `installed_service_runtime` requires Hub runner evidence, not the HA adapter's self-assertion |
| `discovery_identity_profile`, `unauthenticated_discovery` | Same Hub/profile/capabilities and unauthenticated identity request |
| `bad_invitation`, `expired_invitation`, `replayed_invitation`, `real_auth` | Reject invalid claims and use the real paired bearer |
| `credential_lifecycle_reauth`, `revocation`, `credential_loss_reauthentication` | Observe credential loss and complete same-entry repair; do not add a user-facing token-rotation feature |
| `unknown_vehicle`, `exact_current_values` | Correct absent vehicle handling and exact independently expected observations |
| `endpoint_restart`, `outage_recovery` | Stable Hub identity through real service lifecycle and recovery |
| `unsupported_operation_zero_requests`, `polling_transport_zero_sse` | No unsupported commands/drives/SSE traffic and complete polling/unload evidence |

These are 18 case IDs per Hub target, or 54 expected case decisions across HA's three target cells; they are not 54 pytest functions or the entire ecosystem matrix. All mandatory decisions must be admitted. Retain failed/pending/blocked counts per target; never replace the three-target gate with one successful platform or a green composed launcher.

**Current external waits:** Hub fixed-registry admission for the exact HA handoff; an accepted pinned-runtime preflight; reachable isolated Hub installations and trusted TLS names/CA; private invitation/revocation/advance coordination; and final three-target lifecycle admission. Correction-3 semantics, current HA provenance and the canonical profile rebind are already complete source gates. No dependency on TypeScript SDK, Swift SDK, Viewer or Edge is required for ordinary HA polling; those products matter only to Hub's wider cohort coordination. Protocol owns the frozen public contract and any future discovery expansion.

**Azure Debian v1 contract:** Azure is the final bootstrap and Container gate, not an early resource request. Only after all development/integration is complete and Hub has recorded the existing Mac plus VPS verification does the owner supply fresh disposable Azure Debian ARM64 and x86_64 VMs. Hub then coordinates both lanes under [`docs/azure-debian-v1-evidence-contract-2026-09-08.md`](../../azure-debian-v1-evidence-contract-2026-09-08.md). It binds the exact current handoff, source/component/profile identities and expected HA `2026.8.3`/Python `3.14.2` runtime; requires observed image digest and architecture; and specifies redacted installation, UI activation, function, restart, persistence, upgrade, uninstall and cleanup receipts. The Azure lanes supplement rather than replace the separately required macOS ARM64 target. This document neither provisions Azure nor authorizes a release.

| Dependency | Owner task ID | Missing input | Local work possible now | Unblock event |
| --- | --- | --- | --- | --- |
| HA fixed-registry admission | `01a07f89-45cb-7ea2-b96e-aa89de18fd14` (Hub) | Reviewed registry entry for handoff `5d496de0...`, source manifest `e74a9553...`, component `0a4ec2d...`, provenance `33cda376...` | M2 is complete; retain the packet and inspect any Hub acknowledgement | Hub ledger records the exact HA adapter/runtime entry as admitted |
| Pinned HA runtime | `01a07f89-45cb-7ea2-b96e-aa89de18fd14` (Hub) | Actual HA 2026.8.3/Python 3.14.2 actor paths and accepted preflight | Keep source/docs current; do not relabel Python 3.14.7 local tests as installed proof | Hub supplies a passing preflight and SessionInput/runtime inventory |
| Three usable Hub targets | `01a07f89-45cb-7ea2-b96e-aa89de18fd14` (Hub) | macOS ARM64, fresh Debian ARM64 and Debian AMD64 targets with reachable trusted TLS and lifecycle control | Review adapter inputs and preserve the quarantined guest boundary | All three targets are admitted and controller-ready |
| Private lifecycle inputs and UI | `01a07f89-45cb-7ea2-b96e-aa89de18fd14` (Hub) | Short-lived invitations, CA/pin, advance/revoke/restart controls and an isolated HA UI instance | No secret-bearing local substitute; prepare exact expected observations | Controller opens the HA lane with redacted evidence destinations |
| Supported Linux Docker Engine | Hub resource owner, coordinated by `01a07f89-45cb-7ea2-b96e-aa89de18fd14` | Supported Linux daemon, pinned image digest/architecture and reachable Hub | Static Compose/docs review only | Container start, pair, poll, recreate and clean-stop execution becomes available |

**Minimum acceptance record:** tested component product version and member fingerprint; HA/Python/aiohttp versions; container image/digest/architecture when applicable; Hub target/version/source/package fingerprint; profile identity/hash; 18-case outcomes per target; natural-schedule/UI/replacement results; redacted request-count/error evidence; stopped/unloaded/cleanup results; unresolved actions with owner. Reuse existing receipts and add only concise missing observations. Public docs contain a redacted summary and resolvable public links; private fixture paths/tokens must not be published as install instructions.

**Product acceptance boundary:** Tasks 0, 1, 2, 3a and the local portion of 3b are complete. Task 4, installed replacement and Linux Container runtime validation remain open. HA-owned source review, guidance, static Compose work and truthful documentation may proceed while owner resources are pending; only evidence-backed compatibility metadata and publication wait for the installed gates. An unrelated Swift/Viewer/Edge failure does not block HA's own accepted product cells, though Hub's whole-cohort status can remain pending.

## Requirement coverage and execution order

| User requirement | Plan closure point |
| --- | --- |
| Start from dirty unfinished implementation and current correction 3 | Evidence inventory, Task 0 and Task 1; inherited files preserved |
| Installable HA integration with real discovery/pairing/config flow | Tasks 2/3a/4; manual public discovery, real TLS invitation and separate UI verification |
| Bounded polling, sensors and current updates | Task 3a field/behavior table and Task 4 normal-timer observation |
| Unavailable/reconnect/reload/unload/revoked auth | Tasks 2/3a/4 with explicit ownership, registry and lifecycle assertions |
| Meaningful local tests separated from installed acceptance | Distinct runtime identity, two opt-in skips, 18-case/three-target gate and UI/scheduler receipts |
| Practical install and update preservation | Task 3b and final Docker phase; no invented predecessor distribution |
| No unsupported streaming or unnecessary framework | Global constraints and reuse of existing production/adapter seams |
| Final AGENTS coverage outside App | Final phase 1; product-only ownership with Hub coordinating all seven products |
| Simple official HA Docker solution at the end | Final phase 2 after the core gate; one service, persistent config and source mount |
| Current docs and source-only GitHub update last | Final phases 3 then 4; no tags/releases/CI/HACS/artifacts |

Remaining execution order is: reconcile the final HA-owned diff and handoff → obtain Hub registry/runtime admission → run the three-target and ordinary-user acceptance → validate the supported Linux Container path → bind admitted metadata and perform final review → commit and push the authorized source-only update. Finish useful HA-owned work before waiting, keep one writer per file, and never treat a phase checkpoint as completion of the full goal.

## Final phase 1 — Reconcile AGENTS guidance after the core product works

**Files:** Every `AGENTS.md` in this repository (currently only root `AGENTS.md`).

- [x] Inventory again and update all product-owned AGENTS files. The HA guidance now carries the 2026-09-08 Astra review date plus the dirty-checkout, source-only, evidence-boundary and no-automation rules. Keep practical instructions for authorized follow-through, user-over-skill priority, focused clarification, concise communication and proportionate verification. Allow bounded delegation only when useful; add no elaborate agent machinery or model-setting changes.
- [x] Retain repository-specific privacy/API rules; the HA guidance uses the supported polling contract and explicitly excludes SSE, event-stream, command, and private collector calls. Preserve explicit user authority and planning/execution boundaries.
- [x] Send the resulting coverage to Hub. Hub coordinates workspace files and checks all seven non-App products: Hub, Protocol, TypeScript SDK, Swift SDK, Viewer, Home Assistant and Edge. Each owner edits its own repository; no task edits App AGENTS files.

## Final phase 2 — Create and validate the simple Docker solution

**Create:** root `compose.yaml`, `docs/docker.md`. **Modify:** `.gitignore` for local configuration/secret exclusions. No custom image build is needed for a mounted Python custom component.

- [x] Static candidate created in `compose.yaml` with the official 2026.8.3 image, persistent `ha-config`, read-only component mount, host networking and a 60-second stop grace; `docs/docker.md` records the source-only operational path and its unverified runtime boundary. The image digest, architecture and Linux runtime remain unchecked.

- [x] Create one service using the official image `ghcr.io/home-assistant/home-assistant:2026.8.3` to match the baseline. The static configuration exists; resolving and recording the tested image digest/architecture remains part of Linux runtime execution. The candidate uses this minimal shape:

```yaml
services:
  homeassistant:
    image: ghcr.io/home-assistant/home-assistant:2026.8.3
    restart: unless-stopped
    stop_grace_period: 60s
    network_mode: host
    environment:
      TZ: Europe/London
    volumes:
      - ./ha-config:/config
      - ./custom_components/teslatlas_hub:/config/custom_components/teslatlas_hub:ro
```

- [ ] Validate on a supported Linux Docker Engine host with Compose. Shortest startup command from the repository root: `docker compose up -d`; validate configuration first with `docker compose config --quiet`. No Dockerfile, Kubernetes, Hub sidecar or extra service. Do not add privileged/device/D-Bus access for this network-only integration without an actual requirement.
- [ ] Before startup, ensure `ha-config/custom_components` exists and the source mount contains `manifest.json`, component modules, translations and the approved profile. Exclude `/ha-config/`, `/ha-private/` and local secret-bearing `.env` files in `.gitignore`; verify their contents cannot enter the final source diff. Leave unrelated `.DS_Store` files alone. Fail setup clearly for a missing source mount rather than allowing Docker to silently create an empty directory.
- [ ] Explain persistent `/config`, writable directory ownership, backup/restore of HA configuration, private stored device credentials, read-only source mount and restart after source updates. Host networking exposes HA on port 8123 and must reach the Hub endpoint; localhost refers to the Linux container host, not a remote Mac. Keep normal CA/hostname verification; document verified private-CA installation when needed, never disable TLS validation.
- [ ] Make the default example work with a Hub certificate trusted by the image. For a private CA, validate a persistent read-only CA bundle mount using the existing proven HA trust environment (`SSL_CERT_FILE` and `REQUESTS_CA_BUNDLE` set to the same bundle path) or the actual image's supported trust installation. Preserve normal public CA roots and hostname checking. Publish the exact verified optional configuration only after it survives container recreation; a pin alone is insufficient. An unresolved private-CA path is an explicit limitation, not permission for `ssl=False`.
- [ ] Use a clean config, install/pair through the UI, observe a changed sensor value, restart/recreate the container and verify config entry/IDs survive. Exercise outage, revoked authentication and clean shutdown using the already approved fixture. Record actual results and cleanup only owned runtime resources.
- [ ] Validate `docker compose stop` gives HA time to close its database; the 60-second grace is an upper limit, not a fixed delay. Check retained HA logs for an unfinished database session after restart. Confirm `docker compose up -d` recreates with changed image/mount/config settings, whereas `docker compose restart homeassistant` reloads changed component source with the existing container configuration.
- [ ] Document `docker compose pull homeassistant` followed by `docker compose up -d` for an intentionally selected/validated image update. Back up `/config` first and record the old image digest/component source. Do not use an unqualified moving `stable` tag as evidence of compatibility, promise arbitrary HA downgrades, or remove persistent configuration as part of normal shutdown.
- [x] Explain that Container is self-managed and lacks Supervisor-managed apps/add-ons; HA OS includes Supervisor and has a different installation/update path. Document manual custom-component copy into `config/custom_components` plus restart for an existing HA installation; HACS metadata is not publication. Do not claim HA OS or other versions tested from a Container result. README and `docs/docker.md` carry this source-only guidance; runtime acceptance remains pending.

Official installation reference, checked during planning: [Home Assistant Linux installation](https://www.home-assistant.io/installation/linux/). It requires Linux Docker Engine (currently 23.0.0+) and explicitly excludes Docker Desktop; do not label a Mac Docker Desktop run supported installation acceptance. It also recommends allowing 60 seconds for shutdown. Recheck requirements during execution. The reduced network-only Compose configuration above is a proposal to validate, not a claim that every Home Assistant hardware integration works without additional access.

## Final phase 3 — Reconcile all current repository documentation

**Modify:** `README.md`, `docs/architecture.md`, `docs/protocol-readiness.md`, `docs/product-versioning.md`, `docs/docker.md`, and `compatibility/hub.json` as evidence permits. Recheck `hacs.json`, `component/manifest.json`, `component/strings.json` and `component/translations/en.json` for public claims and links. **Create:** `docs/installation.md`, `docs/troubleshooting.md` only where this avoids overloading README. Inspect all other tracked docs, including older plans/specs.

- [x] Put the shortest supported installation/pairing path in README and link Docker, manual install, prerequisite versions, TLS, backup/update, reload and reauthentication instructions. The path remains a source candidate while installed acceptance is pending.
- [x] Specify manual install as copying only `custom_components/teslatlas_hub` into the existing HA `/config/custom_components/teslatlas_hub` location, including its profile/translations, then restarting HA and adding the integration through UI. Do not instruct users to run `uv sync`, install test dependencies or copy `tools/` into a production HA image. State that discovery means an explicit public endpoint probe; this is a custom component, not a Supervisor add-on or HACS listing.
- [x] Document actual public endpoints/capabilities, polling bounds, unsupported Zeroconf/SSE/commands, sensor absence semantics and diagnostic redaction. Reconcile the existing incorrect missing-profile-hash claim and released/candidate wording in the current README, architecture, protocol-readiness and versioning docs.
- [x] Document failures users can act on: wrong Hub, certificate/pin mismatch, invalid/replayed invitation, unavailable endpoint, expired/revoked credential, unsupported profile and container-to-Hub networking. Include development checks and exact acceptance coverage/limits; the live and installed boundary remains explicit.
- [x] Inventory all current docs and check links against real targets. The older foundation plan/specifications are visibly marked historical and point to current guidance; historical evidence is preserved rather than rewritten as current success. Hub updates shared ecosystem docs from this product's handoff.
- [x] Check relative links, anchors, component manifest documentation/issue URLs, code-owner/repository references and commands from a clean-source layout. The tracked Markdown link/anchor checks, manifest URL checks, locked contributor commands, and static Compose check pass. Compatibility remains in the Protocol-owned candidate schema; no unsupported admission or HACS/release claim was added.

## Final phase 4 — Review and source-only GitHub update

**WAITING FOR INSTALLED EVIDENCE after the READY local reconciliation milestone.** No Git writes have been performed. Source publication remains gated on final installed acceptance and review.

- [ ] Review the final product-specific diff, including inherited task-owned correction-3 changes. Confirm tests, Docker instructions and compatibility claims match final bytes; rerun affected checks only when later changes invalidate their evidence.
- [ ] Treat edits to runtime code, profile members, metadata or supported versions after Task 4 as potentially invalidating installed identity. Rebind and repeat the affected acceptance checks before final publication; prose-only documentation changes need link/command review, not another three-target runtime matrix. A reviewer must inspect the final diff and relevant receipts, not only this task's summary.
- [ ] Verify `main`, remote URL, current status and incoming/divergent history before commit/push. Preserve unrelated dirty files and stage explicit owned paths/hunks only, never blanket-stage `.DS_Store`, runtime config, secrets, caches or private receipts.
- [ ] Review staged source/docs and perform the final local commit and push to this product's verified GitHub repository after the installed acceptance gates are resolved. The active goal already authorizes this source-only publication. Unexpected remote divergence requires reconciliation that preserves unrelated work, never force-push.
- [ ] Verify the remote commit matches the intended source/docs update and report remaining limits. No CI, release/tag creation, HACS submission, binaries or artifact uploads. Publication of source does not manufacture installation or compatibility acceptance.

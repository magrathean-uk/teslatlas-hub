# Teslatlas Home Assistant Implementation Plan

> Active model: **gpt-5.6-luna / max**. Owner restart is in effect for the active ARM64/Mac scope; x86 remains deferred.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the read-only Home Assistant integration usable on the active Debian 13 ARM64 and Apple-silicon Mac products: keep the accepted ARM64 runtime working, complete the smallest reliability and packaging work needed for a working Mac consumer path, and defer x86/Intel/Azure and the full architecture matrix.

> **Active scope override (owner direction, 2026-09-09):** ARM64 and Apple-silicon Mac work is active. Reuse the existing `debian13-arm64` and `macos13-arm64` guests and any already available local Mac runtime; do not create a replacement VM or a native macOS Home Assistant daemon. Debian x86/x86_64/amd64, Intel Mac, Azure, and complete cross-architecture lifecycle rows are paused and deferred, not failed or passed. The retained x86 receipt below is historical evidence only and must not block the working ARM64/Mac milestone.

**Architecture:** Retain `custom_components/teslatlas_hub`, its asynchronous bounded public HTTP client, one coordinator refresh authority, stable vehicle entities, and redacted diagnostics. Home Assistant remains its own runtime: HA OS or an explicitly selected HA Container/VM. Hub owns bootstrap selection, minimal fixture/admission, central manifests, aggregate installer, and receipts; this root owns the component, staging payload, Container example, tests, and docs.

**Tech Stack:** Python `>=3.14.2`, Home Assistant `2026.8.3` baseline, `uv`, aiohttp, pytest-homeassistant-custom-component, Ruff, Home Assistant OS/Container, Docker Engine/Compose on Linux when selected.

**Spec:** `/Users/bolyki/dev/source/teslatlas-service/docs/development/PRODUCT_SPEC.md` and `/Users/bolyki/dev/source/teslatlas-service/docs/development/MASTER_PLAN.md`.

## Prepared development VMs

Use [VM_ACCESS.md](../../../docs/development/VM_ACCESS.md) for the shared login, SSH, transfer and lifecycle instructions, and [ENVIRONMENT.md](../../../docs/development/ENVIRONMENT.md) for storage policy. The primary guest is `debian13-arm64`: Debian 13.6 ARM64, 4 CPUs, 4 GiB RAM, 32 GiB disk; user `bolyki`, SSH alias `teslatlas-debian13-arm64`, endpoint `127.0.0.1:60022`. The later native-platform guest is `macos13-arm64`: macOS 13.7.4 ARM64, 4 CPUs, 4 GiB RAM, 50 GB disk; user `admin`, alias `teslatlas-macos13-arm64`; the wrapper resolves its changing NAT address.

From the workspace root, use `scripts/dev/vm.sh debian ssh uname -m` or `scripts/dev/vm.sh mac ssh sw_vers -productVersion`. Both use the private lab key. The SSH config is `~/dev/teslatlas-lab/access/ssh_config`; the Mac GUI password is in private `access/credentials.json`, outside Git. Keep B1/B2/R1 on the active ARM64 path and make the Mac route usable with an existing runtime. Inspect any local Mac/Container capability before selecting it; do not create a new VM for this plan. Retained baseline/quarantine disks and the paused x86 target are not current development targets.

## Global Constraints

- Product development and tests are authorized; follow [START_AUTHORIZATION.md](../../../docs/development/START_AUTHORIZATION.md) and the current task model/resource policy.
- Preserve this independent `main` checkout and all unrelated dirty paths; no branch, reset, clean, stash, commit, push, HACS submission, or release without later authorization.
- Active targets are Debian 13 ARM64 with the existing Hub bootstrap and Apple-silicon Mac using the existing Mac guest/local runtime. A Mac may consume the selected ARM64 HA runtime; HA is not a native macOS daemon.
- Pause Debian x86/x86_64/amd64, Intel Mac, Azure, new VM provisioning, and complete cross-architecture matrix work. Preserve their code, schemas, receipts, and historical evidence; they are deferred, not passed or failed, and do not block the active milestone.
- Keep Python `>=3.14.2`, HA `2026.8.3` lock baseline, and `hub-http-v1@1.0.0`. Align any minimum support claim to observed runtime evidence; the existing HACS minimum is not acceptance proof.
- Use public Hub discovery, invitation claim, credential rotation, vehicles, and current-state routes with bounded polling. No SSE, private storage/collector calls, Tesla credentials, commands, buttons, switches, or invented fields.
- Hub owns Hub files, minimal bootstrap/catalog/fixture changes, central manifests, wrappers, aggregate installer, and compatibility ledger. Request Hub changes through a handoff.
- Keep local, primary Debian, UI/scheduler, Container, replacement, passive parity, and Mac evidence separate. Shared guest/runtime changes require a concrete Hub-owned handoff; only one heavy build or VM installation may run at a time.

## Current state and purpose

HA `main` is `f650331a1af0cf33cec271bc7eefc0f1201ebd4e` with 47 dirty paths. The component implements endpoint probing, stable Hub identity checks, invitation claim, bearer rotation, 30-second polling, at most four current reads, stable entities, migrations, unload cleanup, and redacted diagnostics. The canonical profile digest is `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`.

Current source work records 123 passed/2 skipped tests, including real loopback timeout-to-connection-failure coverage, focused adapter/Ruff checks, source handoff, serialized overlapping-snapshot coverage, duplicate vehicle identity rejection, static Compose validation, and a disposable component-only staging/replacement/rollback smoke. Compatibility remains `candidate`. The selected Debian ARM64 HA Container now has redacted B2.1 UI pairing, scheduled advancement, bearer revocation/re-authentication and replay rejection, same-ID reconfigure, reload/unload, clean stop/start persistence, diagnostics-redaction receipts, and a scoped live Edge coexistence visibility/boundary receipt. A bounded r5 same-Hub flow and one fresh r9 replacement flow each paired a temporary second entry; r5 failed at the Hub collector before ledger mutation, while r9 failed at the Edge certificate-required boundary before Hub admission. The fresh r12 replacement paired a temporary HA entry on 18493 and passed a normal scheduler refresh, but its one authorized producer call failed after TLS at the Edge receiver application-identity boundary before admission; no retry, Hub ledger mutation, or HA synthetic value followed. The temporary 18493 entry and trust were removed, and the original 18483 entry was restored. The Edge actor-certificate correction then passed the independent generator and exact pinned-parser source review, including negative cases. The fresh r13 journey completed one receiver-ACKed VehicleName=synthetic-b2 submission through a closed 20943/20944 Edge and 18494 Hub fixture: HA observed the target display name only after a normal scheduler interval, with no new name sensor, numeric value, unit, or entity, and all disposable trust/entries were cleaned. Hub also supplied a bounded synthetic data-only backup/restore receipt with fail-closed truncated-copy rejection and cleanup; it is not TeslaMate or installed-HA acceptance. Historical sibling-product receipts remain preserved separately and are not HA parity or a dependency of this plan. The Hub D1 source audit initially lacked a reviewed aggregate `packaging/components.json` manifest; HA supplied `docs/development/d1-aggregate-manifest-handoff-2026-09-09.json` plus the exact 31-line payload manifest, and Hub verified the immutable source identity in `hub/packaging/components.json`. Hub then bound and accepted the redacted Debian ARM64 Container selection receipt (`2f7b2b…`) with the selector’s runtime limits intact. Hub’s focused aggregate verifier now passes 9/9 for the recorded optional companion bindings; their package/runtime/platform selectors remain explicitly limited, while Hub core and Fleet helpers are still unbound. Installer/UX integration, standalone packaging, and blocked macOS/x86 selectors remain explicit. Hub has prepared an H7 named-source read-only evidence plan, but its preserved source/backup, read-only PostgreSQL identity, selected car/version, and isolated comparison destination are not authorized or available. The full R1 fault/parity matrix, installer integration, native/cross-platform lifecycle, and final registry admission remain open.

Hub’s source-only HA installer guard now requires the frozen payload and selected-runtime receipt hashes in the companion catalog, binds them into the immutable install identity and durable target receipt, verifies the staged payload before relinking, and reuses the verified binding for repair. A disposable local-candidate smoke also exercised the existing transactional installer with the real HA source manifest: it staged only the component into a fresh temporary HA config, returned an installed receipt carrying both provenance hashes, and created no HA database or config entry. Hub also exposes a fail-closed source-only `d1-plan` selector and a bounded `d1-install` adapter for the accepted Debian ARM64 Container lane. The adapter validates `packaging/components.json`, the exact source/profile/provenance identities, and a caller-supplied byte-complete local source manifest before translating into the existing transactional recipe/target path. Independent verification passed for 80 companion tests, 9 aggregate-manifest checks, 11 Rust delegation tests, `cargo fmt --check`, a locked binary build, and a compiled Rust disposable install that preserved an unrelated HA storage sentinel. The adapter remains a local-unpublished source smoke with `runtime_acceptance: false`; aggregate production promotion, standalone packaging, live installer UX, and live HA mutation remain separate.

Owned paths are `custom_components/teslatlas_hub/**`, profile resources, `tests/**`, `tools/**`, `compose.yaml`, `pyproject.toml`, `uv.lock`, `hacs.json`, `README.md`, `docs/**`, and this plan. Hub owns fixture, runner, registry, wrapper, aggregate package, and ledger.

Current scope reconciliation (2026-09-12): owner restart is active for a usable Debian ARM64 stack and a usable Apple-silicon Mac HA/selected-consumer path using existing infrastructure. HA on Mac must consume an explicitly selected supported HA OS/Container/VM runtime, including the accepted ARM64 runtime where appropriate; no native macOS HA service or replacement VM is in scope. Viewer development, packaging, integration, acceptance, and dependencies are excluded from this current goal; their source and completed receipts remain historical. The paused x86/Intel/Azure and complete-matrix rows remain in the deferred backlog with their historical receipts intact. This owner-scoped paragraph supersedes older full-plan sequencing language below where it names x86 or broad cross-platform work as an active dependency.

Mac runtime preflight (2026-09-12): the Apple-silicon host has no active `18123` consumer forward, and the existing `debian13-arm64` Lima guest is `Stopped` with no `60022` SSH listener. No guest, Docker/Colima runtime, or Mac service was started or repaired by this task. The earlier forward-to-ARM64-HA reachability receipt remains historical; normal authenticated Mac HA/selected-consumer setup requires the existing ARM64 guest to be handed off by Hub and a currently valid HA development login/session.

Working-product sequencing: once a concrete Hub handoff and an already available selected Mac consumer/runtime exist, the ordinary Mac HA/selected-consumer trust, pairing, administration, restart, and data-preservation path may proceed. Named-source TeslaMate comparison, the full R1 fault/parity/passive matrix, and polished D1 aggregate installer/UX are final-acceptance gates; they are not prerequisites for ordinary ARM64/Mac usability. Viewer work is excluded from this current HA goal and must not be treated as a dependency.

Current-state reconciliation (2026-09-09 historical; superseded by the restart check below): a read-only audit of the primary Debian guest found the explicitly retained diagnostic `edge-r12-fresh-20260909` and `edge-r10-diagnostic-20260909` Compose projects running with `unless-stopped` policies and listeners on 20743/20744 and 20543/20544. The accepted HA/primary Hub lane remained healthy (`8123` and `18483` returned 200). Hub confirmed that the scoped cleanup targets are distinct: r12-replacement 20843/20844 and r13 20943/20944, both closed. No stop, delete, restart, credential access, or fixture mutation was performed.

Owner restart reconciliation (2026-09-12): execution resumed in the active ARM64/Mac scope, but the existing `debian13-arm64` guest is currently `Stopped`; its configured SSH endpoint `127.0.0.1:60022` refused connection, the prior Mac consumer forward `127.0.0.1:18123` is absent, and no Playwright session is retained. The single preserved native goal still exists; the goal service reports its prior `paused` state and exposes no resume mutation. No duplicate goal was created. Hub owns the next shared action: hand off the existing guest with an exact bounded start/stop action, fresh expiring inputs and cleanup owner before any runtime mutation. A current HA-owned development login/session is separately required for the signed-in Mac UI workflow. No x86 target was used.

Historical reconciliation (2026-09-09, deferred X1 x86 slice): a fresh QEMU Debian 13 x86_64 target supplied Docker Engine 26.1.5, Compose 2.26.1, and the official Home Assistant 2026.8.3 amd64/linux image. The selected Container paired through the primary Hub fixture and retained one config entry, two devices, and 26 entities across UI reload, revoked-bearer repair, Home Assistant restart, Container restart, malformed component-candidate rollback, clean stop/up, forced stop/up, UI unload/re-enable, and controlled official 2026.8.2 image replacement followed by 2026.8.3 rollback. This is preserved partial evidence only. Owner direction pauses x86 work and forbids further use of that target; it does not block the active ARM64/Mac path.

## Milestones

The first two local work packages belong to the master's B2 stage; Hub completes
the relevant bootstrap path first. Independent owned corrections continue under
the owner restart. Cross-product checks consume the other
product owner's receipts; this task does not implement or rerun sibling suites.


### B2.1 — Existing bootstrap: Debian 13 ARM64 Hub + selected HA runtime

**Purpose:** Get the first real selected Home Assistant path working in the same primary environment as Hub.

**Dependency:** Hub’s B1 bootstrap accepts the HA source/profile identity and provides a minimal current-Hub fixture, invitation, trusted TLS, and either an existing HA instance or an explicitly selected HA Container runtime. No full matrix is required now.

- [x] Recheck status/HEAD and record Python/uv/HA lock identities, component/profile hashes, and Debian ARM64 runtime identity.
- [x] Extend the existing Hub companion selection with `home-assistant` only when explicitly selected; stage only `custom_components/teslatlas_hub` into the provided `--ha-config` or selected Container mount. Preserve HA config entries, entity registry, database, and unrelated integrations.
- [x] Complete the real HA UI pairing flow, stable Hub identity check, short-lived invitation claim, and one current-state read against the primary fixture.
- [ ] Keep the smallest Hub correction limited to bootstrap selection, profile/admission identity, and the disposable fixture/session values needed for this path.

Run: `uv sync --locked --group dev`; `uv run --locked pytest tests/test_package.py tests/integration/test_current_hub_client.py`; `docker compose config --quiet` if Container is selected; Hub-coordinated HA UI smoke. Expected: selected files are staged only into the chosen HA runtime, pairing succeeds, no secret enters source/logs, and deselected HA does not start.

### B2.2 — Selected HA runtime working with SDK resources and Edge in the primary environment

**Purpose:** Complete the selected helper set on the active ARM64/Mac products without introducing a native HA daemon.

**Dependency:** B2.1 and the primary Hub bootstrap. Edge is selected and owned by its root; HA must consume only its public profile. A Mac may consume the selected ARM64 HA runtime; no new VM or native macOS HA service is required.

- [ ] Verify the Hub bootstrap records HA plus any selected Protocol/SDK/Edge resources as one immutable cohort; HA does not build or store unrelated SDK products.
- [x] Run HA against the same primary Hub while Edge is selected, proving current-state visibility and clear ownership of credentials between HA and Edge. The redacted coexistence/restart receipts cover this sub-check; immutable cohort admission and full Edge delivery remain separate.
- [x] For an existing HA instance, verify the component reload path. For HA Container, verify the read-only component mount, persistent private config, host/networking choice, and 60-second shutdown grace.
- [x] Record selected/unselected components, staging path, image/source identity, and operator action required to reload HA; no hidden service start is allowed.

Named checks: `uv run --locked pytest tests/test_init.py tests/test_config_flow.py tests/test_coordinator.py`, `uv run --locked ruff check .`, and the selected primary runtime smoke. Expected: HA and Edge can coexist without reading each other’s secrets or Hub storage.

### R1 — Polling, entity/data parity, and recovery reliability

**Purpose:** Prove the real HA behavior and compare supported data to the named TeslaMate reference.

**Dependency:** B2.1/B2.2 actual ARM64 runtime plus a concrete Hub handoff and selected existing Mac consumer path where applicable. The named-source comparison and full fault/parity/passive matrix are final R1 acceptance gates, not prerequisites for ordinary Mac usability. Do not extend this milestone into the paused x86/Intel/Azure matrix.

- [ ] Prove one non-overlapping 30-second refresh authority, bounded 30/60/120/300-second backoff, at most four current reads, zero preservation, missing/unknown/unavailable semantics, dynamic vehicle addition, and stable entity IDs.
- [ ] Exercise wrong Hub/certificate/pin, malformed/oversized/non-JSON response, invalid/replayed invitation, expired/revoked bearer, `401` reauth, same-ID endpoint reconfigure, current-read timeout, one vehicle failing while others update, reload, unload, restart, and clean stop.
- [ ] Compare vehicles/current state, charge/power/range/odometer/temperature/lock/software/activity, units, null/zero semantics, freshness, gaps, sleep/awake and derived age against the named TeslaMate reference. Record unsupported SSE, commands, cost, backup, collector, and quality fields.
- [ ] Keep diagnostics redacted and prove no endpoint, Hub ID, vehicle identity, coordinates, bearer, invitation, or raw response leaks.

Run: `uv run --locked pytest`, focused `tests/integration/test_current_hub_client.py`, the Hub-coordinated normal scheduler/UI receipt, and the HA receipt validator. Expected: outage and auth failure never publish live values as fresh; a successful reauth preserves intended entity/config continuity.

### D1 — Debian/HA staging, selectable macOS package option, and Compose profile

**Purpose:** Package the working ARM64 integration and selected Mac consumer path late, and describe supported HA runtime choices honestly.

**Dependency:** For ordinary ARM64/Mac usability, B2.1 plus a concrete Hub handoff and selected existing runtime are sufficient to stage the minimum supported path. R1 primary receipts, named-source comparison, and the Hub aggregate installer contract remain applicable final-acceptance gates for polished packaging/UX, not blockers for the ordinary Mac route. HA is still not a native macOS service; x86/Intel/Azure packaging remains deferred.

- [x] Define Debian bundle staging that copies/links only the component/profile/translations into an explicitly selected HA config or selected HA Container setup. Do not add a systemd unit that owns HA’s database.
- [x] Define a deselectable macOS package option that either stages into an explicitly selected existing HA config or configures an explicitly selected HA Container/VM runtime. The UI and docs must state that HA itself runs under HA OS or Container.
- [x] Keep `compose.yaml` official-image based, persistent for HA config, read-only for the component, explicit for network/TLS, and free of Tesla credentials in environment/build layers.
- [x] Request Hub changes only for selection UX, wrapper output, package manifest, and the bounded aggregate-to-existing-installer adapter; this root owns component/staging payload and docs. Production promotion, standalone packaging, and full installer UX remain open.

Run: `docker compose config --quiet`, `uv run --locked pytest tests/test_package.py tests/test_documentation.py`, staging copy/rollback smoke. Expected: no native macOS HA daemon, no config overwrite, no accidental registry loss, and Hub-only mode remains valid.

### X1 — ARM64/Mac working-product lifecycle and final delivery

**Purpose:** Make the Debian 13 ARM64 and Apple-silicon Mac products usable, then deliver source only. The x86/Intel/Azure and complete-matrix obligations remain deferred.

**Dependency:** For the ordinary Mac route, use B2.1, a concrete Hub handoff, and an existing selected ARM64/Mac runtime. D1, named parity input, and the remaining R1 receipts are applicable final-acceptance gates and may remain open while the working product is made usable. Do not create a fresh target for this milestone.

- [ ] Record the HA/Python/OS/architecture/image/Docker/Compose/component/profile identities on the active ARM64 runtime and any selected existing Mac consumer runtime before installation.
- [ ] Exercise the working Mac HA/selected-consumer administration and restart/data-preservation path, plus the selected ARM64 HA UI/scheduler/reauth/reconfigure/reload/unload/restart/replacement/rollback path while preserving config/database/entity registry. Keep any Mac HA use inside its explicitly selected supported runtime.
- [ ] Keep passive collection evidence separate and use only an authorized route; no Tesla commands or registration changes.
- [ ] Reconcile support/docs/compatibility to ARM64/Mac receipts. After explicit authorization, stage only HA source/docs/staging paths for clean-host delivery; no HACS submission, release, tag, CI, registry, or binary upload. Preserve deferred x86/Intel/Azure evidence without treating it as active acceptance.

## Shared gates

G0 is the planning/environment baseline; the active ARM64/Mac scope has no global
execution hold. G1 is minimal Hub + Protocol contract and
fixture admission for B1. G2 is actual HA runtime in Debian ARM64. G3 is the
selectable staging/package profile in D1. G4 is the active ARM64/Mac working-product lifecycle in X1; the broader cross-architecture matrix is deferred.
G5 is passive/parity/recovery evidence. G6 is clean-host review/source-only
delivery. The full 21-cell matrix follows the working primary path.

## End-to-end cases and waiting work

Cover wrong Hub ID, wrong CA/pin, endpoint loss, invalid/replayed invitation,
expired/revoked bearer, `401`, timeout, partial vehicle failure, overlapping
refresh, HA/Container restart, replacement, failed upgrade, rollback, unload,
clean stop, forced stop, and cleanup on the active ARM64 path. Ready: component,
client, tests, profile and staging docs. Current blockers: Hub-owned handoff to
start/reconcile the existing ARM64 guest with fresh expiring inputs, then a
currently valid HA development login/session for the signed-in Mac UI workflow.
The remaining ARM64 reliability gaps, passive reference, and final delivery
authorization remain open. x86/Intel/Azure and the complete matrix are
explicitly deferred and must not be used to block a working ARM64/Mac product.

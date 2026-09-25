# Teslatlas Hub canonical development plan

> **OWNER RESTART — 2026-09-12:** The owner explicitly said restart on all. The 2026-09-10 global pause is lifted for Debian ARM64 and Apple-silicon Mac work; x86 remains paused. Resume from preserved evidence with fresh expiring inputs and no stale-runtime reuse.

> **HUB-ONLY RESTART — 2026-09-18:** Resume Hub-owned work only. Companion tasks remain active under their original owners and are not spawned, woken, polled or edited from this lane. Root coordinator files remain untouched. This lane permits Hub-owned source review and bounded fixes only; no runtime or producer event is authorized.

> **OWNER SCOPE CHANGE — 2026-09-12:** Viewer development is excluded from every current Hub goal, milestone, package, integration, runtime, dependency, and acceptance requirement. Preserve Viewer source and completed evidence as historical records only. The effective current objective is usable Hub-only operation on Debian ARM64 and Apple-silicon macOS, followed by the supported Mac Swift consumer and selected HA/Edge roles. The existing goal-control objective still contains immutable Viewer wording; available goal controls cannot amend it, so this override is authoritative and no duplicate goal may be created.

> Active model: **gpt-5.6-sol / high**. The owner has started development; old G0/hold paragraphs below are historical.
>
> **2026-09-09 active-scope override:** [ACTIVE_SCOPE.md](../../../docs/development/ACTIVE_SCOPE.md) supersedes older strict R1-first and full-matrix ordering for current work. The 2026-09-12 owner override narrows this further to usable Hub-only operation on Debian 13 ARM64 and Apple-silicon macOS; Viewer is excluded. x86/x86_64/amd64, Intel Mac, Azure, and full cross-architecture matrix work are **PAUSED**, not failed or removed; they cannot block the active ARM64/Mac milestone.

**Owner:** Hub repository (`/Users/bolyki/dev/source/teslatlas-service/hub`)  
**Status:** `START_AUTHORIZED / ACTIVE_SCOPE_ARM64_MAC / VIEWER_EXCLUDED` — effective current work delivers usable Hub-only operation on Debian 13 ARM64 and Apple-silicon macOS, then the supported Mac Swift consumer and selected HA/Edge roles. The preserved native goal still reports paused and contains older Viewer wording; current owner authority overrides that clause, available controls cannot amend it, and no duplicate goal is created. Production and vehicle operations remain separately controlled.  
**Candidate:** `2026.36.2`  
**Date:** 2026-09-08

**Active checkpoint — 2026-09-12:** the supported Mac Swift public consumer and
a separate standalone Mac Hub candidate-process restart/retained-state run have
passed and cleaned. The first generic packed TypeScript browser route closed
without API acceptance at the macOS Chrome trust-isolation gate. Its sequential
negative-before-import/positive-after-import source correction passed
independent Hub review, but the first wholly fresh runtime attempt failed before
the pre-import control attached because its previously verified-live CDP
endpoint was unavailable. A later isolated lifecycle diagnostic proved that a
non-detached, directly supervised Chrome child remains alive through CDP attach
and closes cleanly. The one fresh r5 attempt using that topology launched and
attached the exact untrusted Chrome child, then failed before the SDK runner
because the supervisor could not return its delayed readiness response after
the client's request half-close. No Hub request, CA import, trusted browser or
SDK route ran. Both owners completed exact cleanup, the one-use credential was
removed, and no retry is authorized. The source-only supervisor correction now
uses an explicit half-open server response lifecycle; its browser-free
immediate, delayed and failure regression passed independent Hub review. A
future runtime still requires a separately reviewed wholly fresh control and
Hub tuple; the closed r5 tuple remains ineligible. One such r6 tuple then
reached the corrected untrusted Chrome attach, but the SDK runner could not
start because its operator-prepared evidence directory had file mode `0600`
instead of searchable directory mode `0700`. No Hub request or keychain action
ran; both owners cleaned exactly and r6 is closed without retry. A source-only
directory/file-mode correction plus a no-browser runner-output preflight is the
next TypeScript gate. Its first preflight draft was rejected because it could
truncate prior outputs and normalize stale state rather than fail closed. The
superseding v2 template now rejects root and nested symlinks, all named stale
runtime artifacts including `control.sock`, and second-invocation output
collisions before mutation; independent Node 26.7.0 syntax/regression, targeted
Biome and diff checks passed. Hub accepts that correction as source-only. One
wholly fresh r7 control/package candidate then passed independent hash, mode,
symlink, package, Node 26.7.0 syntax, IPC, Biome, diff and closed-port review.
Only its two empty owner-only runner output files exist; no supervisor, Chrome,
Hub, keychain, credential or SDK API runtime started. Hub accepts r7 as
preparation-only. Hub then published one new owner-only endpoint, identity,
certificate and already-claimed credential bound to the exact r7 tuple.
TypeScript independently admitted every binding and consumed the sole attempt,
but the supervisor exited after publishing serving state and before the first
control connection. The untrusted control therefore received `ECONNREFUSED`;
no Chrome, CDP attach, helper, SDK route, Hub API request, certificate import or
keychain mutation occurred. Both owners completed exact cleanup, all four ports
closed, the credential was removed, the Hub child exited zero and r7 is frozen
without retry. Hub then reproduced normal browser-free supervisor/control
lifecycle under Node 26.7.0 when one awaited parent owned and captured both
direct non-detached children. No source defect or specific external terminator
was reproduced, so the accepted correction is bounded to one-parent full
orchestration with persistent lifecycle evidence before any wholly fresh future
browser tuple. Edge receiver loader/readiness passed. The first source-bound
Edge/receiver/Hub cohort remains rejected and frozen after a zero-event pass.
A wholly fresh r2 cohort then machine-bound its certificate/profile and reached
the same real five-second zero-event baseline on new identities, ports, state
and credentials. Hub independently observed Edge delivery connected, pending
publications zero, ACK frontier null and zero observations, then stopped the
exact processes and guest with all ports closed. No r2 producer actor or
one-call guard was included in the reviewed preparation, so the reserved
transaction was not treated as event authority and no event was sent. Durable
forwarding remains a separate future gate. Edge's first r3 source-only actor
proposal had the required one-call/no-retry policy shape, but Hub rejected it
because its temporary helper sources were already absent and it supplied no
stable fresh guard implementation or focused tests. Edge's r4 package corrected
that provenance gap with stable producer and guard sources; their declared
hashes, pinned-source producer tests and six local no-network guard tests passed
independent review. Hub nevertheless rejected r4 for execution because the
guard can invoke the helper before discovering that its configured final-result
path already exists, leaving no final result after a consumed event attempt. A
source-only r5 correction now preflights all output and result-temporary paths
and rejects stale occupants and aliases before the clock, guard or spawn. Its
seven local no-network regressions passed independent review, so Hub accepts
the r5 source contract. Hub completed a wholly fresh lock-guarded staging,
offline producer test and darwin-arm64 helper build. The resulting Mach-O arm64
binary is bound to the pinned 160-file source manifest and accepted producer
overlay; the shared build lock released. Edge independently accepted that
exact source-to-binary receipt. Edge accepted Hub's complete inactive r6
proposal, but the coordinator required a superseding correction before use:
one datum must not imply one durable sequence; the ledger observer must have an
executable fail-closed method; the transaction and inactive binding must be
sealed before event review; and the receiver route must be explicit. Hub added
a same-owner URI-read-only observer whose focused three-sequence fixture tests
pass without changing the database or creating a sidecar. Pinned receiver
source registers `/` as its catch-all data handler and the helper accepts a root
WebSocket endpoint, so the corrected contract uses the canonical root route and
does not claim a named `/telemetry` handler. Edge accepted the complete r7
correction, but its one-use preparer selected a nonexistent Rust launcher after
claiming the host root and before the first build. r7 therefore closed with no
build, PKI, credential, binding, guest, listener, runtime or event. The
superseding r8 preparation reached source/profile staging, but Hub and Edge
rejected it before any guest or runtime action because its preparer proposed a
Hub installation ID before the supported fixture created the authoritative
store ID. Its one-use secrets were removed and r8 is closed and non-reusable.
Hub added a fixture-binding validator and r9 created the supported fixture first,
but its plain read-only SQLite open could not inspect the freshly closed
WAL-mode catalogue without coordination sidecars. r9 therefore closed before
binary staging, PKI, credentials, guest or runtime; its cursor key and diagnostic
sidecars were removed and its root is non-reusable. The validator now follows
Hub's established stopped-snapshot contract: WAL/SHM must be absent, the
catalogue is fingerprinted before and after, SQLite opens immutable/read-only
and query-only, and a pending WAL fails closed. A regression using the accepted
real interop fixture passes without changing any fixture-tree byte or creating
a sidecar. Edge accepted source-only r10, Hub ran its one-use preparation, and
both owners accepted the exact machine-written profile, stopped store and
inactive binding. The reviewed tree transferred to the macOS 13 ARM64 guest
with matching bytes; two umask-tightened pack modes were restored to their
reviewed values before process start and the complete 44-entry comparison
passed. The first receiver invocation then exited before opening a listener
because its required bearer-file environment was omitted. Before retry, Hub
proved the exact receiver binary has an immutable dispatcher endpoint on
`127.0.0.1:8080`, while r10 configured Edge admission on `127.0.0.1:8086`.
That mismatch made the cohort incapable of forwarding. Hub therefore removed
all one-use inputs, verified every reserved port closed and actor output absent,
stopped the VM, and closed r10 as non-reusable without a baseline or event. A
fresh successor must bind Edge admission to port 8080 and explicitly launch the
receiver with `TESLATLAS_FLEET_TELEMETRY_BEARER_FILE`; it still requires fresh
Edge review and both-owner concrete-profile acceptance. Submit-once remains a
later explicit coordinator decision. Hub has prepared that source-only r11
successor: proposal SHA256 `5f6969ab`, preparation spec SHA256 `d800b93a`, and
one-use preparer SHA256 `d3f2ffa5`. It reserves a fresh root, tuple and ports,
asserts the exact receiver's compiled 8080 dispatcher before root creation,
configures Edge on the same port, and verifies all three exact binary interfaces,
rendered configs, argv, working directory, binds, bearer environment/path, TLS
paths, collector URL and start order before root creation. It seals that same
launch contract into the machine-written profile. Edge review is pending; no
r11 root or private input exists.

On 2026-09-18 Hub revalidated the frozen r11 inputs without running the one-use
preparer: proposal SHA256 `5f6969ab`, preparation specification SHA256
`d800b93a`, preparer SHA256 `d3f2ffa5`, fixture-binding validator SHA256
`42a6d635`, and validator test SHA256 `c3697062` remain exact. The source
preflight still binds 195 Hub manifest inputs, 15 Edge manifest inputs, 14 exact
anchors/artifacts, and the complete receiver/Edge/Hub launch contract. The r11
root, public profile and preparation lock remain absent; ports 8080, 21850,
21458, 21459 and 18535 remain closed. Exact handoff:
`docs/development/edge-macos-one-event-r11-restart-revalidation-2026-09-18.json`
SHA256 `d64db856`. Edge independent review remains the next gate; no preparation,
runtime or event occurred.

Edge then returned final source-gate PASS SHA256 `4d8b4449`, binding the exact
Hub restart receipt and frozen r11 inputs. Hub revalidated root/profile/lock
absence, the current `teslatls13arm64` lease at `192.168.64.10`, and closed
ports 8080, 21850, 21458, 21459 and 18535, then invoked preparer SHA256
`d3f2ffa5` exactly once. It completed locally without rebuild, guest action,
listener, runtime, browser, producer or event. The machine-written public
profile SHA256 is `4b896644`; source/staging receipt `509b2e61`; immutable
fixture-store validation `6fe2ed24`; inactive binding `9f3dff1c`; preparation
summary `4f068ba3`. Seven binding relationships passed immutable/query-only
validation with unchanged database fingerprint and no WAL/SHM. Every private
input is mode 0600, single-link and preserved; raw values remain unreported.
All five ports remain closed and no staged executable is open by a process.
Exact Hub handoff:
`docs/development/edge-macos-one-event-r11-concrete-preparation-2026-09-18.json`
SHA256 `96a13421`. Edge independent concrete-profile review is now required before
any guest transfer or runtime. The coordinator must review fresh concrete
readiness before any producer/event authority.

Hub also independently reviewed TypeScript's one-parent browser orchestration
source package. Its declared four browser-free cases still pass under pinned
Node 26.7.0, but two blocking fail-closed defects are reproducible: stale
orchestration-capture rejection happens only after preflight creates runner
outputs, and a cleanup sequence stops after an earlier cleanup closes the
supervisor while the overall run can still report passed. The source package is
rejected pending focused corrections and regressions. Exact handoff:
`docs/development/active-macos-typescript-browser-orchestration-source-review-2026-09-18-r1.json`
SHA256 `2e9653c8`. No TypeScript file, companion task, browser, Hub process, keychain,
credential, SDK API or live fixture was mutated or started.

This is the one active Hub development plan. The workspace product boundary is
defined by [`/Users/bolyki/dev/source/teslatlas-service/docs/development/PRODUCT_SPEC.md`](../../../docs/development/PRODUCT_SPEC.md).
The root coordinator owns the workspace master plan and redirects old plans;
this file owns the Hub implementation, packaging, integration, and acceptance
work. Existing evidence, receipts, and wire specifications remain inputs and
must be rechecked against current source before being used as acceptance.

The plan preserves the existing independent `main` checkout and all unrelated
dirty work. It does not authorize branch, reset, clean, commit, push, release,
tag, image upload, deployment, vehicle command, production-service restart, or
public publication.

## Prepared development VMs

Use [VM_ACCESS.md](../../../docs/development/VM_ACCESS.md) for the shared login, SSH, transfer and lifecycle instructions, and [ENVIRONMENT.md](../../../docs/development/ENVIRONMENT.md) for storage policy. The primary guest is `debian13-arm64`: Debian 13.6 ARM64, 4 CPUs, 4 GiB RAM, 32 GiB disk; user `bolyki`, SSH alias `teslatlas-debian13-arm64`, endpoint `127.0.0.1:60022`. The later native-platform guest is `macos13-arm64`: macOS 13.7.4 ARM64, 4 CPUs, 4 GiB RAM, 50 GB disk; user `admin`, alias `teslatlas-macos13-arm64`; the wrapper resolves its changing NAT address.

From the workspace root, use `scripts/dev/vm.sh debian ssh uname -m` or `scripts/dev/vm.sh mac ssh sw_vers -productVersion`. Both use the private lab key. The SSH config is `~/dev/teslatlas-lab/access/ssh_config`; the Mac GUI password is in private `access/credentials.json`, outside Git. The active guests are only Debian ARM64 and Apple-silicon macOS. Keep B1/B2/R1 evidence on the primary path and polished native/cross-platform acceptance late. There is no active Colima VM; provision Docker in the primary Debian guest only when an owned active-target development step needs it. Retained baseline/quarantine disks, including the unadmitted x86 guest and its artifacts, are preserved but not current development targets.

## Product boundary and definition of done

Hub is the primary product. It must install, start, onboard, collect, retain,
recover, upgrade, and uninstall with no Teslatlas companion present. Hub owns
the common packaging, component selection, integration contracts, and shared
installed acceptance runner.

The optional products are treated according to their actual roles:

| Component | Hub treatment | Runtime rule |
| --- | --- | --- |
| Hub | Required core service, SQLite store, API, administration, native macOS app, bootstrap and distribution owner | Runs as the selected service on macOS, Debian, or Docker |
| Protocol | Versioned contract/data package and conformance tools | Developer resource; never an idle daemon |
| TypeScript SDK | Typed Node/browser library | Developer resource; never an idle daemon |
| Swift SDK | Swift package for supported clients | Developer resource; never an idle daemon |
| Home Assistant | Integration loaded into a selected HA instance | Install into the selected supported HA runtime; never advertise a native HA daemon |
| Edge | Optional receiver/forwarding service | Explicit service with its own credentials, encrypted spool, and deployment target |

“Full” distributions include the selected optional services and integrations,
but Protocol and SDK packages remain developer resources. Home Assistant is
either installed into an existing selected HA instance or run through an
explicitly selected supported HA Container profile. Hub must never silently
install a replacement HA instance, take ownership of an existing collector, or
select Tesla telemetry credentials.

The historical full-product completion ledger means all required delivery routes
have evidence at the appropriate layer:

- Hub-only bootstrap and native core operation work without optional products.
- Bootstrap offers interactive selection and deterministic unattended selection.
- macOS offers visible, deselectable core/helper/component choices with correct
  LaunchAgent ownership.
- Debian offers core, optional component packages, and an explicit bundle.
- Docker offers a core composition and explicit optional/full profiles with
  persistent data, readiness, restart, upgrade, and recovery behavior.
- Every installed component exercises the actual current Hub contract and its
  declared runtime, with no fixture or static check substituted for runtime
  proof.
- TeslaMate parity, passive collection, recovery, upgrade, and uninstall data
  preservation are reported as separate evidence.
- The final owner-provided Azure clean-host validation is complete only after
  local macOS, authorized VPS, and reviewed local distribution candidates are
  complete. It is deferred under the active scope and does not authorize
  publication afterward.

The current operational completion target is deliberately smaller: a normal
person can start, administer, pair, restart, and retain data with Hub alone on
the active Debian ARM64 and Apple-silicon Mac paths. A missing
named TeslaMate source, x86/amd64/Intel Mac work, Azure, or the historical full
matrix is an explicit acceptance gap, never a reason to stall that active
product work.

The execution order is deliberately narrower than the final completion scope:

1. **M1 / B1 — simple working bootstrap:** on the primary Debian 13 ARM64
   development VM and the supported Apple-silicon Mac, bring up Hub through the
   existing ordinary bootstrap path. Use the real Hub binary, SQLite store,
   TLS/config paths and service manager. Prove Hub-only setup, administration,
   pairing/API access, restart and retained data without any optional product.
2. **M1b / B2 — selected helpers:** add Home Assistant in an existing or
   explicitly selected test HA Container runtime, then Edge using its actual
   receiver, encrypted spool, Hub pull/commit/ack, retry, deduplication, and
   restart paths. A helper that is not selected remains outside the running
   core slice.
3. **Active Mac route:** use the existing Apple-silicon Mac runtime to make
   Hub-only setup, launch/service, administration, pairing/API access, restart
   and retained-data behavior usable, then provide a separate fresh handoff to
   the supported Swift public consumer.
4. **R1 — collection, recovery, parity, and reliability:** keep named-source
   TeslaMate admission, passive observation, migration, and soak as explicit
   acceptance work. It can proceed in parallel when its owner-supplied inputs
   exist; their absence must not stall independent ARM64/Mac usability work.
5. **D1/X1 active subset:** complete only the minimum ARM64/Mac bootstrap,
   launch/service, and lifecycle work needed for usable products. x86/x86_64/
   amd64, Intel Mac, broad Docker profiles, Azure, and the final matrix remain
   paused pending a new owner direction.

H2 and the active ARM64/Mac subsets of H5/H6 are the immediate implementation
path. H3/H7 evidence is consumed where it improves a working product, but the
old strict stage order must not delay a ready active-target correction. The
historical static, matrix, and final-acceptance gates remain fail-closed.

## Current baseline and evidence rules

The source declares Rust 1.98, edition 2024, AGPL-3.0-only, and version
2026.36.2 in `Cargo.toml`. The native app declares macOS 13.0 and Apple arm64
in `macos/TeslatlasHubApp/project.yml`. The declared Linux floor is Debian 13
on ARM64 and x86_64. Go 1.27.0 is pinned for the macOS helper chain. x86_64 is
retained as a declared support floor but is paused for all current build,
runtime, package, image, VM, and acceptance work.

The current ledger in `docs/compatibility/execution-state.json` records the
development state, including `0/21` installed cells, `0/444` required case
slots, and `0/10` cohorts. Those pending counters must remain visible. Existing
receipts and old counters are historical evidence until an exact current
candidate completes the authoritative runner.

Evidence is stored in four layers:

1. **Source/static:** source review, schema/manifest validation, unit tests,
   formatting, lint, repository layout, provenance, package-shape checks, and
   `docker compose config`.
2. **Candidate artifact:** exact source, toolchain, package/image, profile,
   component manifest, legal bundle, and digest identities.
3. **Installed runtime:** fresh host installation, service state, actual binary
   and architecture, endpoint behavior, restart/persistence, cleanup, child
   zero, upgrade, uninstall, and data preservation.
4. **Live/passive data:** real authorized TeslaMate comparison, durable passive
   observations, outage/reconnect behavior, recovery, and soak.

No layer inherits a pass from an earlier layer. Compilation does not establish
installation. A health endpoint does not establish collection. A synthetic
fixture does not establish passive live data. Static Compose checks do not
establish Docker runtime support.

## Component installation contract

Hub will own a canonical manifest at the proposed path
`packaging/components.json`. It is a product input, not a second source
resolver. The manifest is generated or reviewed from the exact selected
candidate and is bound into each package/bootstrap/Docker receipt.

For D1, first extend the existing catalogue/recipe data. Introduce
`packaging/components.json` only if a shared installer view cannot be derived
cleanly from that data; it must not become a second catalogue or resolver.
Required component information is:

| Field | Meaning and source |
| --- | --- |
| `component_id`, `kind` | Existing component selector and `service`, `integration`, or `developer-resource` role |
| `product_version`, `profile` | Current product version and separate canonical wire/profile identity |
| `artifacts` | Exact source manifest and produced package/image hashes from the existing build receipts |
| `runtime_requires`, `platforms` | Verified OS/architecture/runtime requirements, preserving declared floors |
| `service` | Actual existing or newly tested service-manager label, startup scope and listeners; absent for libraries |
| `config`, `data` | Verified paths/ownership/modes and explicit preservation policy |
| `health` | Real component health/readiness probe and its interpretation |

Derive values from existing packaging and measured candidate receipts. Do not
copy illustrative service labels or invent output hashes. Protocol/SDK entries
have no idle process. HA names the selected external runtime/configuration;
Edge names its selected service payload. Viewer is excluded from current
component selection.

Selectors must resolve to a complete, deterministic component set before any
mutation. The selector result records component IDs, profile, source and
artifact digests, platform, runtime requirements, service ownership, config and
data paths, and the exact failure or omission reason for every unselected
component. A component may not be silently dropped because its build or
runtime is unavailable.

The lifecycle contract is:

- **Admission:** reject missing fields, unsupported platform/runtime, profile
  mismatch, digest mismatch, wrong owner/mode, conflicting service ownership,
  missing selected HA runtime, or direct-Fleet/Edge mutual-exclusion errors.
- **Install:** stage outputs privately, verify every digest and legal input,
  create config/data paths with the declared ownership, activate only after all
  selected components pass, then record a receipt.
- **Failure:** leave the previously working set active, remove only the staged
  candidate, release locks, and emit a redacted actionable error. Interrupted
  bootstrap and package installation must be resumable or safely retryable.
- **Upgrade:** stop only the affected services in dependency order, create a
  verified backup, migrate storage/configuration transactionally, activate the
  candidate, and verify readiness plus persistence. A failed migration must
  restore the previous working set or provide the documented compatible restore
  path.
- **Uninstall:** stop and disable only Hub-owned services, remove package code
  and logs, preserve data/configuration/identity/TLS/pairing by default, and
  require a separate explicit purge for destructive deletion.
- **Runtime:** service health and readiness are distinct. A process may be
  healthy while unconfigured; readiness may be unavailable until onboarding.
  The receipt must include actual process identity, architecture, runtime,
  service state, endpoint closure, and child-process zero at final stop.

## H1 — G0 planning and environment hold

**Status:** `ACTIVE_ARM64_BOOTSTRAP_EVIDENCED_MAC_ROUTE_NEXT`.

G0 is a shared planning and environment gate. The original planning hold ended on 2026-09-08. This section records the historical preparation gate; implementation now proceeds under START_AUTHORIZATION.md.

Hub work in G0 is limited to read-only inventory, local environment preparation
that does not alter product state, and preservation of evidence. The current
dirty `main` checkout, nested sibling checkouts, 26 GiB `hub/target` data, old
VM evidence, and retained receipts remain untouched unless a separately
authorized verified-disposable cleanup is identified.

Inspect with:

```sh
git -C hub status --short
git -C hub rev-parse HEAD
python3 -m json.tool hub/docs/compatibility/execution-state.json >/dev/null
python3 -m json.tool hub/docs/compatibility/matrix.json >/dev/null
find hub/tools/interop hub/packaging hub/scripts -maxdepth 3 -type f -print
```

G0 exit assertions:

- the active source checkout, branch, HEAD, dirty paths, and owners are
  recorded;
- `PRODUCT_SPEC.md`, this plan, the compatibility ledger, current profile, and
  historical plans are classified correctly;
- no current pending counter or old receipt is overwritten;
- target availability and toolchain identity are recorded before any build;
- all new implementation goals are `WAITING` until the owner lifts the hold.

## H2 — B1 / G1 simple working Hub-only bootstrap and admission

**Status:** `WAITING`.

B1 binds the current Hub HTTP profile and creates the smallest real Hub fixture
for the primary Debian 13 ARM64 bootstrap and active Apple-silicon Mac route.
Historical Viewer evidence is preserved but is not a current dependency or
acceptance criterion. Each route must run the actual Hub binary
against a private config and SQLite data directory; it may not be a Python mock,
a fake HTTP server, or a compile-only substitute.

The B1 path is intentionally simple: use the existing `bootstrap`, setup,
service, and status/doctor paths with the minimum configuration needed to
establish a usable local Hub service. Do not introduce a
new general-purpose orchestration framework, preinstall all seven clients, or
make B1 depend on `21/21`, `444/444`, or `10/10`. Those are final acceptance
gates after the primary integrated behavior is working.

### B1 operator command to deliver

The existing H2 source-bootstrap interface is the following thin command, run
from the Hub checkout inside `debian13-arm64`. Its prior ARM64 local-candidate
evidence is retained; active work must reuse it or record the exact Mac
equivalent rather than create a parallel installer.

```sh
scripts/bootstrap-dev.sh --prefix "$HOME/teslatlas-dev" --components none
```

Hub core is always included. Repeating the same invocation must preserve
identity and data. The wrapper reuses the existing Hub lifecycle commands. It
locates the required source in this workspace,
validates their exact inputs, provisions the bounded declared prerequisites,
and prints the actual service address and stop/restart instructions.
Pairing secrets remain in private files or the normal pairing flow. Do not
require the operator to assemble a matrix descriptor or publish a catalogue.

If an existing entry point can already provide this complete behavior, use it
and record its exact equivalent here instead of retaining two entry points.
The H2 handoff must contain the final literal invocation, resolved candidate
identities, required private input paths, and its observed fresh/rerun/restart
results. A companion `dry-run` alone does not satisfy the bootstrap exit.

Existing Hub seams to preserve and inspect:

- `src/api/protocol.rs`
- `src/api/server.rs`
- `src/api/server/browser_cors.rs`
- `src/runtime/config.rs`
- `src/storage/db.rs` and `src/storage/data_recovery.rs`
- `tests/interop_fixture.rs`
- `tools/interop/installed_hosts/{contract.py,prepare.py,session.py,transport.py}`
- `tools/interop/matrix_runner/{adapter_wire.py,installed.py,receipt_validation.py}`
- `docs/compatibility/{matrix.json,receipt.schema.json}`

Reuse the existing interop seed and process helpers. Add only a thin missing
helper needed to exercise the ordinary B1 path; extending the formal installed
runner is X1 work unless current code demonstrably requires it. Candidate
Hub-owned locations for those focused additions are:

- `tests/interop_fixture.rs`: seed a minimal real Hub data directory and
  current-profile fixture with no vehicle credentials and no external broker;
- `tools/interop/installed_hosts/hub_fixture.py`: launch the built
  `teslatlas-hub` process, write a private config, capture PID/architecture/
  runtime/service inventory, probe `/healthz`, `/readyz`, discovery, pairing,
  vehicle/history and sync routes, and close the process through the same
  runner-owned stop path;
- `tools/interop/installed_hosts/test_hub_fixture.py`: assert actual process
  identity, private data permissions, bounded request behavior, endpoint
  closure, no leftover children, and unchanged source-of-truth receipts.

For B1, record the exact candidate source, current profile, Hub binary and seed
inputs actually used. Reuse existing
local-candidate validation; keep formal installed-adapter admission and its full
manifest/validator/raw-schema bindings for X1. Unused adapters remain
fail-closed. Protocol supplies versioned contracts and conformance tools; it is
not installed as a daemon.

Planned source/static gate:

```sh
rustup run 1.98.0 cargo fmt --all -- --check
rustup run 1.98.0 cargo test --locked --test interop_fixture
python3 tools/interop/test_run.py
PYTHONPATH=tools/interop python3 -m unittest matrix_runner.test_installed_registry
sh scripts/test-repository-layout.sh
python3 scripts/verify-provenance.py
```

Run only affected gates for the first bootstrap correction and the required
repository checks before milestone closure. Complete final matrix-adapter suites
in X1 rather than rebuilding that framework before B1. The exact commands may
be narrowed to affected suites after implementation,
but the B1 assertions remain mandatory: profile and validator hashes agree;
ordinary request and claim body limits are enforced; malformed inputs return
the bounded declared form; the real minimal Hub process is admitted; and
runner cleanup proves no child or listener remains. B1 produces a redacted
Hub-only working-bootstrap receipt; it does not increment the final installed
matrix.

## H3 — B2 / G1-G2 selected helpers, runtime registry, adapters, and Swift correction gates

**Status:** `ACTIVE_TARGET_DEPENDENT`.

Only a selected helper action that closes a concrete Debian ARM64 or
Apple-silicon Mac usability gap is current work. Keep the fixed registry and
all validator boundaries fail-closed; do not start an optional service merely to
fill a historical matrix cell. Any x86/x86_64/amd64 or Intel-Mac helper row is
paused and cannot block Hub-only operation on the active targets.

After B1 is working, Hub owns the fixed installed registry and adapter
composition for selected helpers. Every admitted
entry implements the existing `FixedInstalledAdapter` boundary with source-
bound `build_session_input`, `launch_adapter`, `admit`, `build_supplement`,
`execution_logs`, and `runtime_inventory` callables.

Hub-owned seams:

- `tools/interop/matrix_runner/installed_registry.py`
- `tools/interop/matrix_runner/installed.py`
- `tools/interop/run.py`
- `tools/interop/matrix_runner/adapter_wire.py`
- `tools/interop/matrix_runner/typescript_installed.py`
- `tools/interop/matrix_runner/protocol_installed.py`
- `tools/interop/matrix_runner/swift_installed.py`
- `tools/interop/matrix_runner/home_assistant_installed.py`
- `tools/interop/matrix_runner/edge_installed.py`
- `tools/interop/installed_hosts/edge_fixture.py`
- `tools/interop/installed_hosts/edge_runtime.py`
- `tools/interop/matrix_runner/test_*_installed.py`
- `tools/interop/matrix_runner/test_installed_registry.py`
- `tools/interop/test_run.py`

The current TypeScript Node/browser rows are source-fixed but remain installed
runtime pending. Protocol is source-registered pending independent final
binding. Swift, Home Assistant, and Edge remain fail-closed until their
actual runtime, launch inventory, profile, path-map, browser-origin, or root
fixture inputs are bound and reviewed.

B2 is limited to the selected helper path on the primary Debian 13 ARM64 VM.
Add Home Assistant to an existing or explicitly selected supported HA Container
runtime, and then add Edge only
through its actual receiver/spool/Hub path. The HA runtime is selected by the
operator and retains its configuration and registry data; Hub does not start a
native HA daemon. Edge persists encrypted spool, keys, sequence state, and
acknowledgement state separately from the Hub image/data volume. Protocol,
TypeScript, and Swift remain libraries/developer resources even when their
artifacts are selected for the helper build.

The Swift correction gate is explicit. `swift_installed.py` and
`swift_entrypoint.py` must admit an actual Swift package/runtime and real HTTP
transport behavior on each claimed platform. The source adapter must not copy
job metadata into `runtime_actual`, accept synthetic stream output, or infer
service ownership from a job-supplied executable. The sibling Swift package
(`teslatlas-sdk-swift/Package.swift`, `Sources/`, and `Tests/`) is a developer
resource; its current source/profile/validator handoff is reviewed by Hub but
it does not become a resident daemon.

Home Assistant admission must install the integration into a selected supported
HA runtime, preserve that runtime's configuration and registry data, and run
actual config-flow, authentication, update, unavailable, reauthentication, and
unload cases. A test HA container is a disposable selected runtime; it is not a
claim that Hub ships a native Home Assistant service.

Viewer is excluded from current admission. Edge admission must use the real
fixture/controller, encrypted spool, receiver identity, pull/commit/ack,
retry, deduplication, and restart paths. None may use a fake consumer to fill a
matrix row.

G1/G2 exit assertions:

- all seven client entries are either independently admitted or explicitly
  pending with a named missing runtime/input;
- source registry and adapter tests pass with exact current profile bindings;
- one composed local process lane per admitted adapter reaches final stop and
  child-zero proof;
- no library/developer-resource entry creates an idle service;
- the current installed counters remain pending until an actual host run.

## H4 — R1 collection, recovery, data parity, and reliability contract

**Status:** `ACTIVE_WHEN_NAMED_SOURCE_INPUTS_EXIST_NONBLOCKING_FOR_USABILITY`.

Once B1/B2 are working on the primary VM, R1 exercises the Hub lifecycle against
real collection, migration, recovery, and the selected consumer runtimes. Hub
core remains independently usable without optional components. The
implementation uses the current Rust/AppKit lifecycle rather than adding a
second service manager.

Existing lifecycle seams:

- `src/application/main/cli.rs`
- `src/application/main/dispatch.rs`
- `src/application/main/companions.rs`
- `src/application/main/migration.rs`
- `src/application/main/macos_service.rs`
- `src/platform/linux_systemd.rs`
- `src/platform/macos_launch_agent.rs`
- `src/runtime/lifecycle.rs`
- `src/runtime/config.rs`
- `src/storage/db.rs`
- `src/storage/data_recovery.rs`
- `scripts/bootstrap-companions.py`
- `tools/companions/{core.py,recipes.py,targets.py,cli.py}`

The bootstrap contract established in B1 is:

- interactive selection lists Hub core, Fleet helpers, Home Assistant, Edge,
  and developer resources with platform/runtime requirements;
- unattended selection accepts a deterministic component list and explicit
  catalog/source/profile paths;
- core selection never pulls a companion, telemetry credential, public
  ingress, or another token-refresh owner;
- the selected result is written as a redacted component-selection receipt;
- the operation is lock-protected, staged, digest-checked, and reversible;
- service start occurs only after configuration and credential ownership checks;
- setup, status, doctor, preflight, bootstrap, backup, restore, and service
  controls work when no optional component is installed.

The minimum real Hub fixture from H2 is used to test bootstrap and lifecycle.
The normal service must be stopped before exclusive-store commands; setup must
not invent a second writer. The app must report failure and retain a usable
Retry/Settings path when setup, package verification, or diagnostics fail.

R1 adds the following gates:

```sh
rustup run 1.98.0 cargo test --locked application::main
rustup run 1.98.0 cargo test --locked runtime::lifecycle
rustup run 1.98.0 cargo test --locked storage::data_recovery
python3 scripts/bootstrap-companions.py --help
python3 -m unittest discover -s tools/companions/tests -p 'test_*.py' -v
```

These are source/static gates only until run on an isolated installed target.
Runtime assertions must prove: clean bootstrap, no companion core operation,
correct service ownership, readiness distinction, restart/persistence, failed
selection rollback, upgrade rollback, and default data-preserving uninstall.

The parity and recovery work is detailed in H7. A working bootstrap with
synthetic data does not prove TeslaMate collection parity or durable recovery.
The missing named-source inputs remain an acceptance blocker for those claims,
but must not block an independent active ARM64/Mac Hub-only usability
correction.

## H5 — D1 / G3 selectable native packages and full Docker distributions

**Status:** `ACTIVE_MINIMUM_ARM64_MAC_USABILITY_ONLY`.

Current H5 work is limited to the smallest bootstrap, dependency, native-app,
or service work needed to make the active Debian ARM64 and
Apple-silicon Mac products usable. Polished selectable component packages,
full bundles, broad Docker profiles, x86/x86_64/amd64 images and packages, and
Intel-Mac work are deferred. Preserve their schemas and historical evidence;
do not use the deferral to weaken a validator or invent a candidate.

### 2026-09-09 bounded Home Assistant adapter evidence

The source now exposes one machine-readable, fail-closed bridge for the
accepted `home-assistant` / `debian13-arm64-container` selector. `teslatlas-hub
companions d1-plan` records the immutable aggregate selection without a
mutation. The separately explicit `companions d1-install` accepts only a
caller-supplied disposable local candidate with the reviewed HA source head,
profile, payload-manifest, and selection-receipt identities; it then enters the
existing transactional HA stage/link path. It does not resolve another source,
create a package, start Home Assistant, or promote a runtime result.

The bridge rejects Hub core and Fleet helpers before inspecting the manifest,
because neither has an admitted candidate artifact. Its disposable integration
test preserves the existing HA configuration sentinel and stages only the
owned integration link. Focused source checks passed: 80 companion Python
tests, 9 aggregate-manifest checks, 1 bootstrap-catalog check, and 11 Rust
companion-delegation tests (including 5 D1-specific cases); Rust formatting
and the Hub binary build also passed. These
are source/disposable-path checks only: `runtime_acceptance` remains false, and
they do not satisfy this milestone's package, selected persistent target,
runtime, R1, or X1 gates.

This later milestone implements the user-visible distribution choices. The explicit
product requirement for a selectable helpers package overrides the older
planning ruling that avoided separate companion packages. Hub remains optional-
companion-free: helpers and other components are selectable additions, never a
core dependency.

### macOS package choices

For the active target, first make the core Hub app/service runnable on Apple
silicon with setup, administration, pairing/API access, restart, and retained
data. The broader visible component-selection UI and every non-core package
choice remain later delivery work unless a concrete active usability gap needs
one. Home Assistant remains a selected external ARM64 runtime, never a native
Mac daemon. Intel Mac is paused.

Extend the existing paths:

- `scripts/build-macos-app.sh`
- `scripts/build-macos-service-package.sh`
- `packaging/macos-service/`
- `macos/TeslatlasHubApp/project.yml`
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubController.swift`
- `macos/TeslatlasHubApp/TeslatlasHubApp/ServiceDetailsWindowController.swift`

Add the smallest focused helper/selection seams needed, expected under:

- `scripts/build-macos-component-package.sh`
- `packaging/macos-components/`
- `packaging/components.json`
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubPackageSelection.swift`
- `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubPackageSelectionTests.swift`

The installer visibly offers deselectable choices for:

1. Hub core app plus Hub service;
2. optional Fleet helpers package (Tesla command proxy and Fleet Telemetry
   receiver, only when their reviewed arm64 binaries/evidence are supplied);
3. optional Edge service components where their runtime/platform is selected;
4. Home Assistant integration target, which points at a selected HA runtime
   and never installs a native HA service;
5. Protocol/TypeScript/Swift developer resources, which install files/tools
   only and never create LaunchAgents.

The core choice must install and run without the helpers package. Each selected
package binds `components.json`, legal material, source/artifact digests,
service ownership, and data/config paths. The app must display blocked reasons
for unavailable or incompatible choices rather than silently selecting them.

### Debian package choices

For the active target, retain the verified ARM64 Hub-only path and correct only
the next observed ordinary bootstrap/service gap. The
x86/x86_64/amd64 Debian package rows are deferred and cannot gate this work.

Extend `scripts/build-deb.sh` and existing files under `packaging/linux/` to
produce explicitly named core, helper/component, and bundle selections. The
core package owns `teslatlas-hub.service`; helper packages own only their
declared units and are disabled until configured. Developer resources do not
install systemd units. Home Assistant is an integration install into a
selected HA runtime, never a native Debian HA service.

The package must support `bootstrap`, deterministic component selection,
architecture checks, exact sidecar locks, legal bundle verification, and a
transactional post-install path. Core-only installation is the first required
case; helper and bundle cases are separate candidates.

### Docker core and full profiles

An ARM64 Docker action is in scope only when it is the minimum missing runtime
for an active product path. Full profiles and every x86/x86_64/amd64 image or
Docker validation are deferred; static composition evidence is not a substitute
for the active native Hub-only route.

Retain and extend:

- `Dockerfile`
- `compose.yaml`
- `packaging/docker/config.toml`
- `packaging/docker/config.toml.example`
- `packaging/docker/prepare-volumes.sh`
- `scripts/test-docker-packaging.sh`
- `docs/guides/install-docker.md`

Extend the existing `compose.yaml` with optional profiles; add a single
focused override only if needed for different deployment settings. Reuse the
Edge repository's existing Dockerfile/build target and the official HA
Container image. Do not duplicate that Dockerfile into Hub or
create one Compose file per component unless a concrete runtime requirement
makes that necessary.

Hub owns the aggregate composition and `scripts/test-docker-ecosystem.sh`
(proposed) if no existing runtime gate covers selected startup, persistence
and stop. Companion owners supply their actual images/build contexts and fix
architecture defects in their own roots. Reuse `scripts/test-docker-packaging.sh`
for static shape checks; keep runtime checks separate.

Protocol and SDKs participate in build/export stages only. Home Assistant uses an explicit HA runtime
profile and integration activation. Edge retains its encrypted spool and
receiver identity. Direct Fleet and Edge are explicit alternatives; the full
profile must reject both when configured simultaneously.

### Distribution lifecycle gate

For each active ARM64/Mac package/profile choice, test the same lifecycle contract: clean
install, config ownership, bootstrap, readiness, normal function, stop/start,
restart/persistence, interrupted install recovery, upgrade with backup, failed
upgrade rollback, uninstall preserving data/configuration/identity/TLS/pairing,
and explicit destructive purge. The final receipt records package/profile
selector, source/artifact hashes, platform, service state, endpoint closure,
and data identity before and after.

Required source/static commands:

```sh
sh scripts/test-macos-packaging.sh
sh scripts/test-linux-packaging.sh
sh scripts/test-docker-packaging.sh
python3 scripts/verify-repository-layout.py
python3 scripts/verify-provenance.py
```

These commands must not be presented as installed acceptance. Active-target
package builds and runtime tests are authorized only when they close the next
identified usability gap and have the required Hub-owned handoff and heavy-build
lock. Paused architecture rows remain untouched.

## H6 — X1 / G4 active ARM64/Mac usability and deferred installed matrix

**Status:** `ACTIVE_ARM64_MAC_SUBSET_FULL_MATRIX_PAUSED`.

The active X1 subset makes an ordinary current candidate usable on the two
active targets. It may proceed when its minimum Hub-owned handoff is ready; it
does not wait for named-source parity or a final package matrix:

1. macOS 13+ Apple arm64 with the selected macOS package choices and LaunchAgent;
2. Debian 13 ARM64 with native systemd;
3. an ARM64 Docker or Apple-silicon Docker Desktop route only if it is required
   by the active product path and separately claimed.

Debian x86_64, every amd64/x86 image or native row, Intel Mac, the remaining
cross-architecture Docker rows, and the final `21/21`, `444/444`, `10/10`
matrix are **PAUSED**. They remain visible for later acceptance, but cannot
block the active ARM64/Mac product milestone.

The existing VMs, macOS image, Colima context, old receipts, and protected
production resources are not interchangeable. QEMU compilation or a foreign
container is not native architecture acceptance.

The following historical full-matrix command remains authoritative only when
the owner resumes the paused matrix:

```sh
python3 tools/interop/run.py \
  --config "$HUB_MATRIX_CONFIG" \
  --require-complete \
  --receipt "$HUB_MATRIX_RECEIPT"
```

The private config and receipt path must point to the reviewed candidate and
fresh run. The runner must assert exact source/artifact/profile/runtime
identity, all required cells/cases, service readiness, real function, restart
and persistence, cleanup, final stop, endpoint closure, and child-process
zero. A zero exit status or receipt parser pass without those fields is
insufficient.

The historical full completion result is exactly `21/21` cells, `444/444` case
slots, and `10/10` cohorts. Until it exists, the ledger counters stay pending
and no support claim is widened. Those counters are not an active-target
usability blocker while the corresponding rows are paused.

Docker assertions include `docker compose config` for core and full profiles,
trusted external TLS, persistent named volumes, no-new-privileges/capability
drop/read-only root behavior, restart/recreate persistence, backup/restore,
profile dependency closure, selected HA runtime activation, Edge
spool/retry/deduplication, and no private Fleet port
publication.

## H7 — R1 / G5 TeslaMate parity, passive data, recovery, and soak

**Status:** `WAITING_FOR_NAMED_SOURCE_INPUTS_NONBLOCKING_FOR_ARM64_MAC_USABILITY`.

This is a central Hub deliverable and must use a named TeslaMate reference,
not only row counts. Existing import seams are:

- `src/import/teslamate/reader.rs`
- `src/import/teslamate/schema.rs`
- `src/import/teslamate/stage.rs`
- `src/import/teslamate/direct.rs`
- `src/import/teslamate/importer.rs`
- `src/import/teslamate/projection.rs`
- `src/import/teslamate/projection_state.rs`
- `src/import/teslamate/parity.rs`
- `src/import/teslamate/source.rs`
- `src/import/teslamate/writeback.rs`
- `src/application/main/migration.rs`
- `src/application/main/teslamate_check.rs`
- `src/storage/db/observations_and_lifecycle.rs`
- `src/storage/db/observation_persistence.rs`
- `src/runtime/lifecycle/`

The parity matrix must record, for every reference field and derived value:

- source field, Hub field, type, units, conversion, and provenance;
- `NULL` versus zero versus absent versus inapplicable semantics;
- drive start/end, duration, distance, energy, positions, sleep/awake state,
  charging process/session, charge samples, state/update events, and geocoding;
- gaps, outage and reconnect boundaries, duplicate delivery, ordering,
  retention, migration, open-session cutover, and successor-delta behavior;
- supported, absent, and inapplicable fields separately;
- normalized values and time bounds, not only row counts.

Use the existing read-only TeslaMate check/import path. PostgreSQL inspection
must use an explicit read-only transaction such as:

```sql
BEGIN READ ONLY;
-- bounded schema, selected-car, and parity queries
COMMIT;
```

No copied production token is refreshed, no Tesla egress is enabled for a
history-only import, and no live service is modified. Preserve the source
backup and compare IDs/nulls/units/time ranges/content before and after.

For passive collection, use the existing commands through the admitted config:

```sh
teslatlas-hub --config "$HUB_CONFIG" observation-watermark --car-id "$CAR_ID"
teslatlas-hub --config "$HUB_CONFIG" verify-observation \
  --car-id "$CAR_ID" --watermark "$WATERMARK"
```

Record starting and ending durable watermarks, source provenance, collection
mode, outage/reconnect behavior, duplicates, and the agreed observation window.
No event remains pending; absence of an event is not a pass. Vehicle wake or
command is never an acceptance shortcut.

Recovery and soak use the existing commands and data structures:

```sh
teslatlas-hub --config "$HUB_CONFIG" backup --destination "$BACKUP"
teslatlas-hub --config "$HUB_CONFIG" verify-backup --source "$BACKUP"
teslatlas-hub --config "$HUB_CONFIG" restore-data \
  --source "$BACKUP" --destination "$RESTORED_DATA"
```

Restore remains stopped and without collector authority until credentials are
explicitly recovered or replaced. Prove data identity, pairing/TLS behavior,
newer synthetic observation continuity, and original-volume preservation. Soak
must cover restart, reconnect, storage growth/retention, duplicate delivery,
and clean shutdown with a redacted durable receipt.

G5 results are live/passive evidence and do not replace installed matrix rows.
The absent preserved source, read-only identity, and selected car are explicit
acceptance gaps; they do not stop independent active ARM64/Mac Hub-only or
minimum launch/service work.

## H8 — G6 final owner Azure clean-host validation and closure

**Status:** `PAUSED_OWNER_SCOPE_2026_09_09`.

Azure and the final clean-host matrix are deferred. Do not request, provision,
compile for, test, repair, or clean an Azure/x86 target until a new owner
direction resumes this section. Retain all requirements below as historical
final-acceptance criteria.

G6 is available only after H2–H7 have completed the applicable source,
candidate, local macOS, and authorized VPS gates. It requires a reviewed local
distribution candidate and fresh owner-provided Azure Debian 13 ARM64 and
x86_64 clean hosts. Azure cannot replace a missing Mac/VPS gate or justify
emulation as native support.

Before requesting Azure, freeze and independently review:

- exact Hub source commit and dirty-tree disposition;
- exact core/helper/component package and Docker profile identities;
- `packaging/components.json` and selected component receipt;
- profile/validator/raw-schema and sibling source digests;
- toolchain, base-image, legal bundle, config, TLS, and data identities;
- local macOS and VPS results, limitations, and parity/recovery receipts.

On each Azure host run the same reviewed candidate through clean install,
bootstrap, onboarding, readiness, required component function, restart/
persistence, upgrade/rollback where applicable, uninstall/data preservation,
Docker profile tests, final stop, endpoint closure, and child-zero proof. Use
the exact runner command from H6 with a fresh private receipt. The final result
must contain `21/21`, `444/444`, and `10/10`; any pending row blocks completion.

G6 does not authorize a commit, push, release, tag, image upload, registry
publication, or deployment. The root coordinator handles any later source-only
publication decision under a separate explicit authority. If a sibling source
commit changes an accepted runtime, rerun the affected candidate and installed
cases rather than relying on reachability alone.

## Required review and reporting format

Each milestone produces a small redacted result index with:

- candidate source/dirty identity and exact file manifest;
- profile, package/image, component-selection, toolchain, platform, and runtime
  identities;
- command, exit status, and timestamp;
- source/static, candidate, installed, and live/passive results in separate
  sections;
- passed, failed, blocked, skipped, and untested cases with reasons;
- service ownership, readiness, endpoint closure, child-zero, data identity,
  backup/recovery, and upgrade/uninstall evidence;
- artifact paths and SHA-256 digests without private credentials or vehicle
  identifiers;
- remaining support and publication limits.

Use the existing `tools/interop` harness and receipt schema. Extend it only at
the bounded adapter/fixture/receipt seams above; do not create a second large
framework or rewrite the stored pending counters. Keep old receipts and
historical plans immutable and label them as historical.

## Current non-goals and hold conditions

- The owner lifted the implementation hold on 2026-09-08; follow the current start authorization and stage order.
- No real vehicle command, wake, account reconfiguration, production broker,
  TeslaMate service mutation, or collector restart is part of source planning.
- No GitHub CI, release, tag, registry image, binary upload, or public catalog
  publication is implied.
- No universal Windows, NAS, Docker Desktop, or Home Assistant native-daemon
  claim is made without its own accepted runtime evidence.
- No Protocol, TypeScript SDK, or Swift SDK daemon is added merely to fill a
  package/profile choice.
- x86/x86_64/amd64, Intel Mac, Azure, and full cross-architecture matrix work
  are paused. Preserve all related code, schemas, receipts, VM artifacts, and
  candidate records; do not build, install, image, run, repair, or clean them
  without a new owner direction.

The active ARM64/Mac product milestone is complete only when the documented
Hub-only routes are usable on both active targets with the appropriate setup,
administration, pairing/API access, restart, retained-data, and cleanup
evidence. Viewer is excluded from this milestone.
The historical full product goal remains incomplete until each H1–H8 exit
condition, including the currently paused rows, is resumed and produces current
evidence.

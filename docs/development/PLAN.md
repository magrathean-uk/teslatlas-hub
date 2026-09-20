# Hub full-product completion plan — 2026-09-19

Objective: deliver a complete, independently usable Hub and the integrated six-product
Hub ecosystem. App v7 adoption readiness is an accepted historical milestone, not the
completion criterion.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[product specification](../../../docs/development/PRODUCT_SPEC.md),
[coordination](../../../docs/development/COORDINATION.md), and [STATUS.json](STATUS.json).

## Current position

G0–G7 and the bounded Home Assistant Container journey remain accepted exactly within
their recorded scopes. Their receipts, hashes and closed runtime identities are
immutable. They prove useful source-built synthetic journeys; they do not prove the
complete supported feature set, ordinary installation and lifecycle, minimum floors,
real-source parity, reproducible distribution or the final installed ecosystem.

`full_solution_state` remains `NOT_ACCEPTED`. The owner activated the goal on
2026-09-19. F0 passed independent review. The first bounded Debian 13 ARM64 package
foundation is independently accepted only within its explicit partial scope: F1 and
F6 remain active and unaccepted. The bounded F6 source/catalog foundation is
independently accepted only within its partial scope after closing three P2 contract
findings: a distributable build must be given one exact pushed 40-hex
Hub commit and exposes its immutable `/tree/<commit>` source URL, while an unbound
developer build remains useful but explicitly non-distributable. No final artifact has
yet been admitted as an ordinary public production artifact. Separately accepted
bounded slices now prove exact pushed-source Debian-package and ARM64-container image
byte reproducibility; F6 remains open beyond those private acceptance artifacts.

The active companion bootstrap model contains exactly Protocol, TypeScript SDK,
Swift SDK, Home Assistant and Edge. Hub source identity is recorded separately, making
six repositories in the full cohort. The shipped current catalog now contains one exact
five-companion `2026.36.2` cohort and excludes Viewer. Its status is truthfully
`local-unpublished`: every commit was fetched anonymously by exact hash, but source
reachability does not publish the accepted TypeScript archive or make the local-source
lifecycle an ordinary production input. Historical D1 records, including their Viewer
entry, and Viewer R1 fixtures remain inactive evidence and are not catalog inputs.

The accepting Sol/high delta review found no remaining issues. It verified the
baseline-identical historical D1 manifest at SHA-256
`37b6fc64fd804813d82053c7cf8d12e89cb9ebfc69ce569c4d1c84d645bed0aa`,
JSON Schema/runtime parser agreement, and finalizer binding of a bound source commit to
the tested Hub cohort head. Exact clean-export native and container artifact
reproducibility are now independently accepted in later bounded slices; genuine
predecessor update/rollback and ordinary public production admission remain open F6 work.

The bounded five-companion catalog/lifecycle slice recomputed byte-complete Hub source
manifests from immutable exports for Protocol `53b5c648`, TypeScript `56a07dd7`, Swift
`f98dde98`, Home Assistant `dfb2b050` and Edge `c9965cd2`. Exact anonymous fetches,
profile digests, TypeScript package `070906b5…`, HA payload `ebf7d09f…` and selection
receipt `99fdac66…` all read back. It closed three observed Hub defects: the obsolete
TypeScript artifact pin, compatibility/publication status conflation, and the missing
data-preserving companion removal command. On macOS arm64 the exact catalog passed a
four-component dry run and install, verified status, locked no-op install/update,
explicit no-predecessor rollback failure, removal with data/config preservation,
post-removal status and idempotent removal. The exact five-component macOS dry run
correctly rejects Edge's Linux-only recipe before mutation. That accepted slice stopped
after confirming that the retained Debian ARM64 guest lacked the remaining exact
fixed-recipe toolchains, so its receipt makes no five-component runtime or
update/rollback claim. Exact outputs and limitations are recorded in
[`f6-five-companion-catalog-lifecycle-2026-09-19-r1.json`](f6-five-companion-catalog-lifecycle-2026-09-19-r1.json).
Initial independent review rejected one P2 removal-safety defect: a present regular
`PREFIX/active` path was misclassified as absent, allowing replaceable releases to be
deleted while the unsupported active path remained. The closure now distinguishes an
absent path with `lstat`, rejects every present non-symlink before lock creation or
recovery mutation, and rechecks under the lock. An exact pre/post tree regression covers
regular-file, directory and FIFO active paths. Removal hard-exit coverage at every
transaction checkpoint proves rollback before commit, completion after commit, final
cleanup and data/config preservation. Same-reviewer delta review confirmed that fix but
rejected a second P2: unsafe `releases` or `history.json` inode types were detected only
after journaled deactivation. The second closure now validates both paths without
following symlinks before lock creation, under the lock, after recovery, before the
transition and immediately before deletion. Exact zero-change fixtures cover a
symlink/file/FIFO `releases`, symlink/directory/FIFO history, and post-recovery plus
pre-delete symlink swaps without touching external markers.
The same reviewer then returned `ACCEPT` with no P1/P2 findings. It independently
confirmed exact zero-change releases/history symlink reproductions, refusal of the
final-validator swap without changing outside or saved bytes, preservation of the
prior active-path closure, and the recorded 88 Python plus 13 Rust checks. This is an
independently accepted bounded partial catalog/lifecycle slice only: the exact-five
runtime, genuine predecessor update/rollback, ordinary production admission and
retained detailed installed-output receipt remain absent, so F6 remains open.

A subsequent fresh Debian 13.7 ARM64 slice provisioned the exact fixed-recipe
toolchains and passed the local-unpublished exact-five dry-run, install, installed
status, identical reinstall and same-cohort update no-ops. It retained the complete
installed receipt and tree, then proved the honest no-predecessor rollback error made
zero tree changes and that removal preserved exact data/config plus unrelated Home
Assistant state; post-removal status and idempotent removal also passed. The 44-file
redacted bundle is `COMPLETE` at aggregate SHA-256
`9101da714e19fb85f8efdbf713d72718a8835d4993df51c77e1faebf24fd7d39` and manifest
SHA-256 `5c0250f97503ce51ab8818b5c34671f0e54f23e733a573fb38f9c077d57b0d56`.
Independent Sol/high review returned `ACCEPT` with no P1/P2 findings after recomputing
all 44 payload identities, the aggregate and manifest, exact source and output
manifests, the 9,238-entry installed tree, lifecycle transitions, package symmetry,
secret scan and cleanup. This is independently accepted bounded partial evidence only.
The slice installed or started no companion service, had no immutable predecessor,
performed no publication, and does not accept F3, F4, F6 or F7; see
[`f6-five-companion-linux-runtime-2026-09-19-r1.json`](f6-five-companion-linux-runtime-2026-09-19-r1.json).

The next bounded container-preparation slice binds the official Rust and Debian
base tags to exact multi-platform index digests whose Linux ARM64 children were
read back from the registry, removes all runtime package-manager inputs, copies the
public CA bundle from the pinned builder, and replaces the shell/curl probe with a
bounded Hub CLI TLS healthcheck. Initial Sol/high review rejected the draft with
three P1 and one P2 findings. The frozen delta now copies and clean-context-compiles
all production embedded inputs, validates the documented certificate DNS name over
loopback with a real positive/mismatch TLS test, uses a distinct volume initializer
with only `CHOWN`, `FOWNER`, and `DAC_OVERRIDE` plus an exact ownership/mode check,
and excludes private key/certificate extensions from the build context. Same-reviewer
delta review rejected only a P2 mismatch between STATUS and the receipt's truthful
ephemeral test-listener record. After correcting that claim, the final metadata-only
review returned ACCEPT with no findings for this partial foundation. Compose still
requires an explicit exact pushed Hub commit and the documented runtime readback
requires the image's `source` result to match it. No image, container or VM was
started in this slice; clean-export ARM64 build/runtime/lifecycle proof remains open
after source publication.

## Full-product gates

- **F0 — feature and support ledger.** Inventory every user-visible and operator-facing
  claim in current Hub documentation, configuration, CLI, Mac UI, package/container
  recipes and public contracts. For every claim record its implementation owner,
  target/platform floor, distribution path, dependencies, tests and required runtime
  evidence. A claim must be implemented and proven or corrected before acceptance.
  The ledger includes setup and diagnostics, Legacy and Fleet collection adapters,
  pairing/trust, current/history and sync/export behavior, TeslaMate import, repair,
  retention, backup/recovery and all ordinary lifecycle commands.
- **F1 — standalone Hub.** Prove a fresh operator can build and use Hub without any
  companion on Debian 13 ARM64 and Apple-silicon macOS 13, plus the documented
  ARM64 container path. Cover first setup, restart/persistence, degraded and failed
  setup, TLS/device lifecycle, collection-adapter behavior, data operations and
  documented UI/CLI diagnostics. Vehicle-command logic may use local provider
  emulation only; no real vehicle action is permitted.
- **F2 — Edge path.** After F1, accept the pinned receiver/proxy and Edge delivery
  path under Edge's product plan. Hub must prove exact configuration, service order,
  commit/ACK/deduplication, restart and recovery boundaries without making Edge
  mandatory for standalone Hub.
- **F3 — Protocol and SDK consumers.** Admit the complete Protocol, TypeScript and
  Swift supported surfaces, packages, declared floors and recovery behavior against
  the exact Hub build. Keep product, HTTP-profile, Edge-profile and storage versions
  separate.
- **F4 — Home Assistant required.** Home Assistant is required for overall completion,
  though it may execute after the primary F1–F3 lanes. Accept its supported install,
  update, recovery and ordinary user journey against the exact Hub.
- **F5 — real input and parity.** A fresh owner-supplied, read-only named-source export
  and a fresh separately authorized passive capture are mandatory input-dependent
  gates. Prove field/unit/null/zero/history/import parity, passive ingestion,
  interruption/recovery and redaction. Never reuse historical private inputs and do
  not wake, command or mutate a vehicle or source.
- **F6 — reproducible distribution and usable documentation.** Prove source-built
  Debian ARM64 package, Apple-silicon package, ARM64 container and source handoffs,
  including install, upgrade, rollback, verified backup/restore and data-preserving
  removal. The shipped companion source catalog must admit exact immutable sources for
  the six active repositories only; ordinary install/update/status/rollback/removal
  must work without Viewer. An unavailable source or unsupported target must fail
  before activation. GitHub remains source storage; publication/release actions need
  their own authority.
- **F7 — combined installed end to end.** From clean supported hosts, run the complete
  installed ecosystem using F6 artifacts: Hub alone, each supported optional-component
  composition, the selected collection path, Protocol-bound TypeScript and Swift
  consumers, and required Home Assistant. Prove upgrade/restart/recovery and final
  cleanup with no hidden source-tree dependency.

## Hub-specific acceptance

F0 must map the exact documented Mac app and service, Debian systemd, Docker Compose,
Legacy/Fleet, migration/import, sync/export/repair/retention, pairing/API, backup and
recovery behavior. The Mac UI must complete setup, diagnostics, service control,
upgrade and data-preserving uninstall on its declared floor. Debian and container
paths must cover their documented capabilities and limitations rather than borrowing
Mac evidence.

External Fleet receiver and command-proxy dependencies are bounded dependencies, not
new products. Audit and pin their source/version, build, legal inputs, configuration,
credentials, service topology and Hub integration. Use source tests or local provider
emulation for command logic. Do not perform production, account or vehicle actions.

The companion bootstrap currently has no admitted ordinary source cohort. F6 must
populate and verify the exact six-repository catalog, exclude Viewer, and exercise
the real operator commands and rollback records. Hub remains usable when companions
are absent or one optional companion is unavailable.

## Work slices

L1–L3 are execution slices only; none is a completion substitute:

1. **L1:** close F0, then implement and prove Debian ARM64 F1/F2 installed lifecycles.
2. **L2:** prove Apple-silicon floors and native lifecycles for F1–F4.
3. **L3:** close F3/F4 packaging, the six-source catalog, F6 documentation and
   reproducibility, then run F5 when its fresh inputs exist and finish with F7.

Current L3 checkpoint: source-identity enforcement, exact five-companion catalog
generation/readback, the populated local-unpublished cohort, Viewer-free active
selectors, Hub-only `--components none` bootstrap, data-preserving companion removal,
and native/container packaging inputs are implemented and focused-tested. The bounded
four-component macOS lifecycle and the local-unpublished exact-five Debian ARM64
lifecycle are independently accepted bounded partial evidence. Genuine predecessor
update/rollback, complete native/container rebuilds and ordinary public production
admission remain pending. Release, tag and
binary publication remain outside this slice. F6 is open.

Container foundation checkpoint: immutable official base index identities, the
verified Linux ARM64 child/config identities, complete builder inputs,
package-manager-free runtime assembly, DNS-validating private-CA Hub-binary
healthcheck, bounded volume initializer and exact source readback procedure are
independently accepted within this partial source/static scope after the initial and
delta REJECT findings were closed. This is not a built image or a runtime acceptance
claim. Exact clean-export ARM64 image identity, source readback, health, security
posture, persistence, recovery, upgrade/rollback and removal remain pending.

Exact-export container runtime attempt: commit
`afae80a3aa5d3d440eb7d89d5d38b7d87fb37184` produced one native Linux ARM64
image from an archive whose host and guest manifests were reported matching. The
image reports the exact immutable source tree, runs as `10001:10001` with a read-only root, no
capabilities and `no-new-privileges`, and passed one-use DNS-validating TLS health,
doctor, status, restart and empty synthetic identity/data continuity. The build is not
a clean-host acceptance: the guest lacked Buildx, so exact Debian package
`docker-buildx 0.13.1+ds1-3` was downloaded and temporarily installed before the
later stop instruction arrived, then removed without changing the retained
Docker/Compose baseline or pruning the 889.3 MB BuildKit cache. These image/runtime
results are runner-reported, not accepted: cleanup removed the raw evidence root
without first retaining a redacted path/size/SHA-256 bundle, so independent review
could reproduce the source archive but could not inspect the image, binary or runtime
outputs. Scope A is `REJECTED_UNAUDITABLE_AFTER_CLEANUP`; it will not be rerun from
`afae80a`.

The runtime also rejected the ordinary one-shot Compose volume handoff. The accepted
source initializer left a newly prepared volume empty, allowing Docker's next mount
to replace its `10001:10001/0700` root metadata with the image WORKDIR's `0755`
metadata. Re-running the initializer only after synthetic state existed enabled the
bounded image-runtime evidence; that workaround receives no ordinary lifecycle
credit. Independent source review rejected the first sentinel correction because it
could follow a pre-existing symlink and because generic Compose startup did not order
the privileged initializer before Hub. Delta review then found a remaining
post-publication root-mutation race, cleanup evidence not bound to its prepared cohort,
incomplete secret screening, and self-asserted archive/tree fields. The accepted
closure temporarily quarantines the mounted directory to root, fully prepares
and verifies a private inode, and publishes the fresh regular sentinel as its final
sentinel mutation through `ln -T`. It accepts only an exact existing non-symlink regular
sentinel and adversarially proves that symlink, raced-destination and wrong-regular
targets are unchanged. Hub now depends on
successful initializer completion, and the test checks the rendered Compose JSON
topology when Compose v2 is available. BuildKit-only `COPY --chmod` flags are replaced
by ordinary COPY plus explicit root ownership/mode application for the retained
Docker 26 legacy builder. A two-phase allowlisted evidence harness must reach
`READY_FOR_CLEANUP` before cleanup and `COMPLETE` afterward. It retains and derives the
commit/tree/archive/manifest identities from the exact source tar, records per-file and
aggregate hashes, rejects common structured/text credential forms, generates an
evidence-run ID, and requires cleanup to match the source and complete cohort/resource
identity. This candidate is source-only. A second delta review verified those four closures but
found one remaining generic structured-key bypass (`token`, `secret`, and camel-case
`apiToken`); normalized nested key screening and exact regressions were then checked.
That final narrow review returned `ACCEPT` with no findings for the bounded
source/static correction only. This earlier receipt did not itself rebuild the
publication's exact export or exercise the ordinary retained-bundle sequence. Later
separate receipts now accept image-byte reproducibility plus a synthetic no-credential
pristine-first-start, TLS, scoped backup/restore, invalid-command recovery and
data-preserving removal/recreate lifecycle. Genuine immutable update/rollback,
credential/pairing and complete clean-host recovery, real collection,
retention/migration, public production admission, F1, F6 and F7 remain open.

The fresh exact-export rerun from published commit
`4102dce1376d725c7959ec71ec46ffdbe47b9c6b` is now frozen for independent
review. One native Linux ARM64 image was built through the retained Docker 26
legacy path without installing tooling. The ordinary single `docker compose up`
sequence completion-ordered one initializer, preserved the exact
`10001:10001/0700` volume root and regular sentinel, and reached healthy Hub
startup. The same synthetic empty cohort passed source/version readback, TLS
positive and wrong-name checks, doctor/status, non-root/read-only/no-capabilities
inspection, and explicit restart identity/status/source continuity. The fresh test
certificate and key were initially staged with ownership/mode that failed Hub's
inode predicate, causing ten automatic retries; correction of only those one-use
inputs made the same cohort healthy, so this is not a pristine first-attempt TLS
startup claim. The retained two-phase bundle reached `READY_FOR_CLEANUP` before
cleanup and `COMPLETE` afterward, with 25 allowlisted files and aggregate SHA-256
`ec3ad4ff24ea801e414eeb1f0f1f725cf09a40ffa789a858a621cd15366c382e`.
The two rejected preparation attempts—raw doctor free text and secret-shaped
metadata field names—are disclosed with their redaction/rename closures. The exact
jq filter and raw doctor hashes were not retained, so the redaction is
runner-reported rather than independently reproducible; only the allowlisted
derivative is inspectable. Independent review rejected two P2 claim-integrity
issues: that overstatement, and ambiguity between the removed cohort runtime image
and a disclosed retained anonymous builder ancestor. The metadata-only closure
defines the cleanup flag as the exact cohort image ID and tags, while the build
cache and anonymous ancestor remain explicitly retained. Same-reviewer delta review
returned `ACCEPT` with no findings for this bounded runtime foundation. No owned
final runtime image/tag, listener, one-use TLS material or lock remains; the guest
is stopped. This earlier foundation did not itself prove image-byte reproducibility,
pristine first-attempt TLS startup, backup/restore, failed-candidate recovery or
removal; later separate receipts now accept those exact bounded image and synthetic
no-credential lifecycle results. Genuine immutable update/rollback, credential/pairing
and complete clean-host recovery, real collection, retention/migration, public
production admission, F1, F6 and F7 remain open.

The bounded HUB-08 repair-atomicity source fix is independently accepted. On base commit
`09684337332fdd7eae9ad460a8cc3bf6b1aaafd3`, `repair_at` previously deleted
expired retired-lineage metadata before later pack/binding validation, SQLite
quick-check and stale-staging cleanup could fail. The accepted two-file patch moves
the unchanged cutoff deletion to the final fallible operation. Filesystem staging
and orphan cleanup still run first; on success, the same predicate deletes expired
parents and SQLite cascades their child bindings. On any earlier returned failure,
eligible parent and child metadata remain unchanged for retry. A focused corrupt
retained-lineage fixture proves exact failure preservation and same-cutoff successful
parent/child deletion. Initial review rejected a P2 child-cascade test gap; the
same-reviewer delta returned `ACCEPT` with no findings. This is source-level repair
ordering only, not installed-path, retained corruption-cohort, complete HUB-08, F1
or F6 acceptance.

The fresh installed Debian 13 ARM64 storage and recovery cohort at published head
`0d5e63f23b8649c0d56c865bc9cb1fd9c1b43950` is independently accepted within its
bounded scope. An ordinary `2026.36.1-1` install initialized schema 57 and the
exact source-built `2026.36.2-1` package migrated that store to schema 59. The installed
repair command then failed on one deliberately malformed unexpired retained lineage
without changing the exact eligible parent/child metadata snapshot, retried after only
that malformed fixture was removed, deleted the eligible expired parent by cascade,
retained one row and later deleted it after the runner advanced the fixture. The exact
one-hour grace classification and cutoff ordering are runner-recorded because the
retained bundle does not include the fixture expiry, cutoff or post-advance timestamps.
Data backup/verification, separate encrypted cursor-
key recovery, separately retained config/TLS restoration, systemd writable-path and
enablement restoration, strict TLS health, doctor, restart/persistence and ordinary
data-preserving package removal all passed. The retained allowlisted evidence bundle is
`COMPLETE`; it does not independently prove a prior `READY_FOR_CLEANUP` state. Its 24
payload files have aggregate
SHA-256 `2c6efc9c7a196abc851bc961da4d430ecc63f7ea84bde78ee965e4b4491f1774`
and manifest SHA-256 `ed7dcb639e990dfdb54d36c1766a37133f9af009983cb1af39aceb75f333c5bf`.
No product source defect was observed and no source file changed. Initial migration
stdout was not retained, so those transition values remain runner-recorded; the exact
packages and final restored schema-59 state are retained. This is a synthetic core-only
storage cohort, not F1 or F6 acceptance, real-source or passive-data evidence,
Fleet-sidecar acceptance, or package-byte reproducibility. Initial independent review
rejected two P2 claim-integrity gaps: the retained bundle did not prove a prior
`READY_FOR_CLEANUP` state, and it lacked the expiry/cutoff/post-advance timestamps needed
to reproduce the exact one-hour boundary. The metadata/evidence delta calibrated both
claims without changing runtime results. Same-reviewer review independently recomputed
all 24 payload hashes, aggregate and manifest, verified the receipt, artifacts,
redaction and cleanup, and returned `ACCEPT` with no P1/P2 findings.

The bounded Debian ARM64 package-byte reproducibility slice starts from two
independent clean archives of pushed Hub commit
`1b6dc00379819869f4cd8ed8897c2f1f915023a2`. The original package builder produced
different `control.tar.xz`, `data.tar.xz` and complete `.deb` bytes even though both
release binaries and every extracted payload file were identical; fresh staging and
ar/tar wall-clock mtimes were the only observed delta. The then-uncommitted correction,
now published at `1e6132aa826aaff83a7c4899604ee4c59e2f852e`, requires the selected
commit's explicit `SOURCE_DATE_EPOCH` and exports it to
`dpkg-deb`. Two fresh corrected builds now match completely at package SHA-256
`73aee10be8f0b4fc285e43ba341c00cf2a06d3e77146860c28b67e5cacb1159a` and binary
SHA-256 `2b7c071614c9cb31e2fb3ac4b666b636ff8aae33035d661c09c4b935d5a0fd3c`;
all ar members, control/data metadata and extracted payload hashes match, and the
embedded source is the exact public `/tree/1b6dc003…` URL. The retained packages
bind to the original built patch;
the published implementation adds only stricter malformed-epoch rejection and its focused
tests, so package bytes were not rebuilt after that valid-input-output-neutral delta.
Independent Sol/high review accepted the bounded slice after closing both the
whole-value validation gap and the built/current/delta evidence identity gap, with
no remaining P1/P2 findings. Source commit
`1e6132aa826aaff83a7c4899604ee4c59e2f852e` was pushed and read back exactly from
`origin/main` and the live remote. This is a bounded package-byte/source-identity result, not
complete HUB-03, F1 or F6 acceptance. That receipt itself did not repeat a package
lifecycle; the separate later cohort below does.

The exact retained reproducible package was then exercised once in a fresh Debian 13
ARM64 ordinary dpkg/systemd cohort without rebuilding or downloading it. Package
SHA-256 `73aee10be8f0b4fc285e43ba341c00cf2a06d3e77146860c28b67e5cacb1159a`,
binary SHA-256 `2b7c071614c9cb31e2fb3ac4b666b636ff8aae33035d661c09c4b935d5a0fd3c`,
version `2026.36.2-1`, architecture `arm64` and embedded public source
`/tree/1b6dc003…` all matched before installation. The package created the expected
service identity, permissions and hardened unit, then passed bootstrap, a pristine
first strict-TLS start with zero retries, positive and wrong-name health checks,
explicit restart PID/Hub/config continuity, verified scoped data backup and
separate-root restore with a fresh cursor key, and rejection of a deliberately corrupt
Debian-envelope copy without changing installed state. Ordinary `dpkg -r` removed the
binary/unit/listener while preserving the service user, config, TLS and data; normal
clean-shutdown checkpointing changed database bytes, so byte identity is not claimed,
but installation identity and SQLite quick-check remained exact.

The deterministic 59-file evidence bundle is SHA-256
`36251a9213258bcaba232dac89c5d6931fcaa36813231891401f26a6fc9e6ed2`
with 58-entry manifest
`2423f97d534be020906bfacec9ad60a74b8b8a6b1bd2e1a8271ab0535b53c910`.
Independent Sol/high review revalidated the Debian ar/xz envelope, package/binary/source
identity, every payload and archive byte, secret scan, cleanup and current host state,
returning `ACCEPT` with no P1/P2. The final purge removed exact package state,
config/TLS/data, user/group, units, processes, listeners and private roots; the guest is
stopped, SSH is closed, the lock is released and the retained host package is unchanged.
This binds exact reproducible package bytes to a bounded synthetic no-credential
HUB-03/HUB-11 lifecycle. It does not prove genuine update/rollback,
credential/pairing recovery, real collection, retention/repair/migration, Fleet,
complete clean-host recovery, public release, complete HUB-03/HUB-11, F1, F6 or F7;
see
`docs/development/f1-f6-debian13-arm64-package-systemd-lifecycle-2026-09-20-r1.json`.

The same exact retained package then passed a separate fresh Debian 13 ARM64
pairing-authority and data-backup boundary. A one-time claim succeeded and replay
failed; rotation invalidated the old bearer while preserving device identity; live
listing and revocation reduced active devices from one to zero and invalidated the
current bearer. Before backup the source contained two paired-device rows with one
active device. The sealed backup and separately restored data each contained zero
pairing invitations, zero paired devices and zero active devices. The pre-backup
bearer failed against the restored service, while a fresh restored invitation could
be claimed once and its bearer worked. Installation ID, backup generation, schema 59
and SQLite quick-check remained logically continuous; SQLite page-byte identity is
explicitly not claimed.

The deterministic 25-file evidence bundle is SHA-256
`7336f2c989ece755eeb294dd5eaf46ae158cc683fd5a8ad4d554a2ed70df71d8`
with 24-entry manifest
`23d301d2ee380e7a755f38d0be4def3f7760b3f142bc1219579f85897f1ed2fc`.
Independent Sol/high review returned `ACCEPT` with no P1/P2 after recomputing the
bundle, inspecting the pairing/backup contract and sanitized facts, scanning for
secrets and rechecking cleanup. Two fully cleaned zero-credit harness calibrations
are disclosed: pairing creation requires the exclusive Hub lock, and restore proves
logical database facts rather than identical SQLite pages. This is bounded synthetic
HUB-06/HUB-11 evidence only; it excludes provider credential recovery, real
vehicles/data/collection, genuine update/rollback, complete clean-host recovery,
Fleet, public admission, complete HUB-06/HUB-11, F1, F6 and F7; see
`docs/development/f1-pairing-authority-backup-restore-debian13-arm64-2026-09-20-r1.json`.

The bounded native ARM64 container image-byte reproducibility slice is independently
accepted at published implementation commit
`e0b290e9d6d086ff75921596dcddfd497a077c7f`. Two distinct fresh official clones,
each proven absent before cloning and bound to exact tree
`4ebe78bf712427954fdcbcd53c1a9458ee1fe657`, ran the Docker Buildx `--no-cache`
path on Debian 13 ARM64 with Docker 26.1.5 and Buildx 0.13.1. Both produced the
same 129,280,000-byte canonical image archive at SHA-256
`56052b71c944922f58cf89e6a65e317e89c10e7a4cbaec23aa37c098ca3e490e`
and image ID `sha256:3967715d…2704`. Both archives loaded and returned exact
Linux/ARM64, non-root user, work directory, source and version readback. Replacement
r4 evidence closed the initial review's missing fresh-clone provenance and exact
image-ID cleanup proof; same-reviewer Sol/high review returned `ACCEPT` with no
remaining P1/P2 findings. The exact target tag, private cohort tags, matching
containers and image ID were removed; the guest work root is absent, the guest is
stopped and the shared lock is released. This receipt closes only the observed
image-byte reproducibility and load/readback gap; it does not itself prove TLS,
Compose lifecycle, update/rollback, backup/restore, failed-candidate recovery,
clean-host removal, complete F6 or F7. A separate later lifecycle slice advances
part of that boundary below; see
`docs/development/f6-arm64-container-image-reproducibility-2026-09-20-r1.json`.

The accepted image then passed a fresh bounded native ARM64 Docker Compose lifecycle
without another build or pull. A fresh official source clone supplied the shipped
Compose/configuration inputs, while the exact accepted archive was loaded once and all
starts used `--no-build`. The r2 cohort passed idempotent volume initialization,
pristine first service start, strict positive TLS plus wrong-name rejection, non-root
read-only/capability-free hardening, explicit restart identity continuity, verified
scoped data backup and separate-root restore, recovery from an invalid-command
candidate with a byte-identical primary volume, and data-preserving Compose removal
and recreation. The Hub installation identity remained
`d0798d3d-0394-4092-b67c-393a1a19e6f8`. The first calibration cohort receives zero
credit because it incorrectly used a CA certificate as the leaf; r2 used a separate
private CA and CA:FALSE leaf before its first service start.

The 69-file r2 evidence bundle is byte-identical across two deterministic packaging
passes at SHA-256
`8e2e6c084e684eef949281ba167c66516b0c103b483d862769168c527736da48`;
its 68-entry payload manifest is
`d20a01e4ab5b2000012a930915c90f1ee86d14900638734ffa79ce15d6ef01b6`.
Independent Sol/high review recomputed every payload and archive byte, rescanned the
retained boundary and returned `ACCEPT` with no P1/P2 findings. Cleanup removed the
exact root, volume, containers, network, three tags, image ID and listeners; the guest
is stopped, SSH is closed, the lock is released and the accepted host artifact is
unchanged. This advances only the synthetic no-credential portions of HUB-01, HUB-05,
HUB-06, HUB-08 persistence and HUB-11 data backup/restore. The scoped backup explicitly
excludes credentials, pairing authority, keys, TLS, configuration and service state;
the invalid-command candidate is not a genuine version predecessor. Genuine immutable
update/rollback, credential/pairing recovery, real collection, retention/migration,
complete clean-host recovery, ordinary public production admission, F1, F6 and F7
remain open; see
`docs/development/f6-arm64-container-lifecycle-2026-09-20-r1.json`.

## Start and boundaries

The sent full-solution goal authorizes bounded implementation, tests, isolated
runtime/VM work, validated source commits/pushes and exact catalog updates. It does
not authorize CI, releases, tags, binary publication, signing, production or real
Tesla activity.

Preserve the independent dirty `main` checkout and immutable G0–G7 evidence. Hub owns
shared runtimes, fixtures, installers and integration orchestration. Exclude App,
Viewer, x86/amd64/Intel and Azure. Do not inspect private closed runtime roots or reuse
credentials, endpoints, CAs, invitations, source exports, captures or receipts.

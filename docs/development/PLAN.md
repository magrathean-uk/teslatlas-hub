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
yet been rebuilt from a pushed commit.

The active companion bootstrap model now contains exactly Protocol, TypeScript SDK,
Swift SDK, Home Assistant and Edge. Hub source identity is recorded separately, making
six repositories in the full cohort. The shipped current catalog remains deliberately
empty until all five companion source commits, source manifests and artifact identities
are stable and independently reviewed. Historical D1 records, including their Viewer
entry, and Viewer R1 fixtures are preserved byte-for-byte as inactive evidence; they
are not catalog inputs. The current TypeScript
archive identity is recorded for future admission, but its exact bytes are not staged in
this checkout, so installed admission remains pending.

The accepting Sol/high delta review found no remaining issues. It verified the
baseline-identical historical D1 manifest at SHA-256
`37b6fc64fd804813d82053c7cf8d12e89cb9ebfc69ce569c4d1c84d645bed0aa`,
JSON Schema/runtime parser agreement, and finalizer binding of a bound source commit to
the tested Hub cohort head. Exact clean-export native/container artifacts, ARM64
container runtime source proof, final catalog population and installed companion
lifecycle remain open F6 work.

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

Current L3 foundation checkpoint: source-identity enforcement, exact five-companion
catalog generation/readback, Viewer-free active selectors, Hub-only `--components none`
bootstrap, and native/container packaging inputs are implemented and focused-tested.
The source/catalog foundation is independently accepted and published. Exact
clean-export native/container rebuilds, final five-companion catalog population and
installed lifecycle proof remain pending. Release, tag and binary publication remain
outside this slice. F6 is open.

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
That final narrow review returned `ACCEPT` with no
findings for the bounded source/static correction only. The corrected source must be
rebuilt from this publication's exact export and exercised in the ordinary sequence with
the retained bundle. Container
upgrade, rollback, backup/restore, failed candidate, removal, image-byte
reproducibility, F1, F6 and F7 remain open.

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
is stopped. Image-byte reproducibility, pristine first-attempt TLS startup,
clean-host package lifecycle, upgrade/rollback, backup/restore, failed candidate,
removal, F1, F6 and F7 remain open.

## Start and boundaries

The sent full-solution goal authorizes bounded implementation, tests, isolated
runtime/VM work, validated source commits/pushes and exact catalog updates. It does
not authorize CI, releases, tags, binary publication, signing, production or real
Tesla activity.

Preserve the independent dirty `main` checkout and immutable G0–G7 evidence. Hub owns
shared runtimes, fixtures, installers and integration orchestration. Exclude App,
Viewer, x86/amd64/Intel and Azure. Do not inspect private closed runtime roots or reuse
credentials, endpoints, CAs, invitations, source exports, captures or receipts.

# Lean development environment

Revision: 2026-09-18. Owner authorizes minimal replacement VMs; prior no-new-VM
instructions are superseded. Hub owns provisioning and runtime changes.

## Current measured state

The previous Debian/Mac VMs were deleted by the owner and none of their identities
was reused. G0 created one replacement `debian13-arm64` guest from the pinned recipe;
its accepted stopped instance occupied about 1.22 GB of host blocks and its retained
Lima image cache about 1.38 GB. No Mac guest exists. The empty Tart state target is
present only so the shared status wrapper can report an empty inventory. Current
access, platform and restart evidence is in [VM_ACCESS.md](VM_ACCESS.md).

The G0 post-provision check found about 179.6 GB free on the host data volume. The
earlier inventory found lab usage of about 26 GiB: build 18 GiB, candidates 3.5 GiB,
cache 1.9 GiB, runtime fixtures 1.6 GiB, tooling 317 MiB and toolchains 336 MiB.
Figures are rounded observations, not a quota or promise of reclaimable space.
Shared Rust tools and all protected source/evidence remain retained.

## Provisioning policy

- One shared Debian 13 ARM64 VM first: start with the existing 4 CPU / 4 GiB RAM /
  32 GiB sparse-disk recipe. Target <=16 GiB actual host allocation initially.
- One Apple-silicon Mac guest only when the next gate needs isolation or Hub's macOS
  13 support floor. Reuse the existing 4 CPU / 4 GiB / 50 GB sparse/COW recipe if
  compatible with the verified image. Target <=30 GiB actual allocation initially.
- Budget <=50 GiB combined new VM disks, retained base images and VM download cache.
  These are proposed working budgets chosen for this reset; measure actual allocated
  blocks, deduplicated/shared images and peak download needs before installation.
  Do not silently expand past the budget: reclaim reproducible outputs or present the
  measured need and a revised bounded choice.
- Keep >=30 GiB host free after the projected operation. At most one heavy build or
  VM install at once using `lab/locks/heavy-build`. No duplicate per-product guests,
  extra Colima/Docker-desktop VM, unbounded snapshots or automatic VM startup.
- Share the Debian guest with HA when its turn arrives. Use the existing host Mac
  for supported builds/Swift consumers. Swift's macOS 14+ floor cannot be tested in
  the macOS 13 Hub-floor guest; document each actual platform separately.
- Prefer transferring pinned candidate artifacts over installing full compiler/SDK
  stacks into every guest. Install only tools required by the selected test.

Validate current image availability/digests and VM-tool behavior through official
sources or pinned local recipes before downloading. Use existing `scripts/dev/vm.sh`,
`debian13-arm64.yaml` and `run.sh`; repair their stale paths as an owned change if
necessary. No host source mounts or forwarded agent/production credentials by default.
Provisioning is complete only after actual OS/architecture, SSH host keys, toolchain,
free space, stop/start and owner are recorded in VM_ACCESS.md. A boot is not product acceptance.

After a test, stop idle guests and remove only verified reproducible staged outputs.
Delete a disposable guest clone only after its data/evidence retention decision is
recorded. Keep at most one necessary pinned base; do not retain duplicate full images.
Do not assume sparse capacity equals physical disk usage.

## Cleanup queue

Owner-authorized cleanup is limited to proven reproducible, inactive output. Inventory
exact canonical paths, symlinks, writer/process ownership and evidence references first.
Record before/after allocated bytes and each deletion. Never use blanket `git clean`,
whole-lab deletion or broad cache pruning that can touch App or shared tooling.

| Candidate under `/Users/bolyki/dev/teslatlas-lab/` | Observed size | Treatment |
| --- | --- | --- |
| `build/hub/target/debug/incremental` | 2.8 GiB | First cleanup candidate after no active Cargo/writer and canonical path check |
| `build/teslatlas-edge/target/debug/incremental` | 411 MiB | Same check |
| `build/hub/target/debug/deps` | 4.2 GiB | Remove only obsolete reproducible outputs; preserve current candidate requirements |
| `build/teslatlas-edge/target/debug/deps` | 1.0 GiB | Same check |
| `build/teslatlas-sdk-swift/.build/out` | 865 MiB | Review derivation/references first |
| `build/teslatlas-sdk-swift/.build/ios-runtime-host` | 433 MiB | Preserve pending ownership check; may relate to separately scoped App tooling |
| `cache/lima/download` | 1.9 GiB | Inspect manifests; retain only the one reusable required base, avoid redownload churn |
| `candidates/`, `runtime-fixtures/` | 5.1 GiB combined | Preserve: packages/provenance/private state may be unique; no bulk deletion |

Preserve all source, Git storage (`repository-roots` included), dirty changes, receipts,
private access/runtime roots, databases, unique baselines, App/Viewer material and shared
pinned tools. Do not inspect protected contents just to increase a cleanup total.

The new coordinator completed the first provisioning slice without deleting a cleanup
candidate. G0 stayed below the VM budget, so no build/cache deletion was justified.
Future cleanup still requires the exact inventory and ownership checks above. Save a
compact result in TASKS.json or a single evidence receipt; do not generate a report per file.

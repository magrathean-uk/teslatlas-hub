# Development environment and storage

> Scope update 2026-09-12: Viewer development, planning and acceptance are excluded by owner. Existing Viewer paths and receipts below are preserved historical context, not active work.


> **Active scope, 2026-09-09:** use the existing Debian ARM64 and Apple-silicon Mac environments to deliver working products. All x86/amd64/Intel Mac development, builds, provisioning and runtime/acceptance checks are paused under [ACTIVE_SCOPE.md](ACTIVE_SCOPE.md). The fresh `debian13-x86_64-20260909` VM and its unadmitted HA/Hub artifacts remain preserved; no restart, shutdown, cleanup or further inspection is requested. This supersedes historical statements below about which VM directories exist. Hub owns shared runtime changes, and every host/guest heavy build or VM install requires the shared lock before starting.

> Runtime update (2026-09-08 hourly check): the original clean-OS measurements below are a preparation snapshot. Hub is now installed and active on the primary Debian guest, and Viewer owns a disposable browser fixture. Read the current Hub/product STATUS.json and handoff before changing guest services or assuming the VM is empty.


Prepared and consolidated 2026-09-08; this includes the owner's follow-up for Lima/Colima cleanup and usable VM logins. See [VM_ACCESS.md](VM_ACCESS.md) for the current access details. This is environment readiness, not product installation or acceptance. Product development is authorized under [START_AUTHORIZATION.md](START_AUTHORIZATION.md).

## Visible storage

| Location | Purpose |
| --- | --- |
| `~/dev/teslatlas-lab/build/<product>/` | Product compiler output, derived data and future disposable build environments |
| `~/dev/teslatlas-lab/cache/` | npm, Python, uv and Go caches used through the common wrapper |
| `~/dev/teslatlas-lab/tmp/` | Temporary work for wrapped commands |
| `~/dev/teslatlas-lab/logs/` | Development command/VM logs |
| `~/dev/teslatlas-lab/evidence/` | Private environment receipts, cleanup and relocation records |
| `~/dev/teslatlas-lab/vms/lima/` | Active `debian13-arm64` development VM |
| `~/dev/teslatlas-lab/vms/colima/` | Colima configuration only; no active VM or data disk |
| `~/dev/teslatlas-lab/vms/retained/` | Offline historical baseline/quarantine/manual disks |
| `~/dev/teslatlas-lab/access/` | Private SSH key, pinned host keys and GUI login file |
| `~/dev/teslatlas-lab/tooling/tart-2.36.0/` | Verified VM control tool |
| `~/dev/teslatlas-lab/vms/tart/` | macOS VMs and the retained pinned base image |
| `~/dev/toolchains/rust/cargo/` | Shared Cargo home and existing installed Cargo utilities |
| `~/dev/toolchains/rust/rustup/` | Shared Rust toolchains, including preserved App 1.97.0 and Hub/Edge 1.98.0 |

`~/.cargo`, `~/.rustup`, `~/.lima`, `~/.colima` and `~/.tart` are now compatibility symlinks to those visible locations. Lima and Colima download caches formerly under `~/Library/Caches` also link into the lab cache. Their bulk data is under `~/dev`. Hub/Edge `target` and Swift SDK `.build` likewise point into the lab. This preserves existing scripts that use fixed repository output paths. No App source or App build script was changed, and the pinned App compiler still responds after relocation.

Old Debian/macOS baselines, the quarantined ARM64 guest and the opaque manual Debian VM are now offline under `vms/retained`. Same-filesystem moves preserved file identities without booting or rewriting their disks. Their former receipt paths remain historical; later reuse needs identity/quarantine reconciliation. The default Lima home now lists only the fresh primary Debian guest. There was no second directory at `~/dev/source/teslatlas-lab`.

The old artifact-tree Tart executable disappeared during this follow-up. Tart 2.36.0 was retrieved from its official release, SHA-256 verified and installed in the lab tooling directory; the wrapper now uses that location. Do not assume all prior artifact-tree tools or receipts still exist. In particular, a later product task must verify or re-provision its exact pinned Node tool under the lab before reusing old package evidence.

## Use the existing scripts through one entry point

From the workspace root, use:

```sh
scripts/dev/run.sh hub cargo check --locked
scripts/dev/run.sh hub bash scripts/build-macos-app.sh --help
scripts/dev/run.sh teslatlas-edge cargo check --locked
scripts/dev/run.sh teslatlas-sdk-swift swift test
scripts/dev/run.sh teslatlas-sdk-typescript npm run build
scripts/dev/storage.sh
```

These are development examples for use after owner start. Only wrapper syntax, output routing and compiler version queries were exercised during preparation; product builds/tests were not run.

`run.sh` sets the shared Rust homes, per-product Cargo output, npm/Python/Go caches and temporary directory. Cargo uses the installed 1.98.0 toolchain explicitly. Incremental compilation defaults to off to prevent the previous large incremental-cache growth; builds default to four jobs. `TESLATLAS_DERIVED_DATA` names the per-product Xcode output location for scripts that accept it. Existing Hub Xcode paths already resolve through its relocated `target` link.

Preserve each product's exact toolchain recipe: the wrapper does not silently replace the recorded Node 26.7.0 package-build tool with ambient Node 26.8.1, choose a Python interpreter, or add missing Swift/HA tooling. Product owners select their declared tools explicitly when development starts. Tart is verified in the lab. Reuse a matching pinned Node tool if present; otherwise provision one shared copy under lab/tooling, not one copy per task.

New evidence/build scripts must accept the common lab prefix and reuse existing `.sh` entry points. When a script hard-codes a new cache outside the lab, change that output option as part of its owned development task; do not rewrite historical receipt paths. Runtime-owned state belongs inside its VM or selected service prefix, not in a source checkout. Keep generated credentials private and out of Git.

## Running primary guests

| Guest | Verified system | Resources | Role |
| --- | --- | --- | --- |
| `debian13-arm64` | Debian GNU/Linux 13.6, `aarch64`, systemd state `running` | 4 vCPUs, 4 GiB RAM, 32 GiB sparse disk; about 30 GiB guest free | First bootstrap/integration environment |
| `macos13-arm64` | macOS 13.7.4, build 22H420, `arm64` | 4 vCPUs, 4 GiB RAM, 50 GB sparse/copy-on-write disk | Existing minimum macOS target; later native package acceptance |

Both guests were running and accessible through the dedicated SSH key when verified. Debian is `bolyki@127.0.0.1:60022`; macOS is `admin` at its current Tart-reported NAT address. The Mac GUI password is stored in private `~/dev/teslatlas-lab/access/credentials.json`. Use [VM_ACCESS.md](VM_ACCESS.md) for complete verified instructions. At initial preparation these guests had no Teslatlas products or build environment. That is a historical measurement: Debian has since hosted active Hub/helper development and the Mac state must be inspected before a new handoff. Never assume either guest is empty. Hub owns bounded provisioning for the current working-product step. The macOS guest was cloned locally from the already retained pinned Ventura base; no new macOS image download was needed.

Debian uses the pinned Debian cloud image and digest in [debian13-arm64.yaml](../../scripts/dev/debian13-arm64.yaml). It has no host-directory mounts, no forwarded SSH agent, and no automatic application-port forwards. Its only routine host connection is managed SSH. Future browser/product access must be explicitly configured for the isolated test path.

macOS runs with no graphics window, no audio, no clipboard and no host shares. It uses ordinary NAT; that is not a network-isolation guarantee. Do not put production credentials in it. The Tart image digest is `sha256:9fb387bb987cdda942fbc27118296ed8e16a3359a672c95c6d5ac960165ef1c5`; Tart 2.36.0 is the retained verified tool.

```sh
scripts/dev/vm.sh status
scripts/dev/vm.sh debian ssh uname -m
scripts/dev/vm.sh mac ssh sw_vers
scripts/dev/vm.sh debian stop
scripts/dev/vm.sh debian start
scripts/dev/vm.sh mac stop
scripts/dev/vm.sh mac start
```

The Mac start command remains attached to the terminal while the guest runs. Use a dedicated terminal if starting it manually. The primary VM controls operate only on `debian13-arm64` and `macos13-arm64`; they do not start or retire historical guests. Do not run several installations/builds concurrently. Stop the primary guests when the owner no longer needs them; do not schedule automatic launches.

Fresh guest names and moved paths require new runtime identities/leases in later installed acceptance. Old source-bound receipts are historical; they cannot simply be renamed to count as acceptance for these guests.

## Completed cleanup

- Removed 40 verified Cargo compiler-output directories beneath the Hub/Edge build roots: incremental caches, compiled dependency outputs, build intermediates, fingerprints and generated examples. Their pre-deletion allocated sizes totalled approximately 43.35 GiB. Final package binaries, screenshots, source inputs and retained receipts were not blanket-deleted.
- Removed stopped disposable `teslatlas-matrix-browser-20260905` and `teslatlas-matrix-debian13-amd64-20260905` guests after checking their preparation-only role, no active process/lease, and no unique original upgrade baseline. The cleanup decision is recorded in the lab; verify any older runtime-receipt path still exists before reusing it.
- Removed the unused 701 MiB `~/dev/VM/debian-13.6.0-arm64-netinst.iso`. The retained manual VM's boot script does not attach it; its installed disk and firmware remain.
- Retired both old Colima profiles and data disks: 60 GiB `interop-20260905` and 35 GiB `hub-v1-arm64`. The active engine had six stopped test/build containers, zero running containers, zero host mounts and zero named volumes; a read-only layer inspection found no unique user dataset. Image/compiler caches were regenerable. No Colima VM remains. Its unused 317 MiB download cache was also removed. Future Docker work provisions the primary 32 GiB Debian guest.
- Moved three stopped legacy Lima guests, two stopped legacy macOS guests and the manual opaque Debian VM into `vms/retained`. Preserved their original files and kept them outside active development VM listings. The pinned macOS base remains available for clean cloning.
- Moved Lima/Colima state and download-cache directories into the lab with compatibility links. Colima fallback template capacity is 32 GiB if a distinct later lane is explicitly needed. No replacement Colima VM was created.

The first planning cleanup observed 54 → 115 GiB free. In this follow-up, the host reported about **299 GiB free immediately before Colima retirement, 342 GiB afterward, and 343 GiB after clearing its remaining download cache**. Other cleanup happened between those measurements and the earlier planning run, so do not attribute the entire combined change to these VM operations. APFS shared blocks and sparse disks mean virtual capacities do not sum to physical recovery. No Time Machine/APFS snapshots were deleted by this task.

Private receipts are `evidence/relocation-2026-09-08.json`, `cleanup-completed-2026-09-08.json`, `retired-vms-2026-09-08-completed.json`, `removed-installer-2026-09-08.json` and `vm-readiness-2026-09-08.json` under the lab. [Storage retention assessment](assessments/storage-retention.md) explains the retained originals. That assessment is historical. Current retained paths are in [VM_ACCESS.md](VM_ACCESS.md). Follow-up receipts are `vm-consolidation-lima-2026-09-08.json`, `vm-consolidation-retained-2026-09-08.json`, `colima-retirement-2026-09-08/` and `vm-access-2026-09-08.json`.

## Resource policy after start

Keep one active candidate per product/toolchain/target, one primary Debian guest and one minimum-macOS guest. Reuse downloaded tools and dependencies. Before a large build or image acquisition, inspect available storage; if under 30 GiB, first prune verified stale compiler intermediates. Preserve release/acceptance evidence separately from disposable build output. Do not delete whole `target`, artifact or VM roots merely because their names sound temporary.

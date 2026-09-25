> OWNER RESTART — 2026-09-18: The owner explicitly resumed development on the six in-scope products and ecosystem orchestration. Do not use fast mode. Resume Hub with Sol/high and Protocol, TypeScript SDK, Swift SDK, Home Assistant and Edge with Luna/max from preserved checkpoints. Viewer remains excluded and all x86/amd64/Intel work remains paused. The App keeps its separately authorized scope.

> APP-ONLY RESTART EXCEPTION — 2026-09-12T16:53:09.669326+00:00: The App task reports the owner directly authorized continued App work. App v6, its localization workers and its own heartbeat may resume under that task. Hub, Protocol, TypeScript SDK, Swift SDK, Home Assistant, Edge and ecosystem coordination remain paused until explicit owner restart. Viewer remains excluded; x86 remains paused.

> OWNER PAUSE — 2026-09-12T16:51:03.456724+00:00: The owner paused this work and all development on this project. Stop development, reviews, tests, builds, runtime preparation, delegation and polling until explicit owner restart. This supersedes earlier restart and standing execution authority, including r11. Preserve dirty source, evidence and goals. Hourly coordination is PAUSED. Viewer remains excluded; x86 remains paused.

# Teslatlas service workspace authority

> Scope update 2026-09-12: Viewer development, planning and acceptance are excluded by owner. Existing Viewer paths and receipts below are preserved historical context, not active work.


Status: active
Date: 2026-09-05

## Only development roots

| Product | Path | Branch | Current local tip |
| --- | --- | --- | --- |
| Hub | `/Users/bolyki/dev/source/teslatlas-service/hub` | `main` | `dc12ee427fd44889e730c1e5e21c2184893b0f79` plus the preserved working tree |
| App | `/Users/bolyki/dev/source/teslatlas-service/app` | `main` | `a00c751ed20ab7af972670b8f6cde667b5fddddb` plus authority documentation edits |

The table above records the historical 2026-08-14 reconciliation tips, not
current HEAD values. Reinspect Git before editing.

On 2026-09-05 the owner authorized implementation of the Hub ecosystem plan.
Additional existing `main` development roots are `teslatlas-protocol/`,
`teslatlas-sdk-typescript/`, `teslatlas-sdk-swift/`, `teslatlas-viewer/`,
`teslatlas-home-assistant/`, and `teslatlas-edge/`. Each remains an independent
repository. The App is preservation/regression scope for this objective.

All new work goes directly to these existing `main` checkouts. Do not create a
branch, worktree, detached candidate, Fleet clone, release branch, or alternate
development root.

## Historical material

`repository-roots`, `fleet-architecture-maps-v1`,
`prep-v16-audit-sol.eWGGrF`, `app-ui-population-proof`, and other isolated
candidates are not development roots. They are read-only inputs. A useful
change must be reviewed and ported path-by-path into the active `main`; no
detached completion claim becomes product truth by itself.

`repository-roots` must remain: it contains the Git common directories backing
the active `app` and `hub` worktrees. Its name is historical, but its Git
storage is live.

Do not bulk-delete branches or worktrees. Several retain unique or uncommitted
history. Cleanup requires an inventory and owner decision.

## Truth and proof

- Source truth is the existing independent `main` working trees above. Recheck HEAD and dirty state before editing.
- Current programme authority is `docs/development/MASTER_PLAN.md` and `docs/development/PRODUCT_SPEC.md`; each product has one active `docs/development/PLAN.md`.
- The 2026-09-09 owner direction in `docs/development/ACTIVE_SCOPE.md` prioritizes working Debian ARM64 and Apple-silicon Mac products across all seven tasks. All x86/amd64/Intel Mac development and acceptance is paused; preserve existing x86 state without further action. Missing named-source parity input is not a blocker for independent usability work. Polished distribution follows ordinary use. Hub is the main product; companions are optional; App is excluded.
- On 2026-09-08 the owner started all seven product tasks: Hub uses GPT-5.6 Terra/XHigh; companions use GPT-5.6 Luna/Max. On 2026-09-09 the owner deleted the old native goals and authorized one fresh ARM64/Mac working-product goal per existing task. Verify native state before and after creation and follow docs/development/START_AUTHORIZATION.md. Hourly coordination is authorized.
- Current task state is `docs/development/TASKS.json`. Historical compatibility evidence and pending counters remain in `hub/docs/compatibility/execution-state.json` and do not prove the first bootstrap works.
- New selectable component installers are authorized planning scope and supersede the old prohibition on separate companion macOS packages/embedded payloads. Core Hub must remain independent.
- Superseded operational plans now redirect to the canonical plans; their original bytes are indexed in `docs/development/SUPERSEDED_PLANS.md`. Wire specifications and original receipts remain evidence, not competing execution plans.
- The coordinator is authorized to prepare the two local ARM64 development VMs and remove verified disposable development caches/images. Preserve source, Git storage, dirty work, unique data, original upgrade baselines and the quarantined guest.
- Canonical development guests are `debian13-arm64` (32 GiB) and `macos13-arm64` (50 GB); use `docs/development/VM_ACCESS.md` and the shared VM wrapper. Lima/Colima/Tart state and caches reside under the lab. Colima profiles are retired; historical guests are offline in `vms/retained`, not current development targets.
- Development outputs live under `~/dev/teslatlas-lab`; shared Rust homes under `~/dev/toolchains/rust`. Use `scripts/dev/run.sh` and the documented VM controls. Compatibility symlinks preserve existing build scripts and App toolchain paths.
- GitHub is source storage only. No CI, releases, tags, artifact upload, commit or push without an explicit instruction.

## Historical 2026-08-14 reconciliation

- App was fast-forwarded without content conflict from
  `feature/app-local-intelligence` to local `main`; the old branch still names
  the same commit and is not a development target. Current App working-tree
  changes are authority documentation only.
- Hub remains on `main`. Its broad working tree is preserved while changes are
  reviewed and integrated there; no reset, clean, stash, or candidate checkout
  is allowed.
- On 2026-08-14 the owner authorized workspace cleanup. Six App audit roots,
  two obsolete receipt Fleet roots, two rejected Grok candidates, superseded
  Grok goals, and empty top-level roots were moved to Trash. Fifteen clean,
  patch-equivalent `app/diff` worktrees were removed through Git; their branch
  refs remain. These actions are recoverable until Trash is emptied.
- `prep-v16-audit-sol.eWGGrF` remains because it contains unique schema-2.2
  preparation, `fleet-architecture-maps-v1` remains because it contains unique
  request maps, and `app-ui-population-proof` remains because another process
  currently holds that registered worktree.

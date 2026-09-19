# Next programme — installed lifecycle and reproducible handoff

State: **DRAFT_NOT_STARTED**. Revision: 2026-09-19.
Launch only when the owner sends [GOAL_PROMPT.md](GOAL_PROMPT.md).

Preserve APP_V7_ADOPTION_READY and secondary HA acceptance. Close the gaps between
the tested source/synthetic ecosystem and ordinary self-hosted installation on
Debian ARM64 and Apple silicon. Local packages are development artifacts; no release,
signing, notarization, CI or production deployment is authorized by this draft.

## Work packages

| ID | Owner/dependency | Acceptance |
| --- | --- | --- |
| L1 | Hub; shared Debian guest, exact artifacts and known predecessor for upgrades | Ordinary Hub package installation and service ownership/permissions; pair/current/history; restart and upgrade retain identity/config/data; backup restores; supported rollback succeeds or documented migration refusal preserves data; uninstall preserves data. Then optional Edge lifecycle, including encrypted pending spool and durable Hub state. |
| L2 | Hub + Swift; one suitable Apple-silicon guest and pinned tooling | Equivalent supported Mac installed lifecycle, with separate sequential macOS 13 Hub and macOS 14 Swift receipts using at most one Mac guest at a time. Both floor assertions are required for L2 acceptance. Record precise image/hardware/toolchain gaps; macOS 27 is not floor acceptance. |
| L3 | Product owners; exact package/source inputs | Reproduce local source exports and package consumption outside working trees; verify manifests/version/profile locks, docs and optional components. Hub works alone. Repeat only newly affected Node/browser/Swift/HA assertions. HA replacement-upgrade retains the entry/devices/entities and credential ownership; prove supported rollback or safe refusal. |
| D1 | Hub; fresh owner-supplied named read-only export and provenance | Compare imported rows and public current/history against source; preserve null/zero/units/timestamps and document exclusions. Synthetic harness work cannot accept D1. Do not inspect historical excluded fixtures or use Tesla credentials. |

L1–L3 are the next execution scope. D1 is input-dependent: continue independent work
while it waits. Broader HACS/HA OS/Supervisor, iOS runtime, architecture matrices, real
passive collection and polished distribution remain separate future scope.

## First executable slice

1. Verify current Hub writer and heavy-lock ownership; inspect maintained package
   scripts and source identity.
2. Define one Debian native Hub install/service journey. Identify an exact retained
   predecessor and migrations before claiming upgrade/rollback. If unavailable, do
   fresh-install/restart/backup/removal and record that missing baseline separately.
3. Build once under the lock, start the retained guest, create a fresh private
   synthetic runtime, and use ordinary install/setup/pair/read/restart paths.
4. Repair observed defects, verify changed behavior and remaining lifecycle assertions.
   Capture one redacted receipt with source/package hashes; stop runtime/guest and
   verify cleanup.

Do not begin with a full matrix rerun, speculative contract design or more planning.
Dispatch SDK/Protocol/HA workers only for concrete ready dependencies. Default to two
useful bounded workers; no persistent product tasks.

## Evidence and boundaries

Receipts name OS/architecture, artifact hashes, ordinary user route, identity/data
assertions, reviewer and cleanup. Distinguish source, package, installed service,
synthetic and named-source proof. Health, ACK, package build or bootstrap is insufficient.

Sol/max coordinates, Sol/high implements/reviews, Luna/max explores; no fast mode or
silent substitution. One writer per repo; Hub owns VMs; one heavy job under the lock.
Use the existing disk policy: at most one Debian and one needed Mac guest. Fresh
one-use handoffs only. Preserve dirty source, original receipts, Git storage and tags.

No App, Viewer, x86/amd64/Intel/Azure, production, vehicle actions or old cohort reuse.
No further commit/push after the one-off upload without a later request. Keep automation
paused. Finish only when L1–L3 pass; if an external input blocks a required assertion,
complete independent work and report that exact blocker without marking it accepted
or the goal complete. Record D1's missing named input separately.

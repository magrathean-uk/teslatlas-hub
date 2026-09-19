# Home Assistant post-adoption plan — 2026-09-19

Objective: Preserve the accepted Debian ARM64 Container integration and close
the first replacement upgrade/rollback gap.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[coordination](../../../docs/development/COORDINATION.md), and
[STATUS.json](STATUS.json).

## Current position

The secondary working-product objective is accepted on Debian 13.6 ARM64 with
official Home Assistant Container 2026.8.3. The fresh lane proved authenticated
config flow, one-use pairing, two devices/26 entities, scheduled refresh,
zero/unknown/unavailable semantics, Hub outage/restart recovery, same-entry
reauthentication, reload, reconfigure, disable/re-enable, Container stop/start,
diagnostics redaction and cleanup.

The receipt is source-built synthetic Container evidence. It does not prove a
headed visual flow, component replacement upgrade/rollback, Home Assistant OS
or Supervisor, HACS/publication, real data, named-source parity or production.
Repeat the accepted lane only for a component/profile/Hub/image/behavior delta.

## Next goal draft — not started

L3: deliver one Debian ARM64 Container component replacement lifecycle receipt.
Freeze the accepted baseline component, replacement candidate, profile, Hub
fingerprint, HA image and persistent config identity before execution. Stage
through the bounded Hub-owned selector into an explicitly selected HA config,
verify successful replacement, then inject one fail-closed invalid candidate
and prove rollback to the last working component without losing the existing
config entry, device/entity registry or unrelated HA state.

Acceptance requires:

- exact baseline/candidate payload manifests and HA/Hub/profile identities;
- pre/post config-entry, device and entity identity/count comparison;
- successful replacement with scheduler, reauth and reload still working;
- failed candidate never becoming active, rollback restoring the prior working
  component, and unrelated HA storage fingerprints preserved;
- Container/Hub stop, private-root removal and no retained credentials,
  listeners or heavy-build lock.

This draft does not authorize source changes, Container/image work, guest start,
runtime, tests, HACS submission, or publication. The coordinator must create and
start a new goal.

## Later work

Headed visual acceptance, Home Assistant OS/Supervisor, HACS distribution,
broader fault/parity coverage, fresh-owner-input named-source/real-data behavior
and production remain separate lanes. HA remains an integration in a selected
runtime, never a native macOS daemon.

## Boundaries

Preserve the dirty `main` checkout and existing HA config/database/entity
state. Hub owns shared fixtures and aggregate installers. No App or Viewer work,
x86/Intel/Azure, production or vehicle action, commit, push, CI, release,
publication, or reuse of closed private inputs.

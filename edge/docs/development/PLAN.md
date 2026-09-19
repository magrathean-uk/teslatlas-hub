# Edge post-adoption plan — 2026-09-19

Objective: Preserve accepted durable forwarding and prepare the first installed
Edge/receiver lifecycle on Debian ARM64.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[coordination](../../../docs/development/COORDINATION.md),
[App v7 handoff](../../../docs/development/APP_V7_READINESS.md), and
[STATUS.json](STATUS.json).

## Current position

G3 and G5 are accepted for product `2026.36.2` and
`edge-delivery-v2@2.0.0`. The fresh Debian 13 ARM64 r5 cohort proved one
guarded synthetic event through receiver, encrypted spool and Hub commit; exact
ACK occurrence, duplicate safety, outage/restart recovery, readable projection,
Hub restart durability and cleanup passed.

The accepted lane used source-built synthetic processes. It is not Debian
package, systemd, Apple package, Docker lifecycle, real telemetry or production
acceptance. Do not replay r5 or reuse its credentials, roots or identities.

## Next goal draft — not started

L1: deliver one Debian 13 ARM64 installed lifecycle receipt for Edge core and
the pinned Fleet Telemetry receiver after the installed Hub baseline is ready.
Freeze previous-working and candidate packages, binaries, legal/lock inputs,
configuration, systemd units, profile and target. Install the baseline, upgrade
to the candidate, confirm service ordering and one new bounded synthetic
delivery, exercise one fail-closed candidate/rollback path, then remove package
code and services while preserving spool/configuration by default. Do not purge.

Acceptance requires:

- package/payload hashes, `dpkg-query`, architecture and actual service units;
- separate owner-only receiver, Edge and Hub credentials/configuration with
  correct modes, users, listeners and loopback admission;
- one occurrence-bound committed/readable event, stable spool identity through
  restart/upgrade, no duplicate projection and a drained frontier;
- rollback to the working baseline after the failed-candidate path;
- removal with owned services/listeners absent and retained spool/config
  fingerprints unchanged.

This draft does not authorize implementation, package build, installation,
guest start, runtime, event submission, or tests. The coordinator must create
and start a new goal.

## Later work

L2 covers Apple-silicon `.pkg`/LaunchAgent lifecycle. L3 covers reproducible
source-only package inputs and only changed recovery paths. ARM64 Compose, wider
spool fault coverage and separately authorized passive/real telemetry remain
later lanes. Real-data work requires fresh owner input and never an old fixture.

## Boundaries

Preserve the dirty `main` checkout. Hub owns shared fixtures and runtimes; Edge
owns its packages, receiver binding and spool. No App or Viewer work,
x86/Intel/Azure, production or vehicle action, commit, push, CI, release,
publication, public ingress, or reuse of closed cohorts.

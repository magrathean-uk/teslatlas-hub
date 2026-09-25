# Hub plan pointer

The Hub's plan is the workspace programme plan, `docs/development/MASTER_PLAN.md` at
the workspace root (`/Users/bolyki/dev/source/teslatlas-service`), written 2026-09-25.
The Hub's phases there:

- Phase 2: fully working on the current Mac, installer and LaunchAgent lifecycle
  included, collecting from the owner's car through the VPS owner token and the Fleet
  API, with the P1 fixes (efficiency units, loopback Host check, streaming backoff,
  sliding bearers with rotation grace, permanent no-op status, HA rotation) and
  terminal pairing for headless hosts.
- Phase 3: the same receipt inside the `macos13-lab` VM (cross-built, macOS 13 floor kept).
- Phase 4: v7 transport (snapshot by schema, changes-since, multi-chunk 2.2, larger
  packs) and shared compute through the `teslatlas-compute` crate.
- Phases 6 and 7: Debian 13 arm64, then x86_64 (the VPS).

`STATUS.json` records the current state. Everything under `archive/` is background
only; every VM receipt there predates the guests' deletion on 2026-09-22.

# Protocol full-product completion plan — 2026-09-19

Objective: complete the source-neutral Protocol as the reproducible authority for every
supported Hub, SDK, Edge and Home Assistant contract used by the finished ecosystem.

Authority: [master plan](../../../docs/development/MASTER_PLAN.md),
[product specification](../../../docs/development/PRODUCT_SPEC.md),
[coordination](../../../docs/development/COORDINATION.md), and [STATUS.json](STATUS.json).

## Current position

G3 remains accepted for the exact five-product current-Hub/Edge compatibility binding.
Keep its receipt immutable. It does not prove every rich profile, distributable bundle,
independent implementation, named-source semantics or final installed ecosystem.
`full_solution_state` is `NOT_ACCEPTED`. F0 passed independent review; F3/F6 are in
progress.

## Required completion

- **F0:** inventory every normative and machine-readable claim across rich semantic
  profiles `1.0.0`–`1.2.0`, `hub-http-v1@1.0.0`, `edge-delivery-v2@2.0.0`, schemas,
  OpenAPI, events/SSE, commands/metadata, examples, fixtures, compatibility policy,
  conformance cases and Python/Docker tooling. Conflicts block acceptance and must be
  resolved in every affected artifact and consumer binding.
- **F3:** prove clean independent adapters for Hub, TypeScript, Swift, Edge and HA use
  the exact public profile bytes and fail closed on unsupported versions, capabilities,
  malformed data, identity/trust errors, bounds and recovery cases. Preserve separate
  profile identities and the current-plus-two-minors compatibility policy where claimed.
- **F5:** turn the fresh named-source export and authorized passive capture into
  redacted semantic evidence for units, field presence, null/zero, timestamps, history,
  import and Edge dispositions. Update profiles only for real observed contract gaps.
  This input-dependent evidence is mandatory for full acceptance.
- **F6:** produce a deterministic, unpublished developer bundle containing all required
  schemas, OpenAPI, profiles, checksum maps, redacted fixtures, conformance tooling,
  licences and usable docs. Prove Python 3.11+ native checks, the documented ARM64
  Docker checker, clean offline extraction and Hub-catalog install/update/status/
  rollback/removal. No daemon or hidden Hub source dependency is allowed.
- **F7:** admit exact final product artifacts and run conformance/readback in the
  combined installed ecosystem without weakening validators to fit observations.

## Work slices

1. **L1:** complete F0 and close contract/artifact conflicts required by F3.
2. **L2:** prove independent consumer bindings and active platform/tool floors.
3. **L3:** finish F5 evidence, deterministic distribution/catalog/docs for F6, and
   final compatibility admission for F7.

## Current F3/F6 source foundation

The bounded native source slice passed independent Sol/high review. This accepts
only the native developer-bundle foundation, not full F3 or F6.
`tools/developer_bundle.py` builds a byte-deterministic, unpublished archive with
canonical gzip/tar metadata, a closed manifest, payload checksum map, exact
`uv.lock`, public schemas/OpenAPI/profiles, redacted fixtures, conformance tooling,
licence notices and top-level operator documentation. Verification rejects unsafe
paths, links, duplicate names, parent/file conflicts, extra or missing files,
renamed roots, non-canonical metadata, special or changed modes, content or lock
divergence, unknown or mutated manifest contracts, and all compressed or decoded
trailing input.

On macOS 27 arm64, Python 3.11.16 and uv 0.12.12, two builds produced identical
archive SHA-256 `6368e2f59303b8ed7623c7817492269fc869e77754909ab47bd4cc6aefb196ec`.
A fresh locked install seeded an isolated cache; deleting that environment and
reinstalling from the cache with `UV_OFFLINE=1` passed the complete 142-test gate
and all 31 conformance runs from the verified extracted archive. This proves the
native bundle foundation only.

## Linux ARM64 Docker candidate

The exact published commit `6686615c93a72c8c278391bdcc69e8a05853a970` and its
accepted archive SHA-256 `6368e2f5...96ec` were reproduced in the retained Debian 13
ARM64 guest. Its documented Docker image build succeeded natively, but the default
network-isolated checker collected 142 tests and ended with six errors because the
final image omitted `Dockerfile`, `THIRD-PARTY-NOTICES.md` and top-level `docs/`
required by the bundle self-tests.

A minimal dirty-tree Dockerfile correction copies those public bundle inputs. Two
candidate archive builds were byte-identical at SHA-256 `8ac5ccfa...b8be`; the native
`linux/arm64` candidate image ran as UID/GID 10001 with `UV_OFFLINE=1` and
`--network none`, passing 142/142 tests and 31/31 conformance runs. Independent
Sol/high review accepted this bounded dirty-tree candidate with no findings. It remains
candidate evidence only: a published source commit and clean reproduction from that
exported commit are required before accepting the ARM64 Docker slice.
Cold-cache offline dependency installation, remote-wire smoke, Hub catalog
install/update/status/rollback/removal, downstream final-product admission and the F5
real-input assertions remain open.

## Start and boundaries

The sent goal authorizes bounded contract work, generation, checks and validated
source commits/pushes. It does not authorize releases, tags, binary publication,
CI, production or private-input reuse. Preserve the dirty `main` tree and G3 receipt.
Protocol remains source-neutral and owns no service. Exclude App, Viewer,
x86/amd64/Intel and Azure; never include credentials or identifiable source/capture data.

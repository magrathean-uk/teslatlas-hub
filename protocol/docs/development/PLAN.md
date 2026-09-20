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
historical candidate evidence only.

The correction is now published at exact runtime-source commit
`53b5c6483990db84e5214176755f398e93d87b1b`. A clean git archive of that commit
reproduced bundle SHA-256 `8ac5ccfa...b8be`, built image
`sha256:721f2ad8...58095` natively on Linux ARM64 and passed the same UID/GID,
offline, network-none, 142/142-test and 31/31-conformance assertions. Independent
Sol/high review accepted the r2 exact-source ARM64 Docker slice with no findings,
strictly pinned to runtime-source commit `53b5c648...d87b1b`; full F3/F6 is not
accepted.
Cold-cache offline dependency installation, remote-wire smoke, Hub catalog
install/update/status/rollback/removal, downstream final-product admission and the F5
real-input assertions remain open.

The same exact runtime source then passed a separate current-source PRO-04 cache
provenance slice on macOS 27 ARM64. A fresh physical export and genuinely empty
task-owned `uv` cache/environment performed one locked Python 3.11 online seed,
installed 32 distributions and passed 142/142 tests plus 31/31 conformance. After
deleting the environment, `UV_OFFLINE=1` reinstalled from the exact seeded cache
under a macOS sandbox that denied remote IP networking while retaining only the
loopback and task-local Unix sockets required by synthetic checks; the same checker
passed and the developer bundle reproduced byte-for-byte at SHA-256
`8ac5ccfa03d8432284b7045e0ec93b66254897d9ec96a9a7d516fd112eb1b8be`.
The 1,606-entry cache manifest remained identical at SHA-256
`946223efa9759909e20bbdf5a8e43d9f160418e5ec37d8b32a8198fa3e6d5417`.
Independent Sol/high review returned `ACCEPT` with no P1/P2. This proves an empty-cache
online seed followed by exact cache-seeded offline reinstall/check/bundle reproduction;
it does not prove cold-cache offline installation because the bundle contains no
wheels, nor full PRO-04/F3/F6. See
`docs/development/f3-f6-pro04-current-source-cold-cache-offline-reinstall-2026-09-20-r1.json`.

## F6 current-source catalog handoff

The bounded source-only handoff is pinned to the accepted Docker runtime-source commit
`53b5c6483990db84e5214176755f398e93d87b1b`, tree
`931507b56c641d330d3a75513f3eb1f88f24e219`, and product `2026.36.2`. Two clean
exports were byte-identical. The tracked complete manifest binds all 188 regular-file
paths, hashes, sizes and modes; its aggregate is
`71ba5a6a740dfcc37620d71445c7a742757f721bde19129c4b83a63c93ef3948`.
Applying the Hub source-manifest algorithm includes 187 files, excludes only
`AGENTS.md`, and yields `source_sha256`
`a4b68800e7164c229915fced33223a7c9954bb67019306348e3e99671d28046c`.
The candidate binds `hub-http-v1@1.0.0` at
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`.

Review-base commit `e82ed205f728cff3cf1e2ceab0ceb40692c562d6` is one evidence-only
descendant: its delta contains only the plan, status and accepted r2 Docker receipt.
It was not the source executed by that accepted lane, and this metadata-only handoff
delta likewise does not replace `53b5c648...d87b1b` as the cohort source. The Hub
catalog schema forbids Protocol `artifacts`, so the handoff supplies only repository,
commit, source aggregate, product version and profile identity. This is a prepared
catalog input, not runtime,
catalog-lifecycle, F3, F6 or F7 acceptance; Hub recomputation/admission, remote-wire,
cold-cache, final-consumer and F5 evidence remain open.

## Start and boundaries

The sent goal authorizes bounded contract work, generation, checks and validated
source commits/pushes. It does not authorize releases, tags, binary publication,
CI, production or private-input reuse. Preserve the dirty `main` tree and G3 receipt.
Protocol remains source-neutral and owns no service. Exclude App, Viewer,
x86/amd64/Intel and Azure; never include credentials or identifiable source/capture data.

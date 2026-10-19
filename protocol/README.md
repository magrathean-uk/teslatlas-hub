<h1 align="center">Teslatlas Protocol</h1>

<p align="center">Public, source-neutral protocol contracts for Teslatlas Hub clients and integrations.</p>

<p align="center">
  <a href="docs/legal/licensing.md">Licence</a> ·
  <a href="AGENTS.md">Agent guidance</a>
</p>

## Overview

This repository owns the stable public boundary between Teslatlas Hub and
Teslatlas, public SDKs, open reference clients, Home Assistant, user-operated
telemetry receivers, and future third-party integrations. The protocol is
implementable from this repository alone: a client does not need Hub Rust or
proprietary Teslatlas source.

It carries three distinct contract families with different identities:

- Current-Hub HTTP: `hub-http-v1@1.2.0`, documented in
  [`docs/reference/current-hub.md`](docs/reference/current-hub.md). It adds
  JSON pages for charges, charge samples and drive positions, address and
  elevation provenance, device vehicle grants, same-successor rotation retries,
  a bounded credential lifetime and self-revocation to the discovery, pairing,
  vehicle, current and drives reads. A Hub built from the current source names
  `hub-http-v1@1.2.0` in its discovery document; the earlier `1.0.0` and `1.1.0`
  profiles were retired and are no longer in this package.
- Candidate Hub changes-since: `hub-sync-v1@1.4.0`, documented in
  [`docs/reference/hub-sync.md`](docs/reference/hub-sync.md). It defines
  schema 2.2 PhysicalV3 bootstrap checkpoints, bounded signed changed sets,
  no-op receipts, vehicle-bound signing-key discovery, and complete signed
  replacement after compaction. The retained `1.3.0` profile also defines
  signed prepared map months.
  The candidate `hub-sync-v1@1.5.0` adds a live overlay (`base_live_sequence`,
  `live_sequence`, the `sync.live` capability) to a PhysicalV3 head; no Hub
  serves it yet.
- `profiles/receiver-v1/1.0.0/`: the receiver envelope a self-hosted telemetry receiver posts to the Hub

These are protocol profiles, not claims about a deployed Hub or SDK release.
This repository contains no Hub implementation, generated SDK, or proprietary
Teslatlas source.

## Contract discipline

This repository is source-neutral. Public files must be sufficient for an
unaffiliated client to implement the contract without Hub Rust, proprietary
App source, or private deployment knowledge. `MUST`, `MUST NOT`, `REQUIRED`,
`SHOULD`, `SHOULD NOT`, and `MAY` are normative terms as defined by RFC 2119
and RFC 8174 when written in uppercase.

Machine-readable artifacts are the wire authority. Prose explains intent but
must not contradict schemas, OpenAPI, profiles, or
conformance cases; an unresolved conflict blocks release. A contract change
updates every affected artifact, example, fixture, profile, generated output,
and test, with the local gate as the release check.

Goal: publish stable, versioned, implementation-neutral contracts and
executable evidence for independent clients. Non-goals: Hub internals,
proprietary App behaviour, generated SDK implementations, hosted automation,
or deployment instructions.

## Features

- `profiles/hub-http-v1/1.2.0/` — current-Hub HTTP schemas, OpenAPI 3.1.0,
  examples, cases, and content hash. Distribute the complete directory because
  its OpenAPI references adjacent schema files. Offline checks run with
  `./conformance/run --profile hub-http-v1@1.2.0`.
- `profiles/hub-sync-v1/1.5.0/` — candidate live overlay on a PhysicalV3 head:
  `base_live_sequence`, one-step live changed sets, a live no-op, a signed
  rebase for any superseded or expired head, and an in-place upgrade of a 1.4
  checkpoint, chained on 1.4.0 and referring to its unchanged artefacts by
  relative path.
- `profiles/hub-sync-v1/1.4.0/` — negotiated schema 2.2 PhysicalV3 changed
  sets, typed delta packs, bounded admission, no-op and full-replacement rebase.
- `profiles/hub-sync-v1/1.3.0/` — retained changes-since, no-op, key discovery,
  schema 2.2 multi-chunk manifests, and signed prepared map months.
- `profiles/receiver-v1/1.0.0/` — the receiver envelope schema, examples and
  vectors, with the two annotated rules a conforming receiver enforces.
- `generators/` — documented `openapi-generator` recipes for
  `profiles/hub-http-v1/1.2.0/openapi.json`; no generated code is committed.
- `conformance/` — the `hub-sync-v1@1.3.0` fixture cases, the offline and native
  checks of `hub-http-v1@1.2.0`, and the runner.

## Getting started

Python 3.11 or later and [uv](https://docs.astral.sh/uv/) are required. From
the repository root, install the locked environment and run the complete
local gate:

```sh
uv sync --locked
./tools/check
```

The gate checks that each retained generator reproduces its profile byte for
byte, runs the unit suite, and runs the 65 `hub-sync-v1@1.3.0` fixture cases.
This is local contract evidence, not installed-Hub, product, real-data, or
release acceptance. It uses no hosted CI.

## Documentation

- [Product versioning](docs/reference/product-versioning.md)
- [Architecture](docs/architecture/overview.md)
- [Current-Hub walkthrough](docs/reference/current-hub.md)
- [Generated-client recipes](generators/README.md)
- [Hub changes-since contract](docs/reference/hub-sync.md)
- [Docker checker and read-only smoke](docs/guides/docker.md)
- [Verification status](docs/reference/verification.md)
- [Conformance](docs/guides/conformance.md)
- [Standards the profiles rely on](docs/reference/standards.md)

Read [CONTRIBUTING](.github/CONTRIBUTING.md) before changing a contract. Use
[SUPPORT](.github/SUPPORT.md) for questions and [SECURITY](.github/SECURITY.md)
for sensitive reports. GitHub is source storage for this project; validation
runs locally.

## Licence

Teslatlas Protocol is licensed under the Apache License 2.0. See
[LICENSE](LICENSE) and [NOTICE](NOTICE). Third-party dependency notices are in
[`docs/legal/third-party-notices.md`](docs/legal/third-party-notices.md); the
project/dependency licensing boundary is explained in the
[licensing guide](docs/legal/licensing.md).

<sub>© 2026 MAGRATHEAN UK LTD · [Legal](https://github.com/magrathean-uk/.github/blob/main/LEGAL.md)</sub>

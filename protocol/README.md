<h1 align="center">Teslatlas Protocol</h1>

<p align="center">Public, source-neutral protocol contracts for Teslatlas Hub clients and integrations.</p>

<p align="center">
  <a href="docs/legal/licensing.md">Licence</a> ·
  <a href="AGENTS.md">Agent guidance</a>
</p>

## Overview

This repository owns the stable public boundary between Teslatlas Hub and
Teslatlas, public SDKs, open reference clients, Home Assistant, user-operated
edge receivers, and future third-party integrations. The protocol is
implementable from this repository alone: a client does not need Hub Rust or
proprietary Teslatlas source.

It carries four distinct contract families with different identities:

- Candidate current-Hub HTTP: `hub-http-v1@1.1.0`, documented in
  [`docs/reference/current-hub.md`](docs/reference/current-hub.md). It
  describes discovery, pairing, authenticated vehicle/current/drives reads,
  and credential rotation.
- Candidate Hub changes-since: `hub-sync-v1@1.4.0`, documented in
  [`docs/reference/hub-sync.md`](docs/reference/hub-sync.md). It defines
  schema 2.2 PhysicalV3 bootstrap checkpoints, bounded signed changed sets,
  no-op receipts, vehicle-bound signing-key discovery, and complete signed
  replacement after compaction. The retained `1.3.0` profile also defines
  signed prepared map months.
- Rich semantic HTTP and event profiles: `1.0.0`, `1.1.0`, and `1.2.0`. The
  local conformance gate covers the current minor and its two predecessors.
  Their walkthrough is
  [`docs/guides/client-quickstart.md`](docs/guides/client-quickstart.md) and
  includes version negotiation, SSE, commands, and metadata.
- Edge delivery: `profiles/edge-delivery-v2/2.0.0/`, with its own pull and
  acknowledgement contract.

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
must not contradict schemas, OpenAPI, the SSE JSON contract, profiles, or
conformance cases; an unresolved conflict blocks release. A contract change
updates every affected artifact, example, fixture, profile, generated output,
and test, with the local gate as the release check.

Goal: publish stable, versioned, implementation-neutral contracts and
executable evidence for independent clients. Non-goals: Hub internals,
proprietary App behaviour, generated SDK implementations, hosted automation,
or deployment instructions.

## Features

- `openapi/teslatlas-v1.openapi.json` — self-contained OpenAPI 3.1.1 rich query API.
- `schemas/` — canonical JSON Schema 2020-12 contracts.
- `events/teslatlas-v1.sse.json` — SSE framing, replay, and event catalogue.
- `examples/` — redacted valid and deliberately invalid wire examples.
- `fixtures/v1/` — deterministic normal, duplicate, delayed, missing, and
  reordered observation scenarios.
- `compatibility/` — the current and previous two minor profiles.
- `profiles/hub-http-v1/1.1.0/` — current-Hub HTTP schemas, OpenAPI 3.1.0,
  examples, cases, and content hash. Distribute the complete directory because
  its OpenAPI references adjacent schema files.
- `profiles/hub-sync-v1/1.4.0/` — negotiated schema 2.2 PhysicalV3 changed
  sets, typed delta packs, bounded admission, no-op and full-replacement rebase.
- `profiles/hub-sync-v1/1.3.0/` — retained changes-since, no-op, key discovery,
  schema 2.2 multi-chunk manifests, and signed prepared map months.
- `profiles/edge-delivery-v2/2.0.0/` — Edge pull, stable identity,
  sequence/gap, acknowledgement, and durable-consumer disposition contract.
- `conformance/` — JSONL adapter protocol, executable cases, and runner.

## Getting started

Python 3.11 or later and [uv](https://docs.astral.sh/uv/) are required. From
the repository root, install the locked environment and run the complete
local gate:

```sh
uv sync --locked
./tools/check
```

The gate validates generated profiles, every JSON Schema, OpenAPI, SSE
example, valid and invalid example, deterministic fixture, conformance
vector, and all three compatibility profiles. It then runs the unit suite and
bundled conformance adapter. This is local contract evidence, not
installed-Hub, product, real-data, or release acceptance. It uses no hosted
CI.

To build and verify the deterministic, unpublished source bundle, follow
[`docs/guides/developer-bundle.md`](docs/guides/developer-bundle.md). The
bundle includes the exact dependency lock and public contract resources but
does not claim a service, release, cold-cache offline dependency install, or
downstream product acceptance.

## Documentation

- [Product versioning](docs/reference/product-versioning.md)
- [Architecture](docs/architecture/overview.md)
- [Current-Hub walkthrough](docs/reference/current-hub.md)
- [Hub changes-since contract](docs/reference/hub-sync.md)
- [Docker checker and read-only smoke](docs/guides/docker.md)
- [Deterministic developer bundle](docs/guides/developer-bundle.md)
- [Compatibility record](docs/reference/compatibility.md)
- [Verification status](docs/reference/verification.md)
- [Rich-profile client walkthrough](docs/guides/client-quickstart.md)
- [Rich-profile HTTP contract](docs/reference/http.md)
- [Canonical data model](docs/reference/data-model.md)
- [Event stream](docs/reference/events.md)
- [Commands and metadata](docs/reference/commands-and-metadata.md)
- [Conformance](docs/guides/conformance.md)
- [Normative references](docs/reference/standards.md)

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

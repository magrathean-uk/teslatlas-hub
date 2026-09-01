# Protocol and deployed-Hub activation record

## Strict protocol 1.2 authority

`TeslatlasHubSDK` and `TeslatlasCommands` continue to consume
`teslatlas-protocol` commit
`79ced4c7fdc79520ad31d72a0280bf5f3f19f407`, profile `1.2.0`, with public
compatibility profiles `1.0.0`, `1.1.0`, and `1.2.0`.

Those profiles remain public protocol definitions. They are not evidence that
Teslatlas Hub v1.0.0 implements any one of them. The strict SDK surface and its
pinned protocol fixtures were not rewritten to match deployed Hub behaviour.

## Deployed-Hub dependency finding

The protocol repository checked for this work had no GitHub release objects,
no deployed-Hub binding, and no separately named machine-readable authority for
Hub v1.0.0. The generic `compatibility/1.0.0/profile.json` advertises protocol
features that are not the deployed Hub v1.0.0 surface and could not safely be
used as its binding.

Under the original task wording this was a mandatory implementation stop. The
first result archive documented that stop. The later instruction to pull the
needed public material from the internet changed the execution decision: an
isolated local binding was audited from the immutable Hub `v1.0.0` tag rather
than inventing routes or altering the strict SDK.

The local authority is pinned to:

| Item | Identity |
| --- | --- |
| Hub annotated tag | `v1.0.0` / `4b45708a00f14f76306f6cb37375eb0c538643d7` |
| Hub commit | `a5e6c5c4f86776da96c9946f7e45b2080c571f86` |
| Binding SHA-256 | `78aca4b6014625420efb66fbe35f6ea10d72a7f596210dac80964c33e0f04f65` |
| Binding path | `Sources/TeslatlasHubV1Compatibility/Binding/deployed-hub-v1.0.0.json` |

The binding itself records the exact Hub Git blob identities used for each
wire-contract fact. Runtime loading checks both its content hash and critical
constants before network I/O.

## Activated deployed slice

| Slice | Compatibility behaviour |
| --- | --- |
| Discovery | Exact unauthenticated route and strict response field set |
| Identity | Caller-pinned non-nil Hub UUID retained across refresh |
| Origin | Same-origin only, no redirects, HTTPS except loopback |
| Authentication | Caller-owned existing paired-device bearer, applied last |
| Vehicles | Exact deployed list route and response model |
| Current state | Exact deployed route and full v1.0.0 response model |
| Drives | Exact route, bounds, opaque cursor, strong ETag, stable errors |

Events, commands, metadata, charges, pairing, rotation, manifests, packs,
positions, states, updates, and all unbound routes remain absent from the
compatibility product.

## Proof boundary and remaining gate

Passing XCTest proves the behaviour of the Swift clients against pinned local
fixtures and test transports. It does not prove a live Hub journey. The opt-in
`LiveHubBlackBoxTests` suite is separate and requires an unchanged Hub v1.0.0,
its pinned UUID, and an existing paired-device bearer.

The protocol-governance blocker remains: `teslatlas-protocol` should publish a
released, separately named deployed-Hub v1.0.0 binding. This local binding must
not be relabelled as protocol authority. When an official binding appears, it
must be compared with the vendored bytes, fields, limits, errors, and fixtures
before any replacement.

Other external validation still includes macOS/Xcode/iOS builds and an opted-in
live Hub run. Linux FoundationNetworking enforces response size after URL
loading returns; Apple URLSession uses bounded streamed reads.

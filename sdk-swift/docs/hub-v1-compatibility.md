# Deployed Hub v1.0.0 compatibility

## Contract boundary

`TeslatlasHubV1Compatibility` is an independent SwiftPM product for the wire
surface exposed by unchanged Teslatlas Hub v1.0.0. It neither imports nor wraps
`TeslatlasHubSDK`, and it does not reinterpret Hub v1.0.0 as public protocol
profile 1.0, 1.1, or 1.2.

The public operations are exactly:

| Operation | Method and path | Authentication |
| --- | --- | --- |
| Discovery | `GET /.well-known/teslatlas-hub` | None |
| Vehicles | `GET /v1/vehicles` | Existing paired-device bearer |
| Current state | `GET /v1/vehicles/{vehicle_id}/current` | Existing paired-device bearer |
| Drives | `GET /v1/vehicles/{vehicle_id}/drives` | Existing paired-device bearer |

No other route is generated or publicly addressable by this target. Events,
commands, mutable metadata, charges, pairing claims, token rotation, manifests,
packs, positions, states, and updates remain absent.

## Binding authority and provenance

At implementation time, `teslatlas-protocol` commit
`79ced4c7fdc79520ad31d72a0280bf5f3f19f407` had no released deployed-Hub
v1.0.0 binding. Its compatibility profiles describe public protocol profiles,
not the deployed Hub release.

The local compatibility binding was derived from that immutable Hub release boundary. Its identities are:

- Hub tag: `v1.0.0`
- annotated tag object: `4b45708a00f14f76306f6cb37375eb0c538643d7`
- dereferenced Hub commit: `a5e6c5c4f86776da96c9946f7e45b2080c571f86`
- bundled binding: `Sources/TeslatlasHubV1Compatibility/Binding/deployed-hub-v1.0.0.json`
- binding SHA-256: `78aca4b6014625420efb66fbe35f6ea10d72a7f596210dac80964c33e0f04f65`

The binding records the exact Git blob identities used to audit discovery,
routing, authentication, current-state models, vehicle models, drive models,
pagination, source identity, and release version. The loader validates the
resource SHA-256 and all critical constants before any network request. No Hub
Rust implementation is included in this package.

This local binding is compatibility evidence, not a protocol release. The
remaining governance action is for `teslatlas-protocol` to publish a separately
named deployed-Hub v1.0.0 binding. Any later replacement must be compared
field-for-field, route-for-route, and fixture-for-fixture before changing this
module.

## Credential safety

`HubV1Client.connect` requires both a caller-pinned Hub UUID and an existing
paired-device bearer. Hub v1.0.0 issues a 64-character ASCII hexadecimal wire
token; the credential wrapper validates that exact binding rule before a
client can be constructed. The following work completes before the bearer is
added to an HTTP request:

1. load and hash-check the bundled binding;
2. reject a nil expected identity;
3. require the exact discovery path with no user information, query, or
   fragment;
4. require HTTPS except for a loopback origin;
5. fetch discovery without an `Authorization` header;
6. reject redirects or a changed final URL;
7. validate the exact discovery fields, Hub UUID, release version, source URL,
   protocol marker, API version, pack format, capability set, and manifest-key
   relationship;
8. resolve only a route declared by the binding and validate its same-origin
   URL;
9. validate the requested drive bounds and limit.

The bearer is applied last. Its normal description, debug description, and
reflection are redacted. The default URL session is ephemeral and disables
cookies, URL credential storage, and caching. URLSession system trust remains
the TLS authority; the deployed binding contains no certificate-pin
provisioning contract.

## Drive pagination

The client sends explicit `from_ms`, `to_ms`, and `limit` values using the
binding defaults when the caller omits them. It enforces:

- `from_ms >= 0`;
- `to_ms >= 0` and `from_ms < to_ms`;
- `1 <= limit <= 500`;
- an opaque cursor passed through without decoding or logging;
- `If-None-Match` only with the quoted lowercase SHA-256 ETag emitted by Hub
  v1.0.0;
- a strong ETag on both `200` and `304` drive responses;
- an empty body on `304`;
- vehicle identity consistency for every returned drive.

Only the six stable deployed drive error codes in the binding are surfaced as
typed API errors. Unknown codes and response fields fail closed.

## Proof separation

Deterministic fixture proof lives under
`Tests/TeslatlasHubV1CompatibilityTests/Fixtures`. `FIXTURE-SHA256.json` pins
every fixture byte, while `AUTHORITY` records that this is offline evidence
only. Fixture tests cover the exact normal responses, pagination, one stable
error, invalid fields, identity changes, capability failures, origin failures,
redirect rejection, and bearer redaction.

`LiveHubBlackBoxTests` is a separate opt-in journey. It is skipped unless all
three environment variables are supplied:

```text
TESLATLAS_HUB_V1_DISCOVERY_URL
TESLATLAS_HUB_V1_EXPECTED_HUB_ID
TESLATLAS_HUB_V1_BEARER
```

The live test performs only discovery, vehicles, current state, and at most two
drive pages. It neither pairs a device nor modifies Hub state. A passing
fixture suite must never be reported as live-Hub proof.

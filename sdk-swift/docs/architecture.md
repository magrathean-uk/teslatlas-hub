# Swift SDK architecture

## Responsibility

Provide idiomatic Swift boundaries over two explicit contracts without
importing the Teslatlas app or Hub implementation.

## Package boundaries

| Unit | Responsibility |
| --- | --- |
| `TeslatlasHubSDK` | Strict protocol-1.2 discovery, version negotiation, endpoint trust, typed reads, ETags, problems, and event decoding |
| `TeslatlasCommands` | Strict protocol-1.2 asynchronous commands with explicit confirmation and idempotency |
| `TeslatlasHubV1Compatibility` | Isolated deployed-Hub v1.0.0 discovery, existing bearer, vehicles, current state, and drives |
| Internal mechanics | Atomic JSON state, bounded SSE framing, generic HTTP range validation, binding hash validation, and isolated transports |

`TeslatlasCommands` depends on `TeslatlasHubSDK`. The Hub-v1 compatibility
target depends on neither strict target. It owns separate models, errors,
routes, transport, fixtures, and tests. This prevents deployed behaviour from
silently weakening or expanding the public protocol-1.2 API.

## Strict protocol trust model

`TeslatlasClient.connect` obtains public discovery without authorization. It
then applies two independent checks before any authenticated request:

1. every advertised endpoint origin must be the discovery origin or an origin
   explicitly approved by the caller;
2. later discovery documents must retain the original `hub_id`.

Non-loopback endpoints require HTTPS. System URL loading performs TLS trust;
the SDK does not invent certificate pin material absent from the released
protocol. Endpoint-origin approval prevents a valid-looking discovery document
from redirecting a bearer credential to an arbitrary HTTPS host.

The client selects the highest compatible protocol version not newer than its
declared maximum. Every authenticated request sends that selected version and
the supplied authorization provider. Conditional GET handling requires the
selected version, ETag, cache metadata, bounded response bytes, and a
schema-derived Codable model. `304` responses require an empty body and retain
the opaque ETag.

The public SSE decoder performs bounded incremental framing, preserves opaque
replay IDs, caps retry values, ignores unknown event names before payload
decoding, and checks event identity. Network reconnect timing, terminal `204`,
replay-expired `410`, checkpoint persistence, and resynchronisation remain
caller-owned.

## Deployed Hub v1 trust model

`HubV1Client.connect` first hash-checks its bundled machine-readable binding,
then validates the exact discovery URL. Discovery is always unauthenticated.
The document must retain the caller-pinned Hub UUID and match the immutable Hub
v1.0.0 source identity, version, protocol marker, API version, pack format, and
one of two exact capability sets.

Authenticated URLs are built only from binding route templates on the discovery
origin. Redirects are disabled, final URLs are compared with the validated
request URL, and the bearer is attached only after binding, capability, query,
path, and origin validation. HTTPS is mandatory outside loopback. No alternate
origin allow-list exists for this compatibility target because the deployed
Hub binding does not advertise API endpoint relocation.

## Unsupported or deliberately separate contracts

The strict protocol targets retain their released positions, charges, events,
data quality, metadata models, and command product. None of those symbols are
re-exported by the Hub-v1 compatibility target.

The compatibility target exposes no pairing claim, token rotation, device
management, events, commands, metadata endpoint, charge history, manifest,
pack, position, state, or update route. It does not expose `sync.packs` even
when discovery advertises that capability.

Full protocol conformance remains pending an independently implemented JSONL
adapter. Live Hub-v1 proof remains separate from deterministic fixture proof.

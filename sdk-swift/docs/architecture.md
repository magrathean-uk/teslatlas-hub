# Swift SDK architecture

## Responsibility

Provide an idiomatic Swift boundary over released `teslatlas-protocol`
contracts without importing the proprietary Teslatlas app or AGPL Hub
implementation.

## Package boundaries

| Unit | Responsibility |
| --- | --- |
| `TeslatlasHubSDK` | Discovery, version negotiation, endpoint trust, typed reads, ETags, problems, and event decoding |
| `TeslatlasCommands` | Advertised asynchronous commands with explicit confirmation and idempotency |
| Internal mechanics | Atomic JSON state, bounded SSE framing, and generic HTTP range validation |

The read and command products share public protocol value types and transport
interfaces. Command submission remains a separate import and makes one network
attempt. Reissue decisions require application-side state reconciliation.

## Trust model

`TeslatlasClient.connect` obtains public discovery without authorization. It
then applies two independent checks before any authenticated request:

1. every advertised endpoint origin must be the discovery origin or an origin
   explicitly approved by the caller;
2. later discovery documents must retain the original `hub_id`.

Non-loopback endpoints require HTTPS. System URL loading performs TLS trust;
the SDK does not invent certificate pin material absent from the released
protocol. Endpoint-origin approval prevents a valid-looking discovery document
from redirecting a bearer credential to an arbitrary HTTPS host.

## Read path

The client selects the highest compatible protocol version not newer than its
declared maximum. Every authenticated request sends that selected version and
the supplied authorization provider. JSON GET handling requires the selected
version, ETag, cache metadata, bounded response bytes, and a schema-derived
Codable model. `304` responses require an empty body and preserve the opaque
ETag.

Server failures decode as `TeslatlasProblemDetails`. Stable `code`,
`request_id`, `retryable`, and field errors remain available without exposing
credentials in descriptions.

## Event path

The public decoder wraps the internal incremental SSE framer. It:

- dispatches only on blank lines;
- supports fragmented UTF-8 and repeated `data` fields;
- preserves opaque replay IDs and empty-ID reset semantics;
- caps server retry values at 30,000 ms;
- ignores unknown event names before payload decoding;
- checks event name, ID, vehicle, resource, and revision equality.

Network reconnect timing, terminal `204`, replay-expired `410`, checkpoint
persistence, and query resynchronisation remain caller-owned in this batch.

## Unsupported contracts

The released protocol defines an already provisioned bearer credential, not
QR claim, cancellation, rotation, or revocation. It also defines no signed
manifest or immutable-pack endpoint. Those APIs stay absent. The internal
atomic store and range validator do not create public deployment contracts.

Mutable metadata is released in protocol profile 1.2 but is not yet exposed by
this SDK batch. Full conformance remains pending an independently implemented
JSONL adapter against the protocol runner.

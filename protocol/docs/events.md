# Event stream

`GET /v1/events` is the ordered live-notification surface. OpenAPI defines the
HTTP operation. `events/teslatlas-v1.sse.json` defines SSE framing, replay, and
the event catalogue. JSON formed from SSE `data` lines validates against
`schemas/event.schema.json`.

## Connection

Send the selected `Teslatlas-Protocol-Version` and normal bearer credential.
The response is UTF-8 `text/event-stream`. Implementations must dispatch only
at a blank line, concatenate repeated `data` fields with a newline, and ignore
unknown SSE fields.

The server emits a comment heartbeat at least every 15 seconds while the
connection is idle. Comments carry no semantic event. A server-supplied
`retry` value is milliseconds; clients cap it at 30,000 ms. HTTP `204` means the
client must stop reconnecting.

## Event envelope

Every dispatched event has `id`, `event`, and `data` fields. The normalized
values obey:

- event IDs are opaque and unique in one Hub stream;
- SSE `id` equals `data.event_id`;
- SSE `event` equals `data.event_type`;
- `occurred_at` is an exact-millisecond UTC timestamp;
- `vehicle_id`, `resource_id`, and `revision` identify the visible change;
- `data` contains the full current public resource or command/metadata record.

The event schema binds each recognized event name to its exact payload schema.
Standard JSON Schema cannot compare arbitrary sibling values, so equality
between the envelope identity/revision and its payload is a separate normative
semantic check in the language-neutral conformance runner. Schema validation
alone is insufficient for event conformance. `metadata.changed` carries either
a live record or a persistent deletion tombstone.

Consumers use the named event catalogue, then retrieve the canonical resource
when they need a representation not carried by the event. Unknown event names
are ignored before JSON data decoding or schema validation and do not terminate
the stream. Servers emit only event names defined by the negotiated profile;
the strict event schema is the server-conformance contract for those names.

## Replay

After accepting an event, persist its exact ID. Reconnect with:

```http
Last-Event-ID: event_demo_0042
```

The server resumes strictly after that event. IDs are bound to Hub identity,
authenticated principal, and normalized filters. Clients must not decode,
edit, compare, or manufacture them.

The replay window is at least 86,400 seconds. Failure semantics are stable:

| Status | Code | Meaning |
| --- | --- | --- |
| `400` | `event_id_invalid` | Malformed, forged, or wrong-scope ID |
| `410` | `event_replay_expired` | Required replay point is no longer retained |
| `204` | none | Terminal response; do not reconnect |

An SSE line containing an empty `id:` resets the browser/client replay buffer.
The next reconnect omits `Last-Event-ID` until another non-empty ID has been
accepted. An expired replay point is not silently converted into a live-only
stream; the client must resynchronize through query resources first.

## Ordering and duplicates

Wire order is the stream order for one bound connection. Replayed events can
overlap an application checkpoint, so clients apply `event_id` idempotently.
Event order does not replace the observation canonical-order rules.

Disconnects, heartbeats, and reconnects do not imply data loss. A `410`
explicitly proves the opposite: replay continuity is unavailable and query
resynchronization is required.

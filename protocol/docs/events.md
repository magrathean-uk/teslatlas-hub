# Event stream

`GET /v1/events` is the ordered live-notification surface. OpenAPI defines the
HTTP operation. `events/teslatlas-v1.sse.json` defines SSE framing, replay, and
the event catalogue. Event bodies validate against
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

- SSE `id` equals `data.event_id`;
- SSE `event` equals `data.event_type`;
- event IDs are opaque and unique in one Hub stream;
- `occurred_at` is an exact-millisecond UTC timestamp;
- `vehicle_id`, `resource_id`, and `revision` identify the visible change;
- `data` contains the full current public resource or command/metadata record.

Consumers use the named event catalogue, then retrieve the canonical resource
when they need a representation not carried by the event. Unknown event names
are ignored and do not terminate the stream.

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

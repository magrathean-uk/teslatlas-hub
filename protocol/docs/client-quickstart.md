# Client quickstart

This flow is sufficient to build an unaffiliated v1 client from public
artifacts.

## 1. Discover

```sh
curl -sS https://hub.example.invalid/.well-known/teslatlas-hub
```

Validate the response against `schemas/discovery.schema.json`. Record
`hub_id`, choose the highest mutually supported protocol version, and inspect
capabilities and limits. Do not copy endpoint URLs between Hubs.

## 2. Query

Use a provisioned bearer credential and send the selected version:

```sh
curl -sS \
  -H 'Authorization: Bearer REDACTED' \
  -H 'Teslatlas-Protocol-Version: 1.2.0' \
  'https://hub.example.invalid/v1/vehicles?limit=100'
```

Follow `next_cursor` unchanged. Preserve the original filters. Stop when it is
`null`. Treat IDs, cursors, and ETags as opaque.

For a cacheable read, retain ETag and revalidate:

```sh
curl -i \
  -H 'Authorization: Bearer REDACTED' \
  -H 'Teslatlas-Protocol-Version: 1.2.0' \
  -H 'If-None-Match: "opaque-etag"' \
  https://hub.example.invalid/v1/vehicles/vehicle_demo_alpha/current
```

A matching representation returns `304` without a body.

## 3. Stream

```sh
curl -N \
  -H 'Accept: text/event-stream' \
  -H 'Authorization: Bearer REDACTED' \
  -H 'Teslatlas-Protocol-Version: 1.2.0' \
  -H 'Last-Event-ID: event_demo_0042' \
  https://hub.example.invalid/v1/events
```

Persist an ID only after applying its event. On `410`, query current resources,
replace the local checkpoint, then reconnect without the expired ID. On `204`,
stop reconnecting.

## 4. Command

Read the `commands.async` discovery catalogue first. Validate parameters and
expected state against that command's schemas, then submit a fresh idempotency
key:

```sh
curl -i \
  -X POST \
  -H 'Authorization: Bearer REDACTED' \
  -H 'Content-Type: application/json' \
  -H 'Teslatlas-Protocol-Version: 1.2.0' \
  -H 'Idempotency-Key: 11111111-1111-4111-8111-111111111111' \
  --data @examples/command-request.json \
  https://hub.example.invalid/v1/commands
```

Expect `202`. Poll the `Location` conditionally or consume `command.changed`.
Do not interpret acceptance as vehicle success.

## 5. Update metadata

GET a metadata record and retain its strong ETag. Replace it only with that
ETag:

```sh
curl -i \
  -X PUT \
  -H 'Authorization: Bearer REDACTED' \
  -H 'Content-Type: application/json' \
  -H 'Teslatlas-Protocol-Version: 1.2.0' \
  -H 'If-Match: "mC7pL2xW9dR5tN8q"' \
  --data '{"value":{"text":"Redacted client note."}}' \
  https://hub.example.invalid/v1/metadata/metadata_demo_note_0001
```

On `409`, fetch the current record and reconcile. Never overwrite blindly.

## 6. Test the client adapter

Implement the JSONL adapter in `docs/conformance.md`, then run:

```sh
./conformance/run --adapter /absolute/path/to/your-adapter
```

The OpenAPI document is self-contained. JSON Schemas, examples, fixtures, and
all compatibility profiles remain authoritative even if no generated SDK is
used.

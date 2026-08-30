# HTTP contract

OpenAPI is the operation authority. This document defines cross-operation
semantics that clients and servers must apply consistently.

## Transport and representation

- Non-loopback deployments use HTTPS.
- JSON request and response bodies use UTF-8 and `application/json`.
- Errors use `application/problem+json`.
- SSE uses `text/event-stream` and is specified separately.
- JSON numbers are finite. `NaN`, positive infinity, and negative infinity are
  invalid.
- Unknown RFC 9457 extension members are ignored.

The OpenAPI bearer scheme represents an already provisioned paired-device
credential. Credential claim, rotation, and revocation are deployment
contracts, not hidden requirements of this query protocol. Discovery never
contains credentials.

## Discovery and negotiation

`GET /.well-known/teslatlas-hub` is public. Its response is validated by
`schemas/discovery.schema.json` and contains:

- stable Hub identity;
- current, minimum, and supported protocol versions;
- stable, experimental, or deprecated capabilities;
- a command catalogue with parameter schemas and required scopes;
- API, event, and OpenAPI endpoints;
- enforced limits.

For versioned operations, a client sends `Teslatlas-Protocol-Version` with the
highest version it understands. The server selects the highest compatible
version with the same major number that is not newer than the client value.
The response repeats the selected version. If there is no compatible version,
the server returns `426` and code `unsupported_protocol_version`.

If the request header is absent, the client is treated as requesting the
minimum supported version. Capability presence, not version comparison alone,
authorises use of optional surfaces.

## Time bounds

All wire timestamps are RFC 3339 UTC with exactly three fractional digits:

```text
2026-08-30T12:00:00.000Z
```

`from` is inclusive. `to` is exclusive. `from` must be earlier than `to`.
When both are omitted, a history endpoint selects the newest bounded window
ending at request admission time; it never performs an unbounded scan.

General history windows are at most 366 days. Dense positions and charge
samples are at most 31 days. A cursor preserves the original bounds and
snapshot even if wall-clock time advances between pages.

## Opaque cursors

Pagination uses `cursor` and `limit`. Offset, page number, and page size
aliases are not part of the contract.

A cursor is URL-safe and opaque. A client stores and returns it unchanged. It
must not decode it, infer ordering from it, edit it, or construct one. Base64
encoding alone is not integrity protection.

The server binds every cursor to:

1. endpoint;
2. normalized query excluding `cursor`;
3. authenticated principal;
4. authorization revision;
5. selected immutable snapshot.

The server detects forgery and invalidates a cursor when its authorization
scope changes. Stable failures are:

| Status | Code | Meaning |
| --- | --- | --- |
| `400` | `invalid_cursor` | Malformed, forged, or otherwise invalid token |
| `409` | `cursor_query_mismatch` | Token used with different endpoint or filters |
| `403` | `cursor_scope_changed` | Principal or permissions changed |
| `410` | `cursor_expired` | Snapshot or cursor retention elapsed |

Page items have a deterministic server order. `next_cursor` is `null` after
the final page. `snapshot_revision` remains identical across pages belonging
to the same traversal.

## ETags and conditional requests

An ETag validates the selected representation, including protocol version,
authorization scope, filters, and encoding. Clients treat it as opaque.

All JSON `GET` responses expose ETag and accept `If-None-Match`. GET uses the
RFC 9110 weak comparison function. A match returns `304` with no body. The
`304` repeats at least ETag, cache controls, Vary, and any applicable
Deprecation, Link, and Sunset metadata.

Mutable metadata uses a strong ETag with `If-Match`:

- missing `If-Match` returns `428` `precondition_required`;
- a stale or mismatched value returns `409` `metadata_revision_conflict`;
- success returns the new representation and a new ETag.

ETags do not expose hashes, revisions, database keys, or server internals.

## Problem details

Every error body follows RFC 9457 and `schemas/error.schema.json`. `status`
matches the HTTP status. `type` and `title` retain stable meaning. `instance`
and `request_id` are opaque correlation values and contain no secrets.

`code` is the stable programmatic discriminator. Clients branch on `code`, not
human `detail`. Unknown extension members are ignored. Retry only when
`retryable` is true and the operation itself permits retry.

## Limits

The current v1 baseline has these fixed limits. They are repeated in discovery
and `x-teslatlas-limits` in OpenAPI.

| Limit | Value |
| --- | ---: |
| Request body | 262,144 bytes |
| Default page | 100 items |
| Maximum page | 500 items |
| General history | 366 days |
| Dense positions/samples | 31 days |
| Concurrent requests per credential | 8 |
| Concurrent SSE connections per credential | 2 |
| Event replay retention | at least 86,400 seconds |
| Command idempotency retention | at least 86,400 seconds |

The byte limit applies after transfer decoding and before JSON parsing. A body
over the limit returns `413` `request_too_large`. A range over the limit returns
`400` `range_too_large`. Concurrency and request-rate limits return `429` with
`Retry-After` and a stable code.

## Deprecation

Deprecated capabilities are marked in discovery. Affected responses emit:

- RFC 9745 `Deprecation`;
- `Link` with `rel="deprecation"`;
- RFC 8594 `Sunset` once removal is scheduled.

Removal occurs only after two later minor profiles and 180 days, whichever is
later. The exact policy is in `docs/versioning.md`.

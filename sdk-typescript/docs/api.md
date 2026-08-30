# SDK API reference

The current package is a transport foundation. It intentionally exposes no Teslatlas
resource method, payload model, event name, credential format, or command state.

## Create a runtime transport

Use the runtime-specific entry point. Both factories return the same `FetchTransport`.

```typescript
import { createNodeTransport } from "@teslatlas/sdk/node";

const transport = createNodeTransport({
  baseUrl: "https://hub.example",
  authorization: async ({ url, method }) => {
    console.log(method, url.origin);
    return credentialStore.load();
  },
});
```

Use `createBrowserTransport()` from `@teslatlas/sdk/browser` in a browser. The SDK calls
the authorization provider for every request and retains no returned value.

`FetchTransport.request(path, init)` accepts only a root-relative path, reserves the
`Authorization` header for the provider, optionally applies `ifNoneMatch`, makes one fetch
attempt, and returns the native `Response`. HTTP statuses are not decoded because the
released error envelope does not exist.

The transport errors are:

| Error | Meaning |
| --- | --- |
| `InvalidBaseUrlError` | Base URL is malformed, non-HTTP(S), or embeds credentials |
| `InvalidRequestPathError` | Request target is not a safe root-relative path |
| `ReservedAuthorizationHeaderError` | Request tried to bypass the authorization provider |
| `InvalidAuthorizationValueError` | Provider returned an empty or control-bearing value |
| `MissingFetchError` | Runtime has no Fetch API and none was injected |
| `TransportError` | Fetch or authorization failed; underlying cause is not retained |

Abort errors are propagated unchanged so callers can distinguish cancellation.

## Preserve cursors and ETags

```typescript
import {
  appendOpaqueQueryValue,
  applyIfNoneMatch,
  asEntityTag,
  asOpaqueCursor,
  readEntityTag,
} from "@teslatlas/sdk";

const url = appendOpaqueQueryValue(
  new URL("https://hub.example/items"),
  protocolCursorParameterName,
  asOpaqueCursor(savedCursor),
);
const headers = applyIfNoneMatch(new Headers(), asEntityTag(savedEntityTag));
```

`OpaqueCursor` and `EntityTag` are branded strings. Safe values are preserved and never
decoded; empty or control-bearing values are rejected. `isNotModified(status)` recognizes
HTTP status `304`.

## Check versions and capabilities

```typescript
import {
  checkProtocolVersion,
  createCapabilitySet,
  parseProtocolVersion,
  requireCapabilities,
} from "@teslatlas/sdk";

const compatibility = checkProtocolVersion(parseProtocolVersion(discoveredVersion), {
  major: supportedMajor,
  minimumMinor: supportedMinimumMinor,
  maximumMinor: supportedMaximumMinor,
});

requireCapabilities(createCapabilitySet(discoveredCapabilities), requiredCapabilities);
```

The compatibility window and capability identifiers come from released protocol artifacts
or the caller. The SDK does not hard-code policy or names.

Invalid inputs raise `InvalidProtocolVersionError`, `InvalidCompatibilityWindowError`, or
`InvalidCapabilityError`. Missing values raise `MissingCapabilitiesError` with a sorted,
deduplicated `missing` list.

## Handle stable errors safely

`TeslatlasError` is the typed base class. `ProtocolError` carries only an opaque
`ProtocolErrorCode`, HTTP status, and an explicitly supplied safe request ID. It accepts no
arbitrary response detail or body.

```typescript
import { ProtocolError, asProtocolErrorCode, asSafeRequestId } from "@teslatlas/sdk";

throw new ProtocolError({
  code: asProtocolErrorCode(decodedStableCode),
  status: response.status,
  requestId: asSafeRequestId(decodedRequestId),
});
```

`asSafeRequestId()` accepts 1 to 256 characters and rejects control characters. Public
errors never retain arbitrary underlying causes. The future generated decoder will define
where protocol error values come from.

## Keep credentials caller-owned

`CredentialStore<T>` declares `load`, `save`, and `clear`. The SDK ships no implementation.
`AuthorizationProvider` returns a complete authorization value for one request.

Do not put long-lived credentials in browser storage unless the embedding product has made
and documented that security decision.

## Parse or subscribe to SSE

`parseSseStream(stream, options)` incrementally yields raw `event`, `retry`, and
ID-only `checkpoint` items from a UTF-8 `ReadableStream<Uint8Array>`.

Use `subscribeToSse()` for fetch, replay, checkpoint, and reconnect behavior:

```typescript
import { subscribeToSse } from "@teslatlas/sdk/browser";

for await (const event of subscribeToSse({
  transport,
  path: protocolGeneratedEventPath,
  checkpoint: callerCheckpointStore,
  reconnect: ({ attempt }) => (attempt <= 3 ? 250 * attempt : undefined),
  maximumServerRetryMilliseconds: 30_000,
  signal,
})) {
  await handleRawEvent(event);
}
```

The subscription sends `Accept: text/event-stream`. It sends `Last-Event-ID` only when a
non-empty checkpoint exists. An event ID is saved only when the consumer requests the next
event. Breaking or returning early does not commit it, so restart can replay the
unprocessed event. An ID-only block is saved immediately because it contains no consumer
event. An empty ID clears the checkpoint.

Reconnect is disabled unless a caller policy returns a nonnegative delay. A valid server
`retry` hint overrides that one delay and is clamped to the configured minimum and maximum.
HTTP and content-type failures are typed; abort always stops before another fetch.
Checkpoint-store failures are normalized without retaining the store's underlying error.

## Guard command submission

`assertCommandSafety(value)` requires a non-empty, header-safe `idempotencyKey` and exact
retry mode `never`.

```typescript
import { assertCommandSafety } from "@teslatlas/sdk";

const safety = { idempotencyKey: callerGeneratedKey, retry: "never" as const };
assertCommandSafety(safety);
```

This is only a safety primitive. The SDK does not submit, poll, cancel, or interpret a
command job while its protocol contract is absent.

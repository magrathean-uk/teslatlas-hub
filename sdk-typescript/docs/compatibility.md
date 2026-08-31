# Teslatlas SDK compatibility

## Locked input and regeneration

The SDK is generated and tested against protocol commit
`79ced4c7fdc79520ad31d72a0280bf5f3f19f407`. The current profile is `1.2.0`;
the supported profile set is `1.0.0`, `1.1.0`, and `1.2.0`.

`protocol/lock.json` records every vendored input and generated-output SHA-256
digest. Validate the checked snapshot with:

```bash
npm run protocol:check
shasum -a 256 protocol/lock.json
```

Regenerate only from a checkout at the locked revision:

```bash
npm run protocol:sync -- /absolute/path/to/teslatlas-protocol
npm run protocol:generate
npm run protocol:check
```

The canonical SDK codebase was retained. A separate documentation-only clone
was design input, not an executable branch to merge.

## Public methods

The closed `TeslatlasClient` surface contains exactly these named operations:

| Area | Methods |
| --- | --- |
| Discovery and vehicle query | `discoverHub`, `listVehicles`, `getVehicleCurrentState` |
| Drives and positions | `listVehicleDrives`, `getDrive`, `listDrivePositions` |
| Charges | `listVehicleCharges`, `getCharge`, `listChargeSamples` |
| State and update history | `listVehicleStates`, `listVehicleUpdates` |
| Events and data quality | `streamEvents`, `listDataQuality` |
| Commands | `createCommand`, `getCommand` |
| Metadata | `listVehicleMetadata`, `createMetadata`, `getMetadata`, `replaceMetadata`, `deleteMetadata` |

Browser and Node.js factories are available from `@teslatlas/sdk/browser` and
`@teslatlas/sdk/node`. The root entry point exports public types and safe
constructors only; it does not export an arbitrary route executor or a factory.

## Results, metadata, and errors

Successful reads use `ReadResult<T>`. A `200` is `modified` and includes a
validated value; a `304` is `not-modified` and never invents an empty value.
Successful writes use `WriteResult<T>`. Both result shapes carry safe response
metadata: status, ETag where supplied, Location where supplied, request ID
where valid, and protocol version where supplied.

All JSON GET responses require a protocol-valid ETag. `If-None-Match` values
are ordinary opaque ETags, limited only on the outgoing request boundary.
Metadata entity reads and mutations require strong quoted ETags; metadata
replacement and deletion send the caller-provided `If-Match` unchanged.

`ProtocolValidationError` represents an invalid local/protocol boundary.
`ProtocolHttpError` represents a validated problem response. Capability and
profile failures use typed errors. Error objects retain safe scalar fields only:
no raw body, header collection, authorization value, credential-bearing URL, or
arbitrary cause is retained.

## Pagination and events

Opaque cursors are passed unchanged. Page iteration stops on a missing or
repeated cursor and does not merge or retry pages.

`streamEvents` validates known event payloads and ignores unknown event names
before parsing. It sends replay state strictly after the last accepted event.
An empty event ID clears the checkpoint. A yielded event is saved only when the
consumer asks for the next one, preserving at-least-once replay. `204` ends the
stream without reconnecting; replay expiry is a typed terminal failure.

Checkpoints and credentials are caller-owned. Scope every checkpoint store to
the caller's authorization principal and stable event filters. Reusing one in a
different scope can yield the terminal protocol event-ID failure and is not
retried by the SDK.

## Commands

`createCommand` requires a caller-supplied UUID `Idempotency-Key` and makes one
dispatch. It never generates, stores, retries, or reissues the key. A
conforming problem response remains `ProtocolHttpError`. If dispatch began and
no conforming response arrived because of an abort or transport failure, the
result is cause-free `CommandUncertainError`; callers must decide what to do
next. `getCommand` provides typed status reads with ordinary conditional ETag
support.

## Evidence boundary

SDK protocol-case evidence is not server conformance. The local Node and
Chromium suites show that this package maps the vendored cases through its
public API consistently. They do not establish the behavior of any remote
deployment.

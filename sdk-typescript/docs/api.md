# SDK API reference

## Entry points

Use `@teslatlas/sdk/browser` or `@teslatlas/sdk/node` for `createClient`.
The root `@teslatlas/sdk` entry point intentionally has no factory; it exports
public types, safe errors, opaque constructors, and caller-owned interfaces.

```ts
import { createClient } from "@teslatlas/sdk/node";
import { asEntityTag } from "@teslatlas/sdk";

const client = await createClient({
  baseUrl: "https://hub.example.invalid",
  authorization: () => undefined,
});

const vehicles = await client.listVehicles({ ifNoneMatch: asEntityTag('W/"saved"') });
```

`CreateClientOptions` accepts a bootstrap URL, an authorization provider, an
optional injected Fetch implementation, an optional requested profile, and an
abort signal. Discovery is public and unauthenticated; authenticated operations
ask the provider immediately before dispatch.

## Results and errors

JSON reads return `ReadResult<T>`: either `modified` with a validated `value`,
or `not-modified` for `304`. Both carry safe `ResponseMetadata`, including the
HTTP status and validated ETag when supplied. Writes return `WriteResult<T>`.

Safe failures include `ProtocolValidationError`, `ProtocolHttpError`,
`IncompatibleProtocolError`, `MissingCapabilityError`, `ReplayGapError`,
`CommandUncertainError`, and `TransportError`. They retain only stable typed
fields; they do not retain response bodies, authorization values, or arbitrary
causes.

## Opaque inputs

`asOpaqueCursor`, `asEntityTag`, `asStrongEntityTag`, and
`asIdempotencyKey` construct the protocol-sensitive opaque inputs accepted by
the relevant methods. Cursors and ordinary ETags remain opaque; strong ETags
are required for metadata replacement/deletion; command creation requires a
caller-supplied UUID idempotency key.

## Client methods

The public client has 20 named methods:

- discovery and vehicles: `discoverHub`, `listVehicles`,
  `getVehicleCurrentState`;
- drives, positions, charges, samples, states, and updates:
  `listVehicleDrives`, `getDrive`, `listDrivePositions`,
  `listVehicleCharges`, `getCharge`, `listChargeSamples`,
  `listVehicleStates`, `listVehicleUpdates`;
- events and data quality: `streamEvents`, `listDataQuality`;
- commands: `createCommand`, `getCommand`;
- metadata: `listVehicleMetadata`, `createMetadata`, `getMetadata`,
  `replaceMetadata`, `deleteMetadata`.

The full per-method compatibility, pagination, event replay, and uncertainty
semantics are in [compatibility.md](compatibility.md).

## Caller-owned state

The SDK does not implement credential storage. `AuthorizationProvider` supplies
one complete authorization value per request. `SseCheckpointStore` is also
caller-owned; scope it to the authorization principal and stable stream filters
that own the checkpoint.

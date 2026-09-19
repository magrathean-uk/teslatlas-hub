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

## Current Hub client

The browser and Node entry points also export `createHubClient`, a dedicated
adapter for the frozen `hub-http-v1@1.0.0` profile. It requires one fixed HTTPS
endpoint, the expected Hub UUID, and a caller-owned `HubCredentialStore`.
Its methods are `discover`, `health`, `readiness`, `claimPairing`,
`rotateDevice`, `vehicles`, `current`, and `drives`.

`discover` is always unauthenticated and verifies the returned Hub identity
before an authenticated method can dispatch. `drives` returns either a `page`
or `notModified`; cursors are bound to the vehicle and millisecond filter set.
`claimPairing` serializes its compact JSON request as UTF-8 and rejects bodies
over 4,096 bytes with `ProtocolValidationError` (`HubClaimRequest.size`) before
dispatching the claim request. Claim extractor failures with status `400`,
`415`, or `422` must be nonempty `text/plain`; the SDK exposes them as a
body-free `HubHttpError` with code `hub_http_error` and never retains the
extractor text. Other error responses use the profile's validated JSON shape.
`logout()` aborts pending work, invalidates cached identity, waits for SDK-owned
credential writes already dispatched, and then clears caller credentials. A
failed clear rejects; it is not retried. `dispose()` aborts pending work and
permanently closes that client while preserving caller credentials. If a caller
save was already dispatched, disposal cannot cancel that caller I/O, so callers
that need deletion must await `logout()` before disposal.
Neither operation claims to revoke credentials at the Hub.

Invitation TLS pins are validated as invitation data, but browser Fetch cannot
apply a certificate fingerprint. Browser callers must use their normal trusted
PKI and the invitation's exact endpoint; the SDK never disables certificate
validation or derives a replacement pin from discovery.

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

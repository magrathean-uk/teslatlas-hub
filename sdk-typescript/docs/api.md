# SDK API reference

## Entry points

Use `@teslatlas/sdk/node` for `createHubClient`. The root `@teslatlas/sdk` entry
point intentionally has no factory; it exports public types, safe errors, opaque
constructors, and caller-owned interfaces. There is no browser entry point.

```ts
import { createHubClient } from "@teslatlas/sdk/node";
import { asStrongEntityTag } from "@teslatlas/sdk";

const client = createHubClient({
  endpoint: "https://hub.example.invalid",
  expectedHubId: "11111111-1111-4111-8111-111111111111",
  credentials, // A caller-owned HubCredentialStore.
});

const page = await client.drives(vehicleId, { ifNoneMatch: asStrongEntityTag('"saved"') });
```

`CreateHubClientOptions` accepts one fixed endpoint (HTTPS, or HTTP on loopback), the
expected Hub UUID, a caller-owned `HubCredentialStore`, an optional injected Fetch
implementation, an optional `claimTransport`, and an abort signal. The Node entry
point adds `nodeTls`.

## Hub client

`createHubClient` is a dedicated client for the `hub-http-v1@1.2.0` profile. Its
methods are `discover`, `health`, `readiness`, `claimPairing`, `rotateDevice`,
`vehicles`, `current`, and `drives`.

`discover` is always unauthenticated and verifies the returned Hub identity
before an authenticated method can dispatch. The discovery document must carry
`profile`, so a Hub that predates the 1.2.0 profile is refused. `vehicles`,
`current`, and `drives` also check that discovery advertises the capability each
needs (`query.vehicles`, `query.current`, or `query.drives`) and otherwise fail
with `UnsupportedHubMethodError`. Capabilities are open tokens: a token the
client does not use is accepted and ignored. `drives` returns either a `page` or
`notModified`; cursors are bound to the vehicle and millisecond filter set.

`claimPairing` rejects a device name that is empty, longer than 128 UTF-8 bytes,
or holds a C0 or C1 control character (U+0000 to U+001F, U+007F to U+009F) with
`ProtocolValidationError` (`HubClaimRequest.deviceName`) before any network I/O,
as the profile's Hub would refuse it with 422. It also serializes its compact
JSON request as UTF-8 and keeps the profile's 4,096-byte body bound as a backstop
(`ProtocolValidationError`, `HubClaimRequest.size`); the device-name and secret
rules keep the body far below that bound, so the public API does not reach it.
Claim extractor failures with status `400`, `415`, or `422` must be nonempty
`text/plain`; the SDK exposes them as a body-free `HubHttpError` with
code `hub_http_error` and never retains the extractor text. Other error
responses use the profile's validated JSON shape.

`rotateDevice` saves the successor only after it validates. When the device is at
its absolute lifetime the Hub answers 200 with the current bearer and
`reason: "max_lifetime"`, and issues nothing. The SDK then keeps the stored
bearer, saves the expiry the answer states into the stored credential (only when
it differs from the stored one, so the SDK's expiry follows the Hub's), sends no
second rotate, and returns that claim: branch on
`result.value.reason === "max_lifetime"`, which means the device must be paired
again before `result.value.expiresAtMs`. That save is a compare-and-set inside
the credential-mutation queue: it writes only if the store still holds the same
bearer and device id as the credential the rotation presented, so a pairing
claimed while the rotation was in flight keeps its credential and nothing is
written. Once the stored `expiresAtMs` has passed, the SDK rejects the credential
locally with `ProtocolValidationError` (`HubCredential.expiry`) before it
dispatches any authenticated request, so the expired bearer never reaches the
Hub (an uncached discovery request may still go out first) and the SDK does not
surface a 401 for it; callers must re-pair before that expiry. A
`max_lifetime` answer that carries a different device or a different bearer than
the stored credential is a `ProtocolValidationError` (`HubClaim.deviceId` or
`HubClaim.accessToken`) and nothing is saved. An answer without `reason` behaves
as before.

`logout()` aborts pending work, invalidates cached identity, and revokes the
device at the Hub on a best-effort basis before it clears: when the stored
credential is valid and the Hub advertises `auth.revoke`, it sends one
`POST /v1/device/revoke` with that bearer and no body. The Hub is the one named
by the discovery cached when `logout()` started or, when none was cached, by one
fresh discovery whose Hub identity matches `expectedHubId`; a cached discovery
without `auth.revoke` sends nothing. One 2 second deadline bounds the credential
load, that discovery and the request. A 204 or a 401 counts as revoked (a
repeated revoke is 401 because the bearer is already unusable). Any other status,
a transport failure, the deadline or `dispose()` mean not revoked. There is no
retry. Either way `logout()` then waits for SDK-owned credential writes already
dispatched and clears caller credentials, and it rejects only when that clear
rejects; a failed clear is not retried. A credential load that an operation
starts after `logout()` waits until the clear has finished, and a rotation never
saves a credential derived from a load that ran before the logout. `dispose()`
aborts pending work, abandons a revoke that a pending `logout()` has in flight,
and permanently closes that client while preserving caller credentials. If a
caller save was already dispatched, disposal cannot cancel that caller I/O, so
callers that need deletion must await `logout()` before disposal.

The Node entry point verifies normal CA/hostname trust and the invitation's
lowercase DER leaf SHA-256 on the same new TLS connection before it sends the
claim body. `nodeTls.ca` configures CA bytes for that claim connection; callers
using a private CA must also configure their caller-owned Fetch transport (or
Node process trust) for discovery and later reads. A caller-supplied
`claimTransport` replaces the built-in Node claim transport and must enforce
the same CA, hostname, connected-leaf pin, and pre-request ordering contract.
An absent or malformed invitation pin is rejected before network I/O as
`ProtocolValidationError` with validator `HubInvitation`. Ordinary CA or
hostname failures are exposed as the bounded `TransportError`; a valid TLS peer
whose leaf does not match the invitation is `HubTlsPinMismatchError`. None of
these errors retains the invitation secret or a native TLS cause.

A client built without a claim transport throws `HubTlsPinUnavailableError`
before discovery or claim network I/O. The SDK never disables certificate
validation or pretends a discovery value proves the connected certificate.

## Results and errors

Reads return `HubResponse<T>`: a validated `value` and safe `HubResponseMetadata`
(the HTTP status, the request ID when valid, and the ETag when supplied). `drives`
returns `HubDrivesResult`: a `page` with a validated value and a strong ETag, or
`notModified` for `304`.

Safe failures include `ProtocolValidationError`, `HubHttpError`,
`HubIdentityMismatchError`, `VehicleIdentityMismatchError`,
`UnsupportedHubMethodError`, `HubClientDisposedError`,
`HubTlsPinUnavailableError`, `HubTlsPinMismatchError`, and `TransportError`. They
retain only stable typed fields; they do not retain response bodies,
authorization values, or arbitrary causes.

`HubHttpError.code` is `hub_http_error` for a bare status, or one of the profile
error codes the client maps to a status: `invalid_query`, `invalid_time_range`,
`invalid_limit`, `invalid_cursor`, `vehicle_not_found`, `drive_not_found`,
`charge_not_found`, and `service_unavailable`. The profile lets any lowercase
token through as an error code; a well-formed code the client does not know, or
one whose status does not match, is a `ProtocolValidationError`.

## Opaque inputs

`asStrongEntityTag` constructs the strong ETag that `drives` accepts for
`ifNoneMatch`. `asOpaqueCursor` and `asEntityTag` are also exported; the drives
cursor is passed through unchanged as a string.

## Client methods

The Hub client has eight named methods:

- discovery and health: `discover`, `health`, `readiness`;
- pairing and credentials: `claimPairing`, `rotateDevice`, plus `logout` (one
  best-effort revoke at the Hub, then the local clear) and the local `dispose`;
- reads: `vehicles`, `current`, `drives`.

The profile, result, and pagination semantics are in
[compatibility.md](compatibility.md).

## Caller-owned state

The SDK does not implement credential storage. `HubCredentialStore` loads, saves,
and clears the one credential the client uses. Saves and clears through a single
client are ordered; callers sharing a store across clients must coordinate
rotations and external writes.

## Response resource bounds

Hub JSON and text responses enforce a 1 MiB received-byte limit before parsing,
including chunked bodies with no reliable Content-Length. Cancellation interrupts a
stalled read and cancels its upstream reader.

Drive pages are admitted only within the requested limit and half-open
start-time range, in descending start time and ID order, with matching vehicle
identity. A malformed page is rejected before its cursor is retained.

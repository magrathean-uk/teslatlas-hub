# Teslatlas SDK compatibility

## Contract input and regeneration

The SDK is generated and tested against the Protocol profile `hub-http-v1@1.2.0`.
The generator reads `../protocol/profiles/hub-http-v1/1.2.0` in place. There is no
vendored copy and no lock file. Generation emits only `src/generated/hub-protocol.ts`
(OpenAPI types), `src/generated/hub-validators.js` (standalone Ajv validators), and
`src/generated/hub-validators.d.ts`.

```bash
npm run protocol:generate
npm run protocol:check
```

`protocol:check` verifies that the profile's `SHA256SUMS` lists exactly the files in
the profile directory and that every digest matches, then regenerates into a
temporary directory and compares the three generated files byte for byte with the
committed ones. A change to a profile file makes it fail until the outputs are
regenerated; do not hand-edit generated files.

## Hub profile

The client targets `hub-http-v1@1.2.0`. The validators accept additive fields. Against
the 1.0.0 profile this client was first built for, 1.2.0 changes what the client
checks:

- Discovery must carry `profile` (`hub-http-v1@1.x.y`). A Hub that predates the
  1.2.0 profile does not send it, and the client refuses that discovery.
- Discovery `capabilities` are up to 16 unique tokens matching `^[a-z0-9.]{1,64}$`
  that include `query.vehicles` and `query.current`. Other tokens are accepted.
  `attributions` and `manifestPublicKey` are optional.
- An error `code` is any `^[a-z_]{1,64}$` token. The client maps the codes it knows
  to a status and rejects any other code as invalid. `drive_not_found` and
  `charge_not_found` join the profile's codes.
- A claim or rotation response may carry `reason: "max_lifetime"`. On rotation it means the
  device is at its absolute lifetime and the Hub issued nothing: the SDK keeps the stored
  bearer, saves the answer's `expiresAtMs` into the stored credential when it differs (only
  while the store still holds the presented bearer and device id; a pairing claimed meanwhile
  is never overwritten), does not retry, and returns the claim with `reason` set, which tells
  the caller to re-pair before `expiresAtMs`. After the stored `expiresAtMs` passes, the SDK
  rejects the credential locally with `ProtocolValidationError` (`HubCredential.expiry`)
  before any authenticated request, so the expired bearer is never sent and no 401 reaches the
  caller through the SDK for that case; an uncached discovery request may still go out first.
- Discovery that advertises `auth.revoke` offers `POST /v1/device/revoke` (no request body;
  204, 400, 401, 404 or 503, all with an empty body or one the SDK ignores). `logout()` uses it
  once, best effort; 204 and 401 count as revoked.
- A claim device name is 1 to 128 UTF-8 bytes with no C0 or C1 control character.
- A drive may carry `start_address_provenance` and `end_address_provenance`
  (either may be null); `looked_up_at_ms`, when present, is an exact signed 64-bit
  integer.

Run `npm run test:hub:node` only with the private fixture environment variables
described by the task harness. The lane uses default Fetch from a freshly packed SDK.

For a real Hub run, pair with an invitation produced by Hub's `pair --json`
command and supply the expected Hub UUID from trusted setup context. The Node entry
point validates CA trust and hostname, then compares the invitation `tlsPin` with
the connected leaf DER SHA-256 before request bytes. `nodeTls.ca` affects only that
native claim connection; private-CA users must separately configure the
caller-owned Fetch path used by discovery and authenticated reads. A custom
`claimTransport` must provide the same guarantee. A client without a claim
transport fails with `hub_tls_pin_unavailable` before network I/O. The SDK does not
perform mDNS discovery, background polling, or automatic reconnect/rotation.

The current reproducibility tuple is Node 26.10.0 and npm 12.1.0. The accepted
runtime evidence recorded in `package.json` remains Node 26.7.0 on macOS 27; no
broader Node or operating-system floor is claimed by that evidence. The declared
Node and npm floors are null, which means unaccepted, not unlimited support. The
historical 2026-09-20 Docker receipt records a bounded package-only pass; the
Docker lane itself was retired on 2026-10-08. Neither
record establishes current final-Hub acceptance. New runtime claims need
exact-source receipts.

`logout()` clears the local credential store, and first asks the Hub to revoke the
device when discovery advertises `auth.revoke` (one `POST /v1/device/revoke`, a 2 second
deadline for the load, any discovery and the request, no retry). A 204 or a 401 counts as
revoked; any other outcome means the Hub may still honour the old bearer until it expires.
The local clear happens in every case. Consumers await it before `dispose()` and must pair
again after a 401, a `max_lifetime` rotation answer or a confirmed server revocation. An
explicit discovery refresh invalidates
the cached identity on transport failure or UUID mismatch, so an authenticated
request cannot silently reuse an old discovery result.

The `claimPairing` request is compact JSON measured after UTF-8 serialization.
The SDK rejects a device name outside the profile's rule as
`ProtocolValidationError` (`HubClaimRequest.deviceName`) before dispatching the
claim request. It also enforces the profile's 4,096-byte body bound
(`HubClaimRequest.size`) as a backstop; the device-name and secret rules keep a
body far below that bound, so the public API does not reach it. Hub claim extractor statuses `400`, `415`, and `422` are accepted
only as nonempty bounded `text/plain` responses and surface as a body-free
`HubHttpError` (`hub_http_error`); extractor text is never retained.

## Public methods

The closed `HubClient` surface contains exactly these named operations:

| Area | Methods |
| --- | --- |
| Discovery and health | `discover`, `health`, `readiness` |
| Pairing and credentials | `claimPairing`, `rotateDevice` |
| Reads | `vehicles`, `current`, `drives` |
| Lifecycle | `logout` (best-effort Hub revoke, then local clear), `dispose` (local) |

The Node.js factory is available from `@teslatlas/sdk/node`. The root entry point
exports public types and safe constructors only; it does not export an arbitrary
route executor or a factory.

## Results, metadata, and errors

Successful reads use `HubResponse<T>`, carrying a validated value and safe response
metadata: status, the request ID where valid, and the ETag where supplied. `drives`
uses `HubDrivesResult`: a `200` is a `page` with a validated value and a required
strong ETag, and a `304` is `notModified` and never invents an empty value.

`If-None-Match` values for `drives` are strong quoted ETags, limited on the outgoing
request boundary. `ProtocolValidationError` represents an invalid local or profile
boundary. `HubHttpError` represents a validated profile error response or a bare
status. Error objects retain safe scalar fields only: no raw body, header
collection, authorization value, credential-bearing URL, or arbitrary cause is
retained.

## Pagination

Opaque cursors are passed unchanged. A cursor the client received is bound to the
vehicle and the millisecond filter set that produced it; reusing it with another
vehicle or filter is rejected before any request. The page iteration helper in
`scripts/drive-pagination.mjs` stops on a missing or repeated cursor and does not
merge or retry pages.

## Evidence boundary

SDK fixture evidence is not server conformance. The local Node suites show that this
package maps the profile's examples through its public API consistently. They do not
establish the behavior of any remote deployment.

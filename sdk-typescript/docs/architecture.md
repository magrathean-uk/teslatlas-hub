# TypeScript SDK architecture

## Boundary

`HubClient` is the closed public client surface. It exposes one named method for
each Hub read-API operation the SDK supports and has no arbitrary-path executor.
The root package exports public models, options, errors, opaque-value
constructors, and caller-owned interfaces; the Node.js subpath adds the
`createHubClient` factory. There is no browser entry point.

The implementation retains private response decoding, generated validators, and
Fetch transport primitives inside the package. Those files are emitted because
the client needs them at runtime, but the package export map does not expose
them as consumer subpaths.

## Lifecycle

1. `createHubClient` validates the fixed endpoint and the expected Hub UUID. The
   endpoint is HTTPS, or HTTP on loopback only.
2. `discover` fetches public discovery without authorization or redirect
   following. The document is schema-validated, must carry `profile`, and must
   name the expected Hub UUID before any authenticated method can dispatch.
3. `vehicles`, `current`, and `drives` validate their local inputs and the
   capability discovery advertises for them before the credential store is asked
   for a credential. `rotateDevice` also requires verified discovery first.
4. Fixed request paths encode only declared path, query, header, and body
   inputs.
5. Response values and metadata are decoded into safe typed results or errors.
   Raw bodies, credentials, and arbitrary causes are not retained.

## Runtime

The client accepts an injected Fetch implementation for deterministic tests and
embedding-specific network policy. Claim transport is intentionally separate:
the Node entry point supplies a native same-connection DER leaf-pin check, and a
client without a pin-capable claim transport fails closed before any pairing
network I/O.

The client is a static-endpoint client. It shares safe Fetch transport
primitives where wire semantics match, and uses validators generated from the
in-repository `hub-http-v1@1.2.0` profile. Explicit logout or disposal provides
the required abort and identity reset boundary without inventing a mutable
endpoint API. Logout serializes SDK-owned credential mutations and clears
caller state after those writes; disposal is synchronous, preserves existing
credentials, and rejects stale operation success.

The Node claim transport opens a non-pooled TLS connection, retains Node's CA
and hostname validation, checks the connected leaf DER SHA-256, and only then
sends the secret-bearing request. It never changes global TLS settings. An
injected claim transport is caller-owned and carries the same security
obligation; ordinary injected Fetch remains responsible for non-claim routes.

## Protocol dependency

The contract is read in place from the Protocol package of this repository,
`../protocol/profiles/hub-http-v1/1.2.0`. There is no vendored copy and no lock
file. `npm run protocol:generate` writes the generated types and validators in
`src/generated`; `npm run protocol:check` verifies that profile's `SHA256SUMS`
and that the committed generated files equal a fresh generation. See
[compatibility](compatibility.md) for the profile and regeneration.

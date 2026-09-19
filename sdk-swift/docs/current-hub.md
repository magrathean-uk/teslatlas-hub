# Current Hub Swift product

`TeslatlasCurrentHub` is the independent Swift client for
`hub-http-v1@1.0.0`. It embeds the complete protocol-owned profile and verifies
the exact `SHA256SUMS` bytes against
`b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`
before connecting. It currently admits only Hub product `2026.36.2`.

This binding does not change or reuse the richer `TeslatlasHubSDK` and
`TeslatlasCommands` wire contract. It also does not change the historical
`TeslatlasHubV1Compatibility` adapter or broaden its exact Hub `v1.0.0`
release check.

## Security boundary

An application supplies the HTTPS endpoint, expected Hub UUID and an actor or
other `Sendable` credential store. `CurrentHubCredential` and
`CurrentHubInvitation` redact their descriptions. Bearers and claim secrets
must remain in private storage and must not be logged.

The client validates these values before use:

- endpoint is an HTTPS origin with no credentials, path, query or fragment;
- discovery returns the expected Hub UUID, `teslatlas-sync`, protocol major 1,
  API version `1.0`, tested product version and required capabilities;
- every route remains on the original origin;
- invitation endpoint, pairing URI, secret, TLS pin and expiry agree;
- stored credentials have a non-nil device UUID, a 64-character lowercase
  hexadecimal bearer and a future expiry;
- vehicle UUIDs, drive range, limit, cursor and ETag satisfy profile bounds.

The default maximum response body is 1 MiB. Redirects are returned without
following. Apple transports use an ephemeral URLSession and Security trust
evaluation with caller-supplied DER anchors and leaf-certificate SHA-256.
Linux transports call system libcurl directly with system CA and hostname
validation, a 64 KiB response-header bound, streamed body bound, cancellation,
HTTP(S)-only protocols, disabled redirects, a 20-second connection ceiling and
the request timeout capped at 30 seconds. Informational and proxy response
blocks are discarded, trailers cannot replace final metadata, and singleton
headers such as Content-Type and ETag are normalized case-insensitively and
rejected when conflicting. Repeated list-valued fields such as Cache-Control,
Transfer-Encoding and WWW-Authenticate are preserved in wire order as a
comma-separated field value.

The pairing secret is sent only through a
`CurrentHubInvitationPinningTransport`. Apple Security and the supported Linux
OpenSSL callback both compare the invitation's SHA-256 of the complete DER leaf
certificate during the claim connection's TLS handshake, while retaining
normal CA and hostname validation. A custom HTTP transport without this
explicit ownership fails before the claim request. Linux builds require an
OpenSSL-backed libcurl plus libssl/libcrypto development headers and runtime
libraries. An isolated live environment installs its private CA in that
environment's trust store; caller-supplied DER anchors remain Security-only.

## API surface

The client supports discovery refresh, health/readiness, single-use pairing
claim, bearer rotation, vehicles, current state and drives. Current state and
drive models preserve signed 64-bit identifiers, wire units, nulls and zeroes.
Drive pagination uses descending `(start_date_ms, id)` ordering, opaque cursors,
limits from 1 through 500, inclusive `from_ms`, exclusive `to_ms`, ETags and
typed `304` results.

Charges, commands, event streams, metadata, paired-device administration and
data-quality endpoints are unavailable under this profile. Calling `require`
for one of these operations throws `capabilityUnavailable` without network
I/O.

## Live acceptance

`CurrentHubLiveTests` requires `TESLATLAS_CURRENT_HUB_LIVE_CONFIG`. The JSON
file names the endpoint, expected Hub and selected vehicle UUIDs, private
invitation and certificate paths, receipt path, client platform, and on Linux
an untrusted TLS endpoint. A selected run fails if these inputs, expected
records, multiple pages, ETags or negative cases are missing.

The live journey uses the production transport and real Hub process. It covers
discovery, health/readiness, wrong identity, wrong trust, claim, five drives in
three pages at limit two, three per-page conditional `304` responses, units,
nulls, zeroes, bearer rotation, old-bearer rejection, invitation replay,
unsupported zero-I/O behavior, cancellation and a streamed oversized response.

G6 accepted the maintained external consumer against a wholly fresh
source-built synthetic Hub on macOS 27.0 arm64 with Swift 6.4. The accepted
journey covered the exact discovery/profile/product/capabilities, normal TLS,
claim and replay rejection, health/readiness, two current results, drive pages
`2/2/1` and three `304`s, rotation, old-token rejection, post-rotation readback,
and Hub restart continuity. See the
[Swift G6 receipt](development/g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json)
and [Hub G6 receipt](../../hub/docs/development/g6-macos-arm64-swift-hub-acceptance-2026-09-19-r2.json).

[G3 r2](../../teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json)
admitted the exact product `2026.36.2` compatibility record and profile digest
after G6 (receipt SHA-256
`5df27073463ca985f409332a043b5f46aa753ba4b1b65fe214bc434521ef1865`).
This remains current-macOS synthetic source-built evidence.
The declared iOS 17 and macOS 14 package floors were not minimum-floor tested,
and no App, installer, notarization, package service, real Tesla data,
production, source publication/tag, or full-platform acceptance is claimed.

## External consumer and caller lifecycle

`Examples/CurrentHubConsumer` is a small SwiftPM client that imports only
`TeslatlasCurrentHub`. Build it with `swift build -c release` from that
directory, then pass an owner-only JSON configuration file to
`swift run -c release CurrentHubConsumer /private/current-hub-consumer.json`.
The sample uses the production transport and an in-memory actor credential
store, so no bearer or invitation is written to disk. Applications should
replace that actor with an atomic Keychain or file-backed implementation and
clear it on sign-out. Create a new store and client after changing Hub
identity; never carry credentials or cursors between identities.

The SDK does not expose a server-revoke or logout operation. Claim and bearer
rotation are single-use requests: when their network result is uncertain,
resolve the state with the Hub or re-pair rather than retrying automatically.
Cancellation and reconnect remain caller responsibilities around the public
transport and credential store; a custom store that never returns cannot be
bounded by the SDK.

For Linux development, `Dockerfile` provides an unprivileged Swift 6.0.3
environment with CA certificates, OpenSSL-backed libcurl and Python for the
fixture server. It is not a Hub runtime or installed-matrix acceptance image.

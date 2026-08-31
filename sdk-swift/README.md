# Teslatlas Swift SDK

Public Swift client for the released Teslatlas Hub protocol.

> **Status:** API foundation for protocol profile `1.2.0`, pinned to
> `teslatlas-protocol@79ced4c7fdc79520ad31d72a0280bf5f3f19f407`.
> Pairing, credential rotation, signed manifests, and pack downloads are not
> implemented because those deployment contracts are not released. The
> language-neutral conformance runner is not yet wired to this SDK, so this
> repository does not claim full protocol conformance.

## Add the package

Use Swift Package Manager and select the product you need:

- `TeslatlasHubSDK` for discovery, read queries, ETags, errors, and events.
- `TeslatlasCommands` for the separate vehicle-command write surface.

The package supports iOS 17+, macOS 14+, and Swift 6. It has no third-party
dependencies.

## Connect and read current state

```swift
import Foundation
import TeslatlasHubSDK

let discoveryURL = URL(
  string: "https://hub.example/.well-known/teslatlas-hub"
)!
let credential = try BearerCredential("provisioned-device-token")
let maximumVersion = TeslatlasProtocolVersion("1.2.0")!

let client = try await TeslatlasClient.connect(
  discoveryURL: discoveryURL,
  maximumProtocolVersion: maximumVersion,
  authorization: credential
)

switch try await client.currentState(vehicleID: "vehicle_example") {
case .modified(let state, let entityTag):
  print(state.batteryLevelPercent as Any, entityTag)
case .notModified(let entityTag):
  print("unchanged", entityTag)
}
```

The discovery origin is trusted by default. If one Hub advertises a different
LAN, VPN, or public origin, approve each origin explicitly:

```swift
let client = try await TeslatlasClient.connect(
  discoveryURL: discoveryURL,
  maximumProtocolVersion: maximumVersion,
  additionalTrustedEndpointOrigins: [
    URL(string: "https://hub-vpn.example")!
  ],
  authorization: credential
)
```

An unapproved origin fails before the bearer credential can be sent. A later
discovery refresh must also retain the pinned Hub identity.

The default transport uses an ephemeral session with cookies, credential
storage, and caching disabled. It stops reading a response above 16 MiB; use
`URLSessionTeslatlasTransport(maximumResponseBytes:)` to choose a lower cap.

## Query released resources

`TeslatlasClient` provides typed conditional GETs for:

- vehicles and current state;
- drives and positions;
- charges and charge samples;
- state intervals and software updates;
- data-quality assessments.

Cursors and ETags stay opaque. Cursor descriptions are redacted. Page and
history bounds are checked against the discovered limits before network I/O.

## Consume event bytes

Build the authenticated request with `eventRequest`, then feed received bytes
to `TeslatlasEventStreamDecoder`:

```swift
let request = try await client.eventRequest(
  lastEventID: savedEventID,
  vehicleID: "vehicle_example"
)

var decoder = TeslatlasEventStreamDecoder()
for chunk in receivedChunks {
  for output in try decoder.append(chunk) {
    if case .event(let event) = output {
      persist(event.eventID)
    }
  }
}
```

The decoder handles fragmented SSE, caps server retry instructions at 30
seconds, ignores unknown event names before JSON decoding, and fails closed on
SSE/envelope/payload identity mismatches. The caller still owns connection
retries, terminal HTTP `204`, and query resynchronisation after replay `410`.

## Submit a command

Import `TeslatlasCommands`. Submission is catalogue-gated, requires an
idempotency UUID, and performs exactly one network attempt. The SDK never
blindly retries a command.

## Verify

```sh
swift package dump-package
swift test
swift build -c release
swift test -c release
swift run TeslatlasHubSDKExample
```

Tests include copies of the released protocol examples with an exact authority
SHA and per-file byte hashes. Protocol artifact validation is a separate gate
in `teslatlas-protocol`; fixture decoding is not a substitute for an SDK
conformance adapter.

## Documentation

- [Architecture](docs/architecture.md)
- [Protocol activation record](docs/protocol-dependency-gate.md)

## Licence

Apache-2.0.

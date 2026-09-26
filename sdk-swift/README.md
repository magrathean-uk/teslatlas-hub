# Teslatlas Swift SDK

Teslatlas Swift SDK is a Swift Package Manager source package with four public
products across three contract families. `TeslatlasCommands` builds on the
strict `TeslatlasHubSDK` models, while the deployed-Hub and current-Hub
products remain separate. Consume it from a source checkout or a reviewed
source snapshot.

## Products

| Product | Use it for |
| --- | --- |
| `TeslatlasHubSDK` | The strict Teslatlas public protocol profile 1.2.0: discovery, conditional reads, history, events, and typed protocol models. |
| `TeslatlasCommands` | The strict protocol command surface, using `TeslatlasHubSDK` protocol models. It submits an advertised command once with an idempotency key and returns the accepted job. |
| `TeslatlasHubV1Compatibility` | The deployed Hub v1.0.0 compatibility surface: discovery, vehicles, current state, and drive pages using an existing bearer. |
| `TeslatlasCurrentHub` | The approved `hub-http-v1@1.0.0` current-Hub surface: discovery, health, readiness, claim, rotation, vehicles, current state, and bounded drive pages. |

The three contract families are intentionally separate. Do not use a type,
route, capability, or conformance claim from one contract as evidence for
another.
The historical Hub v1 product does not implement protocol profile 1.2.0, and
the current-Hub product does not expose the richer protocol or command APIs.

The package declares iOS 17 and macOS 14 floors, uses Swift tools 6.0, and
also contains SwiftPM Linux targets. Linux builds use an OpenSSL-backed system
`libcurl`, `libssl`, and `libcrypto`; those development headers and runtime
libraries are required by the Linux environment. There are no remote Swift
package dependencies.

## Add the package

For a local source checkout:

```swift
dependencies: [
  .package(path: "../teslatlas-sdk-swift")
]
```

For a moving source dependency, select the repository branch explicitly:

```swift
.package(
  url: "https://github.com/magrathean-uk/teslatlas-sdk-swift.git",
  branch: "main"
)
```

Choose a reviewed source snapshot when reproducibility matters. The calendar
product version in `VERSION` and `teslatlasProductVersion` is separate from the Swift
tools version and every wire-contract identity. See
[product versioning](docs/product-versioning.md) and
[source distribution](docs/source-distribution.md).

## Strict protocol client

`TeslatlasHubSDK` discovers a Hub, negotiates the highest protocol version up
to the caller's maximum, and validates endpoint trust before sending bearer
credentials. `TeslatlasClient` provides typed conditional reads for vehicles,
current state, drives, positions, charges, charge samples, state intervals,
software updates, and data quality. Responses preserve ETags and typed
`304 Not Modified` results. Cursors are opaque and query bounds are checked
before network I/O.

```swift
import Foundation
import TeslatlasHubSDK

let client = try await TeslatlasClient.connect(
  discoveryURL: URL(string: "https://hub.example/.well-known/teslatlas-hub")!,
  maximumProtocolVersion: TeslatlasProtocolVersion("1.2.0")!,
  authorization: try BearerCredential("provisioned-device-token")
)

switch try await client.currentState(vehicleID: "vehicle_example") {
case .modified(let state, let entityTag):
  print(state.batteryLevelPercent as Any, entityTag)
case .notModified(let entityTag):
  print("unchanged", entityTag)
}
```

Use `TeslatlasCommands` with a discovered command descriptor and a caller
chosen UUID idempotency key. The client checks the advertised command class,
confirmation requirement, request size, and protocol metadata in the `202`
response. Submission is deliberately one attempt. Reconcile an uncertain
result before issuing another command.

## Deployed Hub v1 compatibility

`TeslatlasHubV1Compatibility` is for an unchanged deployed Hub v1.0.0. It
requires a caller-pinned Hub UUID and an existing 64-character lowercase
hexadecimal bearer. It exposes only discovery, vehicles, current state, and
drive pagination. It does not pair devices, rotate credentials, submit
commands, stream events, or expose inferred routes.

Supply `expectedHubID` and `existingBearer` from your application’s trusted configuration and credential store.

```swift
import Foundation
import TeslatlasHubV1Compatibility

let hub = try await HubV1Client.connect(
  discoveryURL: URL(string: "https://hub.example/.well-known/teslatlas-hub")!,
  expectedHubID: expectedHubID,
  credential: try HubV1BearerCredential(existingBearer)
)

let vehicles = try await hub.vehicles()
if let vehicle = vehicles.first {
  let state = try await hub.currentState(vehicleID: vehicle.vehicleID)
  // Use state in your application; keep vehicle data out of logs.
}
```

The compatibility binding is bundled and hash checked. Its provenance and
deliberately narrow route set are described in
[Hub v1 compatibility](docs/hub-v1-compatibility.md).

## Current Hub client

`TeslatlasCurrentHub` loads and verifies the bundled
`hub-http-v1@1.0.0` profile before discovery. The caller supplies an HTTPS
origin, expected Hub UUID, and a `Sendable` credential store. The client
validates the discovery identity, product version, capabilities, same-origin
responses, route bounds, credential expiry, and opaque drive cursors.

The public operations are `discoveryDocument`, `refreshDiscovery`, `health`,
`readiness`, `claim`, `rotateCredential`, `vehicles`, `current`, and
`drives`. Claim requires a transport conforming to
`CurrentHubInvitationPinningTransport`, so the invitation's certificate leaf
hash is checked on the claim TLS connection before the secret is sent. The
default transport rejects redirects, disables ambient cookies and credential
storage, bounds responses, and uses platform-specific URLSession or
OpenSSL-backed libcurl support. Credential persistence and server revocation
remain application responsibilities.

The maintained external example under
[`Examples/CurrentHubConsumer`](Examples/CurrentHubConsumer/README.md) shows
the current-Hub flow and its owner-only input and output files.

## Verification

See the [development guide](docs/development.md) for the supported local
verification selection and Linux platform-gate commands. SwiftPM build output
and caches should use a scratch path outside the checkout.

Live Hub, installed-matrix, device, minimum-platform, installer, notarization,
real Tesla data, production, and full-platform acceptance require separately
provisioned evidence. A local build or fixture suite does not establish those
claims.

## Documentation and support

- [Architecture](docs/architecture.md)
- [Current Hub profile](docs/current-hub.md)
- [Development](docs/development.md)
- [Protocol dependency gate](docs/protocol-dependency-gate.md)
- [Source distribution](docs/source-distribution.md)
- [Support](SUPPORT.md)
- [Contributing](CONTRIBUTING.md)
- [Security](SECURITY.md)
- [Licensing](docs/licensing.md)
- [Apache License 2.0 text](LICENSE)

## License

This package is licensed under the Apache License 2.0. Preserve the complete
[LICENSE](LICENSE) text, copyright notices, and grant terms when redistributing
the source. See the [licensing guide](docs/licensing.md) for package-specific
attribution guidance.

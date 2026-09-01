# Teslatlas Swift SDK

Public Swift clients for two deliberately separate wire surfaces:

- `TeslatlasHubSDK` and `TeslatlasCommands` implement the strict public
  `teslatlas-protocol` profile `1.2.0`, pinned to
  `79ced4c7fdc79520ad31d72a0280bf5f3f19f407`.
- `TeslatlasHubV1Compatibility` is a narrow compatibility product for the
  unchanged Teslatlas Hub `v1.0.0` deployment surface. It does not claim that
  Hub v1.0.0 implements public protocol profile 1.2.0.

The package supports iOS 17+, macOS 14+, Linux builds under SwiftPM, and Swift
6. It has no third-party dependencies.

## Add the package

Select only the product needed by the application:

| Product | Contract |
| --- | --- |
| `TeslatlasHubSDK` | Strict protocol-1.2 discovery, read queries, ETags, errors, and events |
| `TeslatlasCommands` | Strict protocol-1.2 command surface |
| `TeslatlasHubV1Compatibility` | Deployed Hub v1.0.0 discovery, existing bearer, vehicles, current state, and drive pagination |

The compatibility product has no dependency on either strict protocol target.
Types and routes are not shared across the two contracts.

## Connect to deployed Hub v1.0.0

Use a Hub identity retained from a trusted pairing or provisioning step and an
existing paired-device bearer. This module does not pair devices or rotate
credentials. The deployed bearer wire value is exactly 64 hexadecimal
characters; malformed values fail before network I/O.

```swift
import Foundation
import TeslatlasHubV1Compatibility

let discoveryURL = URL(
  string: "https://hub.example/.well-known/teslatlas-hub"
)!
let pinnedHubID = UUID(
  uuidString: "018f18d2-6f45-7b3c-8a91-3c7286a10d42"
)!
let bearer = try HubV1BearerCredential(existingPairedDeviceBearer)

let hub = try await HubV1Client.connect(
  discoveryURL: discoveryURL,
  expectedHubID: pinnedHubID,
  credential: bearer
)

let vehicles = try await hub.vehicles()
if let vehicle = vehicles.first {
  let state = try await hub.currentState(vehicleID: vehicle.vehicleID)
  print(state.batteryLevel as Any)

  let result = try await hub.drives(
    vehicleID: vehicle.vehicleID,
    query: HubV1DriveQuery(limit: 100)
  )
  if case .modified(let page, let entityTag) = result {
    print(page.items.count, entityTag)
  }
}
```

Before a bearer enters a request, the client verifies the bundled binding,
exact discovery path, HTTPS requirement, pinned Hub UUID, immutable Hub release
identity, capability set, route template, query bounds, and same origin. The
default transport rejects redirects and uses an ephemeral session without
cookies, ambient credentials, or caching. Plain HTTP is accepted only for a
loopback origin.

This compatibility product deliberately exposes no pairing claim, bearer
rotation, events, commands, metadata endpoint, charges endpoint, manifests,
packs, positions, states, updates, or inferred routes. A Hub may advertise
`sync.packs`; the product still does not expose it.

The protocol repository had not released a deployed-Hub binding when this
module was produced. Following an explicit instruction to retrieve the needed
public material, the repository vendors a hash-pinned machine-readable binding
audited from the immutable Hub `v1.0.0` tag and records the exact source commit
and Git blob identities. This remains separate from protocol authority. See
[Hub v1 compatibility](docs/hub-v1-compatibility.md).

## Use strict protocol 1.2

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

The strict client supports typed conditional reads for vehicles, current state,
drives, positions, charges, charge samples, state intervals, software updates,
and data quality. Its SSE decoder and separate command product remain protocol
1.2 features; they are not available through the Hub-v1 compatibility import.
Cursors stay opaque and redacted. Bounds are checked before network I/O.

The strict default transport uses an ephemeral session with cookies, credential
storage, and caching disabled. Apple platforms stop reading a response above
16 MiB. FoundationNetworking platforms enforce the same decoded body limit
after URL loading returns because streamed `URLSession.bytes(for:)` is not
available there.

## Verification

```sh
swift package dump-package
swift test
swift build -c release
swift test -c release
```

Offline compatibility tests verify the binding hash, deterministic fixture
hashes, exact routes, pagination, response shapes, redaction, and pre-credential
failure paths. They do not prove a live Hub journey. Run the separate opt-in
black-box test only against an unchanged Hub v1.0.0:

```sh
TESLATLAS_HUB_V1_DISCOVERY_URL='https://hub.example/.well-known/teslatlas-hub' \
TESLATLAS_HUB_V1_EXPECTED_HUB_ID='018f18d2-6f45-7b3c-8a91-3c7286a10d42' \
TESLATLAS_HUB_V1_BEARER='<64-character-hex-bearer>' \
swift test --filter LiveHubBlackBoxTests
```

## Documentation

- [Architecture](docs/architecture.md)
- [Hub v1 compatibility](docs/hub-v1-compatibility.md)
- [Protocol and deployment activation record](docs/protocol-dependency-gate.md)

## Licence

Apache-2.0.

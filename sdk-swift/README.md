# Teslatlas Swift SDK

Public Swift clients for three deliberately separate wire surfaces:

- `TeslatlasHubSDK` and `TeslatlasCommands` implement the strict public
  `teslatlas-protocol` profile `1.2.0`, pinned to
  `79ced4c7fdc79520ad31d72a0280bf5f3f19f407`.
- `TeslatlasHubV1Compatibility` is a narrow compatibility product for the
  unchanged Teslatlas Hub `v1.0.0` deployment surface. It does not claim that
  Hub v1.0.0 implements public protocol profile 1.2.0.
- `TeslatlasCurrentHub` implements the approved current-Hub profile
  `hub-http-v1@1.0.0`, pinned to manifest SHA-256
  `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926`
  and admits Hub product `2026.36.2`.
  [G3 r2](../teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json)
  (receipt SHA-256
  `5df27073463ca985f409332a043b5f46aa753ba4b1b65fe214bc434521ef1865`)
  and G6 accepted this exact binding for the bounded external-consumer evidence
  described below.

The package supports iOS 17+, macOS 14+, Linux builds under SwiftPM, and Swift
6. It has no remote Swift package dependencies. Linux builds link an
OpenSSL-backed system `libcurl` plus `libssl`/`libcrypto`; their development
headers and runtime libraries must be installed.

The release-cohort product version is exported as `teslatlasProductVersion`.
See [product versioning](docs/product-versioning.md) for its separation from
the two wire-contract identities above.

## Source-only SwiftPM use

This repository is currently source storage; it does not publish a SwiftPM
release or tag. For local development, point a consumer at a checkout:

```swift
dependencies: [
  .package(path: "../teslatlas-sdk-swift")
]
```

When a moving development dependency is acceptable, use the repository's
`main` branch explicitly:

```swift
.package(url: "https://github.com/magrathean-uk/teslatlas-sdk-swift.git", branch: "main")
```

For reproducible distribution, use a reviewed content-bound source snapshot.
The current G3/G6 receipts do not publish or tag one. The CalVer product
identity, Swift tools version, and wire-contract revisions remain separate.

## Add the package

Select only the product needed by the application:

| Product | Contract |
| --- | --- |
| `TeslatlasHubSDK` | Strict protocol-1.2 discovery, read queries, ETags, errors, and events |
| `TeslatlasCommands` | Strict protocol-1.2 command surface |
| `TeslatlasHubV1Compatibility` | Deployed Hub v1.0.0 discovery, existing bearer, vehicles, current state, and drive pagination |
| `TeslatlasCurrentHub` | Current Hub discovery, health/readiness, claim/rotation, vehicles, current state, and bounded drive pagination |

The current and historical compatibility products have no dependency on the
strict protocol targets. Types and routes are not shared across contracts.

## Connect to a current Hub

`TeslatlasCurrentHub` requires an HTTPS origin, the expected Hub UUID retained
from trusted setup, and an application-owned credential store. The client
checks the bundled profile manifest, exact Hub version, identity, capability
set, invitation origin and lifetime, then requires its claim transport to
validate the invitation's certificate pin on the credential-bearing TLS
connection before sending the claim secret.

The product exposes health/readiness, pairing claim and bearer rotation,
vehicles, current state, and drives. Drive cursors remain opaque and redacted;
the client binds them to the exact Hub, vehicle, and time window and
percent-encodes them once. ETags and `304` are typed results. Unsupported rich
features fail before network I/O.

On Apple platforms the production transport uses an ephemeral URLSession with
explicit Security anchors and an optional DER leaf-certificate SHA-256 pin.
On FoundationNetworking platforms it uses the system libcurl API directly,
because Swift 6.0.3 FoundationNetworking crashes while parsing the Hub's valid
bare `WWW-Authenticate: Bearer` challenge. The libcurl path uses the isolated
process trust store, validates hostnames, rejects redirects, bounds headers and
streamed response bodies, honours request timeouts up to the 30-second
resource ceiling, and supports cooperative cancellation. Per-session DER
anchors remain unavailable on that platform, so install the private CA in the
isolated runtime trust store. The OpenSSL verification callback compares the
invitation's complete DER leaf hash during the claim connection's TLS handshake
before any claim secret is sent.

`CurrentHubClient.claim` requires a
`CurrentHubInvitationPinningTransport`. The supplied production transport owns
that guarantee on Apple and supported Linux runtimes. A custom transport that
implements only `CurrentHubHTTPTransport` can perform discovery and ordinary
requests, but claim fails before transmitting a secret-bearing request.

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

The protocol repository does not own this historical deployed-Hub binding. The
repository vendors a hash-pinned machine-readable binding audited from the
immutable Hub `v1.0.0` source identity and records the exact source commit and
Git blob identities. This remains separate from protocol authority. See
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
storage, and caching disabled. Apple and FoundationNetworking platforms stop
reading a response above 16 MiB. The Linux loader streams into a bounded
accumulator and rejects redirects.

## Verification

```sh
swift package dump-package
swift test --skip CurrentHubLiveTests --skip CurrentHubMatrixWorkerTests --skip LiveHubBlackBoxTests
swift build -c release
swift test -c release --skip CurrentHubLiveTests --skip CurrentHubMatrixWorkerTests --skip LiveHubBlackBoxTests
```

The package also includes a minimal external consumer under
`Examples/CurrentHubConsumer`. It imports only the public current-Hub product
through SwiftPM and prints discovery, readiness, vehicle and query counts.
Build it from that directory with `swift build -c release`; run it with an
owner-only JSON configuration path:

```sh
swift run -c release CurrentHubConsumer /private/current-hub-consumer.json
```

G6 accepted one invocation of this maintained external example after an exact
release build on macOS 27.0 arm64 with Swift 6.4. The journey verified exact
discovery/profile/product/capabilities, normal TLS, claim and replay rejection,
health/readiness, two current results, drive pages `2/2/1` with three `304`s,
credential rotation, old-token rejection, post-rotation readback, and Hub
restart continuity. Evidence is in the
[Swift G6 receipt](docs/development/g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json)
and paired [Hub G6 receipt](../hub/docs/development/g6-macos-arm64-swift-hub-acceptance-2026-09-19-r2.json).

The configuration requires `endpoint`, `expectedHubID`, `deviceName`, and
`invitationPath`; `vehicleID`, `driveFromMs`, `driveToMs`, `caPath`, and the
paired restart-marker paths are optional. Invitation and CA files stay outside
the repository. The sample
intentionally keeps credentials in an in-memory actor store; an application
should replace it with an atomic
Keychain or file-backed actor and clear that store on sign-out. Claim and
rotation are single-use operations: an uncertain network result must be
resolved by the server or by re-pairing, never by an automatic retry. Server
revocation remains a Hub operation.

This acceptance is a source-built synthetic external-consumer result on current
macOS 27 Apple silicon. The declared iOS 17 and macOS 14 package floors were
not minimum-floor tested. It is not App integration, installer, notarization,
package-service, real Tesla data, production, or full-matrix acceptance. The
source remains unpublished and untagged by this work.

For a reproducible Linux build and non-live test environment, use the local
Docker recipe. It uses the official pinned `swift:6.0.3-jammy` multi-architecture
manifest (amd64 and arm64/v8), runs as an unprivileged user, and installs the
OpenSSL-backed curl and Python libraries needed by the test fixtures:

```sh
docker build -t teslatlas-swift-sdk .
docker run --rm teslatlas-swift-sdk
docker run --rm teslatlas-swift-sdk swift build -c release
```

This image is a build/test environment and does not start a Hub. Live tests
remain opt-in and require private mounted inputs and a reachable trusted Hub;
the container's localhost is not the Mac host.

The current-Hub live test is mandatory when selected and reads all inputs from
an owner-only JSON configuration file. It fails when configuration or expected
data is absent:

```sh
TESLATLAS_CURRENT_HUB_LIVE_CONFIG=/private/current-hub-live.json \
swift test --filter CurrentHubLiveTests
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
- [Current Hub product](docs/current-hub.md)
- [Protocol and deployment activation record](docs/protocol-dependency-gate.md)

## Licence

Apache-2.0.

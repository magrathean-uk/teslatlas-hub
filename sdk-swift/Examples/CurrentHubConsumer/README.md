# CurrentHubConsumer

This directory contains a small SwiftPM consumer for `TeslatlasCurrentHub`.
It also contains `FourLibraryContractConsumer`, a reference-only executable
that imports all four public products across the three contract families
and does not use a network connection.

## Build and run

Build from this directory after checking out the parent package:

```sh
CONSUMER_SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-sdk-consumer.XXXXXX")"
swift build -c release --scratch-path "$CONSUMER_SCRATCH"
swift test --scratch-path "$CONSUMER_SCRATCH"
swift run --scratch-path "$CONSUMER_SCRATCH" FourLibraryContractConsumer
```

Use a dedicated test Hub and invitation for the live journey below: it claims a device, attempts an invitation replay and rotates its bearer. Keep the scratch variable from the build step in the same shell.

`CurrentHubConsumer` needs one JSON configuration path:

```sh
swift run -c release --scratch-path "$CONSUMER_SCRATCH" CurrentHubConsumer /private/current-hub-consumer.json
```

The consumer requires an HTTPS origin, a caller-pinned Hub UUID, a private
invitation JSON file, a device name, expected vehicle counts, and two new
owner-only output paths. The configuration file and invitation are bounded to
64 KiB. They must be regular files owned by the current user, without group or
other permissions, and are opened without following symlinks. The output
paths are created exclusively with mode `0600`.

## Configuration

Required keys are:

```json
{
  "endpoint": "https://hub.example",
  "expectedHubID": "11111111-1111-4111-8111-111111111111",
  "invitationPath": "/private/current-hub-invitation.json",
  "deviceName": "my-mac",
  "expectedVehicleCount": 3,
  "expectedObservedCount": 1,
  "expectedAbsentCount": 2,
  "semanticSnapshotPath": "/private/semantic-snapshot.json",
  "cleanupDeviceIDPath": "/private/claimed-device-id"
}
```

`expectedObservedCount + expectedAbsentCount` must equal
`expectedVehicleCount`. Optional keys are `caPath`, `expectedDriveCounts`,
`deriveLatestDriveBoundaryChecks`, `vehicleID`, `driveFromMs`, `driveToMs`,
`restartReadyPath`, and `restartContinuePath`. The two restart paths must be
provided together. Drive bounds must be a non-negative half-open interval.

On Apple platforms, `caPath` is a private DER trust-anchor file for the
URLSession transport. On Linux, the production transport uses the isolated
process trust store through OpenSSL-backed libcurl, so configure the CA there
and omit `caPath`.

## What the journey checks

The executable imports only `TeslatlasCurrentHub`. It checks the approved
discovery profile, claims once with invitation leaf-pin validation, rejects an
invitation replay, checks health and readiness, reads vehicles and current
state, walks bounded drive pages, and exercises ETags. It rotates the bearer
once and verifies that the old credential no longer works. When both restart
paths are configured, an external supervisor can restart the Hub while the
consumer waits, after which the consumer verifies the same identity and a
redacted semantic snapshot.

The credential is held in an in-memory actor store for the process lifetime.
The sample does not persist credentials or revoke a device on the server. A
real application should provide its own protected `CurrentHubCredentialStore`,
clear it on sign-out, and resolve uncertain claim or rotation results with the
Hub instead of retrying a single-use operation.

Without drive overrides, the sample expects drive counts `[5, 0, 0]` in the source-defined synthetic time window. Configure a matching fixture Hub before running it. The sample does not create that Hub or provision its invitation. A successful local build or run is
component evidence and does not establish minimum-platform, iOS, application,
installer, notarization, package-service, real-data, production, or
full-platform acceptance. See the package's [current-Hub documentation](../../docs/current-hub.md)
for the contract and trust model.

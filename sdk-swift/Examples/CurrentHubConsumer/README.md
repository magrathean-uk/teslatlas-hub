# External consumers

This external SwiftPM package has two deliberately separate executables:

- `FourLibraryContractConsumer` imports all four public library products and
  exercises one public type from each contract family without network I/O.
  Its output keeps rich Protocol 1.2, deployed Hub v1.0.0 compatibility, and
  `hub-http-v1@1.0.0` distinct.
- `CurrentHubConsumer` is the live current-Hub journey. It imports only
  `TeslatlasCurrentHub`, uses the SDK's production transport, and keeps its
  credential in an in-memory actor store.

Run the reference-only executable before a Hub is available:

```sh
swift run FourLibraryContractConsumer
```

Its four output lines are compile-and-run evidence for public consumption, not
wire-conformance or live-Hub acceptance. Rich Protocol and deployed-Hub
behavior remain covered by their own deterministic SDK tests; only
`CurrentHubConsumer` is intended to connect to the current native Hub.

A fresh claim is required on each live run because the store starts empty; the
sample rotates that credential at the end of the journey.

Build and test it from this directory:

```sh
swift test
swift build -c release
swift run -c release CurrentHubConsumer /private/current-hub-consumer.json
```

The configuration file must be a regular file owned by the current user with
no group or other permissions (normally mode `0600`). It is bounded to 64 KiB.
The invitation path is required and is read with the same owner and mode
checks, without following symlinks, and with a 64 KiB bound. The invitation
must remain private because it contains the one-use claim secret and its
pairing URI.

The JSON shape is:

```json
{
  "endpoint": "https://hub.example",
  "expectedHubID": "11111111-1111-4111-8111-111111111111",
  "invitationPath": "/private/current-hub-invitation.json",
  "caPath": "/private/current-hub-ca.der",
  "deviceName": "my-mac",
  "expectedVehicleCount": 3,
  "expectedObservedCount": 1,
  "expectedAbsentCount": 2,
  "expectedDriveCounts": [5, 0, 0],
  "deriveLatestDriveBoundaryChecks": false,
  "semanticSnapshotPath": "/private/semantic-snapshot.json",
  "cleanupDeviceIDPath": "/private/claimed-device-id",
  "vehicleID": "11111111-1111-4111-8111-111111111111",
  "driveFromMs": 1788565900000,
  "driveToMs": 1788566200001,
  "restartReadyPath": "/private/restart-ready",
  "restartContinuePath": "/private/restart-continue"
}
```

`caPath` and `vehicleID` are optional. The three expected counts, both output
paths, and the exact canonical drive window are required. The observed and
absent counts must add up to the vehicle count. Both output paths must be new:
the consumer creates regular owner-only mode-`0600` files with no-follow and
exclusive-create semantics. Immediately after a successful claim it writes
only the claimed device UUID to `cleanupDeviceIDPath`, so the operator can
revoke that exact device even if a later check fails. The canonical redacted
semantic JSON is written to `semanticSnapshotPath`; it contains no Hub,
vehicle, device, token, cursor, address, coordinate, display-name or endpoint
value. The sample requests a limit of two and follows at most three pages,
rejecting a repeated cursor or a page that exceeds either bound.

The restart paths are also optional, but must be supplied together. When they
are present, the consumer creates the owner-only ready marker after rotation
and waits up to 30 seconds for an owner-only file containing `continue`. A
supervisor can restart the same Hub store while the consumer waits. The
consumer then verifies the same Hub identity, product version, readiness,
vehicle identities and rotated credential. It re-fetches current data and all
pages for the configured history and boundary windows, rejects any canonical
semantic change or loss, and writes the fresh post-restart snapshot. Console
output identifies that fresh snapshot and reports its SHA-256 on Apple
platforms.

On Apple platforms, `caPath` is a bounded owner-only DER file passed to
`CurrentHubURLSessionTransport` as a Security trust anchor. Normal hostname
and certificate validation and the invitation leaf pin remain enabled. On
Linux, the production transport uses the isolated process trust store through
OpenSSL-backed libcurl, so a configured DER path is rejected; install the CA
in that process trust store and omit `caPath`.

The default synthetic profile retains the canonical three-vehicle fixture, one
observed and two absent current results, drive page counts 2/2/1, exact
half-open history-boundary counts 3/2, and the 2,959-byte snapshot SHA-256
`20b164b499673ba207136dcb0107d8c8d74170029721125442914b99f3ae173a`.
The optional drive-count and derived-boundary fields support an owner-authorized
real-data profile without changing that default. An all-zero `expectedDriveCounts`
list matching the vehicle count skips the synthetic history-boundary windows so a
live current-only Hub can still re-fetch current/history and apply the restart
lost-data comparison. The accepted 2026-09-20 Mac
profile used four vehicles, current counts 1/3, drive counts `[4, 0, 0, 0]`,
derived boundary counts 3/1, and produced the same 3,429-byte redacted snapshot
across Node, Firefox, and Swift with SHA-256
`621d88a12417871a8b28d33933083b4e1a7376a4b7e9049d9381bdcc699ba8c8`.
Both profiles require invitation replay rejection, old-bearer rejection,
post-rotation success, and the configured restart checkpoint.
Console output contains only operation statuses and counts. It does not print
bearer tokens, invitation data, cursors, vehicle identifiers, vehicle records,
or battery values. Failures are reduced to a safe status category. Claim and
rotation are single-use operations; an uncertain network result must be
resolved with the Hub or by re-pairing instead of being retried automatically.

`ConsumerLifecycle.execute` cancels and awaits the journey task before it
clears the in-memory store. Changing Hub identity requires constructing a new
session, store, and client; this sample does not expose server revocation or a
persistent credential store.

G6 accepted one exact release build and one invocation of this maintained
example for product `2026.36.2` on macOS 27.0 arm64 with Swift 6.4 against a
wholly fresh source-built synthetic Hub. The accepted checks were exact
discovery/profile/product and capabilities, normal TLS, claim and invitation
replay rejection,
health/readiness, two vehicles with one observed and one absent current result,
drive pages `2/2/1` with three `304`s, rotation, old-token rejection,
post-rotation readback, and Hub restart continuity. See the
[Swift G6 receipt](../../docs/development/g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json)
and [Hub G6 receipt](../../../hub/docs/development/g6-macos-arm64-swift-hub-acceptance-2026-09-19-r2.json).

This does not make private inputs reusable. Every new live run still requires a
fresh trusted endpoint, owner-only configuration, invitation, and any required
CA. The result is not minimum-floor, App, installer, notarization,
package-service, real Tesla data, production, source publication/tag, or
full-platform acceptance.

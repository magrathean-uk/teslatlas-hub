# Current Hub consumer

This is a small SwiftPM executable that imports the public
`TeslatlasCurrentHub` product. It uses the SDK's production transport and
keeps its credential in an in-memory actor store. A fresh claim is required on
each run because the store starts empty; the sample rotates that credential at
the end of the journey.

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
  "vehicleID": "11111111-1111-4111-8111-111111111111",
  "driveFromMs": 1788560000000,
  "driveToMs": 1788570000000,
  "restartReadyPath": "/private/restart-ready",
  "restartContinuePath": "/private/restart-continue"
}
```

`caPath`, `vehicleID`, `driveFromMs`, and `driveToMs` are optional. The drive
window must be supplied as a pair; every page reuses that exact window. The
sample requests a limit of two and follows at most three pages, rejecting a
repeated cursor or a page that exceeds either bound.

The restart paths are also optional, but must be supplied together. When they
are present, the consumer creates the owner-only ready marker after rotation
and waits up to 30 seconds for an owner-only file containing `continue`. A
supervisor can restart the same Hub store while the consumer waits. The
consumer then verifies the same Hub identity, product version, readiness, and
rotated credential after restart.

On Apple platforms, `caPath` is a bounded owner-only DER file passed to
`CurrentHubURLSessionTransport` as a Security trust anchor. Normal hostname
and certificate validation and the invitation leaf pin remain enabled. On
Linux, the production transport uses the isolated process trust store through
OpenSSL-backed libcurl, so a configured DER path is rejected; install the CA
in that process trust store and omit `caPath`.

The maintained acceptance journey requires the canonical two-vehicle fixture,
one observed and one absent current result, drive page counts 2/2/1, a 304 for
each page, invitation replay rejection, old-bearer rejection, post-rotation
success, and the optional restart checkpoint when configured. Successful
output contains only operation statuses and counts. It does not print bearer
tokens, invitation data, cursors, vehicle identifiers, vehicle records, or
battery values. Failures are reduced to a safe status category. Claim and
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

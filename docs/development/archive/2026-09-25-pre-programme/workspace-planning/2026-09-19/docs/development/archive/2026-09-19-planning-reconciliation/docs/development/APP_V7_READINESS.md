# App v7 Hub adoption handoff

State: **APP_V7_ADOPTION_READY**. This handoff is outside `app/`; it authorizes
the App owner to begin a separate v7 adoption task. Independent G7 review
accepted the exact bindings, commands, links, evidence boundaries and limits.
It does not modify or certify the App.

## Accepted baseline

All runtime and binding gates G0-G6 are accepted. The supported adoption target
is Hub product `2026.36.2` with these exact public contracts and source inputs:

| Component | Accepted identity |
| --- | --- |
| Hub | HEAD `7fe8cb202c0eb7e291901a9b719d3fbe65c675bc`; G6 content manifest `42f1787eb94ad81595fad1e5795d547633d0fea848735dfd47ab4e86ed53249d` |
| Protocol | HEAD `05225bd5b2f56885025180d68fedf3a42baaa90b` |
| Current-Hub profile | `hub-http-v1@1.0.0`; `SHA256SUMS` SHA-256 `b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926` |
| TypeScript SDK | HEAD `b1cd548fef7ddd26be2637c14bb435480166cef7`; `@teslatlas/sdk@2026.36.2`; tarball `03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c` |
| Edge | HEAD `67650cd989f25835bd185d0d8c0ff4e3c8ff1bb2`; `edge-delivery-v2@2.0.0` digest `e304fb6ebe074ee2e71d35b1f52d408f87fa1f0624b8ebcdba2ca2eb1fced224` |
| Swift SDK | HEAD `51494612db6aa3d0b0dafd08001ce432befa7fbf`; tracked diff `96c8a1edae4ad7b9509347bee9cdf6303708223a958b1ca8794c20e3ef691f95`; content manifest `2109dc00f26e755cd8bc3d17135ca7d2ff24142092f37d573eec3340a1c0ba55`; `Package.swift` `cfae0738fec873d0b5361aae8ddc4f33e37bf2f42ee4e40259e23e23875e898f` |

The authoritative admission record is
[`teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json`](../../teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json).
It binds content fingerprints rather than bare Git HEADs. Protocol and the
TypeScript vendored compatibility record are byte-identical; the TypeScript
protocol lock SHA-256 is
`3b1ef30d07acf7ef62f91ab9d2b0993f9ceb25110bf70c29e895ef9da505ffa3`.

## App adoption boundary

Use the Swift package product `TeslatlasCurrentHub`. Do not use the richer
`TeslatlasHubSDK`, historical `TeslatlasHubV1Compatibility`, or inferred routes
for this integration. The complete maintained Swift guidance is in
[`teslatlas-sdk-swift/docs/current-hub.md`](../../teslatlas-sdk-swift/docs/current-hub.md),
and the wire contract is in
[`teslatlas-protocol/docs/current-hub.md`](../../teslatlas-protocol/docs/current-hub.md).

The current-Hub client supports:

- discovery, health and readiness;
- single-use invitation claim and bearer rotation;
- vehicle listing and current state;
- bounded drive history with opaque cursors, ETags and typed `304` results.

It does not expose commands, events, metadata administration, charges,
paired-device administration, data-quality endpoints, server revoke/logout, or
sync-pack download APIs. `sync.packs` is an advertised delivery capability, not
a query route. Unsupported operations must fail before network I/O.

## Trust and credential ownership

The App owns the Hub HTTPS origin, expected Hub UUID and credential store.
Discovery must match the expected UUID, `teslatlas-sync`, protocol major `1`,
API version `1.0`, product `2026.36.2`, and the capabilities required by the
operation.

For pairing, the Hub owner creates a short-lived invitation with the configured
Hub CLI and writes it directly to an owner-only file:

```sh
umask 077
teslatlas-hub --config "$PRIVATE_CONFIG" pair --json > "$PRIVATE_INVITATION"
chmod 600 "$PRIVATE_INVITATION"
```

The invitation contains a secret and is single-use. Validate normal CA and
hostname trust, the invitation origin and expiry, and its SHA-256 pin of the
complete connected leaf-certificate DER bytes before transmitting the secret.
Never use `-k`, log an invitation/bearer/cursor, put a secret in argv, or retry
an uncertain claim automatically.

On Apple platforms, use explicit Security trust anchors and the optional DER
leaf pin supplied by `TeslatlasCurrentHub`. Store the returned credential in an
atomic Keychain-backed actor or equivalent application-owned store. Rotation
replaces the credential: persist the new value atomically, reject the old one,
and resolve an uncertain result with the Hub owner or re-pair instead of
retrying. Clear the store on sign-out. A changed Hub identity requires a new
client, store and cursors.

## Query semantics the App must preserve

- Vehicle and current identifiers are UUIDs. Current may be present or absent;
  absence is not a fabricated zero-valued observation.
- Signed 64-bit identifiers, wire units, explicit nulls and real zeroes remain
  distinct. Do not infer missing values.
- Drive order is descending `(start_date_ms, id)`. `from_ms` is inclusive and
  `to_ms` is exclusive. Limits are `1...500`.
- Cursors are opaque, bound to Hub/vehicle/window, sensitive, and passed back
  unchanged. Do not inspect, persist across Hub identities, or log them.
- ETags are also opaque. A typed `304` means the selected representation is
  unchanged; it is not an empty `200` response.
- Handle documented `400`, `401`, `404`, `415`, `422` and `503` responses by
  category. After `401`, stop using that credential and re-establish owner
  intent; do not loop claim or rotation.

## Maintained external Swift example

The example at
[`teslatlas-sdk-swift/Examples/CurrentHubConsumer`](../../teslatlas-sdk-swift/Examples/CurrentHubConsumer)
imports only `TeslatlasCurrentHub` and uses the production transport. From that
directory:

```sh
swift test
swift build -c release
swift run -c release CurrentHubConsumer /private/current-hub-consumer.json
```

The mode-`0600` configuration shape and restart markers are documented in its
[`README.md`](../../teslatlas-sdk-swift/Examples/CurrentHubConsumer/README.md).
The accepted G6 run used one fresh invocation and proved exact discovery,
normal TLS, claim plus replay rejection, two vehicles, one present and one
absent current result, drive pages `2/2/1`, three conditional `304` results,
opaque cursor continuity, rotation, old-credential rejection, post-rotation
access and Hub restart continuity. The example prints only safe statuses and
counts and keeps the credential in memory; the App must supply persistent
credential storage.

Declared Swift package floors are iOS 17+, macOS 14+ and Swift 6. The accepted
external run used macOS 27 arm64, Swift 6.4 and Xcode 27; it is not a minimum-
floor or full-matrix result.

## Reproduce the accepted public bindings

From the workspace root, the scoped admission/readback is:

```sh
python3 hub/scripts/sync-ecosystem-versions.py \
  --workspace "$PWD" --check-g3

env PATH=/Users/bolyki/dev/teslatlas-lab/tooling/node-v26.7.0-darwin-arm64/bin:/usr/bin:/bin \
  npm --prefix teslatlas-sdk-typescript run protocol:check
```

The G3 checker is deliberately scoped to Hub, Protocol, TypeScript, Edge and
Swift; it does not depend on deferred products.

For a new synthetic native Hub fixture, follow
[`hub/tools/interop/README.md`](../../hub/tools/interop/README.md). Its public
entry points are:

```sh
cd hub
cargo build --locked --release --features interop-fixture \
  --bin teslatlas-hub --example interop_fixture
python3 tools/interop/fixture.py --config /private/fresh-config.json
```

Use a fresh owner-only output directory and invitation for every complete run.
Stop the launcher with SIGINT/SIGTERM and verify its listener and owned child
are gone. Never reuse a closed cohort or its invitation, CA, bearer, cursor,
descriptor or private root. These fixture commands are development evidence,
not an installed service recipe.

The retained Debian ARM64 lab guest is stopped. If later verification needs it,
Hub owns its lifecycle through:

```sh
scripts/dev/vm.sh debian start
scripts/dev/vm.sh debian ssh uname -a
scripts/dev/vm.sh debian stop
scripts/dev/vm.sh status
```

Use only the strict access material and host keys described in
[`VM_ACCESS.md`](VM_ACCESS.md); a new runtime still requires a fresh bounded
handoff.

## TypeScript reference consumer

The accepted G4 lane used the packed package, not workspace imports. The
maintained commands are in
[`teslatlas-sdk-typescript/README.md`](../../teslatlas-sdk-typescript/README.md):

```sh
cd teslatlas-sdk-typescript
npm run build
mkdir -p /tmp/teslatlas-sdk /tmp/teslatlas-hub-consumer
npm pack --pack-destination /tmp/teslatlas-sdk
cp examples/hub/{package.json,node.mjs,index.html,app.js,serve.mjs} \
  /tmp/teslatlas-hub-consumer/
npm --prefix /tmp/teslatlas-hub-consumer install \
  --no-save --package-lock=false --ignore-scripts \
  /tmp/teslatlas-sdk/teslatlas-sdk-2026.36.2.tgz
```

Install the exact tarball into a separate consumer directory. Pass endpoint,
expected Hub UUID and an owner-only invitation file; do not pass a token on the
command line. Browser use additionally requires normal certificate trust and
the exact allowed CORS origin. The accepted lane used Node 26.7.0/npm 11.19.0
and real Chrome 153 on macOS 27 arm64, including trust removal and post-removal
failure.

## Optional Edge path

Edge is optional and does not change the App's query API. Its accepted G5 lane
proved one guarded synthetic receiver record on Debian 13.6 ARM64 through the
encrypted bounded spool to a durable Hub commit and readable projection before
the occurrence-bound ACK. Duplicate pull identity was stable, the record
survived Edge restart, Hub restart preserved the result, and Edge/Hub pending
state drained to zero.

Use the maintained
[`Edge delivery contract`](../../teslatlas-edge/docs/hub-delivery-contract.md)
for mTLS, rotating bearer, at-least-once delivery, sequence/gap and retry rules.
The App consumes only the Hub's committed query projection; it must not speak
the Edge delivery protocol or assume that receiver admission alone means a
committed Hub observation.

## Hub lifecycle and diagnostics

For a source fixture, the fixture launcher owns start/stop and cleanup. For a
future installed deployment, use the platform lifecycle documented in
[`Hub CLI reference`](../../hub/docs/guides/cli.md) and
[`operations runbook`](../../hub/docs/operations/runbook.md). The accepted G1,
G2 and G6 evidence is source-built; it does not accept the package, installer,
systemd/LaunchAgent, upgrade, rollback or removal paths.

Troubleshooting order:

1. Confirm the endpoint is a bare HTTPS origin and the expected Hub UUID came
   from trusted setup.
2. Check discovery product/profile/capabilities before sending a credential.
3. Distinguish TLS trust, hostname and leaf-pin failure from CORS or HTTP auth.
4. Treat an expired/replayed invitation as consumed; create a new one.
5. Treat `401` after rotation as expected for the old bearer, not a reason to
   replay rotation.
6. Keep the original drive window while following a cursor; reject repeated
   cursors and page counts above local/profile bounds.
7. On `503`, preserve state and retry only idempotent reads with bounded caller
   policy. Do not automatically retry claim or rotation.
8. Use bounded redacted `status`/`doctor` output; never attach private configs,
   invitations, tokens, TLS keys, vehicle records or raw cursors.

## Acceptance evidence

| Gate | Accepted evidence |
| --- | --- |
| G0 | [`VM_ACCESS.md`](VM_ACCESS.md) and its retained compact receipt |
| G1 | [`active-debian-arm64-hub-standalone-2026-09-18-r1.json`](../../hub/docs/development/active-debian-arm64-hub-standalone-2026-09-18-r1.json) |
| G2/G4 | [`active-macos-arm64-hub-typescript-g2-g4-2026-09-18-r1.json`](../../hub/docs/development/active-macos-arm64-hub-typescript-g2-g4-2026-09-18-r1.json) and [`macos-arm64-packed-node-browser-g4-2026-09-18-r1.json`](../../teslatlas-sdk-typescript/docs/development/macos-arm64-packed-node-browser-g4-2026-09-18-r1.json) |
| G3 | [`g3-compatibility-admission-2026-09-19-r2.json`](../../teslatlas-protocol/docs/development/g3-compatibility-admission-2026-09-19-r2.json) |
| G5 | [`g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json`](../../teslatlas-edge/docs/development/g5-debian-arm64-edge-hub-acceptance-2026-09-18-r5.json) |
| G6 | [`g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json`](../../teslatlas-sdk-swift/docs/development/g6-macos-arm64-external-consumer-acceptance-2026-09-19-r2.json) and its [Hub peer](../../hub/docs/development/g6-macos-arm64-swift-hub-acceptance-2026-09-19-r2.json) |

## Limits and takeover checklist

Accepted evidence is source-built and synthetic on Debian 13.6 ARM64 and
macOS 27 arm64. It does not cover a real Tesla account or vehicle, production,
named-source parity, installed packages/services, installers, signing,
notarization, publication, minimum OS floors, x86/Intel, a full platform matrix,
Home Assistant, the App, or any deferred product.

The separate App v7 task should start by recording the exact Swift package
source identity above, adding only `TeslatlasCurrentHub`, providing an
application-owned Keychain credential actor, and mapping the typed public
results without changing null/zero/unit/cursor/ETag semantics. It should use a
new owner-approved Hub invitation and test environment. App implementation,
builds, tests and acceptance remain wholly owned by that later task.

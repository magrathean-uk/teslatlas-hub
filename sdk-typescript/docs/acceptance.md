# SDK acceptance record

Date: 2026-09-12

This record covers the current working-tree candidate for `@teslatlas/sdk`
version `2026.36.2`. The source checkout is at `b1cd548fef7ddd26be2637c14bb435480166cef7`
with the task-owned working-tree changes listed below. This file is unpacked
evidence and is intentionally outside the package file whitelist.

## Active owner scope

The current aim is usable packed Node/browser consumers on the existing Debian
13 ARM64 and Apple-silicon Mac paths. Minimum bootstrap, install and service
integration needed for those products is in scope.

x86, x86_64, amd64, Intel Mac, Azure and the full cross-architecture installed
matrix are paused. Their historical schemas, receipts and source selectors are
preserved, but no deferred target may block ARM64/Mac usability and no deferred
target may be built, provisioned, started, tested or cleaned up under this
scope. Named-source TeslaMate parity remains a separate acceptance gap; it is
not a prerequisite for independent package or consumer usability.

The owner restarted development on 2026-09-12 for the active Debian ARM64
and Apple-silicon Mac paths. The prior owner-stopped Viewer fixture and its
credentials are closed and not reusable; the next browser attempt requires a
wholly fresh Hub-owned handoff. x86, Intel Mac, Azure and full
cross-architecture work remain deferred.

Viewer development, packaging, integration and Viewer-specific acceptance are
excluded from this task as of 2026-09-12. Existing Viewer source and receipts
remain retained as historical evidence only and are not current dependencies;
generic public browser SDK work remains in scope.

The source-bound contract for the next generic Apple-silicon browser slice is
[macos-browser-sdk-consumer-proposal-2026-09-12.json](development/macos-browser-sdk-consumer-proposal-2026-09-12.json)
(SHA-256 `57b959da15dfb8b408882bd40e392b5c9256c28b7811f96fe4808fbbbb45e2e7`).
It fixes the packed archive/source identity, local origin `http://localhost:4174`,
Hub fixture port `18521`, normal TLS/CORS/data assertions and split cleanup
ownership. The proposal is not a runtime-start authorization; the next receipt
remains blocked on the SDK route and cleanup result for Hub's wholly fresh handoff.

## Historical local package gates

The required toolchain is Node `v26.7.0` and npm `11.19.0`. The initial
working-tree verification used Node `v26.8.1` and npm `11.19.0` with npm's
forced dev-engine warning because the pinned runtime was not then available;
no dependency or source substitution was made. The current exact-toolchain
receipt is recorded under [M1 exact supported-toolchain reproduction](#m1-exact-supported-toolchain-reproduction).

`npm run verify` passed with:

- format and lint checks;
- TypeScript typecheck and the locked protocol check;
- 271 unit tests across 30 files;
- 25 Node/browser conformance tests;
- 52 protocol tests;
- the fixture Node example;
- an 81-file package inventory (including `docs/docker.md`); and
- clean and stale builds producing identical packs, with strict packed
  declaration consumers passing.

The earlier core-acceptance candidate archive had SHA-256
`75e14e8fa633fe4b17b23296ebd54db63cbd74fda53a872dedfd1d219affbf95` and an
81-member inventory. The protocol-bound claim preflight, claim extractor error
handling, and their public documentation changed after that run, so its
runtime receipts remain historical and are not evidence for the final archive.

The later pre-M1 locally packed candidate had SHA-256
`4602b950690baf7b727b839b7c876cb461f8870b7750eb268383a40672fb1233` and the
same 81-member inventory. A complete member comparison found no additions or
removals and these changed members only: `dist/browser.js`,
`dist/generated/hub-protocol.d.ts`, `dist/hub/client.js`, `docs/api.md`,
`docs/architecture.md`, `docs/compatibility.md`, `docs/product-versioning.md`,
and `docs/protocol-dependency-gate.md`. That candidate passed format, lint,
typecheck, protocol, build, unit (271), conformance (25), protocol (52), package
inventory and clean/stale pack checks under the available Node runtime. It was
subsequently superseded by the M1 archive; installed Hub evidence must use the
M1 digest.

An installed import smoke against that candidate also verified the SDK behavior
itself: an oversized Unicode claim returned
`HubClaimRequest.size` without a Fetch dispatch, and a `400 text/plain` claim
extractor response returned a body-free `hub_http_error` without retaining its
text.

The minimal consumer was installed from that candidate in a separately named
package. A fresh copied Node consumer run against the canonical
`protocol/source/profiles/hub-http-v1/1.0.0/examples` payloads passed with one
vehicle and one drive page, and exercised discovery, health, readiness,
authenticated vehicles/current/drives requests and the expected bearer
credential. The installed package witness recorded entry
`dist/node.js` SHA-256
`83bb4403af395dd1d0b88c3ce9e4b1faf0c01177029323a69e955cd09c2de1c9`, content
manifest SHA-256
`4b1a44f7d2cd0ab6c772642125358703aa25c1efe32d164a119a6fc9d858ded6`, and 81
members; the same installed package's `dist/browser.js` entry SHA-256 is
`7969c32c20f4598c78b78e668cc72642ca78c130e93f97681658e3a8f2738e8e`. The Node
help path and JavaScript syntax checks also passed. The copied loopback browser
server returned 200 for `/` and `/sdk.js`, and 404 for `/config.json`. No
credential or invitation is included in this record.

`hadolint Dockerfile` passes with no findings. A Docker test-target build was
attempted but the local daemon was unavailable at
`unix:///var/run/docker.sock`; no image or container acceptance is claimed.

Task-owned changes in this candidate include `AGENTS.md`, `Dockerfile`,
`.dockerignore`, `package.json`, the current-Hub client and tests, the five
files under `examples/hub/`, the reconciled public documentation, and the two
contract manifests, two raw schemas, two pure validators and source-gate test
under `tools/`. The acceptance record itself remains unpacked.

## Historical B1 real Debian ARM64 Node/browser slice

The current Hub-owned Debian 13 ARM64 fixture accepted the exact packed SDK
archive above. The external packed Node consumer passed discovery,
health/readiness, single-use invitation claim, a nonempty two-vehicle result,
current-state decoding, a bounded two-drive page, and credential rotation
using Node `v26.7.0`, npm `11.19.0`, default Fetch and the fixture's owner-only
CA. Hub verified the redacted receipt and credential were owner-only, then
verified direct-child exit `0`, a mode-`0600` stopped record and port closure.

The Viewer-owned browser lane independently passed one installed smoke on the
same current profile with normal NSS trust and no certificate bypass. It
covered pairing and identity, current-state values and observed-time display,
ordered five-drive paging, empty-vehicle handling, refresh, local-session
clearing and cleanup. The browser lane used Node `v26.7.0`, Playwright
`1.62.1` and Chromium `152.0.7977.82`.

These are real B1 consumer receipts for one disposable Debian ARM64 fixture.
They do not, by themselves, establish the later 51-drive/helper or recovery
receipts, passive collection parity, Docker acceptance, or the Apple-silicon
Mac handoff. The historical full cross-architecture installed matrix is
deferred and does not gate active ARM64/Mac usability.

The SDK-owned Node and browser helpers now consume a bounded, variable-length
drive history and verify the terminal cursor, conditional 304, and cursor
continuation after 304. A Hub-owned T4/T5 descriptor may bind
`drive_limit: 25` and `expected_drive_count: 51` for the required `25/25/1`
history. This is helper readiness only; it does not substitute for the fresh
owner-only descriptor, browser trust/CORS witness, recovery controls, or an
installed-service receipt.

## Historical bounded R1 receipts (retained, out of current scope)

The checked-in Hub status records a fresh coordinator-authorized r13
same-Hub Edge-to-Hub-to-Home-Assistant name-only path. One synthetic
`VehicleName` actor call was ACKed, committed and drained, and the existing
Home Assistant device/display name was observed without a new numeric value,
unit or name sensor. The disposable Edge, Hub and temporary Home Assistant
inputs were cleaned or restored. Hub records three classified ingress
sequences for the one actor datum, so this remains bounded synthetic B2
integration evidence rather than a one-payload/one-sequence, passive-parity,
collection, migration, soak or installed-matrix result. The standalone
coordinator receipt expected by the handoff was not present; the checked-in
Hub status is the source for this summary.

The fresh Viewer receipt
[`2026-09-09-r1-installed-data-state.json`](../../teslatlas-viewer/docs/development/receipts/2026-09-09-r1-installed-data-state.json)
records one installed Debian 13 ARM64 data-state pass using the packaged
Viewer: 51 drives remained visible across `25/25/1` pages after three `304`
responses, unsupported resources made no requests, partial drive failure
retained and recovered data, and clearing the local session canceled delayed
current data without late repopulation. Viewer and Hub cleanup closed `4173`
and `18480` while preserving unrelated installed listeners.

These receipts retire the historical r9 transport failure as the current SDK
blocker. They do not supply the named-source passive-parity receipt. The
historical full cross-architecture installed matrix and its per-cell
SessionInputs/runner receipts are deferred by the owner scope and do not gate
active ARM64/Mac usability.

## Apple-silicon Mac candidate artifact

Hub built a fresh local unsigned Apple-silicon candidate from Hub HEAD
`7fe8cb202c0eb7e291901a9b719d3fbe65c675bc` plus the one-file reviewed
`scripts/tesla-proxy-lock.json` overlay
(`b4b02f44698d4d394d1e3dd1407d25f7223ad475b407eea109b844616c8244d2`). The
candidate contains an arm64 Hub app executable
(`2d5bba42ea16deceaa821677ea1f5a4fd7116f0d1197ee18e17d65875202dc22`), an
arm64 embedded Hub binary
(`895873efe102fa4b2db0d839ea1d413f48b61d44d49d323b43907f7c26eead55`), and
`TeslatlasHub.pkg`
(`c3b50aecce3958bd2b434a5382e178e01f99638a203b214576631c3b4ef085d4`). Its
Go provenance receipt binds Go `1.27.0`, Darwin arm64 and the reviewed lab
toolchain hash.

This is candidate-artifact evidence only. It does not prove installation,
LaunchAgent or user-process lifecycle, a Hub listener, browser trust/CORS,
browser pairing, retained data, restart, or any SDK consumer acceptance. Those
actions remain gated on a fresh Hub-owned runtime handoff.

## Apple-silicon Mac packed Node consumer

Hub supplied one fresh owner-only HTTPS handoff for the r2 Apple-silicon
candidate at `127.0.0.1:18489`. The exact packed archive
(`03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c`) was
installed into a fresh `@teslatlas/sdk@2026.36.2` consumer using Node
`v26.7.0` and npm `11.19.0`. With the supplied CA and normal Node TLS, the
public `test:hub:node` route passed discovery, health/readiness, invitation
claim, two vehicles, five drives over `2/2/1` pages, conditional `304`,
post-`304` cursor continuation, credential rotation and default Fetch.

The receipt recorded `dist/node.js` SHA-256
`83bb4403af395dd1d0b88c3ce9e4b1faf0c01177029323a69e955cd09cf0a75c9`, an
81-member installed content manifest
(`4b1a44f7d2cd0ab6c772642125358703aa25c1efe32d164a119a6fc9d858ded6`), and
Hub ID `a95c379d-3b10-4af0-920a-1907981083c8`. The credential and redacted
receipt were mode `0600`. Hub then terminated only its recorded launcher;
the Hub child exited `0`, the stopped receipt was mode `0600`, and port
`18489` closed. The invitation, CA and descriptor are closed and must not be
reused.

This is real packed Node consumer evidence for the active Apple-silicon path.
It does not establish the separate generic browser origin, CORS, trust,
pairing or retained-data route, named-source TeslaMate parity, or deferred
cross-architecture matrix.

Hub then published a fresh generic-browser handoff at
`127.0.0.1:18521` with a new runtime root, descriptor, CA and pre-paired
mode-`0600` credential. The SDK validated the exact profile, scenario, package,
certificate, origin and loopback ownership before starting one bounded attempt.
The first control setup proved trusted Chrome could not see a profile-local
temporary keychain (`ERR_CERT_AUTHORITY_INVALID`). One corrected setup kept
default macOS `HOME`, started untrusted Chrome before CA import, and proved the
untrusted control rejected the certificate before import; after the exact CA
was added to the login keychain for trusted Chrome, that same untrusted control
accepted it. `test:hub:browser` therefore stopped at its negative-control gate.
No browser API/data/rotation acceptance or receipt was produced, no certificate
bypass was used, and the SDK closed its helper, Chrome controls, temporary
trust, profiles and install. The Mac route now requires a supported separate
trust context; the current credential and runtime tuple are closed and not
reusable.

The source-only correction proposal
[`macos-browser-trust-isolation-correction-2026-09-12.json`](development/macos-browser-trust-isolation-correction-2026-09-12.json)
(SHA-256 `98ebe5a6195db9bff8b8310942be5aa18a40492682f423a56dbcc8a01307519a`)
defines the minimal sequential alternative: prove the untrusted rejection
before CA import, close that control, then import the fresh CA and run the
trusted normal-TLS/CORS/data route. It explicitly removes any simultaneous
trust-isolation claim. The SDK runner now implements this mode with shell-free
JSON trust hooks, actual CDP observations, an independent canonical export path
that must be absent before import, and neutral witness fields. It also invokes
an exact trusted-browser cleanup/port-verification hook on every import-started
failure path. Hub's substantive review is clean; a wholly fresh handoff is
still required before runtime, and no new runtime has been started.

## Active consumer targets: ARM64 and Apple silicon

| Active target | Node consumer | Generic browser consumer | Status |
| --- | --- | --- | --- |
| Debian 13 ARM64 | Packed 51-drive consumer passed; active fresh handoff still required for another route | Historical browser/data-state receipts retained; generic route remains handoff-gated | Active |
| macOS Apple silicon | Packed Node consumer passed against a fresh strict-CA Hub handoff; Hub cleanup verified | Generic browser route remains pending a wholly fresh handoff after the reviewed sequential trust correction; no product-UI integration required | Active; fresh handoff required |
| x86/x86_64/amd64, Intel Mac, Azure | Paused | Paused | Deferred; cannot block active products |

The SDK-owned `test:hub:node` and `test:hub:browser` scripts both stop before
starting a lane when `TESLATLAS_HUB_HTTP_CONFIG` is absent. A concrete Hub
handoff is required before any active guest/runtime action: it must identify the
target, exact package/root, endpoint/trust/pairing controls and cleanup owner.
The current Hub runner command is a user-process harness and cannot be used as
installed-service proof. No synthetic descriptor, mocked Fetch, certificate
bypass, permission change, or quarantined guest is used to fill active cells.

The Apple-silicon packed Node route is admitted and closed. The fresh generic
browser handoff was published and consumed only for the single bounded trust
isolation attempt described above; the route remains unaccepted because the
current macOS Chrome controls cannot provide distinct trusted/untrusted roots.
The source correction is implemented and substantively reviewed by Hub; no
trust or origin bypass is allowed, and the installed runner source branch pins
this archive digest. A wholly fresh handoff is still required before the one
bounded sequential route. The historical six-cell cross-architecture
requirement is deferred and does not gate this scope.

### Installed-admission contract handoff (source-only)

The SDK now publishes two inert, source-owned adapter contracts for Hub's reviewed v2 installed runner:

- `tools/matrix-contract-node.json` binds `typescript_node`, actor `sdk_node`, and `tools/matrix_contract_node.py`.
- `tools/matrix-contract-browser.json` binds `typescript_browser`, actor `sdk_browser`, and `tools/matrix_contract_browser.py`.

Each manifest declares the authoritative Node/browser case IDs from Hub's matrix, the single packed-SDK actor, the strict `typescript-<mode>-raw-v1` schema, and no worker phases. The pure validators require exact actor/source/artifact roles, runtime and installed-member provenance, expected scenario facts, bounded request transcripts, controller sequence bindings, and clean actor teardown. `installed_service_runtime` remains runner-owned and returns `pending/runner_owned_service_runtime` until Hub supplies independent installed-service proof.

Current source hashes (verified after the manifest rebind; recompute before any later admission review):

```text
node manifest:   7800f75b03e046f53e56b7356fac95089aae73ab7719e82ead88f39e45ae3299  tools/matrix-contract-node.json
node validator:  600e1cc363d309e163b87e19498e1daf4ba309c325aeb8402513168ff1c12ef2  tools/matrix_contract_node.py
node raw schema: 5a4ad38caa2dee8432eef0e8dd2f7c00b92db0aa3946eb2dab93c4fc13ad6908  tools/typescript-node-raw-v1.schema.json
browser manifest:   3a04e33415330d2d3ce53d489841f0a28b768d982d2af5f255323d2f0cf6acb9  tools/matrix-contract-browser.json
browser validator:  784d1618efdc6739248426d7ee6538772a3560c1569c903f75fb2af72755bf92  tools/matrix_contract_browser.py
browser raw schema: 638feee0156286a9103148ed775ee126bbdc198f9a8e21210652c1871704415c  tools/typescript-browser-raw-v1.schema.json
```

Recompute these hashes with `shasum -a 256 tools/matrix-contract-*.json tools/matrix_contract_*.py tools/typescript-*-raw-v1.schema.json` before any admission review; the listed values are the current source bindings. Run `python3 -m unittest -v tools/test_matrix_contract.py` and `python3 -m py_compile tools/matrix_contract_node.py tools/matrix_contract_browser.py` for the source gate. The rebind source gate passed 6/6, and Hub's focused installed/registry checks passed 11/11 against these manifest and validator bytes. These files are not package members and do not prove installed runtime acceptance; Hub stages owner-only copies, reviews the launch inventory, and executes the installed Node/browser lifecycle.

A local boundary check copied each three-file contract bundle into a fresh owner-only directory, rebound the manifest paths and hashes to those copies, and loaded it through Hub's `adapter_wire.load_reviewed_contract`; both `typescript_node` and `typescript_browser` loaded with revision `1`. This verifies the common loader interface without claiming any installed runtime result.

### Hub aggregate source-only binding

Hub independently reviewed the SDK D1 handoff and bound only its optional
caller-owned Node/browser developer-resource selectors in the aggregate
`packaging/components.json`. The review rechecked the 254-file frozen source
record, read-only snapshot modes, `hub-http-v1@1.0.0` identity, and the exact
81-member archive with its Node/browser entry hashes. The SDK service selector
remains blocked because this package owns no daemon, listener, service-manager
registration, Hub storage, collector or aggregate activation.

This is source and selector-boundary evidence only. It does not promote an
installed target, consumer process, browser, Docker image, native package,
passive-parity lane or platform lifecycle to acceptance. The sibling Home
Assistant provenance/planning handoff is owned by Home Assistant and Hub; it
does not supply a TypeScript SDK runtime input.

## M1 exact supported-toolchain reproduction

On 2026-09-08, an isolated official macOS arm64 Node `v26.7.0` runtime with
npm `11.19.0` completed `npm ci` and `npm run verify` without `--force` or an
engine override. The verification passed format, lint, typecheck, protocol
lock, 271 unit tests, 25 conformance tests, 52 protocol tests, the fixture
Node example, the 81-member package check, and the clean/stale packed
declaration-consumer gate.

The clean/stale gate and a fresh `npm pack` both produced
`03ddddf132185056d60a490bc5237b3f6213d8e212209cfe111be5e09cf0a75c`.
That differs from the previously Hub-bound archive digest `4602b950…`.
Both archives have the same ordered 81-member inventory and identical
extracted member bytes: `dist/node.js` remains
`83bb4403af395dd1d0b88c3ce9e4b1faf0c01177029323a69e955cd09c2de1c9`,
`dist/browser.js` remains
`7969c32c20f4598c78b78e668cc72642ca78c130e93f97681658e3a8f2738e8e`,
and their declarations also match. The archive-byte difference is therefore
not treated as behavioral equivalence for installed admission.

A separately created external consumer installed the fresh tarball with npm
and resolved the root, Node and browser public imports from its own
`node_modules/@teslatlas/sdk`; public export checks passed. A headless
Chromium page then loaded that consumer's installed `dist/browser.js` and
confirmed both public factories. This is package-consumer evidence only.

Hub has source-bound this exact archive in its fixed TypeScript installed
adapter. Its focused archive-pin test accepts `03dddd…` and rejects
`4602b950…`. The Hub records expected owner-staged 0600 manifest
`480226f91ef676461b171b131a0184aed72d94f1748e28752c4dd0d2164f2618`;
that becomes integrity evidence only when a future installed session verifies
the actual owner-only package root. No installed Hub, Docker, service,
browser-trust, or live acceptance claim is made here.

### Deferred Azure and cross-architecture handoff

Azure Debian, x86/x86_64/amd64 and Intel Mac source-only bootstrap instructions
are retained in historical planning records but are paused by the owner scope
override. Do not provision Azure, use the retained x86 guest, acquire an image,
run a cross-architecture build or execute a deferred lifecycle/test lane. The
active package identity and consumer work continues on the existing Debian 13
ARM64 and Apple-silicon Mac paths after a concrete Hub handoff.

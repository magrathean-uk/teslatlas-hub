# Hub progress

- Fixed the installed Mac app's 30-second false start failure. An unloaded
  RunAtLoad/KeepAlive LaunchAgent was bootstrapped and then immediately killed
  with `kickstart -k`, causing launchd to apply its 30-second throttle while the
  app timed out and showed Attention needed. Start now bootstraps once, loaded
  Start is idempotent, and Restart performs a settled bootout/bootstrap. The
  installer update and rollback paths no longer double-start either. The UI
  waits briefly for collector readiness before refreshing and queues one
  refresh requested while another is in flight, so Stop cannot leave a stale
  running dashboard. Live launchd proof
  reached ready in under one second for Start and Restart, preserved the PID on
  idempotent Start, and left Hub stopped. All 105 AppKit tests and macOS
  packaging checks pass.

- Cmd-L support diagnostics complete — the native Mac app now keeps one bounded
  owner-only app/import log alongside bounded service logs; records safe app,
  Tesla login, SSH discovery/tunnel, migration handover, service, diagnostics,
  and vehicle-command lifecycle events with durations and typed reason codes;
  and explicitly reports an unsafe or unavailable app log. Display, copy, and
  save remove credentials, OAuth material, VINs, vehicle/numeric IDs, names,
  coordinates, email addresses, public/private IP addresses, ANSI, and control
  bytes. Saved reports are created mode 0600 through a nonblocking no-follow
  descriptor and refuse symlinks, FIFOs, and non-user-owned files. SSH
  Docker/sudo failures are
  actionable without retaining server, account, password, or key-path data.
  A failed stale SSH-secret cleanup scan is also recorded with a safe typed
  reason instead of disappearing silently. Foundation, URL, POSIX, and support
  report save failures use stable domain/code identifiers rather than Swift
  runtime type names.
  Dashboard status failures record one warning per failure type and one recovery
  event, so transient service issues are visible without 15-second polling spam.
  Command-L is wired through the standard View menu and works from onboarding
  or the dashboard. All 105 exact-source AppKit tests and Xcode 27 beta static
  analysis pass.

- Cmd-L final acceptance — the app launched directly from the newly expanded
  all-in-one package while the Hub service remained stopped. Cmd-L opened the
  combined app/import/service log window with Refresh, Run Diagnostics, Copy,
  and Save available. The rendered service log contained no raw terminal
  escapes. Redirected Rust logs now disable terminal colour at their source;
  share output also strips ANSI/control sequences and redacts private IPv6.
  Xcode-hosted tests use a per-process temporary log and no longer pollute the
  user's production `app.log`.

- Pinned diagnostic log directories — Cmd-L app/service log reads and app-log
  writes now open a real, owned, non-writable directory descriptor with
  `O_NOFOLLOW`, then open the final log through `openat`. A replaced or
  symlinked log directory cannot redirect support-data reads or writes.

- Entropy failure handling — TeslaMate token nonces, legacy/Fleet/cursor keys,
  credential-recovery nonces, and pairing/device secrets now return typed
  errors if operating-system randomness is unavailable instead of directly
  panicking during setup, recovery, or pairing.

- Fleet refresh continuity — native Fleet Telemetry now keeps its local push
  collector alive and retries after five minutes when the refresh request is
  proven unsent. Any response, timeout, or otherwise ambiguous rotation still
  fails closed instead of risking reuse of a consumed refresh token.

- Active macOS log bounds — the service wrapper now supervises both legacy and
  Fleet modes while checking and compacting both launchd-owned logs every 30
  seconds. This preserves the existing 1 MiB/512 KiB bound without waiting for
  a service restart. Packaging regressions cover startup, active-run, and
  receiver-free legacy supervision.

Status on 2026-08-27: the current Rust 1.98 Hub and native macOS app are
installed, but the Hub LaunchAgent is deliberately stopped. The imported
TeslaMate v4.1.1 store is ready in legacy mode with one vehicle, encrypted
legacy credentials, a 2,065,952,768-byte SQLite catalogue, and the normal
60-second collector interval. The explicit handover gate completed. VPS
TeslaMate 4.1.1 and TeslaMateAPI are both healthy; TeslaMate is again the only
active legacy-token owner. The installed Hub SHA-256 is
`893717f1601419d66737dd6ab88013c0128adbb81f411055d560fbd2c8f6d63b`.
The current local installer SHA-256 is
`137706cb7f72e5452c3e1799309555cda8d56dd62745da78d67251b9c6f3f8d7`.
It is a 66,709,404-byte ad-hoc development package, not a notarized release.

Current verification on 2026-08-28: full locked Rust tests passed (838 library,
50 CLI, one TLS integration, and 3 doc tests; 2 intentional fixtures ignored),
Clippy passed with `-D warnings`, and the optimized release build passed. All
105 AppKit tests and Xcode 27 beta static analysis passed. macOS and Linux
packaging source, macOS release, release-evidence, and dependency-audit gates
passed. The current
package expands with the app and root service payload in their exact paths,
contains no AppleDouble or Finder metadata, and its ad-hoc app signature passes
deep strict verification. The current built app binary SHA-256 is
`388074db4d4b5413d192299ac7228af2c8f67ed29a83d4019844a8845dd068e8`.
Its embedded Hub SHA-256 is
`c9b6176c0ce6602699cd521fe0e8bdebb65b2902a2d27427df6d432ed42043ef`;
the root service payload Hub SHA-256 is
`347ea11b5c1c77e2fd090a5212ec08040cc7e6e5eddb2fd803446823c9e76e36`.
The exact packaged root Hub also passed isolated bootstrap, status, and all
seven doctor checks with a zero-byte WAL and no ANSI output. The final live UI
replay was unavailable because the Mac was locked; the earlier packaged Cmd-L
acceptance and the exact-source 105-test suite remain the UI evidence.
The corrected package upgraded the existing installation without error,
auto-opened the exact app under `/Applications`, installed app binary SHA-256
`388074db4d4b5413d192299ac7228af2c8f67ed29a83d4019844a8845dd068e8`,
and preserved the safely stopped Hub and migration data.

Cmd-L now records Tesla legacy-login start, completion, cancellation, and safe
typed failure codes in addition to the existing SSH discovery, authentication,
tunnel, compatibility, import, setup, service, and diagnostics events. It never
records authorization URLs, callback codes, tokens, account identifiers, SSH
passwords, or server addresses. The exact-source 105-test suite and Xcode
analysis passed after this addition.

Fresh all-in-one package onboarding now checks the installed root Hub's exact
version after credentials are configured. The app requires its embedded Hub to
report the release metadata version, then compares that actual version output
with the installed root Hub. An exact match starts that already-installed
root-owned service instead of attempting a redundant privileged installation.
A missing, stale, or mismatched embedded/installed version still enters the
fail-closed signed update path. Exact-match and mismatch paths are covered for
both Fleet and legacy setup.

Package updates now stop only the console user's exact `Teslatlas Hub` GUI
process before replacing `/Applications/Teslatlas Hub.app`, wait five seconds
for a normal exit, and use a bounded kill fallback for an unresponsive old
process. Postinstall then opens the newly installed app. The helper is exercised
with a real named process, and the packaging gate verifies its scope and order.
The installed LaunchAgent now uses launchd's native 30-second failure throttle,
preventing an invalid configuration or binary from creating a tight restart,
CPU, and log-churn loop. Before each launch, its root-owned supervisor validates
both user-owned service logs and compacts any file over 1 MiB in place to the
newest 512 KiB. Cmd-L therefore keeps useful recent output without retaining an
unbounded launchd log history; packaging and real-file compaction checks pass.
Fleet command-proxy readiness now waits 25 ms after an immediate loopback
refusal instead of busy-spinning for up to ten seconds while the proxy starts
or fails. A focused regression keeps that retry delay inside its bounded
10–250 ms range.
Debian's Hub, command-proxy, and Fleet Telemetry units now stop after five
failed starts within five minutes instead of producing an indefinite crash,
CPU, and journal loop. Normal provider/network retries remain inside Hub.

Successful all-in-one macOS builds now remove only the exact
`hub/target/macos-app` Xcode staging tree after the final package is verified.
Failed builds retain staging for diagnosis. The current build left the package
as the sole distribution artifact, removed the standalone app copy, and left no
Xcode staging directory; package expansion, exact app/root payload paths,
AppleDouble absence, and the app's deep strict ad-hoc signature passed.

The pinned Fleet Telemetry source archive is now retained only as one verified
836 KiB file under the normal `hub/target/upstream-cache`. Every build rechecks
its locked SHA-256 before extraction; missing or corrupt content is downloaded
to a same-directory temporary file and atomically published. This removes the
repeated network download without retaining another source tree or target.

Current macOS reliability pass: the dashboard now exposes a native selector
for every configured vehicle and sends each confirmed command to the explicit
selected UUID. Single-vehicle presentation is unchanged. Cmd-L was exercised
against the fresh built app and opened the real combined app/import/service log
window. App and service log reads are bounded to one MiB, refuse symlinks and
non-regular files, and large app events rotate without whole-file reads. App
logs now record safe account, service, and vehicle-command lifecycle/error
codes without vehicle ids or credentials. The persistent file keeps up to
16 KiB per event, while the duplicate macOS unified-log copy is capped at
512 bytes to avoid excessive system-log and test output.
Cmd-L now redacts credentials and vehicle identifiers before service output is
shown, not only when it is copied or saved. Repeated Cmd-L or Refresh requests
cannot replace an active diagnostic run. Full diagnostics include bounded
doctor, preflight, status, and log-read durations in both the report and safe
app event, making a slow phase visible without extra probes. Focused display
redaction and report-timing AppKit tests pass.

Guided SSH migration now binds a successful tunnel to the exact non-secret
server/authentication settings used to open it, locks those fields while work
is active, and refuses import if they changed. Session close terminates and
then kills an uncooperative SSH tunnel after one second. Tunnel stderr is
drained concurrently into a bounded 64 KiB tail, preventing pipe deadlock or
unbounded diagnostic memory use. Discovery rejects multiple running TeslaMate
or database containers instead of selecting an arbitrary stack. Common SSH
authentication, host-key, DNS, route, refusal, timeout, and reset failures now
produce safe actionable reason codes without exposing the server address.
Tunnel admission now requires both OpenSSH and its local listener to remain
live across a second check, so an unrelated process winning the local-port race
cannot be mistaken for a ready database tunnel. App launch removes only prior
current-user-owned, UUID-named Hub SSH secret directories and refuses symlinked
or unrelated paths. Hub serve and bounded-observation logs now distinguish a
clean stop from an unexpected worker failure.
Debian's Hub unit now matches the proxy/telemetry units' no-capability,
private-device, kernel/control-group protection, address-family, SUID/SGID,
native-syscall, and 0077-umask restrictions. Linux packaging regressions passed.

All macOS config reads used by account setup, rollback snapshots, and status
now open the exact inode with `O_NOFOLLOW`, require a regular file, cap it at
one MiB during both metadata validation and reading, and require UTF-8. The
descriptor is opened nonblocking so a substituted FIFO is rejected instead of
hanging setup. Unsafe FIFO, symlink, and oversized configs fail before any
account command or service mutation.

The per-user lifetime lock and shared publication gate now stop after 32
create/open identity races instead of allowing a same-user path replacement
loop to consume CPU forever during startup. Existing lock identity,
replacement, permission, and publication-gate regressions pass.

Cmd-L app and service log reads, plus app-log writes, now use nonblocking,
no-follow descriptors and validate the opened inode as a regular file. This
closes the remaining path where replacing a log with a FIFO could freeze log
opening or event recording. The shared tail reader remains capped at one MiB;
the FIFO regression completes in milliseconds. Migration handover state uses
the same bounded descriptor admission, so a substituted marker FIFO fails
closed at verification rather than freezing app launch. All 99 AppKit tests
pass.

Data recovery now opens every copied, hashed, metadata, and synced backup member
through a nonblocking no-follow descriptor, validates that opened inode as a
regular file, and aborts immediately if a source grows beyond its signed
manifest size. Terrain tiles are likewise admitted once and retained by their
validated descriptor; elevation reads no longer reopen a mutable path for every
sample. The provenance SHA now hashes that same pinned descriptor, and a
substituted source-marker FIFO is ignored without blocking. FIFO and
path-replacement regressions pass on both paths. Validated tile SHA-256 values
are cached by bounded file identity, eliminating a repeated multi-megabyte hash
on every nearby observation while still invalidating an atomically replaced
tile.

Rust-side bounded readers now also open TLS identity files, update packs,
schema-finalizer files, import ownership markers, and TeslaMate staging files
nonblocking before validating the descriptor as a regular file. A substituted
FIFO is therefore rejected instead of hanging Hub startup, recovery, import, or
update work. The TLS FIFO regression, the affected focused suites, the full
locked Rust suite, doc tests, formatting, and Clippy with warnings denied pass.

Unlocked native UI acceptance passed. Cmd-L opened the bounded redacted app,
SSH/import and service logs; Run Diagnostics completed doctor, preflight,
status and recent-log checks. Its support header now includes only safe app,
expected/observed Hub, service, provider, macOS, and architecture metadata so a
saved report identifies the failing build without vehicle or account identity.
The header also carries its generation timestamp. Fleet and legacy account
setup, service installation/removal, and diagnostic checks now record bounded
start/success/failure timing with typed error codes, never credential values or
vehicle identity. Share redaction additionally covers generic, ingest, pairing,
device, API-key, and private-key fields. App events now retain their correct
info/warning/error severity in macOS unified logging, and Cmd-L records safe
refresh, copy, save, byte-count, duration, and export-failure events without
paths. Its redacted copy path is exercised in the 98-test AppKit suite.
The dashboard now polls Hub status every 15 seconds only while the GUI is active
and visible; account and service actions still refresh immediately. This removes
the previous five-second background process/SQLite wakeup while the app sits
behind other work.
The explicit one-owner handover stopped
TeslaMate, started Hub, and advanced the durable legacy observation id. No
vehicle command ran. Hub was then stopped and TeslaMate plus TeslaMateAPI were
restored healthy. A five-second dashboard refresh now changes stale starting,
running and stopped presentation without reopening the app; the live retest
showed the new observation and enabled controls only while Hub was running.

Debian 13 ARM64 acceptance used one disposable 5.5 GiB native guest. The
current 15,186,340-byte package, SHA-256
`89b4336105f2d5173b639abcba78de2ce4bb57809043e6a91b644004e2321364`,
installed and bootstrapped, passed status, seven doctor checks, loopback health,
service start/restart/stop, and both stopped and running overinstall. That live
test exposed and fixed an upgrade failure for valid unconfigured stores; the
postinst now accepts that state only after a successful doctor catalogue check
and an exact empty-vehicle status. The guest and native build root were deleted.

The guided v4.1.1 migration was exercised live over key-authenticated SSH while
TeslaMate stayed running. It copied 3,303 drives, 11,039,715 positions, 800
charging processes, and 309,147 charge samples through one protected loopback
tunnel, passed compatibility and doctor checks, retained the migrated legacy
token encrypted, and left TeslaMate unchanged. Stale multiple open drive,
charge, and state rows are now retained as history without being guessed as the
single live lifecycle. App import diagnostics are mode-0600, bounded and
redacted; Cmd-L exposes them with service logs and an explicit full-diagnostics
action. SSH discovery failures now have safe phase/reason codes and actionable
messages without logging host, user, key path, database password, encryption
key, or token values. Standard Edit commands and the explicit migration Tab
order have AppKit coverage; the live key-auth import previously proved Cmd+A
replacement and Tab movement through the first fields.

Earlier verification and product history: live EMEA
Fleet authorization, vehicle
discovery, vehicle-data polling, partner registration, and virtual-key pairing
passed on macOS. The root package upgraded the running legacy per-user service,
the installed Hub now supervises the loopback Tesla command proxy as its child,
and the installed Mac app exposes all seven reviewed non-charging controls. One
additional UI climate-start and UI climate-stop were accepted; subsequent Fleet
telemetry reported climate on and then off. No charge command ran. `status`,
immutable preflight, and doctor passed
with a zero-byte WAL. The local TeslaMate PostgreSQL write-back dry-run reported
zero affected rows and left charge cost `0.10` unchanged.

Live legacy handovers on 2026-08-26: the first test exposed 355 reconnects
because Hub rejected Tesla's normal `control:hello` with
`connection_timeout: 0` and falsely completed subscribe receipts before any
telemetry. The repaired Mac build accepted the real hello, required matching-tag
telemetry as authentication proof, and completed one physical drive through one
WebSocket connection: 1,413 positions, three subscribe receipts including two
same-socket `vehicle_disconnected` resubscriptions, one advancing stream
watermark, and one orderly unsubscribe. Repeated telemetry was also found to
cancel Owner API retry deadlines; stream-health scheduling is now transition-
only and its regression passes. A completed WebSocket task was then found to be
awaited twice during cleanup, panicking Tokio and leaving the collector lease
behind. Stream tasks are now single-consumer `Option<JoinHandle>` values. The
installed fixed process remained at one PID while its stream observation ID
advanced from 661 through 901 and durable CLI verification advanced from 775
to 838. No new panic, worker-failure, or lease-conflict log appeared.

The next live drive exposed one further backpressure defect: after 707
positions the bounded stream channel filled while the collector was doing other
work, and a 250 ms send timeout incorrectly killed the stream supervisor. The
LaunchAgent restarted Hub once; the persisted open lifecycle recovered and the
same drive closed durably with 1,255 positions. The final source removes that
fatal timeout: the still-bounded 256-event channel now waits for capacity or
shutdown, and collector backlog drains before Owner API, projection, or
enrichment work. A production-shaped 257-event regression persists every event
without reconnect or loss. The final installed build is ready on one new PID,
its stream advanced immediately after upgrade, SQLite integrity is `ok`, its
WAL settled to zero, and no post-upgrade panic, lease, worker, or queue error
appeared.

The fresh v4.1.1 migration projected 11,361,997 rows into 457 immutable packs
using 1.9 GB, without a second raw PostgreSQL copy. SQLite integrity is `ok` and
the WAL settles to zero bytes. The previous 26 MB Fleet store is preserved as
`data-fleet-preserved-20260826`; the active legacy store is `data`. The migration
also exposed that durable verification queried only prunable raw observations;
it now reads the union of raw and current observation metadata. No vehicle
command ran.

Retained 2026-08-25 artifacts (pre-security-remediation; not current release
candidates):

- macOS ARM64 app, embedded Hub SHA-256
  `d67fe6265c0647b0f723c0bff899ccd4ac94d3c8d83afd7c373eafde2db72736`;
  installed root Hub SHA-256
  `1e4e78fdfa92686555cf0352c9d9b9023935f58ab3605110d1844f7ae60ed663`;
  embedded Tesla command proxy SHA-256
  `ee61a89137c8eb73db4db1d57f2e393a084221e51421b06087e1677a2c631cc2`;
  embedded service package SHA-256
  `2da3f22d50421ffce32f6cc994b723e9ed315b35eca3f91b16bb4bca095f302d`;
  deep strict ad-hoc signature verification passed.
- Debian 13 amd64 package SHA-256
  `b4ba561c173ac6b4df759247a26d4d3ca28406c45b4eacb8709e7ef00e089ebc`.
- Debian 13 ARM64 package SHA-256
  `8f7d111c12f4f6a9b4c7a57f26f3ca476fd56a6b74966a52d2db1df53834181d`.

Both Linux packages were built natively from commit `e19ff55` on Debian 13,
installed, bootstrapped, checked, started on loopback, and stopped. Their build
roots, test installs, and ARM64 VM were deleted. TeslaMate stayed healthy.

The final stopped TeslaMate v4.1.1 migration read 105 migrations, 10,782,436
positions, 3,267 drives, and 1 car into about 1.86 GiB / 446 packs. Source
counts stayed unchanged. Hub refreshed the real legacy token, its encrypted
successor was handed back in one PostgreSQL transaction, and TeslaMate then
refreshed successfully itself. Hub, its data, build inputs, emulation packages,
and caches were removed from the VPS. TeslaMate and TeslaMateAPI are healthy;
VPS root has 28 GiB free.

Current work plan:

- Completed 2026-08-25: bundle and manage Tesla's official command proxy on
  macOS, expose confirmed non-charging controls in the Hub app, and test the
  installed app climate start/stop against the paired car.
- Completed 2026-08-26: repaired and live-proved legacy WebSocket streaming on
  a closed 1,413-position physical drive, then atomically handed the rotated
  token back and restored TeslaMate plus Fleet Hub.
- Completed 2026-08-25: rebuilt and installed source-identical Debian amd64 and
  ARM64 packages, ran package/service smoke checks, and deleted disposable data.
- 2026-08-25 through 2026-08-31: the active `Teslatlas Fleet endurance` daily
  check keeps Fleet Hub collection under read-only observation without vehicle
  commands or a second refresh owner.
- Completed 2026-08-25: repaired Fleet REST endpoint selection, scope admission,
  polling cadence, and incomplete-drive cleanup; production-path restart and
  idempotence regressions pass. One physical drive is still required for live
  moving-coordinate and closed-drive proof.
- Produce a Developer ID-signed, notarized, provenance-bound macOS release using
  matching Apple credentials. The available 4AA Developer ID identities sign
  the exact app and package successfully, but the only usable local notary key
  belongs to NPS. Physical iOS sync remains outside Hub scope.

Goal audit at 2026-08-25 09:41 UTC: managed proxy, Mac controls/live climate,
both native Linux packages, build cleanup, driving-stream plan, and VPS
TeslaMate continuity have direct current evidence. Fleet endurance remains open
until its scheduled checks run. Notarization remains conditional on a matching
Apple credential, which is not present locally.

Driving-stream handover plan for 2026-08-25: keep VPS TeslaMate running until a
new short test window is explicitly approved; record TeslaMate and Hub database
watermarks; stop TeslaMate once; transfer the current legacy token pair to Hub;
observe one physical drive; verify stream samples and the closed drive; hand the
latest rotated pair back in one PostgreSQL transaction; stop legacy Hub; restart
and health-check TeslaMate. Any failed step aborts to TeslaMate restart. Fleet
Hub may continue polling separately because it owns a different Fleet token.

Fleet endurance baseline on 2026-08-25: local Fleet Hub ready with one vehicle,
latest observation ID 910, database 3.6 MB, next token refresh scheduled for
2026-08-25 13:51 UTC, and token expiry at 14:21 UTC. The VPS TeslaMate and
TeslaMateAPI containers were both healthy. Recheck through 2026-08-31; do not
issue endurance vehicle commands. At 09:36 UTC a read-only check observed the
current record advance from 1013 to 1014 within five seconds, SQLite integrity
was `ok`, and the WAL was zero bytes. No refresh attempt exists yet because the
first scheduled refresh is still in the future.

Physical Fleet drive observation on 2026-08-25: the installed Fleet Hub had
remained ready and continued advancing current observations while the operator
drove. It opened a driving lifecycle with a nonzero maximum speed, but the
provider observations contained no coordinates, so it retained zero positions
and could not materialise a drive. The repaired collector now requests the
location endpoint, verifies the location scope, closes zero-position sessions,
and removes rejected provisional rows. That old drive is not recoverable; a new
physical drive is still required for parity proof. Legacy streaming was not
exercised by this drive.

Deliberate exclusions: TeslaFi import, `addresses.raw`, Grafana/MQTT/dashboard,
and Fleet Telemetry fields Tesla does not publish through the configured native
push contract. Fleet supports official native push with bounded REST setup and
initial snapshots; legacy Owner API keeps TeslaMate-compatible streaming.

Blocked: Fleet REST drive capture needs one new physical-drive confirmation.
Real Developer ID notarization is separately blocked by a local
credential team mismatch: the application/installer identities and App Store
Connect notary key belong to different Apple teams. A disposable signing proof
passed for both exact artifacts with timestamps and hardened runtime, then was
deleted. No matching notary profile, API key, or app-specific password was found
locally. The local old-schema TeslaMate PostgreSQL copy remains an intentional
read-only negative fixture.

## Grok branch consolidation, 2026-08-26

- The native dashboard now uses flat borderless actions and square groups,
  reports an intentionally stopped Hub as stopped, hides Connect Tesla when an
  account is present, and labels that account as Fleet API or Legacy token.
  Manage Tesla exposes Fleet setup, legacy setup, exact-v4.1.1 migration and a
  cancel-first disconnect. Service Details, Logs and Diagnostics reuse one
  window each; diagnostics run bounded doctor, preflight and status checks and
  redact copied or saved output.
- The five-step onboarding handles new Fleet/legacy installations and
  TeslaMate migration through verification and explicit handover. It never
  stops or removes TeslaMate, blocks window closure during authentication or a
  mutating operation, and leaves failed provider or migration setup stopped.
- Rust parity repairs preserve TeslaMate current-state fields, resumed driving
  after gained-range handling, imported open-session aggregates, complete
  cutover watermarks and source content, and atomic settled publication. Doctor
  validates the selected encrypted provider without mutating credentials.
- Final local gates passed: 860 Rust checks, strict all-target/all-feature
  Clippy, 75 AppKit tests, optimized macOS release build, macOS packaging and
  Linux packaging. Independent Rust and macOS reviews found no remaining P0-P2
  issue.
  Final screenshot comparison passed at 1800x1324. No service, VPS, TeslaMate or
  vehicle state changed.
- Cleanup removed 24,285 disposable debug/test files reported as 14.9 GiB by
  Cargo. The retained project target is 607 MB and the distribution app is
  70 MB.

## Security remediation, 2026-08-26

- Closed the direct security findings: privileged macOS install now binds the
  signed app, Team ID, signed package and packaged SHA-256 before root execution;
  uninstall runs only the installed root-owned helper. Remote TLS serving has
  bounded concurrency, handler time and pack streams; readiness is cached and
  bogus pairing/rotation proofs are rejected before SQLite writer admission.
- Data restore now removes invitations, device bearers and collector leases,
  including accepted legacy-v3 backups. Fleet proxy CA reads are no-follow,
  private, bounded and descriptor-pinned. Credential recovery proves the cursor
  key against the retained catalogue before publishing secrets. Provider JSON
  persistence is a typed allowlist shared by Owner and Fleet responses.
- Fixed the two remaining non-security defects from the audit: a wrong TeslaMate
  key cannot pass preflight without a previous generation, and the first selected
  import atomically publishes schema 2.1 and 2.2 catalogue heads. A pre-commit
  fault exposes neither head; successor retry behavior remains unchanged.
- Added a local dependency gate. It scans 355 locked crates and ignores
  `RUSTSEC-2026-0235` only after `cargo tree --target all` proves `rkyv 0.7.46`
  unreachable from normal/build edges. Added a fail-closed, clean signed-tag
  release-evidence generator for Linux artifacts: deterministic exact source,
  Cargo metadata, SPDX 2.3 SBOM, dependency inventory/license texts, checksums,
  and provenance bound to an exact maintainer tag fingerprint and signed by an
  independently pinned public-key digest.
- Release evidence now additionally requires `HEAD` to equal the signed tag
  commit at both ends, reuses one stable artifact witness for the manifest and
  checksums, revalidates artifacts before publication, and publishes the whole
  evidence directory with one atomic sibling rename. Mutation, wrong-HEAD,
  partial-output, retry, and destination-collision regressions pass.
- Immutable Doctor/Preflight now wait briefly for a transient WAL, rerun the
  complete check once if the catalogue changes, and verify the snapshot only
  after every command read. A continuously active collector still returns an
  explicit bounded "retry when idle or stop Hub" result instead of mutating or
  copying the live 1.9 GB catalogue.
- Final local gates: 813 Rust tests passed, 2 fixture tests ignored; format,
  all-target Clippy `-D warnings`, optimized release build, 48 AppKit tests,
  macOS package/release checks, Linux package checks, dependency audit and the
  signed-evidence fixture passed. A final independent security diff review was
  clean after its tag-signer trust finding was fixed. No live service,
  TeslaMate, VPS or vehicle state changed during this remediation.
- Honest remaining release/architecture gates: the current per-user LaunchAgent
  is private and non-root but not a dedicated service account. Moving to a fixed
  LaunchDaemon requires authenticated app-to-service IPC and an ownership/data
  migration, so it is deliberately deferred rather than represented by cosmetic
  launchd settings. Real notarization still needs a matching 4AA notary
  credential. macOS evidence remains blocked until the pinned Tesla command
  proxy's complete Go source/notices are captured; the evidence tool refuses
  `.pkg`/`.zip` candidates until then.

## Three-platform review, 2026-08-22

- macOS: migration stop/install/schema/restart rollback, bounded child output,
  service update, loaded-state upgrade rollback, and data-preserving privileged
  uninstall are implemented. Current ARM64 app assembly and validation passed.
- Debian amd64 and ARM64: upgrade rollback preserves active/enabled state;
  migration ownership and configured data paths are admitted; maintainer-script
  removal/reload behavior and generated shared-library dependencies are tested;
  package construction checks complete ELF class, data, ABI, machine, and
  interpreter identity.
- Shared Rust: paired-device expiry/revoke/rotate, deterministic refresh
  recovery, bounded TLS identity admission, direct-migration capacity admission,
  crash-safe reset, and current schema migration are implemented and tested.
- Artifact state: all three retained artifacts come from the repaired source.
  Independent macOS, Debian, and Rust reviews found no concrete blocker.

## Completed

- Fleet REST drive repair — the Fleet vehicle-data request now explicitly
  includes location data; setup/preflight verify device-data and location JWT
  scopes while status stays recovery-safe. Parked samples clear incomplete
  drives, and rejected zero/one-position drives delete provisional rows across
  atomic, non-atomic, and restart paths. Production-shaped Fleet tests cover
  driving, restart, parked close, duplicate retry, and incomplete-drive cleanup.
  The final root package is installed and running: provider Fleet, one vehicle,
  observation advanced, token refresh current/scheduled, all required scopes
  present, proxy child bound only to `127.0.0.1:4443`, SQLite integrity `ok`, no
  quarantine/open drive, and zero-byte WAL. No vehicle command ran; VPS
  TeslaMate was not touched. Final cleanup removed 30,047 generated target
  files (16.3 GiB), the accidental duplicate Rust 1.98 toolchain, the bounded
  preinstall backup, and the leftover Hub Xcode temporary directory; the 57 MB
  final app/package remains in `dist/`.

- Final native Linux packages — final packaging rejects symlink binary inputs
  and includes `PROVENANCE.md`. Native Debian 13 amd64 and ARM64 builds used
  Rust 1.98; package regression, ELF/loader identity, fresh install, bootstrap,
  doctor, status, loopback systemd lifecycle, and package content checks passed.
  No Tesla credential, API, or command was used. VPS TeslaMate/TeslaMateAPI
  remained healthy; both build roots, test installs, and the ARM64 VM were
  removed.

- Build cleanup — removed the 4.0 GiB project target, 4,415 stale Hub test files
  totalling 1.68 GB from the macOS temp directory, and 31 MB of superseded
  per-user Hub binaries. Retained artifacts total 56 MB; the installed app and
  root service total 76 MB.

- Managed macOS command service and complete controls — pinned Tesla's official
  `vehicle-command` v0.4.1 source, packaged its loopback proxy beside Hub, and
  made Hub own proxy startup, readiness, exit, and shutdown. Fixed a real
  legacy-to-root package upgrade failure, installed the package, and verified
  the proxy was the Hub child on `127.0.0.1:4443`. Installed the Mac app in
  `/Applications`; Wake, Start/Stop Climate, Lock/Unlock, Flash, and Honk
  deliberately exclude charging. One UI start and stop each produced a command
  confirmation and Fleet telemetry changed true then false. A live proxy
  termination also made Hub fail closed; launchd restarted Hub and a new proxy,
  restored the loopback listener, and returned to ready Fleet collection.

- Fleet signed-command onboarding — published the EC public key at the Tesla
  well-known path without changing the root site, registered `magrathean.uk` as
  an EMEA Fleet partner, and opened Tesla's virtual-key pairing flow. The macOS
  Hub app now offers confirmed Start Climate and Stop Climate actions for one
  ready vehicle through the resident service; charging controls and iOS changes
  are deliberately absent. The release app built and signed, all 27 AppKit tests
  passed, and macOS package checks passed. After Tesla-app pairing approval,
  exactly one live climate-start and one climate-stop command succeeded, each
  wrote an audit receipt, and subsequent Fleet telemetry reported climate on
  then off. TeslaMate stayed healthy; no charge command ran.

- Official Fleet login — completed the EMEA authorization-code flow with the
  exact selected scopes, configured encrypted Fleet credentials through stdin,
  discovered the account vehicle, and stored live discovery plus vehicle-data
  observations. Added the self-hosted setup guide with exact regional endpoints
  and a no-temporary-file token exchange. Live testing found and fixed
  short-lived WAL residue, Fleet status reading the pruned raw table, writable
  status getters, and the Tesla.cn refresh endpoint. No wake or command ran.

- macOS controller/package repair — fixed migration stop/confirm/restart,
  bounded concurrent child output, URL credential parsing, TOML path escaping,
  loaded-state upgrade rollback, data-preserving privileged uninstall, and a
  private process umask. Eleven focused AppKit tests and lightweight package
  checks passed without rebuilding the Rust binary or release artifact.

- Tesla stream wire compatibility — Hub decodes UTF-8 binary JSON and requires
  the exact active subscription tag for telemetry, matching TeslaMate v4.1.1.
  Follow-up live proof established that Tesla's normal hello uses
  `connection_timeout: 0`; the hello proves socket liveness while the first
  matching telemetry frame proves authentication. An explicit foreign or
  missing telemetry tag remains rejected. The amd64 package was rebuilt,
  ELF/package-verified,
  installed, and Hub restarted cleanly. It established a Tesla TLS stream
  connection. No raw or current stream row arrived while the parked vehicle
  was quiet. The one 5.6 GiB remote source/build directory was deleted
  immediately after copying the 6.5 MiB package back. Hub was later purged in
  full and TeslaMate restored healthy; VPS free space is 28 GiB.

- Native Debian amd64 package and direct cutover — built the 5.9 MiB amd64
  package natively on Debian 13, verified its ELF/package architecture, and
  installed it at `/usr/bin/teslatlas-hub`. During the test, the loopback-only
  systemd service was active while TeslaMate stayed stopped, preserving one
  legacy-token refresher. Its final repeatable-read, read-only TeslaMate v4.1.1 migration
  had 105 source migrations, 10,782,432 positions, and unchanged source
  counts; Hub finished at about 1.86 GiB with 447 packs. The single named
  `/var/tmp` build directory, private build inputs, and its stray `.zshenv`
  source line were removed. The package, unit, data, config, and service account
  were removed after testing; TeslaMate is healthy and VPS root has 28 GiB free.

- Native onboarding and recovery security — adapted Tesla Auth v0.15.0 PKCE,
  callback/state/issuer routing, private WebKit login, bounded no-redirect token
  exchange, and stdin-only Rust setup into the AppKit installer flow. Removed
  wake/climate CLI transport and its second credential-manager path. Plaintext
  sync is documented as a loopback trust boundary, not authentication. Backup
  v3 excludes both local keys; explicit AES-256-GCM credential export binds the
  installation ID and rejects wrong keys, tampering, or overwrite. Focused and
  full validation is recorded above.

- Rust 1.98 dependency and packaging refresh — updated the minimum toolchain to
  Rust 1.98 and all resolvable lockfile packages; `matchit` 0.8.4 remains Axum
  0.8.9's exact requirement. Mac format, all-target check, 702 tests with 2
  intentional ignores, Clippy `-D warnings`, release build, 4 macOS app tests,
  app assembly, and strict ad-hoc signature verification passed. A native
  Debian 13 ARM64 guest built and installed the 5.3 MiB package, then passed
  version, bootstrap, doctor, and systemd status smoke checks. Package SHA-256 is
  `1fee14f4d77839265df64869d29aeb5d4c3944baaf562289cc3c5a61dcacbda7`;
  the bounded QEMU guest was deleted.

- Compact direct catalogue — current direct imports retain one digest/state
  catalogue rather than a second `teslamate_import_projection_rows` copy.
  Legacy tombstone and collector-ID paths derive their compatible view from
  that state, while old stores keep their existing inventory. Focused direct
  base/state, direct successor, and staged-compatibility tests plus release
  `cargo check` passed.

- Compact direct v4.1.1 Mac measurement — the stopped VPS TeslaMate v4.1.1
  source (105 migrations; 10,782,430 positions) imported through one local-only
  SSH tunnel without a raw stage. It projected 11,100,209 rows, created one base
  and no deltas, used 446 packs, passed SQLite `integrity_check` and `doctor`,
  and left the source counts unchanged. Final disk was 1.88 GiB; sampled peak
  was 2.94 GiB. The legacy `teslamate_import_projection_*` inventory tables had
  zero rows; the compact state tables had 11,096,061 rows. TeslaMate was then
  restarted healthy. The exact test store and its tunnel were removed.

- Compact direct v4.1.1 Linux measurement — a disposable Debian 13 ARM64 QEMU
  guest built the current Hub source natively and read the same stopped source
  through its own loopback-only tunnel. Migration exited 0; `integrity_check`
  and `doctor` passed. Final data was 1,966,668 KiB with one base, no deltas,
  446 packs, 11,096,061 state rows, and zero legacy inventory rows. The source
  fingerprint remained 105 migrations, 10,782,430 positions, and 2,123,577,023
  bytes. TeslaMate restarted healthy. Guest, disks, private inputs, logs, and
  tunnel were deleted.

- Direct v4.1.1 Mac measurement before catalogue compaction — with the source
  stopped, one read-only direct import projected 11,085,583 rows. It used no
  raw-history stage, finished at 3.17 GiB, and had a sampled 4.17 GiB peak
  (including its temporary comparison spool). The disposable store and tunnel
  were removed. The current compact-catalogue change was measured separately.

- Direct bounded migration — the CLI now uses the existing repeatable-read
  PostgreSQL-to-pack importer rather than `imports/.staging`. The initial pass
  builds packs inline; the final pass captures comparison state only and emits
  sparse deltas. Legacy ciphertext is read from that exported snapshot. Capacity
  admission reserves one active fragment plus the free-space floor instead of
  a 16 GiB cap. 667 library tests passed (2 ignored); Mac and Linux live space
  receipts passed.

- Historical staged-path measurement — with TeslaMate stopped, Mac migrated car
  1 from exact TeslaMate v4.1.1 d6c43bc8c48784da8f0b701945b80b20911b3d1a through
  two read-only snapshots. Each snapshot staged 11,082,539 rows / 7,408,814,477
  bytes; final store was 3.2 GiB; peak staging/publication use was 12 GiB and
  took 853 seconds. The test store was deleted. This measures the retired raw
  stage path, not the direct migration now in the CLI.

- Debian ARM64 v4.1.1 migration boundary — one 5.5 GiB QEMU Debian 13 guest installed the Hub package, reached the real stopped v4.1.1 source through a loopback-only SSH tunnel, and copied until its deliberate 64 MiB stage cap returned stage database byte limit exceeded. The service remained inactive and the stage was empty afterward (660 KiB local state). The guest, package test data, private password file, tunnel, and 2.6 GiB host debug cache were deleted. No TeslaMate PostgreSQL write or vehicle command ran.

- Earlier Debian ARM64 package and live collection — one 5.5 GiB QEMU Debian 13 guest compiled `447c904` natively from the mounted Hub source and existing offline Cargo registry. The 5.3 MiB package (`523beee58b1e83790c82c0a4601a38741ce4321eb6695b8b3fcca57661652b18`) installed, bootstrapped, reported status, completed systemd start/restart/stop, and made a bounded default-streaming live observation with `is_climate_on=true`. Host format, 664 library + 34 CLI + 1 TLS tests (2 intentional fixtures ignored), and Clippy `-D warnings` passed. No vehicle command ran.

- Debian ARM64 acceptance — an earlier Debian 13 ARM64 QEMU guest built and package-tested the broader bootstrap, status, systemd lifecycle, backup, verification, restore, repair, and read-only migration-rejection surface. The dummy migration request safely refused the 17 GiB stage requirement before any PostgreSQL connection; no real Tesla or vehicle command ran.

- Linux portability — fixed Unix-sized Rustix mode/device handling, Linux service error formatting, root-owned packaged config admission, normal non-022 test umasks, Linux service CLI wording, and host-only byte-fixture execution. The package builder now removes only its exact temporary staging directory.

- Linux CLI delivery (historical) — added bootstrap, systemd
  `status|start|stop|restart`, Debian package files, and Linux documentation.
  The earlier wake/climate commands and command-only credential path are now
  intentionally removed for TeslaMate parity and single refresh ownership.

- Debian native portability — QEMU ARM64 compilation exposed platform-sized
  Rustix mode types and systemd test lifetimes; both are portable. Later native
  package acceptance passed; its historical fake wake/climate surface has since
  been removed.

- Linux runtime gates — lifted the existing admitted Unix runtime for setup, read-only migration, serving, and bounded observation; added the small systemd `status/start/stop/restart` adapter. Host format and all-target Rust check passed; Debian ARM64 compilation and package smoke were later completed in one bounded native QEMU guest.

- Final integration — fixed monotonic observation IDs after bounded raw-row pruning, preventing later Owner/stream telemetry from being skipped after SQLite row reuse; corrected historical migration fixtures and current-snapshot assertions — collector 60/60 and all 8 affected upgrade tests passed.

- v4.1.1 surface closeout — matched Nominatim address aliases including Australian territories and corrected the stale 17-item parity ledger: schema 2.2 preserves 16 reviewed domains, excludes only provider raw JSON, and loses none — 3 focused tests passed.

- Charge and geofence parity — charge cost edits now accept total, per-kWh, or per-minute input; geofence changes relabel bounded historical drive/charge pages, optionally calculate missing matching charge costs, and enforce TeslaMate's sub-5-km radius — 2 focused tests passed.

- Credential continuity (historical, superseded in part) — `control sign-out`
  stops the LaunchAgent, refuses a concurrently running direct Hub, and deletes
  the token row and both key generations. The former backup-v2 key restoration
  was replaced by current data-only backup v3 plus separate encrypted recovery.

- Native TeslaMate controls — added live pause/resume and settings pickup within 30 seconds, geofence create/list/delete, completed-charge cost replacement, bounded TeslaMate-compatible GPX export, and rated-range charge-consensus efficiency recalculation — 5 focused tests passed.

- Bounded terrain storage — added a 512 MiB default cache quota, serialized cache admission, oldest-tile eviction, corrupt-tile removal, bounded provenance reads, and release of unused per-tile locks — 3 focused tests passed.

- Current vehicle and raw-storage parity — added authenticated `GET /v1/vehicles/{vehicle_id}/current` with the TeslaMate v4.1.1 live-summary surface, durable Owner/stream overlay, current geofence and restart state; processed raw observations are transactionally pruned while three bounded current snapshots preserve display and deduplication — 3 focused tests passed.

- TeslaMate v4.1.1 derived data — charge integration now uses the current row, falls back to charger power when phase inference is unavailable, normalizes nonpositive phases, and recomputes from all durable samples before publication; Model 3 MY2022+ trim `50` is now `RWD` — 5 focused tests passed. TeslaMate v4.1.1 itself no longer writes the legacy per-drive efficiency column, so live `NULL` remains compatible.

- TeslaMate v4.1.1 import — pinned the exact 105-migration tag at `d6c43bc8c48784da8f0b701945b80b20911b3d1a`, updated VIN/cost schema admission and pack contracts, and kept the local 99-migration database as a read-only negative fixture — 16 schema tests, 20 pack-contract tests, and the bounded preflight test passed.

- Final validation and smoke — format, all-target check, 697 tests passed with 2 ignored, Clippy `-D warnings`, and release build passed; the release binary initialized, reported status, passed doctor/repair, backed up, verified, restored, and re-checked the restored store — the 1.8 MiB fixture was removed, no Hub `/tmp` artifacts remain, and the retained release-only `hub/target` is 611 MiB after deleting 20.1 GiB of debug/test artifacts.

- Data recovery — verified immutable backup, backup verification, restore without source mutation, unsafe-destination refusal, and repair that preserves quarantine while deleting proven orphan packs — 2 focused tests passed.

- Pairing and local sync — verified failure-safe invitation persistence, single-use TLS claim, authenticated vehicle/manifest/pack access, Range resume, restart persistence, and wrong-key rejection — 3 focused tests passed.

- Optional TeslaMate import — verified exact migration-set admission and final-snapshot publication; the local PostgreSQL copy stayed read-only at 99 migrations/2,644 drives and was correctly rejected as incompatible — 2 focused tests and one CLI negative smoke passed; the 632 KiB fixture was removed.

- Fake collection and restart durability — wired the observer acceptance path through native setup, then verified products, vehicle-data, streaming reconnect, encrypted credentials, and durable car/drive/charge/position/state/settings/update behavior across restart — 5 focused tests passed.

- macOS service control — added `service status`, `start`, `stop`, and `restart` with preflight and bounded launchctl state checks; focused launchctl/CLI tests passed and live read-only status reported stopped without changing the existing plist.

- Native clean setup — added `setup` with bounded private token files, products-only vehicle discovery, explicit multi-car selection, durable V2 publication, encrypted credential persistence, and general service preflight without TeslaMate — `cargo check --locked` and 5 focused tests passed.

- Product-fix merge — transport, migration, snapshot, pairing, credential handling, and durability changes merged to `main` — format, check, Clippy, release build, and 674 tests passed with 2 ignored.
- Workspace cleanup — 69 redundant Hub-only temporary clones, targets, and logs removed; sibling App artifacts untouched — zero matching Hub items remained in `/tmp`.
- Goal rewrite — direct sequential development, one checkout, one Cargo target, focused tests, one progress file, and cleanup before handoff.

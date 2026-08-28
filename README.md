# Teslatlas Hub

Teslatlas Hub is an independent, self-hosted vehicle telemetry collector and local data hub written in Rust. It is being developed for macOS and Linux and is intended to support a native controller application, full command-line operation and an automated bootstrap path.

The Hub is aimed primarily at new self-hosted installations. It also includes an optional, one-way migration path for data and legacy Owner API credentials held in a user-controlled TeslaMate PostgreSQL database. Migration support is secondary and does not make TeslaMate a runtime dependency.

> **Alpha software:** interfaces, storage formats, authentication routes and operational procedures may change. Keep an independent backup and test recovery before using real data.
>
> **Release status:** `main` is the unreleased `1.0.0-alpha.2` development line. The published `v1.0.0-alpha.1` release is the older macOS Apple-silicon, one-vehicle legacy-token build; it does not contain the multi-vehicle, Fleet API, Fleet Telemetry, signed-command, Debian packaging, or system-service work documented below. Use the release page for the immutable alpha.1 scope and build `main` only for development or testing.

## Product direction

Teslatlas Hub is intended to provide:

- a native Rust collector and local data store;
- loopback-only local synchronisation for the separately distributed Teslatlas client;
- service and CLI operation on macOS and Linux;
- an automated bootstrap/install path;
- new installations that do not require TeslaMate or Grafana;
- optional, read-only migration from an operator-controlled TeslaMate database.

The paid Teslatlas application is a separate product and is not part of this repository.

## Current unreleased scope

The current `main` branch (`1.0.0-alpha.2`, not yet published) includes:

- macOS 13 or later on Apple silicon, and Debian 13 amd64 or ARM64;
- independent collection for every configured vehicle on an account;
- legacy Owner API authentication with native Tesla OAuth onboarding, polling,
  and Tesla streaming;
- official Fleet API authentication, discovery, low-cost native Fleet
  Telemetry push, token rotation, direct wake, and signed commands through a
  local Tesla vehicle-command proxy;
- PostgreSQL history import, encrypted credential transfer, lifecycle
  persistence, backup, repair, bounded provider-response retention, and an
  explicit charge-cost write-back command;
- a native AppKit control app for legacy onboarding plus full CLI operation;
- a per-user LaunchAgent on macOS or a systemd service on Debian;
- no Grafana, MQTT, TeslaFi import, bundled dashboard, or App Store build.

Legacy mode uses Tesla's streaming endpoint while driving. Fleet mode can use
Tesla's native Fleet Telemetry protocol: the vehicle sends changed fields to a
self-hosted public mTLS receiver, which forwards authenticated records over
loopback to Hub. With that mode configured, the resident collector makes no
periodic Fleet `vehicle_data` calls and has no paid polling fallback. Fleet
Telemetry is not fully equivalent to `vehicle_data`; see the setup guide for
the selected fields and remaining caveats.

## Project boundaries

Teslatlas Hub:

- collects and stores vehicle telemetry under the operator's control;
- exposes a loopback-only local sync interface;
- does not require TeslaMate for a new installation;
- does not modify or write to a TeslaMate database during migration;
- does not grant any right to use a vehicle manufacturer's API, account, service or trade marks;
- is not designed for safety-critical, emergency, autonomous-driving or vehicle-control decisions.

The separate `write-back` command is never called by migration or collection.
It can update only one selected TeslaMate charging-process cost, defaults to a
locked-row dry run, and requires `--apply` to commit.

A separate client does not become covered by this repository's licence merely because it communicates with the Hub through a documented protocol. That conclusion can change where code is copied, linked or combined, or where the programs form one derivative work.

## Independence and compatibility

Teslatlas Hub is independently maintained by Magrathean UK Ltd. TeslaMate references are limited to truthful migration, provenance and compatibility statements.

**This project is an unofficial community tool and is not affiliated with, endorsed by, or supported by the official TeslaMate project.**

TeslaMate is not bundled, modified or started by the Hub. The migration adapter reads an operator-supplied PostgreSQL source in a read-only transaction and converts selected records into Teslatlas-owned storage. See [MIGRATION.md](MIGRATION.md), [PROVENANCE.md](PROVENANCE.md) and [Independence and interoperability](docs/INDEPENDENCE_AND_INTEROPERABILITY.md).

Tesla, vehicle model names, TeslaMate and all third-party marks belong to their respective owners. Compatibility references do not imply sponsorship.

## Build current `main` on macOS

Requirements for current `main` are Rust 1.98, Go 1.27.0 exactly,
Xcode 27 and XcodeGen.

```sh
cargo build --locked --release
scripts/build-macos-app.sh
```

The build writes one all-in-one `dist/TeslatlasHub.pkg`. It installs the app in
`/Applications`, installs the root-owned Hub service payload, and opens the app.
The temporary standalone app used to assemble the package is removed. This
local package is ad-hoc signed and not notarised, so it is only for direct local
testing through macOS Installer. The app's own privileged update action remains
disabled because it will not elevate an unsigned embedded package. Public and
app-driven updates require the package digest, Team ID, Developer ID signatures,
and notarisation metadata injected by `scripts/release-macos.sh`.

A signed release can use Fleet API (recommended; see
[Fleet API setup](docs/FLEET_SETUP.md)) or the legacy TeslaMate-style login.
Fleet credentials and legacy tokens enter the Hub over stdin, never process
arguments or temporary files. Once connected, the dashboard hides **Connect
Tesla** and exposes climate, wake, lock, unlock, flash, and honk controls;
charging controls are deliberately absent.

The signed macOS service also bundles the pinned Fleet Telemetry receiver. It
runs only when the user supplies a private receiver configuration and bearer;
otherwise the LaunchAgent runs the Hub alone. In Fleet receiver mode, Hub owns
the bundled loopback command proxy as its child while the LaunchAgent
supervisor owns Hub and the receiver together. See the macOS receiver steps in
[Fleet API setup](docs/FLEET_SETUP.md).

The migration route accepts exact TeslaMate 4.1.1 only. It checks compatibility
before copying one read-only PostgreSQL snapshot, installs Hub stopped, runs
diagnostics, and waits for explicit handover before collection starts. The app
never stops, removes, or changes TeslaMate. After successful handover, disable
Tesla access in TeslaMate so both services do not refresh the same account.

macOS service upgrades keep the old payload only until the new binary begins a
bounded `bootstrap`. That is the same forward-only boundary: after migration
starts, failure retains the new service stopped for repair or package retry
instead of restoring an old binary onto a potentially newer schema.

Use **Service Details → Uninstall Hub…** to remove the current user's
LaunchAgent, service payload, and logs. Uninstall preserves the Hub database and
configuration by default. Permanent data deletion is a separate choice with a
second confirmation. The uninstaller refuses to remove the shared service
payload while another local user still has a Hub LaunchAgent.

### macOS logs and diagnostics

Press **Command-L** from onboarding or the dashboard to open the combined app,
SSH-import, and Hub service logs. **Run Diagnostics** adds bounded `doctor`,
`preflight`, `status`, database, credential, connection, and recent-log checks.
SSH failures are recorded as safe reason codes such as authentication, host-key,
DNS, routing, forwarding, timeout, local-port, Docker, or sudo-access failure;
credentials, server addresses, usernames, and key paths are not recorded.
Dashboard status failures are logged once per failure type and a recovery event
is recorded when status becomes readable again, avoiding polling log spam.

App and service log reads are bounded and reject symlinks and non-regular files.
App logs rotate at 1 MiB; service logs are compacted before launch and every 30
seconds while Hub runs. Display, Copy, and Save use the same redaction pass for
credentials, VINs, opaque vehicle/install IDs, vehicle names, email addresses,
public/private network addresses, precise coordinates, terminal controls, and
the current user's home path. Saved reports are created owner-readable only and
refuse symlinks or non-regular destinations. Review a report before sharing it.

## Debian package (amd64 and ARM64)

Build on the target Debian host. The package script defaults to the host Debian
architecture and accepts only `amd64` or `arm64`; it verifies that the binary
matches before creating the package.

Building the core package requires Rust 1.98 plus Debian's `build-essential`,
`pkg-config`, `libssl-dev`, `dpkg-dev`, and `binutils` packages (`readelf` is
the ELF architecture check). Building the Fleet sidecars additionally requires
Go 1.27.0 exactly. Installing a finished package needs only its declared
runtime dependencies.

Linux service logs stay in journald instead of a second unbounded file. For a
support check, use `sudo journalctl -u teslatlas-hub -n 200 --no-pager` and
`sudo systemctl status teslatlas-hub --no-pager`. Run the same read-only Hub
checks as the Mac app with
`sudo -u teslatlas /usr/bin/teslatlas-hub --config /etc/teslatlas-hub/config.toml doctor`.

```sh
cargo build --locked --release
scripts/build-tesla-command-proxy.sh \
  --target "linux-$(dpkg --print-architecture)" \
  --output dist/tesla-http-proxy
scripts/build-fleet-telemetry-bridge.sh \
  --target "linux-$(dpkg --print-architecture)" \
  --output dist/fleet-telemetry
scripts/build-deb.sh \
  --binary target/release/teslatlas-hub \
  --command-proxy-binary dist/tesla-http-proxy \
  --fleet-telemetry-binary dist/fleet-telemetry \
  --version 1.0.0-alpha.2 \
  --architecture "$(dpkg --print-architecture)" \
  --output "dist/teslatlas-hub_1.0.0-alpha.2_$(dpkg --print-architecture).deb"
sudo dpkg -i "dist/teslatlas-hub_1.0.0-alpha.2_$(dpkg --print-architecture).deb"
```

The package creates the private `teslatlas` service user, configuration at
`/etc/teslatlas-hub/config.toml`, and data directory at
`/var/lib/teslatlas-hub`. New and existing minimal package configurations get
explicit offline defaults for geocoding and terrain; existing table settings
are never overridden. Bootstrap and setup run as that service user:

Supplying both sidecar binaries also installs the official Tesla command proxy,
the pinned Fleet Telemetry receiver bridge, their configuration, and hardened
systemd units. The builder accepts only the exact amd64 or ARM64 outputs bound
in `packaging/linux/sidecar-sha256.lock`; arbitrary caller-supplied digests are
not trusted. Those units remain disabled until the operator supplies the
required keys, certificates, and private loopback bearer. Omit both binaries
for a core-only package; an incomplete or mismatched pair is rejected. The
accepted lock and digests are included in the package.

```sh
sudo -u teslatlas teslatlas-hub bootstrap
sudo -u teslatlas teslatlas-hub setup \
  --access-token-file /private/access-token \
  --refresh-token-file /private/refresh-token \
  --all-vehicles
sudo systemctl enable --now teslatlas-hub.service
sudo teslatlas-hub service status
sudo -u teslatlas teslatlas-hub status
```

The packaged unit is sandboxed: `data_dir` must remain
`/var/lib/teslatlas-hub`, and an optional `terrain.cache_dir` must be inside
that directory. `ProtectHome=true` means TLS certificate and key files cannot
be placed under `/home`, `/root`, or `/run/user`; package-managed TLS material
belongs below `/etc/teslatlas-hub` and must be readable by `teslatlas` (use
`teslatlas:teslatlas`; private key mode `0600`, certificate mode `0644` or
`0600`). To deliberately use another writable path,
create it as `teslatlas` mode `0700`, then add an administrator-owned drop-in
before changing the configuration:

```sh
sudo install -d -o teslatlas -g teslatlas -m 0700 /srv/teslatlas-hub
sudo systemctl edit teslatlas-hub.service
```

```ini
[Service]
ReadWritePaths=/srv/teslatlas-hub
```

Run `sudo systemctl daemon-reload` after saving the drop-in. This augments the
packaged writable path; do not remove `/var/lib/teslatlas-hub` while existing
Hub data remains there.

Package upgrades restart the service only when it was running before the
upgrade. Before replacement, one old binary and unit copy is kept temporarily
under `/run`. Health failures after successful read-only admission restore that
payload. If admission fails on an existing database, the new binary runs a
bounded `bootstrap`, which includes the existing transactional schema
migrations, without copying the database. Bootstrap is the forward-only
boundary: a migration or later admission/health failure retains the new binary
stopped, because the old binary may not understand the advanced schema.
Reinstall the same or a newer package after correcting the reported problem;
automatic downgrade is intentionally unavailable. An installed but stopped
service remains stopped. Package removal stops and disables the unit but
intentionally retains its data directory and the `teslatlas` account.

Service controls are `sudo teslatlas-hub service status|start|stop|restart`.
Migration stays read-only; after a Linux migration, start the package service
with `sudo systemctl start teslatlas-hub.service`.

Run packaging regression checks without compiling Hub:

```sh
sh scripts/test-linux-packaging.sh
```

## CLI setup

Create a private configuration file:

```toml
data_dir = "/Users/me/Library/Application Support/Teslatlas Hub/data"
bind = "127.0.0.1:8080"
```

Initialize and configure every legacy Owner API vehicle without TeslaMate.
Token files must be private (`chmod 600`). Use `--vehicle-id ID` instead of
`--all-vehicles` only to select one account vehicle.

```sh
teslatlas-hub --config /absolute/path/config.toml init
teslatlas-hub --config /absolute/path/config.toml setup \
  --access-token-file /absolute/path/access-token \
  --refresh-token-file /absolute/path/refresh-token \
  --all-vehicles
```

For Fleet API operation, set the provider before setup:

See [Fleet API setup](docs/FLEET_SETUP.md) for developer-application
registration, OAuth scopes, secure code exchange, regional setup, verification,
and virtual-key command requirements.

```toml
[collector]
provider = "fleet"

# Required for signed commands and native Fleet Telemetry configuration.
fleet_command_proxy_url = "https://127.0.0.1:4443/"
fleet_command_proxy_root_certificate_path = "/absolute/path/proxy-ca.pem"

# Optional low-cost push collection. The public receiver terminates vehicle
# mTLS; Hub ingestion stays on 127.0.0.1:8080 behind a private bearer. Tesla
# requires this hostname to use the Fleet application's registered domain.
[collector.fleet_telemetry]
hostname = "telemetry.example.com"
port = 443
ca_certificate_path = "/absolute/path/public-receiver-ca.pem"
ingest_token_path = "/absolute/private/path/fleet-telemetry-bearer"
```

Feed one bounded JSON object to `setup-fleet` through stdin. It contains
`accessToken`, `refreshToken`, `clientId`, `region`, and `expiresInSeconds`;
`region` is `north_america_and_asia_pacific`,
`europe_middle_east_and_africa`, or `china`. Obtain these third-party Fleet
credentials through Tesla's documented authorization flow. Do not put them in
arguments, logs, or shell history.

The linked guide includes a complete no-temporary-file code-exchange pipeline
that feeds this JSON directly to `setup-fleet`.

Run without installation:

```sh
teslatlas-hub --config /absolute/path/config.toml preflight
teslatlas-hub --config /absolute/path/config.toml serve
```

Install the current per-user service:

```sh
teslatlas-hub --config /absolute/path/config.toml install
teslatlas-hub service status
teslatlas-hub --config /absolute/path/config.toml service restart
teslatlas-hub service stop
teslatlas-hub --config /absolute/path/config.toml service start
```

Useful checks:

```sh
teslatlas-hub --config /absolute/path/config.toml status
teslatlas-hub --config /absolute/path/config.toml doctor
teslatlas-hub legal
```

Only the resident collector owns and refreshes provider credentials. Explicit
vehicle actions go through its private local control socket, require
`--confirm`, and require `--vehicle-id UUID` when more than one car is
configured:

```sh
teslatlas-hub --config /absolute/path/config.toml control \
  --vehicle-id HUB-VEHICLE-UUID wake --confirm
teslatlas-hub --config /absolute/path/config.toml control \
  --vehicle-id HUB-VEHICLE-UUID climate-start --confirm
```

Supported actions are wake, climate start/stop, charging start/stop, charge
limit, lock/unlock, flash lights, and horn. Fleet commands require the optional
loopback command proxy above. Fleet wake uses the direct Fleet endpoint.

Successful provider vehicle-data envelopes are recursively stripped of
credential-like fields, including authorization, tokens, passwords, secrets,
API keys, and cookies, and kept only as bounded current observations. Raw
processing rows are pruned after lifecycle projection; this is not an
unbounded provider-response archive.

## Backup and credential recovery

The normal backup is data-only. It contains the catalogue, encrypted token row,
and immutable packs. Pairing invitations and device bearer authority are
removed, so restored devices must pair again. The backup deliberately excludes
the TeslaMate decryption key, Hub cursor-signing key, TLS identity,
configuration, and service state:

```sh
teslatlas-hub --config /absolute/path/config.toml backup \
  --destination /backups/hub-data-2026-08-22
teslatlas-hub verify-backup --source /backups/hub-data-2026-08-22
teslatlas-hub restore-data \
  --source /backups/hub-data-2026-08-22 \
  --destination /restore/hub-data
```

Credential disaster recovery is separate and explicit. Create a random raw
32-byte key in a private mode-0600 file, then export an AES-256-GCM encrypted,
secret-bearing recovery file. Store the data backup, encrypted credential file,
and its encryption key separately.

```sh
umask 077
openssl rand 32 > /private/hub-recovery.key
teslatlas-hub --config /absolute/path/config.toml export-recovery-credentials \
  --destination /separate-private-location/hub-credentials.tthcr \
  --recovery-key-file /private/hub-recovery.key

teslatlas-hub --config /absolute/path/restored-config.toml restore-recovery-credentials \
  --source /separate-private-location/hub-credentials.tthcr \
  --recovery-key-file /private/hub-recovery.key
```

Credential restore requires the matching data-backup installation ID and an
absent `secrets/` directory; it never overwrites existing key material. Restore
credentials only while the service is stopped.

## Optional TeslaMate migration

The PostgreSQL URL must not contain a password. Supply the database password through a protected file.

To migrate history and copy the encrypted legacy token pair using the matching TeslaMate encryption key:

```sh
teslatlas-hub --config /absolute/path/config.toml migrate \
  --source postgresql://reader@127.0.0.1/teslamate \
  --car-id 1 \
  --postgres-password-file /absolute/path/postgres-password \
  --encryption-key-file /absolute/path/teslamate-encryption-key
```

The first copy may run while TeslaMate remains active. At cutover, stop TeslaMate before confirming the final snapshot. Do not let TeslaMate and the Hub refresh the same legacy token concurrently.

Migration streams the supported TeslaMate projection directly from a read-only
PostgreSQL snapshot into Hub packs. It does not create a second raw-history
SQLite copy: disk use is the final Hub data, one active pack build, and compact
comparison state for the stopped cutover pass. “1:1” means the supported
TeslaMate records and values are preserved in Hub's schema, not PostgreSQL
storage-file byte identity.

Where the original encryption key is unavailable, supply fresh legacy access and refresh token files instead:

```sh
teslatlas-hub --config /absolute/path/config.toml migrate \
  --source postgresql://reader@127.0.0.1/teslamate \
  --car-id 1 \
  --postgres-password-file /absolute/path/postgres-password \
  --access-token-file /absolute/path/access-token \
  --refresh-token-file /absolute/path/refresh-token
```

The migration code reads `private.tokens` only for the explicit credential-transfer path. Imported ciphertext remains encrypted at rest; the matching key is written to a mode-0600 file and plaintext tokens exist only in process memory when required. Use a dedicated source role with only the minimum privileges needed for the selected migration mode.

## Optional bounded TeslaMate write-back

Write-back is separate from migration and ordinary collection. The command
below locks and validates exactly one charging-process row, then rolls back and
prints a receipt:

```sh
teslatlas-hub write-back \
  --source postgresql://writer@127.0.0.1/teslamate \
  --car-id 1 \
  --postgres-password-file /absolute/path/postgres-password \
  charge-cost --charging-process-id 604 --cost 12.34
```

Repeat with `--apply` after checking the dry-run receipt to commit that one
cost. No other TeslaMate table or field is writable through Hub.

## Security and operational warning

Vehicle telemetry includes precise location, travel history, identifiers and credential material. Treat the host, backups, logs, exported packs and secret files as sensitive systems.

The default plaintext listener is a loopback transport, not an authentication
boundary. Every local process is trusted for this interface. Never port-forward,
proxy, tunnel, or otherwise expose the plaintext port. Any non-loopback listener
requires TLS and authentication at the Hub or a trusted network boundary.

Before deployment:

1. use a dedicated operating-system account;
2. keep the plaintext Hub listener on loopback only;
3. require authentication and TLS across untrusted networks;
4. restrict database roles to the minimum privileges;
5. keep secrets outside source control and shell history;
6. verify backup restoration;
7. retain a tested rollback path;
8. do not expose development or fake-source endpoints;
9. do not use the software for safety-critical or vehicle-control purposes;
10. confirm that every third-party-service access route is authorised by current terms.

Report vulnerabilities privately under [SECURITY.md](SECURITY.md).

## Release and source status

A release is complete only when its binary, exact source archive, checksums, build instructions, dependency notices and legal notices are published together. See [RELEASE_COMPLIANCE.md](RELEASE_COMPLIANCE.md).

## Legal notices

Copyright © 2026 MAGRATHEAN UK LTD and identified contributors.

Teslatlas Hub is free software licensed under the **GNU Affero General Public License, version 3 only** (`AGPL-3.0-only`). See [LICENSE](LICENSE).

Magrathean-owned material is also subject to the permitted GNU AGPL section 7 notices in [ADDITIONAL_TERMS.md](ADDITIONAL_TERMS.md). Those notices require preservation of reasonable attribution and origin statements; they do not restrict commercial use, competition, modification or interoperability.

Required factual attribution:

> Teslatlas Hub — originally authored by Gyorgy Bolyki and published by MAGRATHEAN UK LTD. Source: https://github.com/magrathean-uk/teslatlas-hub

The attribution is not an advertising requirement and does not prohibit commercial or competitive use.

Users interacting remotely with a modified network deployment must be offered the complete Corresponding Source of the version actually running. See [AGPL compliance](AGPL_COMPLIANCE.md) and [Corresponding Source availability](SOURCE_AVAILABILITY.md).

External contributions require DCO 1.1 sign-off and a completed individual or corporate contributor assignment, unless employment or contractor records already cover the contributor. See [CONTRIBUTING.md](CONTRIBUTING.md).

No trade mark licence is granted. See [TRADEMARKS.md](TRADEMARKS.md).

## Documentation

- [Legal framework](LEGAL.md)
- [Licensing map](LICENSING.md)
- [Copyright and ownership](COPYRIGHT.md)
- [Attribution](ATTRIBUTION.md)
- [Safety and use limits](SAFETY_AND_USE_LIMITS.md)
- [Provenance](PROVENANCE.md)
- [Migration](MIGRATION.md)
- [Privacy and deployment roles](PRIVACY.md)
- [Security policy](SECURITY.md)
- [Contribution rules](CONTRIBUTING.md)
- [DCO 1.1](DCO-1.1.md)
- [Contributor assignment process](DCO_AND_CONTRIBUTOR_ASSIGNMENT_PROCESS.md)
- [Individual contributor assignment](INDIVIDUAL_CONTRIBUTOR_ASSIGNMENT_AGREEMENT.md)
- [Corporate contributor assignment](CORPORATE_CONTRIBUTOR_ASSIGNMENT_AGREEMENT.md)
- [Founder contribution policy](FOUNDER_CONTRIBUTION_POLICY.md)
- [Dependency policy](DEPENDENCY_POLICY.md)
- [Third-party notices](THIRD_PARTY_NOTICES.md)
- [Release compliance](RELEASE_COMPLIANCE.md)
- [Legal changelog](LEGAL_CHANGELOG.md)
- [Branding guidelines](docs/BRANDING_GUIDELINES.md)
- [Support policy](SUPPORT.md)
- [Governance](GOVERNANCE.md)
- [Enforcement policy](ENFORCEMENT_POLICY.md)

## Contact

Magrathean UK Ltd  
Company number 16955343  
16 Caledonian Court, West Street, Watford, England, WD17 1RY  
Email: contact@magrathean.uk

# Teslatlas Hub

Teslatlas Hub is an independent, self-hosted vehicle telemetry collector and local data hub written in Rust. It is being developed for macOS and Linux and is intended to support a native controller application, full command-line operation and an automated bootstrap path.

The Hub is aimed primarily at new self-hosted installations. It also includes an optional, one-way migration path for data and legacy Owner API credentials held in a user-controlled TeslaMate PostgreSQL database. Migration support is secondary and does not make TeslaMate a runtime dependency.

> **Alpha software:** interfaces, storage formats, authentication routes and operational procedures may change. Keep an independent backup and test recovery before using real data.

## Product direction

Teslatlas Hub is intended to provide:

- a native Rust collector and local data store;
- authenticated local synchronisation for the separately distributed Teslatlas client;
- service and CLI operation on macOS and Linux;
- an automated bootstrap/install path;
- new installations that do not require TeslaMate or Grafana;
- optional, read-only migration from an operator-controlled TeslaMate database.

The paid Teslatlas application is a separate product and is not part of this repository.

## Current alpha scope

The current `v1.0.0-alpha.1` implementation is narrower than the planned cross-platform product:

- macOS 12 or later on Apple silicon, and Debian 13 ARM64;
- one vehicle;
- legacy Owner API token authentication; no official Fleet API integration;
- PostgreSQL history import, encrypted token/key transfer, token refresh, Owner API polling, Tesla streaming, lifecycle persistence, backup and repair;
- native AppKit control app plus full CLI access;
- a per-user LaunchAgent on macOS or a systemd service on Debian;
- no Grafana, MQTT, multi-vehicle collection, or App Store build.

## Project boundaries

Teslatlas Hub:

- collects and stores vehicle telemetry under the operator's control;
- exposes a local authenticated sync interface;
- does not require TeslaMate for a new installation;
- does not modify or write to a TeslaMate database during migration;
- does not grant any right to use a vehicle manufacturer's API, account, service or trade marks;
- is not designed for safety-critical, emergency, autonomous-driving or vehicle-control decisions.

A separate client does not become covered by this repository's licence merely because it communicates with the Hub through a documented protocol. That conclusion can change where code is copied, linked or combined, or where the programs form one derivative work.

## Independence and compatibility

Teslatlas Hub is independently maintained by Magrathean UK Ltd. TeslaMate references are limited to truthful migration, provenance and compatibility statements.

**This project is an unofficial community tool and is not affiliated with, endorsed by, or supported by the official TeslaMate project.**

TeslaMate is not bundled, modified or started by the Hub. The migration adapter reads an operator-supplied PostgreSQL source in a read-only transaction and converts selected records into Teslatlas-owned storage. See [MIGRATION.md](MIGRATION.md), [PROVENANCE.md](PROVENANCE.md) and [Independence and interoperability](docs/INDEPENDENCE_AND_INTEROPERABILITY.md).

Tesla, vehicle model names, TeslaMate and all third-party marks belong to their respective owners. Compatibility references do not imply sponsorship.

## Build the current macOS alpha

Requirements for the current tagged alpha are Rust 1.97, Xcode 27 and XcodeGen.

```sh
cargo build --locked --release
scripts/build-macos-app.sh
```

The app is written to `dist/Teslatlas Hub.app`. Current alpha builds are ad-hoc signed and are not notarised.

## Debian ARM64 package

Build the package from an ARM64 Debian host (or the supplied development VM):

```sh
cargo build --locked --release
scripts/build-deb.sh \
  --binary target/release/teslatlas-hub \
  --version 1.0.0-alpha.1 \
  --output dist/teslatlas-hub_1.0.0-alpha.1_arm64.deb
sudo dpkg -i dist/teslatlas-hub_1.0.0-alpha.1_arm64.deb
```

The package creates the private `teslatlas` service user, configuration at
`/etc/teslatlas-hub/config.toml`, and data directory at
`/var/lib/teslatlas-hub`. Bootstrap and setup run as that service user:

```sh
sudo -u teslatlas teslatlas-hub bootstrap
sudo -u teslatlas teslatlas-hub setup \
  --access-token-file /private/access-token \
  --refresh-token-file /private/refresh-token
sudo systemctl enable --now teslatlas-hub.service
sudo teslatlas-hub service status
sudo -u teslatlas teslatlas-hub status
```

Service controls are `sudo teslatlas-hub service status|start|stop|restart`.
Migration stays read-only; after a Linux migration, start the package service
with `sudo systemctl start teslatlas-hub.service`.

## CLI setup

Create a private configuration file:

```toml
data_dir = "/Users/me/Library/Application Support/Teslatlas Hub/data"
bind = "127.0.0.1:8080"
```

Initialize and configure one vehicle without TeslaMate. Token files must be
private (`chmod 600`); add `--vehicle-id ID` only when the account has more
than one vehicle.

```sh
teslatlas-hub --config /absolute/path/config.toml init
teslatlas-hub --config /absolute/path/config.toml setup \
  --access-token-file /absolute/path/access-token \
  --refresh-token-file /absolute/path/refresh-token
```

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

Wake and climate start are explicit one-car commands. They are not used by
scheduled collection and require a fresh command-line confirmation:

```sh
teslatlas-hub --config /absolute/path/config.toml control wake --confirm
teslatlas-hub --config /absolute/path/config.toml control climate-start --confirm
```

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

## Security and operational warning

Vehicle telemetry includes precise location, travel history, identifiers and credential material. Treat the host, backups, logs, exported packs and secret files as sensitive systems.

Before deployment:

1. use a dedicated operating-system account;
2. bind the Hub only to required interfaces;
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

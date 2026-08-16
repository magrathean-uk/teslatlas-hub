# Teslatlas Hub

Teslatlas Hub is an independent, self-hosted vehicle telemetry collector and local data hub written in Rust. It is being developed for macOS and Linux and is intended to support a native application, command-line operation and an automated bootstrap path.

The Hub is aimed at new self-hosted installations. It also includes an optional, one-way migration path for data held in a user-controlled TeslaMate PostgreSQL database. Migration support is secondary and does not make TeslaMate a runtime dependency.

> **Alpha software:** interfaces, storage formats and operational procedures may change. Test recovery and keep an independent backup before using real data.

## Project boundaries

Teslatlas Hub:

- collects and stores vehicle telemetry under the operator's control;
- exposes a local authenticated sync interface;
- supports local CLI and service operation;
- is designed for macOS and Linux;
- does not include Grafana;
- does not require TeslaMate for a new installation;
- does not write to a TeslaMate database during migration;
- does not grant any right to use a vehicle manufacturer's API, account or trade marks.

The separately distributed Teslatlas application is not part of this repository. A separate client does not become covered by this repository's licence merely because it communicates with the Hub through a documented protocol. That conclusion can change where code is copied, linked or combined, or where the programs form one derivative work.

## Independence and compatibility

Teslatlas Hub is independently maintained by Magrathean UK Ltd. TeslaMate compatibility is limited to truthful migration and interoperability statements.

**This project is an unofficial community tool and is not affiliated with, endorsed by, or supported by the official TeslaMate project.**

TeslaMate is not bundled, modified or started by the Hub. The migration adapter reads an operator-supplied PostgreSQL source in a read-only transaction and converts selected records into Teslatlas-owned storage. See [MIGRATION.md](MIGRATION.md), [PROVENANCE.md](PROVENANCE.md) and [Independence and interoperability](docs/INDEPENDENCE_AND_INTEROPERABILITY.md).

Tesla, vehicle model names, TeslaMate and all third-party marks belong to their respective owners. Compatibility references do not imply sponsorship.

## Security and operational warning

Vehicle telemetry includes precise location, travel history, account identifiers and potentially credential material. Treat the host, backups, logs and exported packs as sensitive systems.

Before deployment:

1. use a dedicated operating-system account;
2. bind the Hub only to interfaces that are required;
3. require authentication and TLS across untrusted networks;
4. restrict database users to the minimum privileges;
5. keep secrets outside source control and shell history;
6. verify backup restoration;
7. retain a tested rollback path;
8. do not expose development or fake-source endpoints;
9. do not use the software for safety-critical, emergency or autonomous control;
10. confirm that your use of any third-party service is authorised by its current terms.

Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## Installation status

The repository is currently an alpha implementation. Platform-specific bootstrap and service instructions must be taken from a tagged release, not from an arbitrary branch. Linux packaging is planned; documentation must not present macOS as the permanent or exclusive target.

A release is supported only when its binary, source archive, checksums, build instructions and dependency notices are published together.

## Legal notices and source

Copyright © 2026 Magrathean UK Ltd and contributors.

Teslatlas Hub is licensed under the **GNU Affero General Public License version 3 only**. See [LICENSE](LICENSE).

Magrathean-owned material is also subject to the permitted GNU AGPL section 7 notices in [ADDITIONAL_TERMS.md](ADDITIONAL_TERMS.md). Those notices require preservation of reasonable attribution and origin statements; they do not restrict commercial use, competition, modification or interoperability.

The required attribution is:

> Teslatlas Hub — originally authored by Gyorgy Bolyki and published by Magrathean UK Ltd. Source: https://github.com/magrathean-uk/teslatlas-hub

Users interacting with a modified network deployment must be offered the complete Corresponding Source of the version actually running. See [AGPL compliance](docs/AGPL_COMPLIANCE.md).

No trade mark licence is granted. See [TRADEMARKS.md](TRADEMARKS.md).

## Documentation

- [Legal framework](LEGAL.md)
- [Copyright and ownership](COPYRIGHT.md)
- [Provenance](PROVENANCE.md)
- [Migration](MIGRATION.md)
- [Privacy and deployment roles](PRIVACY.md)
- [Security policy](SECURITY.md)
- [Contribution rules](CONTRIBUTING.md)
- [Third-party notices](THIRD_PARTY_NOTICES.md)
- [Release compliance](RELEASE_COMPLIANCE.md)
- [Legal changelog](LEGAL_CHANGELOG.md)
- [Corresponding Source availability](docs/SOURCE_AVAILABILITY.md)
- [Branding guidelines](docs/BRANDING_GUIDELINES.md)
- [Support policy](SUPPORT.md)
- [Governance](GOVERNANCE.md)

## Contact

Magrathean UK Ltd  
Company number 16955343  
16 Caledonian Court, West Street, Watford, England, WD17 1RY  
Email: contact@magrathean.uk

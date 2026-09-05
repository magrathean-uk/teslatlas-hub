# Teslatlas Hub documentation

Teslatlas Hub 2026.36.1 targets Apple-silicon macOS 13 or later and
Debian 13 on amd64 or ARM64.

Created by György Bolyki. Published and maintained by MAGRATHEAN UK LTD.

Hub is source-only. [Build your own package](guides/build-from-source.md)
before installing. GitHub releases and downloadable assets are no longer
provided; existing source tags and historical notes remain available.

## First installation

1. [Choose your setup path](guides/getting-started.md): new collection or an
   existing TeslaMate history import.
2. Follow [Mac setup and everyday use](guides/install-macos.md) or
   [Debian installation](guides/install-debian.md).
3. [Pair the separately distributed client](guides/getting-started.md#pair-your-client)
   after configuring a secure connection.
4. Keep a [recovery copy](operations/backup-and-recovery.md).

Looking for an installer? Follow [Build from source](guides/build-from-source.md).
If something fails, start with [Troubleshooting](guides/troubleshooting.md).

## Start here

| Task | Guide |
|---|---|
| Choose a host and set up collection | [Getting started](guides/getting-started.md) |
| Install and use the Mac app | [Mac setup and everyday use](guides/install-macos.md) |
| Install the Debian package | [Install on Debian](guides/install-debian.md) |
| Configure collection, TLS, geocoding, or terrain | [Configuration](guides/configuration.md) |
| Configure Fleet API and Fleet Telemetry | [Fleet setup](guides/fleet-setup.md) |
| Learn the command-line interface | [CLI reference](guides/cli.md) |
| Operate and diagnose Hub | [Operations](operations/runbook.md) |
| Back up or recover a deployment | [Backup and recovery](operations/backup-and-recovery.md) |
| Upgrade or plan rollback | [Upgrade and rollback](releases/upgrade.md) |
| Import TeslaMate history | [TeslaMate migration](releases/migration.md) |
| Solve a fault | [Troubleshooting](guides/troubleshooting.md) |

## Understand the system

- [Architecture](architecture/overview.md)
- [Source layout](architecture/source-layout.md)
- [HTTP and sync API](guides/api.md)
- [Security model](architecture/security-model.md)
- [Data and retention](operations/data-and-retention.md)
- [Independence and interoperability](architecture/independence-and-interoperability.md)

## Verify or reproduce a release

- [Calendar versioning](releases/versioning.md)
- [2026.36.1 release notes](releases/release-notes-2026.36.1.md)
- [v1.0.0 release notes](releases/release-notes-v1.0.0.md)
- [v1.0.0-beta.1 historical notes](releases/release-notes-v1.0.0-beta.1.md)
- [Changelog](releases/changelog.md)
- [Verify a release](releases/verification.md)
- [Historical release-key notes](releases/release-keys.md)
- [Release process](releases/releasing.md)
- [Corresponding Source](legal/source-availability.md)
- [Release compliance gate](releases/compliance.md)
- [Provenance](legal/provenance.md)

## Legal and project policies

- [Authorship and stewardship](governance/authorship-and-stewardship.md)
- [Citation metadata](../CITATION.cff)
- [Legal framework](legal/overview.md)
- [Licence](../LICENSE)
- [Additional terms](legal/additional-terms.md)
- [Notices](../NOTICE)
- [Third-party notices](legal/third-party-notices.md)
- [Privacy](legal/privacy.md)
- [Security policy](../.github/SECURITY.md)
- [Support](../.github/SUPPORT.md)
- [Contributing](../.github/CONTRIBUTING.md)
- [Code of conduct](../.github/CODE_OF_CONDUCT.md)

## Maintainer and governance material

- [Governance](governance/governance.md)
- [Contributor agreement process](governance/contributor-agreement-process.md)
- [Dependency policy](legal/dependency-policy.md)
- [Branding guidelines](brand/branding-guidelines.md)
- [Repository settings](maintainers/github-repository-settings.md)
- [Documentation style](maintainers/documentation-style.md)

Tagged release documentation controls for that release. `main` may describe a
newer development state.

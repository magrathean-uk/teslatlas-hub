# Teslatlas Hub documentation

Teslatlas Hub v1.0.0-beta.1 supports Apple-silicon macOS 13 or later and
Debian 13 on amd64 or ARM64.

This version is the first public beta. Its signed tag, platform artifacts, exact
source, and evidence set form one atomic GitHub prerelease. A missing tag page
or required asset means the download is incomplete and must not be installed.

## Start here

| Task | Guide |
|---|---|
| Install the Mac app and service | [Install on macOS](guides/install-macos.md) |
| Install the Debian package | [Install on Debian](guides/install-debian.md) |
| Configure collection, TLS, geocoding, or terrain | [Configuration](guides/configuration.md) |
| Configure Fleet API and Fleet Telemetry | [Fleet setup](guides/fleet-setup.md) |
| Learn the command-line interface | [CLI reference](guides/cli.md) |
| Operate and diagnose Hub | [Operations](operations/runbook.md) |
| Back up or recover a deployment | [Backup and recovery](operations/backup-and-recovery.md) |
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

- [Verify a release](releases/verification.md)
- [Release signing keys](releases/release-keys.md)
- [Release process](releases/releasing.md)
- [Corresponding Source](legal/source-availability.md)
- [Release compliance gate](releases/compliance.md)
- [Provenance](legal/provenance.md)

## Legal and project policies

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

Tagged release documentation controls for that release. `main` may describe a
newer development state.

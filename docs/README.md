# Teslatlas Hub documentation

Teslatlas Hub v1.0.0-beta.1 supports Apple-silicon macOS 13 or later and
Debian 13 on amd64 or ARM64.

This version is the first public beta. Its signed tag, platform artifacts, exact
source, and evidence set form one atomic GitHub prerelease. A missing tag page
or required asset means the download is incomplete and must not be installed.

## Start here

| Task | Guide |
|---|---|
| Install the Mac app and service | [Install on macOS](INSTALL_MACOS.md) |
| Install the Debian package | [Install on Debian](INSTALL_DEBIAN.md) |
| Configure collection, TLS, geocoding, or terrain | [Configuration](CONFIGURATION.md) |
| Configure Fleet API and Fleet Telemetry | [Fleet setup](FLEET_SETUP.md) |
| Learn the command-line interface | [CLI reference](CLI.md) |
| Operate and diagnose Hub | [Operations](OPERATIONS.md) |
| Back up or recover a deployment | [Backup and recovery](BACKUP_RECOVERY.md) |
| Import TeslaMate history | [TeslaMate migration](../MIGRATION.md) |
| Solve a fault | [Troubleshooting](TROUBLESHOOTING.md) |

## Understand the system

- [Architecture](ARCHITECTURE.md)
- [HTTP and sync API](API.md)
- [Security model](SECURITY_MODEL.md)
- [Data and retention](DATA_AND_RETENTION.md)
- [Independence and interoperability](INDEPENDENCE_AND_INTEROPERABILITY.md)

## Verify or reproduce a release

- [Verify a release](../RELEASE_VERIFICATION.md)
- [Release signing keys](../RELEASE_KEYS.md)
- [Release process](RELEASING.md)
- [Corresponding Source](../SOURCE_AVAILABILITY.md)
- [Release compliance gate](../RELEASE_COMPLIANCE.md)
- [Provenance](../PROVENANCE.md)

## Legal and project policies

- [Legal framework](../LEGAL.md)
- [Licence](../LICENSE)
- [Additional terms](../ADDITIONAL_TERMS.md)
- [Notices](../NOTICE)
- [Third-party notices](../THIRD_PARTY_NOTICES.md)
- [Privacy](../PRIVACY.md)
- [Security policy](../SECURITY.md)
- [Support](../SUPPORT.md)
- [Contributing](../CONTRIBUTING.md)
- [Code of conduct](../CODE_OF_CONDUCT.md)

Tagged release documentation controls for that release. `main` may describe a
newer development state.

Files under `docs/review/` are dated historical review records, not current
build or release instructions. Use `docs/RELEASING.md` for current gates.

<p align="center">
  <img src="https://raw.githubusercontent.com/magrathean-uk/magrathean-uk/main/assets/icons/teslatlas.png" width="96" height="96" alt="">
</p>

<h1 align="center">Teslatlas Home Assistant</h1>

<p align="center">Read-only vehicle sensors for Home Assistant, using the public Teslatlas Hub HTTP API.</p>

<p align="center">
  <a href="docs/architecture/overview.md">Documentation</a> ·
  <a href="LICENSE">Licence</a>
</p>

## Overview

Read-only vehicle sensors for Home Assistant, using the public Teslatlas Hub HTTP API. The integration connects to your Hub; it does not contact Tesla or ask for Tesla credentials.

## Status

This checkout contains candidate version `2026.36.2`, bound to `hub-http-v1@1.0.0`. It has not been published through HACS. Do not install it on a production Home Assistant instance yet.

The [compatibility record](compatibility/hub.json) names one exact source-built, synthetic Debian 13 ARM64 Container lane. The later [development status](docs/development/archive/STATUS.json) records that the former runtime and external artifacts were removed. Historical results do not prove that a test installation is running or that this checkout works with a current real Hub. HA OS, real-data operation, replacement upgrades and broader platform acceptance are not established by that record.

## What it provides

- Manual setup, invitation pairing, reauthentication and endpoint reconfiguration.
- One polling coordinator per entry, with a 30-second interval, no overlapping refreshes and at most four current-state reads in flight.
- Vehicle sensors for charge level, charging state, power and limit, range, odometer, activity, temperatures, lock state, software state and telemetry age.
- Stable vehicle and entity identities, dynamic vehicle addition, and unavailable entities when a vehicle disappears.
- Credential rotation and diagnostics that redact credentials, identities, endpoints and locations.

Missing fields remain unknown; numeric zero remains zero. There are no vehicle commands, buttons or switches. Zeroconf and event streaming are not implemented by the current profile. Collector health, Fleet cost, backup age and data quality are not exposed as sensors.

## Try it in an isolated Home Assistant installation

The locked development environment uses Home Assistant `2026.9.4`. The minimum version declared in [hacs.json](hacs.json) is `2026.8.3`, and the Compose example below pins that version. Use a compatible Hub that implements the checked profile, a reachable HTTPS endpoint with a trusted certificate, and a fresh pairing invitation. A certificate pin supplements normal certificate and hostname validation.

1. Back up the Home Assistant configuration and the previous component before replacing an installation.
2. From this repository, copy only `custom_components/teslatlas_hub` to `/config/custom_components/teslatlas_hub`, including its `profile` and `translations` directories. Exclude generated caches. Restart Home Assistant.
3. Open **Settings > Devices & services > Add integration > Teslatlas Hub**.
4. Enter the Hub host, port and TLS settings. Keep TLS enabled; supply the certificate fingerprint when provided with the invitation.
5. Enter the pairing ID, secret and device name. Complete the validation step so the integration can confirm authenticated access before saving the credential.

This is a custom integration, not a Supervisor add-on or a HACS publication. Do not copy the test tools, fixtures or development environment into `/config`.

For an isolated Linux Container installation, use the [Container guide](docs/guides/docker.md) and the pinned [Compose example](compose.yaml). A Mac can access a separately managed Home Assistant runtime; this project does not install a native macOS Home Assistant service.

## Everyday operation

Use the entry's reauthentication flow with a fresh invitation when a credential expires or is revoked. Reconfigure an endpoint through Home Assistant; the new endpoint must identify the same Hub. Stop if the certificate, pin or Hub identity is unexpected.

A temporary outage makes affected entities unavailable. The integration retries with bounded backoff up to 300 seconds. Missing current-state values stay unknown. See [support](.github/SUPPORT.md) for setup and recovery checks.

Protect the whole Home Assistant configuration directory, including `core.config_entries`, as credential-bearing data. To roll back, stop Home Assistant and restore the matching configuration backup and component version before restarting. Older components may not understand a newer configuration schema.

## Development and reference

Start with [contributing](.github/CONTRIBUTING.md) for local setup and targeted checks.

- [Architecture](docs/architecture/overview.md): requests, credentials and entity lifecycle.
- [Protocol binding](docs/reference/protocol-readiness.md): embedded profile and compatibility limits.
- [Product versioning](docs/reference/product-versioning.md): component, Home Assistant and protocol versions.
- [Security](.github/SECURITY.md): reporting and data boundaries.

## Licence

Teslatlas Home Assistant is licensed under the Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). See [licensing and attribution](docs/legal/licensing.md) for the distinction between this integration, its protocol bundle and third-party dependencies. Contributions: see [CONTRIBUTING](.github/CONTRIBUTING.md).

<sub>© 2026 MAGRATHEAN UK LTD · [Legal](https://github.com/magrathean-uk/.github/blob/main/LEGAL.md)</sub>

# Home Assistant Container

This repository includes a minimal Docker Compose example for the official
Home Assistant Container image. It runs Home Assistant with persistent
configuration and mounts the Teslatlas Hub component read-only:

```yaml
image: ghcr.io/home-assistant/home-assistant:2026.8.3
```

The example remains operator-facing source guidance for the Home Assistant
2026.8.3 baseline. The selected Debian 13 ARM64 runtime has supplied a current
redacted Container receipt, but that bounded result is not a general
platform-support claim.

## Active scope

Owner direction for this programme (2026-09-09) keeps Debian 13 ARM64 and
Apple-silicon Mac work active. A Mac may consume an explicitly selected
existing supported HA OS/Container/VM runtime, including the accepted ARM64
Container where appropriate; this project never creates a native macOS HA
daemon. Debian x86/x86_64/amd64, Intel Mac, Azure, and the complete
cross-architecture matrix are deferred. Any retained x86 receipt is historical
evidence only and is not an instruction to start, repair, or mutate that target.

## Before starting

Use a supported Linux host with Docker Engine and Compose. [Home Assistant's
Linux installation guidance](https://www.home-assistant.io/installation/linux/)
currently requires Docker Engine 23.0.0 or newer and does not treat Docker
Desktop as supported Linux installation evidence.
Check the host before starting:

```bash
docker version
docker compose version
mkdir -p ha-config/custom_components
test -f custom_components/teslatlas_hub/manifest.json
test -f custom_components/teslatlas_hub/profile/hub-http-v1/1.0.0/profile.json
docker compose config --quiet
```

The `ha-config` directory is persistent Home Assistant state and must stay
private. The component source is mounted from this checkout and is read-only
inside the container. Do not copy `tools/`, the Python development environment,
or test fixtures into `/config`. Use a clean source checkout for the mount;
generated `__pycache__` directories and test output must not be treated as
installed component members.

## Start and pair

From this repository root, start the single service after the configuration
check succeeds:

```bash
docker compose up -d
docker compose logs --follow homeassistant
```

Home Assistant uses host networking and listens on port 8123. In this setup,
`localhost` from the container means the Linux host. Configure Teslatlas Hub
with the real reachable Hub hostname or address, its trusted TLS certificate
chain, and the invitation values supplied by the Hub. Keep hostname
verification and the configured TLS pin enabled. Do not use `ssl=False`, a
plain HTTP pairing URL, or a copied bearer in Compose configuration.

If the Hub uses a private CA, create a combined bundle containing the image's
normal public roots plus that private CA under the persistent, owner-controlled
`ha-config` tree. Mount it through `/config` and set both variables to the same
container path before recreating the service:

```yaml
environment:
  SSL_CERT_FILE: /config/teslatlas-private/ca-bundle.pem
  REQUESTS_CA_BUNDLE: /config/teslatlas-private/ca-bundle.pem
```

Home Assistant's managed HTTP connector reads `REQUESTS_CA_BUNDLE`; the second
variable is therefore required even when direct `curl` or Python probes already
work with `SSL_CERT_FILE`. Do not replace the system trust store or use a
fixture-only bundle that omits normal public roots.

The normal supported flow is Home Assistant's Settings → Devices & services →
Add integration → Teslatlas Hub. The invitation is used once to obtain the
device credential; invitation material is not stored by the integration.
Pairing, the UI flow, and a real Hub observation still require the pinned
runtime and isolated Hub resources described in the working-product plan.

## Stop, backup, and replace

Home Assistant stores its database, config entries, credentials, and entity
registry under `ha-config`. Back up that directory while the service is
stopped before replacing the component or selecting a new image:

```bash
docker compose stop
tar -C ha-config -czf ../teslatlas-ha-config-backup.tgz .
```

The Compose service has a 60-second stop grace period so Home Assistant can
close its database. Retain the backup and the exact component bytes together.
To roll back, stop Home Assistant, restore the matching component directory
and configuration backup, then start the service again. An older component
cannot be assumed to read an unknown newer major config-entry schema.

After changing the mounted component source, restart the service:

```bash
docker compose restart homeassistant
```

For an intentionally selected image update, back up `/config`, record the old
image digest and component fingerprint, then pull and recreate the service:

```bash
docker compose pull homeassistant
docker compose up -d
```

Do not use the moving `stable` tag, delete `ha-config`, or use `docker compose
down --volumes` as ordinary shutdown. Container installation is self-managed;
it does not provide Home Assistant OS Supervisor apps or add-ons. Existing HA
installations can instead copy only `custom_components/teslatlas_hub`, including
its profile and translations, into `/config/custom_components` and restart HA.

On Apple-silicon macOS, use an explicitly selected existing HA OS, Container,
or VM target; the accepted ARM64 Container may be consumed where that is the
selected runtime. The Teslatlas Hub option stages the integration into that
target and does not install or launch a native macOS Home Assistant daemon.
Inspect the existing runtime first and keep the selector pending until a
supported target and owner handoff are concrete; do not create a replacement
VM for this plan.

For the accepted Debian ARM64 target, `tools/mac-consumer-forward` provides a
reversible UI-only SSH forward to port 8123. It does not start or provision a
runtime; the target guest must already be running, and the target continues to
own all HA state and Hub credentials.

## Current verification boundary

The selected Debian 13 ARM64 lane has been exercised with Docker Engine
26.1.5+dfsg1 and Compose 2.26.1. The resolved official image is
`ghcr.io/home-assistant/home-assistant:2026.8.3`, architecture `arm64/linux`,
with its immutable digest recorded in the redacted
[development status receipt](development/STATUS.json). That lane passed real
user-source config-flow pairing against a disposable trusted-TLS Hub fixture,
replay rejection, a changed scheduled sensor observation, Hub outage and
restart recovery, revoked-bearer reauthentication, reload, reconfigure,
disable/re-enable, redacted diagnostics, and a controlled Container stop/up
with one config entry, two devices, and 26 entities retained. The exact
official multi-platform repository digest and ARM64 platform image ID are in
the receipt. The component is mounted read-only and the fixture CA is an
owner-only file in the disposable `/config` tree. System trust alone correctly
rejected that private fixture CA; this isolated lane did not accept unrelated
outbound integrations. Keep the persistent `/config` tree,
including `core.config_entries`, owner-only.

This is one selected Linux Container acceptance lane, not a general support
claim. Apple-silicon Mac work remains active, but its HA use must consume an
explicitly selected existing supported runtime; no native macOS HA service is
claimed here. A fresh Debian 13 x86_64 QEMU target supplied a separate partial
X1 receipt with the same official 2026.8.3 image on amd64/linux. It completed
UI pairing against the primary fixture through a temporary loopback-preserving
forward, revoked-bearer repair, UI reload, Home Assistant restart, Container
restart, a malformed component-candidate rollback, clean stop/up, and forced
stop/up. The x86 lane retained one Teslatlas config entry, two devices, and 26
entities; invitation and bearer values remain redacted. Its combined
system/fixture trust bundle and component mounts are read-only. This x86
receipt is preserved historical evidence only; owner scope pauses all further
x86/Intel/Azure work and it must not block ARM64/Mac usability.

HA OS, native macOS service creation, HACS/publication, replacement upgrade,
and the full multi-platform/parity matrix remain pending. The standalone
`docker-compose config --quiet` check on a host without a daemon remains only
static YAML validation.

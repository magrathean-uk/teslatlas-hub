# Home Assistant Container

[compose.yaml](../compose.yaml) is a minimal Linux Container example. It pins `ghcr.io/home-assistant/home-assistant:2026.8.3`, persists `ha-config`, mounts the component read-only and allows 60 seconds for shutdown. It is intended for isolated evaluation of this candidate.

The selected Debian 13 ARM64 lane has historical synthetic acceptance evidence, which is not a general support claim. The [later status record](development/STATUS.json) says the former runtime was removed. These instructions do not imply that it still exists.

## Prepare an isolated installation

Use an existing Linux host with Docker Engine and Compose. Check the [Home Assistant Linux installation guide](https://www.home-assistant.io/installation/linux/) for host requirements. From this repository root:

```sh
docker version
docker compose version
mkdir -p ha-config/custom_components
test -f custom_components/teslatlas_hub/manifest.json
test -f custom_components/teslatlas_hub/profile/hub-http-v1/1.0.0/profile.json
docker compose config --quiet
```

Review the example timezone for your installation. Use clean component files without generated caches. Keep `ha-config` private and out of Git; it will contain credentials, registry state and the Home Assistant database. Do not copy test fixtures, tools or a Python development environment into it.

The example uses host networking. The Home Assistant UI normally listens on port 8123; manage access through the host's existing network policy and do not expose this evaluation installation publicly. `localhost` from this container refers to the Linux host, not another computer.

## TLS trust and pairing

Use a reachable HTTPS Hub endpoint with a certificate trusted by Home Assistant and a hostname that matches that certificate. A leaf-certificate pin supplements this trust check. Do not disable certificate validation or use plain HTTP for invitation or bearer traffic.

For a private CA, prepare a combined PEM bundle with the normal public roots and the intended Hub CA inside the owner-controlled `ha-config` directory. Add both variables under the existing service's `environment` mapping, using the same container path:

```yaml
SSL_CERT_FILE: /config/teslatlas-private/ca-bundle.pem
REQUESTS_CA_BUNDLE: /config/teslatlas-private/ca-bundle.pem
```

The recorded Home Assistant 2026.8.3 setup uses `REQUESTS_CA_BUNDLE` for its managed HTTP context. Setting only `SSL_CERT_FILE` for a command-line probe does not establish that Home Assistant trusts the Hub. Do not replace the host trust store or use a bundle that drops the public roots.

After reviewing the configuration:

```sh
docker compose up -d
docker compose logs --follow homeassistant
```

Open Home Assistant, finish its onboarding and add **Teslatlas Hub** under **Settings > Devices & services**. Use a fresh invitation and complete the authenticated validation step. Review logs locally; do not publish credentials or raw state.

## Stop, back up and update

Stop before making a private backup:

```sh
docker compose stop
umask 077
tar -C ha-config -czf ../teslatlas-ha-config-backup.tgz .
```

Choose a unique private backup filename for each backup. Retain the exact previous component bytes with the matching configuration backup. Do not use `docker compose down --volumes` as a shutdown or cleanup step.

After replacing component files, start a stopped service with `docker compose up -d`. Restart a running service with:

```sh
docker compose restart homeassistant
```

Before selecting a different image, record the current image identity, back up the configuration and verify component compatibility. Keep the image version explicit. Pull and recreate only after that choice:

```sh
docker compose pull homeassistant
docker compose up -d
```

To roll back, stop the service and restore the matching component and configuration backup before starting. An older component may reject a newer major config-entry schema.

## Scope of the example

Container installations are self-managed and do not include Home Assistant OS Supervisor apps. This integration does not create a native macOS service or provision a guest. A Mac may use a separately selected, operator-managed Home Assistant runtime; reachability alone is not pairing or lifecycle acceptance.

The retained `tools/mac-consumer-forward` and `tools/mac-hub-forward` helpers belong to historical private runtime arrangements. They do not provision Home Assistant. Inspect their arguments and the current runtime ownership before using them; old defaults do not identify a currently available guest.

The historical Container receipt covered pairing, scheduled refresh, outage recovery, reauthentication and lifecycle checks against synthetic data. Fresh acceptance must identify the exact component, image, Hub profile and runtime. It must not inherit claims from a deleted environment.

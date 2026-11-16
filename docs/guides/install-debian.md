# Install on Debian

Tesatlas Hub targets Debian 13 on amd64 and ARM64. First
[build a local package](build-from-source.md); no prebuilt releases are provided.
The current `2026.36.2` core-only package contains core/Legacy collection
without Fleet command-proxy or Fleet Telemetry companions. It is not a
complete Fleet installation. [Verify the package](../releases/verification.md)
before installing. For an existing deployment, follow
[Upgrade and rollback](../releases/upgrade.md). The historical `2026.36.1`
limits remain in its [release note](../releases/release-notes-2026.36.1.md).

For the verified local development route, see
[Mac to Debian ARM64 builds](build-from-source.md#mac-to-debian-arm64-development-build).
That route produces an unbound executable for an isolated service test; it does
not install the package or its service account and paths described below.

## Select the package

```sh
dpkg --print-architecture
```

## Install

```sh
sudo dpkg -i "tesatlas-hub_2026.36.2_$(dpkg --print-architecture).deb"
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml bootstrap
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml doctor
```

The package creates:

- service account `tesatlas`;
- configuration `/etc/tesatlas-hub/config.toml`;
- data directory `/var/lib/tesatlas-hub`;
- hardened `tesatlas-hub.service`;
- optional disabled Fleet command-proxy and Telemetry units when included.

## Configure credentials

For legacy tokens, create owner-readable token files outside source control:

```sh
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml setup \
  --access-token-file /etc/tesatlas-hub/private/access-token \
  --refresh-token-file /etc/tesatlas-hub/private/refresh-token \
  --all-vehicles
```

The [Fleet setup](fleet-setup.md) guide applies to installations with separately
verified compatible Fleet companions; those companions are absent from the
core-only package built by the command above.

## Start and inspect

```sh
sudo systemctl enable --now tesatlas-hub.service
sudo systemctl status tesatlas-hub.service --no-pager
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml status
sudo -u tesatlas -- /usr/bin/tesatlas-hub \
  --config /etc/tesatlas-hub/config.toml doctor
```

Service controls are also available through:

```sh
sudo /usr/bin/tesatlas-hub service status
sudo /usr/bin/tesatlas-hub service start
sudo /usr/bin/tesatlas-hub service stop
sudo /usr/bin/tesatlas-hub service restart
```

On Linux, these controls report systemd's `ActiveState`; `running` means the unit
is active, not that its HTTPS listener is ready. Check the configured `/readyz`
endpoint separately using the intended certificate trust and hostname. A readiness
check should confirm its `ready` response and match
`x-teslatlas-native-config-sha256` to the loaded config bytes and
`x-teslatlas-native-process-id` to the selected unit's `MainPID`. Do not send pairing
or bearer credentials over the packaged default's plain loopback HTTP listener.

## Paths and sandboxing

Keep `data_dir = "/var/lib/tesatlas-hub"` for the packaged unit. TLS keys and
certificates belong below `/etc/tesatlas-hub`; `ProtectHome=true` prevents the
service from reading `/home`, `/root`, or `/run/user`.

To add another writable data path, create it as the service user and add an
administrator-owned systemd override:

```sh
sudo install -d -o tesatlas -g tesatlas -m 0700 /srv/tesatlas-hub
sudo systemctl edit tesatlas-hub.service
```

Add:

```ini
[Service]
ReadWritePaths=/srv/tesatlas-hub
```

Then run `sudo systemctl daemon-reload`. Do not remove the existing writable
path while data remains there.

## Logs and removal

```sh
sudo journalctl -u tesatlas-hub -n 200 --no-pager
sudo dpkg -r tesatlas-hub
```

Package removal stops and disables the service but preserves the service user,
configuration, and data. Back up and verify recovery before permanent deletion.

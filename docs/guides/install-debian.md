# Install on Debian

Teslatlas Hub targets Debian 13 on amd64 and ARM64. First
[build a local package](build-from-source.md); no prebuilt releases are provided.
The core-only packages contain core/legacy collection without Fleet command-proxy or Fleet
Telemetry companions. They are not a complete Fleet installation. Read the
[release limits](../releases/release-notes-2026.36.1.md) and
[verify the package](../releases/verification.md) before installing. For an
existing deployment, follow [Upgrade and rollback](../releases/upgrade.md).

## Select the package

```sh
dpkg --print-architecture
```

## Install

```sh
sudo dpkg -i "teslatlas-hub_2026.36.1_$(dpkg --print-architecture).deb"
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml bootstrap
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml doctor
```

The package creates:

- service account `teslatlas`;
- configuration `/etc/teslatlas-hub/config.toml`;
- data directory `/var/lib/teslatlas-hub`;
- hardened `teslatlas-hub.service`;
- optional disabled Fleet command-proxy and Telemetry units when included.

## Configure credentials

For legacy tokens, create owner-readable token files outside source control:

```sh
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml setup \
  --access-token-file /etc/teslatlas-hub/private/access-token \
  --refresh-token-file /etc/teslatlas-hub/private/refresh-token \
  --all-vehicles
```

The [Fleet setup](fleet-setup.md) guide applies to installations with separately
verified compatible Fleet companions; those companions are absent from these
2026.36.1 Debian packages.

## Start and inspect

```sh
sudo systemctl enable --now teslatlas-hub.service
sudo systemctl status teslatlas-hub.service --no-pager
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml status
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml doctor
```

Service controls are also available through:

```sh
sudo /usr/bin/teslatlas-hub service status
sudo /usr/bin/teslatlas-hub service start
sudo /usr/bin/teslatlas-hub service stop
sudo /usr/bin/teslatlas-hub service restart
```

## Paths and sandboxing

Keep `data_dir = "/var/lib/teslatlas-hub"` for the packaged unit. TLS keys and
certificates belong below `/etc/teslatlas-hub`; `ProtectHome=true` prevents the
service from reading `/home`, `/root`, or `/run/user`.

To add another writable data path, create it as the service user and add an
administrator-owned systemd override:

```sh
sudo install -d -o teslatlas -g teslatlas -m 0700 /srv/teslatlas-hub
sudo systemctl edit teslatlas-hub.service
```

Add:

```ini
[Service]
ReadWritePaths=/srv/teslatlas-hub
```

Then run `sudo systemctl daemon-reload`. Do not remove the existing writable
path while data remains there.

## Logs and removal

```sh
sudo journalctl -u teslatlas-hub -n 200 --no-pager
sudo dpkg -r teslatlas-hub
```

Package removal stops and disables the service but preserves the service user,
configuration, and data. Back up and verify recovery before permanent deletion.

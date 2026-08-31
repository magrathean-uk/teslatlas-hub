# Install on Debian

Teslatlas Hub v1.0.0-beta.2 supports Debian 13 on amd64 and ARM64.

## Select and verify the package

```sh
dpkg --print-architecture
```

No binary release is published yet. The package instructions below apply only
after the complete signed prerelease is published and passes
[release verification](../releases/verification.md).

## Install

```sh
sudo dpkg -i "teslatlas-hub_1.0.0-beta.2_$(dpkg --print-architecture).deb"
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

For Fleet API, use the bounded standard-input flow in
[Fleet setup](fleet-setup.md).

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

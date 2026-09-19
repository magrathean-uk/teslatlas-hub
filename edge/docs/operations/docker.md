# Docker installation

The Compose example runs one Linux Edge image twice: `edge` owns the encrypted
spool, Hub mTLS listener and loopback admission; `receiver` runs the pinned
Fleet Telemetry bridge in a separate filesystem and PID namespace while sharing
Edge's network namespace. This is a Linux container workflow. Docker Desktop
on macOS is a Linux VM and does not establish native macOS service acceptance.

The image is built locally from the pinned Rust 1.98 and Go 1.27.0 toolchains.
The bridge build verifies the upstream archive and dispatcher patch checksums.
No credentials, certificates or tokens are part of the build context or image.
The runtime user is fixed at UID/GID `10001:10001`; the receiver bearer must be
an owner-only regular file owned by that identity.

## Prepare a private installation

Use a new or explicitly reviewed directory outside the repository. The example
uses `/srv/teslatlas-edge` and keeps each role's mounts separate:

```sh
export EDGE_RUNTIME_DIR=/srv/teslatlas-edge
sudo install -d -m 0700 -o 10001 -g 10001 \
  "$EDGE_RUNTIME_DIR/state" \
  "$EDGE_RUNTIME_DIR/receiver-secret" \
  "$EDGE_RUNTIME_DIR/config" \
  "$EDGE_RUNTIME_DIR/hub-tls" \
  "$EDGE_RUNTIME_DIR/vehicle-tls"
sudo install -m 0600 -o 10001 -g 10001 \
  packaging/docker/config.toml.example \
  "$EDGE_RUNTIME_DIR/config/config.toml"
sudo install -m 0600 -o 10001 -g 10001 \
  packaging/docker/fleet-telemetry.json.example \
  "$EDGE_RUNTIME_DIR/config/fleet-telemetry.json"
```

Before startup, provide the dedicated Hub-link files in `hub-tls/`:
`hub-server.crt`, `hub-server.key`, and `hub-client-ca.crt`. Provide the
separate vehicle receiver certificate, key, and client CA in `vehicle-tls/` as
`vehicle-tls.crt`, `vehicle-tls.key`, and `vehicle-client-ca.crt`. These PKI
roles are not interchangeable.

The receiver's mounted TLS files must be owned by UID/GID `10001:10001`. The
receiver process reads them as that identity.
After copying files as root, normalize the receiver input ownership and keep
the private key owner-only:

```sh
sudo chown 10001:10001 "$EDGE_RUNTIME_DIR/vehicle-tls/vehicle-tls.key"
sudo chmod 0600 "$EDGE_RUNTIME_DIR/vehicle-tls/vehicle-tls.key"
```

The public receiver certificate and client CA may remain mode 0644, but must be
readable by UID/GID `10001:10001`. A root-owned mode-0600 receiver key causes
the sidecar to exit before it can listen.
The receiver bearer is created by Edge `init` in `receiver-secret/` and the
spool key and mutable Hub credential store live in `state/`.

The final mount table is:

| Host input | Edge container | Receiver container |
| --- | --- | --- |
| `state/` | `/var/lib/teslatlas-edge` read/write | absent |
| `receiver-secret/` | `/run/teslatlas-edge-receiver` read/write | same path read-only |
| `config/config.toml` | `/etc/teslatlas-edge/config.toml` read-only | absent |
| `config/fleet-telemetry.json` | absent | `/etc/teslatlas-edge/fleet-telemetry.json` read-only |
| `hub-tls/` | `/run/teslatlas-edge-hub-tls` read-only | absent |
| `vehicle-tls/` | absent | `/run/teslatlas-edge-vehicle-tls` read-only |

The receiver environment points to
`/run/teslatlas-edge-receiver/receiver-token`. Its JSON uses
`/run/teslatlas-edge-vehicle-tls/vehicle-tls.crt` and `.key`, and listens on
8444 inside the shared network namespace. Host raw TCP 443 is mapped directly
to that port; Edge Hub TLS is published on the selected private or tunnel
address at 8443 by default. Set `EDGE_HUB_PORT` and
`EDGE_RECEIVER_PORT` for a disposable isolated run; the defaults preserve
8443 and 443. Port 8080 remains loopback-only and is never published.

## Initialize and start

Run the image build and one-time initialization after reviewing the mounted
inputs. `init` refuses to overwrite an existing secret.

```sh
docker compose build edge
docker compose run --rm --no-deps --entrypoint /usr/bin/teslatlas-edge \
  edge --config /etc/teslatlas-edge/config.toml init
docker compose run --rm --no-deps --entrypoint /usr/bin/teslatlas-edge \
  edge --config /etc/teslatlas-edge/config.toml doctor
docker compose up --build -d
docker compose exec -T edge curl --fail --silent --show-error \
  http://127.0.0.1:8080/healthz
docker compose exec -T edge curl --fail --silent --show-error \
  http://127.0.0.1:8080/readyz
```

The liveness healthcheck only proves that Edge is listening. Receiver
acceptance requires the receiver process, its mTLS listener on host 443, and a
dispatcher request that reaches durable Edge admission. `/readyz` can be 503
for a full or degraded spool while `/healthz` remains a liveness endpoint.

Enrol a Hub credential in a private one-off container. Capture the one-time
JSON output with `umask 077`; do not put its token into Compose environment
values, shell history or task output.

```sh
umask 077
edge_enrolment_file=$(mktemp)
docker run --rm --log-driver=none --user 10001:10001 \
  --entrypoint /usr/bin/teslatlas-edge \
  --mount "type=bind,src=$EDGE_RUNTIME_DIR/state,dst=/var/lib/teslatlas-edge" \
  --mount "type=bind,src=$EDGE_RUNTIME_DIR/receiver-secret,dst=/run/teslatlas-edge-receiver" \
  --mount "type=bind,src=$EDGE_RUNTIME_DIR/config/config.toml,dst=/etc/teslatlas-edge/config.toml,readonly" \
  --mount "type=bind,src=$EDGE_RUNTIME_DIR/hub-tls,dst=/run/teslatlas-edge-hub-tls,readonly" \
  teslatlas-edge:local --config /etc/teslatlas-edge/config.toml \
  credential enrol home-hub --ttl-seconds 7776000 > "$edge_enrolment_file"
chmod 600 "$edge_enrolment_file"
```

The current Edge bearer grants the complete queue. Bind one intended Hub
consumer to the admitted source/vehicle identity and supported VIN scope; do
not run a simultaneous direct Fleet receiver for the same source. Hub loads
its mounted CA, client certificate/key and bearer when constructing its
consumer, so replace them through the supported Hub restart/reconstruction
path when rotating credentials.

## Rotate and replace

Receiver bearer rotation is a coordinated stop/start because Edge and the
bridge cache it at startup:

```sh
docker compose stop receiver edge
docker compose run --rm --no-deps --entrypoint /usr/bin/teslatlas-edge \
  edge --config /etc/teslatlas-edge/config.toml receiver-token rotate \
  > "$EDGE_RUNTIME_DIR/receiver-token-rotation.json"
chmod 600 "$EDGE_RUNTIME_DIR/receiver-token-rotation.json"
docker compose up -d edge receiver
```

For a coordinated image/config replacement, preserve all persistent mounts and
use `docker compose stop receiver edge` followed by
`docker compose up -d --force-recreate edge receiver`. Never use `down -v`,
remove the spool, or rerun `init` as a reconnect fix. A spool replacement or a
snapshot older than the Hub's durable frontier requires explicit lineage and
receipt reconciliation.

## Acceptance and limits

Run `docker compose config` before a disposable acceptance. Record the tested
Docker Compose version, image digest, target architecture, UID/modes, actual
mounts, native linkage, published ports and absence of secret bytes from image
history/logs. Confirm the receiver cannot read the Edge spool key or Hub
credential mount. Exercise Edge restart, receiver restart, coordinated
replacement, full stop/start, bearer rotation, pending delivery during Hub
outage, old ACK replay and successful drain with actual Hub output.

The Docker files are source-level packaging in this checkout. A green YAML
parse or image build is not installed Edge/Hub acceptance, and no Docker daemon
was available during this execution if the commands above are reported as
untested. Linux amd64/arm64, filesystem headroom, certificate expiry, raw TCP
routing and the single-writer boundary remain separate evidence items.

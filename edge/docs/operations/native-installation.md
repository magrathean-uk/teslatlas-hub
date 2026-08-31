# Native installation

Install Edge only on a host you operate. The receiver needs a public TCP route;
the Hub API should be restricted to the home Hub's IP or private tunnel.

> **Critical:** Forward Tesla receiver traffic as raw TCP. Do not terminate or
> replace vehicle mTLS in nginx, Caddy, a CDN, or a generic HTTP proxy. Never
> copy Tesla account tokens, refresh tokens, OAuth client secrets, or command
> keys onto this host.

Supported source-build targets are macOS 13+ on Apple silicon and current
Debian/Ubuntu on amd64 or arm64. Rust 1.98 and Go 1.27.0 are exact build
requirements.

## Build the two binaries

```bash
git clone https://github.com/magrathean-uk/teslatlas-edge.git
cd teslatlas-edge
cargo build --locked --release
scripts/test-fleet-telemetry-bridge.sh
scripts/build-fleet-telemetry-bridge.sh \
  --target darwin-arm64 \
  --output "$PWD/target/release/teslatlas-fleet-telemetry"
```

For Linux, replace `darwin-arm64` with `linux-amd64` or `linux-arm64`. The
bridge build verifies the pinned source archive, patch digest, Go version,
CGO-disabled build, architecture, reliable-ack behavior, and the privacy patch
that removes raw telemetry from debug dispatch logs. The supplied config routes
`V`, `connectivity`, `alerts`, and `errors`; connectivity remains outside
reliable acknowledgement because upstream forbids that combination.

## Install on Debian or Ubuntu

Create one unprivileged service identity. The sidecar runs under the same Unix
identity because its private receiver bearer must be owner-only; systemd makes
the spool key and Hub credentials inaccessible to the sidecar process.

```bash
sudo useradd --system --home /var/lib/teslatlas-edge --shell /usr/sbin/nologin teslatlas-edge
sudo install -d -o root -g teslatlas-edge -m 0750 /etc/teslatlas-edge
sudo install -d -o teslatlas-edge -g teslatlas-edge -m 0700 /var/lib/teslatlas-edge
sudo install -d -o root -g root -m 0755 /usr/lib/teslatlas-edge
sudo install -o root -g root -m 0755 target/release/teslatlas-edge /usr/bin/teslatlas-edge
sudo install -o root -g root -m 0755 target/release/teslatlas-fleet-telemetry /usr/lib/teslatlas-edge/fleet-telemetry
sudo install -o root -g root -m 0755 scripts/run-with-spool-format-guard.sh /usr/lib/teslatlas-edge/
sudo install -o root -g teslatlas-edge -m 0640 packaging/config.toml.example /etc/teslatlas-edge/config.toml
sudo install -o root -g teslatlas-edge -m 0640 packaging/fleet-telemetry.json.example /etc/teslatlas-edge/fleet-telemetry.json
```

Install three Hub-link TLS files and two vehicle-receiver TLS files:

- `hub-server.crt`: Edge server certificate presented to Hub, mode 0644.
- `hub-server.key`: matching private key, owner `teslatlas-edge`, mode 0600.
- `hub-client-ca.crt`: CA used only to authenticate Hub clients, mode 0644.
- `vehicle-tls.crt`: public Tesla receiver certificate, mode 0644.
- `vehicle-tls.key`: matching Tesla receiver private key, mode 0600.

Use certificates from your chosen private PKI. `hub-client-ca.crt` must be a
dedicated CA for this one Hub installation; do not reuse a broad organizational
client CA. Do not reuse the vehicle key as the Hub listener key. Then initialize
Edge-owned secrets:

```bash
sudo -u teslatlas-edge /usr/bin/teslatlas-edge \
  --config /etc/teslatlas-edge/config.toml init
sudo -u teslatlas-edge /usr/bin/teslatlas-edge \
  --config /etc/teslatlas-edge/config.toml doctor
```

Install and verify the service units:

```bash
sudo install -o root -g root -m 0644 packaging/linux/teslatlas-edge.service /etc/systemd/system/
sudo install -o root -g root -m 0644 packaging/linux/teslatlas-fleet-telemetry.service /etc/systemd/system/
sudo systemd-analyze verify /etc/systemd/system/teslatlas-edge.service /etc/systemd/system/teslatlas-fleet-telemetry.service
sudo systemctl daemon-reload
sudo systemctl enable --now teslatlas-edge.service teslatlas-fleet-telemetry.service
systemctl is-active teslatlas-edge.service teslatlas-fleet-telemetry.service
```

If binding the sidecar directly to port 443, the supplied unit grants only
`CAP_NET_BIND_SERVICE`. A router may instead forward external TCP 443 to a
non-privileged sidecar port. Restrict TCP 8443 to the home Hub address or VPN.

## Install on macOS

> **Development only:** the supplied LaunchAgents share the signed-in user
> identity and do not provide the Linux service's process-to-file isolation.
> Use Linux for a production boundary until separate macOS service identities
> or an equivalent sandbox are implemented and independently verified.

Use `/Users/Shared/TeslatlasEdge` as the mode-0700 state directory and adjust
the copied config paths accordingly. Install binaries and launch agents:

```bash
sudo install -d -m 0755 /usr/local/libexec/teslatlas-edge
sudo install -m 0755 target/release/teslatlas-edge /usr/local/libexec/teslatlas-edge/
sudo install -m 0755 target/release/teslatlas-fleet-telemetry /usr/local/libexec/teslatlas-edge/
sudo install -m 0755 scripts/run-with-spool-format-guard.sh /usr/local/libexec/teslatlas-edge/
sudo install -d -o "$USER" -g staff -m 0700 /Users/Shared/TeslatlasEdge
install -m 0600 packaging/config.toml.example /Users/Shared/TeslatlasEdge/config.toml
install -m 0600 packaging/fleet-telemetry.json.example /Users/Shared/TeslatlasEdge/fleet-telemetry.json
```

Set all paths in both configs to `/Users/Shared/TeslatlasEdge`, install the five
TLS files described above, and run:

```bash
/usr/local/libexec/teslatlas-edge/teslatlas-edge \
  --config /Users/Shared/TeslatlasEdge/config.toml init
/usr/local/libexec/teslatlas-edge/teslatlas-edge \
  --config /Users/Shared/TeslatlasEdge/config.toml doctor
install -m 0600 packaging/macos/uk.co.magrathean.teslatlas-edge.plist ~/Library/LaunchAgents/
install -m 0600 packaging/macos/uk.co.magrathean.teslatlas-fleet-telemetry.plist ~/Library/LaunchAgents/
launchctl bootstrap "gui/$UID" ~/Library/LaunchAgents/uk.co.magrathean.teslatlas-edge.plist
launchctl bootstrap "gui/$UID" ~/Library/LaunchAgents/uk.co.magrathean.teslatlas-fleet-telemetry.plist
```

LaunchAgents cannot bind privileged port 443. Configure the receiver for 8444
and use router/firewall raw TCP forwarding from public 443 to host 8444. The
Edge Hub listener remains 8443. Do not use HTTP reverse proxying.

## Enrol the home Hub

```bash
teslatlas-edge --config /etc/teslatlas-edge/config.toml \
  credential enrol home-hub --ttl-seconds 7776000
```

Transfer the one-time bearer and Hub client identity through a separate secure
channel. Configure Hub to trust `hub-server.crt`, present a client certificate
rooted in the dedicated `hub-client-ca.crt`, store both v1 and v2 record IDs,
and connect outbound to Edge. Current bearer grants cover the complete queue.
See the [delivery contract](../hub-delivery-contract.md).

## Rotate the receiver bearer

The Edge process and Tesla sidecar both load the receiver bearer at startup.
Rotate it with a coordinated bounded restart: stop sidecar, stop Edge, run the
rotation command, start Edge, then start sidecar.

```bash
sudo systemctl stop teslatlas-fleet-telemetry.service teslatlas-edge.service
sudo -u teslatlas-edge teslatlas-edge \
  --config /etc/teslatlas-edge/config.toml receiver-token rotate
sudo systemctl start teslatlas-edge.service teslatlas-fleet-telemetry.service
```

The command prints the new value once for audit/recovery handling. Do not place
that output in shell history, logs, or an ordinary backup.

## Verify the running boundary

```bash
curl --fail http://127.0.0.1:8080/healthz
curl --fail http://127.0.0.1:8080/readyz
curl --fail http://127.0.0.1:8080/metrics
ss -ltnp
```

Only the Tesla receiver and mTLS Hub ports should be public. Receiver admission,
health, readiness, and metrics must appear only on loopback. Health output and
logs must contain no VIN, coordinates, bearer, or payload.

These checks prove local process and listener behavior only. They do not prove
Tesla vehicle delivery, router TCP pass-through, Hub durable commit ordering,
physical host isolation, certificate enrollment, or release readiness.

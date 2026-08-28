# Configure Teslatlas Hub

Hub reads one TOML file, rejects unknown fields, requires absolute paths, and
caps the file at 1 MiB.

## Minimal local configuration

```toml
data_dir = "/var/lib/teslatlas-hub"
bind = "127.0.0.1:8080"

[geocoder]
enabled = false

[terrain]
enabled = false
```

The plaintext listener must remain on loopback. Local plaintext is a transport
choice, not an authentication boundary; every local process is trusted.

## TLS and paired devices

A non-loopback bind requires TLS:

```toml
data_dir = "/var/lib/teslatlas-hub"
bind = "0.0.0.0:8443"

[tls]
certificate_path = "/etc/teslatlas-hub/tls/server.pem"
private_key_path = "/etc/teslatlas-hub/tls/server-key.pem"
public_url = "https://hub.example.net/"
```

TLS-facing sync endpoints require a paired-device bearer. The one-time pairing
claim is the only unauthenticated mutation. Never reuse pairing invitations.

## Collector mode

Legacy mode is the default. It combines adaptive Owner API polling with Tesla
streaming while driving:

```toml
[collector]
provider = "legacy"
interval_seconds = 60
max_backoff_seconds = 900
driving_poll_milliseconds = 2500
stream_health_timeout_seconds = 30
```

Fleet mode uses Fleet API credentials and can use native Fleet Telemetry push:

The example below is for the packaged Debian services. Its command proxy is
fixed to loopback port `4445` by `/etc/teslatlas-hub/command-proxy.env`:

```toml
[collector]
provider = "fleet"
fleet_command_proxy_url = "https://127.0.0.1:4445/"
fleet_command_proxy_root_certificate_path = "/etc/teslatlas-hub/command-proxy-cert.pem"

[collector.fleet_telemetry]
hostname = "telemetry.example.net"
port = 443
ca_certificate_path = "/etc/teslatlas-hub/fleet-telemetry-ca.pem"
ingest_token_path = "/var/lib/teslatlas-hub/fleet-telemetry-bearer"
```

The macOS bundled command proxy uses `https://127.0.0.1:4443/`; keep the
app-generated certificate and data paths rather than copying the Debian paths.

When Fleet Telemetry is configured, resident collection is push-only and does
not fall back to paid periodic `vehicle_data` calls. Follow
[Fleet setup](FLEET_SETUP.md) for certificates, receiver, virtual key, scopes,
and regional registration.

## Geocoding and terrain

Geocoding is off by default:

```toml
[geocoder]
enabled = true
endpoint = "https://geocoder.example.net/"
language = "en"
timeout_seconds = 30
```

There is no public default endpoint. Configure a self-hosted or otherwise
authorised Nominatim-compatible provider. Each uncached lookup sends the exact
vehicle latitude/longitude, zoom 19, requested language, Hub user agent,
operator IP address, and request metadata; the returned address and provider
identifiers are cached locally. Operators must comply with the selected
provider's attribution, database-right, caching, privacy, transfer, and rate
rules. The public OpenStreetMap Foundation Nominatim service is not a default
and its usage policy warns against personal-data submission and systematic or
vehicle-tracking geocoding.

Terrain is disabled by default for source builds and packaged macOS and Debian
installations. Enabling it opts into the provider egress and local cache below;
free-space and cache limits remain mandatory:

```toml
[terrain]
enabled = true
cache_dir = "/var/lib/teslatlas-hub/cache/terrain"
min_free_bytes = 134217728
max_cache_bytes = 536870912
connect_timeout_seconds = 15
read_timeout_seconds = 60
```

On a cache miss, terrain enrichment sends an HTTPS request first to
`https://elevation-tiles-prod.s3.amazonaws.com/` and, if that source is
unusable, to the ESA STEP SRTMGL1 fallback at
`https://step.esa.int/auxdata/dem/SRTMGL1/`. The request path contains the SRTM
tile name derived by flooring latitude and longitude, so either provider learns
the one-degree location tile containing the vehicle position. It does not send
the precise coordinate as a query parameter. The ESA endpoint serves SRTMGL1
elevation data; it is not the separate ESA WorldCover land-cover product.

To prevent all terrain-provider egress and terrain downloads, set:

```toml
[terrain]
enabled = false
```

Cached tiles remain local until removed under the operator's retention policy.

## Validate changes

Stop the service before changing credentials or paths, then run:

```sh
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml doctor
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml preflight
```

These are Debian package commands. On macOS, use the app or the absolute binary
path in [CLI reference](CLI.md). Restart only after both commands pass.

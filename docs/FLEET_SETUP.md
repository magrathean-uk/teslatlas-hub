# Fleet API setup

Fleet API login requires a Tesla Developer application. The application owns
the HTTPS callback URI and client secret; Hub stores only the resulting access
token, single-use refresh token, public client ID, and region.

## 1. Register the Tesla application

1. Sign in at <https://developer.tesla.com/dashboard>, or
   <https://developer.tesla.cn/dashboard> for China, with a Tesla account that
   has a verified email address and multi-factor authentication.
2. Create an application with the `authorization-code` grant.
3. Register an HTTPS origin, callback URI, and return URL on a domain you
   control. The callback must not log or retain its `code` query parameter.
4. Select `Vehicle Information` and `Vehicle Location`.
5. Select `Vehicle Commands` and `Vehicle Charging Management` only when those
   Hub controls are required.
6. Create an EC `prime256v1` (`secp256r1`) key pair. Keep the private key
   private and publish only the public key at
   `/.well-known/appspecific/com.tesla.3p.public-key.pem`.
7. Obtain a partner token and register the application in every Tesla region
   it will serve.

Keep the client secret and private key outside the Hub repository, shell
history, process arguments, logs, and backups.

## 2. Configure and initialize Hub

Create a mode-0600 configuration and a private data directory:

```toml
data_dir = "/absolute/private/path/teslatlas-hub-data"
bind = "127.0.0.1:8080"

[collector]
provider = "fleet"

[geocoder]
enabled = false

[terrain]
enabled = false
```

```sh
chmod 600 /absolute/path/config.toml
mkdir -p /absolute/private/path/teslatlas-hub-data
chmod 700 /absolute/private/path/teslatlas-hub-data
teslatlas-hub --config /absolute/path/config.toml init
```

## 3. Choose the account region

Use one row consistently. Audience values have no trailing slash.

| Hub `region` | Authorization endpoint | Token endpoint | Audience |
| --- | --- | --- | --- |
| `north_america_and_asia_pacific` | `https://auth.tesla.com/oauth2/v3/authorize` | `https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token` | `https://fleet-api.prd.na.vn.cloud.tesla.com` |
| `europe_middle_east_and_africa` | `https://auth.tesla.com/oauth2/v3/authorize` | `https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token` | `https://fleet-api.prd.eu.vn.cloud.tesla.com` |
| `china` | `https://auth.tesla.cn/oauth2/v3/authorize` | `https://auth.tesla.cn/oauth2/v3/token` | `https://fleet-api.prd.cn.vn.cloud.tesla.cn` |

China uses a separate Tesla.cn account and application. Do not select a region
from the vehicle's present location; use the Tesla account region.

## 4. Authorize the Tesla account

Generate a fresh random `state` (`STATE=$(openssl rand -hex 32)`). Open the
regional authorization endpoint with these URL-encoded query parameters:

```text
response_type=code
client_id=APPLICATION_CLIENT_ID
redirect_uri=EXACT_REGISTERED_HTTPS_CALLBACK
scope=openid offline_access vehicle_device_data vehicle_location
state=FRESH_RANDOM_STATE
prompt_missing_scopes=true
require_requested_scopes=true
```

Add `vehicle_cmds` only for general vehicle controls. Add
`vehicle_charging_cmds` only for charging controls. Request both only when both
control groups are required. On callback, reject the response unless its
`state` exactly matches. URL-decode the returned `code` exactly once and treat
it as a secret. The code is short-lived and single-use.

The following `zsh`/`bash` pipeline performs the exchange without putting the
client secret, authorization code, or returned tokens in process arguments or
temporary files. It requires `curl` and `jq`. Set the public values and regional
URLs from the table above, then enter secrets only at the hidden prompts:

```sh
(
set +x
set -o pipefail

trap 'unset AUTH_CODE CLIENT_SECRET client_secret_form code_form' EXIT

CONFIG=/absolute/path/config.toml
CLIENT_ID=APPLICATION_CLIENT_ID
REDIRECT_URI=https://example.com/auth/callback
REGION=europe_middle_east_and_africa
TOKEN_URL=https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token
AUDIENCE=https://fleet-api.prd.eu.vn.cloud.tesla.com

printf 'Authorization code: ' >&2
read -rs AUTH_CODE
printf '\nClient secret: ' >&2
read -rs CLIENT_SECRET
printf '\n' >&2

client_id_form=$(printf %s "$CLIENT_ID" | jq -sRr @uri)
client_secret_form=$(printf %s "$CLIENT_SECRET" | jq -sRr @uri)
code_form=$(printf %s "$AUTH_CODE" | jq -sRr @uri)
audience_form=$(printf %s "$AUDIENCE" | jq -sRr @uri)
redirect_form=$(printf %s "$REDIRECT_URI" | jq -sRr @uri)

{
  printf '%s' 'grant_type=authorization_code'
  printf '&client_id=%s' "$client_id_form"
  printf '&client_secret=%s' "$client_secret_form"
  printf '&code=%s' "$code_form"
  printf '&audience=%s' "$audience_form"
  printf '&redirect_uri=%s' "$redirect_form"
} | curl --disable --silent --show-error --fail-with-body --proto '=https' \
  --header 'Content-Type: application/x-www-form-urlencoded' \
  --data-binary @- "$TOKEN_URL" | \
  jq -ce --arg client_id "$CLIENT_ID" --arg region "$REGION" '
    select(
      (.access_token | type == "string") and
      (.refresh_token | type == "string") and
      (.expires_in | type == "number")
    ) |
    {
      accessToken: .access_token,
      refreshToken: .refresh_token,
      clientId: $client_id,
      region: $region,
      expiresInSeconds: .expires_in
    }
  ' | teslatlas-hub --config "$CONFIG" setup-fleet --all-vehicles
oauth_status=$?

test "$oauth_status" -eq 0
)
```

Use `--vehicle-id TESLA_EID` instead of `--all-vehicles` to select one vehicle.
Hub verifies the selected regional Fleet API by listing the account's vehicles,
then encrypts the credentials in its private store. Only the resident collector
owns refresh-token rotation. Setup and service preflight reject tokens missing
`vehicle_device_data` or `vehicle_location`; `status` exposes only safe scope
booleans and a redacted scope status.

## 5. Enable low-cost native Fleet Telemetry

Tesla Fleet Telemetry sends changed fields directly from the vehicle and avoids
periodic paid `vehicle_data` polling. It requires all of the following:

- a stable public FQDN and direct inbound TCP port 443;
- TLS terminated by the Fleet Telemetry receiver, with vehicle client
  certificates verified there (mTLS);
- the host and complete CA chain supplied in the signed vehicle configuration;
- the application's virtual key paired with every selected vehicle;
- Tesla's command proxy on `127.0.0.1:4443` to sign the configuration;
- Hub on exactly `127.0.0.1:8080`; and
- a private bearer shared only by the receiver bridge and Hub.

Do not put a TLS-terminating CDN, HTTP reverse proxy, or HTTP tunnel in front of
the receiver: it must preserve Tesla's vehicle mTLS connection. A TCP
pass-through load balancer is suitable. Keep Hub and the command proxy on
loopback; expose only the Fleet Telemetry receiver.

On macOS, the signed service package bundles the receiver and supervises it
with Hub in the same per-user LaunchAgent. Hub starts and stops the bundled
loopback command proxy as its direct child using the already configured private
key, certificate/key, and session-cache paths; do not start a second proxy.
Copy the packaged
`fleet-telemetry.json.example` to the private Hub data directory as
`fleet-telemetry.json`, set absolute certificate/key paths and create the
private bearer at the `ingest_token_path` from Hub configuration. The example
uses unprivileged port 8443; route the public Tesla mTLS connection to that port
without terminating TLS. If the receiver JSON is absent, Hub runs alone.

On Debian, build the two pinned sidecars and include both in the package. The
builds require Go 1.27.0 exactly. Replace `amd64` with `arm64` on ARM64:

```sh
mkdir -p dist
scripts/build-tesla-command-proxy.sh \
  --target linux-amd64 \
  --output dist/tesla-http-proxy
scripts/build-fleet-telemetry-bridge.sh \
  --target linux-amd64 \
  --output dist/fleet-telemetry
scripts/build-deb.sh \
  --binary target/release/teslatlas-hub \
  --command-proxy-binary dist/tesla-http-proxy \
  --fleet-telemetry-binary dist/fleet-telemetry \
  --version 1.0.0-alpha.2 \
  --architecture amd64 \
  --output dist/teslatlas-hub_1.0.0-alpha.2_amd64.deb
sudo dpkg -i dist/teslatlas-hub_1.0.0-alpha.2_amd64.deb
```

The package installs both sidecar units disabled. Supply the command-signing
private key, loopback proxy certificate/key, public receiver certificate/key,
and private bearer before enabling them. The example paths match the packaged
units:

```sh
sudo install -d -o teslatlas -g teslatlas -m 0700 /var/lib/teslatlas-hub
sudo -u teslatlas sh -c \
  'umask 077; openssl rand -hex 32 > /var/lib/teslatlas-hub/fleet-telemetry-bearer'

sudo install -o teslatlas -g teslatlas -m 0600 \
  /private/command-auth-private.pem \
  /etc/teslatlas-hub/command-auth-private.pem
sudo install -o teslatlas -g teslatlas -m 0644 \
  /private/command-proxy-cert.pem \
  /etc/teslatlas-hub/command-proxy-cert.pem
sudo install -o teslatlas -g teslatlas -m 0600 \
  /private/command-proxy-tls-key.pem \
  /etc/teslatlas-hub/command-proxy-tls-key.pem
sudo install -o teslatlas -g teslatlas -m 0644 \
  /private/public-receiver-fullchain.pem /etc/teslatlas-hub/tls.crt
sudo install -o teslatlas -g teslatlas -m 0600 \
  /private/public-receiver-key.pem /etc/teslatlas-hub/tls.key
sudo install -o teslatlas -g teslatlas -m 0644 \
  /private/public-receiver-ca.pem \
  /etc/teslatlas-hub/fleet-telemetry-ca.pem
```

The public receiver certificate must match the configured hostname. Validate
the hostname, port, and CA with Tesla's official `tools/check_server_cert.sh`
before configuring a vehicle. The receiver hostname must also be within the
same registered partner domain as the Fleet application; Tesla rejects a
different domain before any vehicle is configured.

Add the command proxy and telemetry destination to Hub configuration. The
bearer path must match `teslatlas-fleet-telemetry.service`:

```toml
bind = "127.0.0.1:8080"

[collector]
provider = "fleet"
fleet_command_proxy_url = "https://127.0.0.1:4445/"
fleet_command_proxy_root_certificate_path = "/etc/teslatlas-hub/command-proxy-cert.pem"

[collector.fleet_telemetry]
hostname = "telemetry.example.com"
port = 443
ca_certificate_path = "/etc/teslatlas-hub/fleet-telemetry-ca.pem"
ingest_token_path = "/var/lib/teslatlas-hub/fleet-telemetry-bearer"
```

Start only the command proxy, then send Hub's fixed signed configuration to
every enabled Hub vehicle while the resident Hub service is stopped. The
configuration command takes Hub's single-process data lock:

```sh
sudo systemctl enable --now teslatlas-command-proxy.service
sudo -u teslatlas teslatlas-hub preflight
sudo -u teslatlas teslatlas-hub configure-fleet-telemetry
sudo systemctl enable --now teslatlas-hub.service
sudo systemctl enable --now teslatlas-fleet-telemetry.service
```

The command prints `vehicles_configured`, `vehicles_skipped`,
`vehicles_revoked`, and `expires_at`.
Each configuration is valid for 30 days. The resident Hub applies the same
configuration at startup and renews it every seven days; failures retry every
six hours without disabling an already-valid push configuration. A nonzero
skipped count means at least one vehicle is not configured; investigate its
virtual-key, firmware, hardware, or configuration-limit state before relying
on telemetry. A vehicle marked disabled is rejected at Hub ingress and its
Tesla configuration is explicitly removed at the next configuration pass.

The fixed low-cost policy contains 47 fields covering drive, charge, climate,
doors/windows, tyre pressure, vehicle state, configuration, and software
updates. Fields are change-driven and have per-field minimum intervals; during
driving, location, speed, and heading are capped at one update every five
seconds. Hub derives drive power from `PackVoltage` and `PackCurrent` only when
their timestamps are within 30 seconds. Fleet Telemetry does not provide the
elevation used by TeslaMate-style positions; elevation remains absent unless
Hub's optional terrain enrichment is enabled and resolves it.

With `[collector.fleet_telemetry]` present, the resident collector makes no
periodic Fleet vehicle-list or `vehicle_data` calls and has no paid polling
fallback. `setup-fleet` still performs bounded account discovery and one
initial snapshot per selected vehicle. Fleet Telemetry is not fully equivalent
to `vehicle_data`, so unavailable fields stay unavailable rather than being
invented.

## 6. Verify push collection without a vehicle command

```sh
sudo systemctl --no-pager --full status \
  teslatlas-hub.service \
  teslatlas-command-proxy.service \
  teslatlas-fleet-telemetry.service
sudo -u teslatlas teslatlas-hub status
sudo -u teslatlas teslatlas-hub doctor
```

`status` reports Fleet Telemetry mode `native_push_configured`, delivery policy
`latest`, and `paidVehicleDataPolling` as `false`. This proves only the selected
Hub runtime path. Its `operationalState` requires separate receiver service
health and a recent durable vehicle receipt before an operator calls the push
path active. Confirm that the latest observation advances after the vehicle
emits a changed field. This check does not wake the vehicle or send a vehicle
command. Never run two Hub processes with the same Fleet refresh token.

## Vehicle commands

Wake uses Fleet API directly. Signed commands require Tesla virtual-key pairing
and a separately configured loopback HTTPS command proxy:

1. Publish the command public key at the well-known path from step 1.
2. Register that domain as a Tesla partner account in each Fleet API region.
3. Have the vehicle owner open `https://tesla.com/_ak/EXAMPLE.COM`, scan the QR
   code, and approve the virtual key in the Tesla app.
4. Run Tesla's official `tesla-http-proxy` on loopback with the matching private
   key and a local TLS certificate.

```toml
[collector]
provider = "fleet"
fleet_command_proxy_url = "https://127.0.0.1:4443/"
fleet_command_proxy_root_certificate_path = "/absolute/path/proxy-ca.pem"
```

Hub never issues a command implicitly. Every action goes through the resident
control socket and requires `--confirm`. The macOS Hub app exposes confirmed
Start Climate and Stop Climate actions when the service is running with exactly
one configured vehicle. It does not expose charging controls.

## Revoke access

Revoke the application from Tesla Account Security, then run Hub sign-out to
remove both Fleet and legacy credential generations locally.

Official references:

- <https://developer.tesla.com/docs/fleet-api/getting-started/what-is-fleet-api>
- <https://developer.tesla.com/docs/fleet-api/getting-started/regions-countries>
- <https://developer.tesla.com/docs/fleet-api/authentication/third-party-tokens>
- <https://developer.tesla.com/docs/fleet-api/authentication/overview>
- <https://developer.tesla.com/docs/fleet-api/fleet-telemetry>
- <https://developer.tesla.com/docs/fleet-api/fleet-telemetry/available-data>
- <https://developer.tesla.com/docs/fleet-api/endpoints/vehicle-endpoints#fleet-telemetry-config-create>
- <https://github.com/teslamotors/fleet-telemetry>
- <https://github.com/teslamotors/vehicle-command>

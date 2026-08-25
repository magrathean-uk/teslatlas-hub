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
owns refresh-token rotation.

## 5. Verify collection without a vehicle command

This bounded observation polls Fleet API but does not issue a wake or command:

```sh
teslatlas-hub --config /absolute/path/config.toml preflight
teslatlas-hub --config /absolute/path/config.toml observe --duration-seconds 30
teslatlas-hub --config /absolute/path/config.toml status
teslatlas-hub --config /absolute/path/config.toml doctor
```

Install or start the service only after these checks pass. Never run two Hub
processes with the same Fleet refresh token.

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
- <https://github.com/teslamotors/vehicle-command>

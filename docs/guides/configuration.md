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

The plaintext listener must remain on loopback. Every request must use one
matching `Host` header. Protected routes also require the private local bearer.
Health, readiness, and discovery routes do not require that bearer. Fleet
Telemetry ingress uses its own bearer check.

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

A paired device's bearer slides 30 days on use but never past an absolute maximum
that counts from the claim, 180 days by default. Set another span, 1 to 3650 days,
with:

```toml
[pairing]
device_max_lifetime_days = 365
```

A change applies to devices paired after the Hub restarts; a device keeps the
maximum it was given. From the maximum on the bearer is refused with 401 and the
device must pair again. Until then, once a device's stored expiry has reached
the maximum, rotating its bearer returns the current bearer unchanged with
`"reason": "max_lifetime"` and nothing is issued. A device can revoke itself
with `POST /v1/device/revoke`; clients call it only when discovery advertises
`auth.revoke`, which the Hub does whenever paired-device authentication is on.

## Browser clients

Cross-origin browser access is disabled by default. List each permitted browser
client origin explicitly when it is hosted separately from Hub:

```toml
[http]
allowed_origins = ["https://client.example.net", "http://localhost:5173"]
```

Origins must use canonical `http://host[:port]` or `https://host[:port]` syntax,
with no trailing slash, path, credentials, query or fragment. Default ports
are omitted. Duplicate values, wildcards and `null` are rejected. There may
be at most 32 origins, each at most 2048 bytes. The configured Hub public
origin also supports same-origin requests without being listed.

CORS permits the following routes for an admitted origin, with preflights for
the listed method. The common request headers are `Authorization`,
`Content-Type`, `If-None-Match` and `Accept`.

| Routes | Method | Additional request headers |
| --- | --- | --- |
| `/healthz`, `/readyz`, `/.well-known/teslatlas-hub`, `/v1/vehicles` | GET | None |
| `/v1/vehicles/{vehicle_id}/current`, `/v1/vehicles/{vehicle_id}/drives` | GET | None |
| `/v1/pairings/{pairing_id}/claim`, `/v1/device/rotate`, `/v1/device/revoke` | POST | None |
| `/v1/vehicles/{vehicle_id}/sync/manifest`, `/v1/vehicles/{vehicle_id}/sync/noop` | GET | `X-Teslatlas-Supported-Schemas`, `X-Teslatlas-Sync-Capability`, `X-Teslatlas-Sync-Profile` |
| `/v1/packs/sha256/{object_name}` | GET | `Range`, `If-Range` |

The registered `sync/signing-keys`, `sync/prepared-artefacts/{artifact_id}` and
`sync/changes-since` routes are outside this browser allowlist. Requests carrying
`Origin`, including same-origin requests and preflights, receive 403 on those
routes once they reach CORS admission. An outer queue timeout can first return
503. Requests without `Origin` retain their ordinary native authentication
path. Browser support for those capabilities has not been selected.

Internal Fleet ingress has no CORS access. Allowed responses vary by origin and
expose `ETag`, `X-Request-ID`, `Accept-Ranges`, `Content-Range`, `Content-Length`,
`Content-Encoding`, `X-Teslatlas-Manifest-Signature` and
`X-Teslatlas-Native-Config-Sha256`.
An allowed-route non-preflight request that reaches the bounded handler/queue timeout
retains CORS metadata on its 503 response. Cookies are not enabled; clients use
dedicated paired-device bearer tokens. An allowed origin does not bypass
authentication or certificate validation. Browsers must trust Hub's TLS
certificate through their normal trust store. Plaintext loopback remains a
development mode with pairing disabled and cannot verify client authentication.

## Collector mode

The collector reads from Tesla in one of three modes. Two settings choose it.

| Mode | Chosen by | What it uses |
|---|---|---|
| Legacy (default) | `provider = "legacy"` and `owner_api_base_url` at one of Tesla's Owner API origins | The Owner API on Tesla's own host and the legacy stream. Requests are not billed. |
| Provider | `provider = "legacy"` and `owner_api_base_url` at any other origin | An Owner-API-compatible service at that URL, with its stream URL. Its own pricing and rules apply. |
| Fleet polling | `provider = "fleet"` | Consented requests to Tesla's Fleet API with your own application. Tesla bills each request. |

The mode follows the origin of `owner_api_base_url`: scheme, host and port. Case, a
trailing dot or slash, the path and an explicit default port (`:443`) do not
matter, so writing out Tesla's default address, with or without a slash, still
means Legacy. Tesla's global and China addresses are both Legacy. A token
endpoint never selects the mode.

### Sign-in endpoint and region

Legacy mode renews a sign-in only at Tesla's two login hosts. The access token's own
issuer picks the host and the region. A token without a readable issuer needs the
owner's statement of the region, `stream_region = "global"` or `"china"` under
`[collector]`; the Hub never assumes one. If both are present and disagree, or the
issuer is not one of Tesla's two, the collector starts but sends nothing and says why
in its status; imports, the read API, sync and backups keep running.

Provider mode has no issuer check. Name the provider's token endpoint with `token_url`:

```toml
[collector.legacy_auth]
enabled = true
token_url = "https://auth.provider.example/oauth2/token"
```

The value must be an `https` URL with a host and no user name, query or fragment.
Legacy mode refuses the key, so it cannot send a Tesla refresh token to another host.

### Sign-in status

The Hub keeps the state of the Tesla sign-in in its database, so `doctor` and `status`
show it from any process, even while the collector runs elsewhere. Under the Legacy
credentials `doctor` lists `authState`, `authReason`, `retryAt`, `collecting` and
`clockSkew`; none of them carries a token, a URL or a response body. `validForCollection`
is false while the state is `signed_out`, `configuration_error` or `api_refused`.

The state shown is the one in force now, not only the one last written. It follows the
stored pair at the time you ask: a sign-in stop applies only while its generation is the
generation of the stored credentials (new credentials lift it in the display at once), a
refusal applies only to the mode it was set in, and a "sign in again" state whose access
token has stopped being usable is shown as signed out, with the same reason, at the
second the token stops being usable. Reading the state never writes anything, never
contacts Tesla and never hands out a token. The loaded configuration, not the account's
recorded mode, decides whether the sign-in applies: Legacy and Provider show it, Fleet
polling ignores a retained Legacy status and does not read Legacy credential material
for it. Only a read that has no configuration at all falls back on the mode the account
recorded.

What `status`, `doctor` and the Mac app report is in two tiers, and `/readyz` carries
only the first.

- **Service readiness** says whether the Hub is ready. A failure is one of six reasons:
  `catalogue_unavailable`, `lifecycle_quarantined`, `published_content_unservable`,
  `collector_absent`, `collector_stale` and `collector_auth_terminal`. They are checked
  in that order: the catalogue and lifecycle, then published content, then, when
  collection is on, the collector's lease (absent, unreadable, expired or terminal), then
  a sign-in that is, or has become, `signed_out`. A signed-out sign-in is the service
  reason `collector_auth_terminal`.
- **Collection attention** is the one condition you can fix while the service stays up.
  It never makes the Hub not ready, is read whether or not the collector holds a lease,
  and is empty while collection is turned off. When several apply the first of this list
  is shown: `collector_cadence_invalid`; `collector_api_refused` or
  `collector_provider_refused`; a sign-in attention (`collector_auth_waiting`,
  `collector_auth_configuration_error`, `collector_auth_sign_in_required`);
  `collector_fleet_refresh_outcome_unknown`; another account stop
  (`collector_fleet_access_refused`, `collector_billing_suspended`,
  `collector_cap_reached` (a cap of zero counts), `collector_config_error`,
  `collector_account_locked`); then `collector_stream_audit_failed`.

`status` adds `collectionAttention`, `readinessDetail` (healthy states worth showing:
`collector_cap_warning`, `collector_rate_limited`, `collector_vehicle_paused`,
`collector_tesla_unreachable` and `collector_gap_unnotified`, which never repeat the
attention), `collectorRunning` and, with the redacted lease timing in `collector`, a
`collectorCadenceError` only while a cadence rule is broken. `collectorRunning` is true
while the collector holds an unexpired lease, including a terminal one; lease times do not
show when a process really stopped. `doctor` reports the same under `collector`
(`collectionAttention`, `readinessDetail`, `running`, `lease`, `cadenceError`) and adds
`attention=<code or none>` to its `collectorReadiness` check and `authState` and
`unresolvedRefresh` to its credential check (a Fleet setup shows `not_applicable` for
both, and never opens a retained Legacy key: that pair shows as present and not valid for
collection). `/readyz` keeps its published body: `{"status":"ready"}` or
`{"status":"not_ready","reason":"<one of the six>"}`, with neither attention nor detail.
`/readyz` answers are cached for a second, but never past the moment a usable sign-in
becomes signed out.

| Attention or reason | Meaning | What ends it |
|---|---|---|
| `collector_auth_waiting` (attention) | Nothing is collected for now: a renewal is being retried after the token expired, a confirmation is paused, an observer has no usable token, or the host clock reads before 2020 | Usually nothing; the Hub retries. For an observer, import or enter a newer token. Fix a wrong clock |
| `collector_auth_configuration_error` (attention) | The Tesla sign-in's own region, issuer or `token_url` setting is refused before anything is sent. This is not the Fleet account's `collector_config_error`, which means Tesla refused the Fleet application's configuration | Correct the setting and restart; no retry command is needed |
| `collector_auth_sign_in_required` (attention) | The renewal token was rejected, or a renewal's outcome is unknown, and the current access token still works: collection continues until it expires | Enter or import new credentials before the token expires; the Hub stays ready meanwhile |
| `collector_auth_terminal` (service reason) | The sign-in must be done again and nothing can be collected | Enter or import new credentials; a restart does not lift it |

The Hub logs a redacted line when a Legacy or Provider sign-in attention begins, changes
or ends, including a sign-in that still collects, so a sign-in that is only about to
expire is not missed. The line carries codes only: no token, generation, URL or response
body.

`authReason` says why the state is what it is. `store_unavailable` is one of the reasons
for a retrying sign-in:

> An `authReason` of `store_unavailable` means the Hub could not reserve a renewal attempt or read or adopt stored credentials. It retries with increasing waits and can continue using its current access token while that token remains usable.

A stopped sign-in lasts until the stored credentials change. After a crash the Hub
also remembers its rejection count and waits at least a minute after any earlier
renewal before it sends another. If the Hub stopped while a renewal was in flight, so
that Tesla's answer is unknown, the sign-in is shown as terminal at the next start and
nothing is sent until new credentials are stored; the collector still starts, and the
service keeps running in every state above except that a signed-out sign-in is not ready.
An unresolved renewal of the Legacy credentials stays a failure for `doctor` and for
starting collection: `doctor` never settles it, and the owner's retry never sends the same
refresh token again.

When Tesla answers a request with 401 the Hub does not count it if the token was
already replaced, has expired or a renewal is under way. A refusal for one car is
checked first with one vehicle-list read: if the list works, only that car's access is
refused and nothing is renewed. A refusal the whole account shares is counted once per
token. The first renews the sign-in at once and sends the request again once; the
second pauses for five minutes, then renews and tries again; the third signs the
account out (`signed_out`, reason `access_token_rejected`). A count is forgotten after
an hour without another, and a successful answer clears it. The Owner API "retry"
control lifts a stop of this kind, because the renewal token is still valid; it cannot
lift a stop whose renewal token was rejected, whose renewal outcome is unknown or whose
answer was unusable, which need new credentials. An observer never renews: a refusal
makes it wait for a newer token, and it looks for one every minute.

Legacy and Provider use the `[collector]` intervals below. Fleet polling uses
`[collector.fleet_polling]`. Every key is optional.

```toml
[collector]
provider = "legacy"
owner_api_base_url = "https://owner-api.teslamotors.com/"
request_timeout_seconds = 15
interval_seconds = 300             # vehicle list when no state check is due; 0 turns collection off
driving_poll_milliseconds = 10000  # driving, no healthy stream
driving_stream_poll_seconds = 60   # driving, stream healthy
charging_poll_seconds = 30
updating_poll_seconds = 120        # software install
online_poll_seconds = 60           # idle window after use
suspended_poll_seconds = 30        # state check while the car is left to sleep
sleeping_poll_seconds = 30         # state check for an asleep or offline car
offline_drive_timeout_seconds = 600
max_backoff_seconds = 300
stream_health_timeout_seconds = 60
```

A request for a car's data is sent only when Tesla's list says the car is online
and the Hub has evidence that it is in use. A parked car is left alone for its
suspension (`suspend_min`) so that it can fall asleep. The Hub never wakes a car.

What each interval is for:

- Driving: spacing between positions is speed times interval. At 10 s that is
  139 m at 50 km/h and 333 m at 120 km/h. While the stream is healthy it delivers
  positions itself, so reads only refresh the groups the stream lacks.
- Charging: at 30 s one interval is 0.09 kWh at 11 kW AC and 1.25 kWh at 150 kW DC.
- State checks (`suspended_poll_seconds`, `sleeping_poll_seconds`): one list request
  covers every car. In REST-only operation the delay before a wake is noticed
  equals this interval.
- `max_backoff_seconds`: the longest wait after repeated failures (exponential with
  jitter, between half and all of the nominal delay).
- `stream_health_timeout_seconds`: how long the stream may stay silent while the car
  is moving before the Hub subscribes again. While the car is not moving, and before
  its first frame, the stream waits a fixed 300 s instead, because a parked car may
  send nothing. A value outside 60 to 300 s is not rejected: it loads moved into that
  range, with one log line at start. The collector's driving cadence does not use this
  key.

Fleet polling uses these keys:

```toml
[collector]
provider = "fleet"

[collector.fleet_polling]
driving_poll_seconds = 30
charging_poll_seconds = 120
updating_poll_seconds = 300
online_poll_seconds = 120
# state_poll_seconds      default 600 while Tesla bills state checks, otherwise 30
# inventory_poll_seconds  default 3600 while Tesla bills lists, otherwise 300
monthly_cap_cents = 1000           # 0 stops all scheduled paid requests
```

Tesla bills every request that gets a status below 500, so Fleet polling has a local
monthly cap on its gross cost, `monthly_cap_cents` (default 1,000 cents, equal to
Tesla's monthly discount). With the defaults a month of ordinary use is about $22 gross,
so collection stops around day 14 of the month. Raise the cap, widen the intervals
or accept the stop. Tesla's own billing limit stays the authoritative cap. Tesla's
published rates are 500 data requests per dollar, 1,000 commands, 50 wakes and 150,000
streamed signals. The Hub treats state checks as billed like data until Tesla's pricing
category for the list and vehicle endpoints has been recorded.

The Hub counts and costs every scheduled request by category (data, state, wake and
command) for each UTC calendar month, in every collector mode, and enforces the cap in
Fleet polling. It keeps the counts, the month's gross and net cost (net is gross less
Tesla's $10 discount, which is never used for the cap), a projection and the date polling
is expected to stop in its own tables, but does not yet show them (see below). A request
is sent only when the cap allows it and a reservation for it has been stored, so a crash can lose at
most a block of 20 requests of count (the next start counts a reserved request as sent and
billed, at most $0.04 of over-estimate per category). When the cap is reached, collection
stops until the next month, until the cap is raised and the Hub restarted, or until you
retry collection. The cap governs scheduled polling only: wakes, commands, setup and
Fleet Telemetry configuration requests are counted and shown but never blocked.

The counts, the month's cost and the expected stop date are computed by the Hub and kept
in its own tables. Status shows only the 80 % warning (`collector_cap_warning`, a healthy
detail) and, once it has happened, the stop (`collector_cap_reached`, attention); the full
display of holds, cost, projection and expected stop date, and a collection object in the
read API, come later.

Restoring a backup also restores its Fleet request counts and reservations. A same-month restore can undercount requests already billed and allow total spending to exceed the intended local cap. See the Restore section of [Backup and recovery](../operations/backup-and-recovery.md) before restarting Fleet polling.

Validation of the cadence. These rules are applied when the collector starts, not when
the configuration loads, so an old file that now breaks one still loads (`status`,
`doctor`, `control` and every other command keep working, except that the Fleet setup
commands still refuse with the cadence message):

- Every interval is greater than zero. `driving_poll_milliseconds` is at least 1000
  and Fleet `driving_poll_seconds` at least 1, because Tesla allows 60 data requests a
  minute per car and the limit is shared with your phone app.
- For the mode in use, the driving, charging and online intervals and
  `max_backoff_seconds` are each below `offline_drive_timeout_seconds`. Otherwise the
  Hub would close every drive between two polls. A `max_backoff_seconds` at or above the
  timeout, whether you wrote it or it is the default, is moved to one second below the
  timeout for this release (see the owner-visible changes below); only a timeout of one
  second leaves no valid value.
- Both charging intervals, `[collector] charging_poll_seconds` and
  `[collector.fleet_polling] charging_poll_seconds`, are below 600 s in every mode,
  whatever `offline_drive_timeout_seconds` is. Energy is integrated only across
  charging intervals of at most 600 s.
- `request_timeout_seconds` is between 1 and 120 (a zero still refuses to load).

When a rule is broken the Hub names the first one, in a fixed order: zero intervals of
the Fleet table, then of the `[collector]` table, the driving minimum, the timeout range,
the charging bound (the `[collector]` key, then the Fleet key), then the intervals that
must stay below the silence tolerance, then the back-off. The message is
`collector setting <full key> = <value> <rule>`, for example:

```text
collector setting collector.request_timeout_seconds = 121 must be between 1 and 120 seconds inclusive
```

Values are in the key's own unit (milliseconds for `driving_poll_milliseconds`, seconds
for the others).

An otherwise eligible Serve (collection on, a car and credentials present) does not
refuse to start for it. It keeps the service up and **waits**: it takes the collector's
lease and renews it, records the wait as a stop in the collection gaps, hands its listener
the key it needs and logs the message once as an error with the code
`collector_cadence_invalid`. It builds no sign-in, client, scheduler or stream, sends no
request and changes no stored refusal, hold, count or owner action; it is not an ordinary
start, so a mode change that a corrected start would act on (lifting a refusal, clearing a
hold) is deferred until that corrected start. If you change the mode while the Hub waits
and then go back to the original mode before a corrected start, the earlier refusal is
still there. `status` and `doctor` show `collector_cadence_invalid` as attention while the
Hub reads the broken file; `/readyz` stays ready while the wait holds its lease, and still
reports a lease that is absent, expired or terminal, or a sign-in that is signed out.
The remedy is to correct the setting **and restart the service**; the retry command does
not end the wait. A `status` or `doctor` run later reads the file as it is now, so it can
show a corrected setting while a Serve that started before the correction still waits on
the one it loaded; the sign-in state is read afresh at every reading in either case.

A bounded `observe` run does not wait: it refuses at once with the same message, before it
starts a listener, and exits with an error. Collection turned off (`interval_seconds = 0`)
or delivered by Edge has no cadence to break and shows nothing of this.

Owner-visible changes in this release:

- `idle_suspend_after_seconds` is still read, so old files load, but it is ignored. The
  per-car `suspend_after_idle_min` governs (default 15 minutes), and a non-default
  value logs `config_key_ignored` once at start.
- `stream_health_timeout_seconds` now defaults to 60 and is moved into the range 60 to
  300 seconds. A value outside it still loads and logs `config_value_clamped` once at
  start.
- `max_backoff_seconds` must be below `offline_drive_timeout_seconds` (default 600).
  The previous release's default (900) and its guide's example set it higher, and a file
  that only lowers the timeout leaves this release's default (300) at or above it, so for
  this release a back-off at or above the timeout, written by you or the default, is
  moved to one second below it and logs `config_value_clamped` once at start, instead of
  making Serve wait without collecting. Remove or lower the key, or raise the timeout, to
  stop the notice. A timeout of one second leaves no valid value and is still the cadence
  error.
- The other limits this release now enforces, which the last release accepted, are
  errors, and a collector start that breaks one waits as described above:
  `driving_poll_milliseconds` below 1000, `request_timeout_seconds` above 120, a
  charging interval of 600 seconds or more, and a driving, charging or online interval
  at or above `offline_drive_timeout_seconds`.
- The collector defaults changed, so an owner who relied on them collects at a different
  density: driving reads without a healthy stream every 10 s (was 2.5 s), charging every
  30 s (was 5 s), the vehicle list every 300 s (was 60 s) when no state check is due,
  state checks of a car left to sleep every 30 s (was 21 min), a software update every
  120 s (was 15 s), the request timeout 15 s (was 20 s) and the longest back-off 300 s
  (was 900 s).
- Fleet polling follows `[collector.fleet_polling]` (driving 30 s, charging 120 s,
  updating 300 s, online 120 s) with a default monthly cap of 1,000 cents. With these
  defaults a month of ordinary use costs about $22 gross, so collection stops around day
  14 of the month, until the next month, a raised cap and a restart, or a retry.
- A Legacy token without a readable issuer now needs `stream_region`, and an
  `owner_api_base_url` that is not Tesla's is Provider mode and needs
  `[collector.legacy_auth] token_url`. Without them the collector sends nothing and shows
  `collector_auth_configuration_error`.
- New commands `collector retry`, `collector resume <vehicle UUID>` and
  `collector gaps acknowledge` (see [When collection stops or a car is held](#when-collection-stops-or-a-car-is-held)
  and the [CLI reference](cli.md)).
- Live cars now start with `suspend_min = 30` and the suspension length is final. Rows
  written by earlier versions that were waiting for a model-based value are given 30
  once. Values set with `control settings` and imported values are never changed.
  Imported cars keep the source database's column defaults for values the source left
  empty.
- `vehicle_data` requests ask for the groups the Hub uses (`charge_state`,
  `climate_state`, `drive_state`, `location_data`, `vehicle_config`, `vehicle_state`).
  Display preferences (`gui_settings`) are no longer requested.

### When collection stops or a car is held

When Tesla or a provider refuses a request that is not a one-off fault, the Hub stops
asking, records the reason and waits for you. `status` and `doctor` report it as the
collection attention code (`collector_api_refused`, `collector_provider_refused`,
`collector_fleet_access_refused`, `collector_billing_suspended`, `collector_cap_reached`,
`collector_config_error` or `collector_account_locked`), and the Mac app shows it in plain
words; the Hub stays ready and `/readyz` does not change. History, imports and the read API
keep working.

| Account state | What caused it | What ends it |
|---|---|---|
| Owner API refused (Legacy) | A 403 that names the Fleet API, a 403 that repeats after one token refresh and one resend, a 410, or every collected car refused | Choose a provider or consented Fleet API polling, or `collector retry` if Tesla's refusal was lifted |
| Provider refused | The provider answered 402, 410 or 403 as above | Resolve it with the provider, then `collector retry`, or change the provider settings |
| Billing suspended (Fleet) | Tesla answered 402 | The Hub asks for the car list every 6 hours and at the start of each month, and resumes on the first answer |
| Fleet access refused | 403 on the car list | Re-consent in Tesla's console, then `collector retry` |
| Configuration error | 412 (partner account not registered) or 421 (wrong region) | Correct the configuration, then `collector retry` |
| Account locked | 423 | Tesla must unlock the account, then `collector retry` |

Changing the mode, the provider URL, the stream URL or the stored credentials also
ends a refusal at the next start.

A Fleet token renewal whose outcome is unknown, because the Hub stopped or lost the answer
while Tesla may have rotated the refresh token, stops Fleet collection the same way until
you reconnect the Fleet credentials; the attention is
`collector_fleet_refresh_outcome_unknown`. It is shown before the cap, billing and Fleet
access stops, after a cadence fault and an Owner or provider refusal. No collection or
token request of any kind is sent while it stands, and the retry command does not send the
fenced refresh token again. Service start, the Mac app's Start and `preflight` accept this
state when the stored pair is otherwise sound: the Hub then runs with its lease, settings
and owner actions as a stopped Fleet collector, over any cap or billing stop it already
holds, which it keeps unchanged. `doctor` does not fail for it when the stored Fleet pair, its
key, its scopes and its claims are sound, but `validForCollection` and `canStart` stay
false until the credentials are reconnected.

A single car can be held without stopping the others:

- Paused: the car's own requests failed at least 12 times in a row over at least an
  hour, or Tesla answered 451 or `mobile_access_disabled`. Reads stop; the stream and
  its frames continue. While the car is listed online the Hub tries one read an hour
  and resumes on the first success. A 403 on one car's request pauses it without a
  probe, because only you can fix that (share the car, add the application's virtual
  key).
- Service: Tesla refused a read as "in service". Open drives and charges end at their
  last evidence. The Hub checks every 15 minutes and resumes when the car leaves
  service.
- Removed: the car was missing from two consecutive car lists. Its history is kept
  and it comes back when a later list names it again.

Three commands act on a running Hub. Each records a request that the collector
applies within a few seconds. A recorded request never contacts Tesla by itself and never
bypasses the collector's own policy (the cap, a refusal that still stands, a fenced
refresh):

```sh
teslatlas-hub collector retry                 # clear refusals, waits and pauses; check the account once
teslatlas-hub collector resume <vehicle UUID> # clear one car's pause
teslatlas-hub collector gaps acknowledge      # mark alert gaps in collection as seen
```

Each prints one JSON line:

```text
{"status":"requested","action":"retry_collection","actionId":<integer>}
{"status":"requested","action":"resume_vehicle","vehicleId":"<UUID>","actionId":<integer>}
{"status":"acknowledged","gaps":<integer>}
```

An unknown car fails with `the vehicle UUID does not identify a configured car` and records
no action. `collector resume` removes a collector pause hold, which the collector placed
because a car's requests kept failing; it is not your own `control pause` and
`control resume`, which turn Owner API and streaming collection off and on for one car
and are not undone by a collector retry.

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
[Fleet setup](fleet-setup.md) for certificates, receiver, virtual key, scopes,
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
identifiers are cached locally. A cached answer is reused only for the same
endpoint, the same language and the same coordinates (rounded to six decimal
places), so changing `endpoint` or `language` queries the provider again.
Operators must comply with the selected provider's attribution, database-right,
caching, privacy, transfer, and rate rules. The public OpenStreetMap Foundation
Nominatim service is not a default and its usage policy warns against
personal-data submission and systematic or vehicle-tracking geocoding.

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

The cache directory must be an absolute path. The Hub creates it, and any
missing parent directories below an existing one, with mode `0700`. It uses the
directory only if it is not a symbolic link and is owned by the user that runs
the Hub; an existing directory with a wider mode is tightened to `0700`, and on
macOS any extended access-control list on it (including entries inherited from
a parent) is removed. Every existing parent directory must be owned by that user or by root, and may be
writable by its group or by others only when that user owns it (or root owns it
with the sticky bit, as `/tmp` is). When the directory fails these checks,
terrain enrichment stays off for that run and the Hub logs `untrusted_root`;
collection and everything else continue. Move a root-owned or shared cache to a
directory the Hub's user owns, or delete it and let the Hub create it. The
directory's filesystem must also support advisory file locks (`flock`), which the
cache uses to tell a download in progress from a leftover; on one that does not
(some network mounts) terrain enrichment stays off and the Hub logs
`locking_unsupported`. Choose a local directory for `terrain.cache_dir`.

A cached tile is used only when its SHA-256 equals the digest the Hub recorded
in its catalogue when it downloaded that tile. A tile with no record or a
different digest is ignored, deleted and fetched again, so tiles cached by an
earlier version, or copied in by hand, are downloaded once more the next time
they are needed.

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
path in [CLI reference](cli.md). Restart only after both commands pass.

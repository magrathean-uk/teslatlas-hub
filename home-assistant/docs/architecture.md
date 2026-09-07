# Home Assistant integration architecture

## Responsibility

The integration maps the frozen `hub-http-v1@1.0.0` current-state surface to
Home Assistant vehicle devices and read-only sensors. It does not read Hub
storage or hold Tesla account credentials.

## Data path

`current_hub_client.py` uses Home Assistant's aiohttp session helpers and shared
verified connector. Every refresh first fetches public discovery without
authorization and verifies the stored Hub identity. Only then does it send the
paired device bearer to the vehicle list and per-vehicle current routes. The
coordinator is the single polling authority: it polls every 30 seconds, never
overlaps refreshes, backs off boundedly after transient errors, and permits at
most four current reads in flight. A failed group of reads is cancelled and
drained before the refresh returns; unload closes dispatch before cancelling
and draining any request still in progress.

The current profile has no public event stream. The integration creates no SSE
task and sends no epoch or `Last-Event-ID` traffic.

## TLS and credentials

TLS always uses normal certificate and hostname validation. A private pairing
invitation can add the Hub leaf-certificate SHA-256 pin. A pin-aware aiohttp
request class checks the certificate on the exact pooled or newly established
HTTP connection before aiohttp writes headers or the invitation body. HTTP
responses reject redirects, have a ten-second total timeout, are read through
EOF in bounded chunks, and are limited to one MiB.

The setup and reauthentication flows consume pairing UUID, secret, and device
name. Config entries retain Hub identity, device identity, bearer expiry, and
the bearer. They do not retain the pairing UUID, secret, URI, or device name.

## Entities and absence

Vehicle identifiers and matching sensor meanings keep stable unique IDs.
Vehicles removed from a later list remain registered but unavailable; new
vehicles receive one sensor set. A missing field is unknown while numeric zero
remains zero and a null lock state stays unknown. Telemetry age comes only from
`observed_at_ms`; missing time is unknown, and observations more than five
minutes in the future fail validation.

Collector health, Fleet cost, backup age, data quality, charges, commands, and
remote device management have no public current-Hub route. Their fixture-era
entities are retired rather than populated from guessed values.

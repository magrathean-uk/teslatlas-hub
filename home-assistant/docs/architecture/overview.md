# Integration architecture

## Responsibility

The integration maps `hub-http-v1@1.0.0` to vehicle devices and read-only sensors. It uses the public Hub API and holds a scoped device credential. It does not read Hub storage or hold Tesla account credentials.

## Requests and refresh

[client.py](../../custom_components/teslatlas_hub/client.py) creates the Home Assistant-managed HTTP session. [current_hub_client.py](../../custom_components/teslatlas_hub/current_hub_client.py) validates public discovery and the saved Hub identity before sending a saved bearer to vehicle or current-state routes. Rotation also probes the Hub identity before sending the credential.

[coordinator.py](../../custom_components/teslatlas_hub/coordinator.py) owns the 30-second refresh schedule. Refreshes do not overlap; current-state reads are bounded to four at a time. Transient retry delays are 30, 60, 120 and 300 seconds. Closing the client prevents new dispatch, cancels outstanding work and drains it. There is no SSE task or event replay traffic.

Requests reject redirects, use a ten-second total timeout and bound responses to one MiB while reading through EOF. The checked profile and [compatibility record](../../compatibility/hub.json) constrain protocol use.

## TLS and credentials

Keep HTTPS enabled. When TLS is selected, normal certificate and hostname validation remain active. An optional SHA-256 leaf-certificate pin is checked on the actual HTTP connection before headers or invitation content are written. A pin does not replace CA trust. The UI still exposes a TLS choice, so this is not a claim that all plain HTTP configuration is rejected by the code.

Setup consumes pairing ID, secret and device name transiently. The config entry retains the endpoint, Hub and device identities, credential and expiry, but not invitation material. Candidate credentials are validated by an authenticated read before setup or reauthentication completes. The coordinator rotates credentials near expiry and stores the replacement before switching the active client to it.

## Entity lifecycle and diagnostics

Vehicle IDs and sensor meanings form stable entity identities. New vehicles receive sensors; removed vehicles remain registered but unavailable. A missing current projection keeps the vehicle's values unknown. Numeric zero is preserved. Telemetry age uses `observed_at_ms`; a timestamp more than five minutes in the future is rejected.

The sensor platform exposes only fields bound by the current profile. Legacy fixture models do not establish support for collector health, Fleet cost, backup age or data quality.

[diagnostics.py](../../custom_components/teslatlas_hub/diagnostics.py) redacts credentials, pairing material, endpoint and identity information, coordinates and raw observations. The Home Assistant configuration remains sensitive even when exported diagnostics are redacted.

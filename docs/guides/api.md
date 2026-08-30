# HTTP and sync API

The v1.0.0-beta.1 API is designed for the Teslatlas client and supervised
companions. It is not a general remote-control API. Protocol and response
formats may change during beta.

## Transport and authentication

- Plaintext HTTP is accepted only on loopback.
- A non-loopback bind requires TLS.
- TLS-facing mirror routes require a paired-device bearer.
- `POST /v1/pairings/{pairing_id}/claim` is the sole unauthenticated mutation
  and accepts only a live, single-use invitation.
- Internal Fleet Telemetry ingestion uses a separate private bearer and must
  remain on loopback behind the supervised receiver.
- Ordinary request bodies are capped at 4 KiB. Concurrency and handler time are
  bounded. Pack response streams use separate bounded file slots.

## Routes

| Method | Route | Purpose |
|---|---|---|
| `GET` | `/healthz` | Process and store health. |
| `GET` | `/readyz` | Collector and serving readiness. |
| `GET` | `/.well-known/teslatlas-hub` | Version, protocol, capabilities, and immutable source identity. |
| `POST` | `/v1/pairings/{pairing_id}/claim` | Claim one pairing invitation. |
| `POST` | `/v1/device/rotate` | Rotate the current device bearer. |
| `GET` | `/v1/vehicles` | List vehicles visible to the paired device. |
| `GET` | `/v1/vehicles/{vehicle_id}/current` | Get bounded current state. |
| `GET` | `/v1/vehicles/{vehicle_id}/sync/manifest` | Get a signed sync manifest. |
| `GET` | `/v1/vehicles/{vehicle_id}/sync/noop` | Schema 2.2 no-op synchronization response. |
| `GET` | `/v1/packs/sha256/{object_name}` | Stream one manifest-authorized immutable pack. |
| `POST` | `/v1/internal/fleet-telemetry` | Private supervised Fleet Telemetry ingestion. |

Pack objects are content-addressed and served only when an authorized manifest
references the digest. Unknown, retired, orphaned, and unauthorized digests are
rejected.

## Pair a device

Create a short-lived invitation on the Hub host:

```sh
sudo -u teslatlas -- /usr/bin/teslatlas-hub \
  --config /etc/teslatlas-hub/config.toml pair \
  --label "Teslatlas iPhone" --expires-in-seconds 900
```

The displayed QR contains secret-bearing claim material. Do not screenshot,
log, email, or reuse it. Revoke a paired device from the local CLI if a client
is lost.

## Errors and request IDs

Responses include `x-request-id` when the request reaches the router. Preserve
that identifier in a redacted bug report. Never attach bearers, pairing
payloads, precise locations, or pack contents to a public issue.

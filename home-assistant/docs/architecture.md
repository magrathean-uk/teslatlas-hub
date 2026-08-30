# Home Assistant integration architecture

## Responsibility

Map released public Teslatlas Hub data and events to Home Assistant devices and entities. The integration is a read-only, local-push hub client.

## Components

- `client.py`: typed integration-facing boundary. The production placeholder raises `ProtocolContractUnavailable`; it encodes no route or payload.
- `models.py`: immutable Home Assistant-side snapshot and event model.
- `config_flow.py`: manual setup, Zeroconf, transient pairing, reauthentication, and same-identity endpoint reconfiguration.
- `coordinator.py`: one initial snapshot, replayable push events, availability, bounded reconnect, and client lifecycle.
- `sensor.py`: Hub and per-vehicle read-only sensors with stable registry identities.
- `diagnostics.py`: redacted aggregate support data only.

## Data path

```text
released public Hub adapter
        |
        | one bounded initial snapshot
        v
TeslatlasDataCoordinator ----> Hub and vehicle sensors
        ^
        | SSE events + Last-Event-ID replay
        |
released public Hub adapter
```

No polling interval exists. Stream loss marks coordinator entities unavailable and reconnects after 1, 2, 4, 8, 16, then 30 seconds. The delay remains capped at 30 seconds. The first replayed event restores availability. Device-bearer failure stops reconnect and starts Home Assistant reauthentication.

## Identity and discovery

The manifest advertises `_teslatlas-hub._tcp.local.`. Home Assistant supplies the discovered address and port. The flow ignores every TXT property because no TXT contract is frozen, and asks the user to confirm TLS before probing.

A client probe must return stable public Hub identity. Duplicate discovery updates an existing endpoint. Reconfiguration accepts endpoint roaming only when the replacement resolves to the same identity.

## Entities and availability

One Hub device owns Hub-health sensors. Each public vehicle identifier owns a separate vehicle device and stable sensor set. New vehicles observed through push receive one entity set without duplicates.

If the Hub stream is unavailable, its entities are unavailable. If the Hub is available but an individual field is absent, that entity is unknown. No stale missing value is reported as current.

## Privacy

Config-entry diagnostics redact host, port, bearer, Hub identity, pairing secret, latitude, and longitude. Runtime diagnostics return only protocol version, capability names, aggregate data-quality counts, vehicle count, connection state, collector health, timestamp, and whether a replay cursor exists. They omit vehicle IDs/names, raw states, event IDs, coordinates, and provider payloads.

## Boundaries

The integration does not read Hub storage, embed collector logic, require Tesla credentials, call Tesla, or expose commands. Production transport stays disabled until the public protocol gate in [protocol readiness](protocol-readiness.md) closes.

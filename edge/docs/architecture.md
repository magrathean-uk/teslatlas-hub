# Edge architecture

## Responsibility

Receive Fleet Telemetry on a user-operated host and deliver authenticated encrypted batches to the user's Hub.

## Data path

```text
Tesla vehicle -> mTLS -> Teslatlas edge -> authenticated encrypted batches -> home Hub
```

## Security and reliability constraints

- Edge has no Tesla account access, refresh token, or vehicle-command capability.
- Edge-to-Hub uses mutually authenticated TLS and a Hub-scoped credential.
- Spooling is encrypted, bounded, retention-limited, and deleted after Hub acknowledgement.
- Delivery is at-least-once; Hub owns deterministic deduplication.
- Home Hub prefers outbound connectivity to Edge.
- Health and queue metrics expose no VIN, secret, or raw telemetry payload label.

## Boundaries

Edge is a narrow receiver/delivery component. It does not host the consumer API, project drives or charges, make vehicle commands, or become a mandatory central relay.

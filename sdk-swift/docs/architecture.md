# Swift SDK architecture

## Responsibility

Provide a stable, idiomatic Swift boundary over released Teslatlas protocol contracts. It must not expose undocumented Hub routes or depend on proprietary Teslatlas application code.

## Package boundaries

| Unit | Responsibility |
| --- | --- |
| Core | Public value types, capability checks, error taxonomy |
| Discovery | Hub identity and endpoint selection |
| Pairing | QR claim, resumable lifecycle, bearer rotation |
| Query | Paginated resources, ETags, and incremental cursors |
| Events | SSE reconnect and Last-Event-ID handling |
| Sync | Signed manifest and resumable immutable-pack transport |

## Required client behaviour

- Pin one Hub identity across LAN, VPN, and public endpoints.
- Fail closed on certificate or identity changes with actionable errors.
- Resume partial transfers and pairing safely after process termination.
- Keep all errors typed and preserve server request IDs.
- Keep Teslatlas read-only even when the protocol exposes command jobs.

## Boundaries

The SDK consumes released `teslatlas-protocol` artifacts only. Local persistence, maps, analytics, product UI, and private compatibility work do not belong here.

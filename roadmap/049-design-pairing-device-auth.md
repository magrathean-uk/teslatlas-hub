---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design pairing and device authorization

Blocked by: [Design delta synchronization](048-design-delta-sync.md).

## Question

How are one-use pairing, TLS identity, device credentials, revocation,
rotation, expiry, permissions, and multiple phones handled?

## Starting recommendation

Keep short-lived one-use invitations, pin the Hub identity, issue independent
revocable device credentials, and expose no Tesla credential to a client.

## Resolution

One-use, digest-only invitations and pinned TLS establish separate bearer
credentials for each phone. The public scope is mirror read only. Device
revocation and replacement are owner actions; TLS identity changes require
fresh pairing. No Tesla credential reaches a phone.

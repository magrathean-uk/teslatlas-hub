---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design credential handoff

Blocked by: [Design destination reconciliation](059-design-destination-reconciliation.md).

## Question

How can the migration hand off legacy owner tokens or Fleet credentials without
printing, persisting plaintext, invalidating recovery, or creating dual refresh?

## Starting recommendation

Use protected local channels and encrypted atomic storage, verify credential
identity without disclosure, and keep rollback custody explicit. Read legacy or
Fleet material only from the discovered TeslaMate source and write only to Hub's
credential store; never rotate, revoke, rewrite, or delete the TeslaMate copy.

## Resolution

Handoff is optional and inactive by default. A protected in-memory transfer
creates an atomically encrypted Hub candidate, verifies non-secret identity,
and leaves TeslaMate unchanged. Legacy and Fleet modes stay separate; no dual
refresh or automated cutover occurs.

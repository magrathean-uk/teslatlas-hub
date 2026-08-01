---
type: wayfinder:task
status: closed
parent: 000-map
---
# Design credential lifecycle

Blocked by: [Define Tesla credential modes](025-define-credential-modes.md).

## Question

How are credentials acquired, handed off, encrypted, rotated, revoked,
recovered, audited, and proven absent from logs and process state?

## Starting recommendation

Keep plaintext only in protected process memory, use atomic encrypted
replacement, and test crash points around every credential transition.

## Resolution

Existing owner-token and TeslaMate-password provision writes host-encrypted,
private candidate files and atomically replaces ciphertext before a Hub unit
can consume it. Plaintext is accepted only from protected input, read through
the systemd credential directory for the specific operation, and excluded from
configuration, argv, environment, SQLite, packs, diagnostics, logs, and
backups.

Credential replacement validates a complete candidate before atomic handoff;
failure keeps the previous credential active. Revocation removes the
Hub-owned ciphertext and drop-in, stops only its Hub authority, and leaves a
non-secret audit record. Missing, invalid, expired, or revoked credentials
fail closed and require explicit reprovisioning. Fleet rotation will use the
same candidate-before-replace lifecycle, including crash-point tests, before
Fleet is enabled.

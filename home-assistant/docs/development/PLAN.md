# Home Assistant — accepted retained guest integration

Revision 2026-09-21. **COMPLETED** for MF-0/MF-2/MF-3.

## Accepted result

Exact source `78186795dfe2575258d4e3c2b138f263f8df21b5` is published on `main`.
Its exact 31-file component is mounted read-only in the existing Debian arm64 Home
Assistant 2026.8.3 guest. The preserved entry validates strict TLS, the direct
diagnostic returned one vehicle, and all 13 Teslatlas entities are loaded with zero
unavailable. Recovery copies cover both the replaced certificates and prior guest
configuration.

## Boundary

This accepts the retained local guest route only. HACS/store distribution,
production credentials, real Tesla access and broader platform work remain
deferred or separately gated.

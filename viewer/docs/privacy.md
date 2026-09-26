# Viewer privacy

## Fixture mode

Fixture mode uses synthetic data and in-memory discovery, pairing, reads, and device removal. It needs no Tesla account, provider token, Hub database, real vehicle identifier, or location history. Its six-digit invitation is public example data. Fixture mode makes no Hub API requests.

The frontend contains no analytics, advertising, remote fonts, embedded map, or remote error-reporting integration. This describes the checked-in client, not the behavior of a separate host, proxy, browser extension, or Hub.

## Live mode

The [SDK adapter](../src/data/sdk-data-source.ts) sends pairing material through the public SDK to the configured HTTPS Hub. Endpoint, expected Hub UUID, invitation TLS identity, and discovery manifest key have distinct roles. Confirm the supplied identity and use normal browser certificate validation.

The Viewer holds its credential and cached data in the live data-source instance. It does not persist credentials in local storage, session storage, IndexedDB, cookies, or a service worker. Reloading requires pairing again. This does not control the Hub's own storage or revocation policy.

Clearing the local session cancels active reads and clears viewer-held credentials and identity-bound caches. An authenticated `401` also clears the session. Local clearing does not revoke the paired device on the Hub; remote paired-device management is unsupported by the current profile.

## Reports and evidence

Use synthetic records for screenshots and reproductions. Never put invitations, authorization values, raw cursors, precise locations, or private configuration in URLs, logs, public issues, copied errors, or support attachments. Inspect and redact any material before sharing it.

The fixture screenshot workflow uses synthetic examples. Live test configuration disables automatic traces, screenshots, and video, but that is not a guarantee that every external tool or operator log is safe. Keep private test descriptors and raw evidence outside public documentation.

The optional static server and container do not provide a persistent Viewer account or database. Their liveness check is not a check of Hub privacy or security. See [SECURITY.md](../SECURITY.md) for reporting guidance.

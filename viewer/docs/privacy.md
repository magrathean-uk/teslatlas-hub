# Viewer privacy

## Current fixture mode

- No Tesla account, Hub database, provider token, real vehicle identifier, or
  precise location is required.
- Fixture identifiers and names are synthetic and redacted.
- Discovery, pairing, reads, and paired-device removal happen in memory.
- A visible clear-session action drops viewer-held pairing state and returns to
  discovery; it does not revoke a Hub device.
- The app does not write a credential to local storage, session storage,
  IndexedDB, a cookie, or a service worker.
- Fixture mode makes no Hub API request.
- The repository contains no analytics, advertising, telemetry, or hosted Hub
  deployment. Its optional Docker/Compose files serve only static Viewer
  assets and do not add persistence.
- The client loads no external font, tracker, embedded map, remote error
  reporter, or third-party analytics script.

The six-digit fixture invitation is public example data. It grants no access to
anything.

## Live mode

Live mode uses the packaged public SDK browser entrypoint. Pairing material is
submitted only to the configured HTTPS Hub. The SDK returns a caller-owned
credential which this viewer holds only in the live data-source instance; it
does not persist the invitation or credential in browser storage. Reloading the
page therefore requires pairing again.

The endpoint, expected Hub UUID, invitation TLS identity, and discovery
manifest key remain separate values. Clearing the local session cancels active
reads and clears credentials and identity-bound cached data. It does not send a
remote device-revocation request. An authenticated `401` also clears the local
session and returns the viewer to pairing.

## Logs and screenshots

Automated screenshots show only deterministic fixture data. Live acceptance
uses synthetic Hub records and private owner-only invitations and receipts.
Tests do not record authorization values or invitation secrets. Pairing
material must never enter a URL, browser history, console log, copied error,
support bundle, or screenshot.

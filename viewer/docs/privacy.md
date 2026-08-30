# Viewer privacy

## Current fixture mode

- No Tesla account, Hub database, provider token, real vehicle identifier, or
  precise location is required.
- Fixture identifiers and names are synthetic and redacted.
- Discovery, pairing, reads, and paired-device removal happen in memory.
- The app does not write a credential to local storage, session storage,
  IndexedDB, a cookie, or a service worker.
- Fixture mode makes no Hub API request.
- The repository contains no analytics, advertising, telemetry, or hosted
  deployment configuration.

The six-digit fixture invitation is public example data. It grants no access to
anything.

## Live mode

Live mode is disabled while the public protocol and TypeScript SDK remain
foundation-only. The viewer returns a local `SDK_NOT_RELEASED` error and makes
no speculative request.

Before live mode can be enabled, the released SDK must define browser-safe,
caller-owned credential interfaces, safe error diagnostics, Hub identity
validation, and paired-device scopes. This repository must then document the
chosen browser credential policy and its threat model.

## Logs and screenshots

Automated screenshots show only deterministic fixture data. Tests do not log a
secret, real identifier, or precise coordinate.

# macOS Firefox browser acceptance without Keychain access

The MAC-3 browser lane uses a fresh disposable Firefox profile and an isolated SQL NSS database.
It never reads or changes the macOS Keychain, enables enterprise roots, imports certificates through
the Firefox UI, weakens HTTPS verification, or supplies certificate-error bypass flags.

## Preconditions

- `/opt/homebrew/opt/nss/bin/certutil` is the approved NSS 3.129 executable.
- The Hub CA, descriptor, provisioned credential, and output parent are current-user-owned regular
  files or directories with no group or world permission bits.
- The Hub permits the exact `http://127.0.0.1:4174` CORS origin.
- The packed SDK is already extracted with its existing dependencies available. This lane does not
  install or build packages.

The helper creates a mode-0700 profile beneath the current user's private temporary directory, runs:

```text
certutil -N -d sql:<profile> --empty-password
certutil -A -d sql:<profile> -n "Teslatlas Hub disposable CA" -t "C,," -i <hub-ca>
certutil -L -d sql:<profile> -n "Teslatlas Hub disposable CA"
```

It requires owner-only `cert9.db`, `key4.db`, and `pkcs11.txt`, then calls Playwright Firefox
`launchPersistentContext` with that exact profile, `ignoreHTTPSErrors: false`, and
`security.enterprise_roots.enabled: false`. The context and exact disposable profile are removed on
success or failure.

## Future authorized invocation

```sh
TESLATLAS_HUB_HTTP_CONFIG=/private/runtime/mac3-sdk-descriptor.json \
TESLATLAS_HUB_CREDENTIAL_FILE=/private/runtime/mac3-sdk-credential.json \
TESLATLAS_HUB_SDK_PACKAGE_ROOT=/private/runtime/node_modules/@teslatlas/sdk \
TESLATLAS_HUB_BROWSER_ORIGIN=http://127.0.0.1:4174 \
TESLATLAS_HUB_BROWSER_RECEIPT=/private/runtime/mac3-sdk-firefox-receipt.json \
/Users/bolyki/dev/teslatlas-lab/tooling/node-v26.7.0-darwin-arm64/bin/node \
  scripts/test-hub-browser-direct.mjs
```

The provisioned credential remains caller-owned and is never emitted in the receipt. Pairing is a
separate synthetic fail-closed preflight: without a caller pin-capable transport it must return
`hub_tls_pin_unavailable` with zero network requests. The subsequent CORS reads use the browser's
literal default Fetch implementation.

Playwright Firefox intentionally suppresses ordinary browser-generated `OPTIONS` requests before
both page-level and context-level request events. The receipt therefore does not invent a preflight
count. It records the exact page origin, successful browser-enforced default-Fetch results, observed
cross-origin Hub application requests, and
`preflightVisibility: not-exposed-by-playwright-firefox-request-events`.

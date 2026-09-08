# Browser support

## Proven today

The fixture suite runs against the Chromium revision bundled with the locked
Playwright package on macOS. It verifies:

- discovery and fixture pairing;
- all seven reference views;
- complete, empty, stale, inferred, degraded, offline, loading, and error
  states;
- paired-device confirmation, removal, and clear-session flow;
- no fixture API requests;
- axe accessibility scans;
- keyboard entry and focus order;
- reduced-motion preference;
- 320px reflow, equivalent to a 1280px layout at 400% zoom;
- deterministic desktop and mobile screenshots.

The recorded built-Viewer acceptance lane used native ARM64 Chromium
`152.0.7977.75` on Debian 13.6 with a private NSS database and normal CA
validation against a synthetic Hub. That historical receipt covered wrong-Hub
rejection, pairing, two bounded pages, `304` replay, outage recovery,
authenticated-session loss, local logout, and unsupported resources. The
current Viewer requests up to 25 rows per user-driven page; a fresh run is
required before claiming current paging or installed-Hub compatibility. A
second browser without the fixture CA failed with
`ERR_CERT_AUTHORITY_INVALID` in that lane.

## Design target, not yet a compatibility claim

The code uses standard evergreen-browser features: ES modules, `AbortController`,
`structuredClone`, CSS Grid, and Flexbox. Current Safari, Firefox, Chrome, and
Edge are design targets, but only Chromium has executable evidence today.

Add a browser to the support claim only after the same Playwright suite passes
there from a clean checkout. The Chromium automation is not a Safari or WebKit
claim.

## Viewports

Committed screenshots cover 1280×800 desktop and 375×812 mobile. Automated
reflow also checks 320×800. Layouts use content-driven breakpoints rather than
device names.

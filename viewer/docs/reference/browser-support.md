# Browser evidence

Chromium is the only browser named in the repository's automated configuration and retained installed-browser receipts. Those receipts are historical and apply to their exact source, package, browser, and synthetic Hub setup. They do not establish current cross-browser support.

## What the source covers

The fixture specs cover the seven views, state labels, connection and local-session interactions, keyboard behavior, reduced motion, and narrow layouts. Screenshot specs use 1280 by 800 desktop and 375 by 812 mobile viewports; the existing accessibility spec includes 320 by 800 reflow checks.

The [verification guide](verification.md) separates fixture checks from installed Hub runs and explains why the unqualified browser test command should not be used as a fixture smoke check.

## Limits

Safari, Firefox, Edge, and WebKit do not have equivalent accepted evidence in this checkout. Modern browser APIs in the source are implementation choices, not proof of compatibility. Expand a browser claim only after recording the relevant ordinary-user route on that browser, including live TLS, pairing, current state, history, recovery, and local session clearing where applicable.

Existing tests do not lift the owner's development deferral or authorize accessibility work. See [the development note](../development/PLAN.md).

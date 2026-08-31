# Browser support

## Proven today

Automated browser evidence runs against the Chromium revision bundled with the
locked Playwright package on macOS. It verifies:

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

## Design target, not yet a compatibility claim

The code uses standard evergreen-browser features: ES modules, `AbortController`,
`structuredClone`, CSS Grid, and Flexbox. Current Safari, Firefox, Chrome, and
Edge are design targets, but only Chromium has executable evidence in this
repository today.

Add a browser to the support claim only after the same Playwright suite passes
there from a clean checkout. Do not infer Safari support from a Chromium pass.

## Viewports

Committed screenshots cover 1280×800 desktop and 375×812 mobile. Automated
reflow also checks 320×800. Layouts use content-driven breakpoints rather than
device names.

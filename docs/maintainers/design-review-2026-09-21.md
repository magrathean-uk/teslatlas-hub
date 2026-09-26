# Hub macOS design polish — 21 September 2026

## Scope and direction

Implemented in `codex/hub-macos-sidebar-redesign`, preserving the existing branch work. Native AppKit only. No separate Teslatlas app source, backend changes, commits, pushes, signing, or deployment.

Approved reference: `/Users/bolyki/dev/teslatlas-lab/build/hub/target/design-review-20260921/approved-direction.png`. The full brief and visually captured Teslatlas references are in that directory. The reference relationships are preserved: health-first overview, unboxed row symbols, aligned grouped content, four toolbar destinations, embedded utilities, and a focused full-window onboarding flow. Removed repeated helper copy where it added no information.

## Implemented

- Shared 22-point headings, 14-point body/emphasis, 13-point actions, and 12-point captions. Shared 52-point information rows with 16-point horizontal insets, 24-point icon bounds and 18-point SF Symbols.
- Standard actions are 32 points high, minimum 96 points wide; navigation remains a separate 36-point role. Onboarding primary actions stay 240 by 32 points, with a stable 640-point content column. Text is measured using the native text cell so symbol/title content stays inside its button.
- Status dots sit beside their values. Diagnostics and Service Details use the same content alignment and row padding as other pages. Settings no longer repeats two links to the same service page.
- Activity search filters events immediately and has a recovery message for no matches. Removed its duplicate event-detail card and inert category filter.
- Diagnostics, Logs and Service Details stay inside the main window from page buttons, native menus and shortcuts. Tabs and Back cannot dismiss a pending service operation.
- Configured stopped/degraded Hubs no longer expose the main import affordance. Eligibility uses the existing unconfigured account/database state; it does not infer history from database size. This is a UI eligibility correction, not a new persisted import contract.
- Onboarding is a full-window flow, with Back/progress above, a stable primary action below, aligned selection cards and form fields. Its Logs destination is embedded too. A busy page owns one progress indicator.
- Hover and press fills animate over 140 ms; click feedback uses 120 ms. Page/state changes use 160 ms fades, forward/back movement and disclosure layout use 220 ms. Rapid color changes start from the presented value. Reduce Motion replaces directional transitions with a short fade and removes animated layout travel. Healthy idle screens do not loop decorative animation.

## Evidence

All evidence is local under `/Users/bolyki/dev/teslatlas-lab/build/hub/target/design-polish-20260921`.

- `tests-r5.xcresult`: full native suite, **253 passed, zero failed/skipped**, current application source.
- `tests-r6.xcresult`: three visual tests passed after correcting the snapshot harness to finish AppKit window layout before capturing resized windows. No application source changed between r5 and r6.
- `renders-r6/`: 34 native captures: 18 catalog scenes, nine state/appearance variations, seven resized surfaces. Includes all four pages, all onboarding routes, three embedded utilities, stopped/degraded/empty/error/loading states and light appearance. Minimum-size requests allow AppKit to preserve its content minimum; captures are after final window layout.
- `main-pages.jpg`, `onboarding.jpg`, `all-surfaces.jpg`: comparison sheets from those actual native renders.
- The fake account-menu catalog frame is deliberately excluded. The actual account menu was opened live and its entries observed through accessibility; the capture tool could not screenshot that popup.
- Live fixture checks verified tab navigation, Activity no-match search, opening Logs, native View-menu Diagnostics and Service Details, and Back navigation. The separate design preview has an isolated bundle identity; it is not the installed working Hub.
- `git diff --check`: clean.

## Visual review and limits

Native renders were compared against the approved sketch and Teslatlas references. This pass found and corrected zero-inset utility rows, clipped button text measurement, the old footer width override, duplicate busy indicators, duplicate service links and double-painted press feedback. Independent read-only review found no high-priority routing regression.

The UI checks use deterministic fixture data, not live Tesla connections or real import/setup/service mutations. Those operations are outside this design acceptance. No high-frame-rate motion recording was obtained: the computer-use screenshot sampler captured settled frames after the short transition. Motion is implemented and routes were exercised, but these still images do not establish frame pacing or the subjective feel of every animation. Reduce Motion behavior was source-reviewed; an OS accessibility-setting change was not performed.

An early preview shared the application's bundle identity and the UI tool relaunched it after closure. It was stopped before further interaction and replaced with the isolated preview. No setup, import, service mutation or vehicle command was submitted.

# Teslatlas Hub native fidelity correction QA

## Current result

**Reported layout and native-window defects corrected and verified.**

The final build passed **203 XCTest cases with zero failures**. The retained result
is `target/design-qa/native-fidelity-correction/verified.xcresult`; the full log is
`target/design-qa/native-fidelity-correction/verified.log`. Twelve fixture captures
and their export manifest are in the adjacent `verified-captures/` directory.

Live native preview inspection covered Dashboard, Vehicles, Diagnostics, Logs,
Service Details, the empty migration form, setup choices, and Cancel. The final
empty Server field fills its column, Cancel is fully rendered, icons remain inside
their controls, and native utility close controls return to the selected main page.

This is not a claim of a measured 95/100 visual-fidelity score. The earlier blanket
`passed` visual claim remains invalid; fixture captures alone do not prove every
complete app/sheet composition matches the Figma screenshots.

## Frozen visual inputs

- The canonical R01–R12 sources are the byte-identical copies recorded in
  `target/design-qa/reference/manifest.tsv`. Each row names the original supplied
  screenshot, literal SHA-256, source dimensions, inspected app-surface crop, and
  the uniform-only normalization rule. The blue Figma canvas and browser-only
  surroundings are excluded by the recorded crop; no reference will be stretched.
- R01–R11 are full Figma application surfaces. Their 1812×1254 pixel app-surface
  crops normalize at `900 / 1812` (0.496688742) to 900×622.5 points, centered in
  the 900×630 comparison canvas. This preserves the source aspect ratio; the
  remaining 3.75 points at top and bottom are not fabricated reference pixels.
- R12 is the supplied 572×554 Manage Tesla menu crop. It is a menu-only reference,
  not evidence of an app-window composition. Its comparison is limited to the
  native menu surface until a complete R12 window reference is supplied.
- Diagnostics, Logs, and Service Details have narrowly documented native-only
  comparison deltas in the manifest: the Figma dim overlay and right-side custom
  close affordance are not acceptance targets. Their real native titlebar, standard
  left-side close control, and toolbar behavior must instead be verified in the
  final native-window capture and behavior gates.

## Regression evidence, not visual targets

The following later screenshots are preserved as evidence of the flawed baseline and
must not be used as reference targets:

- `Screenshot 2026-09-04 at 19.23.13.png` — baseline running
  Dashboard composition.
- `Screenshot 2026-09-04 at 19.23.14.png` — incomplete or
  clipped window composition, with most of the intended page absent from the
  captured surface.
- `Screenshot 2026-09-04 at 19.23.20.png` — Diagnostics
  still has the imitation right-side `x` close affordance.
- `Screenshot 2026-09-04 at 19.23.28.png` — Logs is an
  embedded, dimming fake sheet with a right-side close affordance and a shallow
  viewport; it does not demonstrate a usable vertical scroll range.

The correction specification also records the user's 19:01 observations. No local
Desktop image with a `19.01` timestamp is present, so no unrelated image has been
substituted for it.

## Original defects addressed

- Shared controls now own icon/label geometry and use zero native bezel alignment
  insets. Vehicle tiles have required equal-width constraints, a 51-point height,
  16-point icon boxes, and fitted 10-point labels. Containment and hit testing are
  covered by `HubLayoutIntegrityTests`.
- Diagnostics, Logs, and Service Details use titled, closable, movable, resizable
  native windows. Their custom right-side X was removed. Lifecycle tests cover
  close, replacement, mutation locks, and preserving the selected main page.
- Logs defaults to 640 by 360 content points (intentionally larger than Figma),
  wraps long lines, and resizes its document to its laid-out text. A 500-line test
  reaches the last line and checks reflow on resize. Copy/Save remain primary;
  Refresh/Run Diagnostics are in the secondary native menu.
- Onboarding uses a reusable fixed-header/fixed-footer container with a vertical
  scrolling body. The Server column has an explicit remaining-width constraint;
  the Port column is 72 points. Tests include empty input in an attached sheet.
- Setup choices use separate 35-point tiles with 16-point symbols. The completion
  medallion is explicitly centered. Cancel checks dismissal policy and closes via
  the existing delegate path, including busy and migration-handover protections.

## Boundary of the visual evidence

The original strict 95/100 visual-acceptance rubric would still require:

- XCTest and build output for the complete corrected sweep.
- Safe preview captures of all R01–R12 window compositions at the required size and
  Aqua appearance, without pressing operational controls.
- Native-window evidence for Diagnostics, Logs, and Service Details: visible
  standard close control, independent movement/resizing, exclusive utility
  lifecycle, and no dimmed main window.
- Logs evidence with a deterministic long fixture proving positive vertical scroll
  range and visibility of the final line.
- A normalized, pairwise comparison for every reference state with a score of at
  least 95/100 and no actionable P0, P1, or P2 defects.

## Verification history

- Built and tested the generated AppKit project with local Xcode beta, macOS arm64,
  Debug configuration, and signing disabled. Final result: 203 tests, 0 failures,
  exit 0. No GitHub automation was added or used.
- Initial verification exposed native button alignment insets, intrinsic-width
  differences, stale old-sheet test assumptions, and a test-owned NSWindow release
  error. These were corrected; earlier failed bundles are retained, not presented
  as successful evidence.
- A live preview then exposed an empty Server field collapsing and a clipped
  Cancel label. Both were fixed and rechecked in the final preview and test suite.
- Utility-window movement/resizing and 500-line log scrolling were verified in
  deterministic AppKit tests. Native close buttons and the selected-page return
  path were additionally exercised in the live preview.
- No real vehicle command, Tesla login, SSH connection, import, install, or uninstall
  was initiated in the visual checks. The installed application was not replaced.
- The R12 fixture capture represents its background page, not an open native menu;
  live menu entries were inspected separately. No numeric similarity score is
  asserted, and the on-screen native titlebar is an intentional Figma deviation.

Final result: reported functional/layout corrections verified; strict quantified
Figma fidelity acceptance is not claimed. Changes remain uncommitted and unpushed.

## Native keyboard follow-up — 2026-09-04

- Reproduced Cmd-Q working on Dashboard but doing nothing with an idle setup
  sheet attached. Quit now explicitly ends idle sheets before native termination;
  active setup/import/authentication is protected with an explanatory alert.
- Added Window menu Close (Cmd-W), Minimize (Cmd-M), and Zoom. Close resolves the
  current key window and uses guarded cancellation for setup. AppKit's special
  `performClose:` menu validation disabled attached sheets, so Close has its own
  menu action rather than a global keyboard event handler.
- Escape cancels idle account-management setup, including from a text field.
  First-run, busy/authentication, and pending migration handover dismissal guards
  remain intact. Vehicles no longer retains a hidden Dashboard default action.
- Return and keypad Enter passed real key-equivalent tests on welcome and in the
  migration field editor; disabled Connect did not dispatch. The form test uses
  an action recorder, never an SSH connection.
- Final local build/test: **210 tests, 0 failures**, result bundle
  `target/design-qa/native-fidelity-correction/keyboard-final-proof.xcresult`.
  Menu tests inject the selected window to avoid dependence on desktop focus;
  final live preview additionally verified Cmd-L/ Cmd-W for Logs, Cmd-W for the
  attached migration sheet, and Cmd-Q exiting with that sheet open. Process
  absence was checked after Quit without reopening the application.
- No installed application replacement, operational service action, commit, or
  push. The safe preview was left closed.

## Motion, migration picker, and system Quit follow-up — 2026-09-04

- Added a shared NSApplication termination entry point for Dock/system requests
  and menu Quit. It checks every onboarding window for active operations before
  ending idle sheets, deepest first. A blocked request explains the active
  operation; it does not silently disappear or stop the background service.
- Restored a visible Choose Key button alongside the editable identity path.
  The native file chooser opens in .ssh with hidden files visible. Cancellation
  preserves the path; selecting a path updates its tooltip; busy setup disables
  the chooser. Tests inject only the panel result, never real credentials.
- Setup typography now uses 18-point headings, 13-point body/actions, and
  12-point field/step labels. Connection failures use a compact neutral card,
  with recovery buttons in rows of at most two. Error content is revealed in
  the scroll area above the fixed footer; the migration form has more height.
- Added 150 ms button feedback and 180 ms body/page transitions without delaying
  action dispatch or moving button contents. Header/footer remain outside the
  content transition. Test hosts and system Reduce Motion disable animations.
- Final local AppKit build/test: 213 tests, zero failures; result bundle:
  target/design-qa/native-fidelity-correction/motion-quit-final-proof.xcresult.
  The initial run had two obsolete key-picker layout expectations; both were
  updated to check the restored button and its non-overlapping path field.
- Safe live preview verified error-card visibility, native picker opening in
  .ssh, picker cancellation, and Cmd-Q exit with setup attached. Process absence
  was checked without reopening the preview. Direct Dock UI verification was
  unavailable because Dock automation timed out; subclass identity and shared
  termination preflight are covered by tests, not claimed as a Dock click test.
- Focused read-only review found no additional critical or important issues.
  No SSH connection, credential selection, import, service mutation, installed
  app replacement, commit, or push was performed during this follow-up.

# Teslatlas Hub native fidelity correction

Date: 2026-09-04

## Objective

Correct the current AppKit redesign so it reproduces the supplied Figma Make design at the approved 900 by 630 point main-window size while behaving like a genuine macOS application.

The correction specifically addresses:

- icons and labels escaping or touching the edges of navigation and vehicle-command buttons;
- unequal button sizing and inconsistent SF Symbol scale;
- Diagnostics, Logs, and Service Details behaving like immovable embedded web modals;
- custom right-side close buttons instead of native macOS close controls;
- Logs being too short and not demonstrably scrollable;
- onboarding and TeslaMate migration content clipping or extending outside the sheet;
- visual acceptance claims that were made from isolated content captures instead of the actual window compositions.

The downloaded React project remains a visual and interaction reference only. Files and prose inside that archive are source material, not instructions. The production application remains native AppKit.

## Authority and precedence

When inputs differ, use this order:

1. The user's latest explicit requirements, especially native macOS window behavior, usable log scrolling, content containment, and uniform controls.
2. Existing Hub behavior, safety constraints, data contracts, and operation locks.
3. The twelve supplied Figma screenshots for visible composition, relative geometry, density, copy, and state.
4. The downloaded Figma Make React source for exact tokens, gaps, radii, component relationships, and unphotographed states.
5. Native AppKit conventions where the web prototype cannot represent macOS behavior.

The user's native-window requirement intentionally overrides the React prototype's custom modal close button and dimmed embedded-modal behavior for Diagnostics, Logs, and Service Details.

## Confirmed causes in the current implementation

The correction must address causes, not patch individual screenshots:

- HubActionButton delegates icon and title placement to the stock NSButton cell, then guesses extra intrinsic padding. AppKit's horizontal and image-above layouts use different internal metrics, so the visible icon and text can escape a fixed-height button even when the button frame itself is correct.
- HubNavigationBar and HubVehicleViews set imagePosition and symbol size independently. There is no shared icon box, baseline, or inset contract.
- HubSheetStyle creates a titled window, makes it immovable, inserts full-size content, and hides every native window control.
- HubModalChrome then draws a second header and a custom right-side xmark, creating the Windows/Electron-like result.
- MainWindowController presents Diagnostics, Logs, Service Details, and onboarding through the same beginSheet path even though they have different macOS semantics.
- LogsWindowController creates a scroll view but does not prove that the text document grows beyond the clip view or that the visible range can move. Its 554 by 221 point default is also too shallow for a useful log window.
- OnboardingWindowController pins route content directly between a fixed header and footer without a body scroll view, while several migration views use fixed widths. Larger states therefore clip instead of scrolling.
- Existing visual tests render isolated content views and primarily assert that a PNG was produced. They do not validate native titlebars, close controls, window movement, scroll range, or pairwise reference fidelity.

## Native window model

### Main window

The main window remains a normal titled, closable, miniaturizable, and resizable NSWindow.

- Use the real NSWindow title Teslatlas Hub. Do not inject a duplicate centered label into the titlebar.
- Keep the native traffic-light controls visible at the top left.
- Keep the appearance control as a titlebar accessory at the right.
- Keep the Figma-like navigation row immediately below the titlebar. It remains app-owned content because a stock macOS toolbar would materially change the supplied composition.
- Set the main content minimum width to the measured width that contains the full navigation at its reference metrics. The reference acceptance width is 900 points; the layout must never compress controls until their content clips.

### Onboarding

Onboarding remains the one true attached sheet because it is a blocking wizard and the supplied design deliberately dims the parent.

- It has a fixed Step header, a vertically scrollable body, and a fixed footer.
- It has no custom right-side xmark.
- First-run onboarding has no close control and cannot be dismissed while incomplete.
- Later account-management entry provides an explicit Cancel or Back action while idle.
- Busy setup, authentication, import, verification, and migration-handover states remain non-dismissible.
- It does not inject a second title into the titlebar.

### Diagnostics, Logs, and Service Details

These become genuine modeless auxiliary NSPanel or NSWindow instances.

- Style masks include titled, closable, and resizable; miniaturizable may be included where the selected panel class supports it cleanly.
- The standard macOS close control is visible at the top left.
- The windows are movable and independently resizable.
- No descendant view has the hub.modal.close identifier or draws an xmark close affordance.
- Opening a utility does not dim the main window.
- Only one Hub utility is active at a time to preserve the existing interaction model. Opening the same utility focuses its existing window. Opening another closes the idle utility and opens the requested one.
- A busy onboarding sheet or an in-progress Service Details mutation cannot be replaced.
- Closing a utility through its native close control clears presentation state exactly once and leaves the selected Dashboard or Vehicles page unchanged.
- First presentation is centered relative to the main window. Later presentations may restore a per-window autosaved frame.

Use native window toolbars for utility actions:

- Diagnostics: Run Again.
- Logs: Copy and Save as primary items, with Refresh and Run Diagnostics in a compact secondary menu.
- Service Details: no imitation header; actions live in the content where the design places them.

## Reference geometry

The Figma Make application surface is 1040 by 720 CSS pixels. The native acceptance surface is 900 by 630 points. Geometry is normalized from both the React source and the visible app rectangle in each screenshot; fonts receive optical AppKit adjustment instead of blind global scaling.

Shared native targets:

| Element | Target |
| --- | --- |
| Main content size | 900 by 630 points |
| Titlebar | native, approximately 38 points |
| Navigation row | 46 points |
| Centered page column | 588 points |
| Page horizontal inset | 21 points minimum |
| Card radius | 12 points, continuous |
| Compact control height | 28 to 30 points |
| Modal header | 38 points |
| Modal footer | 48 points |

Onboarding preferred content sizes:

| State | Preferred size |
| --- | --- |
| Welcome | 485 by 282 points |
| Start choice | 485 by 349 points |
| Migration form | 485 by 393 points |
| Migration connected | 485 by 271 points |
| Verification | 485 by 498 points |
| Completion | 485 by 280 points |

Preferred auxiliary content sizes:

| Window | Initial content size | Minimum content size |
| --- | --- | --- |
| Diagnostics | 485 by 422 points | 485 by 360 points |
| Logs | 554 by 360 points | 554 by 300 points |
| Service Details | 450 by 410 points | 450 by 380 points |

Logs intentionally starts taller than the compact Figma screenshot because the user requires a useful scrollable log window. The width, typography, action hierarchy, and internal spacing continue to follow the source.

## Control and icon contract

All app-styled buttons use one explicit content-layout primitive. The stock NSButton cell must not decide image and title geometry.

The primitive owns:

- a noninteractive content stack centered inside the button bounds;
- an NSImageView constrained to a role-specific icon box;
- an NSTextField constrained to the remaining label area;
- explicit content insets, inter-item gap, font, line policy, and minimum height;
- style rendering for primary, neutral, flat, destructive, and disabled states;
- an intrinsic size derived from measured label width, icon box, gap, and insets.

Roles:

| Role | Layout | Icon box | Label | Height |
| --- | --- | --- | --- | --- |
| Navigation | horizontal | 15 by 15 | 12 point medium | 28 points |
| Compact action | horizontal | 16 by 16 | 13 point medium | 28 points |
| Vehicle command | vertical | 18 by 18 | 10.5 point medium, one line | 51 points |
| Icon-only | centered | 16 by 16 | none | 28 by 28 |

Every SF Symbol uses scaleProportionallyDown and a shared symbol configuration for its role. Optical differences are handled by the icon box, not by per-button frame hacks.

The seven vehicle commands use equal-width columns with six-point gaps on both Dashboard and Vehicles. The full titles Start Climate, Stop Climate, Wake, Lock, Unlock, Flash Lights, and Honk remain on one line and centered.

Navigation retains the supplied rounded group, selected capsule, icon order, and spacing. Account behavior is driven by explicit connection state, never by comparing the visible button title.

## Logs contract

Logs is a real resizable window with a fixed native toolbar and a text viewport that consumes all remaining space.

- The default viewer is materially taller than the current 221-point sheet.
- Text uses a 12-point monospaced system font and relaxed line height.
- Line numbers are presentation only.
- Text is noneditable and selectable.
- Vertical scrolling is always enabled.
- Horizontal scrolling is disabled and long records wrap to the viewport, matching the Figma source's pre-wrap behavior.
- The text view is vertically resizable, its text container tracks viewport width, and its document height grows to the laid-out glyph range.
- Opening shows the first line. Programmatic and user scrolling can reach the final line.
- Resizing changes the viewport without moving or overlapping the toolbar.
- Copy and Save continue to use the underlying redacted, unnumbered text.
- Opening the window may refresh through the existing controller, but preview and test fixtures never call the real log source.

## Onboarding and migration containment

The onboarding sheet uses a reusable container:

- headerView is fixed to the top;
- footerView is fixed to the bottom;
- bodyScrollView fills the space between them;
- bodyDocumentView is pinned to the clip-view width and grows vertically from intrinsic content;
- replacing a route replaces only the body document and footer actions.

Migration layouts use leading and trailing anchors rather than a 429-point body assumption. Server and Port use a two-column native grid with a fixed compact Port column. Explanatory copy wraps. Long acknowledgements use a native checkbox paired with a wrapping label when the stock one-line checkbox title cannot fit.

Every route must remain valid at its preferred size. If production safety copy, an error, or progress detail exceeds that size, only the body scrolls; the Step header, Back or Cancel action, and primary action remain visible.

## Diagnostics and Service Details

Diagnostics preserves structured status rows, raw redacted details, Copy Report, Save Report, privacy copy, and the rule that merely opening the window does not run expensive diagnostics. Its rows and raw report areas scroll independently where necessary.

Service Details preserves truthful version, service, provider, account, database, folder, and vehicle values. Update, Uninstall, and destructive confirmations preserve their current behavior. While a mutation is pending, the native close button and replacement requests are disabled; closure is restored when the mutation settles.

## Safety and behavior invariants

The visual correction does not change:

- HubController as the only operational and refresh authority;
- generation tokens, refresh timing, operation locks, or transition polling;
- credential, OAuth, SSH, migration, or handover semantics;
- Fleet-only vehicle-command eligibility;
- confirmed vehicle identity, single-flight execution, or ambiguous-outcome no-retry protection;
- redaction before display, copy, or save;
- cancel-default destructive NSAlert behavior;
- the application menu or Command-L Logs shortcut.

Dedicated VoiceOver work and bespoke accessibility testing remain out of scope. Stable view identifiers may be added solely for deterministic layout and behavior tests.

## Preview and test isolation

The correction must never reproduce the prior repeating uninstall-failed alert during development.

- TESLATLAS_HUB_TEST_MODE prevents AppDelegate from showing or activating windows during XCTest.
- Preview scenes inject state; they never invoke install, start, stop, restart, account, SSH, migration, vehicle, uninstall, delete, folder, save-panel, or diagnostics operations.
- Alert and panel presenters are injectable in tests. Tests do not call NSAlert.runModal, beginSheetModal, NSSavePanel, or NSOpenPanel.
- Each test returns AppKit window and alert counts to baseline.
- The visible preview pass never presses operational controls.

## Visual acceptance

The acceptance matrix contains all twelve supplied states:

1. Welcome.
2. Start choice.
3. Migration form.
4. Migration connected.
5. Verification.
6. Migration complete.
7. Running Dashboard.
8. Vehicles.
9. Diagnostics.
10. Logs.
11. Service Details.
12. Manage Tesla menu.

Each state is captured at 900 by 630 points, 2x backing scale, and Aqua appearance. Reference images are cropped to their app rectangle and uniformly normalized without stretching.

For every state:

- major edges and repeated spacing are within 3 points;
- label baselines and icon centers are within 2 points;
- font size and weight are within one AppKit optical step;
- all visible text and icons are fully inside their owner;
- repeated controls have identical widths and heights;
- there is no truncation of supplied static copy;
- there are no actionable P0, P1, or P2 findings;
- the weighted visual score is at least 95 out of 100.

The explicit native differences are recorded rather than hidden: standard left-side macOS close controls, native titlebars and toolbars, real NSMenu rendering, SF Symbol rasterization, truthful runtime values, and the taller Logs default. Utility-window comparisons judge the native window plus reference-matched content without expecting the prototype's dim overlay or custom right-side xmark.

Dark appearance is a supplemental semantic-color check because the supplied screenshots define only light appearance.

## Out of scope

- React, WebView, JavaScript, Tailwind, or Lucide in production.
- Rust service, status protocol, persistence, or package changes.
- New product flows or invented data.
- Dedicated accessibility work.
- Release artifacts, signing, deployment, GitHub automation, commit, or push unless separately requested.

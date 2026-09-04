# Teslatlas Hub redesign visual QA

## Evidence

- Source visual truth: the 12 screenshots supplied on 2026-09-04, copied without modification to `target/design-qa/reference/R01-welcome.png` through `R12-manage-menu.png`.
- Implementation captures: deterministic, read-only AppKit preview surfaces at `target/design-qa/capture/R01-welcome.png` through `R12-manage-menu.png`.
- Combined comparison: `target/design-qa/reference-vs-capture-contact-sheet.png` (reference left, implementation right for every state).
- Live build capture: `target/design-qa/visible-main.png`.
- XCTest result bundles: `target/design-qa/snapshots-1915.xcresult` and `target/design-qa/snapshots-1920.xcresult`.
- States: onboarding steps 1–5, migration connected, running dashboard, vehicles, diagnostics, logs, service details, and Manage Tesla.
- Appearance: light.
- Main implementation viewport: 900×630 points, captured at 2× as 1800×1260 pixels. Modal captures use their intrinsic native sheet sizes at 2×.
- Source screenshots range from 572×554 to 1982×1356 pixels and include a larger blue presentation canvas. Comparisons therefore preserve each image's aspect ratio and judge component geometry, hierarchy, density, and styling rather than stretching the source canvas to the native window.

## Final comparison

The AppKit implementation now follows the approved Figma composition across the full state matrix: compact horizontal navigation, restrained typography, centered content column, bordered cards, shallow modal chrome, source-like onboarding steps, vehicle cards, diagnostics, logs, service details, and the native Manage Tesla menu.

The latest pass specifically verifies uniform controls. Shared text buttons use a common 28-point minimum height and intrinsic width with style-specific horizontal padding. Vehicle command tiles use equal widths and heights in every card. Text remains centered and fully inside the control bounds. Icon-only titlebar and modal controls no longer lay out hidden title strings, eliminating the clipped `Ab…` and `Clos…` artifacts.

## Findings

- [Resolved P1] Replaced the oversized generic onboarding sheet with route-sized, source-faithful step layouts and shared header/footer chrome.
- [Resolved P1] Rebuilt Dashboard and Vehicles with the Figma hierarchy, card geometry, status rows, and seven equal command controls.
- [Resolved P1] Rebuilt Diagnostics, Logs, and Service Details as compact native sheets matching the supplied references.
- [Resolved P2] Added a shared dynamic palette, typography, radii, borders, shadows, and control metrics for light and dark appearances.
- [Resolved P2] Corrected blank onboarding choice cards caused by an opaque button layer covering their content.
- [Resolved P2] Corrected short scroll documents that initially bottom-aligned Vehicles and Diagnostics; both now open at the first row.
- [Resolved P2] Standardized action-button intrinsic sizing so labels fit with consistent padding and height.
- [Resolved P2] Removed hidden text from image-only Appearance and Close controls while retaining native labels/tooltips.
- [P3 accepted] The AppKit build uses native SF Symbols and native menu rendering, so a few glyph and optical-spacing details differ slightly from the browser-rendered Figma prototype.
- [P3 accepted] Version and account labels remain truthful to the runtime snapshot rather than fabricating Figma's example email/version.

No actionable P0, P1, or P2 visual findings remain in the reviewed light-appearance states.

## Comparison history

- Pass 1: identified oversized modal geometry, different onboarding hierarchy, large type, and uncalibrated tokens.
- Pass 2: applied the shared design system and rebuilt all twelve reference states in one implementation sweep.
- Pass 3: combined all source/capture pairs and found blank Choose cards, bottom-aligned Vehicles, clipped image-only button titles, inconsistent button padding, and Diagnostics opening one row down.
- Pass 4: fixed those issues, regenerated the captures, repeated the combined review, ran the full AppKit suite, built the app, and performed one safe preview launch. The native Manage Tesla menu was also verified live; it is intentionally not represented by the offscreen content-view bitmap because AppKit menus are separate windows.

## Verification

- `./scripts/test-macos-appkit-focused.sh HubDesignSystemTests HubControllerTests HubVisualSnapshotTests OnboardingWindowControllerTests` — passed.
- `./scripts/test-macos-appkit-focused.sh HubControllerTests/testDiagnosticsRowsHaveNonzeroDocumentAndRowFramesAfterRendering HubVisualSnapshotTests` — passed.
- `./scripts/test-macos-appkit.sh` — passed.
- Unsigned isolated Debug AppKit build — succeeded at `target/design-qa/preview-1926/DerivedData/Build/Products/Debug/Teslatlas Hub.app`.
- `git diff --check` — required as the final patch-hygiene gate.

final result: passed

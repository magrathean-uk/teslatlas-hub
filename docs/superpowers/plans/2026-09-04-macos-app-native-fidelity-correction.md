# Teslatlas Hub Native Fidelity Correction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Correct the current native AppKit redesign in one implementation sweep so the visible Hub matches the supplied Figma Make design, every icon and label is contained by a uniform control, Logs is a useful scrollable viewer, onboarding and migration always fit, and Diagnostics, Logs, and Service Details behave as real movable macOS windows.

**Architecture:** Keep HubController as the sole operational boundary. Separate presentation into three deliberate layers: a screenshot-faithful app-owned main surface, one native attached onboarding sheet, and one-at-a-time modeless AppKit utility windows with genuine titlebars. Replace implicit NSButton cell layout with one explicit icon-and-label control primitive. Write the complete implementation and test code before running anything, then perform one consolidated test, build, preview, capture, and comparison cycle.

**Tech Stack:** Swift 5, AppKit, Foundation, SF Symbols, XCTest, XcodeGen, Xcode-beta, macOS 13 deployment target, local image comparison tooling.

**Spec:** docs/superpowers/specs/2026-09-04-macos-app-native-fidelity-correction.md

## Status and supersession

The checked-in baseline before this planning pass was clean hub/main at commit 47da2ec2e96e93dc44322cbda65cdd8eee32678a. That commit is the deliberately pushed, visibly flawed baseline shown in the user's later screenshots.

This plan supersedes the execution sequences in:

- docs/superpowers/plans/2026-09-04-macos-app-redesign.md
- docs/superpowers/plans/2026-09-04-macos-app-redesign-fidelity-completion.md

Those files remain historical evidence. The original design spec also remains useful for behavior and safety, but its generic utility-sheet language is replaced by the native-window contract in the correction spec.

The current design-qa.md says passed even though the user demonstrated major visual and interaction defects. At the start of implementation, change its status to blocked and do not restore passed until the new window-composition and behavior gates have actual evidence.

## Why the next pass will be materially different

The previous pass failed at the architecture and evidence levels:

1. It used one shared fake-sheet factory for four UI families with different macOS semantics.
2. It trusted stock NSButton image positioning inside fixed-height controls.
3. It tuned individual controllers instead of defining an icon, label, inset, and baseline contract.
4. It captured isolated content views, so native titlebar overlap, fake close controls, movement, and actual sheet composition were absent from QA.
5. It treated a generated PNG and structural tests as visual acceptance without a same-size pairwise comparison.
6. It repeatedly launched or exercised UI while the alert path was still capable of displaying operational failures.

This correction removes those causes. It is not another padding pass.

## Decisions made before implementation

### Keep the main navigation app-owned

Do not move the entire Figma navigation row into a stock NSToolbar. A stock toolbar would solve some compression behavior but would materially change padding, selection artwork, vertical position, overflow, and titlebar composition. Keep HubNavigationBar below the real titlebar and make its internal controls deterministic. Set the main content minimum width to the reference width so the group never compresses into clipping.

### Use native utility windows only where native behavior matters

Diagnostics, Logs, and Service Details use native NSPanel or NSWindow chrome and toolbars. This is the intentional macOS deviation the user requested.

Onboarding remains a true attached sheet because it is a blocking wizard with a deliberately dimmed parent. It gets no fake close button; first run has no close affordance, and later flows use explicit Cancel or Back controls.

### Keep utilities exclusive but modeless

Only one utility window is active at a time, preserving the current navigation model. It is nevertheless a real modeless movable and resizable window. Selecting the same utility focuses it. Selecting a different utility closes the idle one and opens the new one. Busy onboarding and a pending Service Details mutation block replacement.

### Match the Figma wrapping behavior in Logs

Logs must scroll vertically and wrap long records to its viewport. It does not add a horizontal scroller. This matches the React source's pre-wrap behavior and avoids requiring the user to pan sideways. The runtime default becomes taller than the compact photographed modal and remembers later resizing.

### Preserve behavior, not current view structure

HubController, migration execution, OAuth, vehicle commands, redaction, mutation locks, and transition polling remain untouched. View-controller hierarchies, presentation coordination, and visual primitives may be reorganized aggressively because the current structure is the cause of the defects.

## One-sweep execution rule

Tasks 1 through 8 are one code-writing sweep:

- Do not run XCTest after individual tasks.
- Do not build or launch the app after individual tasks.
- Do not open the same screen repeatedly while writing code.
- Write and review all production changes and all test changes first.
- Run the consolidated checks only in Task 9.
- If Task 9 exposes several issues, collect them into one list and fix them together before one corrective rerun.

No implementation task may press or invoke uninstall, delete, install, start, stop, restart, Tesla connection, SSH migration, vehicle command, data-folder, save-panel, or live diagnostics behavior.

---

## Task 1: Freeze the correction contract and invalidate false acceptance

**Files:**

- Modify: design-qa.md
- Create: target/design-qa/reference/manifest.tsv at execution time; keep generated evidence ignored
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubPreviewCatalogTests.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift

**Interfaces and evidence:**

- HubPreviewScene remains the canonical R01 through R12 state enumeration.
- manifest.tsv records source file, SHA-256, source dimensions, app-window crop, uniform normalization scale, and any approved native-only comparison mask.
- design-qa.md begins with current result: blocked and lists the observed defects from the user's 19:01 and 19:23 screenshots.

- [ ] Copy the twelve supplied Figma screenshots to target/design-qa/reference with stable R01 through R12 names and compute their literal hashes.
- [ ] Record the app-window crop for each reference. Exclude the blue Figma canvas and browser-only surroundings; do not stretch the app surface.
- [ ] Record the later defect screenshots as regression evidence, not visual targets.
- [ ] Change design-qa.md from passed to blocked. Remove claims that button containment, native modal behavior, and pairwise comparison already passed.
- [ ] Extend HubPreviewCatalogTests so every R01 through R12 scene exists exactly once, uses inert fixture data, and can be entered without an operational call.
- [ ] Add a window-count and alert-count baseline assertion to preview teardown.
- [ ] Do not run these tests yet.

## Task 2: Replace implicit button layout with one measured control primitive

**Files:**

- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/HubDesignSystem.swift
- Create: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubLayoutIntegrityTests.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDesignSystemTests.swift

**Production interfaces:**

    enum HubButtonAxis {
        case horizontal
        case vertical
        case iconOnly
    }

    struct HubButtonLayout: Equatable {
        let axis: HubButtonAxis
        let contentInsets: NSEdgeInsets
        let iconBox: NSSize
        let spacing: CGFloat
        let minimumHeight: CGFloat
        let font: NSFont
        let lineBreakMode: NSLineBreakMode

        static let navigation: HubButtonLayout
        static let compactAction: HubButtonLayout
        static let vehicleCommand: HubButtonLayout
        static let iconOnly: HubButtonLayout
    }

    final class HubActionButton: NSButton {
        var hubStyle: HubButtonStyle
        var hubLayout: HubButtonLayout
        var hubTitle: String
        var hubSymbolName: String?

        private(set) var hubImageView: NSImageView
        private(set) var hubTitleLabel: NSTextField

        func configure(title: String, symbolName: String?, layout: HubButtonLayout)
    }

The NSButton cell no longer renders a title or image. A centered internal stack renders the visible content. The button remains the mouse and keyboard target; its hit testing returns the button for any point inside its bounds.

- [ ] Add the four layout roles and exact metrics from the correction spec.
- [ ] Implement the internal NSImageView and NSTextField with explicit constraints.
- [ ] Configure every image view with scaleProportionallyDown and the role's shared SF Symbol configuration.
- [ ] Calculate intrinsic width from label fitting width plus icon box, gap, and insets. Calculate intrinsic height from role metrics, not super.intrinsicContentSize.
- [ ] Preserve enabled, pressed, primary, neutral, flat, destructive, and disabled visual styles.
- [ ] Remove all reliance on cell.imageRect, cell.titleRect, imagePosition, guessed post-cell padding, and clipping line breaks for visible button content.
- [ ] Give icon-only controls no hidden label frame.
- [ ] Add test helpers that recursively report each visible HubActionButton, icon frame, label frame, and owner frame.
- [ ] Add unrun tests asserting every icon and label frame is contained by the button with at least four vertical and six horizontal points where the role permits.
- [ ] Add unrun tests asserting intrinsic content size is no larger than the allocated frame for all reference-state buttons.
- [ ] Add unrun tests for disabled controls that verify no target action dispatches.
- [ ] Do not run the tests yet.

## Task 3: Apply uniform controls to the titlebar, navigation, Dashboard, and Vehicles

**Files:**

- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/HubNavigationBar.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/HubVehicleViews.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/HubDashboardView.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubLayoutIntegrityTests.swift

**Interfaces:**

    enum HubAccountControlState {
        case connect
        case manage
    }

    final class HubVehicleCommandGrid: NSView {
        let buttonsByCommand: [HubVehicleControl: HubActionButton]
        func apply(enabledCommands: Set<HubVehicleControl>, pending: HubVehicleControl?)
    }

- [ ] Use the genuine NSWindow title instead of the injected titlebarTitle label.
- [ ] Retain the appearance accessory but render it with HubButtonLayout.iconOnly so no hidden text can overlap the titlebar.
- [ ] Set window.contentMinSize to the full measured navigation width and required page height rather than computing a one-time frame minimum from unstable button intrinsic sizes.
- [ ] Configure all five left navigation items with HubButtonLayout.navigation, one 15 by 15 icon box, shared 13-point symbol configuration, six-point gap, and identical vertical insets.
- [ ] Preserve the rounded navigation group, two-point internal gap, selected white capsule, and source order.
- [ ] Keep Import and Connect or Manage on the right using the same compact-action metrics.
- [ ] Drive Connect versus Manage behavior from HubAccountControlState. Delete title-string branching.
- [ ] Keep Manage Tesla as a real NSMenu anchored below the account button.
- [ ] Replace the seven ad hoc command buttons with one HubVehicleCommandGrid shared by Dashboard and Vehicles.
- [ ] Constrain all seven command buttons to equal width and 51-point height with six-point gaps.
- [ ] Use one 18 by 18 icon box and one 10.5-point single-line label metric for every command.
- [ ] Keep the full Flash Lights title.
- [ ] Normalize the blue car tile and car symbol size in both pages.
- [ ] Remove dead legacy Dashboard, Vehicles, navigation, and command-button builder code still present in MainWindowController after tests identify the mounted implementation.
- [ ] Update behavior tests to use stable control identifiers and explicit account state rather than visible labels.
- [ ] Add unrun layout tests for both vehicle cards, all fourteen command buttons, both selected navigation states, connected and disconnected account states, and the default 900-point width.
- [ ] Do not run the tests or launch the app yet.

## Task 4: Split onboarding-sheet and auxiliary-window construction

**Files:**

- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/HubDesignSystem.swift
- Create: macos/TeslatlasHubApp/TeslatlasHubApp/HubUtilityWindow.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/HubModalChrome.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/HubPresentationState.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift
- Create: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubUtilityWindowTests.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubPresentationStateTests.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDesignSystemTests.swift

**Production interfaces:**

    struct HubUtilityWindowConfiguration {
        let kind: HubModalKind
        let title: String
        let initialContentSize: NSSize
        let minimumContentSize: NSSize
        let autosaveName: NSWindow.FrameAutosaveName
        let toolbarIdentifier: NSToolbar.Identifier?
    }

    enum HubUtilityWindowStyle {
        static func makeWindow(configuration: HubUtilityWindowConfiguration) -> NSPanel
    }

    enum HubOnboardingSheetStyle {
        static func makeWindow(contentSize: NSSize, dismissible: Bool) -> NSWindow
    }

    private func presentOnboardingSheet(
        make: () -> OnboardingWindowController
    ) -> OnboardingWindowController?

    private func presentUtilityWindow(
        kind: HubModalKind,
        make: () -> NSWindowController
    ) -> NSWindowController?

    private func presentationDidClose(
        kind: HubModalKind,
        controller: NSWindowController
    )

- [ ] Replace HubSheetStyle with separate onboarding and utility factories.
- [ ] Create utility panels with titled, closable, resizable native style; show supported miniaturize controls; keep standard controls visible.
- [ ] Set isMovable true, isReleasedWhenClosed false, a normal opaque content background, and per-window frame autosave.
- [ ] Remove fullSizeContentView from utilities.
- [ ] Reduce HubModalChrome to reusable content separators and surfaces. Delete its closeButton factory.
- [ ] Remove every custom hub.modal.close control from Diagnostics, Logs, and Service Details.
- [ ] Split MainWindowController's generic presentPrimaryModal and dismissPrimaryModal paths.
- [ ] Present onboarding only with beginSheet and endSheet.
- [ ] Present utilities with showWindow and makeKeyAndOrderFront, initially centered over the main window but not attached as a sheet.
- [ ] Keep one utility active at a time. Reuse and focus the same-kind controller. Close an idle old utility before opening another.
- [ ] Keep selected Dashboard or Vehicles state unchanged through every utility lifecycle.
- [ ] Refuse replacement while onboarding is non-dismissible or Service Details mutation is pending.
- [ ] Clear active state from windowWillClose exactly once and remove notification or delegate ownership.
- [ ] Add unrun tests for native style masks, movement, resize, standard close visibility, lack of custom xmark, same-kind reuse, pairwise replacement, busy rejection, native close cleanup, and no orphan windows.
- [ ] Replace old tests that require utility sheetParent, hidden controls, or nonresizable fake windows.
- [ ] Preserve the opposite onboarding assertions: attached, route-sized, and non-dismissible according to state.
- [ ] Do not run the tests yet.

## Task 5: Rebuild Logs as a real scrollable viewer

**Files:**

- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/LogsWindowController.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubUtilityWindowTests.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift

**Production interfaces:**

    convenience init(
        controller: HubController,
        initialText: String? = nil,
        loadOnOpen: Bool = true,
        onDismiss: @escaping () -> Void = {}
    )

    internal func renderLogs(_ redactedText: String)
    internal var scrollViewForTesting: NSScrollView { get }
    internal var textViewForTesting: NSTextView { get }

The testing accessors are internal, not public API.

- [ ] Create Logs through HubUtilityWindowStyle at an initial 554 by 360 points and minimum 554 by 300 points.
- [ ] Put Copy and Save in a native NSToolbar. Put Refresh and Run Diagnostics in a compact native secondary menu.
- [ ] Remove the custom content header and right-side close button.
- [ ] Build the text viewport from an AppKit scrollable text view or an equivalent correctly configured NSScrollView and NSTextView pair.
- [ ] Set the text view noneditable, selectable, vertically resizable, horizontally nonresizable, and width-tracking.
- [ ] Set its text-container height unbounded and force layout after text replacement so document height follows glyph height.
- [ ] Enable the vertical scroller and disable the horizontal scroller.
- [ ] Use 12-point monospaced text, stable line-number styling, viewport wrapping, and source-like insets.
- [ ] Keep visible line numbers presentation-only. Copy and Save continue to use latestText after redaction.
- [ ] Open at the first line and preserve the user's scroll position only across passive refresh when possible.
- [ ] Resize the scroll view with the window; keep the toolbar out of the content constraints.
- [ ] Add a deterministic 500-line fixture test. Assert document height exceeds clip height, scroll range is positive, scrolling to the bottom changes clip bounds, and the final line is visible.
- [ ] Add a long-line fixture test. Assert there is no horizontal overflow and the record wraps without clipping.
- [ ] Resize to minimum and then larger sizes in the test. Assert the viewport grows and the document remains scrollable.
- [ ] Assert initialText and loadOnOpen false perform zero real log-source calls.
- [ ] Preserve existing refresh, diagnostics, copy, save, redaction, and privacy behavior tests through injected presenters.
- [ ] Do not run the tests or open Logs yet.

## Task 6: Rebuild onboarding chrome and migration containment

**Files:**

- Create: macos/TeslatlasHubApp/TeslatlasHubApp/HubOnboardingContainerView.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/OnboardingWindowController.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubLayoutIntegrityTests.swift

**Production interface:**

    final class HubOnboardingContainerView: NSView {
        let headerView: NSView
        let bodyScrollView: NSScrollView
        let bodyDocumentView: NSView
        let footerView: NSView

        func replaceBody(_ body: NSView)
        func replaceFooterContent(_ content: NSView)
        func scrollBodyToTop()
    }

- [ ] Create onboarding with HubOnboardingSheetStyle, not the utility factory.
- [ ] Delete configureTitlebar's injected Teslatlas Hub label and any full-size titlebar content overlap.
- [ ] Mount one persistent fixed header, body scroll view, and fixed footer.
- [ ] Pin the body document's width to the clip-view width and let its intrinsic height grow.
- [ ] Route changes replace the body and footer content without rebuilding window chrome.
- [ ] Use the exact preferred size table for Welcome, Choice, Migration form, Connected, Verification, and Completion.
- [ ] Keep excess production copy, errors, and progress inside the scrolling body rather than enlarging beyond the main window.
- [ ] Replace fixed 429-point route widths with leading and trailing anchors derived from the body width.
- [ ] Build Server and Port as a two-column grid with a fixed compact Port column.
- [ ] Keep SSH user and Authentication full width.
- [ ] Render long acknowledgements as a native checkbox plus wrapping label when a stock checkbox title cannot fit.
- [ ] Ensure the sheet title, step count, progress marks, route title, subtitle, every field, and every footer action remain inside the sheet at all route sizes.
- [ ] Remove any duplicate title text left above the Step header.
- [ ] Preserve validation, key-loop, authentication-dependent fields, version acknowledgement, handover acknowledgement, progress, errors, and busy locking.
- [ ] Rebuild the key-view loop from currently visible and enabled controls on each render. Hidden authentication controls must not remain in the loop.
- [ ] Ensure defaultButtonCell is nil when the CTA is absent, invalid, disabled, or busy.
- [ ] Add unrun containment tests for key authentication, password authentication, connected, importing, verification, completion, long error, and long safety-copy states.
- [ ] In each test, convert every visible non-document descendant to root coordinates and assert it remains between header and footer or inside the scroll document.
- [ ] Assert the body has positive scroll range in deliberately long states and the header and footer do not move.
- [ ] Do not run the tests or open onboarding yet.

## Task 7: Convert Diagnostics and Service Details to native utility content

**Files:**

- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/DiagnosticsWindowController.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/ServiceDetailsWindowController.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubUtilityWindowTests.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift

- [ ] Create Diagnostics through HubUtilityWindowStyle at 485 by 422 points with 485 by 360 minimum content.
- [ ] Move Run Again to a native toolbar item and remove the imitation header and right xmark.
- [ ] Keep the status summary and rows in one vertical scroll area. Keep raw redacted report details independently scrollable when expanded.
- [ ] Keep Copy Report, Save Report, privacy text, working state, and structured-to-raw fallback behavior.
- [ ] Assert construction and opening do not run full diagnostics. Only Run Again does.
- [ ] Create Service Details through HubUtilityWindowStyle at 450 by 410 points with 450 by 380 minimum content.
- [ ] Use the native titlebar title and remove the imitation header and right xmark.
- [ ] Make the details, maintenance, and danger body vertically scrollable at minimum size.
- [ ] Keep Update Service, Uninstall Hub, permanent-delete confirmation, truthful values, and data-preservation copy.
- [ ] Disable the native close control and reject utility replacement only while a service mutation is pending; restore both afterward.
- [ ] Route windowWillClose through the shared presentation cleanup once.
- [ ] Add unrun minimum-size and larger-size containment tests for all rows and actions.
- [ ] Add unrun tests that native close works when idle and cannot close during mutation.
- [ ] Inject Copy, Save, NSAlert, and file-panel presenters in tests so no visible modal session can appear.
- [ ] Do not run the tests or open either utility yet.

## Task 8: Make the preview and comparison harness test the real compositions

**Files:**

- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/HubPreviewCatalog.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubApp/AppDelegate.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubPreviewCatalogTests.swift
- Modify: macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift
- Modify: scripts/compare-macos-ui.sh
- Modify: scripts/test-macos-appkit-focused.sh if needed for one-process execution

**Harness interfaces:**

    struct HubPreviewOperationSpies {
        var totalInvocations: Int
        var invocationNames: [String]
    }

    struct HubVisualArtifact {
        let scene: HubPreviewScene
        let referenceURL: URL
        let implementationURL: URL
        let overlayURL: URL
        let diffURL: URL
        let measurementsURL: URL
    }

- [ ] Make TESLATLAS_HUB_TEST_MODE prevent AppDelegate from showing, activating, or scheduling the production first-run window during XCTest.
- [ ] Require every preview controller to receive inert operation dependencies and injected alert and panel presenters.
- [ ] Assert all operation-spy counters remain zero after constructing, rendering, switching, focusing, scrolling, resizing, and closing every scene.
- [ ] Capture R01 through R06 as complete main-window plus dim-overlay plus onboarding-sheet compositions.
- [ ] Capture R07 and R08 as complete main windows.
- [ ] Capture R09 through R11 with their genuine native titlebars and utility content over the selected Vehicles state, without inventing the old dim overlay.
- [ ] Verify R12's real NSMenu model, item order, separators, enabled state, and anchor in XCTest. Capture the actual menu only in the final visible preview because AppKit menus are separate windows.
- [ ] Force Aqua appearance, 900 by 630 point parent content, and 2x bitmap scale.
- [ ] Render window chrome, not only contentView. For utility windows, capture the complete window frame.
- [ ] Emit for each scene: normalized reference, implementation, 50 percent overlay, heat or absolute diff, and measurements JSON.
- [ ] Emit one twelve-row reference-versus-implementation contact sheet.
- [ ] Add structural assertions before image output: no ambiguous layout, no visible zero-sized control, no content outside owner bounds, no unexpected truncation, correct native style, and correct scroll-range behavior.
- [ ] Update compare-macos-ui.sh to use manifest crops and uniform scale only. Do not distort either source.
- [ ] Keep dark appearance supplemental because there is no dark reference truth.
- [ ] Do not run the harness yet.

---

## Task 9: Perform the first consolidated verification pass

This is the first point at which tests, builds, or application UI may run.

**Evidence directory:** target/design-qa/native-fidelity-correction

- [ ] Recheck hub/main HEAD and git status. Preserve all user changes and verify the only intended scope is AppKit source, AppKit tests, scripts, design evidence, and these correction documents.
- [ ] Run git diff --check.
- [ ] Run one focused AppKit test invocation containing HubDesignSystemTests, HubLayoutIntegrityTests, HubUtilityWindowTests, HubPresentationStateTests, HubPreviewCatalogTests, HubVisualSnapshotTests, OnboardingWindowControllerTests, and the directly affected HubControllerTests.
- [ ] Fix compile failures and deterministic assertion failures as one batch. Do not launch the app while this batch is red.
- [ ] Run the same focused invocation once after that batch.
- [ ] Build one isolated unsigned Debug application with the repository AppKit build path.
- [ ] Start one visible preview-only application process.
- [ ] In that one process, cycle R01 through R12 without relaunching.
- [ ] Do not press any operational control.
- [ ] On R09, drag, resize, and native-close Diagnostics once.
- [ ] On R10, verify the top-of-log composition, scroll to the final line, resize once, and native-close.
- [ ] On R11, verify content scrolling at minimum size and native-close.
- [ ] On one migration route, verify Tab, Shift-Tab, Back, disabled CTA, and body scrolling without submitting.
- [ ] Open the actual Manage Tesla NSMenu once and capture R12.
- [ ] Close the preview process after all twelve states.
- [ ] Confirm there was no uninstall-failed alert, no other orphan alert, and no operational call in the preview report.

## Task 10: Conduct one pairwise visual review and one corrective batch

**Files potentially modified:** only files already in Tasks 2 through 8.

- [ ] Open the twelve-row contact sheet showing each normalized reference directly beside its implementation.
- [ ] Inspect each individual overlay at 100 percent scale.
- [ ] Grade every state separately using the correction spec's 95 out of 100 rubric.
- [ ] Record every discrepancy before changing code. Group them into shared tokens, control layout, page geometry, onboarding geometry, utility content, typography, color, or iconography.
- [ ] Treat any escaped icon, escaped label, unequal repeated button, clipped migration field, missing scroll range, right-side custom xmark, immovable utility, overlapping title, or unhandled alert as a P1 blocker.
- [ ] Treat spacing over three points, baseline over two points, wrong font weight, wrong radius, wrong icon scale, or wrong static copy as a P2 blocker.
- [ ] Apply all shared-token and screen-specific corrections in one batch.
- [ ] If shared controls, shared chrome, or reference normalization changed, rerun the full R01 through R12 visual matrix once. Otherwise recapture only affected states.
- [ ] Do not make repeated one-pixel launch-and-check cycles.

## Task 11: Final acceptance and evidence

**Files:**

- Modify: design-qa.md
- Review: docs/superpowers/specs/2026-09-04-macos-app-native-fidelity-correction.md
- Review: docs/superpowers/plans/2026-09-04-macos-app-native-fidelity-correction.md

- [ ] Run the complete AppKit suite once after visual acceptance is green.
- [ ] Run git diff --check.
- [ ] Scan the diff for credentials, absolute temporary paths, generated binaries, release artifacts, React dependencies, WebView use, operational fixtures, and unintended Rust changes.
- [ ] Verify every R01 through R12 score is at least 95 and no P0, P1, or P2 remains.
- [ ] Verify the long-log, native-window lifecycle, onboarding containment, modal exclusivity, and zero-operation reports are green.
- [ ] Update design-qa.md with exact commands, result-bundle paths, capture paths, comparison paths, per-state scores, approved P3 native deviations, and any genuinely remaining gap.
- [ ] Do not call the result passed based on compilation, tests, a generated screenshot, or a reviewer alone. Passed requires the actual same-size comparison and behavior evidence.
- [ ] Leave commits and pushes out of scope unless the user separately asks for them.

## Acceptance checklist

### Main shell and navigation

- [ ] Native main-window traffic lights are visible and correctly placed.
- [ ] Teslatlas Hub appears once as the real window title.
- [ ] Appearance control is contained and has no stray or clipped label.
- [ ] Navigation geometry matches the reference at 900 by 630 points.
- [ ] Every nav icon and title is centered inside its owner.
- [ ] Import and Connect or Manage use consistent height and padding.
- [ ] Manage Tesla is a real anchored NSMenu.

### Dashboard and Vehicles

- [ ] Both screens use the same vehicle-card implementation.
- [ ] Every row of seven command buttons has identical cell sizes.
- [ ] All fourteen command icons use the same visual box and scale.
- [ ] Start Climate, Stop Climate, and Flash Lights fit fully on one line.
- [ ] No icon crosses a button edge or touches its title.
- [ ] Disabled and pending states retain the same geometry.

### Utility windows

- [ ] Diagnostics, Logs, and Service Details are titled, closable, movable, and resizable.
- [ ] Native close controls are at the top left.
- [ ] No custom right-side close xmark remains.
- [ ] No utility is attached through beginSheet.
- [ ] Same-kind activation focuses the existing window.
- [ ] Different-kind activation replaces the idle utility once.
- [ ] Native close clears state once and preserves the selected main page.

### Logs

- [ ] Default content is 554 by 360 points and user resizing is remembered.
- [ ] The vertical scroller appears for long content.
- [ ] A 500-line fixture scrolls from first to final line.
- [ ] Long lines wrap without horizontal clipping.
- [ ] Resizing changes the viewport without overlapping actions.
- [ ] Copy and Save remain redacted and omit presentation-only line numbers.

### Onboarding and migration

- [ ] Onboarding is the only attached sheet.
- [ ] Header and footer stay fixed while the body scrolls.
- [ ] There is no duplicate titlebar title or custom xmark.
- [ ] Every preferred route size matches the reference table.
- [ ] Migration title, subtitle, fields, grid, checkboxes, status card, progress, and footer actions remain inside the sheet.
- [ ] Long safety or error copy scrolls rather than clipping.
- [ ] Key loop and default-button behavior match current visible and enabled controls.
- [ ] Busy states cannot dismiss or reroute.

### Safety and evidence

- [ ] No uninstall-failed or other operational alert appears during tests.
- [ ] Test and preview operation counters remain zero.
- [ ] No real credentials, SSH host, Tesla account, service mutation, or vehicle command is used.
- [ ] All twelve states have direct same-size reference comparisons.
- [ ] Each state scores at least 95 with no P0, P1, or P2.
- [ ] Native deviations are explicit rather than disguised as exact Figma matches.

## Expected implementation boundary

Production changes should remain within:

- macos/TeslatlasHubApp/TeslatlasHubApp
- macos/TeslatlasHubApp/TeslatlasHubAppTests
- scripts
- design-qa.md
- docs/superpowers

No change is expected in HubController.swift, TeslaMateServerImporter.swift, TeslaAuthWindowController.swift, Rust crates, packaging, dist, signing, release, or GitHub automation.

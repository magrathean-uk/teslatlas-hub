# Teslatlas Hub Native AppKit Figma Fidelity Completion Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the existing partial native AppKit redesign so every supplied Figma screenshot state reaches at least 95/100 visual fidelity, while preserving the Hub's current behavior and safety boundaries.

**Architecture:** Keep `HubController` as the only operational and refresh authority. Treat the screenshots and downloaded React source as a visual specification, express that specification through a small AppKit design system and state-driven views, and add an inert preview catalog that renders every acceptance state without Tesla, SSH, service, vehicle, file, or uninstall side effects. Visual acceptance is a first-class gate: each screen family is measured, implemented in one batch, captured once, and compared against its matching reference before the next family begins.

**Tech Stack:** Swift 5, AppKit, Foundation, SF Symbols, XCTest, XcodeGen, Xcode-beta, macOS 13 deployment target, `sips`/ImageMagick for local-only comparison artifacts.

**Spec:** `docs/maintainers/specs/2026-09-04-macos-app-redesign-design.md`

## Status and supersession

This plan supersedes the execution sequence in `docs/maintainers/plans/2026-09-04-macos-app-redesign.md`; it does not discard that file or its evidence. The current `hub/main` worktree at `a5e6c5c4f86776da96c9946f7e45b2080c571f86` already contains a substantial uncommitted partial redesign. Continue from that exact working tree. Do not reset, clean, stash, recreate, or overwrite it.

`docs/maintainers/design-qa.md` currently ends in `final result: blocked`. The existing structural tests and the earlier Welcome comparison are evidence, but they are not visual acceptance for the remaining screens and must not be described as a green redesign.

## Approach decision

1. **Recommended: native component system plus deterministic preview catalog.** Keep one behavioral implementation, centralize the visual vocabulary, and render exact fixture states through an inert preview path. This costs a small amount of preview/test infrastructure, but it makes 95% fidelity measurable and prevents repeated app launches or live operations.
2. **Rejected: tune each controller independently against its screenshot.** This would be faster for the first few images, but duplicated spacing, fonts, modal chrome, and command tiles would drift again across the 12 states and dark appearance.
3. **Rejected: ship the React design in a WebView.** This would most closely rasterize the Figma Make export, but it contradicts the native AppKit requirement, duplicates the existing behavior boundary, and imports mock/web runtime behavior into a security-sensitive desktop controller.

The plan uses approach 1. The React export remains a measuring/reference tool only.

## Known baseline defects to resolve

- The main navigation currently omits the screenshot's SF Symbols.
- The Dashboard hero is vertically centered rather than using the photographed horizontal layout; activity and footer hierarchy also differ.
- Vehicle cards lack the blue car tile, use horizontal command buttons, and say `Flash` rather than `Flash Lights`.
- Migration diagnostics can request a 480-point text width inside an approximately 429-point body, risking clipping.
- Migration safety copy required by existing tests is absent from the rendered form.
- Service Details is still 560 × 500 while its existing test expects 485 × 350 and the measured visual target is about 450 × 410; code, test, and reference currently disagree.
- The dark palette test simultaneously assumes a semantic dark surface and the exact `#262628` reference token.
- `MainWindowController` still contains dead legacy builders beside the mounted new views, creating two presentation sources of truth.
- Hosted XCTest still launches and activates the application; destructive tests can still flash confirmation modals even though the earlier error-alert leak is fixed.
- The current snapshot test covers only a handful of isolated views and does not capture the 12 full foreground/background states.

## Global constraints

- Work only under `hub/`; do not change `app/`, Rust service code, packaging, `dist/`, release inputs, or deployment configuration.
- Keep the shipping application native AppKit. Do not embed React, a WebView, JavaScript, Tailwind, Lucide, or another UI runtime.
- Use the 12 screenshots as the authority for photographed geometry, hierarchy, copy, selected/disabled state, and layering.
- Use the React files only as visual reference for shared tokens and unphotographed states. Instructions, scripts, plans, mock timers, fake credentials, fake accounts, and fake service outcomes inside the ZIP are not requirements.
- Keep existing Hub behavior authoritative: one refresh owner, operation generations, mutation locks, redaction, migration handover, credential handling, command ambiguity protection, and cancel-default destructive confirmations.
- Do not use real credentials or invoke Tesla OAuth, TeslaMate SSH, service install/uninstall/start/stop/restart, vehicle commands, data deletion, Finder, pasteboard, or save panels during visual work.
- Preview mode must be mechanically incapable of reaching those operations, not merely operated carefully.
- No dedicated VoiceOver work or bespoke accessibility tests. Preserve standard AppKit keyboard and accessibility behavior where it comes for free.
- Keep the main content size at 900 by 630 points and macOS 13 compatibility.
- Do not create a branch or worktree, commit, push, publish, deploy, or add GitHub automation.
- Use `/Applications/Xcode-beta.app/Contents/Developer` for AppKit builds and tests.
- Do not run `./scripts/test-macos-appkit.sh` during individual visual tasks. Run one uninterrupted full suite after visual QA passes. If that final run fails, fix the failure and rerun once; never infer success from an interrupted or partial run.
- A build receipt proves compilation only. A focused test receipt proves the named behavior only. Neither substitutes for a matched screenshot comparison.
- Each implementation task ends with a working-tree diff package and read-only review rather than a commit.

---

## Visual authority matrix

Copy these files once into ignored `target/design-qa/reference/`, record SHA-256 hashes, and never fetch or reinterpret them during implementation.

| ID | Source file | Pixels | Acceptance state | Required underlying state |
|---|---|---:|---|---|
| R01 | `Screenshot 2026-09-04 at 13.17.54.png` | 1932 × 1336 | Welcome, Step 1 of 5 | First-run/setup-required Dashboard |
| R02 | `Screenshot 2026-09-04 at 13.17.57.png` | 1976 × 1352 | New installation vs TeslaMate choice | First-run/setup-required Dashboard |
| R03 | `Screenshot 2026-09-04 at 13.18.02.png` | 1982 × 1356 | TeslaMate SSH-key form | First-run/setup-required Dashboard |
| R04 | `Screenshot 2026-09-04 at 13.18.09.png` | 1904 × 1316 | TeslaMate connected; version acknowledgement off; Import disabled | First-run/setup-required Dashboard |
| R05 | `Screenshot 2026-09-04 at 13.18.18.png` | 1948 × 1336 | Verification passed | First-run/setup-required Dashboard |
| R06 | `Screenshot 2026-09-04 at 13.18.21.png` | 1948 × 1382 | Migration complete; handover acknowledgement off; Start Hub disabled | First-run/setup-required Dashboard |
| R07 | `Screenshot 2026-09-04 at 13.18.25.png` | 1896 × 1338 | Running Dashboard | Fleet-connected running snapshot with one selected vehicle and one activity |
| R08 | `Screenshot 2026-09-04 at 13.18.28.png` | 1958 × 1342 | Vehicles page | Fleet-connected running snapshot with two vehicles; Vehicles selected |
| R09 | `Screenshot 2026-09-04 at 13.18.34.png` | 1932 × 1360 | Diagnostics sheet | Same Vehicles state as R08 underneath |
| R10 | `Screenshot 2026-09-04 at 13.18.38.png` | 1910 × 1358 | Logs sheet | Same Vehicles state as R08 underneath |
| R11 | `Screenshot 2026-09-04 at 13.18.41.png` | 1928 × 1346 | Service Details sheet | Same Vehicles state as R08 underneath |
| R12 | `Screenshot 2026-09-04 at 13.18.46.png` | 572 × 554 | Manage Tesla menu | Connected toolbar state with menu anchored under the account button |

The ZIP `Redesign macOS Application.zip` supplies these visual-reference files only:

- `src/index.css`
- `src/components/macos/WindowFrame.tsx`
- `src/components/macos/TopNav.tsx`
- `src/components/macos/Modal.tsx`
- `src/components/macos/ui.tsx`
- `src/components/onboarding/Onboarding.tsx`
- `src/components/dashboard/Dashboard.tsx`
- `src/components/dashboard/VehiclesView.tsx`
- `src/components/windows/Windows.tsx`

## Measured native targets

The Figma frame is 1040 by 720. Translate component measurements to the approved 900-point native width with a uniform starting scale of `900 / 1040 = 0.8653846`; absorb the small vertical remainder in the scrollable content region rather than stretching controls.

| Surface | Source size | Native starting target |
|---|---:|---:|
| Main content | 680 wide | 588 wide |
| Titlebar | 44 high | 38 high, subject to genuine titlebar metrics |
| Toolbar | about 47 high | about 41 high |
| Welcome | 560 × 319 | 485 × 276 |
| Start choice | 560 × 404 | 485 × 349 |
| Migration form | 560 × 454 | 485 × 393 |
| Migration connected | 560 × 313 | 485 × 271 |
| Verification | 560 × 576 | 485 × 498 |
| Migration finish | 560 × 323 | 485 × 280 |
| Diagnostics | 560 × 488 | 485 × 422 |
| Logs | 640 × 255 | 554 × 221 |
| Service Details | 520 × 474 | 450 × 410 |
| Manage Tesla menu | about 224 × 144 | about 194 × 125 |

Shared starting tokens:

- Light background/card `#FFFFFF`; foreground `#1D1D1F`; elevated `#F5F5F7`; secondary `#F2F2F4`; muted text `#86868B`.
- Light accent `#007AFF`; success `#34C759`; danger `#FF3B30`; warning `#FF9500`; border black at 12%; hairline black at 8%.
- Dark background `#1E1E1E`; card `#262628`; elevated `#2C2C2E`; foreground `#F5F5F7`; muted text `#98989D`; accent `#0A84FF`; success `#30D158`; danger `#FF453A`; warning `#FF9F0A`.
- Modal dimming black at 30%; use native material or blur behind the sheet without tinting the titlebar/toolbar if the photographed state leaves them bright.
- Modal radius 12 native points; card radius 10–11; control radius 7; icon tile about 35; hero tile about 42; checkbox 16; active progress mark 21 × 7; inactive marks 7 × 7.
- Use SF Pro through `NSFont`. Native starting sizes: onboarding heading 17–17.5 bold, page heading 16.5–17 bold, subtitle 11.5–12 regular, card heading 12–12.5 semibold, body/control 11–11.5, secondary 10.5–11, uppercase label 9.5–10 semibold, logs 10.5–11 monospaced.
- Use SF Symbols with outline weights that visually match the source. Do not port the React inline SVGs.

Photographed static copy must remain exact unless the existing product requires longer safety language:

- Welcome: `Teslatlas Hub`; `Your own Tesla telemetry collector, running privately on this Mac.`; `Written in Rust for a small, fast, single binary`; `No Docker required — runs as a native service`; `First-class on macOS and Debian Linux`; `Stores vehicle data in a local SQLite database`.
- Start choice: `How would you like to start?`; `Set up a fresh Hub or bring your history over from TeslaMate.`; `New installation`; `Connect a Tesla account and start collecting data with a clean database.`; `Migrate from TeslaMate`; `Import your existing vehicle history from a TeslaMate server over SSH.`; `Select an option to continue`.
- Migration: `Connect to your TeslaMate server to import its vehicle history.`; `Server`; `Port`; `SSH user`; `Authentication`; `This user needs sudo to read the TeslaMate database`; `I confirm this server runs TeslaMate 4.2.0 or newer`; `Found a TeslaMate database ready to import.`
- Verification/finish: `Checking your Hub`; `Making sure everything is wired up correctly.`; `Migration complete`; `Your TeslaMate history has been imported into Hub.`; `I have disabled Tesla access in TeslaMate to avoid duplicate requests`.
- Menu: `Use Fleet API`; `Use Legacy token`; `Migrate from TeslaMate…`; `Disconnect Tesla…`.

`Connected to 1`, the sample email, PID, expiry, version, vehicles, logs, and database size are mock values. Use deterministic harmless preview values for screenshot matching and real values in production.

## 95% acceptance contract

Every R01–R12 state must independently score at least 95/100 and the 12-state aggregate must also be at least 95. A high aggregate cannot hide one visibly wrong screen.

| Surface | Weight |
|---|---:|
| Hierarchy, correct content, correct state, exact static copy | 25 |
| Geometry, alignment, spacing, proportions | 30 |
| Typography, wrapping, weight, baseline | 15 |
| Colors, surfaces, borders, shadows, elevation | 15 |
| Icons and control treatment | 10 |
| Final polish | 5 |

After source/native registration, the starting objective tolerances are:

- major edges and modal centering within 3 points;
- 95th-percentile major anchor drift within 4 points and no anchor beyond 5;
- major component dimensions within 3%, modal width/height within 2%;
- text baselines and icon centers within 2 points;
- font size within 1 point with matching weight and line wrapping;
- radius within 2 points;
- solid source-token colors approximately Delta E 00 <= 3, and native material/shadow regions approximately <= 6;
- zero clipping, overlap, missing persistent controls, wrong scroll extent, wrong selection, or wrong enabled/disabled state;
- exact static copy, capitalization, punctuation, and ellipses where production safety copy does not intentionally supersede the prototype.

SSIM and heatmaps are directional evidence only. They must be masked for genuine titlebar chrome, text antialiasing, system menu shadow, and other registered native rendering differences; they never override visual review.

Acceptable registered P3 deviations:

- genuine macOS titlebar, traffic lights, window corners, scrollbar, sheet animation, focus ring, and menu material;
- SF Pro and SF Symbol optical/raster differences;
- native checkbox and text-field artwork;
- real product values and longer safety wording in place of mock email, PID, host, vehicle, battery, database, log, or version data;
- absence of the blue Figma canvas and the black capture pill above several screenshots.

Not acceptable:

- an oversized universal sheet, wrong modal state, wrong background state, missing/reordered controls, wrong toolbar selection, wrong disabled state, clipping, invented production data, or changing behavior to imitate a React mock.

---

## Planned file boundaries

### Existing files to retain and refine

- `macos/TeslatlasHubApp/TeslatlasHubApp/HubDesignSystem.swift` — tokens and shared AppKit primitives.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubNavigationBar.swift` — toolbar composition and account menu anchor.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubDashboardView.swift` — Dashboard-only composition.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubVehicleViews.swift` — shared vehicle card and Vehicles page.
- `macos/TeslatlasHubApp/TeslatlasHubApp/OnboardingWindowController.swift` — onboarding state and operational callbacks.
- `macos/TeslatlasHubApp/TeslatlasHubApp/DiagnosticsWindowController.swift` — real diagnostics content/actions.
- `macos/TeslatlasHubApp/TeslatlasHubApp/LogsWindowController.swift` — redacted logs and copy/save behavior.
- `macos/TeslatlasHubApp/TeslatlasHubApp/ServiceDetailsWindowController.swift` — real details and serialized maintenance actions.
- `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift` — shell, snapshot propagation, modal ownership, and workflow locks only.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubController.swift` — operational boundary; only preview selection may change.
- `macos/TeslatlasHubApp/TeslatlasHubApp/AppDelegate.swift` — application/test-host launch policy.

### New focused files

- `macos/TeslatlasHubApp/TeslatlasHubApp/HubOnboardingComponents.swift` — shared wizard header, footer, selection card, vertical field, status card, progress marks, and success medallion; no operational callbacks.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubModalChrome.swift` — common sheet header, dimming/layering, scroll body, close button, and content-height rules.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubPreviewFixtures.swift` — deterministic preview-only scene catalog and inert data.
- `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubPreviewFixturesTests.swift` — preview gating and all-scene coverage.
- `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift` — silent offscreen rendering and PNG emission for component clusters.
- `scripts/test-macos-appkit-focused.sh` — stages the AppKit project, enables silent test-host mode, and runs only explicitly named test selectors.
- `scripts/compare-macos-ui.sh` — validates R01–R12 pairs and generates normalized side-by-side, overlay, heatmap, SSIM, and contact-sheet evidence under `target/design-qa/`.

Do not move operational methods out of `HubController`, `TeslaMateServerImporter`, or `TeslaAuthWindowController`. Do not remove the apparently orphaned `ImportSheetController` as part of this visual plan; that requires a separate behavior-removal decision.

---

### Task 1: Freeze the source truth and establish deterministic preview scenes

**Files:**
- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubPreviewFixtures.swift`
- Create: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubPreviewFixturesTests.swift`
- Create: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/HubController.swift:963-1060`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/OnboardingWindowController.swift:232-320`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift:89-248,1093-1165,1250-1497`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/AppDelegate.swift:78-150`
- Generate ignored evidence: `target/design-qa/reference/manifest.sha256`
- Generate ignored evidence: `target/design-qa/reference/manifest.tsv`

**Interfaces:**
- Produces: `enum HubPreviewScene: String, CaseIterable` with exactly `r01Welcome`, `r02StartChoice`, `r03MigrationForm`, `r04MigrationConnected`, `r05VerificationPassed`, `r06MigrationFinished`, `r07RunningDashboard`, `r08Vehicles`, `r09Diagnostics`, `r10Logs`, `r11ServiceDetails`, and `r12ManageTesla`.
- Produces: `struct HubPreviewFixture { let snapshot: HubSnapshot; let mainSection: HubMainSection; let onboardingRoute: String?; let modal: HubModalKind?; let accountMenuOpen: Bool; let appearance: HubAppearanceMode }`.
- Produces: `MainWindowController.applyPreviewScene(_:)`, plus preview-only Next/Previous Scene menu actions that cycle R01–R12 in one application process.
- Preserves: preview selection is ignored unless `TESLATLAS_HUB_UI_PREVIEW=1`.

- [ ] **Step 1: Write failing catalog/gating tests**

```swift
func testVisualReferenceCatalogContainsEveryPhotographedStateExactlyOnce() {
    XCTAssertEqual(Set(HubPreviewScene.allCases.map(\.rawValue)), Set([
        "r01-welcome", "r02-start-choice", "r03-migration-form",
        "r04-migration-connected", "r05-verification-passed",
        "r06-migration-finished", "r07-running-dashboard", "r08-vehicles",
        "r09-diagnostics", "r10-logs", "r11-service-details", "r12-manage-tesla"
    ]))
}

func testPreviewScenesUseAquaAndContainNoCredentialMaterial() {
    for scene in HubPreviewScene.allCases {
        let fixture = HubPreviewFixtures.fixture(for: scene)
        XCTAssertEqual(fixture.appearance, .light)
        XCTAssertFalse(String(describing: fixture).contains("token"))
        XCTAssertFalse(String(describing: fixture).contains("password"))
    }
}

func testEveryPreviewSceneLeavesOperationalSpiesUntouched() {
    let spies = HubPreviewOperationSpies()
    for scene in HubPreviewScene.allCases {
        let controller = makePreviewController(scene: scene, spies: spies)
        controller.exercisePresentationTransitionsForTesting()
    }
    XCTAssertEqual(spies.totalInvocations, 0)
}
```

Define `HubPreviewOperationSpies` and `makePreviewController(scene:spies:)` as test-only helpers in `HubPreviewFixturesTests.swift`; `exercisePresentationTransitionsForTesting()` may change section, route, checkbox, and modal selection but must never press an operational action.

- [ ] **Step 2: Add the catalog with screenshot-matched underlying states**

Use fixed UUIDs, harmless `.example` values, deterministic dates, and the same two vehicle names across R07–R11. R01–R06 must use the setup-required snapshot. R09–R11 must select Vehicles before presenting their modal. R04 and R06 must keep their acknowledgement checkboxes off and primary buttons disabled. R12 sets `accountMenuOpen = true`; every other fixture sets it to `false`.

- [ ] **Step 3: Make all preview actions inert**

When `previewMode == true`, action closures may move between preview scenes or update visual selection, but must return before calling controller installers, command runners, SSH importers, OAuth, `NSWorkspace`, pasteboard, or panels. Add direct tests that injected operation spies remain at zero calls after every preview-only presentation transition. Install the Next/Previous Scene menu actions only in preview mode; production menus must not expose them.

- [ ] **Step 4: Freeze and hash the references once**

```sh
mkdir -p target/design-qa/reference
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.17.54.png" target/design-qa/reference/R01-welcome.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.17.57.png" target/design-qa/reference/R02-start-choice.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.02.png" target/design-qa/reference/R03-migration-form.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.09.png" target/design-qa/reference/R04-migration-connected.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.18.png" target/design-qa/reference/R05-verification-passed.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.21.png" target/design-qa/reference/R06-migration-finished.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.25.png" target/design-qa/reference/R07-running-dashboard.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.28.png" target/design-qa/reference/R08-vehicles.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.34.png" target/design-qa/reference/R09-diagnostics.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.38.png" target/design-qa/reference/R10-logs.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.41.png" target/design-qa/reference/R11-service-details.png
cp "$HOME/Desktop/Screenshot 2026-09-04 at 13.18.46.png" target/design-qa/reference/R12-manage-tesla.png
shasum -a 256 target/design-qa/reference/R*.png > target/design-qa/reference/manifest.sha256
sips -g pixelWidth -g pixelHeight target/design-qa/reference/R*.png > target/design-qa/reference/dimensions.txt
```

Record window/content/modal crops and allowed masks in `manifest.tsv`; use only uniform scaling and cropping.

- [ ] **Step 5: Review without launching the application**

Run `git diff --check`, inspect the fixture values for credentials and operational calls, and produce a task-scoped diff package. Do not run XCTest or launch the app in this task.

---

### Task 2: Silence the test host and remove modal side effects from tests

**Files:**
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/AppDelegate.swift:78-150`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift:868-1091,1366-1430`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/ServiceDetailsWindowController.swift:183-277`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift:185-345`
- Modify: `macos/TeslatlasHubApp/project.yml:40-64`
- Create: `scripts/test-macos-appkit-focused.sh`

**Interfaces:**
- Produces: `TESLATLAS_HUB_TEST_MODE=1`, which prevents `AppDelegate` from showing, activating, or scheduling a first-run window while preserving normal production launch.
- Produces: injectable confirmation closures returning `NSApplication.ModalResponse`; production defaults still call genuine `NSAlert.runModal()`.
- Produces: `./scripts/test-macos-appkit-focused.sh ClassName[/testMethod]...`; the script copies the current AppKit sources to a temporary project, sets `TESLATLAS_HUB_TEST_MODE=1`, and supplies one `-only-testing:TeslatlasHubAppTests/<selector>` argument for each selector.
- Preserves: the already injected non-modal error presenter.

- [ ] **Step 1: Add failing tests for silent test launch and injected confirmations**

```swift
func testInjectedConfirmationAvoidsApplicationModalSession() {
    var prompts = 0
    let presenter: (NSAlert) -> NSApplication.ModalResponse = { _ in
        prompts += 1
        return .alertSecondButtonReturn
    }
    let controller = makeMainWindowController(confirmationPresenter: presenter)
    controller.requestStopForTesting()
    XCTAssertEqual(prompts, 1)
    XCTAssertNil(NSApp.modalWindow)
}
```

Cover stop, disconnect, uninstall-preserve-data, and delete-data cancellation. No test may call `NSApp.stopModal`.

- [ ] **Step 2: Implement explicit test-host suppression**

In `applicationDidFinishLaunching`, return before cleanup, `showWindow`, `activate`, first-run onboarding, and timers when the test flag is exactly `1`. Do not infer test mode from process names.

In `project.yml`, set this environment variable on the test action so the hosted application receives it deterministically:

```yaml
test:
  environmentVariables:
    TESLATLAS_HUB_TEST_MODE: "1"
```

- [ ] **Step 3: Inject confirmation presentation**

Use a closure at the controller boundary. Keep production construction equivalent to:

```swift
confirmationPresenter: { alert in alert.runModal() }
```

Tests return a selected button without opening a window. Preserve Cancel as the default and Escape target for destructive production alerts.

- [ ] **Step 4: Add the focused-test wrapper**

Copy the staging, XcodeGen, destination, architecture, and code-signing setup from `scripts/test-macos-appkit.sh`. Require at least one selector; reject empty selectors and arguments beginning with `-`; translate each remaining argument to `-only-testing:TeslatlasHubAppTests/$selector`; and invoke `xcodebuild test` once. Keep the temporary-directory cleanup trap and SPDX header.

- [ ] **Step 5: Run one grouped focused test invocation**

```sh
./scripts/test-macos-appkit-focused.sh \
  HubPreviewFixturesTests \
  HubControllerTests/testDeleteDataFailureRestoresTheSharedMutationGate \
  HubControllerTests/testConfirmedDeleteDataActionUsesDeleteDataAndDismissesAfterUnlocking \
  HubControllerTests/testDisconnectConfirmationDefaultsToCancel \
  HubControllerTests/testStopHubConfirmationExplainsCollectionAndDefaultsToCancel \
  HubControllerTests/testPreviewIsReadOnly
```

Expected: the named tests pass; no Teslatlas Hub window or alert becomes visible.

- [ ] **Step 6: Stop immediately on any visible modal**

If any alert appears, terminate the focused run, record the exact test name, and fix the presenter seam before continuing. Do not dismiss it and keep running.

---

### Task 3: Calibrate the shared design system and native window shell

**Files:**
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/HubDesignSystem.swift:5-253`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/HubNavigationBar.swift:15-108`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift:89-248`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDesignSystemTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift:1390-1610`

**Interfaces:**
- Produces: named metrics for shell, toolbar, content column, modal, card, command tile, icon tile, and type roles; route code must not reintroduce ad-hoc copies.
- Produces: dynamic layer colors that refresh in `viewDidChangeEffectiveAppearance()`.
- Preserves: 900 × 630 content size and native traffic lights/titlebar.

- [ ] **Step 1: Correct contradictory token and geometry tests**

Assert the exact light/dark token values from this plan. Remove assertions that simultaneously expect semantic white-level dark cards and `#262628`. Assert initial shell geometry, card radius, button radius, border alpha, navigation selected/unselected surfaces, and appearance persistence.

- [ ] **Step 2: Add the missing navigation symbols and source hierarchy**

Map Dashboard `waveform.path.ecg`, Vehicles `car`, Diagnostics `checkmark.shield`, Logs `doc.text`, Service Details `sun.max`, Import `arrow.down.to.line`, and appearance `moon`/`sun.max`. Keep the five-item rounded group on the left and Import/account actions on the right.

- [ ] **Step 3: Calibrate the shell**

Match the titlebar and toolbar heights, chrome material, hairlines, 12-point outer inset, selected-tab card/shadow, compact typography, and centered title. At minimum width, retain readable labels or introduce a deliberate compact icon-plus-tooltip state; never let persistent controls clip.

- [ ] **Step 4: Remove the dead duplicate view builders after coverage**

Once focused tests prove the mounted Dashboard/Vehicle views and callbacks, remove the unused legacy builders at `MainWindowController.swift:250-495`. Keep orchestration, snapshot propagation, workflow locks, and operation generation logic unchanged.

- [ ] **Step 5: Run one grouped structural test invocation**

```sh
./scripts/test-macos-appkit-focused.sh \
  HubDesignSystemTests \
  HubControllerTests/testMainWindowBuildsInPreviewMode \
  HubControllerTests/testNavigationSeparatesContentSelectionFromModalActions \
  HubControllerTests/testFleetProviderWithoutConnectionShowsConnectTesla
```

Expected: exact token assertions, 900 × 630 geometry, navigation routing, and dynamic appearance refresh pass with no visible windows.

- [ ] **Step 6: Offscreen-render the shell cluster once**

Run `TESLATLAS_HUB_SNAPSHOT_DIR="$PWD/target/design-qa/implementation/shell" ./scripts/test-macos-appkit-focused.sh HubVisualSnapshotTests/testWritesShellDashboardAndVehiclesCluster`. Render R07, R08, and the R12 account-button geometry offscreen at 2×. Do not attempt to rasterize the open system menu and do not make a visible app launch yet. Batch every shell/nav discrepancy found in the combined comparisons before another render.

---

### Task 4: Rebuild the shared onboarding chrome and Steps 1–2

**Files:**
- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubOnboardingComponents.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/OnboardingWindowController.swift:433-723,1080-1293`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift:552-800`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift`

**Interfaces:**
- Produces: one horizontal progress header, content-sized scroll body, shallow footer, vertical choice card, and per-route height table.
- Preserves: first-run non-dismissibility, later-flow cancellation, route/back semantics, busy locking, and existing callbacks.

- [ ] **Step 1: Write exact R01/R02 structural tests**

Assert a 485-point sheet width; horizontal `Step X of 5` and progress marks; no Welcome app icon; four uniform green 16-point checks; exact Welcome copy; two vertical selection cards; hidden Back on Welcome; visible Back on Step 2; and disabled/absent primary action until a choice is made.

- [ ] **Step 2: Build the shared chrome**

Use 28-point horizontal body/header/footer insets, 7-point dots with 21-point active pill, left-aligned 17–17.5-point title, 11.5–12-point subtitle, shallow hairline-separated footer, and content-specific heights. Keep the body scrollable while the header/footer remain fixed.

- [ ] **Step 3: Match R01 Welcome**

Use the exact six lines of Welcome copy listed in the screenshots, four identical `checkmark.circle` symbols, no centered artwork, and a compact trailing Continue button. Correct the current remaining 6–10-point vertical drift and oversized CTA as one batch.

- [ ] **Step 4: Match R02 start choice**

Use two stacked full-width cards with 35-point accent/elevated icon tiles, 12–12.5 semibold titles, 10.5–11-point details, and the exact bottom status `Select an option to continue`.

- [ ] **Step 5: Run one grouped focused test invocation and one offscreen pair capture**

```sh
./scripts/test-macos-appkit-focused.sh \
  OnboardingWindowControllerTests/testWelcomeUsesCompactSourceComposition \
  OnboardingWindowControllerTests/testWelcomeHeaderAndFooterUseTranslatedChromeGeometry \
  OnboardingWindowControllerTests/testSourceCompositionPropagatesSharedChromeAndRouteSpecificSheetHeights \
  OnboardingWindowControllerTests/testChooseUsesExactCopyAndTwoFullWidthVerticalSelectionCards
```

Then render R01 and R02 once, generate combined source/implementation images, and fix only shared P0/P1/P2 findings before marking the cluster passed.

---

### Task 5: Complete Provider, Fleet, Legacy, authorization, and focused operation states

**Files:**
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/OnboardingWindowController.swift:690-741,1295-1409,1731-1926`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/TeslaAuthWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift:777-1096`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift`

**Interfaces:**
- Consumes: shared onboarding chrome/components from Task 4.
- Preserves: secure fields, OAuth ownership, setup invocation construction, credential validation, busy/close locks, error recovery, and provider semantics.

- [ ] **Step 1: Add route-specific structure and gating tests**

Add these exact tests: `testProviderRouteUsesTwoVerticalChoices`, `testFleetRouteUsesCompactVerticalFieldsAndInputGate`, `testLegacyRouteKeepsSecureTokenFieldsAndOAuthEntry`, `testAuthorizationPreviewStatesAreInert`, `testFocusedSetupStateUsesCompactOperationChrome`, and `testInlineErrorUsesSharedDangerCard`. Tests must assert secure controls remain secure and that disabled buttons match input validity.

- [ ] **Step 2: Translate React-only Provider/Fleet/Legacy visuals**

Use the same vertical selection cards and form primitives as the photographed migration route. Match the React visual hierarchy and spacing, but keep production-safe instructions, region values, secure fields, and token meaning.

- [ ] **Step 3: Match focused setup/import presentation**

Keep setup/import operation sheets compact at their measured content height, use left-aligned heading/subtitle, regular-size spinner/progress, and the same 429-point inner body. Do not regress operation locks or callbacks.

- [ ] **Step 4: Keep OAuth as a genuine native child flow**

Apply shared modal chrome around the existing authorization states without simulating a successful Tesla login. Preview routes display inert waiting/success/error fixtures only.

- [ ] **Step 5: Run one grouped focused invocation and one offscreen cluster capture**

```sh
./scripts/test-macos-appkit-focused.sh \
  OnboardingWindowControllerTests/testProviderRouteUsesTwoVerticalChoices \
  OnboardingWindowControllerTests/testFleetRouteUsesCompactVerticalFieldsAndInputGate \
  OnboardingWindowControllerTests/testLegacyRouteKeepsSecureTokenFieldsAndOAuthEntry \
  OnboardingWindowControllerTests/testAuthorizationPreviewStatesAreInert \
  OnboardingWindowControllerTests/testFocusedSetupStateUsesCompactOperationChrome \
  OnboardingWindowControllerTests/testInlineErrorUsesSharedDangerCard \
  OnboardingWindowControllerTests/testMigrationOffersNormalUserKeyPasswordAndPasswordlessSudo \
  OnboardingWindowControllerTests/testNewInstallationBusyStateShowsOnlyCleanSetupFlow \
  OnboardingWindowControllerTests/testBusyOnboardingCannotCloseOrChangeAccountRoute \
  OnboardingWindowControllerTests/testMigrationFormLocksWhileConnecting \
  OnboardingWindowControllerTests/testTunnelFailuresAreSafeAndActionable
```

Capture the React-only state cluster once with `TESLATLAS_HUB_SNAPSHOT_DIR="$PWD/target/design-qa/implementation/account" ./scripts/test-macos-appkit-focused.sh HubVisualSnapshotTests/testWritesProviderFleetLegacyAndAuthorizationCluster`; require no clipping, token drift, inconsistent form geometry, or unsafe mock copy.

---

### Task 6: Match the complete TeslaMate migration journey, R03–R06

**Files:**
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/OnboardingWindowController.swift:742-1078,1138-1224,1412-1623`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift:777-1096`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubPreviewFixturesTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift`

**Interfaces:**
- Consumes: shared onboarding components and inert preview fixtures.
- Preserves: SSH-key/password selection, sudo option, host identity, compatibility result, TeslaMate 4.2 acknowledgement, import progress, failure diagnostics, rollback guidance, and handover acknowledgement.

- [ ] **Step 1: Fix the current migration test/code contradictions**

Restore visible TeslaMate 4.2 and read-only safety meaning required by existing tests. Reduce diagnostic text from the current 480-point width to the 429-point body width so it cannot clip. Keep connected/import eligibility tied to the same connection identity.

- [ ] **Step 2: Match R03 migration form**

Use the two-column Server/Port row, then full-width SSH user and Authentication rows, followed by the sudo checkbox. Match label sizes, field heights, focus treatment, Back placement, and disabled Connect state.

- [ ] **Step 3: Match R04 connected state**

Render one compact success card, the 4.2.0 acknowledgement unchecked, and Import Data disabled. Display the real/safe host value rather than literal `Connected to 1`.

- [ ] **Step 4: Match R05 verification**

Render six photographed success rows using fixed 16-point outcome symbols, compact title/detail hierarchy, View Logs, and enabled Continue. Failure and working fixtures reuse the exact geometry without fake successful checks.

- [ ] **Step 5: Match R06 migration completion**

Render the centered success medallion, exact completion heading/subtitle, unchecked duplicate-request acknowledgement, and disabled Start Hub. Checking the box may enable the preview button visually but must not start a service.

- [ ] **Step 6: Match unphotographed migration failure/progress states**

Use the React danger card and progress hierarchy, but retain bounded diagnostics, rollback guidance, read-only SSH meaning, and credential redaction.

- [ ] **Step 7: Run one grouped migration test invocation and one R03–R06 capture**

```sh
./scripts/test-macos-appkit-focused.sh \
  OnboardingWindowControllerTests/testMigrationFormUsesCompactSourceFieldsAndFooterConnectionGate \
  OnboardingWindowControllerTests/testPreviewFixturesRenderConnectedVerifyAndMigrationFinishSourceStatesWithoutSecrets \
  OnboardingWindowControllerTests/testImportBusyStateShowsOnlyOneHeadingAndDeterminateProgress \
  OnboardingWindowControllerTests/testMigrationUsesExplicitKeyboardNavigationOrder \
  HubControllerTests/testCompatibilityCheckRejectsMissingVersionAcknowledgementBeforeCommands \
  HubControllerTests/testDirectMigrationRejectsMissingVersionAcknowledgementBeforeCommands
```

The grouped tests cover validation, route locking, compatibility acknowledgement, unchanged connection identity, progress mapping, finish acknowledgement, and preview spies at zero operational calls. Capture R03–R06 once and batch all findings before any affected-state recapture.

---

### Task 7: Match Dashboard and Vehicles, R07–R08

**Files:**
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/HubDashboardView.swift:54-335`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/HubVehicleViews.swift:10-216`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift:497-853`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift:1390-1610`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift`

**Interfaces:**
- Preserves: immutable vehicle identity through confirmation, Fleet-only command eligibility, single-flight operation locks, ambiguous-outcome lockout, and one snapshot propagation path.
- Produces: one reusable vehicle card used unchanged by Dashboard and Vehicles.

- [ ] **Step 1: Write exact composition and behavior tests**

Assert the horizontal hero, green status tile, action order, status rows, activity heading outside its card, footer order, Vehicles heading/subtitle, blue car tile, seven equal vertical command tiles, exact `Flash Lights` label, and no enabled commands for unavailable/legacy states.

- [ ] **Step 2: Match R07 Dashboard**

Use the 588-point centered column, horizontal health hero, compact Stop/Restart controls, one vehicle card, three divided status rows, uppercase Latest Activity label, one activity row, and footer with real version plus Service Details/Data Folder actions.

- [ ] **Step 3: Match the vehicle command card**

Use a 35-point blue car tile, real display name/status, optional native selector, seven equal columns, vertically stacked SF Symbol/label treatment, restrained elevated fill, 10–11-point labels, and 7–8-point gaps. Preserve real fields; do not invent model, battery, parking, or location data.

- [ ] **Step 4: Match R08 Vehicles**

Show `2 connected to this Hub`, two separate full-width cards, and the selected Vehicles navigation state. Verify long names truncate without moving the command grid.

- [ ] **Step 5: Cover supplemental real states**

Offscreen-render setup-required, stopped, degraded, starting/stopping/restarting, empty vehicles, legacy provider, multiple vehicles, long names, and minimum width. These use the same components and may not introduce alternate styling.

- [ ] **Step 6: Run one grouped behavior test invocation and one R07–R08 capture**

```sh
./scripts/test-macos-appkit-focused.sh \
  HubControllerTests/testDashboardUsesApprovedCardsAndRealSnapshotValues \
  HubControllerTests/testDashboardDoesNotInventUnavailableVehicleFacts \
  HubControllerTests/testVehiclesPageRendersEveryRealVehicleWithoutMockMetadata \
  HubControllerTests/testVehicleConfirmationKeepsItsOriginalTargetAndStaleTargetIsRejected \
  HubControllerTests/testVehiclePagesShareAcceptedSnapshotEligibility \
  HubControllerTests/testLegacyProviderRejectsVehicleCommandsBeforeRunner
```

Capture R07 and R08 once, compare full-window plus hero/card/nav crops, and batch every shared correction before recapture.

---

### Task 8: Match Diagnostics, Logs, and Service Details, R09–R11

**Files:**
- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubModalChrome.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/DiagnosticsWindowController.swift:21-273`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/LogsWindowController.swift:16-234`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/ServiceDetailsWindowController.swift:25-332`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift:1250-1497`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDiagnosticsPresentationTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift`

**Interfaces:**
- Produces: shared modal header/body/dimming chrome while each controller retains its own data and actions.
- Preserves: one primary modal, mutation retention, diagnostics-not-run-on-open, redacted raw details, redacted copy/save, and safe file writing.

- [ ] **Step 1: Reconcile current modal-size tests with measured targets**

Replace the contradictory Service Details 485 × 350 assertion with the measured 450 × 410 target, adjusted only if a same-state comparison proves native text needs more height. Add exact target tests for Diagnostics 485 × 422 and Logs 554 × 221.

- [ ] **Step 2: Match R09 Diagnostics**

Use compact chrome, seven divided real diagnostic rows, 16-point success/failure symbols, Run Again in the header, and the exact passed-summary hierarchy. Keep working, failure, and expandable redacted raw-report states visually subordinate. Opening the sheet must not start a full diagnostic run.

- [ ] **Step 3: Match R10 Logs**

Use 554-point width, compact Copy/Save header controls, an elevated flat log body, 10.5–11-point SF Mono, aligned two-digit line-number gutter, source-like line spacing, and no row cards. Copy and Save continue to use underlying unnumbered redacted text.

- [ ] **Step 4: Match R11 Service Details**

Use six divided information rows and one compact pale-red danger section. Preserve Update Service if production requires it, but visually subordinate it so the screenshot hierarchy remains dominant. Keep both preserve-data and delete-data uninstall paths and their second critical confirmation.

- [ ] **Step 5: Match layering over Vehicles**

R09–R11 must preserve the R08 page behind the modal, with the photographed dimming/layering. The overlay may use native material, but must not shift, replace, or recolor the underlying page.

- [ ] **Step 6: Run one grouped utility-sheet test invocation and one R09–R11 capture**

```sh
./scripts/test-macos-appkit-focused.sh \
  HubDiagnosticsPresentationTests \
  HubControllerTests/testOpeningDiagnosticsDoesNotRunTheExpensiveChecks \
  HubControllerTests/testDiagnosticsRowsHaveNonzeroDocumentAndRowFramesAfterRendering \
  OnboardingWindowControllerTests/testLogsLineNumbersArePresentationOnly \
  OnboardingWindowControllerTests/testCommandLRedactsServiceSecretsBeforeDisplay \
  HubControllerTests/testPendingServiceDetailsMutationBlocksCloseAndPrimaryModalReplacementUntilCompletion \
  HubControllerTests/testServiceDetailsUseRealSnapshotAndProvider
```

Capture R09–R11 once and batch the whole utility-sheet family before any failed-state recapture.

---

### Task 9: Match Manage Tesla and register native confirmation deviations, R12

**Files:**
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/HubNavigationBar.swift:71-104`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift:1093-1165,1015-1091,1366-1430`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift`

**Interfaces:**
- Preserves: existing provider routes, migration route, disconnect lock, and safe default.
- Uses: native `NSMenu` for platform behavior; its material, shadow, and row metrics are registered P3 deviations.

- [ ] **Step 1: Add exact menu structure tests**

Assert this order and spelling: `Use Fleet API`, `Use Legacy token`, `Migrate from TeslaMate…`, separator, `Disconnect Tesla…`. Assert the menu anchors below the account button, only one menu is open, and the destructive item remains distinct.

- [ ] **Step 2: Match R12 within native-menu limits**

Calibrate account-button size, chevron, menu width, placement, and item ordering. Keep native keyboard dismissal and highlighting instead of drawing a web popover.

- [ ] **Step 3: Register native alerts rather than cloning the React alert**

Keep genuine `NSAlert` for stop, disconnect, uninstall, delete data, and ambiguous vehicle outcomes. Verify construction, exact production safety copy, Cancel default, destructive emphasis, and no auto-retry. Do not include alerts in the 12-reference percentage because no supplied screenshot defines them.

- [ ] **Step 4: Run one grouped menu/confirmation test invocation**

```sh
./scripts/test-macos-appkit-focused.sh \
  HubControllerTests/testAthenaCardShowsControlsAndManageTeslaInsteadOfConnectTesla \
  HubControllerTests/testDisconnectConfirmationDefaultsToCancel \
  HubControllerTests/testStopHubConfirmationExplainsCollectionAndDefaultsToCancel \
  HubControllerTests/testAmbiguousClimateFailureWarnsAgainstRetry
```

Use injected confirmation responses so tests remain silent. Defer the only real R12 capture to the single preview process in Task 11; do not repeatedly reopen the menu during structural work.

---

### Task 10: Dark appearance, minimum geometry, and final component polish

**Files:**
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/HubDesignSystem.swift`
- Modify when a supplemental state proves a defect: `macos/TeslatlasHubApp/TeslatlasHubApp/HubNavigationBar.swift`
- Modify when a supplemental state proves a defect: `macos/TeslatlasHubApp/TeslatlasHubApp/HubDashboardView.swift`
- Modify when a supplemental state proves a defect: `macos/TeslatlasHubApp/TeslatlasHubApp/HubVehicleViews.swift`
- Modify when a supplemental state proves a defect: `macos/TeslatlasHubApp/TeslatlasHubApp/HubOnboardingComponents.swift`
- Modify when a supplemental state proves a defect: `macos/TeslatlasHubApp/TeslatlasHubApp/HubModalChrome.swift`
- Modify when a supplemental state proves a defect: `macos/TeslatlasHubApp/TeslatlasHubApp/DiagnosticsWindowController.swift`
- Modify when a supplemental state proves a defect: `macos/TeslatlasHubApp/TeslatlasHubApp/LogsWindowController.swift`
- Modify when a supplemental state proves a defect: `macos/TeslatlasHubApp/TeslatlasHubApp/ServiceDetailsWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDesignSystemTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubVisualSnapshotTests.swift`

**Interfaces:**
- Preserves: `.system` on first launch; explicit light/dark selection persists; geometry is identical across appearances.

- [ ] **Step 1: Add dynamic-color and persistence assertions**

Assert every layer-backed background, border, status dot, tile, and button refreshes after `viewDidChangeEffectiveAppearance()`. Assert system mode stays unforced and explicit modes restore from `UserDefaults`.

- [ ] **Step 2: Capture the supplemental dark matrix once**

Capture Dashboard, Vehicles, one onboarding form, one utility sheet, and Manage Tesla in dark appearance. These are token/sanity checks, not screenshot-percentage states. Require no stale light CGColors and no geometry changes.

- [ ] **Step 3: Inspect minimum and hostile-content cases once**

Use one supplemental matrix for minimum window width, long vehicle/account names, long paths, long diagnostics, empty states, disabled states, and working states. Require no clipping, inaccessible persistent action, or unexpected sheet growth.

- [ ] **Step 4: Stop at P3 polish**

Fix P0/P1/P2. Record remaining optical P3 items instead of cycling indefinitely on antialiasing, symbol stroke, native shadow, or one-pixel material differences.

---

### Task 11: Run the non-repetitive visual acceptance matrix

**Files:**
- Modify: `docs/maintainers/design-qa.md`
- Create: `scripts/compare-macos-ui.sh`
- Generate ignored evidence: `target/design-qa/implementation/`
- Generate ignored evidence: `target/design-qa/comparisons/`
- Generate ignored evidence: `target/design-qa/contact-sheet.png`
- Modify only failed-state production files if the comparison finds P0/P1/P2 issues

**Interfaces:**
- Consumes: R01–R12 fixtures and all source hashes.
- Produces: normalized implementation images, side-by-side comparisons, 50% overlays, heatmaps, scores, deviation register, and a final contact sheet.

- [ ] **Step 1: Render the offscreen matrix in one silent invocation**

```sh
TESLATLAS_HUB_SNAPSHOT_DIR="$PWD/target/design-qa/implementation/final" \
./scripts/test-macos-appkit-focused.sh \
  HubVisualSnapshotTests/testWritesR01ThroughR12OffscreenMatrix
```

The focused wrapper sets `TESLATLAS_HUB_TEST_MODE=1`. Force 2× output, render every view/sheet body from the deterministic catalog, and save all PNGs without showing or activating the app. This is a selected snapshot renderer, not the full AppKit suite.

- [ ] **Step 2: Normalize without distorting**

Register the app-owned frame and component crops. Downsample or capture at matching density; never stretch width and height independently. Exclude only the documented titlebar/menu/material/antialiasing masks.

- [ ] **Step 3: Generate one comparison package**

For every R state create:

- `Rxx-side-by-side.png`;
- `Rxx-overlay-50.png`;
- `Rxx-heatmap.png`;
- focused crops for navigation, title/body hierarchy, forms, vehicle commands, modal rows, and menu.

Assemble one 12-row contact sheet so the whole design can be reviewed in one inspection.

The script interface is:

```sh
./scripts/compare-macos-ui.sh \
  --reference-dir "$PWD/target/design-qa/reference" \
  --implementation-dir "$PWD/target/design-qa/implementation/final" \
  --output-dir "$PWD/target/design-qa/comparisons"
```

The script must fail on a missing/duplicate R ID, preserve aspect ratio, write `scores.tsv`, and record masks rather than silently excluding pixels. ImageMagick is local QA tooling only and must not become a shipping dependency.

- [ ] **Step 4: Score every state using the 95-point contract**

Record the six category scores, objective tolerances, P0/P1/P2/P3 findings, and registered deviations. A state below 95 or with any actionable P0/P1/P2 remains blocked.

- [ ] **Step 5: Batch corrections and recapture only failed states**

Group shared-token, shell, onboarding, vehicle, and modal fixes before editing. Never recapture a passed unchanged state. Each failed state gets one post-fix capture; further capture is allowed only when its combined comparison still contains an actionable P0/P1/P2.

- [ ] **Step 6: Use exactly one visible preview process for native integration**

Launch the built preview once with `TESLATLAS_HUB_UI_PREVIEW=1` and `TESLATLAS_HUB_PREVIEW_SCENE=r01-welcome`. Cycle R01–R12 with the preview-only Next Scene command in that same process. Capture each distinct state once to verify genuine titlebar, attached-sheet/dimming behavior, and the native menu. Do not click operational controls.

- [ ] **Step 7: Rewrite `docs/maintainers/design-qa.md` from the final combined evidence**

Include source/implementation paths, dimensions and density, viewport, full and focused comparisons, all prior P0/P1/P2 history, per-state score, registered native deviations, interactions inspected, and the exact final line `final result: passed` only when every R01–R12 state passes. Otherwise end with `final result: blocked` and name the failed states.

---

### Task 12: Final behavior verification and handoff

**Files:**
- No planned production edits unless verification exposes a defect
- Update: `.superpowers/sdd/2026-09-04-macos-app-redesign/progress.md`
- Preserve: all `target/design-qa/` evidence

**Interfaces:**
- Consumes: `docs/maintainers/design-qa.md` with `final result: passed`.
- Produces: one final test receipt, one scope receipt, one development build, and the user-visible preview handoff.

- [ ] **Step 1: Perform a fresh read-only source/safety review**

Confirm one refresh authority, mutation locks, modal exclusivity, preview inertness, migration acknowledgements, redaction, credential boundaries, immutable vehicle identity, and cancel-default destructive actions. Confirm `HubController`, `TeslaMateServerImporter`, and `TeslaAuthWindowController` did not acquire visual mock behavior.

- [ ] **Step 2: Run the complete AppKit suite exactly once**

```sh
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer \
TESLATLAS_HUB_TEST_MODE=1 \
./scripts/test-macos-appkit.sh
```

Expected final line: `test-macos-appkit: PASS`. If it fails, the redesign is not complete; fix the named failure and rerun the complete suite once from the beginning.

- [ ] **Step 3: Run final scope and hygiene checks**

```sh
git diff --check
git status --short
git diff --name-only
rg -n "ghp_|AKIA|BEGIN .*PRIVATE KEY|access[_-]?token|refresh[_-]?token" \
  macos/TeslatlasHubApp/TeslatlasHubApp \
  macos/TeslatlasHubApp/TeslatlasHubAppTests
```

Review matches contextually; type names are not credentials. Confirm no changes under `app/`, `src/`, `packaging/`, `dist/`, release scripts, or GitHub automation.

- [ ] **Step 4: Build one unsigned development application without touching `dist/`**

Use an isolated `target/` staging directory and XcodeGen/Xcode-beta:

```sh
preview_root=$(mktemp -d "$PWD/target/macos-ui-final.XXXXXX")
cp macos/TeslatlasHubApp/project.yml "$preview_root/project.yml"
cp -R macos/TeslatlasHubApp/TeslatlasHubApp "$preview_root/TeslatlasHubApp"
cp -R macos/TeslatlasHubApp/TeslatlasHubAppTests "$preview_root/TeslatlasHubAppTests"
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer \
  /opt/homebrew/bin/xcodegen generate --quiet \
  --spec "$preview_root/project.yml" --project "$preview_root"
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer \
  /usr/bin/xcodebuild \
  -project "$preview_root/TeslatlasHubApp.xcodeproj" \
  -scheme TeslatlasHubApp -configuration Debug \
  -derivedDataPath "$preview_root/DerivedData" \
  -destination 'platform=macOS,arch=arm64' \
  ARCHS=arm64 ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO build
```

Record the built app path, binary architecture, minimum macOS version, and build receipt. Do not sign, notarize, package, or publish.

- [ ] **Step 5: Handoff only the verified result**

Report the passed design-QA path, 12-state contact sheet, final full-suite receipt, development app path, registered P3 deviations, and exact files changed. Do not claim release, packaging, deployment, or live-service acceptance.

---

## Execution discipline

Use subagent-driven execution one task at a time, because the user previously selected option 1. Every task receives only its own brief and the global constraints. After implementation, use two read-only gates:

1. specification/behavior review against this task's exact requirements;
2. code-quality/safety review against the dirty working-tree diff package.

Do not run implementation tasks in parallel when they touch shared AppKit files. Read-only reference measurement and comparison generation may run in parallel. Never let two agents edit `MainWindowController.swift`, `OnboardingWindowController.swift`, `HubDesignSystem.swift`, or shared tests at the same time.

## Definition of done

- R01–R12 each score at least 95/100 and have no actionable P0/P1/P2 findings.
- `docs/maintainers/design-qa.md` ends exactly with `final result: passed`.
- React-only states reuse the same tokens/components and have no clipping or unsafe mock behavior.
- Dark and minimum-width supplemental matrices have no P0/P1/P2 findings.
- Preview mode is mechanically inert and test-host UI is silent.
- One final uninterrupted `./scripts/test-macos-appkit.sh` run passes.
- No live service, Tesla, SSH, vehicle, uninstall, data-delete, Finder, pasteboard, or save-panel action was invoked during QA.
- No changes escaped the approved Hub AppKit/test/docs scope.
- No branch, worktree, commit, push, package, release, publish, or deployment action occurred.

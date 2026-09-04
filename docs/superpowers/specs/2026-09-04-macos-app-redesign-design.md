# Teslatlas Hub macOS application redesign

Date: 2026-09-04

## Objective

Replace the current macOS presentation in `macos/TeslatlasHubApp` with the visual system shown in the supplied Figma Make screenshots while keeping the application native AppKit. The downloaded React project is a design and interaction reference only. It will not be embedded, shipped, or used as a source of runtime data.

The redesign covers the main dashboard, the vehicle list, first-run and account onboarding, TeslaMate migration, diagnostics, logs, service details, confirmations, transition states, and light/dark appearance.

## Authority and scope

The inputs have this precedence:

1. Existing native Hub behavior, safety constraints, and live data contracts are authoritative for what the application does.
2. The supplied screenshots are authoritative for visible hierarchy, density, spacing, and overall appearance.
3. The React package fills in visual states that are not captured in the screenshots.
4. Prototype instructions, mock values, timers, version numbers, and simulated outcomes are not product requirements.

The change may reorganize AppKit presentation code and window coordination. It does not change the Rust service, the status protocol, credential handling, vehicle-command semantics, migration semantics, persistence formats, or release artifacts. No React, WebView-based application shell, JavaScript runtime, Tailwind, Lucide, or prototype dependency will be added.

Dedicated VoiceOver work and accessibility-specific acceptance are out of scope. Standard AppKit controls may retain their built-in keyboard and accessibility behavior, but the redesign will not add bespoke accessibility infrastructure or tests.

## Visual system

The application uses genuine macOS window chrome and SF Symbols. It does not draw imitation traffic lights or reproduce the blue Figma presentation canvas.

The default main-window content size remains 900 by 630 points. The minimum width will be raised from 760 points to the smallest measured width that keeps the navigation usable without illegible compression. When vertical space is insufficient, page and modal bodies scroll while primary actions remain visible.

The shared visual system provides:

- system typography with a compact hierarchy for window title, page title, subtitle, section label, row title, secondary detail, and monospaced output;
- dynamic system colors for light and dark appearances;
- white or elevated card surfaces, hairline borders, continuous rounded corners, and restrained shadows;
- primary blue, neutral, flat, destructive, and disabled button treatments;
- status icons and dots for running, stopped, setup-required, degraded, working, success, warning, and failure states;
- reusable card, status-row, field-row, segmented-navigation, progress-dots, modal-header, modal-footer, and empty-state components;
- predictable truncation for compact values, wrapping for explanatory copy, and scrolling for long content.

The appearance control initially follows the macOS system appearance. Once the user selects light or dark, that explicit choice is persisted and restored on later launches.

## Application shell and navigation

The main window always exists, including on first launch. It contains:

- a genuine macOS titlebar with centered `Teslatlas Hub` title and the appearance control;
- a compact navigation row below the titlebar;
- a left segmented group containing Dashboard, Vehicles, Diagnostics, Logs, and Service Details;
- right-side Import and Connect Tesla or Manage Tesla actions;
- a single content area showing Dashboard or Vehicles.

Dashboard and Vehicles are persistent content selections. Diagnostics, Logs, and Service Details are action-like navigation items: selecting one presents its modal over the current content without changing the underlying Dashboard/Vehicles selection. Only one primary modal is presented at a time.

On first run, the main window displays the setup-required dashboard and immediately presents onboarding over it. Later entry points behave as follows:

- Connect Tesla opens provider selection directly.
- Use Fleet API opens the Fleet setup route.
- Use Legacy token opens the Legacy route.
- Import and Migrate from TeslaMate open the migration route.
- Disconnect Tesla opens the existing destructive confirmation and retains its safe default.

The existing application menu and Command-L logs shortcut remain available.

## Dashboard

The dashboard follows the supplied compact centered composition:

1. A hero row shows health icon, title, explanatory subtitle, and state-appropriate service actions.
2. When controllable vehicles exist, a vehicle card shows the selected vehicle and the seven Fleet commands.
3. A grouped card shows Service, Tesla account, and Database status.
4. Latest Activity shows up to three GUI-observed session events or a genuine empty state.
5. The footer shows the real bundled version, Service Details, and Data Folder.

Health titles and actions map to the existing states: running, stopped, setup required, degraded, and starting/stopping/restarting transitions. The controller's existing operation locks, polling, timeout handling, and error paths remain authoritative.

The application does not display prototype account emails, model names, battery percentages, parked/asleep state, or locations because the production status model does not provide them. Vehicle cards use real display names and last-seen status. The bundled application version replaces the prototype's `1.4.0` value.

Latest Activity is non-persistent and contains only actions whose outcomes the GUI observes: successful setup, import, start, stop, restart, account change, disconnect, and vehicle-command acceptance. It does not invent background telemetry or token-refresh events.

## Vehicles

Vehicles is a separate main-window content view. It displays one card per `HubControlVehicle`, using the same vehicle-card and command components as the dashboard.

Fleet-connected vehicles expose Start Climate, Stop Climate, Wake, Lock, Unlock, Flash Lights, and Honk. Legacy-provider and unavailable states retain the current restrictions. Every command keeps the existing confirmation, single-flight behavior, accepted-result explanation, timeout ambiguity warning, and no-retry lockout.

When no vehicles are available, the view explains whether setup, connection, or observations are missing without supplying mock vehicle data.

## Onboarding and account management

Onboarding becomes a native modal sheet over the main window. It preserves the existing state machine:

`welcome -> choose -> provider -> fleet or legacy -> verify -> finish`

or

`welcome -> choose -> migration -> verify -> finish`.

Direct account-management entry may begin at provider, fleet, legacy, or migration while preserving the route's correct step number and back behavior.

The sheet uses a fixed header with `Step X of 5` and five progress marks, a scrollable body, and a fixed footer. The visual layout follows the screenshots for welcome, choice, migration, verification, and completion. The React reference supplies the matching treatment for Fleet, Legacy, authentication, error, and working states that were not photographed.

First-run onboarding cannot be dismissed from inside the sheet. The user can still close the application window. Later account-management onboarding can be cancelled while idle. No onboarding can be dismissed or rerouted during setup, authentication, import, or pending migration handover.

The existing field validation, secure fields, Tesla OAuth window, SSH key selection, password authentication, sudo option, host verification, TeslaMate 4.2 compatibility acknowledgment, determinate import progress, resumable handover state, and verification checks remain intact.

Production safety meaning takes precedence over shorter prototype copy. In particular, the UI must retain the read-only SSH explanation, rollback guidance, TeslaMate version requirements, duplicate-access warning, handover acknowledgment, and warnings against deleting TeslaMate data.

## Diagnostics

Diagnostics appears as a native modal sheet with the screenshot's structured pass/fail rows and Run Again action. The rows are derived from the real diagnostic report rather than simulated values.

A presentation adapter maps known report sections into a structured summary. The complete redacted report remains available in an expandable details area. Copy Report and Save Report remain present, along with the privacy notice explaining redaction and review before sharing. Unknown or newly introduced diagnostic sections remain visible in raw details rather than being discarded.

During execution, the sheet shows a working state and disables conflicting actions. Failure states identify the failed section and keep the report available.

## Logs

Logs appears as a native modal sheet styled like the supplied monospaced log viewer. It refreshes automatically when opened. Visible line numbers are presentation-only; copied and saved data remains the underlying redacted log content.

Copy and Save are primary header actions. Refresh and Run Diagnostics remain available in a compact secondary action area. Existing app/service log combination, operation locking, status messages, redaction, safe file writing, and privacy notice remain unchanged in meaning.

## Service details

Service Details appears as a native modal sheet with structured rows for the real version, service state, provider, account state, database, data folder, and available vehicle summary.

The redesign adds a maintenance section containing Update Service and a danger section containing Uninstall Hub. Uninstall retains both existing choices:

- uninstall the service while preserving data and configuration;
- permanently delete data and configuration only after the existing second critical confirmation.

Service mutations continue to disable conflicting account, service, and vehicle actions until their outcomes are known.

## Modal and alert coordination

The main window owns presentation coordination for onboarding, diagnostics, logs, service details, and confirmation alerts. A primary sheet dims the underlying content. Nested operations such as OAuth, file selection, and destructive confirmations continue to use appropriate native child windows or panels.

The photographed acceptance set does not define confirmation-alert artwork. Confirmations therefore remain genuine native `NSAlert` presentations and are recorded as an intentional AppKit deviation rather than imitating the React alert. Existing safety semantics remain authoritative: Cancel is the default for destructive actions, irreversible actions are visually distinct, ambiguous command outcomes warn against retry, and outside-click dismissal cannot bypass a required decision.

## State and data flow

`HubController` remains the operational boundary. It produces `HubSnapshot` and performs service, account, migration, diagnostics, logs, and vehicle operations.

`MainWindowController` remains the orchestration boundary but delegates visual construction to focused AppKit views and shared components. Snapshot application follows one path:

1. refresh or operation callback produces a `HubSnapshot`;
2. the controller validates that the callback still belongs to the active presentation generation;
3. Dashboard, Vehicles, navigation actions, and open Service Details receive the same snapshot;
4. controls are enabled only when no conflicting operation is active;
5. a confirmed successful GUI-observed action may append a session activity item.

The redesign does not create a second refresh authority. Existing refresh timers, transition polling, generation tokens, and operation guards remain the only sources of refresh and mutation coordination.

## Error handling and safety

All current error outcomes remain visible and actionable. Presentation changes must not convert failure into apparent success or encourage duplicate operations.

- Service transitions retain timeout handling and display the refreshed current state after failure.
- Vehicle-command timeout or ambiguous outcome retains the session lockout and no-retry warning.
- Diagnostics and logs remain redacted before display, copy, or save.
- Migration failures retain bounded diagnostic text and recovery actions without exposing hosts, keys, passwords, or credentials.
- Setup and migration busy states prevent closure and route changes.
- Destructive service and data actions retain separate confirmations and safe default focus.

## Code organization

The implementation may introduce focused files beneath `macos/TeslatlasHubApp/TeslatlasHubApp`, grouped conceptually as:

- shared visual tokens and AppKit components;
- main-window shell and navigation;
- dashboard and vehicle content views;
- modal framing and presentation coordination;
- existing controller files adapted to the shared components.

Operational logic remains in `HubController`, `TeslaMateServerImporter`, and the existing action handlers. Unrelated Rust, packaging, release, documentation, and service work is excluded.

## Verification and acceptance

Verification uses real native builds and deterministic preview/test fixtures. Preview data remains isolated behind the existing preview environment and cannot affect production behavior.

Required checks:

- existing AppKit unit tests pass after being updated for the new view hierarchy;
- new tests cover navigation selection, modal exclusivity, first-run presentation, later-flow cancellation, appearance persistence, snapshot propagation, session activity, structured diagnostic mapping, and retained destructive-action semantics;
- the macOS app builds using the repository's AppKit test/build scripts;
- native screenshots are captured for dashboard states, Vehicles, every onboarding route, migration connection/error/progress, verification success/failure, completion variants, Diagnostics, Logs, Service Details, Manage Tesla, alerts, and light/dark appearance;
- screenshots are compared against the supplied references at matching 900 by 630 point content size;
- long names, paths, diagnostic text, empty states, disabled states, working states, and minimum-window geometry are inspected;
- primary actions are exercised in preview or hermetic test mode without real credentials or live vehicle commands.

Acceptance requires no regression in controller behavior, operation safety, redaction, credential handling, migration handover, command ambiguity handling, or application-menu functionality. Normal differences caused by genuine macOS chrome and controls are acceptable. The redesign is judged on the current development Mac while retaining the macOS 13 deployment target.

Each of the 12 supplied screenshot states must independently achieve at least 95/100 on the agreed weighted visual rubric, with no actionable P0, P1, or P2 finding. A high aggregate score cannot hide one visibly wrong state. Genuine titlebar behavior, SF Pro/SF Symbol rasterization, native menu/material/checkbox/focus-ring metrics, and production-authoritative values or safety copy may remain as documented P3 deviations.

The final handoff includes source changes, tests, native comparison screenshots, and a locally built development application for inspection. Tracked release artifacts in `dist`, commits, pushes, publishing, and deployment remain excluded unless separately authorized.

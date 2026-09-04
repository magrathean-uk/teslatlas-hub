# Teslatlas Hub macOS Application Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current Teslatlas Hub macOS presentation with the approved Figma-derived design while keeping all runtime behavior native AppKit and preserving existing safety semantics.

**Architecture:** Keep `HubController` as the sole operational boundary and `MainWindowController` as the presentation orchestrator. Introduce a small AppKit design system, focused Dashboard and Vehicles views, a navigation bar, modal coordination, and pure presentation adapters for diagnostics, logs, details, appearance, and session activity; then adapt the existing window controllers to these components without changing the Rust/status contract.

**Tech Stack:** Swift 5, AppKit, Foundation, SF Symbols, XCTest, XcodeGen, Xcode-beta, macOS 13 deployment target.

**Spec:** `docs/superpowers/specs/2026-09-04-macos-app-redesign-design.md`

## Global Constraints

- Work only under `hub/`; do not change `app/`, the Rust service contract, packaging inputs, or release artifacts.
- Keep the application native AppKit; add no React, WebView shell, JavaScript, Tailwind, Lucide, or third-party UI dependency.
- Preserve `HubController` as the only refresh and mutation authority; do not introduce another timer or concurrent refresh owner.
- Preserve credential handling, redaction, TeslaMate migration/handover, vehicle-command ambiguity protection, destructive confirmations, and mutation locks.
- Use only real `HubSnapshot` fields in production UI; richer visual fixtures stay behind the existing preview/test environment.
- Keep default main-window content size at 900 by 630 points and retain macOS 13 compatibility.
- Dedicated VoiceOver work and accessibility-specific tests are excluded.
- Do not modify `dist/`, commit, push, publish, deploy, use real credentials, or send live vehicle commands.
- Use `/Applications/Xcode-beta.app/Contents/Developer` through the repository scripts.
- Each task ends with a diff/test checkpoint instead of a commit because commits are not authorized.

---

## Planned file structure

### New production files

- `macos/TeslatlasHubApp/TeslatlasHubApp/HubDesignSystem.swift` — visual tokens, button/card/row primitives, sheet-window construction, and appearance preference.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubDashboardView.swift` — dashboard composition and snapshot/transition rendering.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubVehicleViews.swift` — reusable vehicle card and multi-vehicle page.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubNavigationBar.swift` — Dashboard/Vehicles selection plus diagnostics/logs/details/import/account actions.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubPresentationState.swift` — modal exclusivity, session activity, and small pure presentation types.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubDiagnosticsPresentation.swift` — real report-section parser and structured diagnostic rows.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubConfirmationWindowController.swift` — redesigned confirmation sheet with safe default behavior.

### Modified production files

- `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift` — orchestrate the new shell, views, modal sheets, and existing actions.
- `macos/TeslatlasHubApp/TeslatlasHubApp/AppDelegate.swift` — keep the main window alive during first-run onboarding.
- `macos/TeslatlasHubApp/TeslatlasHubApp/OnboardingWindowController.swift` — compact sheet layout and dismissal policy while retaining its state machine.
- `macos/TeslatlasHubApp/TeslatlasHubApp/DiagnosticsWindowController.swift` — structured summary plus raw redacted details.
- `macos/TeslatlasHubApp/TeslatlasHubApp/LogsWindowController.swift` — redesigned numbered presentation with existing raw copy/save behavior.
- `macos/TeslatlasHubApp/TeslatlasHubApp/ServiceDetailsWindowController.swift` — structured details, maintenance, and danger sections.
- `macos/TeslatlasHubApp/TeslatlasHubApp/HubController.swift` — preview fixtures only; no production parsing or operational changes.

### New and modified tests

- Create `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDesignSystemTests.swift`.
- Create `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubPresentationStateTests.swift`.
- Create `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDiagnosticsPresentationTests.swift`.
- Modify `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift`.
- Modify `macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift`.

`project.yml` already includes the application and test source directories recursively, so new Swift files require no project-file entry.

---

### Task 1: Shared AppKit design system and appearance preference

**Files:**

- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubDesignSystem.swift`
- Create: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDesignSystemTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift`

**Interfaces:**

- Produces: `HubPalette`, `HubMetrics`, `HubButtonStyle`, `HubActionButton`, `HubCardView`, `HubStatusRowView`, `HubSheetStyle`, `HubAppearancePreference`.
- `HubAppearancePreference.init(defaults:key:)`, `.mode`, `.toggle(currentIsDark:)`, and `.apply(to:)` are consumed by Task 4.
- `HubSheetStyle.makeWindow(contentSize:)` is consumed by Tasks 5 through 9.

- [ ] **Step 1: Write appearance and primitive tests**

Add tests using an isolated defaults suite:

```swift
final class HubDesignSystemTests: XCTestCase {
    func testAppearanceStartsAtSystemThenPersistsExplicitToggle() throws {
        let suiteName = "HubDesignSystemTests-\(UUID())"
        let suite = try XCTUnwrap(UserDefaults(suiteName: suiteName))
        defer { suite.removePersistentDomain(forName: suiteName) }
        var preference = HubAppearancePreference(defaults: suite, key: "appearance")

        XCTAssertEqual(preference.mode, .system)
        XCTAssertEqual(preference.toggle(currentIsDark: false), .dark)
        XCTAssertEqual(HubAppearancePreference(defaults: suite, key: "appearance").mode, .dark)
        XCTAssertEqual(preference.toggle(currentIsDark: true), .light)
    }

    func testSheetWindowUsesApprovedContentGeometry() {
        let window = HubSheetStyle.makeWindow(contentSize: NSSize(width: 560, height: 500))
        XCTAssertEqual(window.contentView?.bounds.size, NSSize(width: 560, height: 500))
        XCTAssertFalse(window.styleMask.contains(.resizable))
        XCTAssertEqual(window.backgroundColor, .clear)
    }

    func testCardUsesContinuousRoundedBorderedSurface() {
        let card = HubCardView()
        XCTAssertEqual(card.layer?.cornerRadius, HubMetrics.cardRadius)
        XCTAssertEqual(card.layer?.cornerCurve, .continuous)
        XCTAssertTrue(card.wantsLayer)
    }
}
```

- [ ] **Step 2: Run the AppKit suite and verify the new tests fail**

Run: `./scripts/test-macos-appkit.sh`

Expected: build failure because `HubAppearancePreference`, `HubSheetStyle`, `HubCardView`, and `HubMetrics` do not exist.

- [ ] **Step 3: Implement visual tokens and shared primitives**

Move `HubActionButton` out of `MainWindowController.swift` and define the shared API in `HubDesignSystem.swift`:

```swift
enum HubMetrics {
    static let windowSize = NSSize(width: 900, height: 630)
    static let cardRadius: CGFloat = 12
    static let controlRadius: CGFloat = 8
    static let sheetRadius: CGFloat = 14
    static let contentWidth: CGFloat = 680
    static let pageInset: CGFloat = 24
    static let sectionSpacing: CGFloat = 14
}

enum HubPalette {
    static var card: NSColor { NSColor(name: nil, dynamicProvider: { appearance in
        appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            ? NSColor(deviceWhite: 0.13, alpha: 1)
            : .white
    }) }
    static var elevated: NSColor { .controlBackgroundColor }
    static var hairline: NSColor { .separatorColor }
    static var accent: NSColor { .controlAccentColor }
    static var danger: NSColor { .systemRed }
}

enum HubButtonStyle: Equatable {
    case primary, neutral, flat, flatDanger, destructive
}

final class HubActionButton: NSButton {
    var hubStyle: HubButtonStyle = .neutral { didSet { updateHubAppearance() } }
    override var isEnabled: Bool { didSet { updateHubAppearance() } }
    override var title: String { didSet { updateHubAppearance() } }
    func updateHubAppearance() {
        wantsLayer = true
        isBordered = false
        layer?.cornerRadius = HubMetrics.controlRadius
        layer?.cornerCurve = .continuous
        layer?.borderWidth = hubStyle == .neutral ? 0.5 : 0
        layer?.borderColor = HubPalette.hairline.cgColor
        let foreground: NSColor
        switch hubStyle {
        case .primary:
            layer?.backgroundColor = (isEnabled ? HubPalette.accent : .disabledControlTextColor).cgColor
            foreground = .white
        case .destructive:
            layer?.backgroundColor = (isEnabled ? HubPalette.danger : .disabledControlTextColor).cgColor
            foreground = .white
        case .neutral:
            layer?.backgroundColor = HubPalette.card.cgColor
            foreground = isEnabled ? .labelColor : .disabledControlTextColor
        case .flat:
            layer?.backgroundColor = NSColor.clear.cgColor
            foreground = isEnabled ? .labelColor : .disabledControlTextColor
        case .flatDanger:
            layer?.backgroundColor = NSColor.clear.cgColor
            foreground = isEnabled ? HubPalette.danger : .disabledControlTextColor
        }
        contentTintColor = foreground
        attributedTitle = NSAttributedString(
            string: title,
            attributes: [.foregroundColor: foreground,
                         .font: NSFont.systemFont(ofSize: 13, weight: .medium)]
        )
    }
}

final class HubCardView: NSView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.cornerRadius = HubMetrics.cardRadius
        layer?.cornerCurve = .continuous
        layer?.borderWidth = 0.5
        updateLayer()
    }
    override func updateLayer() {
        layer?.backgroundColor = HubPalette.card.cgColor
        layer?.borderColor = HubPalette.hairline.cgColor
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
}

final class HubStatusRowView: NSView {
    private let valueLabel = NSTextField(labelWithString: "")
    var value: String {
        get { valueLabel.stringValue }
        set { valueLabel.stringValue = newValue }
    }

    init(symbol: String, title: String) {
        super.init(frame: .zero)
        let icon = NSImageView(image: NSImage(systemSymbolName: symbol,
                                             accessibilityDescription: nil) ?? NSImage())
        let titleLabel = NSTextField(labelWithString: title)
        let stack = NSStackView(views: [icon, titleLabel, NSView(), valueLabel])
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 10),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -10)
        ])
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
}

enum HubSheetStyle {
    static func makeWindow(contentSize: NSSize) -> NSWindow {
        let window = NSWindow(contentRect: NSRect(origin: .zero, size: contentSize),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.backgroundColor = .clear
        window.isMovable = false
        return window
    }

    static func inset(_ view: NSView, horizontal: CGFloat, vertical: CGFloat) -> NSView {
        let root = HubCardView()
        view.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(view)
        NSLayoutConstraint.activate([
            view.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: horizontal),
            view.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -horizontal),
            view.topAnchor.constraint(equalTo: root.topAnchor, constant: vertical),
            view.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -vertical)
        ])
        return root
    }
}
```

Extend `HubStatusRowView` with an optional trailing status dot whose color is updated by a `HubStatusTone` value. Call `HubSheetStyle.inset(_:horizontal:vertical:)` when a sheet needs a rounded content root.

- [ ] **Step 4: Implement the appearance preference**

```swift
enum HubAppearanceMode: String, Equatable { case system, light, dark }

struct HubAppearancePreference {
    private let defaults: UserDefaults
    private let key: String
    private(set) var mode: HubAppearanceMode

    init(defaults: UserDefaults = .standard, key: String = "TeslatlasHubAppearance") {
        self.defaults = defaults
        self.key = key
        mode = defaults.string(forKey: key).flatMap(HubAppearanceMode.init(rawValue:)) ?? .system
    }

    @discardableResult
    mutating func toggle(currentIsDark: Bool) -> HubAppearanceMode {
        mode = currentIsDark ? .light : .dark
        defaults.set(mode.rawValue, forKey: key)
        return mode
    }

    func apply(to window: NSWindow) {
        switch mode {
        case .system: window.appearance = nil
        case .light: window.appearance = NSAppearance(named: .aqua)
        case .dark: window.appearance = NSAppearance(named: .darkAqua)
        }
    }
}
```

- [ ] **Step 5: Replace old button styling references and run tests**

Change `.hubAppearance = .flat/.primary` call sites to `.hubStyle = .flat/.primary`, remove the old class declaration from `MainWindowController.swift`, then run `./scripts/test-macos-appkit.sh`.

Expected: `test-macos-appkit: PASS`.

- [ ] **Step 6: Review the task diff**

Run: `git diff --check && git status --short && git diff -- macos/TeslatlasHubApp/TeslatlasHubApp/HubDesignSystem.swift macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDesignSystemTests.swift macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift`

Expected: no whitespace errors; no file outside the listed paths changed.

---

### Task 2: Pure presentation state and session activity

**Files:**

- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubPresentationState.swift`
- Create: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubPresentationStateTests.swift`

**Interfaces:**

- Produces: `HubMainSection`, `HubModalKind`, `HubModalTransition`, `HubModalState`, `HubSessionEvent`, `HubSessionActivityStore`.
- Consumed by Tasks 3 through 9 and `MainWindowController`.

- [ ] **Step 1: Write modal and activity tests**

```swift
final class HubPresentationStateTests: XCTestCase {
    func testModalStateReusesSameKindAndReplacesDifferentKind() {
        var state = HubModalState()
        XCTAssertEqual(state.request(.logs), .present(.logs))
        XCTAssertEqual(state.request(.logs), .reuse(.logs))
        XCTAssertEqual(state.request(.diagnostics), .replace(old: .logs, new: .diagnostics))
        state.dismiss(.diagnostics)
        XCTAssertNil(state.active)
    }

    func testActivityIsNewestFirstBoundedAndRealEventOnly() {
        var store = HubSessionActivityStore(limit: 3, now: { Date(timeIntervalSince1970: 100) })
        store.record(.hubStarted)
        store.record(.hubStopped)
        store.record(.hubRestarted)
        store.record(.accountDisconnected)

        XCTAssertEqual(store.activities.count, 3)
        XCTAssertEqual(store.activities.map(\.message), [
            "Tesla account disconnected", "Hub service restarted", "Hub service stopped"
        ])
    }
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `./scripts/test-macos-appkit.sh`

Expected: build failure for the missing presentation-state types.

- [ ] **Step 3: Implement modal and activity state**

```swift
enum HubMainSection: Equatable { case dashboard, vehicles }
enum HubModalKind: Equatable { case onboarding, diagnostics, logs, serviceDetails }

enum HubModalTransition: Equatable {
    case present(HubModalKind)
    case reuse(HubModalKind)
    case replace(old: HubModalKind, new: HubModalKind)
}

struct HubModalState {
    private(set) var active: HubModalKind?
    mutating func request(_ kind: HubModalKind) -> HubModalTransition {
        if active == kind { return .reuse(kind) }
        if let active {
            let old = active
            self.active = kind
            return .replace(old: old, new: kind)
        }
        active = kind
        return .present(kind)
    }
    mutating func dismiss(_ kind: HubModalKind) {
        if active == kind { active = nil }
    }
}

enum HubSessionEvent: Equatable {
    case hubSetUp, teslaMateImported, hubStarted, hubStopped, hubRestarted
    case accountChanged(HubAccountProvider), accountDisconnected
    case vehicleCommandAccepted(HubVehicleControl, vehicle: String)
}

struct HubSessionActivityStore {
    let limit: Int
    let now: () -> Date
    private(set) var activities: [HubActivity] = []
    mutating func record(_ event: HubSessionEvent) {
        activities.insert(HubActivity(message: event.message, age: "just now", color: event.color), at: 0)
        activities = Array(activities.prefix(limit))
    }
}

private extension HubSessionEvent {
    var message: String {
        switch self {
        case .hubSetUp: return "Hub set up and started"
        case .teslaMateImported: return "Imported TeslaMate history"
        case .hubStarted: return "Hub service started"
        case .hubStopped: return "Hub service stopped"
        case .hubRestarted: return "Hub service restarted"
        case let .accountChanged(provider): return "Now using \(provider.displayName)"
        case .accountDisconnected: return "Tesla account disconnected"
        case let .vehicleCommandAccepted(command, vehicle):
            return "\(command.title) accepted for \(vehicle)"
        }
    }

    var color: NSColor {
        switch self {
        case .hubStopped, .accountDisconnected: return .systemOrange
        case .teslaMateImported, .hubSetUp, .hubStarted, .hubRestarted,
             .accountChanged, .vehicleCommandAccepted: return .systemGreen
        }
    }
}
```

Do not add background telemetry or token-refresh events.

- [ ] **Step 4: Run tests and review**

Run: `./scripts/test-macos-appkit.sh`

Expected: `test-macos-appkit: PASS`.

Run: `git diff --check && git status --short`.

Expected: only the design document, plan, Task 1 files, and the two Task 2 files are changed.

---

### Task 3: Dashboard and reusable vehicle cards

**Files:**

- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubDashboardView.swift`
- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubVehicleViews.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift`

**Interfaces:**

- Consumes: Task 1 primitives and Task 2 activity types.
- Produces: `HubDashboardActions`, `HubDashboardView.apply(snapshot:transition:activity:)`, `HubVehicleCardView.apply(vehicle:provider:enabled:)`, `HubVehiclesView.apply(snapshot:enabled:)`.
- Main-window action closures call the existing controller methods; the views never call `HubController` directly.

- [ ] **Step 1: Replace obsolete dashboard structure assertions with target-structure tests**

Add tests that inspect stable identifiers instead of the old no-card design:

```swift
func testDashboardUsesApprovedCardsAndRealSnapshotValues() throws {
    let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                   initialSnapshot: .previewRunning)
    let windowController = MainWindowController(controller: controller)
    let view = try XCTUnwrap(windowController.window?.contentView)
    let identifiers = descendantViews(in: view).compactMap { $0.identifier?.rawValue }

    XCTAssertTrue(identifiers.contains("hub.dashboard.hero"))
    XCTAssertTrue(identifiers.contains("hub.dashboard.vehicle-card"))
    XCTAssertTrue(identifiers.contains("hub.dashboard.status-card"))
    XCTAssertTrue(labels(in: view).contains { $0.stringValue == HubSnapshot.previewRunning.version })
    XCTAssertFalse(labels(in: view).contains { $0.stringValue.contains("1.4.0") })
}

func testDashboardDoesNotInventUnavailableVehicleFacts() {
    let dashboard = HubDashboardView(actions: .noOp)
    dashboard.apply(snapshot: .previewRunning, transition: nil, activity: [])
    let text = labels(in: dashboard).map(\.stringValue).joined(separator: " ")
    XCTAssertFalse(text.contains("78%"))
    XCTAssertFalse(text.contains("Model 3"))
    XCTAssertFalse(text.contains("Home"))
}
```

Add a recursive `descendantViews(in:)` test helper. Define `.noOp` only in test code as a `HubDashboardActions` value containing empty closures.

- [ ] **Step 2: Run the suite and verify failure**

Run: `./scripts/test-macos-appkit.sh`

Expected: build failure because `HubDashboardView` and `HubDashboardActions` do not exist, followed by obsolete assertion failures once they compile.

- [ ] **Step 3: Implement the vehicle card**

Define callbacks without controller ownership:

```swift
struct HubVehicleCardActions {
    let select: (UUID) -> Void
    let command: (HubVehicleControl, UUID) -> Void
}

final class HubVehicleCardView: HubCardView {
    private let actions: HubVehicleCardActions
    private let nameLabel = NSTextField(labelWithString: "")
    private let statusLabel = NSTextField(labelWithString: "")
    private let selector = NSPopUpButton()
    private var representedVehicles: [HubControlVehicle] = []
    private var commandButtons: [HubVehicleControl: HubActionButton] = [:]

    init(actions: HubVehicleCardActions) {
        self.actions = actions
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("hub.dashboard.vehicle-card")
        selector.target = self
        selector.action = #selector(selectionChanged)
        let titleStack = NSStackView(views: [nameLabel, statusLabel])
        titleStack.orientation = .vertical
        titleStack.alignment = .leading
        let header = NSStackView(views: [titleStack, NSView(), selector])
        header.alignment = .centerY
        let commandStack = NSStackView()
        commandStack.distribution = .fillEqually
        for command in HubVehicleControl.allCases {
            let button = HubActionButton(title: command.title, target: self,
                                         action: #selector(commandPressed(_:)))
            button.identifier = NSUserInterfaceItemIdentifier(command.rawValue)
            commandButtons[command] = button
            commandStack.addArrangedSubview(button)
        }
        let content = NSStackView(views: [header, commandStack])
        content.orientation = .vertical
        content.spacing = 14
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            content.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
            content.topAnchor.constraint(equalTo: topAnchor, constant: 14),
            content.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -14)
        ])
    }
    func apply(vehicle: HubControlVehicle?,
               allVehicles: [HubControlVehicle],
               provider: HubAccountProvider?,
               enabled: Bool) {
        representedVehicles = allVehicles
        selector.removeAllItems()
        selector.addItems(withTitles: allVehicles.map(\.displayName))
        if let vehicle, let index = allVehicles.firstIndex(where: { $0.id == vehicle.id }) {
            selector.selectItem(at: index)
        }
        selector.isHidden = allVehicles.count < 2
        nameLabel.stringValue = vehicle?.displayName ?? "Vehicle"
        statusLabel.stringValue = vehicle?.status ?? "No configured vehicle"
        let controlsEnabled = enabled && provider == .fleet && vehicle != nil
        commandButtons.values.forEach {
            $0.isHidden = provider != .fleet
            $0.isEnabled = controlsEnabled
        }
    }

    @objc private func selectionChanged() {
        guard representedVehicles.indices.contains(selector.indexOfSelectedItem) else { return }
        actions.select(representedVehicles[selector.indexOfSelectedItem].id)
    }

    @objc private func commandPressed(_ sender: NSButton) {
        guard representedVehicles.indices.contains(selector.indexOfSelectedItem),
              let raw = sender.identifier?.rawValue,
              let command = HubVehicleControl(rawValue: raw) else { return }
        actions.command(command, representedVehicles[selector.indexOfSelectedItem].id)
    }
}
```

Give the card identifier `hub.dashboard.vehicle-card`. Use SF Symbols `fan`, `power`, `lock`, `lock.open`, `bolt`, and `speaker.wave.2`; reuse `fan` for both climate actions. Use the existing `HubVehicleControl.rawValue` as each command button identifier so current tests and handlers remain meaningful.

- [ ] **Step 4: Implement the dashboard view**

```swift
struct HubDashboardActions {
    let start: () -> Void
    let stop: () -> Void
    let restart: () -> Void
    let setup: () -> Void
    let diagnostics: () -> Void
    let vehicle: HubVehicleCardActions
    let serviceDetails: () -> Void
    let dataFolder: () -> Void
}

final class HubDashboardView: NSView {
    private let actions: HubDashboardActions
    private let hero = NSStackView()
    private let vehicleCard: HubVehicleCardView
    private let serviceRow = HubStatusRowView(symbol: "gear", title: "Service")
    private let accountRow = HubStatusRowView(symbol: "person", title: "Tesla account")
    private let databaseRow = HubStatusRowView(symbol: "cylinder", title: "Database")
    private let activityStack = NSStackView()

    init(actions: HubDashboardActions) {
        self.actions = actions
        vehicleCard = HubVehicleCardView(actions: actions.vehicle)
        super.init(frame: .zero)
        let statusCard = HubCardView()
        statusCard.identifier = NSUserInterfaceItemIdentifier("hub.dashboard.status-card")
        let statusStack = NSStackView(views: [serviceRow, accountRow, databaseRow])
        statusStack.orientation = .vertical
        let content = NSStackView(views: [hero, vehicleCard, statusCard, activityStack])
        content.orientation = .vertical
        content.spacing = HubMetrics.sectionSpacing
        content.translatesAutoresizingMaskIntoConstraints = false
        statusCard.addSubview(statusStack)
        addSubview(content)
        NSLayoutConstraint.activate([
            content.widthAnchor.constraint(equalToConstant: HubMetrics.contentWidth),
            content.centerXAnchor.constraint(equalTo: centerXAnchor),
            content.topAnchor.constraint(equalTo: topAnchor, constant: HubMetrics.pageInset),
            content.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -HubMetrics.pageInset)
        ])
    }
    func apply(snapshot: HubSnapshot,
               transition: HubServiceTransition?,
               activity: [HubActivity]) {
        serviceRow.value = transition?.service ?? snapshot.service
        accountRow.value = snapshot.accountDisplay
        databaseRow.value = snapshot.database
        vehicleCard.apply(vehicle: snapshot.controlVehicles.first,
                          allVehicles: snapshot.controlVehicles,
                          provider: snapshot.provider,
                          enabled: transition == nil)
        let visibleActivity = Array((activity.isEmpty ? snapshot.activity : activity).prefix(3))
        activityStack.setViews(
            visibleActivity.isEmpty
                ? [NSTextField(labelWithString: "No activity yet.")]
                : visibleActivity.map { NSTextField(labelWithString: "\($0.message)   \($0.age)") },
            in: .top
        )
    }

    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
}
```

Move `HubServiceTransition` from private scope in `MainWindowController.swift` to internal scope in `HubDashboardView.swift`. Keep its current title, subtitle, service label, and symbol mappings. Use identifiers `hub.dashboard.hero`, `hub.dashboard.status-card`, and `hub.dashboard.activity-card`.

The activity renderer uses the provided session activity when non-empty, then the snapshot activity, then `No activity yet.`. It shows no more than three rows.

- [ ] **Step 5: Integrate the dashboard without changing action semantics**

Replace the old `makeContentView()` dashboard tree with one `HubDashboardView`. Keep the current `update()`, generation-token, transition-polling, confirmation, and command methods. Route existing handlers through `HubDashboardActions`; update `applySnapshotPresentation` and `applyServiceTransitionPresentation` to call `dashboardView.apply(...)` instead of writing individual old fields.

Record session activity only in confirmed success branches:

```swift
case .success:
    sessionActivity.record(.hubStarted)
    settleServiceTransition(.starting, expectedHealth: .running, token: token)
```

Do not record errors, issued-but-unconfirmed commands, refreshes, or background events.

- [ ] **Step 6: Run the suite and inspect the dashboard snapshot**

Run: `./scripts/test-macos-appkit.sh`

Expected: `test-macos-appkit: PASS`.

Run: `TESLATLAS_HUB_SNAPSHOT_DIR="$(mktemp -d)/snapshots" ./scripts/test-macos-appkit.sh`

Expected: PASS and a native dashboard snapshot produced by `testSelectedDesignsRenderAtNativeSize`; record the emitted temporary path before the test process exits if the test helper prints it.

- [ ] **Step 7: Review the task diff**

Run: `git diff --check && git diff --stat && git status --short`.

Expected: no Rust, packaging, or `dist/` changes.

---

### Task 4: Vehicles page, navigation bar, and main-window shell

**Files:**

- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubNavigationBar.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/HubVehicleViews.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift`

**Interfaces:**

- Consumes: `HubMainSection`, `HubModalKind`, `HubDashboardView`, and `HubVehicleCardView`.
- Produces: `HubNavigationActions`, `HubNavigationBar.select(_:)`, `HubVehiclesView.apply(snapshot:enabled:)`, and main-window `selectedSection`.

- [ ] **Step 1: Write navigation and multi-vehicle page tests**

```swift
func testNavigationSeparatesContentSelectionFromModalActions() throws {
    let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                   initialSnapshot: .previewRunning)
    let windowController = MainWindowController(controller: controller)

    windowController.selectMainSection(.vehicles)
    XCTAssertEqual(windowController.selectedSection, .vehicles)
    XCTAssertFalse(windowController.vehiclesView.isHidden)
    XCTAssertTrue(windowController.dashboardView.isHidden)

    _ = windowController.showDiagnostics()
    XCTAssertEqual(windowController.selectedSection, .vehicles)
}

func testVehiclesPageRendersEveryRealVehicleWithoutMockMetadata() {
    let first = HubControlVehicle(id: UUID(), displayName: "Aurora", status: "Last seen just now")
    let second = HubControlVehicle(id: UUID(), displayName: "Comet", status: "No observations yet")
    var snapshot = HubSnapshot.previewRunning
    snapshot.controlVehicles = [first, second]
    let view = HubVehiclesView(actions: .noOp)
    view.apply(snapshot: snapshot, enabled: true)

    let text = labels(in: view).map(\.stringValue).joined(separator: " ")
    XCTAssertTrue(text.contains("Aurora"))
    XCTAssertTrue(text.contains("Comet"))
    XCTAssertFalse(text.contains("Model Y"))
}
```

Expose read-only internal `selectedSection`, `dashboardView`, and `vehiclesView` for `@testable` inspection. Provide `.noOp` in test code.

- [ ] **Step 2: Run tests and verify failure**

Run: `./scripts/test-macos-appkit.sh`

Expected: build failure for missing navigation/page interfaces.

- [ ] **Step 3: Implement the navigation bar**

```swift
struct HubNavigationActions {
    let select: (HubMainSection) -> Void
    let diagnostics: () -> Void
    let logs: () -> Void
    let serviceDetails: () -> Void
    let importTeslaMate: () -> Void
    let connectTesla: () -> Void
    let manageTesla: (NSButton) -> Void
}

final class HubNavigationBar: NSView {
    private let actions: HubNavigationActions
    private let dashboardButton = HubActionButton(title: "Dashboard", target: nil, action: nil)
    private let vehiclesButton = HubActionButton(title: "Vehicles", target: nil, action: nil)
    private let accountButton = HubActionButton(title: "Connect Tesla", target: nil, action: nil)
    private let importButton = HubActionButton(title: "Import", target: nil, action: nil)

    init(actions: HubNavigationActions) {
        self.actions = actions
        super.init(frame: .zero)
        let diagnostics = HubActionButton(title: "Diagnostics", target: nil, action: nil)
        let logs = HubActionButton(title: "Logs", target: nil, action: nil)
        let service = HubActionButton(title: "Service Details", target: nil, action: nil)
        let left = NSStackView(views: [dashboardButton, vehiclesButton, diagnostics, logs, service])
        let root = NSStackView(views: [left, NSView(), importButton, accountButton])
        root.translatesAutoresizingMaskIntoConstraints = false
        addSubview(root)
        NSLayoutConstraint.activate([
            root.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            root.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            root.topAnchor.constraint(equalTo: topAnchor, constant: 8),
            root.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8)
        ])
        dashboardButton.target = self; dashboardButton.action = #selector(dashboardPressed)
        vehiclesButton.target = self; vehiclesButton.action = #selector(vehiclesPressed)
        diagnostics.target = self; diagnostics.action = #selector(diagnosticsPressed)
        logs.target = self; logs.action = #selector(logsPressed)
        service.target = self; service.action = #selector(servicePressed)
        importButton.target = self; importButton.action = #selector(importPressed)
        accountButton.target = self; accountButton.action = #selector(accountPressed)
    }
    func select(_ section: HubMainSection) {
        dashboardButton.hubStyle = section == .dashboard ? .neutral : .flat
        vehiclesButton.hubStyle = section == .vehicles ? .neutral : .flat
    }
    func apply(snapshot: HubSnapshot, enabled: Bool) {
        accountButton.title = snapshot.provider == nil ? "Connect Tesla" : "Manage Tesla"
        accountButton.hubStyle = snapshot.provider == nil ? .primary : .neutral
        accountButton.isEnabled = enabled
        importButton.isEnabled = enabled
    }
    @objc private func dashboardPressed() { actions.select(.dashboard) }
    @objc private func vehiclesPressed() { actions.select(.vehicles) }
    @objc private func diagnosticsPressed() { actions.diagnostics() }
    @objc private func logsPressed() { actions.logs() }
    @objc private func servicePressed() { actions.serviceDetails() }
    @objc private func importPressed() { actions.importTeslaMate() }
    @objc private func accountPressed(_ sender: NSButton) {
        sender.title == "Connect Tesla" ? actions.connectTesla() : actions.manageTesla(sender)
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
}
```

Use button identifiers `hub.nav.dashboard`, `hub.nav.vehicles`, `hub.nav.diagnostics`, `hub.nav.logs`, `hub.nav.service`, `hub.nav.import`, and `hub.nav.account`. The first five buttons share one rounded elevated container, but only Dashboard and Vehicles retain selected state.

- [ ] **Step 4: Implement the Vehicles page**

Build a scroll view whose document stack contains the title, real vehicle count, and one `HubVehicleCardView` per `snapshot.controlVehicles`. Reuse the exact command callback and enabling rules from the dashboard. Display an empty card when no vehicles exist.

- [ ] **Step 5: Rebuild the main-window shell**

Set the main content root to a vertical stack containing the navigation row, a separator, and a page container. Configure the genuine titlebar with centered title and appearance button. Keep `HubMetrics.windowSize`; calculate and set a minimum width that fits the measured navigation, with a minimum height of 610 points.

Wire appearance without creating a refresh source:

```swift
@objc private func appearancePressed() {
    let isDark = window?.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
    _ = appearancePreference.toggle(currentIsDark: isDark)
    if let window { appearancePreference.apply(to: window) }
}
```

Both content views receive the same accepted snapshot in `applySnapshotPresentation`. Switching pages performs no controller refresh.

- [ ] **Step 6: Preserve account-menu routes and run tests**

Keep the current native menu labels and route handlers exactly: Use Fleet API, Use Legacy token, Migrate from TeslaMate…, and Disconnect Tesla…. Confirm that Connect Tesla opens `.provider`, Import opens `.migration`, and service/vehicle mutations disable both account actions.

Run: `./scripts/test-macos-appkit.sh`

Expected: `test-macos-appkit: PASS`.

- [ ] **Step 7: Review the task diff**

Run: `git diff --check && git status --short && git diff -- macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift macos/TeslatlasHubApp/TeslatlasHubApp/HubNavigationBar.swift macos/TeslatlasHubApp/TeslatlasHubApp/HubVehicleViews.swift`.

Expected: content selection has no new timer, task, or `HubController.refresh` call.

---

### Task 5: Modal exclusivity and first-run onboarding over the dashboard

**Files:**

- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/AppDelegate.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/OnboardingWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift`

**Interfaces:**

- Consumes: `HubModalState` and `HubSheetStyle`.
- Produces: `HubOnboardingDismissalPolicy`, `MainWindowController.showFirstRunOnboarding()`, and sheet-based `showOnboarding(route:dismissalPolicy:)`.
- Later modal tasks use the same `presentPrimaryModal(kind:controller:)` and `dismissPrimaryModal(kind:)` helpers.

- [ ] **Step 1: Write first-run, dismissal, and modal-exclusivity tests**

```swift
func testFirstRunKeepsDashboardAndPresentsNonDismissibleOnboarding() throws {
    let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"],
                                   initialSnapshot: .firstRun)
    let dashboard = MainWindowController(controller: controller)
    let onboarding = try XCTUnwrap(dashboard.showFirstRunOnboarding())

    XCTAssertNotNil(dashboard.window)
    XCTAssertEqual(dashboard.activeModalKind, .onboarding)
    XCTAssertEqual(onboarding.dismissalPolicy, .firstRun)
    XCTAssertFalse(onboarding.windowShouldClose(try XCTUnwrap(onboarding.window)))
}

func testLaterOnboardingCanDismissOnlyWhileIdle() throws {
    let onboarding = OnboardingWindowController(
        controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
        initialRoute: .provider,
        dismissalPolicy: .accountManagement,
        onComplete: { _ in }
    )
    let window = try XCTUnwrap(onboarding.window)
    XCTAssertTrue(onboarding.windowShouldClose(window))
    onboarding.setBusy(true)
    XCTAssertFalse(onboarding.windowShouldClose(window))
}
```

Update the former idle-close test to construct `.accountManagement` explicitly.

- [ ] **Step 2: Run tests and verify failure**

Run: `./scripts/test-macos-appkit.sh`

Expected: build failure for `HubOnboardingDismissalPolicy`, `showFirstRunOnboarding`, and `activeModalKind`.

- [ ] **Step 3: Implement modal coordination in the main window**

Add one `HubModalState` and helpers:

```swift
private var modalState = HubModalState()
private var activeModalController: NSWindowController?
var activeModalKind: HubModalKind? { modalState.active }

private func presentPrimaryModal(kind: HubModalKind,
                                 controller make: () -> NSWindowController) -> NSWindowController? {
    guard let parent = window else { return nil }
    switch modalState.request(kind) {
    case .reuse:
        activeModalController?.window?.makeKey()
        return activeModalController
    case .replace:
        if let activeWindow = activeModalController?.window { parent.endSheet(activeWindow) }
    case .present:
        break
    }
    let controller = make()
    guard let sheet = controller.window else { return nil }
    activeModalController = controller
    parent.beginSheet(sheet)
    return controller
}
```

The controller-specific close callbacks must call `dismissPrimaryModal(kind:)`, end the sheet once, clear the owned controller, and clear `HubModalState`.

- [ ] **Step 4: Implement dismissal policy and compact sheet geometry**

```swift
enum HubOnboardingDismissalPolicy: Equatable {
    case firstRun
    case accountManagement
}
```

Add `let dismissalPolicy` to `OnboardingWindowController`. Make `windowShouldClose` return false for `.firstRun` and for any existing `interactionBlocked` state. Account-management sheets close while idle.

Replace the 900-by-630 onboarding window with `HubSheetStyle.makeWindow(contentSize: NSSize(width: 560, height: 500))`. Build a fixed header containing `Step X of 5` and five progress marks, a scroll view body no wider than 480 points, and a fixed footer. Keep the route state, fields, validation, async methods, recovery actions, key-file picker, OAuth controller, migration session, progress callbacks, and handover gates unchanged.

- [ ] **Step 5: Change launch behavior without adding a second dashboard refresh**

Replace AppDelegate's close-dashboard/show-separate-onboarding branch:

```swift
showDashboard { [weak self] snapshot in
    guard let self, self.hubController.shouldShowOnboarding(for: snapshot) else { return }
    self.mainWindowController?.showFirstRunOnboarding()
}
```

Remove `AppDelegate.onboardingWindowController`; the main window owns all onboarding after this change. Completion ends the sheet, keeps the main window, applies the current snapshot, and calls `settleStartedHubFromOnboarding()` only for `.hubStarted`.

- [ ] **Step 6: Restyle every onboarding route while preserving production copy**

Use shared cards/rows/fields for Welcome, Choose, Provider, Fleet, Legacy, Migration, Verify, and Finish. Match the screenshots' hierarchy and progress marks. Keep these production-only meanings visible:

- SSH is read-only and TeslaMate remains running and unchanged.
- TeslaMate must be 4.2 or later and the user must make the existing acknowledgment.
- rollback guidance and the final duplicate-access acknowledgment remain present.
- busy/auth/import/handover states cannot close or reroute.

Do not copy prototype email addresses, vehicle facts, versions, timers, or simulated check results.

- [ ] **Step 7: Run focused lifecycle and full AppKit tests**

Run: `./scripts/test-macos-appkit.sh`

Expected: `test-macos-appkit: PASS`, including migration progress, recovery, keyboard order, busy close prevention, and first-run assertions.

- [ ] **Step 8: Review the task diff**

Run: `git diff --check && git status --short`.

Expected: no changes to `TeslaMateServerImporter.swift`, credential operations, migration state persistence, Rust, packaging, or `dist/`.

---

### Task 6: Structured real diagnostics in a redesigned sheet

**Files:**

- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubDiagnosticsPresentation.swift`
- Create: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubDiagnosticsPresentationTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/DiagnosticsWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift`

**Interfaces:**

- Produces: `HubDiagnosticOutcome`, `HubDiagnosticRow`, `HubDiagnosticsPresentation.rows(from:)`.
- Consumed by `DiagnosticsWindowController`; parser does not call the controller or perform diagnostics.

- [ ] **Step 1: Write parser tests for success, failure, and unknown sections**

```swift
final class HubDiagnosticsPresentationTests: XCTestCase {
    func testParsesRealReportSectionsWithoutInventingChecks() {
        let report = """
        == doctor — Hub database, tokens, TLS, collector ==
        Duration: 20 ms
        {"status":"ok"}

        == preflight — selected provider credentials (failed) ==
        Duration: 9 ms
        missing credentials

        == future check ==
        useful detail
        """
        let rows = HubDiagnosticsPresentation.rows(from: report)
        XCTAssertEqual(rows.map(\.title), ["Environment doctor", "Preflight", "Future check"])
        XCTAssertEqual(rows.map(\.outcome), [.passed, .failed, .passed])
        XCTAssertTrue(rows[1].detail.contains("missing credentials"))
    }

    func testEmptyReportProducesNoSyntheticRows() {
        XCTAssertEqual(HubDiagnosticsPresentation.rows(from: ""), [])
    }
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `./scripts/test-macos-appkit.sh`

Expected: build failure for the missing diagnostics presentation types.

- [ ] **Step 3: Implement the section parser**

```swift
enum HubDiagnosticOutcome: Equatable { case passed, failed }

struct HubDiagnosticRow: Equatable {
    let title: String
    let detail: String
    let outcome: HubDiagnosticOutcome
}

enum HubDiagnosticsPresentation {
    static func rows(from report: String) -> [HubDiagnosticRow] {
        let sections = report.components(separatedBy: "\n\n")
        return sections.compactMap(parseSection)
    }
}
```

Parse only blocks whose first line matches `== title ==` or `== title (failed) ==`. Map prefixes `doctor`, `preflight`, `status`, `recent logs`, `service pause`, `service state check`, `service resume`, and `support metadata` to concise display names. For an unknown heading, strip markers, replace repeated whitespace, and capitalize only its first character. Derive failure solely from the explicit `(failed)` marker. Use the first non-duration, non-empty body line as detail and retain the entire redacted report separately.

- [ ] **Step 4: Rebuild DiagnosticsWindowController as a sheet**

Use `HubSheetStyle.makeWindow(contentSize: NSSize(width: 560, height: 520))`. The header contains Diagnostics, Run Again, Copy Report, Save Report, and close. The body contains a scrollable stack of structured rows followed by a collapsed disclosure for the monospaced raw report. The footer retains the redaction/privacy notice.

Opening diagnostics calls only `controller.diagnostics()` for the inexpensive initial summary. Run Again alone calls `runFullDiagnostics`. While running, disable Run/Copy/Save and show the working state. After completion, call `HubShareRedactor.redact`, parse that same safe text, and render rows plus raw details.

- [ ] **Step 5: Route diagnostics through modal coordination**

Expose `@discardableResult func showDiagnostics() -> DiagnosticsWindowController?` on `MainWindowController`. Present `.diagnostics` via `presentPrimaryModal`, replace another open primary sheet, and leave Dashboard/Vehicles selection unchanged.

- [ ] **Step 6: Run tests and review**

Run: `./scripts/test-macos-appkit.sh`

Expected: PASS; `testOpeningDiagnosticsDoesNotRunTheExpensiveChecks` remains green and report redaction/copy/save behavior remains intact.

Run: `git diff --check && git status --short`.

---

### Task 7: Redesigned logs sheet with presentation-only line numbers

**Files:**

- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/LogsWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/AppDelegate.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift`

**Interfaces:**

- Produces: `LogsWindowController.numberedPresentation(_:)` and sheet-based `MainWindowController.showLogs()`.
- Copy and Save continue to consume `latestText`, never the numbered display string.

- [ ] **Step 1: Write line-number and retained-action tests**

```swift
func testLogsLineNumbersArePresentationOnly() {
    XCTAssertEqual(LogsWindowController.numberedPresentation("alpha\nbeta\n"),
                   "01  alpha\n02  beta")
}

func testCommandLLogSheetRetainsRefreshDiagnosticsCopySaveAndPrivacy() throws {
    let logs = LogsWindowController(controller: HubController(
        environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]
    ))
    let titles = buttons(in: logs.window?.contentView).map(\.title)
    XCTAssertTrue(Set(["Refresh", "Run Diagnostics", "Copy", "Save…"]).isSubset(of: Set(titles)))
    XCTAssertTrue(labels(in: logs.window?.contentView).contains {
        $0.stringValue.contains("redact credentials")
    })
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `./scripts/test-macos-appkit.sh`

Expected: missing `numberedPresentation` or layout assertion failure.

- [ ] **Step 3: Implement the redesigned log sheet**

Create the window with `HubSheetStyle.makeWindow(contentSize: NSSize(width: 640, height: 440))`. Put Logs, Copy, Save, and close in the header. Put the monospaced scroll view in the body. Put Refresh, Run Diagnostics, live status, and the existing privacy notice in a compact footer.

Implement numbering:

```swift
static func numberedPresentation(_ text: String) -> String {
    let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
    let visible = lines.last?.isEmpty == true ? Array(lines.dropLast()) : Array(lines)
    let width = max(2, String(visible.count).count)
    return visible.enumerated().map { index, line in
        String(format: "%0*d  %@", width, index + 1, String(line))
    }.joined(separator: "\n")
}
```

Assign `textView.string = Self.numberedPresentation(combined)` for display. Keep `latestText = combined`; Copy and Save use `Self.shareableText(latestText)` exactly as before.

- [ ] **Step 4: Route both toolbar and Command-L logs through the same owner**

Change `MainWindowController.showLogs()` to present `.logs` through the primary modal helper and refresh an already active logs sheet. Change `AppDelegate.showLogs(_:)` to call `mainWindowController?.showLogs()` so the menu shortcut cannot create a second independent logs window. If no main window exists, call `showDashboard()` first and then present logs.

- [ ] **Step 5: Run tests and review**

Run: `./scripts/test-macos-appkit.sh`

Expected: PASS, including redaction-before-display tests and Command-L behavior.

Run: `git diff --check && git status --short`.

---

### Task 8: Structured Service Details with preserved maintenance and deletion flows

**Files:**

- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/ServiceDetailsWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift`

**Interfaces:**

- Produces: `HubServiceDetail`, `ServiceDetailsWindowController.details(for:)`, and sheet-based `MainWindowController.showServiceDetails()`.
- Retains existing `setMutationsEnabled`, `mutationAllowed`, `onMutationStateChanged`, and `onChanged` contracts.

- [ ] **Step 1: Write structured-detail and retained-mutation tests**

```swift
func testServiceDetailsUseRealSnapshotAndProvider() {
    var snapshot = HubSnapshot.previewRunning
    snapshot.provider = .fleet
    let rows = ServiceDetailsWindowController.details(for: snapshot)
    XCTAssertEqual(rows.first { $0.label == "Provider" }?.value, "Fleet API")
    XCTAssertEqual(rows.first { $0.label == "Version" }?.value, snapshot.version)
    XCTAssertFalse(rows.contains { $0.value.contains("1.4.0") })
}

func testServiceDetailsRetainUpdateAndBothUninstallOutcomes() throws {
    let controller = ServiceDetailsWindowController(
        snapshot: .previewRunning,
        controller: HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"]),
        onChanged: {}
    )
    let titles = buttons(in: controller.window?.contentView).map(\.title)
    XCTAssertTrue(titles.contains("Update Service…"))
    XCTAssertTrue(titles.contains("Uninstall Hub…"))
    XCTAssertEqual(ServiceDetailsWindowController.deleteDataConfirmation().buttons.map(\.title),
                   ["Cancel", "Delete Data and Uninstall"])
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `./scripts/test-macos-appkit.sh`

Expected: build failure for `HubServiceDetail` and `details(for:)`.

- [ ] **Step 3: Implement structured rows and redesigned sections**

```swift
struct HubServiceDetail: Equatable {
    let label: String
    let value: String
}

static func details(for snapshot: HubSnapshot) -> [HubServiceDetail] {
    [
        .init(label: "Version", value: snapshot.version),
        .init(label: "Service", value: snapshot.service),
        .init(label: "Provider", value: snapshot.provider?.displayName ?? "Not configured"),
        .init(label: "Tesla account", value: snapshot.accountDisplay),
        .init(label: "Database", value: snapshot.database),
        .init(label: "Data folder", value: snapshot.dataDirectory?.path ?? "Not available")
    ]
}
```

Use a 520-point-wide sheet. Render the rows inside one grouped card, Update Service in a maintenance card, and Uninstall Hub in a pale-red danger card. Long paths truncate in the row but retain their full value as tooltip text.

- [ ] **Step 4: Preserve mutation and destructive semantics**

Keep Update Service calling `controller.installService`. Keep the first uninstall decision offering Uninstall, Keep Data; Delete Data…; Cancel. Keep `deleteDataConfirmation()` unchanged in meaning and safe default. Keep all mutation guards and callbacks so dashboard, account, and vehicle actions remain disabled during service mutation.

- [ ] **Step 5: Present Service Details through modal coordination**

Expose `@discardableResult func showServiceDetails() -> ServiceDetailsWindowController?` and present `.serviceDetails`. Reusing an active sheet updates it from `controller.snapshot`; replacing another modal ends that sheet first.

- [ ] **Step 6: Run tests and review**

Run: `./scripts/test-macos-appkit.sh`

Expected: PASS, including update-service invocation and mutation-owner-gate tests.

Run: `git diff --check && git status --short`.

---

### Task 9: Redesigned confirmation sheets without weakening safety

**Files:**

- Create: `macos/TeslatlasHubApp/TeslatlasHubApp/HubConfirmationWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/MainWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/ServiceDetailsWindowController.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift`

**Interfaces:**

- Produces: `HubConfirmationStyle`, `HubConfirmationModel`, `HubConfirmationWindowController`.
- Callers receive `completion: (Bool) -> Void`, where `true` is returned only from the explicit confirm button.

- [ ] **Step 1: Write model tests for safe defaults and exact safety copy**

```swift
func testDestructiveConfirmationDefaultsToCancel() {
    let model = HubConfirmationModel.stopHub
    XCTAssertEqual(model.title, "Stop collecting vehicle data?")
    XCTAssertTrue(model.message.contains("history stays safe"))
    XCTAssertEqual(model.confirmTitle, "Stop Hub")
    XCTAssertEqual(model.style, .destructive)
    XCTAssertEqual(model.defaultAction, .cancel)
}

func testAmbiguousCommandWarningHasNoRetryAction() {
    let model = HubConfirmationModel.unknownVehicleCommandOutcome
    XCTAssertTrue(model.message.contains("Do not repeat"))
    XCTAssertNil(model.confirmTitle)
}
```

Add equivalent assertions for disconnect, uninstall-keep-data selection, permanent deletion, and vehicle-command confirmation.

- [ ] **Step 2: Run tests and verify failure**

Run: `./scripts/test-macos-appkit.sh`

Expected: build failure for missing confirmation types.

- [ ] **Step 3: Implement the confirmation model and controller**

```swift
enum HubConfirmationStyle: Equatable { case warning, destructive, information }
enum HubConfirmationDefaultAction: Equatable { case cancel, confirm }

struct HubConfirmationModel: Equatable {
    let title: String
    let message: String
    let confirmTitle: String?
    let cancelTitle: String
    let style: HubConfirmationStyle
    let defaultAction: HubConfirmationDefaultAction
}

extension HubConfirmationModel {
    static let stopHub = HubConfirmationModel(
        title: "Stop collecting vehicle data?",
        message: "Hub will stop running. Your existing history stays safe, and you can start Hub again anytime.",
        confirmTitle: "Stop Hub", cancelTitle: "Cancel",
        style: .destructive, defaultAction: .cancel
    )
    static let disconnectTesla = HubConfirmationModel(
        title: "Disconnect Tesla from Hub?",
        message: "Hub will stop and remove its stored Fleet and Legacy credentials. Your collected data stays on this Mac.",
        confirmTitle: "Disconnect", cancelTitle: "Cancel",
        style: .destructive, defaultAction: .cancel
    )
    static let unknownVehicleCommandOutcome = HubConfirmationModel(
        title: "Command outcome unknown",
        message: "Check the vehicle. Do not repeat the command from this app session.",
        confirmTitle: nil, cancelTitle: "OK",
        style: .warning, defaultAction: .cancel
    )
    static func vehicleCommand(_ command: HubVehicleControl, vehicle: String) -> HubConfirmationModel {
        HubConfirmationModel(
            title: "\(command.title) for \(vehicle)?",
            message: "Teslatlas Hub will send this command once.",
            confirmTitle: command.title, cancelTitle: "Cancel",
            style: .warning, defaultAction: .cancel
        )
    }
}

final class HubConfirmationWindowController: NSWindowController {
    private let completion: (Bool) -> Void
    private var completed = false

    init(model: HubConfirmationModel, completion: @escaping (Bool) -> Void) {
        let window = HubSheetStyle.makeWindow(contentSize: NSSize(width: 360, height: 190))
        self.completion = completion
        super.init(window: window)
        let title = NSTextField(labelWithString: model.title)
        title.font = .systemFont(ofSize: 15, weight: .semibold)
        let message = NSTextField(wrappingLabelWithString: model.message)
        message.textColor = .secondaryLabelColor
        let cancel = HubActionButton(title: model.cancelTitle, target: nil, action: nil)
        cancel.hubStyle = .neutral
        cancel.target = self
        cancel.action = #selector(cancelPressed)
        let buttons = NSStackView(views: [NSView(), cancel])
        if let confirmTitle = model.confirmTitle {
            let confirm = HubActionButton(title: confirmTitle, target: self,
                                          action: #selector(confirmPressed))
            confirm.hubStyle = model.style == .destructive ? .destructive : .primary
            buttons.addArrangedSubview(confirm)
        }
        let stack = NSStackView(views: [title, message, buttons])
        stack.orientation = .vertical
        stack.spacing = 14
        window.contentView = HubSheetStyle.inset(stack, horizontal: 22, vertical: 20)
        window.defaultButtonCell = model.defaultAction == .cancel ? cancel.cell : nil
    }

    @objc private func confirmPressed() { finish(true) }
    @objc private func cancelPressed() { finish(false) }
    private func finish(_ confirmed: Bool) {
        guard !completed else { return }
        completed = true
        completion(confirmed)
        close()
    }
    @available(*, unavailable) required init?(coder: NSCoder) { fatalError() }
}
```

Make Cancel the Return-key/default button for every destructive model. Escape and close return false. Outside clicks do nothing. Information-only ambiguous outcome has one `OK` dismissal and no retry action.

- [ ] **Step 4: Replace main-window destructive NSAlert presentation**

Use confirmation sheets for Stop Hub, Disconnect Tesla, and vehicle commands. Preserve current guards before presenting and run the operation only when completion is true. Preserve the accepted-result explanation and ambiguous-outcome no-retry state.

- [ ] **Step 5: Preserve the two-stage uninstall flow**

Present the three-choice uninstall decision using a dedicated variant or retain the native first decision if the new controller supports only two actions. The permanent-delete confirmation must remain a distinct second sheet with Cancel as default. Do not collapse keep-data and delete-data into one confirmation.

- [ ] **Step 6: Run tests and review**

Run: `./scripts/test-macos-appkit.sh`

Expected: PASS, including exact cancel defaults, command ambiguity handling, and service mutation gates.

Run: `git diff --check && git status --short`.

---

### Task 10: Preview fixtures, native screenshot matrix, and final verification

**Files:**

- Modify: `macos/TeslatlasHubApp/TeslatlasHubApp/HubController.swift` (preview fixtures only)
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/OnboardingWindowControllerTests.swift`
- Modify: `macos/TeslatlasHubApp/TeslatlasHubAppTests/HubControllerTests.swift`
- Create: `design-qa.md`

**Interfaces:**

- Consumes every production interface from Tasks 1 through 9.
- Produces deterministic preview snapshots and the blocking visual QA record.

- [ ] **Step 1: Add preview-only fixtures for the acceptance matrix**

Keep `HubSnapshot.firstRun` unchanged in production meaning. Add static preview fixtures containing only non-sensitive samples:

```swift
extension HubSnapshot {
    static let previewStopped: HubSnapshot = { var value = previewRunning; value.health = .stopped; value.service = "Installed and stopped"; return value }()
    static let previewDegraded: HubSnapshot = { var value = previewRunning; value.health = .degraded; value.service = "Installed · needs attention"; return value }()
    static let previewMultipleVehicles: HubSnapshot = { var value = previewRunning; value.controlVehicles = [
        HubControlVehicle(id: UUID(uuidString: "00000000-0000-0000-0000-000000000001")!, displayName: "Aurora", status: "Last seen just now"),
        HubControlVehicle(id: UUID(uuidString: "00000000-0000-0000-0000-000000000002")!, displayName: "Comet", status: "No observations yet")
    ]; return value }()
}
```

These fixtures must be reachable only through `TESLATLAS_HUB_UI_PREVIEW` or tests and must not alter `parseStatus`.

- [ ] **Step 2: Expand the native rendering test**

Update the snapshot test to render named files for:

```swift
let captures: [(String, NSWindow)] = [
    ("dashboard-running-light", runningDashboard),
    ("dashboard-stopped-light", stoppedDashboard),
    ("dashboard-degraded-light", degradedDashboard),
    ("vehicles-light", vehiclesWindow),
    ("onboarding-welcome", welcomeSheet),
    ("onboarding-choose", chooseSheet),
    ("onboarding-provider", providerSheet),
    ("onboarding-fleet", fleetSheet),
    ("onboarding-legacy", legacySheet),
    ("onboarding-migration-key", migrationKeySheet),
    ("onboarding-migration-password", migrationPasswordSheet),
    ("onboarding-verify", verifySheet),
    ("onboarding-finish", finishSheet),
    ("diagnostics", diagnosticsSheet),
    ("logs", logsSheet),
    ("service-details", serviceSheet)
]
```

Add explicit captures for migration connected/error/progress, migration completion, Manage Tesla, representative destructive alerts, and dark Dashboard/Vehicles/onboarding. Render at the approved 900-by-630 parent content size with the sheet attached where applicable.

- [ ] **Step 3: Run the complete AppKit suite fresh**

Run: `./scripts/test-macos-appkit.sh`

Expected: final line `test-macos-appkit: PASS` with no interrupted or extrapolated result.

- [ ] **Step 4: Capture the native screenshot matrix**

Create an explicit temporary destination, then run:

```bash
snapshot_root=$(mktemp -d "${TMPDIR:-/tmp}/teslatlas-redesign-snapshots.XXXXXX")
TESLATLAS_HUB_SNAPSHOT_DIR="$snapshot_root" ./scripts/test-macos-appkit.sh
find "$snapshot_root" -maxdepth 1 -type f -name '*.png' -print | sort
```

Expected: one PNG per named state from Step 2. Keep the path for visual inspection; do not copy screenshots into `dist/`.

- [ ] **Step 5: Compare every captured state with the supplied references**

Open the reference screenshots from `/Users/bolyki/Desktop/` and the matching native captures. Check window geometry, navigation density, selected state, card hierarchy, typography, button emphasis, spacing, sheet dimming, progress marks, field alignment, scrolling, destructive styling, light/dark dynamic colors, and long-content behavior.

Record findings in `design-qa.md` with this exact structure:

```markdown
# Teslatlas Hub macOS redesign QA

## Sources
- Figma Make screenshots supplied on 2026-09-04
- Native AppKit captures from the final fresh test run

## Findings
- P0: none
- P1: none
- P2: none
- P3: native system-control differences only

## Verification
- `./scripts/test-macos-appkit.sh`: passed
- Parent content size: 900 by 630 points
- macOS deployment target: 13.0
- Production mock data introduced: no
- Release artifacts modified: no

final result: passed
```

If any P0, P1, or P2 exists, change `final result` to `blocked`, fix it in the owning task's files, rerun the fresh test and capture commands, and rewrite the report from the new evidence. Do not hand off while the report is blocked.

- [ ] **Step 6: Build a development-only app without touching dist**

Use an explicit generated directory under ignored `target/`:

```bash
preview_root="$PWD/target/macos-ui-preview"
find "$preview_root" -depth -delete 2>/dev/null || true
mkdir -p "$preview_root/project"
cp macos/TeslatlasHubApp/project.yml "$preview_root/project/project.yml"
cp -R macos/TeslatlasHubApp/TeslatlasHubApp "$preview_root/project/TeslatlasHubApp"
cp -R macos/TeslatlasHubApp/TeslatlasHubAppTests "$preview_root/project/TeslatlasHubAppTests"
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer xcodegen generate --quiet --spec "$preview_root/project/project.yml" --project "$preview_root/project"
DEVELOPER_DIR=/Applications/Xcode-beta.app/Contents/Developer xcodebuild -project "$preview_root/project/TeslatlasHubApp.xcodeproj" -scheme TeslatlasHubApp -configuration Debug -derivedDataPath "$preview_root/DerivedData" -destination 'platform=macOS,arch=arm64' ARCHS=arm64 ONLY_ACTIVE_ARCH=YES CODE_SIGNING_ALLOWED=NO build
```

Expected app: `target/macos-ui-preview/DerivedData/Build/Products/Debug/Teslatlas Hub.app`.

- [ ] **Step 7: Launch only in preview mode and inspect primary interactions**

Run:

```bash
TESLATLAS_HUB_UI_PREVIEW=1 \
TESLATLAS_HUB_ONBOARDING_PREVIEW=welcome \
"$PWD/target/macos-ui-preview/DerivedData/Build/Products/Debug/Teslatlas Hub.app/Contents/MacOS/Teslatlas Hub"
```

Exercise Dashboard/Vehicles navigation, each modal, Manage Tesla, appearance toggle, onboarding back/continue, non-destructive preview service transitions, and preview vehicle confirmations. Do not enter credentials or issue commands outside preview mode.

- [ ] **Step 8: Run final repository-scope checks**

Run:

```bash
git diff --check
git status --short
git diff --stat
git diff --name-only | rg '^(dist/|packaging/|src/|Cargo\.|Cargo.lock)' && exit 1 || true
rg -n 'React|Tailwind|lucide|1\.4\.0|gyorgy@teslatlas\.example|Model [3YSX]|78%|54%' macos/TeslatlasHubApp/TeslatlasHubApp
```

Expected: no whitespace errors; no release/Rust/package changes; no prototype framework, fake version, fake email, model, or battery strings in production source. `design-qa.md` ends with `final result: passed`.

---

## Final handoff evidence

The implementation is ready to hand off only when all of the following are present:

- a fresh, uninterrupted `test-macos-appkit: PASS` result;
- `design-qa.md` with `final result: passed`;
- native captures for all listed states;
- a launchable development app under `target/macos-ui-preview/`;
- a clean `git diff --check`;
- no changes under `dist/`, Rust source, Cargo files, packaging, or release automation;
- no commit, push, publish, deployment, real credential use, or live vehicle command.

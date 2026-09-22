// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

enum HubOnboardingPath: Equatable {
    case newInstallation
    case migration
}

enum HubOnboardingProvider: Equatable {
    case fleet
    case legacy
}

enum HubOnboardingCompletion: Equatable {
    case configured
    case hubStarted
}

private enum HubOnboardingOperation {
    case importing
    case setup
}

private final class HubOnboardingChromeView: NSVisualEffectView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        material = .headerView
        blendingMode = .withinWindow
        state = .active
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

final class HubFormPopUpButton: NSPopUpButton {
    static let surfaceCornerRadius: CGFloat = 6

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect, pullsDown: false)
        configureSurface()
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        configureSurface()
    }

    private func configureSurface() {
        isBordered = false
        controlSize = .regular
        font = HubTypography.body
        contentTintColor = HubPalette.foreground
        focusRingType = .default
    }

    override var alignmentRectInsets: NSEdgeInsets { NSEdgeInsetsZero }
    override var focusRingMaskBounds: NSRect { bounds }

    override func drawFocusRingMask() {
        NSBezierPath(roundedRect: bounds,
                     xRadius: Self.surfaceCornerRadius,
                     yRadius: Self.surfaceCornerRadius).fill()
    }

    override func draw(_ dirtyRect: NSRect) {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            let surface = bounds.insetBy(dx: 0.5, dy: 0.5)
            let path = NSBezierPath(roundedRect: surface,
                                    xRadius: Self.surfaceCornerRadius,
                                    yRadius: Self.surfaceCornerRadius)
            NSColor.textBackgroundColor.setFill()
            path.fill()
            HubPalette.border.setStroke()
            path.lineWidth = 1
            path.stroke()
        }
        super.draw(dirtyRect)
    }
}

enum HubOnboardingRoute: Equatable {
    case welcome
    case choose
    case provider
    case fleet
    case legacy
    case migration
    case verify
    case finish
}

struct HubOnboardingState: Equatable {
    var route: HubOnboardingRoute = .welcome
    var path: HubOnboardingPath = .newInstallation
    var provider: HubOnboardingProvider = .fleet

    var step: Int {
        switch route {
        case .welcome: return 1
        case .choose: return 2
        case .provider, .fleet, .legacy, .migration: return 3
        case .verify: return 4
        case .finish: return 5
        }
    }

    mutating func advance() {
        switch route {
        case .welcome:
            route = .choose
        case .choose:
            route = path == .newInstallation ? .provider : .migration
        case .provider:
            route = provider == .fleet ? .fleet : .legacy
        case .fleet, .legacy, .migration:
            route = .verify
        case .verify:
            route = .finish
        case .finish:
            break
        }
    }

    mutating func back() {
        switch route {
        case .choose:
            route = .welcome
        case .provider, .migration:
            route = .choose
        case .fleet, .legacy:
            route = .provider
        case .verify where path == .migration:
            route = .migration
        case .welcome, .verify, .finish:
            break
        }
    }
}

enum HubOnboardingDismissalPolicy: Equatable {
    case firstRun
    case accountManagement
}

final class OnboardingWindowController: NSWindowController, NSWindowDelegate, NSTextFieldDelegate {
    private let controller: HubController
    private let previewRoute: String?
    let dismissalPolicy: HubOnboardingDismissalPolicy
    private let onDismiss: () -> Void
    private let onComplete: (HubOnboardingCompletion) -> Void
    private let closesWindowOnCancel: Bool
    private var didNotifyDismiss = false
    private var state: HubOnboardingState
    private var busy = false
    private var busyMessage: String?
    private var errorMessage: String?
    private var migrationDiagnostic: TeslaMateSSHDiagnostic?
    private var compatibility: HubTeslaMateCompatibility?
    private var checks: [HubOnboardingCheck] = []
    private var verificationFinished = false
    private var handoverAcknowledged = false
    private var authWindow: TeslaAuthWindowController?
    private var logsWindow: LogsWindowController?

    private let continueButton = HubActionButton(title: "Continue", target: nil, action: nil)
    private let backButton = HubActionButton(title: "Back", target: nil, action: nil)
    private let cancelButton = HubActionButton(title: "Cancel", target: nil, action: nil)
    private let spinner = NSProgressIndicator()
    private let migrationSpinner = NSProgressIndicator()
    private let migrationProgress = NSProgressIndicator()
    private let footerSpinner = NSProgressIndicator()
    private var continueWidthConstraint: NSLayoutConstraint?
    private var continueHeightConstraint: NSLayoutConstraint?
    private var backWidthConstraint: NSLayoutConstraint?
    private var footerLogsButton: NSButton?

    private let fleetClientID = NSTextField(string: "")
    private let fleetAccessToken = NSSecureTextField(string: "")
    private let fleetRefreshToken = NSSecureTextField(string: "")
    private let fleetExpiry = NSTextField(string: "3600")
    private let fleetRegion = HubFormPopUpButton(frame: .zero)
    private let legacyAccessToken = NSSecureTextField(string: "")
    private let legacyRefreshToken = NSSecureTextField(string: "")

    private let migrationServer = NSTextField(string: "")
    private let migrationUser = NSTextField(string: "user")
    private let migrationPort = NSTextField(string: "22")
    private let migrationAuthentication = HubFormPopUpButton(frame: .zero)
    private let migrationIdentityFile = NSTextField(string: "")
    private let migrationSSHPassword = NSSecureTextField(string: "")
    private let migrationUseSudo = NSButton(
        checkboxWithTitle: "Use passwordless sudo for Docker access",
        target: nil,
        action: nil
    )
    private let migrationVersionAcknowledgement = NSButton(
        checkboxWithTitle: "I confirm this server runs TeslaMate 4.2.0 or newer",
        target: nil,
        action: nil
    )
    private var migrationConnectButton: NSButton?
    private var migrationKeyViews: [NSView] = []
    private var pageKeyViews: [NSView] = []
    private let migrationSource = NSTextField(string: "")
    private let migrationCarID = NSTextField(string: "")
    private let migrationPasswordFile = NSTextField(string: "")
    private let migrationKeyFile = NSTextField(string: "")
    private var migrationSession: TeslaMateServerImportSession?
    private var connectedMigrationIdentity: String?
    private var onboardingContainer: HubOnboardingContainerView!
    private let headerContentHost = NSView()
    private var renderedStep: Int?
    private var diagnosticView: NSView?
    private var filePanelOpen = false
    private let presentFilePanel: (NSOpenPanel, NSWindow, @escaping (URL?) -> Void) -> Void

    init(controller: HubController,
         resumeMigrationHandoverPhase: HubMigrationHandoverPhase? = nil,
         initialRoute: HubOnboardingRoute? = nil,
         previewRoute: String? = nil,
         dismissalPolicy: HubOnboardingDismissalPolicy = .accountManagement,
         closesWindowOnCancel: Bool = true,
         onDismiss: @escaping () -> Void = {},
         presentFilePanel: @escaping (NSOpenPanel, NSWindow, @escaping (URL?) -> Void) -> Void = { panel, window, completion in
             panel.beginSheetModal(for: window) { response in
                 completion(response == .OK ? panel.url : nil)
             }
         },
         onComplete: @escaping (HubOnboardingCompletion) -> Void) {
        let effectivePreviewRoute = controller.previewMode ? previewRoute : nil
        self.controller = controller
        self.previewRoute = effectivePreviewRoute
        self.dismissalPolicy = dismissalPolicy
        self.closesWindowOnCancel = closesWindowOnCancel
        self.onDismiss = onDismiss
        self.onComplete = onComplete
        self.presentFilePanel = presentFilePanel
        let shouldAutoVerify: Bool
        let resumeMessage: String?
        if let resumeMigrationHandoverPhase {
            let route: HubOnboardingRoute
            switch resumeMigrationHandoverPhase {
            case .importing:
                route = .migration
                shouldAutoVerify = false
                resumeMessage = "The previous import did not finish. Check TeslaMate and run the import again."
            case .awaitingVerification:
                route = .verify
                shouldAutoVerify = true
                resumeMessage = nil
            case .awaitingHandover:
                route = .finish
                shouldAutoVerify = false
                resumeMessage = nil
            }
            state = HubOnboardingState(
                route: route,
                path: .migration,
                provider: .legacy
            )
        } else {
            state = Self.previewState(route: effectivePreviewRoute)
                ?? initialRoute.map { Self.initialState(route: $0) }
                ?? HubOnboardingState()
            shouldAutoVerify = false
            resumeMessage = nil
        }
        let window = HubOnboardingSheetStyle.makeWindow(
            contentSize: HubMetrics.windowSize,
            dismissible: dismissalPolicy == .accountManagement
        )
        window.center()
        super.init(window: window)
        if effectivePreviewRoute == "migration-connected" {
            migrationServer.stringValue = "teslamate.local"
            compatibility = HubTeslaMateCompatibility(
                compatible: true,
                message: "Ready to import.",
                reasonCode: "preview",
                requiredVersion: "4.2.0"
            )
        } else if effectivePreviewRoute == "migration-error" {
            migrationDiagnostic = TeslaMateSSHDiagnostic(
                reasonCode: "preview", title: "TeslaMate connection failed",
                summary: "This account cannot access Docker.",
                suggestions: ["Check the server account and Docker access, then try again."],
                recoveryActions: [.openLogs])
        } else if effectivePreviewRoute == "importing" {
            busy = true
            busyMessage = "Importing data…"
            migrationProgress.isIndeterminate = false
            migrationProgress.minValue = 0
            migrationProgress.maxValue = 1
            migrationProgress.doubleValue = 0.62
        } else if effectivePreviewRoute == "verify-loading" {
            busy = true
        } else if effectivePreviewRoute == "verify" {
            checks = HubController.previewOnboardingChecks
            verificationFinished = true
        }
        window.delegate = self
        errorMessage = resumeMessage
        configureContainer(in: window)
        configureFields()
        render()
        updateWindowCloseAvailability()
        if shouldAutoVerify {
            DispatchQueue.main.async { [weak self] in self?.runVerification() }
        }
    }

    private static func previewState(route: String?) -> HubOnboardingState? {
        switch route {
        case "welcome": return HubOnboardingState(route: .welcome)
        case "choose": return HubOnboardingState(route: .choose)
        case "choose-migration": return HubOnboardingState(route: .choose, path: .migration)
        case "provider": return HubOnboardingState(route: .provider)
        case "fleet": return HubOnboardingState(route: .fleet)
        case "legacy": return HubOnboardingState(route: .legacy, provider: .legacy)
        case "migration", "migration-error": return HubOnboardingState(route: .migration, path: .migration)
        case "migration-connected": return HubOnboardingState(route: .migration, path: .migration)
        case "importing": return HubOnboardingState(route: .migration, path: .migration)
        case "verify", "verify-loading": return HubOnboardingState(route: .verify)
        case "finish": return HubOnboardingState(route: .finish)
        case "finish-migration": return HubOnboardingState(route: .finish, path: .migration)
        default: return nil
        }
    }

    private static func initialState(route: HubOnboardingRoute) -> HubOnboardingState {
        switch route {
        case .migration:
            return HubOnboardingState(route: .migration, path: .migration, provider: .legacy)
        case .legacy:
            return HubOnboardingState(route: .legacy, path: .newInstallation, provider: .legacy)
        case .fleet:
            return HubOnboardingState(route: .fleet, path: .newInstallation, provider: .fleet)
        default:
            return HubOnboardingState(route: route)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    var currentRoute: HubOnboardingRoute { state.route }
    var operationPreventsQuit: Bool { interactionBlocked }
    var canCancel: Bool { dismissalPolicy == .accountManagement && !closeBlocked }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard canCancel else {
            NSSound.beep()
            return false
        }
        return true
    }

    func windowWillClose(_ notification: Notification) {
        resetMigrationProgress()
        migrationSession?.close()
        guard !didNotifyDismiss else { return }
        didNotifyDismiss = true
        onDismiss()
    }

    static func routeChangeAllowed(busy: Bool,
                                   authenticationActive: Bool,
                                   migrationHandoverPending: Bool) -> Bool {
        !busy && !authenticationActive && !migrationHandoverPending
    }

    func navigate(to route: HubOnboardingRoute) {
        guard Self.routeChangeAllowed(
            busy: busy,
            authenticationActive: authWindow != nil,
            migrationHandoverPending: controller.hasPendingMigrationHandover
        ) else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        state = Self.initialState(route: route)
        errorMessage = nil
        compatibility = nil
        migrationVersionAcknowledgement.state = .off
        migrationSession?.close()
        migrationSession = nil
        connectedMigrationIdentity = nil
        checks = []
        verificationFinished = false
        handoverAcknowledged = false
        render()
    }

    private func configureContainer(in window: NSWindow) {
        let header = HubOnboardingChromeView()
        header.identifier = NSUserInterfaceItemIdentifier("onboarding.header")
        let headerLine = NSBox()
        headerLine.boxType = .separator
        headerLine.translatesAutoresizingMaskIntoConstraints = false
        header.addSubview(headerLine)
        headerContentHost.translatesAutoresizingMaskIntoConstraints = false
        header.addSubview(headerContentHost)

        let footer = HubOnboardingChromeView()
        footer.identifier = NSUserInterfaceItemIdentifier("onboarding.footer")
        let footerLine = NSBox()
        footerLine.boxType = .separator
        footerLine.translatesAutoresizingMaskIntoConstraints = false
        footer.addSubview(footerLine)

        NSLayoutConstraint.activate([
            headerContentHost.centerXAnchor.constraint(equalTo: header.centerXAnchor),
            headerContentHost.leadingAnchor.constraint(greaterThanOrEqualTo: header.leadingAnchor, constant: 28),
            headerContentHost.trailingAnchor.constraint(lessThanOrEqualTo: header.trailingAnchor, constant: -28),
            headerContentHost.widthAnchor.constraint(equalToConstant: HubMetrics.onboardingContentWidth),
            headerContentHost.topAnchor.constraint(equalTo: header.topAnchor),
            headerContentHost.bottomAnchor.constraint(equalTo: header.bottomAnchor),
            headerLine.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            headerLine.trailingAnchor.constraint(equalTo: header.trailingAnchor),
            headerLine.bottomAnchor.constraint(equalTo: header.bottomAnchor),
            footerLine.leadingAnchor.constraint(equalTo: footer.leadingAnchor),
            footerLine.trailingAnchor.constraint(equalTo: footer.trailingAnchor),
            footerLine.topAnchor.constraint(equalTo: footer.topAnchor),
        ])

        onboardingContainer = HubOnboardingContainerView(headerView: header, footerView: footer)
        window.contentView = onboardingContainer
    }

    private func configureFields() {
        configureFormPopup(fleetRegion, identifier: "onboarding.fleet-region")
        configureFormPopup(migrationAuthentication, identifier: "onboarding.migration-authentication")
        fleetRegion.addItems(withTitles: [
            "Europe, Middle East and Africa",
            "North America and Asia Pacific",
            "China"
        ])
        migrationAuthentication.addItems(withTitles: ["SSH key", "Password"])
        migrationAuthentication.target = self
        migrationAuthentication.action = #selector(migrationAuthenticationChanged)
        migrationUseSudo.state = .off
        migrationUseSudo.title = ""
        migrationUseSudo.setAccessibilityLabel("This user needs sudo to read the TeslaMate database")
        migrationUseSudo.controlSize = .regular
        migrationVersionAcknowledgement.title = ""
        migrationVersionAcknowledgement.setAccessibilityLabel(
            "I confirm this server runs TeslaMate 4.2.0 or newer"
        )
        migrationVersionAcknowledgement.controlSize = .regular
        migrationVersionAcknowledgement.target = self
        migrationVersionAcknowledgement.action = #selector(migrationVersionAcknowledgementChanged)
        migrationServer.delegate = self
        migrationUser.delegate = self
        migrationPort.delegate = self
        fleetClientID.delegate = self
        fleetAccessToken.delegate = self
        fleetRefreshToken.delegate = self
        fleetExpiry.delegate = self
        for field in [fleetClientID, fleetAccessToken, fleetRefreshToken, fleetExpiry,
                      legacyAccessToken, legacyRefreshToken,
                      migrationServer, migrationUser, migrationPort,
                      migrationIdentityFile, migrationSSHPassword] {
            field.controlSize = .regular
            field.font = HubTypography.body
            field.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
        }
        fleetAccessToken.placeholderString = "Access token"
        fleetRefreshToken.placeholderString = "Refresh token"
        fleetClientID.placeholderString = "Tesla application client ID"
        legacyAccessToken.placeholderString = "Access token"
        legacyRefreshToken.placeholderString = "Refresh token"
        migrationServer.placeholderString = "teslamate.local"
        migrationIdentityFile.placeholderString = "Optional — uses SSH agent or default keys"
        migrationSSHPassword.placeholderString = "SSH password"
    }

    private func configureFormPopup(_ popup: NSPopUpButton, identifier: String) {
        popup.identifier = NSUserInterfaceItemIdentifier(identifier)
        // The stock bezel is shorter than its constrained frame. These remain
        // native pop-up controls; HubFormPopUpButton only paints the same
        // full-height form surface used by the adjacent text fields.
        popup.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
    }

    private func render() {
        guard let window else { return }
        let previousStep = renderedStep
        renderedStep = state.step
        diagnosticView = nil
        let operation = focusedOperation
        if previousStep == nil || !window.isVisible {
            window.setContentSize(currentSheetSize)
        }
        let previousField: NSView? = {
            if let editor = window.firstResponder as? NSTextView,
               let delegate = editor.delegate as? NSView { return delegate }
            return window.firstResponder as? NSView
        }()
        window.initialFirstResponder = nil
        window.makeFirstResponder(nil)
        migrationKeyViews = []
        pageKeyViews = []
        headerContentHost.subviews.forEach { $0.removeFromSuperview() }
        let stepLabel = NSTextField(labelWithString: "Step \(state.step) of 5")
        stepLabel.font = HubTypography.label
        stepLabel.textColor = HubPalette.mutedForeground
        stepLabel.setAccessibilityLabel("Setup progress")
        stepLabel.setAccessibilityValue("Step \(state.step) of 5")
        let headerContent = NSStackView(views: [backButton, spacer(), stepLabel, progressView()])
        headerContent.spacing = 16
        headerContent.alignment = .centerY
        headerContent.translatesAutoresizingMaskIntoConstraints = false
        headerContentHost.addSubview(headerContent)
        NSLayoutConstraint.activate([
            headerContent.leadingAnchor.constraint(equalTo: headerContentHost.leadingAnchor),
            headerContent.trailingAnchor.constraint(equalTo: headerContentHost.trailingAnchor),
            headerContent.centerYAnchor.constraint(equalTo: headerContentHost.centerYAnchor)
        ])

        let content: NSView
        if let operation {
            content = operationBody(operation)
        } else {
            let pageHeader = onboardingPageHeader()
            let body = pageBody()
            let page = NSStackView(views: [pageHeader, body])
            page.identifier = NSUserInterfaceItemIdentifier(
                state.route == .welcome ? "onboarding.welcome.body" : "onboarding.body"
            )
            page.orientation = .vertical
            page.alignment = .leading
            page.spacing = 24
            NSLayoutConstraint.activate([
                pageHeader.widthAnchor.constraint(equalTo: page.widthAnchor),
                body.widthAnchor.constraint(equalTo: page.widthAnchor)
            ])
            content = page
        }
        content.translatesAutoresizingMaskIntoConstraints = false

        let footerContent = footerView()
        onboardingContainer.replaceBody(content)
        onboardingContainer.replaceFooterContent(footerContent)
        updateFooter()
        window.recalculateKeyViewLoop()
        configureKeyViewLoop(previousField: previousField)
        onboardingContainer.alphaValue = 1
        if let diagnosticView {
            onboardingContainer.reveal(diagnosticView)
        }
        if let previousStep {
            HubMotion.transition(onboardingContainer.bodyDocumentView,
                                 forward: previousStep == state.step ? nil : state.step > previousStep)
        }
    }

    private var currentSheetSize: NSSize { HubMetrics.windowSize }

    private var focusedOperation: HubOnboardingOperation? {
        if busyMessage == "Importing data…" { return .importing }
        if state.path == .newInstallation, busyMessage == "Setting up Hub…" { return .setup }
        return nil
    }

    private func operationBody(_ operation: HubOnboardingOperation) -> NSView {
        switch operation {
        case .importing:
            return migrationProgressBody()
        case .setup:
            spinner.style = .spinning
            spinner.controlSize = .regular
            spinner.startAnimation(nil)
            let title = NSTextField(labelWithString: "Setting up Hub…")
            title.font = HubTypography.heading
            title.textColor = HubPalette.foreground
            let subtitle = NSTextField(labelWithString: "Saving your connection and preparing Hub.")
            subtitle.font = HubTypography.body
            subtitle.textColor = HubPalette.mutedForeground
            let stack = NSStackView(views: [title, subtitle, spinner])
            stack.orientation = .vertical
            stack.alignment = .leading
            stack.spacing = 0
            stack.setCustomSpacing(4, after: title)
            stack.setCustomSpacing(14, after: subtitle)
            return stack
        }
    }

    private var pageTitle: String {
        switch state.route {
        case .welcome: return "Welcome to Teslatlas Hub"
        case .choose: return "How would you like to start?"
        case .provider: return "Choose how Hub connects"
        case .fleet: return "Set up Fleet Telemetry"
        case .legacy: return "Connect with a token"
        case .migration: return "Migrate from TeslaMate"
        case .verify: return "Checking your Hub"
        case .finish: return state.path == .migration ? "Migration complete" : "Teslatlas Hub is ready"
        }
    }

    private var pageSubtitle: String {
        switch state.route {
        case .welcome:
            return "Your own Tesla telemetry collector, running privately on this Mac."
        case .choose:
            return "Set up a fresh Hub or bring your history over from TeslaMate."
        case .provider:
            return "Fleet Telemetry is recommended. Legacy tokens work with older setups."
        case .fleet:
            return "Create a Tesla Fleet application, then paste its credentials below."
        case .legacy:
            return "Sign in with Tesla, or paste an existing token pair."
        case .migration:
            return "Connect to your TeslaMate server to import its vehicle history."
        case .verify:
            return "Making sure everything is wired up correctly."
        case .finish:
            return state.path == .migration
                ? "Your TeslaMate history has been imported into Hub."
                : "Hub is set up and ready to start collecting vehicle data."
        }
    }

    private var pageSymbol: String {
        switch state.route {
        case .welcome: return "checkmark.shield"
        case .choose: return "arrow.triangle.branch"
        case .provider, .fleet: return "antenna.radiowaves.left.and.right"
        case .legacy: return "key"
        case .migration: return "square.and.arrow.down"
        case .verify: return "stethoscope"
        case .finish: return "checkmark.circle"
        }
    }

    private func onboardingPageHeader() -> NSView {
        let tile = HubIconTileView(symbol: pageSymbol, accessibilityDescription: pageTitle,
                                   size: 48, symbolSize: 24, weight: .medium,
                                   fill: .elevated,
                                   tint: state.route == .welcome ? HubPalette.success : .labelColor,
                                   radius: 12)
        let title = NSTextField(labelWithString: pageTitle)
        title.font = HubTypography.heading
        title.textColor = HubPalette.foreground
        title.alignment = .left
        let subtitle = NSTextField(wrappingLabelWithString: pageSubtitle)
        subtitle.font = .systemFont(ofSize: 14)
        subtitle.textColor = HubPalette.mutedForeground
        subtitle.alignment = .left
        subtitle.maximumNumberOfLines = 2
        let copy = NSStackView(views: [title, subtitle])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2
        if [.welcome, .choose, .provider, .finish].contains(state.route) {
            title.alignment = .center
            subtitle.alignment = .center
            copy.alignment = .centerX
            let column = NSStackView(views: [tile, copy])
            column.orientation = .vertical
            column.alignment = .centerX
            column.spacing = 12
            copy.widthAnchor.constraint(equalTo: column.widthAnchor).isActive = true
            return column
        }
        let row = NSStackView(views: [tile, copy])
        row.spacing = 16
        row.alignment = .centerY
        return row
    }

    private func pageBody() -> NSView {
        switch state.route {
        case .welcome: return welcomeBody()
        case .choose: return chooseBody()
        case .provider: return providerBody()
        case .fleet: return fleetBody()
        case .legacy: return legacyBody()
        case .migration: return migrationBody()
        case .verify: return verifyBody()
        case .finish: return finishBody()
        }
    }

    private func welcomeBody() -> NSView {
        let card = HubCardView()
        let rows = [
            onboardingFeatureRow(symbol: "car.side", title: "Connect your Tesla",
                                 subtitle: "Collect vehicle data in the background."),
            onboardingFeatureRow(symbol: "cylinder", title: "Keep your history here",
                                 subtitle: "Store data locally on this Mac."),
            onboardingFeatureRow(symbol: "square.and.arrow.down", title: "Bring your existing history",
                                 subtitle: "Import from TeslaMate when you are ready.")
        ]
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.spacing = 0
        for (index, row) in rows.enumerated() {
            if index > 0 { stack.addArrangedSubview(separator()) }
            stack.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            stack.topAnchor.constraint(equalTo: card.topAnchor),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor)
        ])
        return card
    }

    private func onboardingFeatureRow(symbol: String, title: String, subtitle: String) -> NSView {
        let tile = HubIconTileView(symbol: symbol, accessibilityDescription: title)
        tile.imageView.identifier = NSUserInterfaceItemIdentifier("onboarding.welcome.feature-icon")
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 14, weight: .medium)
        label.textColor = HubPalette.foreground
        let detail = NSTextField(labelWithString: subtitle)
        detail.font = .systemFont(ofSize: 12)
        detail.textColor = .secondaryLabelColor
        let copy = NSStackView(views: [label, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 1
        let row = NSStackView(views: [tile, copy, spacer()])
        row.edgeInsets = NSEdgeInsets(top: 10, left: HubMetrics.rowHorizontalInset,
                                     bottom: 10, right: HubMetrics.rowHorizontalInset)
        row.spacing = 12
        row.alignment = .centerY
        row.heightAnchor.constraint(equalToConstant: HubMetrics.rowHeight).isActive = true
        return row
    }

    private func chooseBody() -> NSView {
        let fresh = choiceButton(title: "New installation",
                                 subtitle: "Start with a fresh database.",
                                 selected: state.path == .newInstallation,
                                 action: #selector(selectNewInstallation))
        let migration = choiceButton(title: "Migrate from TeslaMate",
                                     subtitle: "Bring your existing vehicle history.",
                                     selected: state.path == .migration,
                                     action: #selector(selectMigration))
        return verticalChoices([fresh, migration])
    }

    private func providerBody() -> NSView {
        let fleet = choiceButton(title: "Fleet Telemetry",
                                 subtitle: "Tesla's official streaming API. Enables live vehicle commands.",
                                 selected: state.provider == .fleet,
                                 action: #selector(selectFleet))
        let legacy = choiceButton(title: "Legacy Token",
                                  subtitle: "Use an owner-API access and refresh token pair.",
                                  selected: state.provider == .legacy,
                                  action: #selector(selectLegacy))
        return verticalChoices([fleet, legacy])
    }

    private func fleetBody() -> NSView {
        let guide = HubActionButton(title: "Create Tesla Fleet App", target: self, action: #selector(openFleetGuide))
        configureFlatButton(guide, symbol: "book")
        fleetExpiry.widthAnchor.constraint(equalToConstant: 120).isActive = true
        let seconds = NSTextField(labelWithString: "seconds")
        seconds.font = HubTypography.body
        seconds.textColor = HubPalette.mutedForeground
        let expiryControl = NSStackView(views: [fleetExpiry, seconds, spacer()])
        expiryControl.alignment = .centerY
        expiryControl.spacing = 12
        let form = onboardingForm([
            ("Region", fleetRegion),
            ("Client ID", fleetClientID),
            ("Access token", fleetAccessToken),
            ("Refresh token", fleetRefreshToken),
            ("Expires in", expiryControl)
        ])
        let note = featureRow("Credentials are encrypted on this Mac", "lock.fill")
        let fields = NSStackView(views: [guide, form, note])
        fields.orientation = .vertical
        fields.alignment = .leading
        fields.spacing = 16
        for field in fields.arrangedSubviews.dropFirst() {
            field.widthAnchor.constraint(equalTo: fields.widthAnchor).isActive = true
        }
        return withError(fields)
    }

    private func legacyBody() -> NSView {
        let signIn = HubActionButton(title: "Sign in with Tesla", target: self, action: #selector(startLegacySignIn))
        configurePrimaryButton(signIn, symbol: "person.crop.circle.badge.checkmark")
        let or = NSTextField(labelWithString: "or use an existing token pair")
        or.textColor = .secondaryLabelColor
        or.alignment = .center
        let form = onboardingForm([
            ("Access token", legacyAccessToken),
            ("Refresh token", legacyRefreshToken)
        ])
        let stack = NSStackView(views: [signIn, or, form,
                                       featureRow("Tokens are encrypted on this Mac", "lock.fill")])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        for field in stack.arrangedSubviews.dropFirst(2) {
            field.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private func migrationBody() -> NSView {
        if busyMessage == "Importing data…" {
            return migrationProgressBody()
        }
        if isPreviewConnectedMigration {
            return connectedMigrationPreviewBody()
        }
        if let migrationSession {
            return connectedMigrationBody(migrationSession)
        }
        migrationPort.widthAnchor.constraint(equalToConstant: 72).isActive = true
        let portLabel = NSTextField(labelWithString: "Port")
        portLabel.font = HubTypography.label
        portLabel.textColor = HubPalette.mutedForeground
        let serverPort = NSStackView(views: [migrationServer, portLabel, migrationPort])
        serverPort.spacing = 10
        serverPort.alignment = .centerY
        serverPort.distribution = .fill
        migrationServer.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        var fields: [(String, NSView)] = [("Server", serverPort),
                                         ("SSH user", migrationUser),
                                         ("Authentication", migrationAuthentication)]
        migrationKeyViews = [migrationServer, migrationUser, migrationPort, migrationAuthentication]
        if migrationAuthentication.indexOfSelectedItem == 0 {
            let choose = HubActionButton(title: "Choose Key…", target: self, action: #selector(chooseMigrationIdentity))
            choose.identifier = NSUserInterfaceItemIdentifier("onboarding.choose-ssh-key")
            choose.hubFont = HubTypography.action
            choose.setContentCompressionResistancePriority(.required, for: .horizontal)
            let keyRow = NSStackView(views: [migrationIdentityFile, choose])
            keyRow.alignment = .centerY
            keyRow.spacing = 8
            migrationIdentityFile.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            fields.append(("SSH key", keyRow))
            migrationKeyViews.append(contentsOf: [migrationIdentityFile, choose])
        } else {
            fields.append(("Password", migrationSSHPassword))
            migrationKeyViews.append(migrationSSHPassword)
        }
        var views: [NSView] = [onboardingForm(fields)]
        views.append(wrappingCheckbox(
            migrationUseSudo,
            title: "This user needs sudo to read the TeslaMate database"
        ))
        migrationKeyViews.append(migrationUseSudo)

        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 7
        for view in views {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        if let compatibility {
            let status = featureRow(compatibility.message,
                                    compatibility.compatible ? "checkmark.circle.fill" : "exclamationmark.triangle.fill",
                                    color: compatibility.compatible ? .systemGreen : .systemOrange)
            stack.addArrangedSubview(status)
            status.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        if let migrationDiagnostic {
            let diagnostic = migrationDiagnosticView(migrationDiagnostic)
            diagnosticView = diagnostic
            stack.addArrangedSubview(diagnostic)
            diagnostic.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private func connectedMigrationBody(_ session: TeslaMateServerImportSession) -> NSView {
        migrationConnectButton = nil
        let host = migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        var views: [NSView] = [migrationSuccessCard(host: host)]
        if let version = session.teslaMateVersion {
            views.append(featureRow("TeslaMate \(version)", "info.circle", color: HubPalette.mutedForeground))
        }
        views.append(wrappingCheckbox(
            migrationVersionAcknowledgement,
            title: "I confirm this server runs TeslaMate 4.2.0 or newer"
        ))
        migrationKeyViews = [migrationVersionAcknowledgement]

        if busyMessage == "Checking compatibility…" {
            spinner.style = .spinning
            spinner.controlSize = .small
            spinner.startAnimation(nil)
            let checking = NSStackView(views: [spinner, NSTextField(labelWithString: "Checking…")])
            checking.spacing = 8
            checking.alignment = .centerY
            views.append(checking)
        } else if compatibility?.compatible == true {
            views.append(featureRow("Ready to import.", "checkmark.circle", color: HubPalette.success))
        }

        let change = HubActionButton(title: "Change Server", target: self,
                                     action: #selector(changeMigrationServer))
        configureFlatButton(change)
        views.append(change)
        migrationKeyViews.append(change)

        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        for view in views.dropLast() {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private var isPreviewConnectedMigration: Bool {
        controller.previewMode && previewRoute == "migration-connected"
    }

    private func connectedMigrationPreviewBody() -> NSView {
        migrationConnectButton = nil
        migrationKeyViews = [migrationVersionAcknowledgement]
        let stack = NSStackView(views: [
            migrationSuccessCard(host: migrationServer.stringValue),
            wrappingCheckbox(
                migrationVersionAcknowledgement,
                title: "I confirm this server runs TeslaMate 4.2.0 or newer"
            )
        ])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 16
        for view in stack.arrangedSubviews {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func migrationSuccessCard(host: String) -> NSView {
        let icon = NSImageView(image: symbolImage("checkmark.circle", description: nil))
        icon.setAccessibilityElement(false)
        icon.image = icon.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 16, weight: .medium)
        )
        icon.contentTintColor = HubPalette.success
        icon.widthAnchor.constraint(equalToConstant: 16).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 16).isActive = true
        let title = NSTextField(labelWithString: "Connected to \(host)")
        title.font = HubTypography.emphasis
        let detail = NSTextField(labelWithString: "Found a TeslaMate database ready to import.")
        detail.font = HubTypography.body
        detail.textColor = HubPalette.mutedForeground
        let copy = NSStackView(views: [title, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2
        let row = NSStackView(views: [icon, copy])
        row.spacing = 11
        row.alignment = .centerY
        row.translatesAutoresizingMaskIntoConstraints = false
        let card = HubCardView()
        card.addSubview(row)
        NSLayoutConstraint.activate([
            card.heightAnchor.constraint(equalToConstant: 58),
            row.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            row.trailingAnchor.constraint(lessThanOrEqualTo: card.trailingAnchor, constant: -14),
            row.centerYAnchor.constraint(equalTo: card.centerYAnchor)
        ])
        return card
    }

    private func migrationProgressBody() -> NSView {
        migrationProgress.identifier = NSUserInterfaceItemIdentifier("onboarding.migration-progress")
        migrationProgress.style = .bar
        migrationProgress.isIndeterminate = false
        migrationProgress.controlSize = .regular

        let title = NSTextField(labelWithString: "Importing data…")
        title.font = HubTypography.heading
        title.textColor = HubPalette.foreground
        let subtitle = NSTextField(labelWithString: "Copying your TeslaMate history into Hub.")
        subtitle.font = HubTypography.body
        subtitle.textColor = HubPalette.mutedForeground

        let stack = NSStackView(views: [title, subtitle, migrationProgress])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 0
        stack.setCustomSpacing(4, after: title)
        stack.setCustomSpacing(14, after: subtitle)
        return stack
    }

    private func startMigrationProgress() {
        migrationProgress.minValue = 0
        migrationProgress.maxValue = 1
        migrationProgress.doubleValue = 0
    }

    func updateMigrationProgress(_ progress: HubMigrationProgress) {
        guard focusedOperation == .importing else { return }
        let total = max(1, Double(progress.totalRows))
        let completed = min(Double(progress.completedRows), total)
        let previousTotal = migrationProgress.maxValue
        let previousCompleted = migrationProgress.doubleValue
        migrationProgress.maxValue = total
        migrationProgress.doubleValue = previousTotal == total
            ? min(max(previousCompleted, completed), total)
            : completed
        migrationProgress.setAccessibilityLabel("Import progress")
        migrationProgress.setAccessibilityValue("\(Int(completed)) of \(Int(total)) rows")
    }

    private func resetMigrationProgress() {
        migrationProgress.stopAnimation(nil)
        migrationProgress.minValue = 0
        migrationProgress.maxValue = 1
        migrationProgress.doubleValue = 0
    }

    private func migrationDiagnosticView(_ diagnostic: TeslaMateSSHDiagnostic) -> NSView {
        let title = NSTextField(wrappingLabelWithString: diagnostic.title)
        title.font = HubTypography.emphasis
        title.textColor = HubPalette.foreground

        let summary = NSTextField(wrappingLabelWithString: diagnostic.summary)
        summary.font = HubTypography.body
        summary.maximumNumberOfLines = 0

        let stack = NSStackView(views: [title, summary])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 6
        for suggestion in diagnostic.suggestions {
            let label = NSTextField(wrappingLabelWithString: "• \(suggestion)")
            label.font = HubTypography.body
            label.textColor = .secondaryLabelColor
            label.maximumNumberOfLines = 0
            stack.addArrangedSubview(label)
        }

        var buttons: [NSButton] = []
        for action in diagnostic.recoveryActions {
            let button: NSButton
            switch action {
            case .chooseKey:
                button = HubActionButton(title: "Choose Another Key…", target: self,
                                  action: #selector(chooseMigrationIdentity))
            case .usePassword:
                button = HubActionButton(title: "Use Password", target: self,
                                  action: #selector(useMigrationPassword))
            case .useKey:
                button = HubActionButton(title: "Use SSH Key", target: self,
                                  action: #selector(useMigrationKey))
            case .openLogs:
                button = HubActionButton(title: "Open Logs", target: self, action: #selector(openLogs))
            }
            configureFlatButton(button)
            buttons.append(button)
        }
        let copy = HubActionButton(title: "Copy Details", target: self,
                            action: #selector(copyMigrationDiagnostic))
        configureFlatButton(copy)
        buttons.append(copy)
        for index in stride(from: 0, to: buttons.count, by: 2) {
            let buttonRow = NSStackView(views: Array(buttons[index..<min(index + 2, buttons.count)]))
            buttonRow.spacing = 12
            buttonRow.alignment = .centerY
            stack.addArrangedSubview(buttonRow)
        }
        for view in stack.arrangedSubviews {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        let card = HubCardView()
        card.identifier = NSUserInterfaceItemIdentifier("onboarding.connection-error")
        stack.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 12),
            stack.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -12),
            stack.topAnchor.constraint(equalTo: card.topAnchor, constant: 12),
            stack.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -12)
        ])
        return card
    }

    private func verifyBody() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 0
        if busy && checks.isEmpty {
            let row = NSStackView(views: [spinner, NSTextField(labelWithString: "Running checks…")])
            row.spacing = 10
            row.alignment = .centerY
            stack.addArrangedSubview(row)
        } else {
            let card = HubCardView()
            let rows = NSStackView()
            rows.orientation = .vertical
            rows.alignment = .leading
            rows.spacing = 0
            rows.translatesAutoresizingMaskIntoConstraints = false
            card.addSubview(rows)
            for (index, check) in checks.enumerated() {
                if index > 0 {
                    let line = HubOnboardingHairlineView()
                    line.heightAnchor.constraint(equalToConstant: 1).isActive = true
                    rows.addArrangedSubview(line)
                    line.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
                }
                rows.addArrangedSubview(verificationRow(check))
            }
            NSLayoutConstraint.activate([
                card.heightAnchor.constraint(equalToConstant: 318),
                rows.leadingAnchor.constraint(equalTo: card.leadingAnchor),
                rows.trailingAnchor.constraint(equalTo: card.trailingAnchor),
                rows.topAnchor.constraint(equalTo: card.topAnchor),
                rows.bottomAnchor.constraint(equalTo: card.bottomAnchor)
            ])
            stack.addArrangedSubview(card)
            card.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private func verificationRow(_ check: HubOnboardingCheck) -> NSView {
        let icon = NSImageView(image: symbolImage(
            check.passed ? "checkmark.circle" : "xmark.circle",
            description: check.passed ? "Passed" : "Failed"
        ))
        icon.image = icon.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 16, weight: .medium)
        )
        icon.contentTintColor = check.passed ? HubPalette.success : HubPalette.danger
        icon.widthAnchor.constraint(equalToConstant: 16).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 16).isActive = true
        let title = NSTextField(labelWithString: check.title)
        title.font = HubTypography.emphasis
        let detail = NSTextField(labelWithString: check.detail)
        detail.font = HubTypography.body
        detail.textColor = HubPalette.mutedForeground
        let copy = NSStackView(views: [title, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 1
        let row = NSStackView(views: [icon, copy])
        row.identifier = NSUserInterfaceItemIdentifier("onboarding.verify.row")
        row.spacing = 11
        row.alignment = .centerY
        row.edgeInsets = NSEdgeInsets(top: 7, left: 14, bottom: 7, right: 14)
        row.heightAnchor.constraint(equalToConstant: 52).isActive = true
        return row
    }

    private func finishBody() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        let medallion = NSView()
        medallion.wantsLayer = true
        medallion.layer?.backgroundColor = HubPalette.success.withAlphaComponent(0.12).cgColor
        medallion.layer?.cornerRadius = 27
        let icon = NSImageView(image: symbolImage("checkmark.circle", description: nil))
        icon.setAccessibilityElement(false)
        icon.image = icon.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 27, weight: .medium)
        )
        icon.contentTintColor = HubPalette.success
        icon.translatesAutoresizingMaskIntoConstraints = false
        medallion.addSubview(icon)
        NSLayoutConstraint.activate([
            medallion.widthAnchor.constraint(equalToConstant: 54),
            medallion.heightAnchor.constraint(equalToConstant: 54),
            icon.centerXAnchor.constraint(equalTo: medallion.centerXAnchor),
            icon.centerYAnchor.constraint(equalTo: medallion.centerYAnchor)
        ])
        let medallionRow = NSView()
        medallion.translatesAutoresizingMaskIntoConstraints = false
        medallionRow.addSubview(medallion)
        NSLayoutConstraint.activate([
            medallionRow.heightAnchor.constraint(equalToConstant: 54),
            medallion.centerXAnchor.constraint(equalTo: medallionRow.centerXAnchor),
            medallion.centerYAnchor.constraint(equalTo: medallionRow.centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 27),
            icon.heightAnchor.constraint(equalToConstant: 27)
        ])
        stack.addArrangedSubview(medallionRow)
        medallionRow.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        if state.path == .migration {
            let acknowledgement = NSButton(checkboxWithTitle: "",
                                           target: self,
                                           action: #selector(handoverChanged(_:)))
            acknowledgement.state = handoverAcknowledged ? .on : .off
            acknowledgement.controlSize = .regular
            acknowledgement.setAccessibilityLabel(
                "I have disabled Tesla access in TeslaMate to avoid duplicate requests"
            )
            let acknowledgementRow = wrappingCheckbox(
                acknowledgement,
                title: "I have disabled Tesla access in TeslaMate to avoid duplicate requests"
            )
            stack.addArrangedSubview(acknowledgementRow)
            acknowledgementRow.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private func progressView() -> NSView {
        let progress = NSProgressIndicator()
        progress.identifier = NSUserInterfaceItemIdentifier("onboarding.progress")
        progress.style = .bar
        progress.controlSize = .small
        progress.isIndeterminate = false
        progress.minValue = 0
        progress.maxValue = 5
        progress.doubleValue = Double(state.step)
        progress.widthAnchor.constraint(equalToConstant: 150).isActive = true
        progress.setAccessibilityLabel("Setup progress")
        progress.setAccessibilityValue("Step \(state.step) of 5")
        return progress
    }

    private func footerView() -> NSView {
        cancelButton.target = self
        cancelButton.action = #selector(cancelPressed)
        cancelButton.identifier = NSUserInterfaceItemIdentifier("onboarding.cancel")
        configureFlatButton(cancelButton)
        cancelButton.controlSize = .regular
        cancelButton.keyEquivalent = "\u{1b}"
        cancelButton.keyEquivalentModifierMask = []
        backButton.target = self
        backButton.action = #selector(backPressed)
        configureFlatButton(backButton)
        backButton.hubStyle = .neutral
        backButton.image = NSImage(systemSymbolName: "chevron.left", accessibilityDescription: "Back")
        backButton.imagePosition = .imageLeading
        backButton.controlSize = .regular
        backWidthConstraint?.isActive = false
        backWidthConstraint = backButton.widthAnchor.constraint(equalToConstant: HubMetrics.actionMinimumWidth)
        backWidthConstraint?.isActive = true
        continueButton.target = self
        continueButton.action = #selector(continuePressed)
        configurePrimaryButton(continueButton)
        continueButton.controlSize = .regular
        continueWidthConstraint?.isActive = false
        continueWidthConstraint = continueButton.widthAnchor.constraint(equalToConstant: 240)
        continueWidthConstraint?.isActive = true
        continueHeightConstraint?.isActive = false
        continueHeightConstraint = continueButton.heightAnchor.constraint(equalToConstant: HubMetrics.onboardingPrimaryHeight)
        continueHeightConstraint?.isActive = true

        footerSpinner.style = .spinning
        footerSpinner.controlSize = .small
        footerSpinner.isDisplayedWhenStopped = false
        footerSpinner.toolTip = "Hub setup is working"
        let logs = HubActionButton(title: "View Logs", target: self, action: #selector(openLogs))
        configureFlatButton(logs)
        logs.controlSize = .regular
        footerLogsButton = logs
        let footer = NSView()
        let leading = NSStackView(views: [cancelButton])
        leading.spacing = 8
        leading.alignment = .centerY
        for view in [leading, continueButton, logs] {
            view.translatesAutoresizingMaskIntoConstraints = false
            footer.addSubview(view)
            view.centerYAnchor.constraint(equalTo: footer.centerYAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            footer.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight),
            leading.leadingAnchor.constraint(equalTo: footer.leadingAnchor),
            continueButton.centerXAnchor.constraint(equalTo: footer.centerXAnchor),
            logs.trailingAnchor.constraint(equalTo: footer.trailingAnchor),
            leading.trailingAnchor.constraint(lessThanOrEqualTo: continueButton.leadingAnchor, constant: -8),
            logs.leadingAnchor.constraint(greaterThanOrEqualTo: continueButton.trailingAnchor, constant: 8)
        ])
        return footer
    }

    private func updateFooter() {
        let blocked = interactionBlocked
        let focused = focusedOperation != nil
        let importing = focusedOperation == .importing
        cancelButton.isHidden = dismissalPolicy != .accountManagement
        cancelButton.isEnabled = !closeBlocked
        backButton.isHidden = state.route == .welcome
            || state.route == .finish
            || (state.route == .verify && state.path == .newInstallation)
            || importing
        backButton.isEnabled = !blocked
        footerLogsButton?.isHidden = state.route != .verify || !verificationFinished
        footerLogsButton?.isEnabled = !blocked
        continueButton.isHidden = importing
        // The focused operation body owns its status copy. Keep the footer's
        // control label stable so shared chrome does not repeat that status.
        continueButton.title = continueTitle
        continueButton.image = nil
        switch state.route {
        case .fleet:
            continueButton.isEnabled = fleetCredentialsValid && !blocked
        case .migration:
            if isPreviewConnectedMigration {
                continueButton.isEnabled = migrationVersionAcknowledgement.state == .on && !blocked
            } else if migrationSession != nil {
                continueButton.isEnabled = compatibility?.compatible == true && !blocked
            } else {
                continueButton.isEnabled = migrationConnectionInputsValid && !blocked
            }
        case .verify:
            continueButton.isEnabled = verificationFinished && !blocked
        case .finish where state.path == .migration:
            continueButton.isEnabled = handoverAcknowledged && !blocked
        default:
            continueButton.isEnabled = !blocked
        }
        if busy {
            spinner.style = .spinning
            spinner.controlSize = .small
            spinner.startAnimation(nil)
            footerSpinner.toolTip = busyMessage ?? "Hub setup is working"
            if focused {
                footerSpinner.stopAnimation(nil)
            } else {
                footerSpinner.startAnimation(nil)
            }
            if busyMessage == "Connecting…" {
                migrationSpinner.toolTip = "Connecting securely to TeslaMate"
                migrationSpinner.startAnimation(nil)
            } else {
                migrationSpinner.stopAnimation(nil)
            }
        } else {
            spinner.stopAnimation(nil)
            migrationSpinner.stopAnimation(nil)
            footerSpinner.stopAnimation(nil)
        }
        migrationConnectButton = state.route == .migration ? continueButton : nil
        migrationConnectButton?.isEnabled = continueButton.isEnabled
        for case let control as NSControl in migrationKeyViews {
            control.isEnabled = !blocked
        }
        if let migrationConnectButton {
            migrationConnectButton.title = busyMessage == "Connecting…"
                ? "Connecting…" : continueTitle
            updatePrimaryAppearance(migrationConnectButton)
        }
        updatePrimaryAppearance(continueButton)
        backWidthConstraint?.constant = HubMetrics.actionMinimumWidth
        continueWidthConstraint?.constant = 240
        window?.defaultButtonCell = blocked || continueButton.isHidden || !continueButton.isEnabled
            ? nil
            : continueButton.cell as? NSButtonCell
    }

    private var continueTitle: String {
        switch state.route {
        case .welcome: return "Get Started"
        case .fleet: return "Set Up Fleet"
        case .legacy: return "Connect Tesla"
        case .migration:
            return isPreviewConnectedMigration || migrationSession != nil
                ? "Import Data" : "Connect to Server"
        case .verify:
            return verificationFinished && !checks.isEmpty && checks.allSatisfy(\.passed)
                ? "Continue" : "Run Again"
        case .finish: return "Start Hub"
        default: return "Continue"
        }
    }

    @objc private func selectNewInstallation() {
        state.path = .newInstallation
        render()
    }

    @objc private func selectMigration() {
        state.path = .migration
        render()
    }

    @objc private func selectFleet() {
        state.provider = .fleet
        render()
    }

    @objc private func selectLegacy() {
        state.provider = .legacy
        render()
    }

    @objc private func backPressed() {
        guard !interactionBlocked else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        errorMessage = nil
        state.back()
        render()
    }

    @objc private func cancelPressed() {
        guard let window, windowShouldClose(window) else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        if !closesWindowOnCancel {
            resetMigrationProgress()
            migrationSession?.close()
            guard !didNotifyDismiss else { return }
            didNotifyDismiss = true
            onDismiss()
            return
        }
        close()
    }

    override func cancelOperation(_ sender: Any?) {
        cancelPressed()
    }

    @objc private func continuePressed() {
        guard !interactionBlocked else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        switch state.route {
        case .welcome, .choose, .provider:
            state.advance()
            errorMessage = nil
            render()
        case .fleet:
            configureFleet()
        case .legacy:
            configureLegacy()
        case .migration:
            if isPreviewConnectedMigration {
                return
            } else if migrationSession != nil {
                importMigration()
            } else {
                checkMigrationCompatibility()
            }
        case .verify:
            if verificationFinished && !checks.isEmpty && checks.allSatisfy(\.passed) {
                state.advance()
                render()
            } else {
                runVerification()
            }
        case .finish:
            finishOnboarding()
        }
    }

    private func configureFleet() {
        guard let expires = Int64(fleetExpiry.stringValue), expires > 0,
              !fleetClientID.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !fleetAccessToken.stringValue.isEmpty,
              !fleetRefreshToken.stringValue.isEmpty else {
            HubAppLog.shared.record("setup.rejected", category: "account", level: "WARN",
                                    fields: [
                                        "provider": "fleet",
                                        "reason": "incomplete_fields"
                                    ])
            showInlineError("Complete the Fleet client ID, token, region, and expiry fields.")
            return
        }
        let regions = [
            "europe_middle_east_and_africa",
            "north_america_and_asia_pacific",
            "china"
        ]
        let credentials = HubFleetSetupCredentials(accessToken: fleetAccessToken.stringValue,
                                                   refreshToken: fleetRefreshToken.stringValue,
                                                   clientID: fleetClientID.stringValue,
                                                   region: regions[fleetRegion.indexOfSelectedItem],
                                                   expiresInSeconds: expires)
        setBusy(true, message: "Setting up Hub…")
        controller.configureFleetAccount(credentials: credentials) { [weak self] result in
            self?.setupFinished(result)
        }
    }

    private func configureLegacy() {
        let access = legacyAccessToken.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let refresh = legacyRefreshToken.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if !access.isEmpty || !refresh.isEmpty {
            guard !access.isEmpty, !refresh.isEmpty else {
                HubAppLog.shared.record("setup.rejected", category: "account", level: "WARN",
                                        fields: [
                                            "provider": "legacy",
                                            "reason": "incomplete_token_pair"
                                        ])
                showInlineError("Enter both the access token and refresh token.")
                return
            }
            setBusy(true, message: "Setting up Hub…")
            controller.configureTeslaAccount(tokens: TeslaAuthTokens(accessToken: access,
                                                                      refreshToken: refresh)) { [weak self] result in
                self?.setupFinished(result)
            }
            return
        }
        startLegacySignIn()
    }

    @objc private func startLegacySignIn() {
        guard authWindow == nil else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        HubAppLog.shared.record("authentication.started", category: "account",
                                fields: ["provider": "legacy"])
        do {
            let auth = try TeslaAuthWindowController { [weak self] result in
                guard let self else { return }
                self.authWindow = nil
                self.updateWindowCloseAvailability()
                self.updateFooter()
                switch result {
                case let .success(tokens):
                    HubAppLog.shared.record("authentication.completed", category: "account",
                                            fields: ["provider": "legacy"])
                    self.setBusy(true, message: "Setting up Hub…")
                    self.controller.configureTeslaAccount(tokens: tokens) { [weak self] setup in
                        self?.setupFinished(setup)
                    }
                case let .failure(error):
                    if error as? TeslaAuthError == .cancelled {
                        HubAppLog.shared.record("authentication.cancelled", category: "account",
                                                fields: ["provider": "legacy"])
                    } else {
                        HubAppLog.shared.record("authentication.failed", category: "account",
                                                level: "ERROR", fields: [
                                                    "provider": "legacy",
                                                    "error_code": HubAppLog.errorCode(error)
                                                ])
                        self.showInlineError(error.localizedDescription)
                    }
                }
            }
            authWindow = auth
            updateWindowCloseAvailability()
            updateFooter()
            auth.showWindow(nil)
            auth.window?.makeKeyAndOrderFront(nil)
        } catch {
            HubAppLog.shared.record("authentication.failed", category: "account", level: "ERROR",
                                    fields: [
                                        "provider": "legacy",
                                        "error_code": HubAppLog.errorCode(error)
                                    ])
            showInlineError(error.localizedDescription)
        }
    }

    private func setupFinished(_ result: Result<Void, Error>) {
        resetMigrationProgress()
        setBusy(false)
        switch result {
        case .success:
            state.advance()
            checks = []
            verificationFinished = false
            render()
            runVerification()
        case let .failure(error):
            showInlineError(error.localizedDescription)
        }
    }

    @objc private func checkMigrationCompatibility() {
        let host = migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let user = migrationUser.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let port = Int(migrationPort.stringValue), (1...65535).contains(port),
              !host.isEmpty, !user.isEmpty else {
            HubAppLog.shared.record("compatibility.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "missing_server_input"])
            showInlineError("Enter the TeslaMate server, SSH user, and port.")
            return
        }
        HubAppLog.shared.record("compatibility.started", category: "teslamate_import")
        setBusy(true, message: "Connecting…")
        let requestedMigrationIdentity = currentMigrationIdentity
        compatibility = nil
        migrationDiagnostic = nil
        errorMessage = nil
        migrationVersionAcknowledgement.state = .off
        migrationSession?.close()
        migrationSession = nil
        connectedMigrationIdentity = nil
        let authentication: TeslaMateSSHAuthentication
        if migrationAuthentication.indexOfSelectedItem == 0 {
            let path = migrationIdentityFile.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            authentication = .key(identityFile: path.isEmpty ? nil : URL(fileURLWithPath: path))
        } else {
            authentication = .password(migrationSSHPassword.stringValue)
        }
        TeslaMateServerImporter.connect(host: host,
                                        user: user,
                                        port: port,
                                        authentication: authentication,
                                        usePasswordlessSudo: migrationUseSudo.state == .on) { [weak self] result in
            guard let self else { return }
            switch result {
            case let .success(session):
                self.migrationDiagnostic = nil
                self.migrationSSHPassword.stringValue = ""
                self.migrationSession = session
                self.connectedMigrationIdentity = requestedMigrationIdentity
                self.migrationSource.stringValue = session.source
                self.migrationCarID.stringValue = session.carID
                self.migrationPasswordFile.stringValue = session.passwordFile.path
                self.migrationKeyFile.stringValue = session.encryptionKeyFile.path
                self.checkConnectedMigrationSession(session)
            case let .failure(error):
                HubAppLog.shared.record("connection.failed", category: "teslamate_import",
                                        level: "ERROR",
                                        fields: ["error_code": HubAppLog.errorCode(error)])
                self.setBusy(false)
                self.compatibility = nil
                self.errorMessage = nil
                self.migrationDiagnostic = TeslaMateServerImporter.connectionDiagnostic(
                    for: error,
                    authentication: authentication
                )
                self.render()
            }
        }
    }

    private func checkConnectedMigrationSession(_ session: TeslaMateServerImportSession) {
        guard session === migrationSession else { return }
        let versionAccepted = migrationVersionAcknowledgement.state == .on
        guard versionAccepted else {
            setBusy(false)
            compatibility = HubTeslaMateCompatibility(
                compatible: false,
                message: "The database schema cannot distinguish TeslaMate 4.1.1 from 4.2.0. Confirm the running server is 4.2.0 or newer, then continue.",
                reasonCode: "v4_2_version_unconfirmed",
                requiredVersion: "4.2.0"
            )
            render()
            return
        }
        controller.checkTeslaMateCompatibility(
            source: session.source,
            carID: session.carID,
            passwordFile: session.passwordFile.path,
            acknowledgeV42CompatibleSchema: versionAccepted
        ) { [weak self] check in
            guard let self else { return }
            self.setBusy(false)
            switch check {
            case let .success(report):
                HubAppLog.shared.record("compatibility.completed",
                                        category: "teslamate_import",
                                        fields: [
                                            "compatible": report.compatible ? "true" : "false",
                                            "reason": report.reasonCode
                                        ])
                self.compatibility = report
                self.errorMessage = nil
            case let .failure(error):
                HubAppLog.shared.record("compatibility.failed", category: "teslamate_import",
                                        level: "ERROR",
                                        fields: ["error_code": HubAppLog.errorCode(error)])
                session.close()
                self.migrationSession = nil
                self.connectedMigrationIdentity = nil
                self.compatibility = HubTeslaMateCompatibility(compatible: false,
                                                               message: error.localizedDescription,
                                                               reasonCode: "unavailable",
                                                               requiredVersion: "4.2.0")
            }
            self.render()
        }
    }

    private func importMigration() {
        let versionAccepted = migrationVersionAcknowledgement.state == .on
        guard let session = migrationSession,
              migrationInputsComplete,
              versionAccepted,
              compatibility?.compatible == true else {
            HubAppLog.shared.record("import.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "connection_not_ready"])
            showInlineError("Connect to the TeslaMate server before importing.")
            return
        }
        guard connectedMigrationIdentity == currentMigrationIdentity else {
            HubAppLog.shared.record("import.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "settings_changed"])
            session.close()
            migrationSession = nil
            connectedMigrationIdentity = nil
            compatibility = nil
            showInlineError("Server settings changed. Connect to the TeslaMate server again before importing.")
            return
        }
        startMigrationProgress()
        setBusy(true, message: "Importing data…")
        controller.importTeslaMateOnline(source: session.source,
                                         carID: session.carID,
                                         passwordFile: session.passwordFile.path,
                                         encryptionKeyFile: session.encryptionKeyFile.path,
                                         acknowledgeV42CompatibleSchema: versionAccepted,
                                         progress: { [weak self] update in
                                             self?.updateMigrationProgress(update)
                                         }) { [weak self] result in
            session.close()
            self?.migrationSession = nil
            self?.connectedMigrationIdentity = nil
            self?.setupFinished(result)
        }
    }

    private var migrationInputsComplete: Bool {
        !migrationSource.stringValue.isEmpty
            && Int64(migrationCarID.stringValue).map { $0 > 0 } == true
            && !migrationPasswordFile.stringValue.isEmpty
            && !migrationKeyFile.stringValue.isEmpty
    }

    private var currentMigrationIdentity: String {
        let values = [
            migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines),
            migrationUser.stringValue.trimmingCharacters(in: .whitespacesAndNewlines),
            migrationPort.stringValue.trimmingCharacters(in: .whitespacesAndNewlines),
            String(migrationAuthentication.indexOfSelectedItem),
            migrationIdentityFile.stringValue.trimmingCharacters(in: .whitespacesAndNewlines),
            migrationUseSudo.state == .on ? "sudo" : "direct"
        ]
        return values.joined(separator: "\u{0}")
    }

    private var migrationConnectionInputsValid: Bool {
        let host = migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let user = migrationUser.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        return !host.isEmpty
            && !user.isEmpty
            && Int(migrationPort.stringValue).map { (1...65535).contains($0) } == true
    }

    private var fleetCredentialsValid: Bool {
        let clientID = fleetClientID.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        return !clientID.isEmpty
            && !fleetAccessToken.stringValue.isEmpty
            && !fleetRefreshToken.stringValue.isEmpty
            && Int64(fleetExpiry.stringValue).map { $0 > 0 } == true
    }

    private func runVerification() {
        checks = []
        verificationFinished = false
        busy = true
        busyMessage = "Running checks…"
        errorMessage = nil
        render()
        controller.runOnboardingChecks(expectRunning: state.path == .newInstallation) { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            self.verificationFinished = true
            switch result {
            case let .success(checks):
                self.checks = checks
                self.errorMessage = checks.allSatisfy(\.passed) ? nil : "One or more checks need attention."
            case let .failure(error):
                self.checks = []
                self.errorMessage = error.localizedDescription
            }
            self.render()
        }
    }

    private func finishOnboarding() {
        guard state.path == .migration else {
            onComplete(.configured)
            return
        }
        guard handoverAcknowledged else { return }
        setBusy(true, message: "Starting Hub…")
        controller.acknowledgeMigrationHandoverAndStart { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case .success: self.onComplete(.hubStarted)
            case let .failure(error): self.showInlineError(error.localizedDescription)
            }
        }
    }

    @objc private func handoverChanged(_ sender: NSButton) {
        handoverAcknowledged = sender.state == .on
        updateFooter()
    }

    @objc private func openFleetGuide() {
        if let url = URL(string: "https://developer.tesla.com") {
            NSWorkspace.shared.open(url)
        }
    }

    @objc private func openLogs() {
        guard !interactionBlocked, let window else { return }
        let logs = LogsWindowController(controller: controller, embedded: true)
        logsWindow = logs
        let page = logs.makeEmbeddedPage { [weak self] in
            guard let self, let window = self.window else { return }
            window.contentView = self.onboardingContainer
            self.logsWindow = nil
            self.updateFooter()
            window.recalculateKeyViewLoop()
            HubMotion.transition(self.onboardingContainer, forward: false)
        }
        window.contentView = page
        window.defaultButtonCell = nil
        HubMotion.transition(page, forward: true)
    }

    @objc private func chooseMigrationPassword() {
        chooseFile(for: migrationPasswordFile)
    }

    @objc private func chooseMigrationKey() {
        chooseFile(for: migrationKeyFile)
    }

    @objc private func chooseMigrationIdentity() {
        chooseFile(for: migrationIdentityFile)
    }

    @objc private func useMigrationPassword() {
        migrationAuthentication.selectItem(at: 1)
        migrationAuthenticationChanged()
        window?.makeFirstResponder(migrationSSHPassword)
    }

    @objc private func useMigrationKey() {
        migrationAuthentication.selectItem(at: 0)
        migrationAuthenticationChanged()
        window?.makeFirstResponder(migrationIdentityFile)
    }

    @objc private func copyMigrationDiagnostic() {
        guard let migrationDiagnostic else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(migrationDiagnostic.safeReport, forType: .string)
    }

    @objc private func migrationAuthenticationChanged() {
        compatibility = nil
        errorMessage = nil
        migrationDiagnostic = nil
        migrationVersionAcknowledgement.state = .off
        migrationSession?.close()
        migrationSession = nil
        connectedMigrationIdentity = nil
        render()
    }

    func controlTextDidChange(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        if field === fleetClientID || field === fleetAccessToken
            || field === fleetRefreshToken || field === fleetExpiry {
            updateFooter()
            return
        }
        guard field === migrationServer || field === migrationUser || field === migrationPort else { return }
        updateFooter()
    }

    @objc private func changeMigrationServer() {
        migrationSession?.close()
        migrationSession = nil
        connectedMigrationIdentity = nil
        compatibility = nil
        migrationVersionAcknowledgement.state = .off
        errorMessage = nil
        render()
    }

    @objc private func migrationVersionAcknowledgementChanged() {
        guard let session = migrationSession else {
            render()
            return
        }
        compatibility = nil
        if migrationVersionAcknowledgement.state == .on {
            setBusy(true, message: "Checking compatibility…")
            checkConnectedMigrationSession(session)
        } else {
            render()
        }
    }

    private func chooseFile(for field: NSTextField) {
        guard let window, !interactionBlocked, !filePanelOpen else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        if field === migrationIdentityFile {
            panel.title = "Choose SSH Key"
            panel.prompt = "Choose Key"
            panel.showsHiddenFiles = true
            panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".ssh")
        }
        filePanelOpen = true
        presentFilePanel(panel, window) { [weak self, weak field] url in
            guard let self else { return }
            self.filePanelOpen = false
            guard let field, let url else { return }
            field.stringValue = url.path
            field.toolTip = url.path
            self.updateFooter()
        }
    }

    func setBusy(_ value: Bool, message: String? = nil) {
        let wasFocused = focusedOperation != nil
        let wasBusy = busy
        let previousMessage = busyMessage
        busy = value
        busyMessage = value ? message : nil
        updateWindowCloseAvailability()
        if wasFocused || focusedOperation != nil {
            render()
        } else {
            updateFooter()
        }
        let announcementSource: Any = onboardingContainer.map { $0 as Any } ?? self
        if value, !wasBusy {
            HubAccessibility.announce(message ?? "Setup operation started.", from: announcementSource)
        } else if !value, wasBusy {
            HubAccessibility.announce("\(previousMessage ?? "Setup operation") finished.",
                                      from: announcementSource)
        }
    }

    private func updateWindowCloseAvailability() {
        window?.standardWindowButton(.closeButton)?.isEnabled = canCancel
    }

    private var interactionBlocked: Bool { busy || authWindow != nil }

    private var closeBlocked: Bool { interactionBlocked || controller.hasPendingMigrationHandover }

    private func showInlineError(_ message: String) {
        setBusy(false)
        errorMessage = message
        render()
        let announcementSource: Any = onboardingContainer.map { $0 as Any } ?? self
        HubAccessibility.announce("Setup failed: \(message)", from: announcementSource)
    }

    private func withError(_ view: NSView) -> NSView {
        guard let errorMessage else { return view }
        let error = NSTextField(wrappingLabelWithString: errorMessage)
        error.textColor = .systemRed
        error.maximumNumberOfLines = 0
        let stack = NSStackView(views: [view, error])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        error.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        return stack
    }

    private func verticalChoices(_ choices: [NSView]) -> NSView {
        let stack = NSStackView(views: choices)
        stack.orientation = .vertical
        stack.spacing = 8
        stack.alignment = .leading
        for choice in choices {
            choice.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func choiceButton(title: String,
                              subtitle: String,
                              selected: Bool,
                              action: Selector) -> NSView {
        let card = HubCardView()
        card.isSelected = selected
        let symbol: String
        switch action {
        case #selector(selectNewInstallation): symbol = "cylinder"
        case #selector(selectMigration): symbol = "square.and.arrow.down"
        case #selector(selectFleet): symbol = "antenna.radiowaves.left.and.right"
        default: symbol = "key"
        }
        let icon = HubIconTileView(symbol: symbol, accessibilityDescription: nil)
        let label = NSTextField(labelWithString: title)
        label.font = HubTypography.emphasis
        let detail = NSTextField(wrappingLabelWithString: subtitle)
        detail.font = HubTypography.caption
        detail.textColor = .secondaryLabelColor
        detail.maximumNumberOfLines = 2
        let copy = NSStackView(views: [label, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 3
        let indicator = HubIconTileView(symbol: selected ? "largecircle.fill.circle" : "circle",
                                        accessibilityDescription: nil, size: 18, symbolSize: 14,
                                        tint: selected ? HubPalette.accent : .secondaryLabelColor)
        let row = NSStackView(views: [icon, copy, spacer(), indicator])
        row.alignment = .centerY
        row.spacing = 12
        row.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(row)
        let radio = NSButton(radioButtonWithTitle: title, target: self, action: action)
        radio.state = selected ? .on : .off
        radio.isTransparent = true
        radio.setAccessibilityHelp(subtitle)
        radio.setAccessibilityValue(selected ? "Selected" : "Not selected")
        radio.translatesAutoresizingMaskIntoConstraints = false
        pageKeyViews.append(radio)
        card.addSubview(radio)
        NSLayoutConstraint.activate([
            card.heightAnchor.constraint(equalToConstant: 64),
            row.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 16),
            row.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -16),
            row.centerYAnchor.constraint(equalTo: card.centerYAnchor),
            radio.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            radio.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            radio.topAnchor.constraint(equalTo: card.topAnchor),
            radio.bottomAnchor.constraint(equalTo: card.bottomAnchor)
        ])
        return card
    }

    private func verticalField(_ title: String, _ field: NSView, width: CGFloat? = nil) -> NSView {
        let label = NSTextField(labelWithString: title)
        label.font = HubTypography.label
        label.textColor = HubPalette.mutedForeground
        if let width {
            field.widthAnchor.constraint(equalToConstant: width).isActive = true
        }
        if let control = field as? NSControl {
            control.controlSize = .regular
            control.font = HubTypography.body
        }
        let stack = NSStackView(views: [label, field])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        if width == nil {
            field.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func wrappingCheckbox(_ checkbox: NSButton, title: String) -> NSView {
        let label = NSTextField(wrappingLabelWithString: title)
        label.font = HubTypography.body
        label.maximumNumberOfLines = 0
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let row = NSStackView(views: [checkbox, label])
        row.alignment = .firstBaseline
        row.spacing = 7
        return row
    }

    private func featureRow(_ title: String,
                            _ symbol: String,
                            color: NSColor = .secondaryLabelColor) -> NSView {
        let image = NSImageView(image: symbolImage(symbol, description: nil))
        image.setAccessibilityElement(false)
        image.image = image.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 24, weight: .medium)
        )
        image.contentTintColor = color
        image.widthAnchor.constraint(equalToConstant: 18).isActive = true
        image.heightAnchor.constraint(equalToConstant: 18).isActive = true
        let label = NSTextField(wrappingLabelWithString: title)
        label.font = HubTypography.body
        label.maximumNumberOfLines = 2
        let row = NSStackView(views: [image, label])
        row.spacing = 12
        row.alignment = .centerY
        return row
    }

    private func configureKeyViewLoop(previousField: NSView?) {
        guard let window else { return }
        var candidates = pageKeyViews
        switch state.route {
        case .fleet:
            candidates.append(contentsOf: [fleetClientID, fleetAccessToken, fleetRefreshToken,
                                           fleetExpiry, fleetRegion])
        case .legacy:
            candidates.append(contentsOf: [legacyAccessToken, legacyRefreshToken])
        case .migration:
            candidates.append(contentsOf: migrationKeyViews)
        case .welcome, .choose, .provider, .verify, .finish:
            break
        }
        var keyViews = candidates.filter { view in
            !view.isHidden && (view as? NSControl)?.isEnabled != false
        }
        if !cancelButton.isHidden && cancelButton.isEnabled { keyViews.append(cancelButton) }
        if !backButton.isHidden && backButton.isEnabled { keyViews.append(backButton) }
        if !continueButton.isHidden && continueButton.isEnabled { keyViews.append(continueButton) }
        guard !keyViews.isEmpty else { return }
        for index in keyViews.indices {
            keyViews[index].nextKeyView = keyViews[(index + 1) % keyViews.count]
        }
        guard let responder = previousField.flatMap({ previous in
            keyViews.first { $0 === previous }
        }) ?? keyViews.first, responder.window === window else { return }
        window.initialFirstResponder = responder
        window.makeFirstResponder(responder)
    }

    private func formRow(_ title: String, _ field: NSView) -> NSView {
        let label = NSTextField(labelWithString: title)
        label.font = HubTypography.body
        label.textColor = HubPalette.foreground
        label.widthAnchor.constraint(equalToConstant: HubMetrics.formLabelWidth).isActive = true
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        field.widthAnchor.constraint(equalToConstant:
            HubMetrics.onboardingContentWidth
                - (HubMetrics.rowHorizontalInset * 2)
                - HubMetrics.formLabelWidth
                - 16
        ).isActive = true
        let row = NSStackView(views: [label, field])
        row.spacing = 16
        row.alignment = .centerY
        row.distribution = .fill
        row.edgeInsets = NSEdgeInsets(top: 10, left: HubMetrics.rowHorizontalInset,
                                     bottom: 10, right: HubMetrics.rowHorizontalInset)
        row.heightAnchor.constraint(equalToConstant: HubMetrics.rowHeight).isActive = true
        return row
    }

    private func onboardingForm(_ fields: [(String, NSView)]) -> NSView {
        let card = HubCardView()
        let rows = NSStackView()
        rows.orientation = .vertical
        rows.spacing = 0
        rows.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(rows)
        for (index, item) in fields.enumerated() {
            if index > 0 {
                let line = HubOnboardingHairlineView()
                rows.addArrangedSubview(line)
                line.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
            }
            let row = formRow(item.0, item.1)
            rows.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: rows.widthAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            rows.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            rows.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            rows.topAnchor.constraint(equalTo: card.topAnchor),
            rows.bottomAnchor.constraint(equalTo: card.bottomAnchor)
        ])
        return card
    }

    private func fieldWithButton(_ field: NSTextField,
                                 _ button: NSButton,
                                 symbol: String = "folder") -> NSView {
        configureFlatButton(button, symbol: symbol)
        let row = NSStackView(views: [field, button])
        row.spacing = 8
        return row
    }

    private func configureFlatButton(_ button: NSButton,
                                     symbol: String? = nil,
                                     tint: NSColor = .labelColor) {
        button.isBordered = false
        button.image = symbol.flatMap {
            NSImage(systemSymbolName: $0, accessibilityDescription: button.title)
        }
        button.imagePosition = .imageLeading
        button.contentTintColor = .labelColor
        (button as? HubActionButton)?.hubStyle = .flat
        button.font = HubTypography.action
        (button as? HubActionButton)?.hubFont = HubTypography.action
        button.focusRingType = .default
    }

    private func configurePrimaryButton(_ button: NSButton, symbol: String? = nil) {
        button.isBordered = false
        (button as? HubActionButton)?.hubStyle = .primary
        button.image = symbol.flatMap {
            NSImage(systemSymbolName: $0, accessibilityDescription: button.title)
        }
        button.imagePosition = .imageTrailing
        button.contentTintColor = .white
        button.font = HubTypography.action
        (button as? HubActionButton)?.hubFont = HubTypography.action
        button.keyEquivalent = "\r"
        updatePrimaryAppearance(button)
    }

    private func updatePrimaryAppearance(_ button: NSButton) {
        if let button = button as? HubActionButton {
            button.updateHubAppearance()
        }
    }

    private func centered(_ view: NSView) -> NSView {
        let row = NSStackView(views: [spacer(), view, spacer()])
        row.alignment = .centerY
        return row
    }

    private func separator() -> NSBox {
        let line = NSBox()
        line.boxType = .separator
        return line
    }

    private func spacer() -> NSView {
        let view = NSView()
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        return view
    }

    private func appIcon() -> NSImage {
        if let url = Bundle.main.url(forResource: "AppIcon", withExtension: "icns"),
           let image = NSImage(contentsOf: url) {
            return image
        }
        return NSApplication.shared.applicationIconImage
    }

    private func symbolImage(_ name: String, description: String?) -> NSImage {
        if let image = NSImage(systemSymbolName: name, accessibilityDescription: description) {
            return image
        }
        let fallback = NSImage(named: NSImage.infoName) ?? NSImage(size: NSSize(width: 16, height: 16))
        fallback.accessibilityDescription = description
        return fallback
    }
}

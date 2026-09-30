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
    private var busyOperation: BusyOperation?

    enum BusyOperation {
        case setup, importing, checkingCompatibility, connecting, runningChecks, starting

        var message: String {
            switch self {
            case .setup: return HubL10n.text("hub.OnboardingWindowController.558.1914", fallback: "Setting up Hub…")
            case .importing: return HubL10n.text("hub.OnboardingWindowController.304.1870", fallback: "Importing data…")
            case .checkingCompatibility: return HubL10n.text("hub.OnboardingWindowController.busy_checking_compatibility", fallback: "Checking compatibility…")
            case .connecting: return HubL10n.text("hub.OnboardingWindowController.1323.2037", fallback: "Connecting…")
            case .runningChecks: return HubL10n.text("hub.DiagnosticsWindowController.210.227", fallback: "Running checks…")
            case .starting: return HubL10n.text("hub.HubDashboardView.12.1336", fallback: "Starting Hub…")
            }
        }
    }
    private var errorMessage: String?
    private var migrationDiagnostic: TeslaMateSSHDiagnostic?
    private var compatibility: HubTeslaMateCompatibility?
    private var checks: [HubOnboardingCheck] = []
    private var verificationFinished = false
    private var handoverAcknowledged = false
    private var authWindow: TeslaAuthWindowController?
    private var logsWindow: LogsWindowController?
    private weak var logsReturnResponder: NSView?

    private let continueButton = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.168.1852", fallback: "Continue"), target: nil, action: nil)
    private let backButton = HubActionButton(title: HubL10n.text("hub.HubUtilityWindow.24.1603", fallback: "Back"), target: nil, action: nil)
    private let cancelButton = HubActionButton(title: HubL10n.text("hub.ImportSheetController.112.1706", fallback: "Cancel"), target: nil, action: nil)
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
        checkboxWithTitle: HubL10n.text("hub.OnboardingWindowController.195.1856", fallback: "Use passwordless sudo for Docker access"),
        target: nil,
        action: nil
    )
    private let migrationUseLocalHistory = NSButton(
        checkboxWithTitle: HubL10n.text("hub.OnboardingWindowController.200.1857", fallback: "Use a local PostgreSQL snapshot (history only)"), target: nil, action: nil
    )
    private let migrationVersionAcknowledgement = NSButton(
        checkboxWithTitle: HubL10n.text("hub.OnboardingWindowController.203.1858", fallback: "I confirm this server runs TeslaMate 4.2.0 or newer"),
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
    private var onboardingToolbar: HubOnboardingToolbar?
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
                resumeMessage = HubL10n.text("hub.OnboardingWindowController.252.1859", fallback: "The previous import did not finish. Check TeslaMate and run the import again.")
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
        if resumeMigrationHandoverPhase != nil && controller.pendingMigrationHandoverIsHistoryOnly {
            migrationUseLocalHistory.state = .on
        }
        let onboardingToolbar = HubOnboardingToolbar()
        self.onboardingToolbar = onboardingToolbar
        window.toolbar = onboardingToolbar.toolbar
        window.toolbarStyle = .unified
        window.titlebarSeparatorStyle = .automatic
        if effectivePreviewRoute == "migration-connected" {
            migrationServer.stringValue = "teslamate.local"
            compatibility = HubTeslaMateCompatibility(
                compatible: true,
                message: HubL10n.text("hub.OnboardingWindowController.292.1862", fallback: "Ready to import."),
                reasonCode: "preview",
                requiredVersion: "4.2.0"
            )
        } else if effectivePreviewRoute == "migration-error" {
            migrationDiagnostic = TeslaMateSSHDiagnostic(
                reasonCode: "preview", title: HubL10n.text("hub.OnboardingWindowController.298.1866", fallback: "TeslaMate connection failed"),
                summary: HubL10n.text("hub.OnboardingWindowController.299.1867", fallback: "This account cannot access Docker."),
                suggestions: [HubL10n.text("hub.OnboardingWindowController.300.1868", fallback: "Check the server account and Docker access, then try again.")],
                recoveryActions: [.openLogs])
        } else if effectivePreviewRoute == "importing" {
            busy = true
            busyOperation = .importing
            busyMessage = BusyOperation.importing.message
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
        let header = NSView()
        header.identifier = NSUserInterfaceItemIdentifier("onboarding.header")

        let footer = HubOnboardingChromeView()
        footer.identifier = NSUserInterfaceItemIdentifier("onboarding.footer")
        let footerLine = NSBox()
        footerLine.boxType = .separator
        footerLine.translatesAutoresizingMaskIntoConstraints = false
        footer.addSubview(footerLine)

        NSLayoutConstraint.activate([
            footerLine.leadingAnchor.constraint(equalTo: footer.leadingAnchor),
            footerLine.trailingAnchor.constraint(equalTo: footer.trailingAnchor),
            footerLine.topAnchor.constraint(equalTo: footer.topAnchor),
        ])

        onboardingContainer = HubOnboardingContainerView(
            headerView: header,
            headerHeight: 0,
            footerView: footer
        )
        window.contentView = onboardingContainer
    }

    private func configureFields() {
        configureFormPopup(fleetRegion, identifier: "onboarding.fleet-region")
        configureFormPopup(migrationAuthentication, identifier: "onboarding.migration-authentication")
        fleetRegion.addItems(withTitles: [
            HubL10n.text("hub.OnboardingWindowController.437.1892", fallback: "Europe, Middle East and Africa"),
            HubL10n.text("hub.OnboardingWindowController.438.1893", fallback: "North America and Asia Pacific"),
            HubL10n.text("hub.OnboardingWindowController.china_region", fallback: "China")
        ])
        migrationAuthentication.addItems(withTitles: [HubL10n.text("hub.OnboardingWindowController.441.1895", fallback: "SSH key"),
                                                 HubL10n.text("hub.OnboardingWindowController.password_label", fallback: "Password")])
        migrationAuthentication.target = self
        migrationAuthentication.action = #selector(migrationAuthenticationChanged)
        migrationUseSudo.state = .off
        migrationUseSudo.title = ""
        migrationUseSudo.setAccessibilityLabel("This user needs sudo to read the TeslaMate database")
        migrationUseSudo.controlSize = .regular
        migrationUseLocalHistory.target = self
        migrationUseLocalHistory.action = #selector(localHistorySelectionChanged)
        migrationUseLocalHistory.identifier = NSUserInterfaceItemIdentifier("onboarding.local-history")
        migrationVersionAcknowledgement.title = ""
        migrationVersionAcknowledgement.setAccessibilityLabel(
            HubL10n.text("hub.OnboardingWindowController.203.1858", fallback: "I confirm this server runs TeslaMate 4.2.0 or newer")
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
                      migrationIdentityFile, migrationSSHPassword,
                      migrationSource, migrationCarID, migrationPasswordFile] {
            field.controlSize = .regular
            field.font = HubTypography.body
            field.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
        }
        fleetAccessToken.placeholderString = HubL10n.text("hub.OnboardingWindowController.474.1900", fallback: "Access token")
        migrationSource.placeholderString = HubL10n.text("hub.OnboardingWindowController.475.1901", fallback: "Password-free local PostgreSQL URL")
        migrationCarID.placeholderString = HubL10n.text("hub.ImportSheetController.65.1695", fallback: "Car ID")
        migrationPasswordFile.placeholderString = HubL10n.text("hub.OnboardingWindowController.477.1903", fallback: "Owner-only PostgreSQL password file")
        migrationSource.delegate = self
        migrationCarID.delegate = self
        migrationPasswordFile.delegate = self
        fleetRefreshToken.placeholderString = HubL10n.text("hub.OnboardingWindowController.481.1904", fallback: "Refresh token")
        fleetClientID.placeholderString = HubL10n.text("hub.OnboardingWindowController.482.1905", fallback: "Tesla application client ID")
        legacyAccessToken.placeholderString = HubL10n.text("hub.OnboardingWindowController.474.1900", fallback: "Access token")
        legacyRefreshToken.placeholderString = HubL10n.text("hub.OnboardingWindowController.481.1904", fallback: "Refresh token")
        migrationServer.placeholderString = "teslamate.local"
        migrationIdentityFile.placeholderString = HubL10n.text("hub.OnboardingWindowController.486.1909", fallback: "Optional — uses SSH agent or default keys")
        migrationSSHPassword.placeholderString = HubL10n.text("hub.OnboardingWindowController.487.1910", fallback: "SSH password")
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
        if busyOperation == .importing { return .importing }
        if state.path == .newInstallation, busyOperation == .setup { return .setup }
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
            let title = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.558.1914", fallback: "Setting up Hub…"))
            title.font = HubTypography.heading
            title.textColor = HubPalette.foreground
            let subtitle = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.573.1916", fallback: "Saving your connection and preparing Hub."))
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
        case .welcome: return HubL10n.text("hub.OnboardingWindowController.588.1917", fallback: "Welcome to Teslatlas Hub")
        case .choose: return HubL10n.text("hub.OnboardingWindowController.589.1918", fallback: "How would you like to start?")
        case .provider: return HubL10n.text("hub.OnboardingWindowController.590.1919", fallback: "Choose how Hub connects")
        case .fleet: return HubL10n.text("hub.OnboardingWindowController.591.1920", fallback: "Set up Fleet Telemetry")
        case .legacy: return HubL10n.text("hub.OnboardingWindowController.592.1921", fallback: "Connect with a token")
        case .migration: return HubL10n.text("hub.OnboardingWindowController.593.1922", fallback: "Migrate from TeslaMate")
        case .verify: return HubL10n.text("hub.OnboardingWindowController.594.1923", fallback: "Checking your Hub")
        case .finish: return state.path == .migration ? HubL10n.text("hub.OnboardingWindowController.595.1924", fallback: "Migration complete") : HubL10n.text("hub.OnboardingWindowController.595.1925", fallback: "Teslatlas Hub is ready")
        }
    }

    private var pageSubtitle: String {
        switch state.route {
        case .welcome:
            return HubL10n.text("hub.OnboardingWindowController.602.1926", fallback: "Your own Tesla telemetry collector, running privately on this Mac.")
        case .choose:
            return HubL10n.text("hub.OnboardingWindowController.604.1927", fallback: "Set up a fresh Hub or bring your history over from TeslaMate.")
        case .provider:
            return HubL10n.text("hub.OnboardingWindowController.606.1928", fallback: "Fleet Telemetry is recommended. Legacy tokens work with older setups.")
        case .fleet:
            return HubL10n.text("hub.OnboardingWindowController.608.1929", fallback: "Create a Tesla Fleet application, then paste its credentials below.")
        case .legacy:
            return HubL10n.text("hub.OnboardingWindowController.610.1930", fallback: "Sign in with Tesla, or paste an existing token pair.")
        case .migration:
            return HubL10n.text("hub.OnboardingWindowController.612.1931", fallback: "Connect to your TeslaMate server to import its vehicle history.")
        case .verify:
            return HubL10n.text("hub.OnboardingWindowController.614.1932", fallback: "Making sure everything is wired up correctly.")
        case .finish:
            return state.path == .migration
                ? HubL10n.text("hub.OnboardingWindowController.617.1933", fallback: "Your TeslaMate history has been imported into Hub.")
                : HubL10n.text("hub.OnboardingWindowController.618.1934", fallback: "Hub is set up and ready to start collecting vehicle data.")
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
                                   tint: [.welcome, .finish].contains(state.route)
                                       ? HubPalette.success : .labelColor,
                                   radius: 12)
        let title = NSTextField(labelWithString: pageTitle)
        title.font = HubTypography.heading
        title.textColor = HubPalette.foreground
        title.alignment = .natural
        let subtitle = NSTextField(wrappingLabelWithString: pageSubtitle)
        subtitle.font = .systemFont(ofSize: 14)
        subtitle.textColor = HubPalette.mutedForeground
        subtitle.alignment = .natural
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
            onboardingFeatureRow(symbol: "car.side", title: HubL10n.text("hub.OnboardingWindowController.687.1943", fallback: "Connect your Tesla"),
                                 subtitle: HubL10n.text("hub.OnboardingWindowController.688.1944", fallback: "Collect vehicle data in the background.")),
            onboardingFeatureRow(symbol: "cylinder", title: HubL10n.text("hub.OnboardingWindowController.689.1946", fallback: "Keep your history here"),
                                 subtitle: HubL10n.text("hub.OnboardingWindowController.690.1947", fallback: "Store data locally on this Mac.")),
            onboardingFeatureRow(symbol: "square.and.arrow.down", title: HubL10n.text("hub.OnboardingWindowController.691.1949", fallback: "Bring your existing history"),
                                 subtitle: HubL10n.text("hub.OnboardingWindowController.692.1950", fallback: "Import from TeslaMate when you are ready."))
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
        let fresh = choiceButton(title: HubL10n.text("hub.OnboardingWindowController.736.1952", fallback: "New installation"),
                                 subtitle: HubL10n.text("hub.OnboardingWindowController.737.1953", fallback: "Start with a fresh database."),
                                 selected: state.path == .newInstallation,
                                 action: #selector(selectNewInstallation))
        let migration = choiceButton(title: HubL10n.text("hub.OnboardingWindowController.593.1922", fallback: "Migrate from TeslaMate"),
                                     subtitle: HubL10n.text("hub.OnboardingWindowController.741.1955", fallback: "Bring your existing vehicle history."),
                                     selected: state.path == .migration,
                                     action: #selector(selectMigration))
        return verticalChoices([fresh, migration])
    }

    private func providerBody() -> NSView {
        let fleet = choiceButton(title: HubL10n.text("hub.OnboardingWindowController.748.1956", fallback: "Fleet Telemetry"),
                                 subtitle: HubL10n.text("hub.OnboardingWindowController.749.1957", fallback: "Tesla's official streaming API. Enables live vehicle commands."),
                                 selected: state.provider == .fleet,
                                 action: #selector(selectFleet))
        let legacy = choiceButton(title: HubL10n.text("hub.OnboardingWindowController.752.1958", fallback: "Legacy Token"),
                                  subtitle: HubL10n.text("hub.OnboardingWindowController.753.1959", fallback: "Use an owner-API access and refresh token pair."),
                                  selected: state.provider == .legacy,
                                  action: #selector(selectLegacy))
        return verticalChoices([fleet, legacy])
    }

    private func fleetBody() -> NSView {
        let guide = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.760.1960", fallback: "Create Tesla Fleet App"), target: self, action: #selector(openFleetGuide))
        configureFlatButton(guide, symbol: "book")
        fleetExpiry.widthAnchor.constraint(equalToConstant: 120).isActive = true
        let seconds = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.seconds_unit", fallback: "seconds"))
        seconds.font = HubTypography.body
        seconds.textColor = HubPalette.mutedForeground
        let expiryControl = NSStackView(views: [fleetExpiry, seconds, spacer()])
        expiryControl.alignment = .centerY
        expiryControl.spacing = 12
        let form = onboardingForm([
            (HubL10n.text("hub.OnboardingWindowController.region_label", fallback: "Region"), fleetRegion),
            (HubL10n.text("hub.OnboardingWindowController.771.1964", fallback: "Client ID"), fleetClientID),
            (HubL10n.text("hub.OnboardingWindowController.474.1900", fallback: "Access token"), fleetAccessToken),
            (HubL10n.text("hub.OnboardingWindowController.481.1904", fallback: "Refresh token"), fleetRefreshToken),
            (HubL10n.text("hub.OnboardingWindowController.774.1967", fallback: "Expires in"), expiryControl)
        ])
        let note = featureRow(HubL10n.text("hub.OnboardingWindowController.776.1968", fallback: "Credentials are encrypted on this Mac"), "lock.fill")
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
        let signIn = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.788.1970", fallback: "Sign in with Tesla"), target: self, action: #selector(startLegacySignIn))
        configurePrimaryButton(signIn, symbol: "person.crop.circle.badge.checkmark")
        let or = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.790.1972", fallback: "or use an existing token pair"))
        or.textColor = .secondaryLabelColor
        or.alignment = .center
        let form = onboardingForm([
            (HubL10n.text("hub.OnboardingWindowController.474.1900", fallback: "Access token"), legacyAccessToken),
            (HubL10n.text("hub.OnboardingWindowController.481.1904", fallback: "Refresh token"), legacyRefreshToken)
        ])
        let stack = NSStackView(views: [signIn, or, form,
                                       featureRow(HubL10n.text("hub.OnboardingWindowController.798.1975", fallback: "Tokens are encrypted on this Mac"), "lock.fill")])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        for field in stack.arrangedSubviews.dropFirst(2) {
            field.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
    }

    private func migrationBody() -> NSView {
        if busyOperation == .importing {
            return migrationProgressBody()
        }
        if isPreviewConnectedMigration {
            return connectedMigrationPreviewBody()
        }
        if migrationUseLocalHistory.state == .on {
            return localHistoryBody()
        }
        if let migrationSession {
            return connectedMigrationBody(migrationSession)
        }
        migrationPort.widthAnchor.constraint(equalToConstant: 72).isActive = true
        let portLabel = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.822.1978", fallback: "Port"))
        portLabel.font = HubTypography.label
        portLabel.textColor = HubPalette.mutedForeground
        let serverPort = NSStackView(views: [migrationServer, portLabel, migrationPort])
        serverPort.spacing = 10
        serverPort.alignment = .centerY
        serverPort.distribution = .fill
        migrationServer.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        var fields: [(String, NSView)] = [(HubL10n.text("hub.OnboardingWindowController.server_label", fallback: "Server"), serverPort),
                                         (HubL10n.text("hub.OnboardingWindowController.832.1980", fallback: "SSH user"), migrationUser),
                                         (HubL10n.text("hub.OnboardingWindowController.authentication_label", fallback: "Authentication"), migrationAuthentication)]
        migrationKeyViews = [migrationServer, migrationUser, migrationPort, migrationAuthentication]
        if migrationAuthentication.indexOfSelectedItem == 0 {
            let choose = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.836.1982", fallback: "Choose Key…"), target: self, action: #selector(chooseMigrationIdentity))
            choose.identifier = NSUserInterfaceItemIdentifier("onboarding.choose-ssh-key")
            choose.hubFont = HubTypography.action
            choose.setContentCompressionResistancePriority(.required, for: .horizontal)
            let keyRow = NSStackView(views: [migrationIdentityFile, choose])
            keyRow.alignment = .centerY
            keyRow.spacing = 8
            migrationIdentityFile.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
            fields.append((HubL10n.text("hub.OnboardingWindowController.441.1895", fallback: "SSH key"), keyRow))
            migrationKeyViews.append(contentsOf: [migrationIdentityFile, choose])
        } else {
            fields.append((HubL10n.text("hub.OnboardingWindowController.password_label", fallback: "Password"), migrationSSHPassword))
            migrationKeyViews.append(migrationSSHPassword)
        }
        var views: [NSView] = [migrationUseLocalHistory, onboardingForm(fields)]
        views.append(wrappingCheckbox(
            migrationUseSudo,
            title: HubL10n.text("hub.OnboardingWindowController.446.1897", fallback: "This user needs sudo to read the TeslaMate database")
        ))
        migrationKeyViews.append(contentsOf: [migrationUseSudo, migrationUseLocalHistory])

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

    private func localHistoryBody() -> NSView {
        let choosePassword = HubActionButton(title: HubL10n.text("hub.ImportSheetController.110.1702", fallback: "Choose…"), target: self,
                                             action: #selector(chooseMigrationPassword))
        configureFlatButton(choosePassword)
        let passwordRow = NSStackView(views: [migrationPasswordFile, choosePassword])
        passwordRow.spacing = 8
        passwordRow.alignment = .centerY
        let note = NSTextField(wrappingLabelWithString:
            HubL10n.text("hub.OnboardingWindowController.888.1990", fallback: "History only. Hub will not inspect or import Tesla credentials. Collection remains off. The database schema can be checked; the TeslaMate application version remains unknown."))
        note.maximumNumberOfLines = 0
        let stack = NSStackView(views: [migrationUseLocalHistory,
            onboardingForm([(HubL10n.text("hub.OnboardingWindowController.891.1991", fallback: "Local PostgreSQL source"), migrationSource),
                            (HubL10n.text("hub.ImportSheetController.65.1695", fallback: "Car ID"), migrationCarID),
                            (HubL10n.text("hub.ImportSheetController.69.1697", fallback: "Password file"), passwordRow)]), note,
            wrappingCheckbox(migrationVersionAcknowledgement,
                title: HubL10n.text("hub.OnboardingWindowController.895.1994", fallback: "I acknowledge this is a v4.2-compatible database schema, not proof of the application version"))])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        for view in stack.arrangedSubviews { view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        if let compatibility {
            stack.addArrangedSubview(featureRow(compatibility.message,
                compatibility.compatible ? "checkmark.circle.fill" : "exclamationmark.triangle.fill",
                color: compatibility.compatible ? .systemGreen : .systemOrange))
        }
        migrationKeyViews = [migrationUseLocalHistory, migrationSource, migrationCarID,
                             migrationPasswordFile, choosePassword, migrationVersionAcknowledgement]
        return withError(stack)
    }

    private func connectedMigrationBody(_ session: TeslaMateServerImportSession) -> NSView {
        migrationConnectButton = nil
        let host = migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        var views: [NSView] = [migrationSuccessCard(host: host)]
        if let version = session.teslaMateVersion {
            views.append(featureRow(HubL10n.format("hub.OnboardingWindowController.915.1997", fallback: "TeslaMate %1$@", arguments: [String(describing: version)]), "info.circle", color: HubPalette.mutedForeground))
        }
        views.append(wrappingCheckbox(
            migrationVersionAcknowledgement,
            title: HubL10n.text("hub.OnboardingWindowController.203.1858", fallback: "I confirm this server runs TeslaMate 4.2.0 or newer")
        ))
        migrationKeyViews = [migrationVersionAcknowledgement]

        if busyOperation == .checkingCompatibility {
            spinner.style = .spinning
            spinner.controlSize = .small
            spinner.startAnimation(nil)
            let checking = NSStackView(views: [spinner, NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.927.2001", fallback: "Checking…"))])
            checking.spacing = 8
            checking.alignment = .centerY
            views.append(checking)
        } else if compatibility?.compatible == true {
            views.append(featureRow(HubL10n.text("hub.OnboardingWindowController.292.1862", fallback: "Ready to import."), "checkmark.circle", color: HubPalette.success))
        }

        let change = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.935.2004", fallback: "Change Server"), target: self,
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
                title: HubL10n.text("hub.OnboardingWindowController.203.1858", fallback: "I confirm this server runs TeslaMate 4.2.0 or newer")
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
        let title = NSTextField(labelWithString: HubL10n.format("hub.OnboardingWindowController.983.2008", fallback: "Connected to %1$@", arguments: [String(describing: host)]))
        title.font = HubTypography.emphasis
        let detail = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.985.2009", fallback: "Found a TeslaMate database ready to import."))
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

        let title = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.304.1870", fallback: "Importing data…"))
        title.font = HubTypography.heading
        title.textColor = HubPalette.foreground
        let subtitle = NSTextField(labelWithString: HubL10n.text("hub.OnboardingWindowController.1016.2012", fallback: "Copying your TeslaMate history into Hub."))
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
            let label = NSTextField(wrappingLabelWithString: HubL10n.format("hub.OnboardingWindowController.1070.2015", fallback: "• %1$@", arguments: [String(describing: suggestion)]))
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
                button = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1082.2016", fallback: "Choose Another Key…"), target: self,
                                  action: #selector(chooseMigrationIdentity))
            case .usePassword:
                button = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1085.2017", fallback: "Use Password"), target: self,
                                  action: #selector(useMigrationPassword))
            case .useKey:
                button = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1088.2018", fallback: "Use SSH Key"), target: self,
                                  action: #selector(useMigrationKey))
            case .openLogs:
                button = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1091.2019", fallback: "Open Logs"), target: self, action: #selector(openLogs))
            }
            configureFlatButton(button)
            buttons.append(button)
        }
        let copy = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1096.2020", fallback: "Copy Details"), target: self,
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
            let row = NSStackView(views: [spinner, NSTextField(labelWithString: HubL10n.text("hub.DiagnosticsWindowController.210.227", fallback: "Running checks…"))])
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
            description: check.passed ? HubL10n.text("hub.OnboardingWindowController.1165.2025", fallback: "Passed") : HubL10n.text("hub.OnboardingWindowController.1165.2026", fallback: "Failed")
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
        if state.path == .migration {
            let handoverTitle = migrationUseLocalHistory.state == .on
                ? HubL10n.text("hub.OnboardingWindowController.1198.2028", fallback: "I understand collection remains disabled; this Hub serves imported history only")
                : HubL10n.text("hub.OnboardingWindowController.1199.2029", fallback: "I have disabled Tesla access in TeslaMate to avoid duplicate requests")
            let acknowledgement = NSButton(checkboxWithTitle: "",
                                           target: self,
                                           action: #selector(handoverChanged(_:)))
            acknowledgement.state = handoverAcknowledged ? .on : .off
            acknowledgement.controlSize = .regular
            acknowledgement.setAccessibilityLabel(
                handoverTitle
            )
            let acknowledgementRow = wrappingCheckbox(
                acknowledgement,
                title: handoverTitle
            )
            stack.addArrangedSubview(acknowledgementRow)
            acknowledgementRow.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return withError(stack)
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
        footerSpinner.toolTip = HubL10n.text("hub.OnboardingWindowController.1250.2034", fallback: "Hub setup is working")
        let logs = HubActionButton(title: HubL10n.text("hub.OnboardingWindowController.1251.2035", fallback: "View Logs"), target: self, action: #selector(openLogs))
        configureFlatButton(logs)
        logs.controlSize = .regular
        footerLogsButton = logs
        let footer = NSView()
        let leading = NSStackView(views: [backButton, cancelButton])
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
            if migrationUseLocalHistory.state == .on {
                continueButton.isEnabled = localHistoryInputsValid && !blocked
            } else if isPreviewConnectedMigration {
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
            footerSpinner.toolTip = busyMessage ?? HubL10n.text("hub.OnboardingWindowController.1250.2034", fallback: "Hub setup is working")
            if focused {
                footerSpinner.stopAnimation(nil)
            } else {
                footerSpinner.startAnimation(nil)
            }
            if busyOperation == .connecting {
                migrationSpinner.toolTip = HubL10n.text("hub.OnboardingWindowController.1324.2038", fallback: "Connecting securely to TeslaMate")
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
            migrationConnectButton.title = busyOperation == .connecting
                ? (busyMessage ?? BusyOperation.connecting.message) : continueTitle
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
        case .welcome: return HubL10n.text("hub.OnboardingWindowController.1354.2041", fallback: "Get Started")
        case .fleet: return HubL10n.text("hub.OnboardingWindowController.1355.2042", fallback: "Set Up Fleet")
        case .legacy: return HubL10n.text("hub.MainWindowController.26.1772", fallback: "Connect Tesla")
        case .migration:
            if migrationUseLocalHistory.state == .on {
                return compatibility?.compatible == true ? HubL10n.text("hub.OnboardingWindowController.1359.2044", fallback: "Import History") : HubL10n.text("hub.OnboardingWindowController.1359.2045", fallback: "Check Snapshot")
            }
            return isPreviewConnectedMigration || migrationSession != nil
                ? HubL10n.text("hub.OnboardingWindowController.1362.2046", fallback: "Import Data") : HubL10n.text("hub.OnboardingWindowController.1362.2047", fallback: "Connect to Server")
        case .verify:
            return verificationFinished && !checks.isEmpty && checks.allSatisfy(\.passed)
                ? HubL10n.text("hub.OnboardingWindowController.168.1852", fallback: "Continue") : HubL10n.text("hub.DiagnosticsWindowController.21.204", fallback: "Run Again")
        case .finish: return HubL10n.text("hub.HubDashboardView.119.1362", fallback: "Start Hub")
        default: return HubL10n.text("hub.OnboardingWindowController.168.1852", fallback: "Continue")
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
            if migrationUseLocalHistory.state == .on {
                if compatibility?.compatible == true {
                    importLocalHistory()
                } else {
                    checkLocalHistory()
                }
            } else if isPreviewConnectedMigration {
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
            showInlineError(HubL10n.text("hub.OnboardingWindowController.1471.2059", fallback: "Complete the Fleet client ID, token, region, and expiry fields."))
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
        setBusy(true, operation: .setup)
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
                showInlineError(HubL10n.text("hub.OnboardingWindowController.1500.2071", fallback: "Enter both the access token and refresh token."))
                return
            }
            setBusy(true, operation: .setup)
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
                    self.setBusy(true, operation: .setup)
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

    private var localHistoryInputsValid: Bool {
        !migrationSource.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && Int64(migrationCarID.stringValue).map { $0 > 0 } == true
            && !migrationPasswordFile.stringValue.isEmpty
            && migrationVersionAcknowledgement.state == .on
    }

    @objc private func localHistorySelectionChanged() {
        migrationSession?.close()
        migrationSession = nil
        compatibility = nil
        connectedMigrationIdentity = nil
        migrationVersionAcknowledgement.state = .off
        render()
    }

    private func checkLocalHistory() {
        guard localHistoryInputsValid else { return }
        setBusy(true, operation: .checkingCompatibility)
        controller.checkTeslaMateCompatibility(source: migrationSource.stringValue,
            carID: migrationCarID.stringValue, passwordFile: migrationPasswordFile.stringValue,
            acknowledgeV42CompatibleSchema: true, historyOnly: true) { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case let .success(report):
                self.compatibility = report
            case let .failure(error):
                self.showInlineError(error.localizedDescription)
            }
            self.render()
        }
    }

    private func importLocalHistory() {
        guard localHistoryInputsValid, compatibility?.compatible == true else { return }
        startMigrationProgress()
        setBusy(true, operation: .importing)
        controller.importTeslaMateHistoryOnlyOnline(source: migrationSource.stringValue,
            carID: migrationCarID.stringValue, passwordFile: migrationPasswordFile.stringValue,
            acknowledgeV42CompatibleSchema: true,
            progress: { [weak self] update in self?.updateMigrationProgress(update) }) {
                [weak self] result in self?.setupFinished(result)
            }
    }

    @objc private func checkMigrationCompatibility() {
        let host = migrationServer.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let user = migrationUser.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let port = Int(migrationPort.stringValue), (1...65535).contains(port),
              !host.isEmpty, !user.isEmpty else {
            HubAppLog.shared.record("compatibility.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "missing_server_input"])
            showInlineError(HubL10n.text("hub.OnboardingWindowController.1631.2105", fallback: "Enter the TeslaMate server, SSH user, and port."))
            return
        }
        HubAppLog.shared.record("compatibility.started", category: "teslamate_import")
        setBusy(true, operation: .connecting)
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
                message: HubL10n.text("hub.OnboardingWindowController.1691.2113", fallback: "The database schema cannot distinguish TeslaMate 4.1.1 from 4.2.0. Confirm the running server is 4.2.0 or newer, then continue."),
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
            showInlineError(HubL10n.text("hub.OnboardingWindowController.1740.2131", fallback: "Connect to the TeslaMate server before importing."))
            return
        }
        guard connectedMigrationIdentity == currentMigrationIdentity else {
            HubAppLog.shared.record("import.rejected", category: "teslamate_import",
                                    level: "WARN", fields: ["reason": "settings_changed"])
            session.close()
            migrationSession = nil
            connectedMigrationIdentity = nil
            compatibility = nil
            showInlineError(HubL10n.text("hub.OnboardingWindowController.1750.2137", fallback: "Server settings changed. Connect to the TeslaMate server again before importing."))
            return
        }
        startMigrationProgress()
        setBusy(true, operation: .importing)
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
        busyOperation = .runningChecks
        busyMessage = BusyOperation.runningChecks.message
        errorMessage = nil
        render()
        controller.runOnboardingChecks(expectRunning: state.path == .newInstallation) { [weak self] result in
            DispatchQueue.main.async {
                guard let self else { return }
                self.setBusy(false)
                self.verificationFinished = true
                switch result {
                case let .success(checks):
                    self.checks = checks
                    self.errorMessage = checks.allSatisfy(\.passed) ? nil : HubL10n.text("hub.OnboardingWindowController.1820.2143", fallback: "One or more checks need attention.")
                case let .failure(error):
                    self.checks = []
                    self.errorMessage = error.localizedDescription
                }
                self.render()
            }
        }
    }

    private func finishOnboarding() {
        guard state.path == .migration else {
            onComplete(.configured)
            return
        }
        guard handoverAcknowledged else { return }
        setBusy(true, operation: .starting)
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
        logsReturnResponder = normalizedResponderView()
        let logs = LogsWindowController(controller: controller, embedded: true)
        logsWindow = logs
        let page = logs.makeEmbeddedPage { [weak self] in
            guard let self, let window = self.window else { return }
            window.contentView = self.onboardingContainer
            self.logsWindow = nil
            self.updateFooter()
            window.recalculateKeyViewLoop()
            self.configureKeyViewLoop(previousField: self.logsReturnResponder)
            self.logsReturnResponder = nil
            HubMotion.transition(self.onboardingContainer, forward: false)
        }
        window.contentView = page
        window.defaultButtonCell = nil
        (page as? HubEmbeddedUtilityPage)?.focusInitialResponder(in: window)
        HubMotion.transition(page, forward: true)
    }

    private func normalizedResponderView() -> NSView? {
        if let editor = window?.firstResponder as? NSTextView,
           let delegate = editor.delegate as? NSView {
            return delegate
        }
        return window?.firstResponder as? NSView
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
        if field === migrationSource || field === migrationCarID || field === migrationPasswordFile {
            compatibility = nil
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
        if migrationUseLocalHistory.state == .on {
            compatibility = nil
            render()
            return
        }
        guard let session = migrationSession else {
            render()
            return
        }
        compatibility = nil
        if migrationVersionAcknowledgement.state == .on {
            setBusy(true, operation: .checkingCompatibility)
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
            panel.title = HubL10n.text("hub.OnboardingWindowController.1980.2147", fallback: "Choose SSH Key")
            panel.prompt = HubL10n.text("hub.OnboardingWindowController.1981.2148", fallback: "Choose Key")
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
            if field === self.migrationPasswordFile {
                self.compatibility = nil
            }
            self.updateFooter()
        }
    }

    func setBusy(_ value: Bool, message: String? = nil, operation: BusyOperation? = nil) {
        let wasFocused = focusedOperation != nil
        let wasBusy = busy
        let previousMessage = busyMessage
        busy = value
        busyOperation = value ? operation : nil
        busyMessage = value ? (message ?? operation?.message) : nil
        updateWindowCloseAvailability()
        if wasFocused || focusedOperation != nil {
            render()
        } else {
            updateFooter()
        }
        let announcementSource: Any = onboardingContainer.map { $0 as Any } ?? self
        if value, !wasBusy {
            HubAccessibility.announce(busyMessage ?? "Setup operation started.", from: announcementSource)
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
        if !backButton.isHidden && backButton.isEnabled { keyViews.append(backButton) }
        if !cancelButton.isHidden && cancelButton.isEnabled { keyViews.append(cancelButton) }
        if !continueButton.isHidden && continueButton.isEnabled { keyViews.append(continueButton) }
        if let footerLogsButton, !footerLogsButton.isHidden, footerLogsButton.isEnabled {
            keyViews.append(footerLogsButton)
        }
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

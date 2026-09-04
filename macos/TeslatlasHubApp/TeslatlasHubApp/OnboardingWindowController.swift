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

private final class HubOnboardingChromeView: NSView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        updateLayer()
    }

    override func updateLayer() {
        layer?.backgroundColor = HubPalette.chrome.cgColor
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateLayer()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

private final class HubOnboardingHairlineView: NSView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        updateLayer()
    }

    override func updateLayer() {
        layer?.backgroundColor = HubPalette.hairline.cgColor
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateLayer()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

private final class HubOnboardingProgressMarkView: NSView {
    enum State {
        case complete
        case active
        case future
    }

    private let state: State

    init(state: State) {
        self.state = state
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("onboarding.progress-mark")
        wantsLayer = true
        layer?.cornerRadius = 3.5
        layer?.cornerCurve = .continuous
        updateLayer()
    }

    override func updateLayer() {
        switch state {
        case .complete:
            layer?.backgroundColor = HubPalette.accent.withAlphaComponent(0.6).cgColor
        case .active:
            layer?.backgroundColor = HubPalette.accent.cgColor
        case .future:
            layer?.backgroundColor = HubPalette.border.cgColor
        }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateLayer()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
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
    private let footerStatus = NSTextField(labelWithString: "Select an option to continue")
    private var footerLogsButton: NSButton?

    private let fleetClientID = NSTextField(string: "")
    private let fleetAccessToken = NSSecureTextField(string: "")
    private let fleetRefreshToken = NSSecureTextField(string: "")
    private let fleetExpiry = NSTextField(string: "3600")
    private let fleetRegion = NSPopUpButton()
    private let legacyAccessToken = NSSecureTextField(string: "")
    private let legacyRefreshToken = NSSecureTextField(string: "")

    private let migrationServer = NSTextField(string: "")
    private let migrationUser = NSTextField(string: "user")
    private let migrationPort = NSTextField(string: "22")
    private let migrationAuthentication = NSPopUpButton()
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
    private let migrationSource = NSTextField(string: "")
    private let migrationCarID = NSTextField(string: "")
    private let migrationPasswordFile = NSTextField(string: "")
    private let migrationKeyFile = NSTextField(string: "")
    private var migrationSession: TeslaMateServerImportSession?
    private var connectedMigrationIdentity: String?
    private var onboardingContainer: HubOnboardingContainerView!
    private let headerContentHost = NSView()

    init(controller: HubController,
         resumeMigrationHandoverPhase: HubMigrationHandoverPhase? = nil,
         initialRoute: HubOnboardingRoute? = nil,
         previewRoute: String? = nil,
         dismissalPolicy: HubOnboardingDismissalPolicy = .accountManagement,
         onDismiss: @escaping () -> Void = {},
         onComplete: @escaping (HubOnboardingCompletion) -> Void) {
        let effectivePreviewRoute = controller.previewMode ? previewRoute : nil
        self.controller = controller
        self.previewRoute = effectivePreviewRoute
        self.dismissalPolicy = dismissalPolicy
        self.onDismiss = onDismiss
        self.onComplete = onComplete
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
            contentSize: HubMetrics.onboardingSheetSize,
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
        case "migration": return HubOnboardingState(route: .migration, path: .migration)
        case "migration-connected": return HubOnboardingState(route: .migration, path: .migration)
        case "verify": return HubOnboardingState(route: .verify)
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
        let headerLine = HubOnboardingHairlineView()
        headerLine.translatesAutoresizingMaskIntoConstraints = false
        header.addSubview(headerLine)
        headerContentHost.translatesAutoresizingMaskIntoConstraints = false
        header.addSubview(headerContentHost)

        let footer = HubOnboardingChromeView()
        footer.identifier = NSUserInterfaceItemIdentifier("onboarding.footer")
        let footerLine = HubOnboardingHairlineView()
        footerLine.translatesAutoresizingMaskIntoConstraints = false
        footer.addSubview(footerLine)

        NSLayoutConstraint.activate([
            headerContentHost.leadingAnchor.constraint(equalTo: header.leadingAnchor, constant: 28),
            headerContentHost.trailingAnchor.constraint(equalTo: header.trailingAnchor, constant: -28),
            headerContentHost.topAnchor.constraint(equalTo: header.topAnchor),
            headerContentHost.bottomAnchor.constraint(equalTo: header.bottomAnchor),
            headerLine.leadingAnchor.constraint(equalTo: header.leadingAnchor),
            headerLine.trailingAnchor.constraint(equalTo: header.trailingAnchor),
            headerLine.bottomAnchor.constraint(equalTo: header.bottomAnchor),
            headerLine.heightAnchor.constraint(equalToConstant: 1),
            footerLine.leadingAnchor.constraint(equalTo: footer.leadingAnchor),
            footerLine.trailingAnchor.constraint(equalTo: footer.trailingAnchor),
            footerLine.topAnchor.constraint(equalTo: footer.topAnchor),
            footerLine.heightAnchor.constraint(equalToConstant: 1)
        ])

        onboardingContainer = HubOnboardingContainerView(headerView: header, footerView: footer)
        window.contentView = onboardingContainer
    }

    private func configureFields() {
        fleetRegion.addItems(withTitles: [
            "Europe, Middle East and Africa",
            "North America and Asia Pacific",
            "China"
        ])
        migrationAuthentication.addItems(withTitles: ["SSH key", "Password"])
        migrationAuthentication.target = self
        migrationAuthentication.action = #selector(migrationAuthenticationChanged)
        migrationAuthentication.controlSize = .regular
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
        for field in [fleetClientID, fleetAccessToken, fleetRefreshToken, fleetExpiry,
                      legacyAccessToken, legacyRefreshToken,
                      migrationServer, migrationUser, migrationPort,
                      migrationIdentityFile, migrationSSHPassword] {
            field.controlSize = .regular
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

    private func render() {
        guard let window else { return }
        let operation = focusedOperation
        window.setContentSize(currentSheetSize)
        let previousField = (window.firstResponder as? NSTextView)?.delegate as? NSView
        window.initialFirstResponder = nil
        window.makeFirstResponder(nil)
        migrationKeyViews = []
        headerContentHost.subviews.forEach { $0.removeFromSuperview() }
        let stepLabel = NSTextField(labelWithString: "Step \(state.step) of 5")
        stepLabel.font = .systemFont(ofSize: 11.5, weight: .medium)
        stepLabel.textColor = HubPalette.mutedForeground
        let headerContent = NSStackView(views: [stepLabel, spacer(), progressView()])
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
            let title = NSTextField(labelWithString: pageTitle)
            title.font = .systemFont(ofSize: 17.5, weight: .bold)
            title.textColor = HubPalette.foreground
            title.alignment = .left
            let subtitle = NSTextField(wrappingLabelWithString: pageSubtitle)
            subtitle.font = .systemFont(ofSize: 12)
            subtitle.textColor = HubPalette.mutedForeground
            subtitle.alignment = .left
            subtitle.maximumNumberOfLines = 2
            let body = pageBody()
            let page = NSStackView(views: [title, subtitle, body])
            page.identifier = NSUserInterfaceItemIdentifier(
                state.route == .welcome ? "onboarding.welcome.body" : "onboarding.body"
            )
            page.orientation = .vertical
            page.alignment = .leading
            page.spacing = 0
            page.setCustomSpacing(4, after: title)
            page.setCustomSpacing(state.route == .welcome ? 18 : 16, after: subtitle)
            NSLayoutConstraint.activate([
                subtitle.widthAnchor.constraint(equalTo: page.widthAnchor),
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
        if operation == nil {
            configureMigrationKeyViewLoop(previousField: previousField)
        }
        if HubUIPresentation.isSilentTestHost {
            onboardingContainer.alphaValue = 1
        } else {
            onboardingContainer.alphaValue = 0
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.18
                context.timingFunction = CAMediaTimingFunction(name: .easeOut)
                self.onboardingContainer.animator().alphaValue = 1
            }
        }
    }

    private var currentSheetSize: NSSize {
        let height: CGFloat
        if let operation = focusedOperation {
            switch operation {
            case .setup: height = 220
            case .importing: height = 235
            }
        } else {
            switch state.route {
            case .welcome: height = 282
            case .choose, .provider: height = 349
            case .fleet: height = 455
            case .legacy: height = 390
            case .migration: height = isPreviewConnectedMigration || migrationSession != nil ? 271 : 393
            case .verify: height = 498
            case .finish: height = 280
            }
        }
        return NSSize(width: 485, height: height)
    }

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
            title.font = .systemFont(ofSize: 17.5, weight: .bold)
            title.textColor = HubPalette.foreground
            let subtitle = NSTextField(labelWithString: "Saving your connection and preparing Hub.")
            subtitle.font = .systemFont(ofSize: 12)
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
        case .welcome: return "Teslatlas Hub"
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
        let rows = NSStackView(views: [
            welcomeFeatureRow("Written in Rust for a small, fast, single binary"),
            welcomeFeatureRow("No Docker required — runs as a native service"),
            welcomeFeatureRow("First-class on macOS and Debian Linux"),
            welcomeFeatureRow("Stores vehicle data in a local SQLite database")
        ])
        rows.orientation = .vertical
        rows.alignment = .leading
        rows.spacing = 9
        return rows
    }

    private func welcomeFeatureRow(_ title: String) -> NSView {
        let image = NSImageView(image: symbolImage("checkmark.circle", description: title))
        image.identifier = NSUserInterfaceItemIdentifier("onboarding.welcome.feature-icon")
        image.image = image.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 16, weight: .medium)
        )
        image.contentTintColor = HubPalette.success
        image.translatesAutoresizingMaskIntoConstraints = false
        image.widthAnchor.constraint(equalToConstant: 16).isActive = true
        image.heightAnchor.constraint(equalToConstant: 16).isActive = true
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 12)
        label.textColor = HubPalette.foreground
        let row = NSStackView(views: [image, label])
        row.spacing = 9
        row.alignment = .centerY
        return row
    }

    private func chooseBody() -> NSView {
        let fresh = choiceButton(title: "New installation",
                                 subtitle: "Connect a Tesla account and start collecting data with a clean database.",
                                 symbol: "sparkles",
                                 accentColor: HubPalette.accent,
                                 selected: state.path == .newInstallation,
                                 action: #selector(selectNewInstallation))
        let migration = choiceButton(title: "Migrate from TeslaMate",
                                     subtitle: "Import your existing vehicle history from a TeslaMate server over SSH.",
                                     symbol: "cylinder",
                                     accentColor: HubPalette.accent,
                                     selected: state.path == .migration,
                                     action: #selector(selectMigration))
        return verticalChoices([fresh, migration])
    }

    private func providerBody() -> NSView {
        let fleet = choiceButton(title: "Fleet Telemetry",
                                 subtitle: "Tesla's official streaming API. Enables live vehicle commands.",
                                 symbol: "checkmark.shield",
                                 accentColor: HubPalette.accent,
                                 selected: state.provider == .fleet,
                                 action: #selector(selectFleet))
        let legacy = choiceButton(title: "Legacy Token",
                                  subtitle: "Use an owner-API access and refresh token pair.",
                                  symbol: "key",
                                  accentColor: HubPalette.accent,
                                  selected: state.provider == .legacy,
                                  action: #selector(selectLegacy))
        return verticalChoices([fleet, legacy])
    }

    private func fleetBody() -> NSView {
        let guide = HubActionButton(title: "Create Tesla Fleet App", target: self, action: #selector(openFleetGuide))
        configureFlatButton(guide, symbol: "book")
        let fields = NSStackView(views: [
            guide,
            verticalField("Region", fleetRegion),
            verticalField("Client ID", fleetClientID),
            verticalField("Access token", fleetAccessToken),
            verticalField("Refresh token", fleetRefreshToken),
            verticalField("Expires in (seconds)", fleetExpiry)
        ])
        fields.orientation = .vertical
        fields.alignment = .leading
        fields.spacing = 7
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
        let stack = NSStackView(views: [
            signIn,
            or,
            verticalField("Access token", legacyAccessToken),
            verticalField("Refresh token", legacyRefreshToken),
            featureRow("Tokens are encrypted on this Mac", "lock.fill")
        ])
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
        let server = verticalField("Server", migrationServer, width: nil)
        let port = verticalField("Port", migrationPort, width: 72)
        let serverPort = NSStackView(views: [server, port])
        serverPort.spacing = 10
        serverPort.alignment = .bottom
        serverPort.distribution = .fill
        port.widthAnchor.constraint(equalToConstant: 72).isActive = true
        server.widthAnchor.constraint(equalTo: serverPort.widthAnchor, constant: -82).isActive = true
        server.setContentHuggingPriority(.defaultLow, for: .horizontal)
        migrationServer.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        var views: [NSView] = [serverPort,
                               verticalField("SSH user", migrationUser, width: nil),
                               verticalField("Authentication", migrationAuthentication, width: nil)]
        migrationKeyViews = [migrationServer, migrationUser, migrationPort, migrationAuthentication]
        if migrationAuthentication.indexOfSelectedItem == 0 {
            views.append(verticalField("SSH key", migrationIdentityFile, width: nil))
            migrationKeyViews.append(migrationIdentityFile)
        } else {
            views.append(verticalField("Password", migrationSSHPassword, width: nil))
            migrationKeyViews.append(migrationSSHPassword)
        }
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
        let icon = NSImageView(image: symbolImage("checkmark.circle", description: "Connected"))
        icon.image = icon.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 16, weight: .medium)
        )
        icon.contentTintColor = HubPalette.success
        icon.widthAnchor.constraint(equalToConstant: 16).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 16).isActive = true
        let title = NSTextField(labelWithString: "Connected to \(host)")
        title.font = .systemFont(ofSize: 12.5, weight: .semibold)
        let detail = NSTextField(labelWithString: "Found a TeslaMate database ready to import.")
        detail.font = .systemFont(ofSize: 11.5)
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
        migrationProgress.style = .bar
        migrationProgress.isIndeterminate = false
        migrationProgress.controlSize = .regular

        let title = NSTextField(labelWithString: "Importing data…")
        title.font = .systemFont(ofSize: 17.5, weight: .bold)
        title.textColor = HubPalette.foreground
        let subtitle = NSTextField(labelWithString: "Copying your TeslaMate history into Hub.")
        subtitle.font = .systemFont(ofSize: 12)
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
    }

    private func resetMigrationProgress() {
        migrationProgress.stopAnimation(nil)
        migrationProgress.minValue = 0
        migrationProgress.maxValue = 1
        migrationProgress.doubleValue = 0
    }

    private func migrationDiagnosticView(_ diagnostic: TeslaMateSSHDiagnostic) -> NSView {
        let title = NSTextField(labelWithString: diagnostic.title)
        title.font = .systemFont(ofSize: 15, weight: .semibold)
        title.textColor = .systemOrange

        let summary = NSTextField(wrappingLabelWithString: diagnostic.summary)
        summary.maximumNumberOfLines = 0

        let stack = NSStackView(views: [title, summary])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 6
        for suggestion in diagnostic.suggestions {
            let label = NSTextField(wrappingLabelWithString: "• \(suggestion)")
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
        let buttonRow = NSStackView(views: buttons)
        buttonRow.spacing = 12
        buttonRow.alignment = .centerY
        stack.addArrangedSubview(buttonRow)
        for view in stack.arrangedSubviews {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
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
        title.font = .systemFont(ofSize: 12.5, weight: .semibold)
        let detail = NSTextField(labelWithString: check.detail)
        detail.font = .systemFont(ofSize: 11.5)
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
        let icon = NSImageView(image: symbolImage("checkmark.circle", description: "Complete"))
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
        var views: [NSView] = []
        for index in 1...5 {
            let mark = HubOnboardingProgressMarkView(
                state: index < state.step ? .complete : (index == state.step ? .active : .future)
            )
            mark.setAccessibilityLabel("Step \(index)")
            mark.widthAnchor.constraint(equalToConstant: index == state.step ? 20 : 7).isActive = true
            mark.heightAnchor.constraint(equalToConstant: 7).isActive = true
            views.append(mark)
        }
        let stack = NSStackView(views: views)
        stack.spacing = 7
        stack.alignment = .centerY
        return stack
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
        backButton.image = NSImage(systemSymbolName: "chevron.left", accessibilityDescription: "Back")
        backButton.imagePosition = .imageLeading
        backButton.controlSize = .regular
        backWidthConstraint?.isActive = false
        backWidthConstraint = backButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 52)
        backWidthConstraint?.isActive = true
        continueButton.target = self
        continueButton.action = #selector(continuePressed)
        configurePrimaryButton(continueButton)
        continueButton.controlSize = .regular
        continueButton.hubFont = .systemFont(ofSize: 12, weight: .medium)
        continueWidthConstraint?.isActive = false
        continueWidthConstraint = continueButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 76)
        continueWidthConstraint?.isActive = true
        continueHeightConstraint?.isActive = false
        continueHeightConstraint = continueButton.heightAnchor.constraint(equalToConstant: 24)
        continueHeightConstraint?.isActive = true

        footerSpinner.style = .spinning
        footerSpinner.controlSize = .small
        footerSpinner.isDisplayedWhenStopped = false
        footerSpinner.toolTip = "Hub setup is working"
        footerStatus.font = .systemFont(ofSize: 11.5)
        footerStatus.textColor = HubPalette.mutedForeground
        let logs = HubActionButton(title: "View Logs", target: self, action: #selector(openLogs))
        configureFlatButton(logs)
        logs.controlSize = .regular
        footerLogsButton = logs
        var footerViews: [NSView] = [cancelButton, backButton, spacer(), footerStatus, logs]
        if focusedOperation == nil {
            footerViews.append(footerSpinner)
        }
        footerViews.append(continueButton)
        let footer = NSStackView(views: footerViews)
        footer.spacing = 10
        footer.alignment = .centerY
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
        footerStatus.isHidden = state.route != .choose && state.route != .provider
        footerLogsButton?.isHidden = state.route != .verify || !verificationFinished
        footerLogsButton?.isEnabled = !blocked
        continueButton.isHidden = importing
            || state.route == .choose
            || state.route == .provider
        // The focused operation body owns its status copy. Keep the footer's
        // control label stable so shared chrome does not repeat that status.
        continueButton.title = busy && !focused ? (busyMessage ?? continueTitle) : continueTitle
        continueButton.image = nil
        switch state.route {
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
        backWidthConstraint?.constant = max(52, ceil(backButton.intrinsicContentSize.width) + 12)
        continueWidthConstraint?.constant = max(76, ceil(continueButton.intrinsicContentSize.width) + 20)
        window?.defaultButtonCell = blocked || continueButton.isHidden || !continueButton.isEnabled
            ? nil
            : continueButton.cell as? NSButtonCell
    }

    private var continueTitle: String {
        switch state.route {
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
        state.advance()
        render()
    }

    @objc private func selectMigration() {
        state.path = .migration
        state.advance()
        render()
    }

    @objc private func selectFleet() {
        state.provider = .fleet
        state.advance()
        render()
    }

    @objc private func selectLegacy() {
        state.provider = .legacy
        state.advance()
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
        if let logsWindow {
            logsWindow.refresh()
            logsWindow.showWindow(nil)
            logsWindow.window?.makeKeyAndOrderFront(nil)
            return
        }
        logsWindow = LogsWindowController(controller: controller)
        logsWindow?.showWindow(nil)
        logsWindow?.window?.makeKeyAndOrderFront(nil)
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
        guard let field = notification.object as? NSTextField,
              field === migrationServer || field === migrationUser || field === migrationPort else {
            return
        }
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
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.beginSheetModal(for: window!) { response in
            if response == .OK { field.stringValue = panel.url?.path ?? "" }
        }
    }

    func setBusy(_ value: Bool, message: String? = nil) {
        let wasFocused = focusedOperation != nil
        busy = value
        busyMessage = value ? message : nil
        updateWindowCloseAvailability()
        if wasFocused || focusedOperation != nil {
            render()
        } else {
            updateFooter()
        }
    }

    private func updateWindowCloseAvailability() {
        window?.standardWindowButton(.closeButton)?.isEnabled = !closeBlocked
    }

    private var interactionBlocked: Bool { busy || authWindow != nil }

    private var closeBlocked: Bool { interactionBlocked || controller.hasPendingMigrationHandover }

    private func showInlineError(_ message: String) {
        setBusy(false)
        errorMessage = message
        render()
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
        stack.spacing = 10
        stack.distribution = .fill
        stack.alignment = .leading
        for choice in choices {
            choice.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
            choice.heightAnchor.constraint(equalToConstant: 77).isActive = true
        }
        return stack
    }

    private func choiceButton(title: String,
                              subtitle: String,
                              symbol: String,
                              accentColor: NSColor,
                              selected: Bool,
                              action: Selector) -> NSView {
        let card = HubCardView()

        let icon = NSImageView(image: symbolImage(symbol, description: title))
        icon.image = icon.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 16, weight: .medium)
        )
        icon.contentTintColor = accentColor
        icon.imageScaling = .scaleProportionallyDown
        let iconTile = NSView()
        iconTile.wantsLayer = true
        iconTile.layer?.backgroundColor = HubPalette.elevated.cgColor
        iconTile.layer?.cornerRadius = 9
        icon.translatesAutoresizingMaskIntoConstraints = false
        iconTile.addSubview(icon)
        NSLayoutConstraint.activate([
            iconTile.widthAnchor.constraint(equalToConstant: 35),
            iconTile.heightAnchor.constraint(equalToConstant: 35),
            icon.widthAnchor.constraint(equalToConstant: 16),
            icon.heightAnchor.constraint(equalToConstant: 16),
            icon.centerXAnchor.constraint(equalTo: iconTile.centerXAnchor),
            icon.centerYAnchor.constraint(equalTo: iconTile.centerYAnchor)
        ])

        let heading = NSTextField(labelWithString: title)
        heading.font = .systemFont(ofSize: 12.5, weight: .semibold)
        heading.alignment = .left
        let detail = NSTextField(wrappingLabelWithString: subtitle)
        detail.font = .systemFont(ofSize: 11.5)
        detail.textColor = HubPalette.mutedForeground
        detail.alignment = .left
        detail.maximumNumberOfLines = 2

        let copy = NSStackView(views: [heading, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 3
        detail.widthAnchor.constraint(equalTo: copy.widthAnchor).isActive = true
        let content = NSStackView(views: [iconTile, copy])
        content.orientation = .horizontal
        content.alignment = .centerY
        content.spacing = 12
        content.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(content)
        copy.widthAnchor.constraint(equalTo: card.widthAnchor, constant: -75).isActive = true

        let button = NSButton(title: title, target: self, action: action)
        button.isBordered = false
        button.isTransparent = true
        button.toolTip = subtitle
        button.setAccessibilityLabel(title)
        button.setAccessibilityValue(selected ? "Selected" : "Not selected")
        button.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(button)

        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            content.trailingAnchor.constraint(lessThanOrEqualTo: card.trailingAnchor, constant: -14),
            content.centerYAnchor.constraint(equalTo: card.centerYAnchor),
            button.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            button.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            button.topAnchor.constraint(equalTo: card.topAnchor),
            button.bottomAnchor.constraint(equalTo: card.bottomAnchor)
        ])
        return card
    }

    private func verticalField(_ title: String, _ field: NSView, width: CGFloat? = nil) -> NSView {
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 11.5, weight: .medium)
        label.textColor = HubPalette.mutedForeground
        if let width {
            field.widthAnchor.constraint(equalToConstant: width).isActive = true
        }
        if let control = field as? NSControl {
            control.controlSize = .regular
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
        let image = NSImageView(image: symbolImage(symbol, description: title))
        image.image = image.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 24, weight: .medium)
        )
        image.contentTintColor = color
        image.widthAnchor.constraint(equalToConstant: 30).isActive = true
        image.heightAnchor.constraint(equalToConstant: 30).isActive = true
        let label = NSTextField(wrappingLabelWithString: title)
        label.font = .systemFont(ofSize: 14)
        label.maximumNumberOfLines = 2
        let row = NSStackView(views: [image, label])
        row.spacing = 12
        row.alignment = .centerY
        return row
    }

    private func configureMigrationKeyViewLoop(previousField: NSView?) {
        guard state.route == .migration, let window else { return }
        var keyViews = migrationKeyViews.filter { view in
            !view.isHidden && (view as? NSControl)?.isEnabled != false
        }
        if !cancelButton.isHidden && cancelButton.isEnabled { keyViews.append(cancelButton) }
        if !backButton.isHidden && backButton.isEnabled { keyViews.append(backButton) }
        if !continueButton.isHidden { keyViews.append(continueButton) }
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
        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.widthAnchor.constraint(equalToConstant: 120).isActive = true
        field.widthAnchor.constraint(greaterThanOrEqualToConstant: 348).isActive = true
        let row = NSStackView(views: [label, field])
        row.spacing = 12
        row.alignment = .centerY
        return row
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
        button.font = .systemFont(ofSize: 13, weight: .medium)
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
        button.font = .systemFont(ofSize: 13, weight: .semibold)
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

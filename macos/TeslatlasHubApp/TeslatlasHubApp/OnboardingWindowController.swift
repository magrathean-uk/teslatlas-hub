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

final class OnboardingWindowController: NSWindowController, NSWindowDelegate {
    private let controller: HubController
    private let onDismiss: () -> Void
    private let onComplete: () -> Void
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
    private let spinner = NSProgressIndicator()
    private let migrationSpinner = NSProgressIndicator()
    private let footerSpinner = NSProgressIndicator()
    private var continueWidthConstraint: NSLayoutConstraint?
    private var backWidthConstraint: NSLayoutConstraint?

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
    private var migrationConnectButton: NSButton?
    private var migrationKeyViews: [NSView] = []
    private let migrationSource = NSTextField(string: "")
    private let migrationCarID = NSTextField(string: "")
    private let migrationPasswordFile = NSTextField(string: "")
    private let migrationKeyFile = NSTextField(string: "")
    private var migrationSession: TeslaMateServerImportSession?
    private var connectedMigrationIdentity: String?

    init(controller: HubController,
         resumeMigrationHandoverPhase: HubMigrationHandoverPhase? = nil,
         initialRoute: HubOnboardingRoute? = nil,
         previewRoute: String? = nil,
         onDismiss: @escaping () -> Void = {},
         onComplete: @escaping () -> Void) {
        self.controller = controller
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
            state = Self.previewState(route: previewRoute)
                ?? initialRoute.map { Self.initialState(route: $0) }
                ?? HubOnboardingState()
            shouldAutoVerify = false
            resumeMessage = nil
        }
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 630),
                              styleMask: [.titled, .closable, .miniaturizable],
                              backing: .buffered,
                              defer: false)
        window.title = "Teslatlas Hub"
        window.titlebarAppearsTransparent = false
        window.center()
        super.init(window: window)
        window.delegate = self
        errorMessage = resumeMessage
        configureTitlebar(window)
        configureFields()
        render()
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
        case "verify": return HubOnboardingState(route: .verify)
        case "finish": return HubOnboardingState(route: .finish)
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

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard !interactionBlocked else {
            NSSound.beep()
            return false
        }
        return true
    }

    func windowWillClose(_ notification: Notification) {
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
        checks = []
        verificationFinished = false
        handoverAcknowledged = false
        render()
    }

    private func configureTitlebar(_ window: NSWindow) {
        window.titleVisibility = .hidden
        guard let titlebar = window.standardWindowButton(.closeButton)?.superview else { return }
        let title = NSTextField(labelWithString: "Teslatlas Hub")
        title.font = .systemFont(ofSize: 13, weight: .semibold)
        title.translatesAutoresizingMaskIntoConstraints = false
        titlebar.addSubview(title)
        NSLayoutConstraint.activate([
            title.centerXAnchor.constraint(equalTo: titlebar.centerXAnchor),
            title.centerYAnchor.constraint(equalTo: titlebar.centerYAnchor)
        ])
    }

    private func configureFields() {
        fleetRegion.addItems(withTitles: [
            "Europe, Middle East and Africa",
            "North America and Asia Pacific",
            "China"
        ])
        migrationAuthentication.addItems(withTitles: ["SSH config, agent, or key", "Password"])
        migrationAuthentication.target = self
        migrationAuthentication.action = #selector(migrationAuthenticationChanged)
        migrationAuthentication.controlSize = .large
        migrationUseSudo.state = .on
        for field in [fleetClientID, fleetAccessToken, fleetRefreshToken, fleetExpiry,
                      legacyAccessToken, legacyRefreshToken,
                      migrationServer, migrationUser, migrationPort,
                      migrationIdentityFile, migrationSSHPassword] {
            field.controlSize = .large
        }
        fleetAccessToken.placeholderString = "Access token"
        fleetRefreshToken.placeholderString = "Refresh token"
        fleetClientID.placeholderString = "Tesla application client ID"
        legacyAccessToken.placeholderString = "Access token"
        legacyRefreshToken.placeholderString = "Refresh token"
        migrationServer.placeholderString = "Server name or IP address"
        migrationIdentityFile.placeholderString = "Optional — uses SSH agent or default keys"
        migrationSSHPassword.placeholderString = "SSH password"
    }

    private func render() {
        guard let window else { return }
        let previousField = (window.firstResponder as? NSTextView)?.delegate as? NSView
        migrationKeyViews = []
        let root = NSView()
        root.wantsLayer = true
        root.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        let page = NSStackView()
        page.orientation = .vertical
        page.alignment = .centerX
        page.spacing = 12
        page.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(page)

        let icon = NSImageView(image: appIcon())
        icon.imageScaling = .scaleProportionallyUpOrDown
        let iconSize: CGFloat = state.route == .welcome ? 104 : 68
        icon.widthAnchor.constraint(equalToConstant: iconSize).isActive = true
        icon.heightAnchor.constraint(equalTo: icon.widthAnchor).isActive = true
        icon.wantsLayer = true
        icon.layer?.cornerRadius = iconSize * 0.22
        icon.layer?.cornerCurve = .continuous
        icon.layer?.masksToBounds = true
        page.addArrangedSubview(icon)

        page.addArrangedSubview(progressView())
        let stepLabel = NSTextField(labelWithString: "Step \(state.step) of 5")
        stepLabel.font = .systemFont(ofSize: 12, weight: .medium)
        stepLabel.textColor = .secondaryLabelColor
        page.addArrangedSubview(stepLabel)
        page.setCustomSpacing(24, after: page.arrangedSubviews.last!)

        let title = NSTextField(labelWithString: pageTitle)
        title.font = .systemFont(ofSize: 26, weight: .semibold)
        title.alignment = .center
        page.addArrangedSubview(title)

        let subtitle = NSTextField(wrappingLabelWithString: pageSubtitle)
        subtitle.font = .systemFont(ofSize: 14)
        subtitle.textColor = .secondaryLabelColor
        subtitle.alignment = .center
        subtitle.maximumNumberOfLines = 2
        subtitle.widthAnchor.constraint(lessThanOrEqualToConstant: 650).isActive = true
        page.addArrangedSubview(subtitle)
        page.setCustomSpacing(22, after: subtitle)

        let body = pageBody()
        page.addArrangedSubview(body)
        body.widthAnchor.constraint(equalToConstant: 650).isActive = true
        body.heightAnchor.constraint(greaterThanOrEqualToConstant: 250).isActive = true

        let footerLine = separator()
        footerLine.translatesAutoresizingMaskIntoConstraints = false
        let footer = footerView()
        footer.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(footerLine)
        root.addSubview(footer)

        NSLayoutConstraint.activate([
            page.topAnchor.constraint(equalTo: root.topAnchor, constant: 34),
            page.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            page.bottomAnchor.constraint(lessThanOrEqualTo: footerLine.topAnchor, constant: -18),
            footerLine.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            footerLine.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            footerLine.bottomAnchor.constraint(equalTo: footer.topAnchor, constant: -10),
            footer.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 38),
            footer.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -38),
            footer.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -14),
            footer.heightAnchor.constraint(equalToConstant: 34)
        ])
        root.alphaValue = 0
        window.contentView = root
        updateFooter()
        window.recalculateKeyViewLoop()
        configureMigrationKeyViewLoop(previousField: previousField)
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.18
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            root.animator().alphaValue = 1
        }
    }

    private var pageTitle: String {
        switch state.route {
        case .welcome: return "Teslatlas Hub"
        case .choose: return "How would you like to begin?"
        case .provider: return "Choose how Hub connects to Tesla"
        case .fleet: return "Set up Fleet Telemetry"
        case .legacy: return "Connect with a Legacy Token"
        case .migration: return "Import from TeslaMate"
        case .verify: return "Checking your Hub"
        case .finish: return state.path == .migration ? "Migration complete" : "Teslatlas Hub is ready"
        }
    }

    private var pageSubtitle: String {
        switch state.route {
        case .welcome:
            return "Teslatlas Hub is a self-hosted backend for Teslatlas. It collects selected Tesla telemetry and can import supported TeslaMate history."
        case .choose:
            return "Choose how you would like to set up Teslatlas Hub."
        case .provider:
            return "Choose official Fleet Telemetry or a free Legacy Token."
        case .fleet:
            return "Sign in with Tesla and create your Fleet application, then enter its credentials."
        case .legacy:
            return "Sign in with Tesla, or paste an existing token pair. Hub encrypts it on this Mac."
        case .migration:
            return "Enter your TeslaMate server. Hub finds and copies the required data automatically."
        case .verify:
            return "Service, account, vehicle, database, diagnostics, and logs are checked before handover."
        case .finish:
            return state.path == .migration
                ? "TeslaMate was not stopped, removed, or changed. Keep it available for rollback."
                : "Collection is running. You can open the Hub dashboard now."
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
            featureRow("Written purely in Rust.", "chevron.left.forwardslash.chevron.right"),
            featureRow("No Docker.", "shippingbox.fill", color: .systemBlue),
            featureRow("Developed natively for macOS and Debian.",
                       "desktopcomputer", color: .systemGreen),
            featureRow("Uses SQLite.", "cylinder.fill", color: .systemPurple)
        ])
        rows.orientation = .vertical
        rows.alignment = .leading
        rows.spacing = 18
        return centered(rows)
    }

    private func chooseBody() -> NSView {
        let fresh = choiceButton(title: "New installation",
                                 subtitle: "Connect Tesla Fleet Telemetry or Legacy Token.",
                                 symbol: "car.fill",
                                 accentColor: .systemGreen,
                                 selected: state.path == .newInstallation,
                                 action: #selector(selectNewInstallation))
        let migration = choiceButton(title: "Migrate from TeslaMate",
                                     subtitle: "Bring your existing drives and charging history.",
                                     symbol: "cylinder.split.1x2",
                                     accentColor: .systemBlue,
                                     selected: state.path == .migration,
                                     action: #selector(selectMigration))
        return horizontalChoices([fresh, migration])
    }

    private func providerBody() -> NSView {
        let fleet = choiceButton(title: "Fleet Telemetry",
                                 subtitle: "Official Tesla setup · low-cost streaming",
                                 symbol: "checkmark.shield.fill",
                                 accentColor: .systemBlue,
                                 selected: state.provider == .fleet,
                                 action: #selector(selectFleet))
        let legacy = choiceButton(title: "Legacy Token",
                                  subtitle: "Free token login or use an existing token",
                                  symbol: "key.fill",
                                  accentColor: .systemPurple,
                                  selected: state.provider == .legacy,
                                  action: #selector(selectLegacy))
        return horizontalChoices([fleet, legacy])
    }

    private func fleetBody() -> NSView {
        let guide = HubActionButton(title: "Create Tesla Fleet App", target: self, action: #selector(openFleetGuide))
        configureFlatButton(guide, symbol: "book")
        let fields = NSStackView(views: [
            guide,
            formRow("Region", fleetRegion),
            formRow("Client ID", fleetClientID),
            formRow("Access token", fleetAccessToken),
            formRow("Refresh token", fleetRefreshToken),
            formRow("Expires in seconds", fleetExpiry)
        ])
        fields.orientation = .vertical
        fields.alignment = .leading
        fields.spacing = 9
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
            formRow("Access token", legacyAccessToken),
            formRow("Refresh token", legacyRefreshToken),
            featureRow("Tokens are encrypted on this Mac", "lock.fill")
        ])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 14
        return withError(stack)
    }

    private func migrationBody() -> NSView {
        let check = HubActionButton(title: "Connect to Server", target: self, action: #selector(checkMigrationCompatibility))
        migrationConnectButton = check
        configurePrimaryButton(check)
        check.controlSize = .large
        check.widthAnchor.constraint(equalToConstant: 230).isActive = true
        check.heightAnchor.constraint(equalToConstant: 38).isActive = true

        migrationSpinner.style = .spinning
        migrationSpinner.controlSize = .small
        migrationSpinner.isDisplayedWhenStopped = false
        migrationSpinner.toolTip = "Connecting securely to TeslaMate"

        let guidance = NSTextField(wrappingLabelWithString:
            "Use a normal server account. It must access the TeslaMate containers directly or through passwordless sudo.")
        guidance.textColor = .secondaryLabelColor
        guidance.maximumNumberOfLines = 2
        guidance.widthAnchor.constraint(equalToConstant: 650).isActive = true

        var views: [NSView] = [
            guidance,
            formRow("Server", migrationServer),
            formRow("SSH user (optional)", migrationUser),
            formRow("SSH port", migrationPort),
            formRow("Authentication", migrationAuthentication)
        ]
        migrationKeyViews = [migrationServer, migrationUser, migrationPort, migrationAuthentication]
        if migrationAuthentication.indexOfSelectedItem == 0 {
            let choose = HubActionButton(title: "Choose Key…", target: self, action: #selector(chooseMigrationIdentity))
            views.append(formRow("SSH key", fieldWithButton(migrationIdentityFile, choose, symbol: "key.fill")))
            let automatic = NSTextField(wrappingLabelWithString:
                "Optional. Hub automatically uses ~/.ssh/config, ssh-agent, ProxyJump, and standard private keys.")
            automatic.textColor = .secondaryLabelColor
            automatic.maximumNumberOfLines = 2
            automatic.widthAnchor.constraint(equalToConstant: 650).isActive = true
            views.append(automatic)
            migrationKeyViews += [migrationIdentityFile, choose]
        } else {
            views.append(formRow("Password", migrationSSHPassword))
            migrationKeyViews.append(migrationSSHPassword)
        }
        views.append(migrationUseSudo)
        migrationKeyViews += [migrationUseSudo, check]
        let connectRow = NSView()
        check.translatesAutoresizingMaskIntoConstraints = false
        let connectControls = NSStackView(views: [check, migrationSpinner])
        connectControls.spacing = 10
        connectControls.alignment = .centerY
        connectControls.translatesAutoresizingMaskIntoConstraints = false
        connectRow.addSubview(connectControls)
        connectRow.widthAnchor.constraint(equalToConstant: 650).isActive = true
        connectRow.heightAnchor.constraint(equalToConstant: 38).isActive = true
        NSLayoutConstraint.activate([
            connectControls.centerXAnchor.constraint(equalTo: connectRow.centerXAnchor),
            connectControls.topAnchor.constraint(equalTo: connectRow.topAnchor),
            connectControls.bottomAnchor.constraint(equalTo: connectRow.bottomAnchor)
        ])
        views.append(connectRow)

        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        if let compatibility {
            let status = featureRow(compatibility.message,
                                    compatibility.compatible ? "checkmark.circle.fill" : "exclamationmark.triangle.fill",
                                    color: compatibility.compatible ? .systemGreen : .systemOrange)
            stack.addArrangedSubview(status)
        }
        if let migrationDiagnostic {
            stack.addArrangedSubview(migrationDiagnosticView(migrationDiagnostic))
        }
        return withError(stack)
    }

    private func migrationDiagnosticView(_ diagnostic: TeslaMateSSHDiagnostic) -> NSView {
        let title = NSTextField(labelWithString: diagnostic.title)
        title.font = .systemFont(ofSize: 15, weight: .semibold)
        title.textColor = .systemOrange

        let summary = NSTextField(wrappingLabelWithString: diagnostic.summary)
        summary.maximumNumberOfLines = 2
        summary.widthAnchor.constraint(equalToConstant: 650).isActive = true

        let stack = NSStackView(views: [title, summary])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 6
        for suggestion in diagnostic.suggestions {
            let label = NSTextField(wrappingLabelWithString: "• \(suggestion)")
            label.textColor = .secondaryLabelColor
            label.maximumNumberOfLines = 2
            label.widthAnchor.constraint(equalToConstant: 650).isActive = true
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
        return stack
    }

    private func verifyBody() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        if busy && checks.isEmpty {
            let row = NSStackView(views: [spinner, NSTextField(labelWithString: "Running checks…")])
            row.spacing = 10
            row.alignment = .centerY
            stack.addArrangedSubview(row)
        } else {
            for check in checks {
                stack.addArrangedSubview(featureRow("\(check.title) — \(check.detail)",
                                                    check.passed ? "checkmark.circle.fill" : "xmark.circle.fill",
                                                    color: check.passed ? .systemGreen : .systemRed))
            }
        }
        let logs = HubActionButton(title: "View Logs", target: self, action: #selector(openLogs))
        configureFlatButton(logs, symbol: "doc.text")
        stack.addArrangedSubview(logs)
        return withError(stack)
    }

    private func finishBody() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 18
        if state.path == .migration {
            stack.addArrangedSubview(featureRow("TeslaMate and its database remain installed and running.",
                                                "checkmark.circle.fill",
                                                color: .systemGreen))
            stack.addArrangedSubview(featureRow("Hub did not write to the TeslaMate database.",
                                                "checkmark.circle.fill",
                                                color: .systemGreen))
            let note = NSTextField(wrappingLabelWithString:
                "Before starting Hub, disable Tesla access in TeslaMate—legacy login or Fleet credentials, whichever it uses. This avoids duplicate polling, refresh-token use, and excess calls. Do not uninstall TeslaMate or delete its data.")
            note.textColor = .secondaryLabelColor
            note.maximumNumberOfLines = 4
            note.widthAnchor.constraint(equalToConstant: 650).isActive = true
            stack.addArrangedSubview(note)
            let acknowledgement = NSButton(checkboxWithTitle: "I disabled Tesla access in TeslaMate.",
                                           target: self,
                                           action: #selector(handoverChanged(_:)))
            acknowledgement.state = handoverAcknowledged ? .on : .off
            stack.addArrangedSubview(acknowledgement)
        } else {
            stack.addArrangedSubview(featureRow("Tesla account connected", "checkmark.circle.fill", color: .systemGreen))
            stack.addArrangedSubview(featureRow("Vehicle and database verified", "checkmark.circle.fill", color: .systemGreen))
            stack.addArrangedSubview(featureRow("Hub service is collecting", "checkmark.circle.fill", color: .systemGreen))
        }
        return withError(stack)
    }

    private func progressView() -> NSView {
        var views: [NSView] = []
        for index in 1...5 {
            let dot = NSImageView(image: symbolImage(index < state.step ? "checkmark.circle.fill" : "circle.fill",
                                                     description: "Step \(index)"))
            dot.contentTintColor = index < state.step ? .systemGreen : (index == state.step ? .systemBlue : .tertiaryLabelColor)
            dot.widthAnchor.constraint(equalToConstant: 13).isActive = true
            dot.heightAnchor.constraint(equalToConstant: 13).isActive = true
            views.append(dot)
            if index < 5 {
                let line = separator()
                line.widthAnchor.constraint(equalToConstant: 25).isActive = true
                line.heightAnchor.constraint(equalToConstant: 1).isActive = true
                views.append(line)
            }
        }
        let stack = NSStackView(views: views)
        stack.spacing = 0
        stack.alignment = .centerY
        return stack
    }

    private func footerView() -> NSView {
        backButton.target = self
        backButton.action = #selector(backPressed)
        configureFlatButton(backButton)
        backButton.controlSize = .large
        backWidthConstraint?.isActive = false
        backWidthConstraint = backButton.widthAnchor.constraint(equalToConstant: 96)
        backWidthConstraint?.isActive = true
        continueButton.target = self
        continueButton.action = #selector(continuePressed)
        configurePrimaryButton(continueButton)
        continueButton.controlSize = .large
        continueWidthConstraint?.isActive = false
        continueWidthConstraint = continueButton.widthAnchor.constraint(equalToConstant: 140)
        continueWidthConstraint?.isActive = true
        continueButton.heightAnchor.constraint(equalToConstant: 32).isActive = true

        footerSpinner.style = .spinning
        footerSpinner.controlSize = .small
        footerSpinner.isDisplayedWhenStopped = false
        footerSpinner.toolTip = "Hub setup is working"
        let footer = NSStackView(views: [spacer(), footerSpinner, backButton, continueButton])
        footer.spacing = 10
        footer.alignment = .centerY
        return footer
    }

    private func updateFooter() {
        let blocked = interactionBlocked
        backButton.isHidden = state.route == .welcome
            || state.route == .finish
            || (state.route == .verify && state.path == .newInstallation)
        backButton.isEnabled = !blocked
        continueButton.isHidden = state.route == .migration && compatibility?.compatible != true
        continueButton.title = busy ? (busyMessage ?? continueTitle) : continueTitle
        continueButton.image = nil
        switch state.route {
        case .migration:
            continueButton.isEnabled = compatibility?.compatible == true && !blocked
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
            footerSpinner.startAnimation(nil)
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
        migrationConnectButton?.isEnabled = !blocked
        for case let control as NSControl in migrationKeyViews {
            control.isEnabled = !blocked
        }
        if let migrationConnectButton {
            migrationConnectButton.title = busyMessage == "Connecting…"
                ? "Connecting…" : "Connect to Server"
            updatePrimaryAppearance(migrationConnectButton)
        }
        updatePrimaryAppearance(continueButton)
        backWidthConstraint?.constant = max(96, ceil(backButton.intrinsicContentSize.width) + 24)
        continueWidthConstraint?.constant = max(140, ceil(continueButton.intrinsicContentSize.width) + 32)
        window?.defaultButtonCell = blocked || continueButton.isHidden
            ? nil
            : continueButton.cell as? NSButtonCell
    }

    private var continueTitle: String {
        switch state.route {
        case .fleet: return "Set Up Fleet"
        case .legacy: return "Connect Tesla"
        case .migration: return compatibility?.compatible == true ? "Import Data" : "Continue"
        case .verify:
            return verificationFinished && !checks.isEmpty && checks.allSatisfy(\.passed)
                ? "Continue" : "Run Again"
        case .finish: return state.path == .migration ? "Start Hub" : "Open Hub"
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
            importMigration()
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
        setBusy(true, message: "Saving Fleet credentials…")
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
            setBusy(true, message: "Saving Tesla credentials…")
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
                    self.setBusy(true, message: "Saving Tesla credentials…")
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
                self.controller.checkTeslaMateCompatibility(source: session.source,
                                                             carID: session.carID,
                                                             passwordFile: session.passwordFile.path) { [weak self] check in
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
                                                                       requiredVersion: "4.1.1")
                    }
                    self.render()
                }
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

    private func importMigration() {
        guard let session = migrationSession,
              migrationInputsComplete, compatibility?.compatible == true else {
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
        setBusy(true, message: "Importing data…")
        controller.importTeslaMateOnline(source: session.source,
                                         carID: session.carID,
                                         passwordFile: session.passwordFile.path,
                                         encryptionKeyFile: session.encryptionKeyFile.path) { [weak self] result in
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
            onComplete()
            return
        }
        guard handoverAcknowledged else { return }
        setBusy(true, message: "Starting Hub…")
        controller.acknowledgeMigrationHandoverAndStart { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case .success: self.onComplete()
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
        migrationSession?.close()
        migrationSession = nil
        connectedMigrationIdentity = nil
        render()
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
        busy = value
        busyMessage = value ? message : nil
        updateWindowCloseAvailability()
        updateFooter()
    }

    private func updateWindowCloseAvailability() {
        window?.standardWindowButton(.closeButton)?.isEnabled = !interactionBlocked
    }

    private var interactionBlocked: Bool { busy || authWindow != nil }

    private func showInlineError(_ message: String) {
        setBusy(false)
        errorMessage = message
        render()
    }

    private func withError(_ view: NSView) -> NSView {
        guard let errorMessage else { return view }
        let error = NSTextField(wrappingLabelWithString: errorMessage)
        error.textColor = .systemRed
        error.maximumNumberOfLines = 3
        error.widthAnchor.constraint(equalToConstant: 650).isActive = true
        let stack = NSStackView(views: [view, error])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        return stack
    }

    private func horizontalChoices(_ choices: [NSView]) -> NSView {
        let stack = NSStackView(views: choices)
        stack.spacing = 18
        stack.distribution = .fillEqually
        stack.alignment = .centerY
        for choice in choices {
            choice.heightAnchor.constraint(equalToConstant: 235).isActive = true
        }
        return stack
    }

    private func choiceButton(title: String,
                              subtitle: String,
                              symbol: String,
                              accentColor: NSColor,
                              selected: Bool,
                              action: Selector) -> NSView {
        let card = NSView()
        card.wantsLayer = true
        card.layer?.cornerRadius = 12
        card.layer?.cornerCurve = .continuous
        card.layer?.backgroundColor = (selected
            ? NSColor.systemBlue.withAlphaComponent(0.10)
            : NSColor.controlBackgroundColor.withAlphaComponent(0.45)).cgColor

        let icon = NSImageView(image: symbolImage(symbol, description: title))
        icon.image = icon.image?.withSymbolConfiguration(
            NSImage.SymbolConfiguration(pointSize: 48, weight: .medium)
        )
        icon.contentTintColor = accentColor
        icon.imageScaling = .scaleProportionallyUpOrDown
        icon.widthAnchor.constraint(equalToConstant: 72).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 72).isActive = true

        let heading = NSTextField(labelWithString: title)
        heading.font = .systemFont(ofSize: 16, weight: .semibold)
        heading.alignment = .center
        let detail = NSTextField(wrappingLabelWithString: subtitle)
        detail.font = .systemFont(ofSize: 13)
        detail.textColor = .secondaryLabelColor
        detail.alignment = .center
        detail.maximumNumberOfLines = 2
        detail.widthAnchor.constraint(lessThanOrEqualToConstant: 270).isActive = true

        let content = NSStackView(views: [icon, heading, detail])
        content.orientation = .vertical
        content.alignment = .centerX
        content.spacing = 9
        content.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(content)

        let button = HubActionButton(title: title, target: self, action: action)
        button.isBordered = false
        button.isTransparent = true
        button.toolTip = subtitle
        button.setAccessibilityLabel(title)
        button.setAccessibilityValue(selected ? "Selected" : "Not selected")
        button.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(button)

        NSLayoutConstraint.activate([
            content.centerXAnchor.constraint(equalTo: card.centerXAnchor),
            content.centerYAnchor.constraint(equalTo: card.centerYAnchor, constant: 4),
            button.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            button.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            button.topAnchor.constraint(equalTo: card.topAnchor),
            button.bottomAnchor.constraint(equalTo: card.bottomAnchor)
        ])
        return card
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
        var keyViews = migrationKeyViews + [backButton]
        if !continueButton.isHidden { keyViews.append(continueButton) }
        guard !keyViews.isEmpty else { return }
        for index in keyViews.indices {
            keyViews[index].nextKeyView = keyViews[(index + 1) % keyViews.count]
        }
        let responder = previousField.flatMap { previous in
            keyViews.first { $0 === previous }
        } ?? migrationServer
        window.makeFirstResponder(responder)
    }

    private func formRow(_ title: String, _ field: NSView) -> NSView {
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.widthAnchor.constraint(equalToConstant: 155).isActive = true
        field.widthAnchor.constraint(greaterThanOrEqualToConstant: 460).isActive = true
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
        (button as? HubActionButton)?.hubAppearance = .flat
        button.font = .systemFont(ofSize: 13, weight: .medium)
        button.focusRingType = .default
    }

    private func configurePrimaryButton(_ button: NSButton, symbol: String? = nil) {
        button.isBordered = false
        (button as? HubActionButton)?.hubAppearance = .primary
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

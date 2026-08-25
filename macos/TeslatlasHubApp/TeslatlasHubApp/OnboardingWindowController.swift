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

final class OnboardingWindowController: NSWindowController {
    private let controller: HubController
    private let onComplete: () -> Void
    private var state: HubOnboardingState
    private var busy = false
    private var errorMessage: String?
    private var compatibility: HubTeslaMateCompatibility?
    private var checks: [HubOnboardingCheck] = []
    private var verificationFinished = false
    private var handoverAcknowledged = false
    private var authWindow: TeslaAuthWindowController?
    private var logsWindow: LogsWindowController?

    private let continueButton = NSButton(title: "Continue", target: nil, action: nil)
    private let backButton = NSButton(title: "Back", target: nil, action: nil)
    private let spinner = NSProgressIndicator()

    private let fleetClientID = NSTextField(string: "")
    private let fleetAccessToken = NSSecureTextField(string: "")
    private let fleetRefreshToken = NSSecureTextField(string: "")
    private let fleetExpiry = NSTextField(string: "3600")
    private let fleetRegion = NSPopUpButton()

    private let migrationSource = NSTextField(string: "postgresql://reader@127.0.0.1/teslamate")
    private let migrationCarID = NSTextField(string: "1")
    private let migrationPasswordFile = NSTextField(string: "")
    private let migrationKeyFile = NSTextField(string: "")

    init(controller: HubController,
         resumeMigrationHandoverPhase: HubMigrationHandoverPhase? = nil,
         previewRoute: String? = nil,
         onComplete: @escaping () -> Void) {
        self.controller = controller
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
            state = Self.previewState(route: previewRoute) ?? HubOnboardingState()
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

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

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
        for field in [fleetClientID, fleetAccessToken, fleetRefreshToken, fleetExpiry,
                      migrationSource, migrationCarID, migrationPasswordFile, migrationKeyFile] {
            field.controlSize = .large
        }
        fleetAccessToken.placeholderString = "Access token"
        fleetRefreshToken.placeholderString = "Refresh token"
        fleetClientID.placeholderString = "Tesla application client ID"
        migrationPasswordFile.placeholderString = "Choose a protected password file"
        migrationKeyFile.placeholderString = "Choose the TeslaMate ENCRYPTION_KEY file"
    }

    private func render() {
        guard let window else { return }
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
        icon.wantsLayer = true
        icon.layer?.cornerRadius = state.route == .welcome ? 18 : 11
        icon.layer?.masksToBounds = true
        icon.widthAnchor.constraint(equalToConstant: state.route == .welcome ? 88 : 56).isActive = true
        icon.heightAnchor.constraint(equalTo: icon.widthAnchor).isActive = true
        page.addArrangedSubview(icon)

        let stepLabel = NSTextField(labelWithString: "Step \(state.step) of 5")
        stepLabel.font = .systemFont(ofSize: 12, weight: .medium)
        stepLabel.textColor = .secondaryLabelColor
        page.addArrangedSubview(stepLabel)
        page.addArrangedSubview(progressView())
        page.setCustomSpacing(36, after: page.arrangedSubviews.last!)

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
        body.heightAnchor.constraint(greaterThanOrEqualToConstant: 270).isActive = true

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
        window.contentView = root
        updateFooter()
    }

    private var pageTitle: String {
        switch state.route {
        case .welcome: return "Your Tesla history, privately collected."
        case .choose: return "How would you like to begin?"
        case .provider: return "Choose how Hub connects to Tesla"
        case .fleet: return "Set up Fleet API"
        case .legacy: return "Connect with a legacy Tesla login"
        case .migration: return "Import from TeslaMate"
        case .verify: return "Checking your Hub"
        case .finish: return state.path == .migration ? "Migration complete" : "Teslatlas Hub is ready"
        }
    }

    private var pageSubtitle: String {
        switch state.route {
        case .welcome:
            return "Teslatlas Hub collects your vehicle data locally and makes it available to Teslatlas."
        case .choose:
            return "Choose how you would like to set up Teslatlas Hub."
        case .provider:
            return "Fleet API is recommended. Legacy login remains available for TeslaMate-compatible tokens."
        case .fleet:
            return "Follow the setup guide, then enter the returned Fleet credentials. Secrets are passed to Hub through stdin."
        case .legacy:
            return "Sign in to Tesla in a private window. Hub stores the returned token pair encrypted on this Mac."
        case .migration:
            return "Hub accepts the exact TeslaMate 4.1.1 schema and reads it without changing the source."
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
            featureRow("Collect drives, charging, locations, and vehicle state", "car.fill"),
            featureRow("Keep your history in storage you control", "externaldrive.fill"),
            featureRow("Connect the Teslatlas app without a cloud account", "lock.shield.fill")
        ])
        rows.orientation = .vertical
        rows.alignment = .leading
        rows.spacing = 18
        return centered(rows)
    }

    private func chooseBody() -> NSView {
        let fresh = choiceButton(title: "New installation",
                                 subtitle: "Connect Tesla and start a new history.",
                                 symbol: "car.fill",
                                 accentColor: .systemGreen,
                                 selected: state.path == .newInstallation,
                                 action: #selector(selectNewInstallation))
        let migration = choiceButton(title: "Migrate from TeslaMate",
                                     subtitle: "Import TeslaMate 4.1.1. Your source stays unchanged.",
                                     symbol: "cylinder.split.1x2",
                                     accentColor: .systemBlue,
                                     selected: state.path == .migration,
                                     action: #selector(selectMigration))
        let choices = horizontalChoices([fresh, migration])
        let note = featureRow("Exact TeslaMate 4.1.1 compatibility is checked before any data is copied.",
                              "info.circle",
                              color: .secondaryLabelColor)
        let stack = NSStackView(views: [choices, note])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 14
        choices.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        return stack
    }

    private func providerBody() -> NSView {
        let fleet = choiceButton(title: "Fleet API",
                                 subtitle: "Recommended · official Tesla API",
                                 symbol: "checkmark.shield.fill",
                                 accentColor: .systemBlue,
                                 selected: state.provider == .fleet,
                                 action: #selector(selectFleet))
        let legacy = choiceButton(title: "Legacy Tesla login",
                                  subtitle: "Compatible with TeslaMate-style tokens",
                                  symbol: "key.fill",
                                  accentColor: .systemPurple,
                                  selected: state.provider == .legacy,
                                  action: #selector(selectLegacy))
        return horizontalChoices([fleet, legacy])
    }

    private func fleetBody() -> NSView {
        let guide = NSButton(title: "Open Fleet Setup Guide", target: self, action: #selector(openFleetGuide))
        guide.bezelStyle = .rounded
        guide.image = NSImage(systemSymbolName: "book", accessibilityDescription: nil)
        guide.imagePosition = .imageLeading
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
        let stack = NSStackView(views: [
            featureRow("Private WebKit sign-in", "person.crop.circle.badge.checkmark"),
            featureRow("PKCE and exact callback validation", "checkmark.shield"),
            featureRow("Encrypted token storage", "lock.fill")
        ])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 20
        return withError(centered(stack))
    }

    private func migrationBody() -> NSView {
        let choosePassword = NSButton(title: "Choose…", target: self, action: #selector(chooseMigrationPassword))
        let chooseKey = NSButton(title: "Choose…", target: self, action: #selector(chooseMigrationKey))
        let check = NSButton(title: "Check TeslaMate 4.1.1", target: self, action: #selector(checkMigrationCompatibility))
        check.bezelStyle = .rounded
        check.image = NSImage(systemSymbolName: "checkmark.shield", accessibilityDescription: nil)
        check.imagePosition = .imageLeading
        let stack = NSStackView(views: [
            formRow("PostgreSQL source", migrationSource),
            formRow("Car ID", migrationCarID),
            formRow("Password file", fieldWithButton(migrationPasswordFile, choosePassword)),
            formRow("TeslaMate ENCRYPTION_KEY", fieldWithButton(migrationKeyFile, chooseKey)),
            check
        ])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 9
        if let compatibility {
            let status = featureRow(compatibility.message,
                                    compatibility.compatible ? "checkmark.circle.fill" : "exclamationmark.triangle.fill",
                                    color: compatibility.compatible ? .systemGreen : .systemOrange)
            stack.addArrangedSubview(status)
        }
        return withError(stack)
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
        let logs = NSButton(title: "View Logs", target: self, action: #selector(openLogs))
        logs.bezelStyle = .rounded
        logs.image = NSImage(systemSymbolName: "doc.text", accessibilityDescription: nil)
        logs.imagePosition = .imageLeading
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
        let lock = NSImageView(image: symbolImage("lock.fill", description: "Private"))
        lock.contentTintColor = .secondaryLabelColor
        lock.widthAnchor.constraint(equalToConstant: 14).isActive = true
        lock.heightAnchor.constraint(equalToConstant: 14).isActive = true
        let privacy = NSTextField(labelWithString: "Your vehicle data stays on this Mac.")
        privacy.font = .systemFont(ofSize: 12)
        privacy.textColor = .secondaryLabelColor
        let privacyRow = NSStackView(views: [lock, privacy])
        privacyRow.spacing = 7
        privacyRow.alignment = .centerY

        backButton.target = self
        backButton.action = #selector(backPressed)
        backButton.bezelStyle = .rounded
        backButton.controlSize = .large
        backButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 90).isActive = true
        continueButton.target = self
        continueButton.action = #selector(continuePressed)
        continueButton.bezelStyle = .rounded
        continueButton.controlSize = .large
        continueButton.bezelColor = .systemBlue
        continueButton.contentTintColor = .white
        continueButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 110).isActive = true

        let footer = NSStackView(views: [privacyRow, spacer(), backButton, continueButton])
        footer.spacing = 10
        footer.alignment = .centerY
        return footer
    }

    private func updateFooter() {
        backButton.isHidden = state.route == .welcome
            || state.route == .finish
            || (state.route == .verify && state.path == .newInstallation)
        continueButton.title = continueTitle
        switch state.route {
        case .migration:
            continueButton.isEnabled = compatibility?.compatible == true && !busy
        case .verify:
            continueButton.isEnabled = verificationFinished && !busy
        case .finish where state.path == .migration:
            continueButton.isEnabled = handoverAcknowledged && !busy
        default:
            continueButton.isEnabled = !busy
        }
        if busy {
            spinner.style = .spinning
            spinner.controlSize = .small
            spinner.startAnimation(nil)
        } else {
            spinner.stopAnimation(nil)
        }
        window?.defaultButtonCell = continueButton.cell as? NSButtonCell
    }

    private var continueTitle: String {
        switch state.route {
        case .fleet: return "Set Up Fleet"
        case .legacy: return "Connect Tesla"
        case .migration: return "Import"
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
        errorMessage = nil
        state.back()
        render()
    }

    @objc private func continuePressed() {
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
        setBusy(true)
        controller.configureFleetAccount(credentials: credentials) { [weak self] result in
            self?.setupFinished(result)
        }
    }

    private func configureLegacy() {
        guard authWindow == nil else {
            authWindow?.window?.makeKeyAndOrderFront(nil)
            return
        }
        do {
            let auth = try TeslaAuthWindowController { [weak self] result in
                guard let self else { return }
                self.authWindow = nil
                switch result {
                case let .success(tokens):
                    self.setBusy(true)
                    self.controller.configureTeslaAccount(tokens: tokens) { [weak self] setup in
                        self?.setupFinished(setup)
                    }
                case let .failure(error):
                    if error as? TeslaAuthError != .cancelled {
                        self.showInlineError(error.localizedDescription)
                    }
                }
            }
            authWindow = auth
            auth.showWindow(nil)
            auth.window?.makeKeyAndOrderFront(nil)
        } catch {
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
        guard migrationInputsComplete else {
            showInlineError("Source, car ID, password file, and ENCRYPTION_KEY file are required.")
            return
        }
        setBusy(true)
        compatibility = nil
        controller.checkTeslaMateCompatibility(source: migrationSource.stringValue,
                                               carID: migrationCarID.stringValue,
                                               passwordFile: migrationPasswordFile.stringValue) { [weak self] result in
            guard let self else { return }
            self.setBusy(false)
            switch result {
            case let .success(report):
                self.compatibility = report
                self.errorMessage = nil
                self.render()
            case let .failure(error):
                self.compatibility = HubTeslaMateCompatibility(compatible: false,
                                                               message: error.localizedDescription,
                                                               reasonCode: "unavailable",
                                                               requiredVersion: "4.1.1")
                self.render()
            }
        }
    }

    private func importMigration() {
        guard migrationInputsComplete, compatibility?.compatible == true else {
            showInlineError("Check the TeslaMate 4.1.1 database before importing.")
            return
        }
        setBusy(true)
        controller.importTeslaMateOnline(source: migrationSource.stringValue,
                                         carID: migrationCarID.stringValue,
                                         passwordFile: migrationPasswordFile.stringValue,
                                         encryptionKeyFile: migrationKeyFile.stringValue) { [weak self] result in
            self?.setupFinished(result)
        }
    }

    private var migrationInputsComplete: Bool {
        !migrationSource.stringValue.isEmpty
            && Int64(migrationCarID.stringValue).map { $0 > 0 } == true
            && !migrationPasswordFile.stringValue.isEmpty
            && !migrationKeyFile.stringValue.isEmpty
    }

    private func runVerification() {
        checks = []
        verificationFinished = false
        busy = true
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
        setBusy(true)
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
        if let url = URL(string: "https://github.com/magrathean-uk/teslatlas-hub/blob/main/docs/FLEET_SETUP.md") {
            NSWorkspace.shared.open(url)
        }
    }

    @objc private func openLogs() {
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

    private func chooseFile(for field: NSTextField) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.beginSheetModal(for: window!) { response in
            if response == .OK { field.stringValue = panel.url?.path ?? "" }
        }
    }

    private func setBusy(_ value: Bool) {
        busy = value
        updateFooter()
    }

    private func showInlineError(_ message: String) {
        busy = false
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
        card.layer?.borderWidth = selected ? 2 : 1
        card.layer?.borderColor = (selected ? NSColor.systemBlue : NSColor.separatorColor).cgColor
        card.layer?.backgroundColor = (selected
            ? NSColor.systemBlue.withAlphaComponent(0.045)
            : NSColor.controlBackgroundColor).cgColor

        let iconCircle = NSView()
        iconCircle.wantsLayer = true
        iconCircle.layer?.cornerRadius = 34
        iconCircle.layer?.backgroundColor = accentColor.withAlphaComponent(selected ? 0.12 : 0.08).cgColor
        iconCircle.widthAnchor.constraint(equalToConstant: 68).isActive = true
        iconCircle.heightAnchor.constraint(equalToConstant: 68).isActive = true

        let icon = NSImageView(image: symbolImage(symbol, description: title))
        icon.contentTintColor = accentColor
        icon.imageScaling = .scaleProportionallyDown
        icon.translatesAutoresizingMaskIntoConstraints = false
        iconCircle.addSubview(icon)
        NSLayoutConstraint.activate([
            icon.centerXAnchor.constraint(equalTo: iconCircle.centerXAnchor),
            icon.centerYAnchor.constraint(equalTo: iconCircle.centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 32),
            icon.heightAnchor.constraint(equalToConstant: 32)
        ])

        let heading = NSTextField(labelWithString: title)
        heading.font = .systemFont(ofSize: 16, weight: .semibold)
        heading.alignment = .center
        let detail = NSTextField(wrappingLabelWithString: subtitle)
        detail.font = .systemFont(ofSize: 13)
        detail.textColor = .secondaryLabelColor
        detail.alignment = .center
        detail.maximumNumberOfLines = 2
        detail.widthAnchor.constraint(lessThanOrEqualToConstant: 270).isActive = true

        let content = NSStackView(views: [iconCircle, heading, detail])
        content.orientation = .vertical
        content.alignment = .centerX
        content.spacing = 9
        content.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(content)

        let indicator = NSImageView(image: symbolImage(selected ? "checkmark.circle.fill" : "circle",
                                                       description: selected ? "Selected" : "Not selected"))
        indicator.contentTintColor = selected ? .systemBlue : .tertiaryLabelColor
        indicator.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(indicator)

        let button = NSButton(title: title, target: self, action: action)
        button.isBordered = false
        button.isTransparent = true
        button.toolTip = subtitle
        button.setAccessibilityLabel(title)
        button.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(button)

        NSLayoutConstraint.activate([
            content.centerXAnchor.constraint(equalTo: card.centerXAnchor),
            content.centerYAnchor.constraint(equalTo: card.centerYAnchor, constant: 4),
            indicator.topAnchor.constraint(equalTo: card.topAnchor, constant: 14),
            indicator.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            indicator.widthAnchor.constraint(equalToConstant: 17),
            indicator.heightAnchor.constraint(equalToConstant: 17),
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
        image.contentTintColor = color
        image.widthAnchor.constraint(equalToConstant: 22).isActive = true
        image.heightAnchor.constraint(equalToConstant: 22).isActive = true
        let label = NSTextField(wrappingLabelWithString: title)
        label.font = .systemFont(ofSize: 14)
        label.maximumNumberOfLines = 2
        let row = NSStackView(views: [image, label])
        row.spacing = 12
        row.alignment = .centerY
        return row
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

    private func fieldWithButton(_ field: NSTextField, _ button: NSButton) -> NSView {
        button.bezelStyle = .rounded
        let row = NSStackView(views: [field, button])
        row.spacing = 8
        return row
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

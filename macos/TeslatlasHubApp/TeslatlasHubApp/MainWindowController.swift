import AppKit

final class MainWindowController: NSWindowController {
    private let controller: HubController
    private let heroDot = NSImageView()
    private let heroTitle = NSTextField(labelWithString: "")
    private let heroSubtitle = NSTextField(labelWithString: "")
    private let heroStateIcon = NSImageView()
    private let serviceValue = NSTextField(labelWithString: "")
    private let accountValue = NSTextField(labelWithString: "")
    private let databaseValue = NSTextField(labelWithString: "")
    private let vehicleControlName = NSTextField(labelWithString: "Vehicle")
    private let vehicleControlStatus = NSTextField(labelWithString: "")
    private let serviceDot = NSImageView()
    private let accountDot = NSImageView()
    private let databaseDot = NSImageView()
    private let vehicleControlDot = NSImageView()
    private let activityStack = NSStackView()
    private let versionLabel = NSTextField(labelWithString: "")
    private let titlebarTitle = NSTextField(labelWithString: "Teslatlas Hub")
    private let stopButton = NSButton(title: "Stop Hub", target: nil, action: nil)
    private let restartButton = NSButton(title: "Restart", target: nil, action: nil)
    private let installButton = NSButton(title: "Set Up Hub", target: nil, action: nil)
    let connectButton = NSButton(title: "Connect Tesla", target: nil, action: nil)
    let importButton = NSButton(title: "Import", target: nil, action: nil)
    let detailsButton = NSButton(title: "Service Details", target: nil, action: nil)
    private var vehicleActionButtons: [NSButton] = []
    private var vehicleControlPending = false
    private var vehicleControlOutcomeUnknown = false
    private(set) var accountWorkflowActive = false
    private var serviceDetailsMutationPending = false
    private var titlebarAccessory: NSTitlebarAccessoryViewController?
    private var logsWindow: LogsWindowController?
    private(set) var detailsWindow: ServiceDetailsWindowController?
    private var diagnosticsWindow: DiagnosticsWindowController?
    private var onboardingWindow: OnboardingWindowController?
    private var onInitialRefresh: ((HubSnapshot) -> Void)?
    private var refreshTimer: Timer?
    private var refreshInFlight = false

    init(controller: HubController, onInitialRefresh: ((HubSnapshot) -> Void)? = nil) {
        self.controller = controller
        self.onInitialRefresh = onInitialRefresh
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 630),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "Teslatlas Hub"
        window.minSize = NSSize(width: 760, height: 610)
        super.init(window: window)
        configureTitlebar(window)
        window.contentView = makeContentView()
        window.center()
        update()
        let refreshTimer = Timer(timeInterval: 5, repeats: true) { [weak self] _ in
            guard let self,
                  self.window?.isVisible == true,
                  !self.accountWorkflowActive,
                  !self.serviceDetailsMutationPending else { return }
            self.update()
        }
        RunLoop.main.add(refreshTimer, forMode: .common)
        self.refreshTimer = refreshTimer
    }

    deinit { refreshTimer?.invalidate() }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func configureTitlebar(_ window: NSWindow) {
        window.titleVisibility = .hidden
        if let titlebar = window.standardWindowButton(.closeButton)?.superview {
            titlebarTitle.font = .systemFont(ofSize: 13, weight: .semibold)
            titlebarTitle.translatesAutoresizingMaskIntoConstraints = false
            titlebar.addSubview(titlebarTitle)
            NSLayoutConstraint.activate([
                titlebarTitle.centerXAnchor.constraint(equalTo: titlebar.centerXAnchor),
                titlebarTitle.centerYAnchor.constraint(equalTo: titlebar.centerYAnchor)
            ])
        }
        connectButton.target = self
        connectButton.action = #selector(connectTeslaPressed)
        configureFlatButton(connectButton, symbol: "person.badge.key")
        importButton.target = self
        importButton.action = #selector(importPressed)
        configureFlatButton(importButton, symbol: "square.and.arrow.down")
        let controls = NSStackView(views: [connectButton,
                                           importButton,
                                           compactButton("Logs", "doc.text", #selector(logsPressed))])
        controls.spacing = 8
        controls.alignment = .centerY
        let wrapper = NSView(frame: NSRect(x: 0, y: 0, width: 330, height: 38))
        controls.translatesAutoresizingMaskIntoConstraints = false
        wrapper.addSubview(controls)
        NSLayoutConstraint.activate([
            controls.centerYAnchor.constraint(equalTo: wrapper.centerYAnchor),
            controls.trailingAnchor.constraint(equalTo: wrapper.trailingAnchor, constant: -8)
        ])
        let accessory = NSTitlebarAccessoryViewController()
        accessory.view = wrapper
        accessory.layoutAttribute = .right
        window.addTitlebarAccessoryViewController(accessory)
        titlebarAccessory = accessory
    }

    private func makeContentView() -> NSView {
        let root = NSView()
        root.wantsLayer = true
        root.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        let content = NSStackView()
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 12
        content.translatesAutoresizingMaskIntoConstraints = false

        let footerLine = separator()
        footerLine.translatesAutoresizingMaskIntoConstraints = false
        let footer = footerView()
        footer.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(content)
        root.addSubview(footerLine)
        root.addSubview(footer)

        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 110),
            content.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -110),
            content.topAnchor.constraint(equalTo: root.topAnchor, constant: 26),
            content.bottomAnchor.constraint(lessThanOrEqualTo: footerLine.topAnchor, constant: -12),
            footerLine.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            footerLine.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            footerLine.bottomAnchor.constraint(equalTo: footer.topAnchor, constant: -10),
            footer.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 38),
            footer.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -38),
            footer.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -12),
            footer.heightAnchor.constraint(equalToConstant: 32)
        ])

        let hero = heroView()
        content.addArrangedSubview(hero)
        hero.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true

        let vehicleCard = vehicleCardView()
        content.addArrangedSubview(vehicleCard)
        vehicleCard.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
        vehicleCard.heightAnchor.constraint(equalToConstant: 174).isActive = true

        let statusBox = NSBox()
        statusBox.boxType = .custom
        statusBox.wantsLayer = true
        statusBox.layer?.cornerRadius = 0
        statusBox.layer?.borderWidth = 1
        statusBox.layer?.borderColor = NSColor.separatorColor.cgColor
        statusBox.fillColor = .clear
        statusBox.cornerRadius = 0
        statusBox.contentViewMargins = .zero
        statusBox.contentView = statusRows()
        content.addArrangedSubview(statusBox)
        statusBox.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
        statusBox.heightAnchor.constraint(equalToConstant: 108).isActive = true

        let activity = activityView()
        content.addArrangedSubview(activity)
        activity.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
        return root
    }

    private func heroView() -> NSView {
        let hero = NSStackView()
        hero.orientation = .vertical
        hero.alignment = .centerX
        hero.spacing = 8

        heroDot.imageScaling = .scaleProportionallyDown
        heroDot.widthAnchor.constraint(equalToConstant: 24).isActive = true
        heroDot.heightAnchor.constraint(equalToConstant: 24).isActive = true
        hero.addArrangedSubview(heroDot)
        heroTitle.font = .systemFont(ofSize: 24, weight: .semibold)
        hero.addArrangedSubview(heroTitle)

        heroStateIcon.imageScaling = .scaleProportionallyDown
        heroStateIcon.contentTintColor = .secondaryLabelColor
        heroStateIcon.widthAnchor.constraint(equalToConstant: 20).isActive = true
        heroStateIcon.heightAnchor.constraint(equalToConstant: 20).isActive = true
        heroSubtitle.font = .systemFont(ofSize: 13)
        heroSubtitle.textColor = .secondaryLabelColor
        let subtitle = NSStackView(views: [heroStateIcon, heroSubtitle])
        subtitle.spacing = 8
        subtitle.alignment = .centerY
        hero.addArrangedSubview(subtitle)

        let actions = NSStackView(views: [stopButton, restartButton, installButton])
        actions.spacing = 12
        actions.alignment = .centerY
        stopButton.target = self
        stopButton.action = #selector(stopPressed)
        configureFlatButton(stopButton, symbol: "stop.fill", tint: .systemRed)
        stopButton.controlSize = .large
        stopButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 110).isActive = true
        restartButton.target = self
        restartButton.action = #selector(restartPressed)
        configureFlatButton(restartButton, symbol: "arrow.clockwise", tint: .controlAccentColor)
        restartButton.controlSize = .large
        restartButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 86).isActive = true
        installButton.target = self
        installButton.action = #selector(connectTeslaPressed)
        configureFlatButton(installButton, symbol: "person.badge.key", tint: .controlAccentColor)
        installButton.controlSize = .large
        hero.addArrangedSubview(actions)
        return hero
    }

    private func vehicleCardView() -> NSView {
        let card = NSBox()
        card.boxType = .custom
        card.wantsLayer = true
        card.layer?.cornerRadius = 0
        card.layer?.borderWidth = 1
        card.layer?.borderColor = NSColor.separatorColor.cgColor
        card.fillColor = .clear
        card.cornerRadius = 0
        card.contentViewMargins = .zero

        vehicleControlName.font = .systemFont(ofSize: 16, weight: .semibold)
        vehicleControlStatus.textColor = .secondaryLabelColor
        vehicleControlStatus.font = .systemFont(ofSize: 12)
        let identity = NSStackView(views: [vehicleControlName, vehicleControlStatus])
        identity.orientation = .vertical
        identity.alignment = .leading
        identity.spacing = 1
        let vehicleIcon = imageView("car.fill", .secondaryLabelColor)
        vehicleIcon.widthAnchor.constraint(equalToConstant: 32).isActive = true
        vehicleIcon.heightAnchor.constraint(equalToConstant: 32).isActive = true
        vehicleControlDot.image = NSImage(systemSymbolName: "circle.fill", accessibilityDescription: "Vehicle status")
        vehicleControlDot.imageScaling = .scaleProportionallyDown
        vehicleControlDot.widthAnchor.constraint(equalToConstant: 10).isActive = true
        vehicleControlDot.heightAnchor.constraint(equalToConstant: 10).isActive = true
        let heading = NSStackView(views: [vehicleIcon, identity, spacer(), vehicleControlDot])
        heading.spacing = 10
        heading.alignment = .centerY

        let actions: [(HubVehicleControl, String, String)] = [
            (.climateStart, "Start Climate", "fan.fill"),
            (.climateStop, "Stop Climate", "fan.slash.fill"),
            (.wake, "Wake", "power"),
            (.lock, "Lock", "lock.fill"),
            (.unlock, "Unlock", "lock.open.fill"),
            (.flashLights, "Flash", "light.beacon.max.fill"),
            (.honkHorn, "Honk", "horn.fill")
        ]
        vehicleActionButtons = actions.map { action, title, symbol in
            vehicleActionButton(action, title: title, symbol: symbol)
        }
        let firstRow = NSStackView(views: Array(vehicleActionButtons.prefix(2)))
        let secondRow = NSStackView(views: Array(vehicleActionButtons.dropFirst(2)))
        for row in [firstRow, secondRow] {
            row.spacing = 10
            row.alignment = .centerY
            row.distribution = .fillEqually
        }
        firstRow.heightAnchor.constraint(equalToConstant: 40).isActive = true
        secondRow.heightAnchor.constraint(equalToConstant: 50).isActive = true
        for button in vehicleActionButtons.prefix(2) {
            button.heightAnchor.constraint(equalTo: firstRow.heightAnchor).isActive = true
        }
        for button in vehicleActionButtons.dropFirst(2) {
            button.heightAnchor.constraint(equalTo: secondRow.heightAnchor).isActive = true
        }
        let actionSeparator = separator()
        let stack = NSStackView(views: [heading, actionSeparator, firstRow, secondRow])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 6
        stack.edgeInsets = NSEdgeInsets(top: 12, left: 16, bottom: 12, right: 16)
        for row in [heading, actionSeparator, firstRow, secondRow] {
            row.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32).isActive = true
        }
        let container = NSView()
        stack.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            stack.topAnchor.constraint(equalTo: container.topAnchor),
            stack.bottomAnchor.constraint(equalTo: container.bottomAnchor)
        ])
        card.contentView = container
        return card
    }

    private func vehicleActionButton(_ action: HubVehicleControl,
                                     title: String,
                                     symbol: String) -> NSButton {
        let button = compactButton(title, symbol, #selector(vehicleCardButtonPressed(_:)))
        button.identifier = NSUserInterfaceItemIdentifier(action.rawValue)
        button.controlSize = .large
        button.isBordered = false
        button.font = .systemFont(ofSize: 13, weight: .medium)
        button.imagePosition = action == .climateStart || action == .climateStop ? .imageLeading : .imageAbove
        button.imageHugsTitle = true
        return button
    }

    private func statusRows() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.spacing = 0
        let views: [NSView] = [
            statusRow("Service", serviceValue, serviceDot, "gearshape.fill"), separator(),
            statusRow("Tesla account", accountValue, accountDot, "person.fill"), separator(),
            statusRow("Database", databaseValue, databaseDot, "cylinder.fill")
        ]
        for view in views {
            stack.addArrangedSubview(view)
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func statusRow(_ name: String, _ value: NSTextField, _ dot: NSImageView, _ symbol: String) -> NSView {
        statusRow(NSTextField(labelWithString: name), value, dot, symbol)
    }

    private func statusRow(_ name: NSTextField, _ value: NSTextField, _ dot: NSImageView, _ symbol: String) -> NSView {
        let icon = imageView(symbol, .secondaryLabelColor)
        icon.widthAnchor.constraint(equalToConstant: 22).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 22).isActive = true
        name.font = .systemFont(ofSize: 13, weight: .medium)
        name.widthAnchor.constraint(equalToConstant: 230).isActive = true
        value.font = .systemFont(ofSize: 13)
        dot.image = NSImage(systemSymbolName: "circle.fill", accessibilityDescription: "Status")
        dot.imageScaling = .scaleProportionallyDown
        dot.widthAnchor.constraint(equalToConstant: 11).isActive = true
        dot.heightAnchor.constraint(equalToConstant: 11).isActive = true
        let row = NSStackView(views: [icon, name, value, spacer(), dot])
        row.spacing = 10
        row.alignment = .centerY
        row.edgeInsets = NSEdgeInsets(top: 0, left: 18, bottom: 0, right: 16)
        row.heightAnchor.constraint(equalToConstant: 34).isActive = true
        return row
    }

    private func activityView() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 0
        let heading = sectionLabel("Latest activity")
        stack.addArrangedSubview(heading)
        stack.setCustomSpacing(7, after: heading)
        let line = separator()
        stack.addArrangedSubview(line)
        line.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        activityStack.orientation = .vertical
        activityStack.alignment = .leading
        activityStack.spacing = 0
        stack.addArrangedSubview(activityStack)
        activityStack.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        return stack
    }

    private func footerView() -> NSView {
        versionLabel.font = .systemFont(ofSize: 11)
        versionLabel.textColor = .secondaryLabelColor
        detailsButton.target = self
        detailsButton.action = #selector(detailsPressed)
        configureFlatButton(detailsButton, symbol: "info.circle")
        let footer = NSStackView(views: [detailsButton,
                                         compactButton("Run Diagnostics", "waveform.path.ecg", #selector(diagnosticsPressed)),
                                         compactButton("Show Data Folder", "folder", #selector(folderPressed)),
                                         spacer(), versionLabel])
        footer.spacing = 10
        footer.alignment = .centerY
        return footer
    }

    private func update() {
        guard !refreshInFlight else { return }
        refreshInFlight = true
        controller.refresh { [weak self] snapshot in
            guard let self else { return }
            defer { self.refreshInFlight = false }
            self.heroDot.image = NSImage(systemSymbolName: "circle.fill", accessibilityDescription: "Hub status")
            self.heroDot.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 24, weight: .regular)
            self.heroDot.contentTintColor = snapshot.health.color
            self.heroTitle.stringValue = snapshot.health.title
            switch snapshot.health {
            case .running:
                self.heroStateIcon.image = NSImage(systemSymbolName: "checkmark.shield", accessibilityDescription: "Running")
                self.heroStateIcon.contentTintColor = .secondaryLabelColor
                self.heroSubtitle.stringValue = "Teslatlas Hub is running in the background."
            case .stopped:
                self.heroStateIcon.image = NSImage(systemSymbolName: "pause.circle", accessibilityDescription: "Stopped")
                self.heroStateIcon.contentTintColor = .secondaryLabelColor
                self.heroSubtitle.stringValue = "Teslatlas Hub is stopped."
            case .needsInstall:
                self.heroStateIcon.image = NSImage(systemSymbolName: "person.badge.key", accessibilityDescription: "Setup required")
                self.heroStateIcon.contentTintColor = .secondaryLabelColor
                self.heroSubtitle.stringValue = "Choose how Hub connects to Tesla."
            case .degraded:
                self.heroStateIcon.image = NSImage(systemSymbolName: "exclamationmark.triangle", accessibilityDescription: "Attention needed")
                self.heroStateIcon.contentTintColor = .systemOrange
                self.heroSubtitle.stringValue = "Open diagnostics for details."
            }
            self.serviceValue.stringValue = snapshot.service
            self.accountValue.stringValue = snapshot.accountDisplay
            self.vehicleControlName.stringValue = snapshot.vehicleName
            self.vehicleControlStatus.stringValue = snapshot.vehicle
            self.databaseValue.stringValue = snapshot.database
            self.versionLabel.stringValue = snapshot.version
            self.serviceDot.contentTintColor = snapshot.health.color
            self.accountDot.contentTintColor = snapshot.account == "Connected" ? .systemGreen : .systemGray
            let vehicleUnavailable = snapshot.vehicle.localizedCaseInsensitiveContains("offline")
                || snapshot.vehicle.localizedCaseInsensitiveContains("no imported")
                || snapshot.vehicle.localizedCaseInsensitiveContains("no configured")
                || snapshot.vehicle == "Unknown"
            self.vehicleControlDot.contentTintColor = vehicleUnavailable ? .systemGray : .systemGreen
            self.databaseDot.contentTintColor = snapshot.database.hasPrefix("Healthy") ? .systemGreen : .systemGray

            self.stopButton.isHidden = snapshot.health == .needsInstall
            self.installButton.isHidden = snapshot.health != .needsInstall
            self.restartButton.isHidden = snapshot.health == .needsInstall || snapshot.health == .stopped
            let mutableActionsAvailable = !self.accountWorkflowActive
                && !self.serviceDetailsMutationPending
            let accountActionsAvailable = mutableActionsAvailable
                && !self.vehicleControlPending
            self.stopButton.isEnabled = mutableActionsAvailable
            self.installButton.isEnabled = mutableActionsAvailable
            self.restartButton.isEnabled = mutableActionsAvailable
            self.connectButton.isEnabled = accountActionsAvailable
            self.importButton.isEnabled = accountActionsAvailable
            self.detailsButton.isEnabled = !self.accountWorkflowActive
            self.connectButton.isHidden = false
            if snapshot.account == "Connected" {
                self.connectButton.title = "Manage Tesla"
                self.connectButton.image = NSImage(systemSymbolName: "person.crop.circle.badge.checkmark",
                                                   accessibilityDescription: "Manage Tesla")
                self.connectButton.action = #selector(self.manageTeslaPressed(_:))
            } else {
                self.connectButton.title = "Connect Tesla"
                self.connectButton.image = NSImage(systemSymbolName: "person.badge.key",
                                                   accessibilityDescription: "Connect Tesla")
                self.connectButton.action = #selector(self.connectTeslaPressed)
            }
            let controlsAvailable = !self.controller.previewMode
                && snapshot.health == .running
                && snapshot.account == "Connected"
                && snapshot.controlVehicleID != nil
                && !self.vehicleControlPending
                && !self.vehicleControlOutcomeUnknown
                && !self.accountWorkflowActive
                && !self.serviceDetailsMutationPending
            self.vehicleActionButtons.forEach {
                $0.isEnabled = controlsAvailable || self.controller.previewMode
            }
            self.stopButton.keyEquivalent = snapshot.health == .stopped ? "\r" : ""
            self.installButton.keyEquivalent = snapshot.health == .needsInstall ? "\r" : ""
            switch snapshot.health {
            case .needsInstall:
                self.window?.defaultButtonCell = self.installButton.cell as? NSButtonCell
            case .stopped:
                self.window?.defaultButtonCell = self.stopButton.cell as? NSButtonCell
            case .running, .degraded:
                self.window?.defaultButtonCell = nil
            }
            if snapshot.health == .stopped {
                self.stopButton.title = "Start Hub"
                self.stopButton.action = #selector(self.startPressed)
                self.stopButton.image = NSImage(systemSymbolName: "play.fill", accessibilityDescription: "Start Hub")
                self.stopButton.contentTintColor = .controlAccentColor
            } else {
                self.stopButton.title = "Stop Hub"
                self.stopButton.action = #selector(self.stopPressed)
                self.stopButton.image = NSImage(systemSymbolName: "stop.fill", accessibilityDescription: "Stop Hub")
                self.stopButton.contentTintColor = .systemRed
            }

            self.activityStack.arrangedSubviews.forEach {
                self.activityStack.removeArrangedSubview($0)
                $0.removeFromSuperview()
            }
            if snapshot.activity.isEmpty {
                let empty = NSTextField(labelWithString: "No activity yet.")
                empty.textColor = .secondaryLabelColor
                empty.heightAnchor.constraint(equalToConstant: 30).isActive = true
                self.activityStack.addArrangedSubview(empty)
            } else {
                for (index, entry) in snapshot.activity.prefix(3).enumerated() {
                    if index > 0 {
                        let line = self.separator()
                        self.activityStack.addArrangedSubview(line)
                        line.widthAnchor.constraint(equalTo: self.activityStack.widthAnchor).isActive = true
                    }
                    let dot = self.imageView("circle.fill", entry.color)
                    dot.widthAnchor.constraint(equalToConstant: 9).isActive = true
                    dot.heightAnchor.constraint(equalToConstant: 9).isActive = true
                    let message = NSTextField(labelWithString: entry.message)
                    let age = NSTextField(labelWithString: entry.age)
                    age.textColor = .secondaryLabelColor
                    let row = NSStackView(views: [dot, message, self.spacer(), age])
                    row.spacing = 9
                    row.alignment = .centerY
                    row.heightAnchor.constraint(equalToConstant: 30).isActive = true
                    self.activityStack.addArrangedSubview(row)
                    row.widthAnchor.constraint(equalTo: self.activityStack.widthAnchor).isActive = true
                }
            }
            let callback = self.onInitialRefresh
            self.onInitialRefresh = nil
            if let callback {
                DispatchQueue.main.async { callback(snapshot) }
            }
        }
    }

    private func compactButton(_ title: String, _ symbol: String, _ action: Selector) -> NSButton {
        let button = NSButton(title: title, target: self, action: action)
        configureFlatButton(button, symbol: symbol)
        button.controlSize = .regular
        return button
    }

    private func configureFlatButton(_ button: NSButton,
                                     symbol: String,
                                     tint: NSColor = .labelColor) {
        button.isBordered = false
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: button.title)
        button.imagePosition = .imageLeading
        button.contentTintColor = tint
        button.font = .systemFont(ofSize: 13, weight: .medium)
        button.focusRingType = .default
    }

    private func sectionLabel(_ title: String) -> NSTextField {
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 14, weight: .semibold)
        return label
    }

    private func imageView(_ symbol: String, _ color: NSColor) -> NSImageView {
        let view = NSImageView(image: NSImage(systemSymbolName: symbol, accessibilityDescription: symbol) ?? NSImage())
        view.contentTintColor = color
        view.imageScaling = .scaleProportionallyDown
        view.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 17, weight: .regular)
        return view
    }

    private func separator() -> NSBox {
        let line = NSBox()
        line.boxType = .separator
        line.heightAnchor.constraint(equalToConstant: 1).isActive = true
        return line
    }

    private func spacer() -> NSView {
        let view = NSView()
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        return view
    }

    private func showError(_ error: Error) { NSAlert(error: error).runModal() }

    static func vehicleControlConfirmation(_ action: HubVehicleControl,
                                           vehicleName: String) -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "\(action.title) for \(vehicleName)?"
        alert.informativeText = "Teslatlas Hub will send this command once."
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: action.title)
        return alert
    }

    static func vehicleControlOutcomeIsUnknown(_ error: Error) -> Bool {
        if let actionError = error as? HubActionError,
           case .commandTimedOut = actionError {
            return true
        }
        let message = error.localizedDescription.lowercased()
        return message.contains("timed out") || message.contains("outcome is ambiguous")
    }

    static func unknownVehicleControlOutcomeAlert() -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Command outcome unknown"
        alert.informativeText = "Check the vehicle. Do not repeat the command from this app session."
        return alert
    }

    private func confirmVehicleControl(_ action: HubVehicleControl) {
        guard let window, !vehicleControlPending else { return }
        let alert = Self.vehicleControlConfirmation(action, vehicleName: controller.snapshot.vehicleName)
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertSecondButtonReturn else { return }
            self?.runVehicleControl(action)
        }
    }

    private func runVehicleControl(_ action: HubVehicleControl) {
        vehicleControlPending = true
        connectButton.isEnabled = false
        importButton.isEnabled = false
        vehicleActionButtons.forEach { $0.isEnabled = false }
        controller.performVehicleControl(action) { [weak self] result in
            guard let self else { return }
            self.vehicleControlPending = false
            switch result {
            case .success:
                self.update()
                let accepted = NSAlert()
                accepted.messageText = "Command accepted"
                accepted.informativeText = action.acceptedMessage
                accepted.runModal()
            case let .failure(error):
                if Self.vehicleControlOutcomeIsUnknown(error) {
                    self.vehicleControlOutcomeUnknown = true
                    self.update()
                    Self.unknownVehicleControlOutcomeAlert().runModal()
                    return
                }
                self.update()
                self.showError(error)
            }
        }
    }

    @objc private func importPressed() {
        showOnboarding(route: .migration)
    }

    @objc private func logsPressed() {
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

    @objc private func connectTeslaPressed() {
        showOnboarding(route: .provider)
    }

    @objc private func manageTeslaPressed(_ sender: NSButton) {
        let menu = NSMenu(title: "Tesla account")
        menu.addItem(menuItem("Use Fleet API", action: #selector(useFleetPressed)))
        menu.addItem(menuItem("Use Legacy token", action: #selector(useLegacyPressed)))
        menu.addItem(menuItem("Migrate from TeslaMate…", action: #selector(migrateTeslaMatePressed)))
        menu.addItem(.separator())
        menu.addItem(menuItem("Disconnect Tesla…", action: #selector(disconnectTeslaPressed)))
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: sender.bounds.height + 4), in: sender)
    }

    private func menuItem(_ title: String, action: Selector) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        return item
    }

    @objc private func useFleetPressed() { showOnboarding(route: .fleet) }

    @objc private func useLegacyPressed() { showOnboarding(route: .legacy) }

    @objc private func migrateTeslaMatePressed() { showOnboarding(route: .migration) }

    @objc private func disconnectTeslaPressed() {
        let alert = Self.disconnectConfirmation()
        guard alert.runModal() == .alertSecondButtonReturn else { return }
        controller.signOutTeslaAccount { [weak self] result in
            switch result {
            case .success: self?.update()
            case let .failure(error): self?.showError(error)
            }
        }
    }

    @discardableResult
    func showOnboarding(route: HubOnboardingRoute) -> OnboardingWindowController? {
        if let onboardingWindow, onboardingWindow.window?.isVisible == true {
            onboardingWindow.navigate(to: route)
            onboardingWindow.window?.makeKeyAndOrderFront(nil)
            return onboardingWindow
        }
        guard !vehicleControlPending, !serviceDetailsMutationPending else {
            NSSound.beep()
            return nil
        }
        onboardingWindow = nil
        let onboarding = OnboardingWindowController(
            controller: controller,
            resumeMigrationHandoverPhase: controller.pendingMigrationHandoverPhase,
            initialRoute: route,
            onDismiss: { [weak self] in
                guard let self else { return }
                self.onboardingWindow = nil
                self.setAccountWorkflowActive(false)
            }
        ) { [weak self] in
            self?.onboardingWindow?.close()
        }
        onboardingWindow = onboarding
        setAccountWorkflowActive(true)
        onboarding.showWindow(nil)
        onboarding.window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        return onboarding
    }

    private func setAccountWorkflowActive(_ active: Bool) {
        accountWorkflowActive = active
        let mutableActionsAvailable = !active && !serviceDetailsMutationPending
        stopButton.isEnabled = mutableActionsAvailable
        restartButton.isEnabled = mutableActionsAvailable
        installButton.isEnabled = mutableActionsAvailable
        connectButton.isEnabled = mutableActionsAvailable
        importButton.isEnabled = mutableActionsAvailable
        detailsButton.isEnabled = !active
        detailsWindow?.setMutationsEnabled(!active)
        if active {
            vehicleActionButtons.forEach { $0.isEnabled = false }
        } else {
            update()
        }
    }

    private func setServiceDetailsMutationPending(_ pending: Bool) {
        serviceDetailsMutationPending = pending
        let mutableActionsAvailable = !pending && !accountWorkflowActive
        stopButton.isEnabled = mutableActionsAvailable
        restartButton.isEnabled = mutableActionsAvailable
        installButton.isEnabled = mutableActionsAvailable
        connectButton.isEnabled = mutableActionsAvailable && !vehicleControlPending
        importButton.isEnabled = mutableActionsAvailable && !vehicleControlPending
        if pending {
            vehicleActionButtons.forEach { $0.isEnabled = false }
        } else {
            update()
        }
    }

    static func disconnectConfirmation() -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Disconnect Tesla from Hub?"
        alert.informativeText = "Hub will stop and remove its stored Fleet and Legacy credentials. Your collected data stays on this Mac."
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: "Disconnect")
        alert.buttons[0].keyEquivalent = "\r"
        alert.buttons[1].keyEquivalent = ""
        return alert
    }

    @objc private func startPressed() {
        guard !accountWorkflowActive, !serviceDetailsMutationPending else { return }
        controller.startHub { [weak self] result in
            switch result { case .success: self?.update(); case let .failure(error): self?.showError(error) }
        }
    }

    @objc private func stopPressed() {
        guard !accountWorkflowActive, !serviceDetailsMutationPending else { return }
        controller.stopHub { [weak self] result in
            switch result { case .success: self?.update(); case let .failure(error): self?.showError(error) }
        }
    }

    @objc private func restartPressed() {
        guard !accountWorkflowActive, !serviceDetailsMutationPending else { return }
        controller.restartHub { [weak self] result in
            switch result { case .success: self?.update(); case let .failure(error): self?.showError(error) }
        }
    }

    @objc private func vehicleCardButtonPressed(_ sender: NSButton) {
        guard !accountWorkflowActive, !serviceDetailsMutationPending else { return }
        guard let rawValue = sender.identifier?.rawValue,
              let action = HubVehicleControl(rawValue: rawValue) else { return }
        confirmVehicleControl(action)
    }

    @objc private func detailsPressed() {
        guard !accountWorkflowActive else {
            NSSound.beep()
            return
        }
        if let detailsWindow {
            detailsWindow.update(snapshot: controller.snapshot)
            detailsWindow.setMutationsEnabled(true)
            detailsWindow.showWindow(nil)
            detailsWindow.window?.makeKeyAndOrderFront(nil)
            return
        }
        detailsWindow = ServiceDetailsWindowController(
            snapshot: controller.snapshot,
            controller: controller,
            mutationAllowed: { [weak self] in
                guard let self else { return false }
                return !self.accountWorkflowActive && !self.serviceDetailsMutationPending
            },
            onMutationStateChanged: { [weak self] in
                self?.setServiceDetailsMutationPending($0)
            }
        ) { [weak self] in self?.update() }
        detailsWindow?.showWindow(nil)
        detailsWindow?.window?.makeKeyAndOrderFront(nil)
    }

    @objc private func diagnosticsPressed() {
        if let diagnosticsWindow {
            diagnosticsWindow.showWindow(nil)
            diagnosticsWindow.window?.makeKeyAndOrderFront(nil)
            return
        }
        diagnosticsWindow = DiagnosticsWindowController(controller: controller)
        diagnosticsWindow?.showWindow(nil)
        diagnosticsWindow?.window?.makeKeyAndOrderFront(nil)
    }

    @objc private func folderPressed() { controller.showDataFolder() }
}

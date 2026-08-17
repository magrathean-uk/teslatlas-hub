import AppKit

final class MainWindowController: NSWindowController {
    private let controller: HubController
    private let heroDot = NSImageView()
    private let heroTitle = NSTextField(labelWithString: "")
    private let heroSubtitle = NSTextField(labelWithString: "")
    private let serviceValue = NSTextField(labelWithString: "")
    private let accountValue = NSTextField(labelWithString: "")
    private let vehicleName = NSTextField(labelWithString: "")
    private let vehicleValue = NSTextField(labelWithString: "")
    private let databaseValue = NSTextField(labelWithString: "")
    private let serviceDot = NSImageView()
    private let accountDot = NSImageView()
    private let vehicleDot = NSImageView()
    private let databaseDot = NSImageView()
    private let activityStack = NSStackView()
    private let versionLabel = NSTextField(labelWithString: "")
    private let titlebarTitle = NSTextField(labelWithString: "Teslatlas Hub")
    private let stopButton = NSButton(title: "Stop Hub", target: nil, action: nil)
    private let restartButton = NSButton(title: "Restart", target: nil, action: nil)
    private let installButton = NSButton(title: "Install Service", target: nil, action: nil)
    private var titlebarAccessory: NSTitlebarAccessoryViewController?
    private var importSheet: ImportSheetController?
    private var logsWindow: LogsWindowController?
    private var detailsWindow: ServiceDetailsWindowController?
    private var diagnosticsWindow: DiagnosticsWindowController?

    init(controller: HubController) {
        self.controller = controller
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 568),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "Teslatlas Hub"
        window.minSize = NSSize(width: 760, height: 548)
        super.init(window: window)
        configureTitlebar(window)
        window.contentView = makeContentView()
        window.center()
        update()
    }

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
        let controls = NSStackView(views: [compactButton("Import", "square.and.arrow.down", #selector(importPressed)),
                                           compactButton("Logs", "doc.text", #selector(logsPressed))])
        controls.spacing = 8
        controls.alignment = .centerY
        let wrapper = NSView(frame: NSRect(x: 0, y: 0, width: 176, height: 38))
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
        let content = NSStackView()
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 18
        content.translatesAutoresizingMaskIntoConstraints = false

        let footerLine = separator()
        footerLine.translatesAutoresizingMaskIntoConstraints = false
        let footer = footerView()
        footer.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(content)
        root.addSubview(footerLine)
        root.addSubview(footer)

        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 82),
            content.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -82),
            content.topAnchor.constraint(equalTo: root.topAnchor, constant: 48),
            content.bottomAnchor.constraint(lessThanOrEqualTo: footerLine.topAnchor, constant: -18),
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

        let statusBox = NSBox()
        statusBox.boxType = .custom
        statusBox.wantsLayer = true
        statusBox.layer?.cornerRadius = 8
        statusBox.layer?.borderWidth = 1
        statusBox.layer?.borderColor = NSColor.separatorColor.cgColor
        statusBox.fillColor = .controlBackgroundColor
        statusBox.cornerRadius = 8
        statusBox.contentView = statusRows()
        content.addArrangedSubview(statusBox)
        statusBox.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
        statusBox.heightAnchor.constraint(equalToConstant: 176).isActive = true

        let activity = activityView()
        content.addArrangedSubview(activity)
        activity.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
        return root
    }

    private func heroView() -> NSView {
        let hero = NSStackView()
        hero.orientation = .vertical
        hero.alignment = .centerX
        hero.spacing = 12

        heroDot.imageScaling = .scaleProportionallyDown
        heroDot.widthAnchor.constraint(equalToConstant: 24).isActive = true
        heroDot.heightAnchor.constraint(equalToConstant: 24).isActive = true
        hero.addArrangedSubview(heroDot)
        heroTitle.font = .systemFont(ofSize: 24, weight: .semibold)
        hero.addArrangedSubview(heroTitle)

        let shield = imageView("checkmark.shield", .secondaryLabelColor)
        shield.widthAnchor.constraint(equalToConstant: 20).isActive = true
        shield.heightAnchor.constraint(equalToConstant: 20).isActive = true
        heroSubtitle.font = .systemFont(ofSize: 13)
        heroSubtitle.textColor = .secondaryLabelColor
        let subtitle = NSStackView(views: [shield, heroSubtitle])
        subtitle.spacing = 8
        subtitle.alignment = .centerY
        hero.addArrangedSubview(subtitle)

        let actions = NSStackView(views: [stopButton, restartButton, installButton])
        actions.spacing = 12
        actions.alignment = .centerY
        stopButton.target = self
        stopButton.action = #selector(stopPressed)
        stopButton.bezelStyle = .rounded
        stopButton.bezelColor = .systemBlue
        stopButton.contentTintColor = .white
        stopButton.controlSize = .large
        stopButton.keyEquivalent = "\r"
        stopButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 110).isActive = true
        restartButton.target = self
        restartButton.action = #selector(restartPressed)
        restartButton.bezelStyle = .rounded
        restartButton.controlSize = .large
        restartButton.widthAnchor.constraint(greaterThanOrEqualToConstant: 86).isActive = true
        installButton.target = self
        installButton.action = #selector(installPressed)
        installButton.bezelStyle = .rounded
        installButton.controlSize = .large
        installButton.keyEquivalent = "\r"
        hero.addArrangedSubview(actions)
        return hero
    }

    private func statusRows() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.spacing = 0
        let views: [NSView] = [
            statusRow("Service", serviceValue, serviceDot, "gearshape.fill"), separator(),
            statusRow("Tesla account", accountValue, accountDot, "person.fill"), separator(),
            statusRow(vehicleName, vehicleValue, vehicleDot, "car.fill"), separator(),
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
        row.heightAnchor.constraint(equalToConstant: 40).isActive = true
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
        let footer = NSStackView(views: [compactButton("Service Details", "info.circle", #selector(detailsPressed)),
                                         compactButton("Run Diagnostics", "waveform.path.ecg", #selector(diagnosticsPressed)),
                                         compactButton("Show Data Folder", "folder", #selector(folderPressed)),
                                         spacer(), versionLabel])
        footer.spacing = 10
        footer.alignment = .centerY
        return footer
    }

    private func update() {
        controller.refresh { [weak self] snapshot in
            guard let self else { return }
            self.heroDot.image = NSImage(systemSymbolName: "circle.fill", accessibilityDescription: "Hub status")
            self.heroDot.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 24, weight: .regular)
            self.heroDot.contentTintColor = snapshot.health.color
            self.heroTitle.stringValue = snapshot.health.title
            self.heroSubtitle.stringValue = snapshot.health == .running
                ? "Teslatlas Hub is running in the background."
                : "Install the service, then import your TeslaMate data."
            self.serviceValue.stringValue = snapshot.service
            self.accountValue.stringValue = snapshot.account
            self.vehicleName.stringValue = snapshot.vehicleName
            self.vehicleValue.stringValue = snapshot.vehicle
            self.databaseValue.stringValue = snapshot.database
            self.versionLabel.stringValue = snapshot.version
            self.serviceDot.contentTintColor = snapshot.health.color
            self.accountDot.contentTintColor = snapshot.account == "Connected" ? .systemGreen : .systemGray
            let vehicleUnavailable = snapshot.vehicle.localizedCaseInsensitiveContains("offline")
                || snapshot.vehicle.localizedCaseInsensitiveContains("no imported")
                || snapshot.vehicle == "Unknown"
            self.vehicleDot.contentTintColor = vehicleUnavailable ? .systemGray : .systemGreen
            self.databaseDot.contentTintColor = snapshot.database.hasPrefix("Healthy") ? .systemGreen : .systemGray

            self.stopButton.isHidden = snapshot.health == .needsInstall
            self.installButton.isHidden = snapshot.health != .needsInstall
            self.restartButton.isHidden = snapshot.health == .needsInstall || snapshot.health == .stopped
            self.window?.defaultButtonCell = (snapshot.health == .needsInstall
                ? self.installButton.cell : self.stopButton.cell) as? NSButtonCell
            if snapshot.health == .stopped {
                self.stopButton.title = "Start Hub"
                self.stopButton.action = #selector(self.startPressed)
            } else {
                self.stopButton.title = "Stop Hub"
                self.stopButton.action = #selector(self.stopPressed)
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
                for (index, entry) in snapshot.activity.enumerated() {
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
        }
    }

    private func compactButton(_ title: String, _ symbol: String, _ action: Selector) -> NSButton {
        let button = NSButton(title: title, target: self, action: action)
        button.bezelStyle = .rounded
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
        button.imagePosition = .imageLeading
        button.controlSize = .regular
        return button
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

    @objc private func importPressed() {
        importSheet = ImportSheetController(controller: controller)
        guard let sheet = importSheet, let window else { return }
        window.beginSheet(sheet.window!) { [weak self] _ in
            self?.importSheet = nil
            self?.update()
        }
    }

    @objc private func logsPressed() {
        logsWindow = LogsWindowController(controller: controller)
        logsWindow?.showWindow(nil)
        logsWindow?.window?.makeKeyAndOrderFront(nil)
    }

    @objc private func installPressed() {
        controller.installService { [weak self] result in
            switch result { case .success: self?.update(); case let .failure(error): self?.showError(error) }
        }
    }

    @objc private func startPressed() {
        controller.startHub { [weak self] result in
            switch result { case .success: self?.update(); case let .failure(error): self?.showError(error) }
        }
    }

    @objc private func stopPressed() {
        controller.stopHub { [weak self] result in
            switch result { case .success: self?.update(); case let .failure(error): self?.showError(error) }
        }
    }

    @objc private func restartPressed() {
        controller.restartHub { [weak self] result in
            switch result { case .success: self?.update(); case let .failure(error): self?.showError(error) }
        }
    }

    @objc private func detailsPressed() {
        detailsWindow = ServiceDetailsWindowController(snapshot: controller.snapshot)
        detailsWindow?.showWindow(nil)
        detailsWindow?.window?.makeKeyAndOrderFront(nil)
    }

    @objc private func diagnosticsPressed() {
        diagnosticsWindow = DiagnosticsWindowController(controller: controller)
        diagnosticsWindow?.showWindow(nil)
        diagnosticsWindow?.window?.makeKeyAndOrderFront(nil)
    }

    @objc private func folderPressed() { controller.showDataFolder() }
}

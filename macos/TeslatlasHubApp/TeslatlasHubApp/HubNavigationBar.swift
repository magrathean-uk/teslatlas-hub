// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

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
    private var accountConnected = false
    private let dashboardButton = HubActionButton(title: "Dashboard", target: nil, action: nil)
    private let vehiclesButton = HubActionButton(title: "Vehicles", target: nil, action: nil)
    private let diagnosticsButton = HubActionButton(title: "Diagnostics", target: nil, action: nil)
    private let logsButton = HubActionButton(title: "Logs", target: nil, action: nil)
    private let serviceButton = HubActionButton(title: "Service Details", target: nil, action: nil)
    private let accountButton = HubActionButton(title: "Connect Tesla", target: nil, action: nil)
    private let importButton = HubActionButton(title: "Import", target: nil, action: nil)

    init(actions: HubNavigationActions) {
        self.actions = actions
        super.init(frame: .zero)

        configure(dashboardButton, identifier: "hub.nav.dashboard", symbol: "waveform.path.ecg",
                  action: #selector(dashboardPressed))
        configure(vehiclesButton, identifier: "hub.nav.vehicles", symbol: "car",
                  action: #selector(vehiclesPressed))
        configure(diagnosticsButton, identifier: "hub.nav.diagnostics", symbol: "checkmark.shield",
                  action: #selector(diagnosticsPressed))
        configure(logsButton, identifier: "hub.nav.logs", symbol: "doc.text",
                  action: #selector(logsPressed))
        configure(serviceButton, identifier: "hub.nav.service", symbol: "sun.max",
                  action: #selector(servicePressed))
        configure(importButton, identifier: "hub.nav.import", symbol: "arrow.down.to.line",
                  action: #selector(importPressed))
        configure(accountButton, identifier: "hub.nav.account", symbol: nil,
                  action: #selector(accountPressed(_:)))

        let leftButtons = NSStackView(views: [dashboardButton, vehiclesButton, diagnosticsButton,
                                             logsButton, serviceButton])
        leftButtons.spacing = 2
        leftButtons.alignment = .centerY
        let leftContainer = HubSurfaceView(fill: .navigationGroup)
        leftContainer.wantsLayer = true
        leftContainer.layer?.cornerRadius = 9
        leftContainer.layer?.cornerCurve = .continuous
        leftContainer.translatesAutoresizingMaskIntoConstraints = false
        leftButtons.translatesAutoresizingMaskIntoConstraints = false
        leftContainer.addSubview(leftButtons)
        NSLayoutConstraint.activate([
            leftButtons.leadingAnchor.constraint(equalTo: leftContainer.leadingAnchor, constant: 2),
            leftButtons.trailingAnchor.constraint(equalTo: leftContainer.trailingAnchor, constant: -2),
            leftButtons.topAnchor.constraint(equalTo: leftContainer.topAnchor, constant: 2),
            leftButtons.bottomAnchor.constraint(equalTo: leftContainer.bottomAnchor, constant: -2)
        ])

        let spacer = NSView()
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let root = NSStackView(views: [leftContainer, spacer, importButton, accountButton])
        root.spacing = 6
        root.alignment = .centerY
        root.translatesAutoresizingMaskIntoConstraints = false
        addSubview(root)
        NSLayoutConstraint.activate([
            root.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            root.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            root.topAnchor.constraint(equalTo: topAnchor, constant: 7),
            root.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -7),
            heightAnchor.constraint(equalToConstant: HubMetrics.navigationHeight)
        ])
        importButton.hubStyle = .flat
        select(.dashboard)
    }

    func select(_ section: HubMainSection) {
        dashboardButton.hubStyle = section == .dashboard ? .neutral : .flat
        vehiclesButton.hubStyle = section == .vehicles ? .neutral : .flat
    }

    func apply(snapshot: HubSnapshot, enabled: Bool) {
        let connected = snapshot.account == "Connected"
        accountConnected = connected
        accountButton.title = connected ? "Manage Tesla" : "Connect Tesla"
        accountButton.hubStyle = connected ? .neutral : .primary
        accountButton.image = connected
            ? NSImage(systemSymbolName: "chevron.down", accessibilityDescription: nil)
            : nil
        accountButton.imagePosition = connected ? .imageTrailing : .noImage
        accountButton.isEnabled = enabled
        importButton.isEnabled = enabled
    }

    func showAccountMenuForPreview() {
        guard accountConnected else { return }
        actions.manageTesla(accountButton)
    }

    @objc private func dashboardPressed() { actions.select(.dashboard) }
    @objc private func vehiclesPressed() { actions.select(.vehicles) }
    @objc private func diagnosticsPressed() { actions.diagnostics() }
    @objc private func logsPressed() { actions.logs() }
    @objc private func servicePressed() { actions.serviceDetails() }
    @objc private func importPressed() { actions.importTeslaMate() }
    @objc private func accountPressed(_ sender: NSButton) {
        accountConnected ? actions.manageTesla(sender) : actions.connectTesla()
    }

    private func configure(_ button: HubActionButton,
                           identifier: String,
                           symbol: String?,
                           action: Selector) {
        button.identifier = NSUserInterfaceItemIdentifier(identifier)
        button.target = self
        button.action = action
        button.hubStyle = .flat
        button.hubFont = .systemFont(ofSize: 12, weight: .medium)
        button.horizontalInset = 10
        button.iconBoxSize = 15
        button.setContentCompressionResistancePriority(.required, for: .horizontal)
        button.image = symbol.flatMap { NSImage(systemSymbolName: $0, accessibilityDescription: button.title) }
        button.imagePosition = symbol == nil ? .noImage : .imageLeading
        button.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 13, weight: .regular)
        button.heightAnchor.constraint(equalToConstant: 28).isActive = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

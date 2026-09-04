// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

enum HubServiceTransition: Equatable {
    case starting
    case stopping
    case restarting

    var title: String {
        switch self {
        case .starting: return "Starting Hub…"
        case .stopping: return "Stopping Hub…"
        case .restarting: return "Restarting Hub…"
        }
    }

    var subtitle: String {
        switch self {
        case .starting: return "Preparing vehicle data collection."
        case .stopping: return "Finishing current work and stopping safely."
        case .restarting: return "Restarting the background service."
        }
    }

    var service: String {
        switch self {
        case .starting: return "Starting…"
        case .stopping: return "Stopping…"
        case .restarting: return "Restarting…"
        }
    }

    var symbol: String {
        switch self {
        case .starting: return "play.circle"
        case .stopping: return "pause.circle"
        case .restarting: return "arrow.clockwise.circle"
        }
    }
}

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

private final class HubDashboardStatusTile: NSView {
    var tone: HubStatusTone = .neutral {
        didSet { updateLayer() }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.cornerRadius = 14
        layer?.cornerCurve = .continuous
        updateLayer()
    }

    override func updateLayer() {
        layer?.backgroundColor = tone.color.withAlphaComponent(0.12).cgColor
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateLayer()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

final class HubDashboardView: HubSurfaceView {
    private let actions: HubDashboardActions
    private let hero = NSStackView()
    private let heroTile = HubDashboardStatusTile()
    private let heroTitle = NSTextField(labelWithString: "")
    private let heroSubtitle = NSTextField(wrappingLabelWithString: "")
    private let heroSymbol = NSImageView()
    private let heroProgress = NSProgressIndicator()
    private let startStopButton = HubActionButton(title: "Stop Hub…", target: nil, action: nil)
    private let restartButton = HubActionButton(title: "Restart Hub", target: nil, action: nil)
    private let setupButton = HubActionButton(title: "Set Up Hub", target: nil, action: nil)
    private let diagnosticsButton = HubActionButton(title: "Run Diagnostics", target: nil, action: nil)
    private let vehicleCard: HubVehicleCardView
    private let serviceRow = HubStatusRowView(symbol: "sun.max", title: "Service")
    private let accountRow = HubStatusRowView(symbol: "person", title: "Tesla account")
    private let databaseRow = HubStatusRowView(symbol: "cylinder", title: "Database")
    private let activityStack = NSStackView()
    private let versionLabel = NSTextField(labelWithString: "")
    private var selectedVehicleID: UUID?
    private var interactionsEnabled = true
    private var vehicleControlsEnabled = true

    var defaultButton: NSButton? {
        if !setupButton.isHidden { return setupButton }
        if !diagnosticsButton.isHidden { return diagnosticsButton }
        if startStopButton.title == "Start Hub" { return startStopButton }
        return nil
    }

    init(actions: HubDashboardActions) {
        self.actions = actions
        vehicleCard = HubVehicleCardView(actions: actions.vehicle)
        super.init(fill: .background)
        buildHero()

        let statusCard = HubCardView()
        statusCard.identifier = NSUserInterfaceItemIdentifier("hub.dashboard.status-card")
        let statusStack = NSStackView(views: [serviceRow, separator(), accountRow, separator(), databaseRow])
        statusStack.orientation = .vertical
        statusStack.translatesAutoresizingMaskIntoConstraints = false
        statusCard.addSubview(statusStack)
        NSLayoutConstraint.activate([
            statusStack.leadingAnchor.constraint(equalTo: statusCard.leadingAnchor),
            statusStack.trailingAnchor.constraint(equalTo: statusCard.trailingAnchor),
            statusStack.topAnchor.constraint(equalTo: statusCard.topAnchor),
            statusStack.bottomAnchor.constraint(equalTo: statusCard.bottomAnchor)
        ])

        let activityHeadingIcon = NSImageView(image: NSImage(systemSymbolName: "waveform.path.ecg",
                                                              accessibilityDescription: nil) ?? NSImage())
        activityHeadingIcon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 10, weight: .regular)
        activityHeadingIcon.contentTintColor = HubPalette.mutedForeground
        let activityTitle = NSTextField(labelWithString: "LATEST ACTIVITY")
        activityTitle.font = .systemFont(ofSize: 10.5, weight: .semibold)
        activityTitle.textColor = HubPalette.mutedForeground
        let activityHeading = NSStackView(views: [activityHeadingIcon, activityTitle])
        activityHeading.alignment = .centerY
        activityHeading.spacing = 6

        let activityCard = HubCardView()
        activityCard.identifier = NSUserInterfaceItemIdentifier("hub.dashboard.activity-card")
        activityStack.orientation = .vertical
        activityStack.spacing = 0
        activityStack.translatesAutoresizingMaskIntoConstraints = false
        activityCard.addSubview(activityStack)
        NSLayoutConstraint.activate([
            activityStack.leadingAnchor.constraint(equalTo: activityCard.leadingAnchor),
            activityStack.trailingAnchor.constraint(equalTo: activityCard.trailingAnchor),
            activityStack.topAnchor.constraint(equalTo: activityCard.topAnchor),
            activityStack.bottomAnchor.constraint(equalTo: activityCard.bottomAnchor)
        ])
        let activitySection = NSStackView(views: [activityHeading, activityCard])
        activitySection.orientation = .vertical
        activitySection.alignment = .leading
        activitySection.spacing = 7

        let details = flatButton(title: "Service Details", symbol: nil,
                                 action: #selector(serviceDetailsPressed))
        let folder = flatButton(title: "Data Folder", symbol: "folder",
                                action: #selector(dataFolderPressed))
        versionLabel.font = .systemFont(ofSize: 11.5)
        versionLabel.textColor = HubPalette.mutedForeground
        let footerLine = separator()
        let footerRow = NSStackView(views: [versionLabel, NSView(), details, folder])
        footerRow.alignment = .centerY
        footerRow.spacing = 4
        let footer = NSStackView(views: [footerLine, footerRow])
        footer.orientation = .vertical
        footer.spacing = 10

        let content = NSStackView(views: [hero, vehicleCard, statusCard, activitySection, footer])
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = HubMetrics.sectionSpacing
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        NSLayoutConstraint.activate([
            content.widthAnchor.constraint(equalToConstant: HubMetrics.contentWidth),
            content.centerXAnchor.constraint(equalTo: centerXAnchor),
            content.topAnchor.constraint(equalTo: topAnchor, constant: HubMetrics.pageInset),
            content.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -HubMetrics.pageInset),
            hero.widthAnchor.constraint(equalTo: content.widthAnchor),
            vehicleCard.widthAnchor.constraint(equalTo: content.widthAnchor),
            statusCard.widthAnchor.constraint(equalTo: content.widthAnchor),
            activitySection.widthAnchor.constraint(equalTo: content.widthAnchor),
            activityCard.widthAnchor.constraint(equalTo: activitySection.widthAnchor),
            footer.widthAnchor.constraint(equalTo: content.widthAnchor),
            footerLine.widthAnchor.constraint(equalTo: footer.widthAnchor),
            footerRow.widthAnchor.constraint(equalTo: footer.widthAnchor)
        ])
    }

    func apply(snapshot: HubSnapshot,
               transition: HubServiceTransition?,
               activity: [HubActivity]) {
        heroTitle.stringValue = transition?.title ?? snapshot.health.title
        heroSubtitle.stringValue = transition?.subtitle ?? subtitle(for: snapshot.health)
        heroSymbol.image = NSImage(systemSymbolName: transition?.symbol ?? symbol(for: snapshot.health),
                                   accessibilityDescription: heroTitle.stringValue)
        let tone = tone(for: snapshot.health)
        heroTile.tone = tone
        heroSymbol.contentTintColor = tone.color
        heroSymbol.isHidden = transition != nil
        heroProgress.isHidden = transition == nil
        if transition == nil { heroProgress.stopAnimation(nil) } else { heroProgress.startAnimation(nil) }

        serviceRow.value = transition?.service ?? compactServiceValue(snapshot)
        serviceRow.statusTone = tone
        accountRow.value = snapshot.accountDisplay
        databaseRow.value = snapshot.database
        versionLabel.stringValue = "Teslatlas Hub \(snapshot.version)"

        let availableIDs = Set(snapshot.controlVehicles.map(\.id))
        if let selectedVehicleID, !availableIDs.contains(selectedVehicleID) {
            self.selectedVehicleID = nil
        }
        if selectedVehicleID == nil {
            selectedVehicleID = snapshot.controlVehicleID ?? snapshot.controlVehicles.first?.id
        }
        let selectedVehicle = snapshot.controlVehicles.first { $0.id == selectedVehicleID }
        let controlsEnabled = vehicleControlsEnabled && interactionsEnabled && transition == nil
            && snapshot.health == .running && snapshot.account == "Connected"
        vehicleCard.apply(vehicle: selectedVehicle,
                          allVehicles: snapshot.controlVehicles,
                          provider: snapshot.provider,
                          enabled: controlsEnabled,
                          emptyTitle: snapshot.vehicleName,
                          emptyStatus: snapshot.vehicle)
        vehicleCard.isHidden = snapshot.controlVehicles.isEmpty
        updateHeroActions(snapshot: snapshot, transition: transition)
        render(activity: activity.isEmpty ? snapshot.activity : activity)
    }

    func setInteractionsEnabled(_ enabled: Bool) { interactionsEnabled = enabled }
    func setVehicleControlsEnabled(_ enabled: Bool) { vehicleControlsEnabled = enabled }
    func selectVehicle(id: UUID) { selectedVehicleID = id }

    private func buildHero() {
        hero.identifier = NSUserInterfaceItemIdentifier("hub.dashboard.hero")
        hero.orientation = .horizontal
        hero.alignment = .centerY
        hero.spacing = 14

        heroTile.translatesAutoresizingMaskIntoConstraints = false
        heroTile.widthAnchor.constraint(equalToConstant: 48).isActive = true
        heroTile.heightAnchor.constraint(equalToConstant: 48).isActive = true
        heroSymbol.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 22, weight: .medium)
        heroSymbol.translatesAutoresizingMaskIntoConstraints = false
        heroProgress.style = .spinning
        heroProgress.controlSize = .small
        heroProgress.isDisplayedWhenStopped = false
        heroProgress.translatesAutoresizingMaskIntoConstraints = false
        heroTile.addSubview(heroSymbol)
        heroTile.addSubview(heroProgress)
        NSLayoutConstraint.activate([
            heroSymbol.centerXAnchor.constraint(equalTo: heroTile.centerXAnchor),
            heroSymbol.centerYAnchor.constraint(equalTo: heroTile.centerYAnchor),
            heroProgress.centerXAnchor.constraint(equalTo: heroTile.centerXAnchor),
            heroProgress.centerYAnchor.constraint(equalTo: heroTile.centerYAnchor)
        ])

        heroTitle.font = .systemFont(ofSize: 19, weight: .bold)
        heroTitle.textColor = HubPalette.foreground
        heroSubtitle.font = .systemFont(ofSize: 13)
        heroSubtitle.textColor = HubPalette.mutedForeground
        heroSubtitle.maximumNumberOfLines = 2
        let copy = NSStackView(views: [heroTitle, heroSubtitle])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2

        startStopButton.target = self
        startStopButton.action = #selector(startStopPressed)
        restartButton.target = self
        restartButton.action = #selector(restartPressed)
        setupButton.target = self
        setupButton.action = #selector(setupPressed)
        diagnosticsButton.target = self
        diagnosticsButton.action = #selector(diagnosticsPressed)
        [startStopButton, restartButton, setupButton, diagnosticsButton].forEach {
            $0.hubFont = .systemFont(ofSize: 12, weight: .medium)
            $0.heightAnchor.constraint(equalToConstant: 28).isActive = true
        }
        let controls = NSStackView(views: [startStopButton, restartButton,
                                           diagnosticsButton, setupButton])
        controls.alignment = .centerY
        controls.spacing = 8

        hero.addArrangedSubview(heroTile)
        hero.addArrangedSubview(copy)
        hero.addArrangedSubview(NSView())
        hero.addArrangedSubview(controls)
    }

    private func updateHeroActions(snapshot: HubSnapshot, transition: HubServiceTransition?) {
        let enabled = interactionsEnabled && transition == nil
        startStopButton.isHidden = snapshot.health == .needsInstall
        restartButton.isHidden = snapshot.health == .needsInstall
        diagnosticsButton.isHidden = snapshot.health == .running || snapshot.health == .needsInstall
        setupButton.isHidden = snapshot.health != .needsInstall
        [startStopButton, restartButton, diagnosticsButton, setupButton].forEach { $0.isEnabled = enabled }

        if snapshot.health == .stopped {
            startStopButton.title = "Start Hub"
            startStopButton.hubStyle = .primary
            startStopButton.keyEquivalent = "\r"
        } else {
            startStopButton.title = "Stop Hub…"
            startStopButton.hubStyle = .flatDanger
            startStopButton.keyEquivalent = ""
        }
        restartButton.hubStyle = snapshot.health == .degraded ? .primary : .neutral
        diagnosticsButton.hubStyle = .neutral
        setupButton.hubStyle = .primary
        setupButton.keyEquivalent = snapshot.health == .needsInstall ? "\r" : ""
        diagnosticsButton.keyEquivalent = snapshot.health == .degraded ? "\r" : ""
    }

    private func render(activity: [HubActivity]) {
        activityStack.arrangedSubviews.forEach {
            activityStack.removeArrangedSubview($0)
            $0.removeFromSuperview()
        }
        let entries = activity.isEmpty
            ? [HubActivity(message: "No activity yet.", age: "", color: HubPalette.mutedForeground)]
            : Array(activity.prefix(3))
        for (index, entry) in entries.enumerated() {
            if index > 0 {
                let line = separator()
                activityStack.addArrangedSubview(line)
                line.widthAnchor.constraint(equalTo: activityStack.widthAnchor).isActive = true
            }
            let message = NSTextField(labelWithString: entry.message)
            message.font = .systemFont(ofSize: 12)
            message.textColor = activity.isEmpty ? HubPalette.mutedForeground : HubPalette.foreground
            let age = NSTextField(labelWithString: entry.age)
            age.font = .systemFont(ofSize: 11.5)
            age.textColor = HubPalette.mutedForeground
            let row = NSStackView(views: [message, NSView(), age])
            row.alignment = .centerY
            row.edgeInsets = NSEdgeInsets(top: 9, left: 14, bottom: 9, right: 14)
            activityStack.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: activityStack.widthAnchor).isActive = true
        }
    }

    private func flatButton(title: String, symbol: String?, action: Selector) -> HubActionButton {
        let button = HubActionButton(title: title, target: self, action: action)
        button.hubStyle = .flat
        button.hubFont = .systemFont(ofSize: 12, weight: .medium)
        button.image = symbol.flatMap { NSImage(systemSymbolName: $0, accessibilityDescription: title) }
        button.imagePosition = symbol == nil ? .noImage : .imageLeading
        button.heightAnchor.constraint(equalToConstant: 27).isActive = true
        return button
    }

    private func separator() -> NSView {
        HubModalChrome.divider()
    }

    private func compactServiceValue(_ snapshot: HubSnapshot) -> String {
        snapshot.health == .running ? "Active" : snapshot.service
    }

    private func subtitle(for health: HubHealth) -> String {
        switch health {
        case .running: return "Hub runs in the background. You can close this window."
        case .stopped: return "Vehicle data is not being collected."
        case .needsInstall: return "Choose how Hub connects to Tesla."
        case .degraded: return "Open diagnostics for details."
        }
    }

    private func symbol(for health: HubHealth) -> String {
        switch health {
        case .running: return "checkmark.shield"
        case .stopped: return "pause.circle"
        case .needsInstall: return "wrench.and.screwdriver"
        case .degraded: return "exclamationmark.triangle"
        }
    }

    private func tone(for health: HubHealth) -> HubStatusTone {
        switch health {
        case .running: return .success
        case .stopped: return .warning
        case .needsInstall: return .neutral
        case .degraded: return .danger
        }
    }

    @objc private func startStopPressed() {
        startStopButton.title == "Start Hub" ? actions.start() : actions.stop()
    }
    @objc private func restartPressed() { actions.restart() }
    @objc private func setupPressed() { actions.setup() }
    @objc private func diagnosticsPressed() { actions.diagnostics() }
    @objc private func serviceDetailsPressed() { actions.serviceDetails() }
    @objc private func dataFolderPressed() { actions.dataFolder() }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

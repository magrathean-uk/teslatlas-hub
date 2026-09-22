// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

struct HubVehicleCardActions {
    let select: (UUID) -> Void
    let command: (HubVehicleControl, UUID) -> Void
}

final class HubVehicleCardView: NSView {
    private static let commandGroups: [String: [HubVehicleControl]] = [
        "climate": [.climateStart, .climateStop],
        "access": [.lock, .unlock],
        "more": [.wake, .flashLights, .honkHorn]
    ]
    private let actions: HubVehicleCardActions
    private let nameLabel = NSTextField(labelWithString: "")
    private let statusLabel = NSTextField(wrappingLabelWithString: "")
    private let selector = NSPopUpButton()
    private let commandStack = NSStackView()
    private let stateRow = HubStatusRowView(symbol: "info.circle", title: "State",
                                            detail: "")
    private let batteryRow = HubStatusRowView(symbol: "battery.75percent", title: "Battery",
                                              detail: "")
    private let locationRow = HubStatusRowView(symbol: "location", title: "Location",
                                               detail: "")
    private let connectionRow = HubStatusRowView(symbol: "wifi", title: "Connection",
                                                 detail: "")
    private let legacySurface = HubSurfaceView(fill: .elevated)
    private let legacyMessage = NSTextField(wrappingLabelWithString:
        "Vehicle commands are available when Hub connects through Fleet Telemetry.")
    private var representedVehicles: [HubControlVehicle] = []
    private var selectedVehicleID: UUID?
    private var commandGroupButtons: [HubActionButton] = []

    init(actions: HubVehicleCardActions) {
        self.actions = actions
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("hub.dashboard.vehicle-card")

        let card = HubCardView()
        card.translatesAutoresizingMaskIntoConstraints = false
        addSubview(card)
        NSLayoutConstraint.activate([
            card.leadingAnchor.constraint(equalTo: leadingAnchor),
            card.trailingAnchor.constraint(equalTo: trailingAnchor),
            card.topAnchor.constraint(equalTo: topAnchor),
            card.bottomAnchor.constraint(equalTo: bottomAnchor)
        ])

        let carTile = HubIconTileView(symbol: "car.side", accessibilityDescription: "Vehicle",
                                      size: 40, symbolSize: 20, weight: .medium,
                                      tint: HubPalette.accent, radius: 9)

        nameLabel.font = .systemFont(ofSize: 17, weight: .semibold)
        nameLabel.textColor = HubPalette.foreground
        nameLabel.lineBreakMode = .byTruncatingTail
        statusLabel.font = .systemFont(ofSize: 12.5)
        statusLabel.textColor = HubPalette.mutedForeground
        statusLabel.maximumNumberOfLines = 2
        let titleStack = NSStackView(views: [nameLabel, statusLabel])
        titleStack.orientation = .vertical
        titleStack.alignment = .leading
        titleStack.spacing = 3

        selector.target = self
        selector.action = #selector(selectionChanged)
        selector.controlSize = .regular
        selector.widthAnchor.constraint(greaterThanOrEqualToConstant: 108).isActive = true
        let header = NSStackView(views: [carTile, titleStack, NSView(), selector])
        header.alignment = .centerY
        header.spacing = 12

        let groups: [(String, String, [HubVehicleControl])] = [
            ("Climate", "fan", [.climateStart, .climateStop]),
            ("Access", "lock", [.lock, .unlock]),
            ("More", "ellipsis.circle", [.wake, .flashLights, .honkHorn])
        ]
        commandStack.orientation = .vertical
        commandStack.alignment = .leading
        commandStack.spacing = 0
        for (index, group) in groups.enumerated() {
            let (title, symbol, commands) = group
            let key = commands == Self.commandGroups["climate"] ? "climate"
                : commands == Self.commandGroups["access"] ? "access" : "more"
            if index > 0 {
                let line = HubOnboardingHairlineView()
                commandStack.addArrangedSubview(line)
                line.widthAnchor.constraint(equalTo: commandStack.widthAnchor).isActive = true
            }
            let button = HubActionButton(title: title,
                                         target: self, action: #selector(showCommands(_:)))
            button.identifier = NSUserInterfaceItemIdentifier(key)
            button.hubFont = HubTypography.action
            button.hubStyle = .neutral
            button.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
            button.widthAnchor.constraint(greaterThanOrEqualToConstant: HubMetrics.actionMinimumWidth).isActive = true
            button.setAccessibilityHelp("Choose a \(title.lowercased()) command")
            commandGroupButtons.append(button)
            let detail: String
            switch key {
            case "climate": detail = "Start or stop cabin climate."
            case "access": detail = "Lock or unlock your vehicle."
            default: detail = "Wake, flash lights, or honk."
            }
            let row = commandRow(symbol: symbol, title: title == "More" ? "Other commands" : title,
                                 detail: detail, control: button)
            commandStack.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: commandStack.widthAnchor).isActive = true
        }

        legacyMessage.font = .systemFont(ofSize: 11.5)
        legacyMessage.textColor = HubPalette.mutedForeground
        legacyMessage.maximumNumberOfLines = 2
        legacyMessage.translatesAutoresizingMaskIntoConstraints = false
        legacySurface.identifier = NSUserInterfaceItemIdentifier("hub.vehicle.legacy-message")
        legacySurface.wantsLayer = true
        legacySurface.layer?.cornerRadius = 9
        legacySurface.layer?.cornerCurve = .continuous
        legacySurface.addSubview(legacyMessage)
        NSLayoutConstraint.activate([
            legacyMessage.leadingAnchor.constraint(equalTo: legacySurface.leadingAnchor, constant: 12),
            legacyMessage.trailingAnchor.constraint(equalTo: legacySurface.trailingAnchor, constant: -12),
            legacyMessage.topAnchor.constraint(equalTo: legacySurface.topAnchor, constant: 8),
            legacyMessage.bottomAnchor.constraint(equalTo: legacySurface.bottomAnchor, constant: -8)
        ])

        let commandTitle = NSTextField(labelWithString: "Vehicle controls")
        commandTitle.font = .systemFont(ofSize: 14, weight: .medium)
        commandTitle.textColor = HubPalette.mutedForeground
        let commandCard = HubSurfaceView(fill: .navigationGroup)
        commandCard.wantsLayer = true
        commandCard.layer?.cornerRadius = 10
        commandCard.layer?.cornerCurve = .continuous
        commandStack.translatesAutoresizingMaskIntoConstraints = false
        commandCard.addSubview(commandStack)
        NSLayoutConstraint.activate([
            commandStack.leadingAnchor.constraint(equalTo: commandCard.leadingAnchor),
            commandStack.trailingAnchor.constraint(equalTo: commandCard.trailingAnchor),
            commandStack.topAnchor.constraint(equalTo: commandCard.topAnchor),
            commandStack.bottomAnchor.constraint(equalTo: commandCard.bottomAnchor)
        ])
        let commandArea = NSStackView(views: [commandTitle, commandCard])
        commandArea.orientation = .vertical
        commandArea.alignment = .leading
        commandArea.spacing = 7
        commandCard.widthAnchor.constraint(equalTo: commandArea.widthAnchor).isActive = true

        let stateCard = HubSurfaceView(fill: .navigationGroup)
        stateCard.wantsLayer = true
        stateCard.layer?.cornerRadius = 10
        stateCard.layer?.cornerCurve = .continuous
        let stateStack = NSStackView(views: [
            stateRow, HubModalChrome.divider(),
            batteryRow, HubModalChrome.divider(),
            locationRow, HubModalChrome.divider(), connectionRow
        ])
        stateStack.orientation = .vertical
        stateStack.spacing = 0
        stateStack.translatesAutoresizingMaskIntoConstraints = false
        stateCard.addSubview(stateStack)
        NSLayoutConstraint.activate([
            stateStack.leadingAnchor.constraint(equalTo: stateCard.leadingAnchor),
            stateStack.trailingAnchor.constraint(equalTo: stateCard.trailingAnchor),
            stateStack.topAnchor.constraint(equalTo: stateCard.topAnchor),
            stateStack.bottomAnchor.constraint(equalTo: stateCard.bottomAnchor)
        ])

        let content = NSStackView(views: [header, stateCard, commandArea, legacySurface])
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 14
        content.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(content)
        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 18),
            content.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -18),
            content.topAnchor.constraint(equalTo: card.topAnchor, constant: 18),
            content.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -18),
            header.widthAnchor.constraint(equalTo: content.widthAnchor),
            stateCard.widthAnchor.constraint(equalTo: content.widthAnchor),
            commandArea.widthAnchor.constraint(equalTo: content.widthAnchor),
            legacySurface.widthAnchor.constraint(equalTo: content.widthAnchor)
        ])
    }

    func apply(vehicle: HubControlVehicle?, allVehicles: [HubControlVehicle],
               provider: HubAccountProvider?, enabled: Bool,
               emptyTitle: String = "Vehicle", emptyStatus: String = "No configured vehicle") {
        if selectedVehicleID != nil, selectedVehicleID != vehicle?.id { HubMotion.transition(self) }
        representedVehicles = allVehicles
        selectedVehicleID = vehicle?.id
        selector.removeAllItems()
        selector.addItems(withTitles: allVehicles.map(\.displayName))
        if let vehicle, let index = allVehicles.firstIndex(where: { $0.id == vehicle.id }) {
            selector.selectItem(at: index)
        }
        selector.isHidden = allVehicles.count < 2
        selector.isEnabled = enabled && allVehicles.count > 1
        nameLabel.stringValue = vehicle?.displayName ?? emptyTitle
        statusLabel.stringValue = vehicle?.status.components(separatedBy: " · ").first ?? emptyStatus
        let components = vehicle?.status.components(separatedBy: " · ") ?? []
        stateRow.value = components.dropFirst().first(where: { !$0.contains("%") }) ?? "Unavailable"
        batteryRow.value = components.first(where: { $0.contains("%") }) ?? "Unavailable"
        locationRow.value = components.count > 3 ? (components.last ?? "Unavailable") : "Unavailable"
        connectionRow.value = vehicle == nil ? "Unavailable" : "Available"
        connectionRow.statusTone = vehicle == nil ? .warning : .success

        let fleet = provider == .fleet
        commandStack.superview?.superview?.isHidden = !fleet
        legacySurface.isHidden = fleet
        commandGroupButtons.forEach { $0.isEnabled = enabled && fleet && vehicle != nil }
        setAccessibilityLabel(vehicle.map { "\($0.displayName), \($0.status)" } ?? emptyTitle)
    }

    @objc private func selectionChanged() {
        guard representedVehicles.indices.contains(selector.indexOfSelectedItem) else { return }
        let vehicle = representedVehicles[selector.indexOfSelectedItem]
        selectedVehicleID = vehicle.id
        actions.select(vehicle.id)
    }

    private func perform(_ command: HubVehicleControl) {
        guard let selectedVehicleID else { return }
        actions.command(command, selectedVehicleID)
    }

    @objc private func showCommands(_ sender: HubActionButton) {
        guard let key = sender.identifier?.rawValue,
              let commands = Self.commandGroups[key] else { return }
        let commandMenu = NSMenu(title: sender.title)
        commandMenu.autoenablesItems = false
        for command in commands {
            let item = NSMenuItem(title: commandTitle(command), action: #selector(runCommand(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = command.rawValue
            item.image = NSImage(systemSymbolName: symbol(command), accessibilityDescription: command.title)
            item.isEnabled = sender.isEnabled
            commandMenu.addItem(item)
        }
        commandMenu.popUp(positioning: nil, at: NSPoint(x: 0, y: sender.bounds.maxY + 4), in: sender)
    }

    @objc private func runCommand(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String,
              let command = HubVehicleControl(rawValue: raw) else { return }
        perform(command)
    }

    private func commandTitle(_ command: HubVehicleControl) -> String {
        switch command {
        case .climateStart: return "Start Climate"
        case .climateStop: return "Stop Climate"
        case .wake: return "Wake Vehicle"
        case .lock: return "Lock Doors"
        case .unlock: return "Unlock Doors"
        case .flashLights: return "Flash Lights"
        case .honkHorn: return "Honk Horn"
        }
    }

    private func symbol(_ command: HubVehicleControl) -> String {
        switch command {
        case .climateStart, .climateStop: return "fan"
        case .wake: return "power"
        case .lock: return "lock"
        case .unlock: return "lock.open"
        case .flashLights: return "bolt"
        case .honkHorn: return "speaker.wave.2"
        }
    }

    private func commandRow(symbol: String, title: String, detail: String,
                            control: HubActionButton) -> NSView {
        let tile = HubIconTileView(symbol: symbol, accessibilityDescription: title)
        let titleLabel = NSTextField(labelWithString: title)
        titleLabel.font = .systemFont(ofSize: 14, weight: .medium)
        let detailLabel = NSTextField(labelWithString: detail)
        detailLabel.font = .systemFont(ofSize: 12)
        detailLabel.textColor = HubPalette.mutedForeground
        let copy = NSStackView(views: [titleLabel, detailLabel])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2
        let row = NSStackView(views: [tile, copy, NSView(), control])
        row.alignment = .centerY
        row.spacing = 12
        row.edgeInsets = NSEdgeInsets(top: 10, left: HubMetrics.rowHorizontalInset,
                                     bottom: 10, right: HubMetrics.rowHorizontalInset)
        row.heightAnchor.constraint(equalToConstant: HubMetrics.rowHeight).isActive = true
        return row
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

private final class HubVehicleTableCellView: NSView {
    private let vehicleImage = NSImageView()
    private let titleLabel = NSTextField(labelWithString: "")
    private let detailLabel = NSTextField(labelWithString: "")

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        vehicleImage.image = NSImage(systemSymbolName: "car.side", accessibilityDescription: nil)
        vehicleImage.translatesAutoresizingMaskIntoConstraints = false
        titleLabel.font = .systemFont(ofSize: 13, weight: .medium)
        titleLabel.lineBreakMode = .byTruncatingTail
        detailLabel.font = .systemFont(ofSize: 11)
        detailLabel.lineBreakMode = .byTruncatingTail
        let labels = NSStackView(views: [titleLabel, detailLabel])
        labels.orientation = .vertical
        labels.alignment = .leading
        labels.spacing = 1
        let row = NSStackView(views: [vehicleImage, labels])
        row.alignment = .centerY
        row.spacing = 9
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            vehicleImage.widthAnchor.constraint(equalToConstant: 20),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            row.centerYAnchor.constraint(equalTo: centerYAnchor)
        ])
        titleLabel.textColor = HubPalette.foreground
        detailLabel.textColor = HubPalette.mutedForeground
        vehicleImage.contentTintColor = HubPalette.accent
    }

    func apply(_ vehicle: HubControlVehicle) {
        titleLabel.stringValue = vehicle.displayName
        detailLabel.stringValue = vehicle.status
        setAccessibilityLabel("\(vehicle.displayName), \(vehicle.status)")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

private final class HubVehicleTableRowView: NSTableRowView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        selectionHighlightStyle = .none
    }

    override var isSelected: Bool {
        didSet { needsDisplay = true }
    }

    override func drawBackground(in dirtyRect: NSRect) {
        super.drawBackground(in: dirtyRect)
        guard isSelected else { return }
        let selection = bounds.insetBy(dx: 5, dy: 2)
        HubPalette.accent.withAlphaComponent(isEmphasized ? 0.16 : 0.10).setFill()
        NSBezierPath(roundedRect: selection, xRadius: 8, yRadius: 8).fill()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

private final class HubVehicleTableView: NSTableView {
    override func highlightSelection(inClipRect clipRect: NSRect) {
        // The row view renders the quieter source-list selection treatment.
    }
}

final class HubVehiclesView: HubSurfaceView {
    private let actions: HubVehicleCardActions
    private let selector = NSPopUpButton()
    private let countLabel = NSTextField(labelWithString: "No connected vehicles")
    private var detailCard: HubVehicleCardView!
    private var vehicles: [HubControlVehicle] = []
    private var provider: HubAccountProvider?
    private var commandsEnabled = false
    private var selectedVehicleID: UUID?

    init(actions: HubVehicleCardActions) {
        self.actions = actions
        super.init(fill: .background)
        detailCard = HubVehicleCardView(actions: actions)

        let tile = HubIconTileView(symbol: "car.side", accessibilityDescription: "Vehicles",
                                   size: 48, symbolSize: 24, weight: .medium, radius: 12)
        let heading = NSTextField(labelWithString: "Vehicles")
        heading.font = .systemFont(ofSize: 24, weight: .semibold)
        countLabel.font = .systemFont(ofSize: 14)
        countLabel.textColor = HubPalette.mutedForeground
        let copy = NSStackView(views: [heading, countLabel])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2
        selector.target = self
        selector.action = #selector(selectionChanged)
        selector.controlSize = .large
        selector.setAccessibilityLabel("Selected vehicle")
        selector.widthAnchor.constraint(greaterThanOrEqualToConstant: 160).isActive = true
        let header = NSStackView(views: [tile, copy, NSView(), selector])
        header.alignment = .centerY
        header.spacing = 16

        let noteIcon = NSImageView(image: NSImage(systemSymbolName: "lock",
                                                   accessibilityDescription: nil) ?? NSImage())
        noteIcon.contentTintColor = .secondaryLabelColor
        let note = NSTextField(labelWithString: "Commands always ask for confirmation before they are sent.")
        note.font = .systemFont(ofSize: 12)
        note.textColor = .secondaryLabelColor
        let footer = NSStackView(views: [noteIcon, note, NSView(),
                                         NSTextField(labelWithString: "Teslatlas Hub \(HubRelease.bundledVersion)")])
        footer.alignment = .centerY
        footer.spacing = 8

        let content = NSStackView(views: [header, detailCard, NSView(), footer])
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 24
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        NSLayoutConstraint.activate([
            content.centerXAnchor.constraint(equalTo: centerXAnchor),
            content.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: HubMetrics.pageInset),
            content.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -HubMetrics.pageInset),
            content.widthAnchor.constraint(equalToConstant: HubMetrics.contentWidth),
            content.topAnchor.constraint(equalTo: topAnchor, constant: HubMetrics.pageTopInset),
            content.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -HubMetrics.pageInset),
            header.widthAnchor.constraint(equalTo: content.widthAnchor),
            detailCard.widthAnchor.constraint(equalTo: content.widthAnchor),
            footer.widthAnchor.constraint(equalTo: content.widthAnchor)
        ])
    }

    func apply(snapshot: HubSnapshot,
               selectedVehicleID preferredVehicleID: UUID? = nil,
               enabled: Bool) {
        vehicles = snapshot.controlVehicles
        provider = snapshot.provider
        commandsEnabled = enabled
        let validIDs = Set(vehicles.map(\.id))
        if let preferredVehicleID, validIDs.contains(preferredVehicleID) {
            selectedVehicleID = preferredVehicleID
        } else if let selectedVehicleID, !validIDs.contains(selectedVehicleID) {
            self.selectedVehicleID = nil
        }
        if selectedVehicleID == nil {
            selectedVehicleID = snapshot.controlVehicleID ?? vehicles.first?.id
        }
        countLabel.stringValue = vehicles.isEmpty
            ? "No connected vehicles"
            : "Your connected vehicles."
        selector.removeAllItems()
        selector.addItems(withTitles: vehicles.map(\.displayName))
        if let selectedVehicleID, let index = vehicles.firstIndex(where: { $0.id == selectedVehicleID }) {
            selector.selectItem(at: index)
        }
        selector.isHidden = vehicles.count < 2
        selector.isEnabled = enabled && vehicles.count > 1
        updateDetail()
    }

    func selectVehicle(id: UUID) {
        guard let index = vehicles.firstIndex(where: { $0.id == id }) else { return }
        selectedVehicleID = id
        selector.selectItem(at: index)
        updateDetail()
    }

    @objc private func selectionChanged() {
        guard vehicles.indices.contains(selector.indexOfSelectedItem) else { return }
        let vehicle = vehicles[selector.indexOfSelectedItem]
        selectedVehicleID = vehicle.id
        actions.select(vehicle.id)
        updateDetail()
    }

    private func updateDetail() {
        let selected = vehicles.first { $0.id == selectedVehicleID }
        detailCard.apply(vehicle: selected, allVehicles: selected.map { [$0] } ?? [],
                         provider: provider, enabled: commandsEnabled,
                         emptyTitle: "No vehicles yet",
                         emptyStatus: "Connect a Tesla account and start Hub to see vehicles here.")
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

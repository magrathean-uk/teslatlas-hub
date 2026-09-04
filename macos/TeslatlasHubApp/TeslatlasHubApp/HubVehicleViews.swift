// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

struct HubVehicleCardActions {
    let select: (UUID) -> Void
    let command: (HubVehicleControl, UUID) -> Void
}

final class HubVehicleCardView: NSView {
    private static let commandOrder: [HubVehicleControl] = [
        .climateStart, .climateStop, .wake, .lock, .unlock, .flashLights, .honkHorn
    ]

    private let actions: HubVehicleCardActions
    private let nameLabel = NSTextField(labelWithString: "")
    private let statusLabel = NSTextField(labelWithString: "")
    private let selector = NSPopUpButton()
    private let commandStack = NSStackView()
    private let legacySurface = HubSurfaceView(fill: .elevated)
    private let legacyMessage = NSTextField(wrappingLabelWithString:
        "Vehicle commands are available when Hub connects through Fleet Telemetry.")
    private var representedVehicles: [HubControlVehicle] = []
    private var commandButtons: [HubVehicleControl: HubActionButton] = [:]

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

        let carTile = HubSurfaceView(fill: .elevated)
        carTile.wantsLayer = true
        carTile.layer?.cornerRadius = 10
        carTile.layer?.cornerCurve = .continuous
        let car = NSImageView(image: NSImage(systemSymbolName: "car",
                                              accessibilityDescription: "Vehicle") ?? NSImage())
        car.contentTintColor = HubPalette.accent
        car.imageScaling = .scaleProportionallyDown
        car.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 15, weight: .medium)
        car.translatesAutoresizingMaskIntoConstraints = false
        carTile.addSubview(car)
        NSLayoutConstraint.activate([
            carTile.widthAnchor.constraint(equalToConstant: 35),
            carTile.heightAnchor.constraint(equalToConstant: 35),
            car.centerXAnchor.constraint(equalTo: carTile.centerXAnchor),
            car.centerYAnchor.constraint(equalTo: carTile.centerYAnchor),
            car.widthAnchor.constraint(equalToConstant: 18),
            car.heightAnchor.constraint(equalToConstant: 18)
        ])

        nameLabel.font = .systemFont(ofSize: 14, weight: .semibold)
        nameLabel.textColor = HubPalette.foreground
        nameLabel.lineBreakMode = .byTruncatingTail
        statusLabel.font = .systemFont(ofSize: 12)
        statusLabel.textColor = HubPalette.mutedForeground
        statusLabel.lineBreakMode = .byTruncatingTail
        let titleStack = NSStackView(views: [nameLabel, statusLabel])
        titleStack.orientation = .vertical
        titleStack.alignment = .leading
        titleStack.spacing = 2

        selector.target = self
        selector.action = #selector(selectionChanged)
        selector.controlSize = .regular
        selector.widthAnchor.constraint(greaterThanOrEqualToConstant: 92).isActive = true
        let header = NSStackView(views: [carTile, titleStack, NSView(), selector])
        header.alignment = .centerY
        header.spacing = 10

        commandStack.distribution = .fillEqually
        commandStack.spacing = 6
        for command in Self.commandOrder {
            let button = HubActionButton(title: displayTitle(for: command), target: self,
                                         action: #selector(commandPressed(_:)))
            button.identifier = NSUserInterfaceItemIdentifier(command.rawValue)
            button.image = NSImage(systemSymbolName: symbol(for: command),
                                   accessibilityDescription: command.title)
            button.imagePosition = .imageAbove
            button.horizontalInset = 3
            button.iconBoxSize = 16
            button.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 14, weight: .medium)
            button.hubFont = .systemFont(ofSize: 10, weight: .regular)
            button.hubStyle = .neutral
            button.heightAnchor.constraint(equalToConstant: 51).isActive = true
            commandButtons[command] = button
            commandStack.addArrangedSubview(button)
        }
        // NSStackView's fillEqually may preserve a button's intrinsic minimum.
        // Command tiles have a stricter contract: every painted frame is equal.
        if let first = commandButtons[Self.commandOrder[0]] {
            for button in commandButtons.values where button !== first {
                button.widthAnchor.constraint(equalTo: first.widthAnchor).isActive = true
            }
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

        let content = NSStackView(views: [header, commandStack, legacySurface])
        content.orientation = .vertical
        content.spacing = 12
        content.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(content)
        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 14),
            content.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -14),
            content.topAnchor.constraint(equalTo: card.topAnchor, constant: 14),
            content.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -14),
            header.widthAnchor.constraint(equalTo: content.widthAnchor),
            commandStack.widthAnchor.constraint(equalTo: content.widthAnchor),
            legacySurface.widthAnchor.constraint(equalTo: content.widthAnchor)
        ])
    }

    func apply(vehicle: HubControlVehicle?,
               allVehicles: [HubControlVehicle],
               provider: HubAccountProvider?,
               enabled: Bool,
               emptyTitle: String = "Vehicle",
               emptyStatus: String = "No configured vehicle") {
        representedVehicles = allVehicles
        selector.removeAllItems()
        selector.addItems(withTitles: allVehicles.map(\.displayName))
        if let vehicle,
           let index = allVehicles.firstIndex(where: { $0.id == vehicle.id }) {
            selector.selectItem(at: index)
        }
        selector.isHidden = allVehicles.count < 2
        selector.isEnabled = enabled && allVehicles.count > 1
        nameLabel.stringValue = vehicle?.displayName ?? emptyTitle
        statusLabel.stringValue = vehicle?.status ?? emptyStatus

        let fleet = provider == .fleet
        commandStack.isHidden = !fleet
        legacySurface.isHidden = fleet
        commandButtons.values.forEach {
            $0.isHidden = !fleet
            $0.isEnabled = enabled && fleet && vehicle != nil
        }
    }

    @objc private func selectionChanged() {
        guard representedVehicles.indices.contains(selector.indexOfSelectedItem) else { return }
        actions.select(representedVehicles[selector.indexOfSelectedItem].id)
    }

    @objc private func commandPressed(_ sender: NSButton) {
        let index = max(0, selector.indexOfSelectedItem)
        guard representedVehicles.indices.contains(index),
              let raw = sender.identifier?.rawValue,
              let command = HubVehicleControl(rawValue: raw) else { return }
        actions.command(command, representedVehicles[index].id)
    }

    private func symbol(for command: HubVehicleControl) -> String {
        switch command {
        case .climateStart, .climateStop: return "fan"
        case .wake: return "power"
        case .lock: return "lock"
        case .unlock: return "lock.open"
        case .flashLights: return "bolt"
        case .honkHorn: return "speaker.wave.2"
        }
    }

    private func displayTitle(for command: HubVehicleControl) -> String {
        switch command {
        case .wake: return "Wake"
        case .climateStart: return "Start Climate"
        case .climateStop: return "Stop Climate"
        case .lock: return "Lock"
        case .unlock: return "Unlock"
        case .flashLights: return "Flash Lights"
        case .honkHorn: return "Honk"
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

final class HubVehiclesView: HubSurfaceView {
    private let actions: HubVehicleCardActions
    private let content = NSStackView()

    init(actions: HubVehicleCardActions) {
        self.actions = actions
        super.init(fill: .background)

        let scrollView = NSScrollView()
        scrollView.drawsBackground = false
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.hasHorizontalScroller = false
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        addSubview(scrollView)
        NSLayoutConstraint.activate([
            scrollView.leadingAnchor.constraint(equalTo: leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: trailingAnchor),
            scrollView.topAnchor.constraint(equalTo: topAnchor),
            scrollView.bottomAnchor.constraint(equalTo: bottomAnchor)
        ])

        let document = HubSurfaceView(fill: .background)
        document.translatesAutoresizingMaskIntoConstraints = false
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = HubMetrics.sectionSpacing
        content.translatesAutoresizingMaskIntoConstraints = false
        document.addSubview(content)
        NSLayoutConstraint.activate([
            content.widthAnchor.constraint(equalToConstant: HubMetrics.contentWidth),
            content.centerXAnchor.constraint(equalTo: document.centerXAnchor),
            content.topAnchor.constraint(equalTo: document.topAnchor, constant: HubMetrics.pageInset),
            content.bottomAnchor.constraint(lessThanOrEqualTo: document.bottomAnchor,
                                            constant: -HubMetrics.pageInset)
        ])
        scrollView.documentView = document
        document.widthAnchor.constraint(equalTo: scrollView.contentView.widthAnchor).isActive = true
        document.heightAnchor.constraint(greaterThanOrEqualTo: scrollView.contentView.heightAnchor).isActive = true
        document.heightAnchor.constraint(greaterThanOrEqualTo: content.heightAnchor,
                                         constant: HubMetrics.pageInset * 2).isActive = true
    }

    func apply(snapshot: HubSnapshot, enabled: Bool) {
        content.arrangedSubviews.forEach {
            content.removeArrangedSubview($0)
            $0.removeFromSuperview()
        }

        let title = NSTextField(labelWithString: "Vehicles")
        title.font = .systemFont(ofSize: 19, weight: .bold)
        title.textColor = HubPalette.foreground
        let count = snapshot.controlVehicles.count
        let countLabel = NSTextField(labelWithString: "\(count) connected to this Hub")
        countLabel.font = .systemFont(ofSize: 13)
        countLabel.textColor = HubPalette.mutedForeground
        let heading = NSStackView(views: [title, countLabel])
        heading.orientation = .vertical
        heading.alignment = .leading
        heading.spacing = 3
        content.addArrangedSubview(heading)
        heading.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true

        if snapshot.controlVehicles.isEmpty {
            let empty = HubVehicleCardView(actions: actions)
            empty.apply(vehicle: nil, allVehicles: [], provider: snapshot.provider, enabled: false,
                        emptyTitle: "No vehicles yet",
                        emptyStatus: "Connect a Tesla account and start Hub to see your vehicles here.")
            addCard(empty)
        } else {
            for vehicle in snapshot.controlVehicles {
                let card = HubVehicleCardView(actions: actions)
                card.apply(vehicle: vehicle, allVehicles: [vehicle], provider: snapshot.provider,
                           enabled: enabled)
                addCard(card)
            }
        }
    }

    private func addCard(_ card: HubVehicleCardView) {
        card.translatesAutoresizingMaskIntoConstraints = false
        content.addArrangedSubview(card)
        card.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
        card.heightAnchor.constraint(greaterThanOrEqualToConstant: 116).isActive = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

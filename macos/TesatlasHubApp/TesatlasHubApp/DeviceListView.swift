// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

/// Paired devices in Service details (M06-T04): each device with the cars it may read
/// (`control paired-devices`) and Revoke through the existing route
/// (`control revoke-device`), after a confirmation whose default is Cancel. The
/// listing holds no bearer material. English only.
final class DeviceListView: NSView {
    typealias ConfirmationPresenter = (NSAlert, NSApplication.ModalResponse) -> NSApplication.ModalResponse

    private let backend: HubMaintenanceBackend
    private let mutationAllowed: () -> Bool
    private let confirmationPresenter: ConfirmationPresenter
    private let date: (Date) -> String

    let rowsStack = NSStackView()
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    /// Pair a phone (DEC-152) and the one-time link it shows: selectable, with Copy and
    /// its expiry, cleared on Done. The link is never logged.
    let pairButton = HubActionButton(title: HubL10n.text("hub.pairing.button", fallback: "Pair a phone…"), target: nil, action: nil)
    let pairingPanel = NSStackView()
    let pairingLinkField = NSTextField(labelWithString: "")
    let pairingExpiryLabel = NSTextField(wrappingLabelWithString: "")
    let copyLinkButton = HubActionButton(title: HubL10n.text("hub.pairing.copy", fallback: "Copy"), target: nil, action: nil)
    let pairingDoneButton = HubActionButton(title: HubL10n.text("hub.pairing.done", fallback: "Done"), target: nil, action: nil)
    private let pasteboard: NSPasteboard
    private(set) var pairing = false
    private(set) var devices: [HubPairedDevice] = []
    private var mutationsEnabled = true
    private var revoking: Set<UUID> = []

    init(backend: HubMaintenanceBackend,
         mutationAllowed: @escaping () -> Bool = { true },
         confirmationPresenter: @escaping ConfirmationPresenter = { alert, silentResponse in
             HubUIPresentation.response(to: alert, silentResponse: silentResponse)
         },
         date: @escaping (Date) -> String = HubCollectionStatus.defaultDateText,
         pasteboard: NSPasteboard = .general) {
        self.backend = backend
        self.mutationAllowed = mutationAllowed
        self.confirmationPresenter = confirmationPresenter
        self.date = date
        self.pasteboard = pasteboard
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("hub.devices")
        translatesAutoresizingMaskIntoConstraints = false
        build()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func reload(completion: (() -> Void)? = nil) {
        backend.listPairedDevices { [weak self] result in
            guard let self else { return }
            switch result {
            case let .success(devices):
                self.show(devices: devices)
            case let .failure(error):
                self.show(devices: [])
                self.showMessage(HubServiceRefusal.parse(error.localizedDescription)?.message
                                 ?? error.localizedDescription, isError: true)
            }
            completion?()
        }
    }

    func setMutationsEnabled(_ enabled: Bool) {
        mutationsEnabled = enabled
        pairButton.isEnabled = enabled && !pairing
        revokeButtons().forEach { button in
            button.isEnabled = enabled && !revoking.contains(where: { Self.revokeIdentifier($0) == button.identifier?.rawValue })
        }
    }

    static func revokeIdentifier(_ id: UUID) -> String {
        "hub.devices.revoke.\(id.uuidString.lowercased())"
    }

    /// Revoke: Cancel first and answering Return.
    static func revokeConfirmation(for device: HubPairedDevice) -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = HubL10n.format("hub.devices.revokeTitle", fallback: "Revoke %1$@?", arguments: [device.name])
        alert.informativeText = HubL10n.text("hub.devices.revokeInfo", fallback: "This device stops reading from the Hub at once. To use it again, pair it again.")
        alert.addButton(withTitle: HubL10n.text("hub.ImportSheetController.112.1706", fallback: "Cancel"))
        alert.addButton(withTitle: HubL10n.text("hub.devices.revoke", fallback: "Revoke"))
        alert.buttons[0].keyEquivalent = "\r"
        alert.buttons[1].keyEquivalent = ""
        return alert
    }

    @objc private func revokePressed(_ sender: NSButton) {
        guard mutationsEnabled, mutationAllowed(),
              let device = devices.first(where: { Self.revokeIdentifier($0.id) == sender.identifier?.rawValue }),
              !revoking.contains(device.id) else {
            HubUIPresentation.playRefusalSound()
            return
        }
        guard confirmationPresenter(Self.revokeConfirmation(for: device), .alertFirstButtonReturn)
            == .alertSecondButtonReturn else { return }
        revoking.insert(device.id)
        sender.isEnabled = false
        backend.revokeDevice(device.id) { [weak self] result in
            guard let self else { return }
            self.revoking.remove(device.id)
            switch result {
            case .success:
                self.reload { self.showMessage(HubL10n.format("hub.devices.revokedDone", fallback: "%1$@ was revoked.", arguments: [device.name]), isError: false) }
            case let .failure(error):
                sender.isEnabled = self.mutationsEnabled
                self.showMessage(HubServiceRefusal.parse(error.localizedDescription)?.message
                                 ?? error.localizedDescription, isError: true)
            }
        }
    }

    // MARK: Pair a phone

    @objc func pairPressed(_ sender: Any?) {
        guard mutationsEnabled, mutationAllowed(), !pairing else {
            HubUIPresentation.playRefusalSound()
            return
        }
        pairing = true
        pairButton.isEnabled = false
        hidePairingLink()
        showMessage(HubL10n.text("hub.pairing.preparing", fallback: "Preparing a pairing link. The first time, Hub sets up its local network identity and restarts."), isError: false)
        backend.pairPhone { [weak self] result in
            guard let self else { return }
            self.pairing = false
            self.pairButton.isEnabled = self.mutationsEnabled
            switch result {
            case let .success(invitation):
                self.messageLabel.isHidden = true
                self.showPairingLink(invitation)
            case let .failure(error):
                self.showMessage(HubServiceRefusal.parse(error.localizedDescription)?.message
                                 ?? error.localizedDescription, isError: true)
            }
        }
    }

    @objc func copyLinkPressed(_ sender: Any?) {
        let link = pairingLinkField.stringValue
        guard !pairingPanel.isHidden, link.hasPrefix(HubPairingInvitation.linkPrefix) else { return }
        pasteboard.clearContents()
        pasteboard.setString(link, forType: .string)
        pairingExpiryLabel.stringValue = pairingExpiryText + " " + HubL10n.text("hub.pairing.copied", fallback: "Copied.")
    }

    @objc func pairingDonePressed(_ sender: Any?) {
        hidePairingLink()
        reload()
    }

    private var pairingExpiryText = ""

    private func showPairingLink(_ invitation: HubPairingInvitation) {
        pairingLinkField.stringValue = invitation.link
        pairingExpiryText = HubL10n.format("hub.pairing.expiry", fallback: "Paste this link in Tesatlas on the phone. It works once and expires %1$@.", arguments: [date(invitation.expiresAt)])
        pairingExpiryLabel.stringValue = pairingExpiryText
        pairingPanel.isHidden = false
    }

    private func hidePairingLink() {
        pairingLinkField.stringValue = ""
        pairingExpiryText = ""
        pairingExpiryLabel.stringValue = ""
        pairingPanel.isHidden = true
    }

    // MARK: Showing

    private func show(devices: [HubPairedDevice]) {
        self.devices = devices
        rowsStack.arrangedSubviews.forEach {
            rowsStack.removeArrangedSubview($0)
            $0.removeFromSuperview()
        }
        messageLabel.isHidden = true
        guard !devices.isEmpty else {
            showMessage(HubL10n.text("hub.devices.none", fallback: "No paired devices."), isError: false)
            return
        }
        let names = backend.vehicleNames
        for (index, device) in devices.enumerated() {
            if index > 0 {
                let line = HubModalChrome.divider()
                rowsStack.addArrangedSubview(line)
                line.widthAnchor.constraint(equalTo: rowsStack.widthAnchor).isActive = true
            }
            let row = row(for: device, vehicleNames: names)
            rowsStack.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: rowsStack.widthAnchor).isActive = true
        }
    }

    private func row(for device: HubPairedDevice, vehicleNames: [UUID: String]) -> NSView {
        let name = NSTextField(labelWithString: device.name)
        name.font = HubTypography.emphasis
        name.textColor = device.isActive ? HubPalette.foreground : HubPalette.mutedForeground
        name.lineBreakMode = .byTruncatingTail
        let grants = NSTextField(labelWithString: HubL10n.format("hub.devices.cars", fallback: "Cars: %1$@", arguments: [device.grantText(vehicleNames: vehicleNames)]))
        grants.font = HubTypography.caption
        grants.textColor = HubPalette.mutedForeground
        grants.lineBreakMode = .byTruncatingTail
        grants.identifier = NSUserInterfaceItemIdentifier("hub.devices.grants.\(device.id.uuidString.lowercased())")
        let state = NSTextField(labelWithString: device.stateText(date: date))
        state.font = HubTypography.caption
        state.textColor = HubPalette.mutedForeground
        let copy = NSStackView(views: [name, grants, state])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2
        copy.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        var views: [NSView] = [copy, NSView()]
        if device.isActive {
            let revoke = HubActionButton(title: HubL10n.text("hub.devices.revokeEllipsis", fallback: "Revoke…"), target: self, action: #selector(revokePressed(_:)))
            revoke.identifier = NSUserInterfaceItemIdentifier(Self.revokeIdentifier(device.id))
            revoke.hubStyle = .flatDanger
            revoke.hubFont = HubTypography.action
            revoke.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
            revoke.isEnabled = mutationsEnabled && !revoking.contains(device.id)
            revoke.setAccessibilityLabel(HubL10n.format("hub.devices.revokeAccessibility", fallback: "Revoke %1$@", arguments: [device.name]))
            views.append(revoke)
        }
        let line = NSStackView(views: views)
        line.alignment = .centerY
        line.edgeInsets = NSEdgeInsets(top: 8, left: 0, bottom: 8, right: 0)
        return line
    }

    private func showMessage(_ text: String, isError: Bool) {
        messageLabel.stringValue = text
        messageLabel.textColor = isError ? HubPalette.danger : HubPalette.mutedForeground
        messageLabel.isHidden = false
    }

    private func revokeButtons() -> [NSButton] {
        func walk(_ view: NSView) -> [NSButton] {
            ((view as? NSButton).map { [$0] } ?? []) + view.subviews.flatMap(walk)
        }
        return walk(rowsStack).filter { $0.identifier?.rawValue.hasPrefix("hub.devices.revoke.") == true }
    }

    private func build() {
        let title = NSTextField(labelWithString: HubL10n.text("hub.devices.title", fallback: "Paired devices"))
        title.font = HubTypography.emphasis
        title.textColor = HubPalette.foreground
        let detail = NSTextField(wrappingLabelWithString: HubL10n.text("hub.devices.detail", fallback: "Phones and apps that read this Hub, with the cars each may read."))
        detail.font = HubTypography.caption
        detail.textColor = HubPalette.mutedForeground
        rowsStack.orientation = .vertical
        rowsStack.alignment = .leading
        rowsStack.spacing = 0
        rowsStack.identifier = NSUserInterfaceItemIdentifier("hub.devices.list")
        messageLabel.identifier = NSUserInterfaceItemIdentifier("hub.devices.message")
        messageLabel.font = HubTypography.caption
        messageLabel.isHidden = true
        pairButton.target = self
        pairButton.action = #selector(pairPressed(_:))
        pairButton.identifier = NSUserInterfaceItemIdentifier("hub.devices.pair")
        pairButton.hubStyle = .flat
        pairButton.hubFont = HubTypography.action
        pairButton.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
        let header = NSStackView(views: [title, NSView(), pairButton])
        header.alignment = .centerY
        pairingLinkField.identifier = NSUserInterfaceItemIdentifier("hub.devices.pairingLink")
        pairingLinkField.isSelectable = true
        pairingLinkField.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        pairingLinkField.lineBreakMode = .byTruncatingMiddle
        pairingLinkField.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        copyLinkButton.target = self
        copyLinkButton.action = #selector(copyLinkPressed(_:))
        copyLinkButton.identifier = NSUserInterfaceItemIdentifier("hub.devices.pairingCopy")
        copyLinkButton.hubFont = HubTypography.action
        pairingDoneButton.target = self
        pairingDoneButton.action = #selector(pairingDonePressed(_:))
        pairingDoneButton.identifier = NSUserInterfaceItemIdentifier("hub.devices.pairingDone")
        pairingDoneButton.hubStyle = .flat
        pairingDoneButton.hubFont = HubTypography.action
        let linkRow = NSStackView(views: [pairingLinkField, copyLinkButton, pairingDoneButton])
        linkRow.alignment = .centerY
        pairingExpiryLabel.identifier = NSUserInterfaceItemIdentifier("hub.devices.pairingExpiry")
        pairingExpiryLabel.font = HubTypography.caption
        pairingExpiryLabel.textColor = HubPalette.mutedForeground
        pairingPanel.orientation = .vertical
        pairingPanel.alignment = .leading
        pairingPanel.spacing = 4
        pairingPanel.identifier = NSUserInterfaceItemIdentifier("hub.devices.pairingPanel")
        pairingPanel.addArrangedSubview(linkRow)
        pairingPanel.addArrangedSubview(pairingExpiryLabel)
        pairingPanel.isHidden = true
        linkRow.widthAnchor.constraint(equalTo: pairingPanel.widthAnchor).isActive = true
        let contents = NSStackView(views: [header, detail, pairingPanel, rowsStack, messageLabel])
        contents.orientation = .vertical
        contents.alignment = .leading
        contents.spacing = 8
        contents.setCustomSpacing(4, after: header)
        contents.translatesAutoresizingMaskIntoConstraints = false
        let card = HubCardView()
        card.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(contents)
        addSubview(card)
        NSLayoutConstraint.activate([
            card.leadingAnchor.constraint(equalTo: leadingAnchor),
            card.trailingAnchor.constraint(equalTo: trailingAnchor),
            card.topAnchor.constraint(equalTo: topAnchor),
            card.bottomAnchor.constraint(equalTo: bottomAnchor),
            contents.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: HubMetrics.rowHorizontalInset),
            contents.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -HubMetrics.rowHorizontalInset),
            contents.topAnchor.constraint(equalTo: card.topAnchor, constant: 14),
            contents.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -14),
            rowsStack.widthAnchor.constraint(equalTo: contents.widthAnchor),
            header.widthAnchor.constraint(equalTo: contents.widthAnchor),
            pairingPanel.widthAnchor.constraint(equalTo: contents.widthAnchor)
        ])
        for view in [detail, messageLabel, pairingExpiryLabel] {
            view.widthAnchor.constraint(lessThanOrEqualTo: contents.widthAnchor).isActive = true
        }
    }
}

import AppKit

final class ServiceDetailsWindowController: NSWindowController {
    private let controller: HubController
    private let mutationAllowed: () -> Bool
    private let onMutationStateChanged: (Bool) -> Void
    private let onChanged: () -> Void
    private let detailsField = NSTextField(labelWithString: "")
    private let updateButton = HubActionButton(title: "Update Service…", target: nil, action: nil)
    private let uninstallButton = HubActionButton(title: "Uninstall Hub…", target: nil, action: nil)
    private var mutationsEnabled = true
    private var mutationPending = false

    init(snapshot: HubSnapshot,
         controller: HubController,
         mutationAllowed: @escaping () -> Bool = { true },
         onMutationStateChanged: @escaping (Bool) -> Void = { _ in },
         onChanged: @escaping () -> Void) {
        self.controller = controller
        self.mutationAllowed = mutationAllowed
        self.onMutationStateChanged = onMutationStateChanged
        self.onChanged = onChanged
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 520, height: 420), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Service Details"
        super.init(window: window)
        detailsField.font = .systemFont(ofSize: 13)
        detailsField.lineBreakMode = .byCharWrapping
        detailsField.translatesAutoresizingMaskIntoConstraints = false

        let legal = NSTextField(labelWithString: "License: AGPL-3.0-only\nUnofficial project. No Tesla affiliation or warranty.")
        legal.font = .systemFont(ofSize: 11)
        legal.textColor = .secondaryLabelColor
        legal.lineBreakMode = .byWordWrapping
        legal.translatesAutoresizingMaskIntoConstraints = false

        let sourceButton = HubActionButton(title: "Open Source", target: self, action: #selector(openSource))
        configureFlatButton(sourceButton, symbol: "chevron.left.forwardslash.chevron.right")
        sourceButton.translatesAutoresizingMaskIntoConstraints = false
        let licenseButton = HubActionButton(title: "Open License", target: self, action: #selector(openLicense))
        configureFlatButton(licenseButton, symbol: "doc.plaintext")
        licenseButton.translatesAutoresizingMaskIntoConstraints = false
        updateButton.target = self
        updateButton.action = #selector(updateServicePressed)
        configureFlatButton(updateButton, symbol: "arrow.down.circle", tint: .controlAccentColor)
        updateButton.translatesAutoresizingMaskIntoConstraints = false
        uninstallButton.target = self
        uninstallButton.action = #selector(uninstallPressed)
        configureFlatButton(uninstallButton, symbol: "trash", tint: .systemRed)
        uninstallButton.translatesAutoresizingMaskIntoConstraints = false
        let buttons = NSStackView(views: [sourceButton, licenseButton, updateButton, uninstallButton])
        buttons.orientation = .horizontal
        buttons.spacing = 8
        buttons.translatesAutoresizingMaskIntoConstraints = false

        let stack = NSStackView(views: [detailsField, legal, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 16
        stack.translatesAutoresizingMaskIntoConstraints = false
        let container = NSView()
        container.addSubview(stack)
        window.contentView = container
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: container.leadingAnchor, constant: 28),
            stack.trailingAnchor.constraint(equalTo: container.trailingAnchor, constant: -28),
            stack.topAnchor.constraint(equalTo: container.topAnchor, constant: 28),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: container.bottomAnchor, constant: -28),
            legal.trailingAnchor.constraint(equalTo: stack.trailingAnchor)
        ])
        window.center()
        update(snapshot: snapshot)
    }

    func update(snapshot: HubSnapshot) {
        detailsField.stringValue = [
            "Status: \(snapshot.service)",
            "Account: \(snapshot.accountDisplay)",
            "Vehicle: \(snapshot.vehicleName) · \(snapshot.vehicle)",
            "Database: \(snapshot.database)",
            "Data folder: \(snapshot.dataDirectory?.path ?? "Not available")",
            "Version: \(snapshot.version)"
        ].joined(separator: "\n\n")
    }

    func setMutationsEnabled(_ enabled: Bool) {
        mutationsEnabled = enabled
        updateMutationButtons()
    }

    private func configureFlatButton(_ button: NSButton,
                                     symbol: String,
                                     tint: NSColor = .labelColor) {
        button.isBordered = false
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: button.title)
        button.imagePosition = .imageLeading
        button.contentTintColor = .labelColor
        (button as? HubActionButton)?.hubAppearance = .flat
        button.font = .systemFont(ofSize: 13, weight: .medium)
        button.focusRingType = .default
    }

    @objc private func openSource() {
        NSWorkspace.shared.open(URL(string: "https://github.com/magrathean-uk/teslatlas-hub")!)
    }

    @objc private func openLicense() {
        guard let license = Bundle.main.url(forResource: "LICENSE", withExtension: nil) else { return }
        NSWorkspace.shared.open(license)
    }

    @objc private func updateServicePressed() {
        guard beginMutation() else { return }
        controller.installService { [weak self] result in
            self?.endMutation()
            switch result {
            case .success:
                self?.onChanged()
            case let .failure(error):
                NSAlert(error: error).runModal()
            }
        }
    }

    @objc private func uninstallPressed() {
        let choice = NSAlert()
        choice.alertStyle = .warning
        choice.messageText = "Uninstall Teslatlas Hub?"
        choice.informativeText = "The background service and logs will be removed. Your Hub database and configuration are preserved by default."
        choice.addButton(withTitle: "Uninstall, Keep Data")
        choice.addButton(withTitle: "Delete Data…")
        choice.addButton(withTitle: "Cancel")
        let response = choice.runModal()
        guard response != .alertThirdButtonReturn else { return }

        let deleteData = response == .alertSecondButtonReturn
        if deleteData {
            guard Self.deleteDataConfirmation().runModal() == .alertSecondButtonReturn else { return }
        }

        guard beginMutation() else { return }
        controller.uninstallService(deleteData: deleteData) { [weak self] result in
            self?.endMutation()
            switch result {
            case .success:
                self?.close()
                self?.onChanged()
            case let .failure(error):
                NSAlert(error: error).runModal()
            }
        }
    }

    private func beginMutation() -> Bool {
        guard mutationsEnabled, !mutationPending, mutationAllowed() else {
            NSSound.beep()
            return false
        }
        mutationPending = true
        updateMutationButtons()
        onMutationStateChanged(true)
        return true
    }

    private func endMutation() {
        guard mutationPending else { return }
        mutationPending = false
        onMutationStateChanged(false)
        updateMutationButtons()
    }

    private func updateMutationButtons() {
        let enabled = mutationsEnabled && !mutationPending
        updateButton.isEnabled = enabled
        uninstallButton.isEnabled = enabled
    }

    static func deleteDataConfirmation() -> NSAlert {
        let confirmation = NSAlert()
        confirmation.alertStyle = .critical
        confirmation.messageText = "Permanently delete Hub data?"
        confirmation.informativeText = "This removes the Hub database and configuration. This cannot be undone."
        confirmation.addButton(withTitle: "Cancel")
        confirmation.addButton(withTitle: "Delete Data and Uninstall")
        confirmation.buttons[0].keyEquivalent = "\r"
        confirmation.buttons[1].keyEquivalent = ""
        return confirmation
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

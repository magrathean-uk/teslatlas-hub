import AppKit

final class ServiceDetailsWindowController: NSWindowController {
    private let controller: HubController
    private let onChanged: () -> Void

    init(snapshot: HubSnapshot, controller: HubController, onChanged: @escaping () -> Void) {
        self.controller = controller
        self.onChanged = onChanged
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 520, height: 420), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Service Details"
        super.init(window: window)
        let text = [
            "Status: \(snapshot.service)",
            "Account: \(snapshot.account)",
            "Vehicle: \(snapshot.vehicleName) · \(snapshot.vehicle)",
            "Database: \(snapshot.database)",
            "Data folder: \(snapshot.dataDirectory?.path ?? "Not available")",
            "Version: \(snapshot.version)"
        ].joined(separator: "\n\n")
        let field = NSTextField(labelWithString: text)
        field.font = .systemFont(ofSize: 13)
        field.lineBreakMode = .byCharWrapping
        field.translatesAutoresizingMaskIntoConstraints = false

        let legal = NSTextField(labelWithString: "License: AGPL-3.0-only\nUnofficial project. No Tesla affiliation or warranty.")
        legal.font = .systemFont(ofSize: 11)
        legal.textColor = .secondaryLabelColor
        legal.lineBreakMode = .byWordWrapping
        legal.translatesAutoresizingMaskIntoConstraints = false

        let sourceButton = NSButton(title: "Open Source", target: self, action: #selector(openSource))
        sourceButton.bezelStyle = .rounded
        sourceButton.translatesAutoresizingMaskIntoConstraints = false
        let licenseButton = NSButton(title: "Open License", target: self, action: #selector(openLicense))
        licenseButton.bezelStyle = .rounded
        licenseButton.translatesAutoresizingMaskIntoConstraints = false
        let updateButton = NSButton(title: "Update Service…", target: self, action: #selector(updateServicePressed))
        updateButton.bezelStyle = .rounded
        updateButton.translatesAutoresizingMaskIntoConstraints = false
        let uninstallButton = NSButton(title: "Uninstall Hub…", target: self, action: #selector(uninstallPressed))
        uninstallButton.bezelStyle = .rounded
        uninstallButton.translatesAutoresizingMaskIntoConstraints = false
        let buttons = NSStackView(views: [sourceButton, licenseButton, updateButton, uninstallButton])
        buttons.orientation = .horizontal
        buttons.spacing = 8
        buttons.translatesAutoresizingMaskIntoConstraints = false

        let stack = NSStackView(views: [field, legal, buttons])
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
    }

    @objc private func openSource() {
        NSWorkspace.shared.open(URL(string: "https://github.com/magrathean-uk/teslatlas-hub")!)
    }

    @objc private func openLicense() {
        guard let license = Bundle.main.url(forResource: "LICENSE", withExtension: nil) else { return }
        NSWorkspace.shared.open(license)
    }

    @objc private func updateServicePressed() {
        controller.installService { [weak self] result in
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
            let confirmation = NSAlert()
            confirmation.alertStyle = .critical
            confirmation.messageText = "Permanently delete Hub data?"
            confirmation.informativeText = "This removes the Hub database and configuration. This cannot be undone."
            confirmation.addButton(withTitle: "Delete Data and Uninstall")
            confirmation.addButton(withTitle: "Cancel")
            guard confirmation.runModal() == .alertFirstButtonReturn else { return }
        }

        controller.uninstallService(deleteData: deleteData) { [weak self] result in
            switch result {
            case .success:
                self?.close()
                self?.onChanged()
            case let .failure(error):
                NSAlert(error: error).runModal()
            }
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

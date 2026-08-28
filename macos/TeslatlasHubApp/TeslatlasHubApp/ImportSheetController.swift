// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

final class ImportSheetController: NSWindowController {
    static let teslaMateHandoverDetail =
        "Hub reads TeslaMate without stopping or changing it. After import, Hub tells you when to disable Tesla access in TeslaMate yourself."

    private let controller: HubController
    private let sourceField = NSTextField(string: "postgres://localhost/teslamate")
    private let carField = NSTextField(string: "1")
    private let passwordField = NSTextField(string: "")
    private let encryptionField = NSTextField(string: "")

    init(controller: HubController) {
        self.controller = controller
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 390),
                              styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Import TeslaMate"
        super.init(window: window)
        window.contentView = contentView()
        window.center()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func contentView() -> NSView {
        let root = NSView()
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 14
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 28),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -28),
            stack.topAnchor.constraint(equalTo: root.topAnchor, constant: 24),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: root.bottomAnchor, constant: -24)
        ])
        let help = NSTextField(labelWithString: "Copy data from a TeslaMate PostgreSQL database into Teslatlas Hub.")
        help.textColor = .secondaryLabelColor
        help.lineBreakMode = .byWordWrapping
        stack.addArrangedSubview(help)
        let handover = NSTextField(wrappingLabelWithString: Self.teslaMateHandoverDetail)
        handover.textColor = .secondaryLabelColor
        handover.maximumNumberOfLines = 2
        handover.widthAnchor.constraint(equalToConstant: 504).isActive = true
        stack.addArrangedSubview(handover)
        stack.addArrangedSubview(field("PostgreSQL source", sourceField))
        stack.addArrangedSubview(field("Car ID", carField))
        let passwordRow = NSStackView(views: [passwordField, button("Choose…", #selector(choosePassword))])
        passwordRow.spacing = 8
        passwordRow.widthAnchor.constraint(equalToConstant: 504).isActive = true
        stack.addArrangedSubview(labeled("Password file", passwordRow))
        let encryptionRow = NSStackView(views: [encryptionField, button("Choose…", #selector(chooseEncryption))])
        encryptionRow.spacing = 8
        encryptionRow.widthAnchor.constraint(equalToConstant: 504).isActive = true
        stack.addArrangedSubview(labeled("TeslaMate ENCRYPTION_KEY file", encryptionRow))
        let buttons = NSStackView(views: [spacer(), button("Cancel", #selector(cancelPressed)), button("Import", #selector(importPressed))])
        buttons.spacing = 8
        stack.addArrangedSubview(buttons)
        return root
    }

    private func field(_ title: String, _ field: NSTextField) -> NSView {
        field.widthAnchor.constraint(equalToConstant: 504).isActive = true
        return labeled(title, field)
    }

    private func labeled(_ title: String, _ view: NSView) -> NSView {
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 12, weight: .medium)
        let stack = NSStackView(views: [label, view])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        return stack
    }

    private func button(_ title: String, _ action: Selector) -> NSButton {
        let button = HubActionButton(title: title, target: self, action: action)
        button.isBordered = false
        button.image = NSImage(systemSymbolName: symbol(for: title), accessibilityDescription: title)
        button.imagePosition = .imageLeading
        button.contentTintColor = .labelColor
        button.hubAppearance = .flat
        button.font = .systemFont(ofSize: 13, weight: .medium)
        button.focusRingType = .default
        return button
    }

    private func symbol(for title: String) -> String {
        switch title {
        case "Choose…": return "folder"
        case "Import": return "square.and.arrow.down"
        case "Cancel": return "xmark"
        default: return "chevron.right"
        }
    }

    private func spacer() -> NSView {
        let view = NSView()
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        return view
    }

    @objc private func choosePassword() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.beginSheetModal(for: window!) { [weak self] response in
            if response == .OK { self?.passwordField.stringValue = panel.url?.path ?? "" }
        }
    }

    @objc private func chooseEncryption() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.beginSheetModal(for: window!) { [weak self] response in
            if response == .OK { self?.encryptionField.stringValue = panel.url?.path ?? "" }
        }
    }

    @objc private func cancelPressed() { window?.sheetParent?.endSheet(window!) }

    @objc private func importPressed() {
        guard !sourceField.stringValue.isEmpty, !carField.stringValue.isEmpty, !passwordField.stringValue.isEmpty, !encryptionField.stringValue.isEmpty else {
            let alert = NSAlert()
            alert.messageText = "Complete all fields"
            alert.informativeText = "Source, car ID, password file, and ENCRYPTION_KEY file are required."
            alert.runModal()
            return
        }
        do {
            try HubController.validateMigrationSource(sourceField.stringValue)
        } catch {
            NSAlert(error: error).runModal()
            return
        }
        controller.importTeslaMate(source: sourceField.stringValue, carID: carField.stringValue, passwordFile: passwordField.stringValue, encryptionKeyFile: encryptionField.stringValue) { [weak self] result in
            switch result {
            case .success: if let window = self?.window, let parent = window.sheetParent { parent.endSheet(window) }
            case let .failure(error): NSAlert(error: error).runModal()
            }
        }
    }
}

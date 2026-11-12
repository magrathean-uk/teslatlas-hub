// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

final class ImportSheetController: NSWindowController {
    static let sourceHandoverDetail =
        HubL10n.text("hub.ImportSheetController.7.1687", fallback: "Hub reads the source database without stopping or changing the source application. After import, Hub tells you when to disable Tesla access in the source application yourself.")
    static let sourceVersionRequirement =
        HubL10n.text("hub.ImportSheetController.9.1688", fallback: "Before import: back up the source application, update it to release 4.4.0 or later, start it once, and wait for its database migrations to finish. A direct database connection cannot prove the running app version.")

    private let controller: HubController
    private let sourceField = NSTextField(string: "")
    private let carField = NSTextField(string: "1")
    private let passwordField = NSTextField(string: "")
    private let versionAcknowledgement = NSButton(
        checkboxWithTitle: HubL10n.text("hub.import.ackRelease", fallback: "I confirm that the source application is at release 4.4.0 or later and will not be updated during the import."),
        target: nil,
        action: nil
    )

    init(controller: HubController) {
        self.controller = controller
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 470),
                              styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = HubL10n.text("hub.ImportSheetController.26.1691", fallback: "Import History")
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
        let help = NSTextField(wrappingLabelWithString: HubL10n.text("hub.import.intro", fallback: "Import history from the PostgreSQL database of the vehicle-data software you used before (the source database). Hub checks the database first and tells you whether this release can import it. Importing only reads the source database; it never changes it."))
        help.textColor = .secondaryLabelColor
        help.lineBreakMode = .byWordWrapping
        stack.addArrangedSubview(help)
        let handover = NSTextField(wrappingLabelWithString: Self.sourceHandoverDetail)
        handover.textColor = .secondaryLabelColor
        handover.maximumNumberOfLines = 2
        handover.widthAnchor.constraint(equalToConstant: 504).isActive = true
        stack.addArrangedSubview(handover)
        let versionRequirement = NSTextField(wrappingLabelWithString: Self.sourceVersionRequirement)
        versionRequirement.font = .systemFont(ofSize: 13, weight: .semibold)
        versionRequirement.textColor = .systemOrange
        versionRequirement.maximumNumberOfLines = 3
        versionRequirement.widthAnchor.constraint(equalToConstant: 504).isActive = true
        stack.addArrangedSubview(versionRequirement)
        let schemaExplanation = NSTextField(wrappingLabelWithString: HubL10n.text("hub.import.ackExplain", fallback: "A schema check cannot show which release of the source application wrote this database: more than one release shares this schema."))
        schemaExplanation.textColor = .secondaryLabelColor
        stack.addArrangedSubview(schemaExplanation)
        sourceField.placeholderString = "postgresql://reader@127.0.0.1/database"
        stack.addArrangedSubview(field(HubL10n.text("hub.ImportSheetController.64.1694", fallback: "PostgreSQL source"), sourceField))
        stack.addArrangedSubview(field(HubL10n.text("hub.ImportSheetController.65.1695", fallback: "Car ID"), carField))
        let passwordRow = NSStackView(views: [passwordField, button(HubL10n.text("hub.ImportSheetController.110.1702", fallback: "Choose…"), #selector(choosePassword))])
        passwordRow.spacing = 8
        passwordRow.widthAnchor.constraint(equalToConstant: 504).isActive = true
        stack.addArrangedSubview(labeled(HubL10n.text("hub.ImportSheetController.69.1697", fallback: "Password file"), passwordRow))
        stack.addArrangedSubview(versionAcknowledgement)
        let buttons = NSStackView(views: [spacer(),
            button(HubL10n.text("hub.ImportSheetController.112.1706", fallback: "Cancel"), #selector(cancelPressed)),
            button(HubL10n.text("hub.ImportSheetController.111.1704", fallback: "Import"), #selector(importPressed))])
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
        button.hubStyle = .flat
        button.font = .systemFont(ofSize: 13, weight: .medium)
        button.focusRingType = .default
        return button
    }

    private func symbol(for title: String) -> String {
        switch title {
        case HubL10n.text("hub.ImportSheetController.110.1702", fallback: "Choose…"): return "folder"
        case HubL10n.text("hub.ImportSheetController.111.1704", fallback: "Import"): return "square.and.arrow.down"
        case HubL10n.text("hub.ImportSheetController.112.1706", fallback: "Cancel"): return "xmark"
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

    @objc private func cancelPressed() { window?.sheetParent?.endSheet(window!) }

    @objc private func importPressed() {
        guard !sourceField.stringValue.isEmpty, !carField.stringValue.isEmpty, !passwordField.stringValue.isEmpty else {
            let alert = NSAlert()
            alert.messageText = HubL10n.text("hub.ImportSheetController.148.1709", fallback: "Complete all fields")
            alert.informativeText = HubL10n.text("hub.ImportSheetController.149.1710", fallback: "Source, car ID and password file are required.")
            HubUIPresentation.presentInformation(alert)
            return
        }
        let versionAccepted = versionAcknowledgement.state == .on
        guard versionAccepted else {
            let alert = NSAlert()
            alert.messageText = HubL10n.text("hub.ImportSheetController.156.1711", fallback: "Confirm the source application version")
            alert.informativeText = Self.sourceVersionRequirement
            HubUIPresentation.presentInformation(alert)
            return
        }
        do {
            try HubController.validateMigrationSource(sourceField.stringValue)
        } catch {
            HubUIPresentation.presentError(error)
            return
        }
        controller.importSource(source: sourceField.stringValue,
                                   carID: carField.stringValue,
                                   passwordFile: passwordField.stringValue,
                                   acceptSchemaEvidence: versionAccepted) { [weak self] result in
            switch result {
            case .success: if let window = self?.window, let parent = window.sheetParent { parent.endSheet(window) }
            case let .failure(error): HubUIPresentation.presentError(error)
            }
        }
    }
}

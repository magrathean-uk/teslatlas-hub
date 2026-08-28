// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

final class DiagnosticsWindowController: NSWindowController {
    private let controller: HubController
    private let textView = NSTextView()
    private let statusIcon = NSImageView()
    private let statusTitle = NSTextField(labelWithString: "Diagnostics")
    private let statusDetail = NSTextField(wrappingLabelWithString: "")
    private let runButton = HubActionButton(title: "Run Diagnostics", target: nil, action: nil)
    private let copyButton = HubActionButton(title: "Copy Report", target: nil, action: nil)
    private let saveButton = HubActionButton(title: "Save Report…", target: nil, action: nil)
    private var latestReport: String?

    init(controller: HubController) {
        self.controller = controller
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 780, height: 560),
                              styleMask: [.titled, .closable, .resizable],
                              backing: .buffered,
                              defer: false)
        window.title = "Hub Diagnostics"
        super.init(window: window)
        window.contentView = contentView()
        window.center()
        showInitialSummary()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func contentView() -> NSView {
        let root = NSView()

        statusIcon.image = NSImage(systemSymbolName: "stethoscope", accessibilityDescription: "Diagnostics ready")
        statusIcon.contentTintColor = .secondaryLabelColor
        statusIcon.imageScaling = .scaleProportionallyDown
        statusIcon.widthAnchor.constraint(equalToConstant: 28).isActive = true
        statusIcon.heightAnchor.constraint(equalToConstant: 28).isActive = true
        statusTitle.font = .systemFont(ofSize: 17, weight: .semibold)
        statusDetail.font = .systemFont(ofSize: 12)
        statusDetail.textColor = .secondaryLabelColor
        statusDetail.maximumNumberOfLines = 2
        let statusText = NSStackView(views: [statusTitle, statusDetail])
        statusText.orientation = .vertical
        statusText.alignment = .leading
        statusText.spacing = 2
        let heading = NSStackView(views: [statusIcon, statusText])
        heading.spacing = 12
        heading.alignment = .centerY

        textView.isEditable = false
        textView.isSelectable = true
        textView.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        textView.textContainerInset = NSSize(width: 10, height: 10)
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.documentView = textView

        configureFlatButton(runButton, symbol: "play.fill", tint: .controlAccentColor,
                            action: #selector(runPressed))
        configureFlatButton(copyButton, symbol: "doc.on.doc", action: #selector(copyPressed))
        configureFlatButton(saveButton, symbol: "square.and.arrow.down", action: #selector(savePressed))
        let actions = NSStackView(views: [runButton, copyButton, saveButton])
        actions.spacing = 14
        actions.alignment = .centerY

        let privacy = NSTextField(labelWithString:
            "Displayed, copied, and saved reports redact credentials and private identifiers. Review before sharing.")
        privacy.font = .systemFont(ofSize: 11)
        privacy.textColor = .secondaryLabelColor

        let line = separator()
        let stack = NSStackView(views: [heading, line, scroll, actions, privacy])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)
        for view in [heading, line, scroll, privacy] {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -20),
            stack.topAnchor.constraint(equalTo: root.topAnchor, constant: 20),
            stack.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -20),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 360)
        ])
        return root
    }

    private func showInitialSummary() {
        statusDetail.isHidden = true
        let summary = controller.diagnostics()
        let text = summary.isEmpty
            ? "No diagnostic report has been run."
            : "Current Hub summary\n\n" + summary.joined(separator: "\n")
        textView.string = Self.shareableReport(text)
        copyButton.isEnabled = false
        saveButton.isEnabled = false
    }

    @objc private func runPressed() {
        statusDetail.isHidden = false
        runButton.isEnabled = false
        copyButton.isEnabled = false
        saveButton.isEnabled = false
        statusIcon.image = NSImage(systemSymbolName: "hourglass", accessibilityDescription: "Diagnostics running")
        statusIcon.contentTintColor = .controlAccentColor
        statusTitle.stringValue = "Running checks"
        statusDetail.stringValue = "Collection pauses briefly while checks run, then resumes automatically."
        textView.string = "Running diagnostics…"
        controller.runFullDiagnostics { [weak self] text in
            guard let self else { return }
            DispatchQueue.main.async {
                let safeText = Self.shareableReport(text)
                self.latestReport = safeText
                self.textView.string = safeText
                let hasFailure = text.contains(" (failed) ==")
                self.statusIcon.image = NSImage(
                    systemSymbolName: hasFailure ? "exclamationmark.triangle" : "checkmark",
                    accessibilityDescription: hasFailure ? "Diagnostics found issues" : "Diagnostics finished"
                )
                self.statusIcon.contentTintColor = hasFailure ? .systemOrange : .systemGreen
                self.statusTitle.stringValue = hasFailure ? "Checks finished with issues" : "Checks finished"
                self.statusDetail.stringValue = hasFailure
                    ? "Review the failed section below."
                    : "Database, credentials, collector, and logs were checked."
                self.runButton.isEnabled = true
                self.copyButton.isEnabled = true
                self.saveButton.isEnabled = true
            }
        }
    }

    @objc private func copyPressed() {
        guard let latestReport else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(Self.shareableReport(latestReport), forType: .string)
        statusDetail.stringValue = "Redacted report copied."
    }

    @objc private func savePressed() {
        guard let latestReport, let window else { return }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "teslatlas-hub-diagnostics.txt"
        panel.canCreateDirectories = true
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let destination = panel.url else { return }
            do {
                try HubAppLog.writePrivateReport(Self.shareableReport(latestReport),
                                                 to: destination)
                self?.statusDetail.stringValue = "Redacted report saved."
            } catch {
                NSAlert(error: error).runModal()
            }
        }
    }

    private static func shareableReport(_ report: String) -> String {
        HubShareRedactor.redact(report)
    }

    private func configureFlatButton(_ button: NSButton,
                                     symbol: String,
                                     tint: NSColor = .labelColor,
                                     action: Selector) {
        button.target = self
        button.action = action
        button.isBordered = false
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: button.title)
        button.imagePosition = .imageLeading
        button.contentTintColor = .labelColor
        (button as? HubActionButton)?.hubAppearance = .flat
        button.font = .systemFont(ofSize: 13, weight: .medium)
        button.focusRingType = .default
    }

    private func separator() -> NSBox {
        let line = NSBox()
        line.boxType = .separator
        line.heightAnchor.constraint(equalToConstant: 1).isActive = true
        return line
    }
}

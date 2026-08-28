import AppKit

final class LogsWindowController: NSWindowController {
    private let controller: HubController
    private let textView = NSTextView()
    private let statusLabel = NSTextField(labelWithString: "Loading logs…")
    private let refreshButton = HubActionButton(title: "Refresh", target: nil, action: nil)
    private let diagnosticsButton = HubActionButton(title: "Run Diagnostics", target: nil, action: nil)
    private let copyButton = HubActionButton(title: "Copy", target: nil, action: nil)
    private let saveButton = HubActionButton(title: "Save…", target: nil, action: nil)
    private var latestText = ""
    private var operationInProgress = false

    init(controller: HubController) {
        self.controller = controller
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 760, height: 470),
                              styleMask: [.titled, .closable, .resizable],
                              backing: .buffered,
                              defer: false)
        window.title = "Teslatlas Hub Logs"
        super.init(window: window)
        window.contentView = contentView()
        window.center()
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func contentView() -> NSView {
        let root = NSView()
        let title = NSTextField(labelWithString: "Recent Hub logs")
        title.font = .systemFont(ofSize: 17, weight: .semibold)
        statusLabel.font = .systemFont(ofSize: 12)
        statusLabel.textColor = .secondaryLabelColor
        let heading = NSStackView(views: [title, spacer(), statusLabel])
        heading.alignment = .centerY

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.documentView = textView
        textView.isEditable = false
        textView.isSelectable = true
        textView.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        textView.textContainerInset = NSSize(width: 10, height: 10)

        configureFlatButton(refreshButton, symbol: "arrow.clockwise", tint: .controlAccentColor,
                            action: #selector(refreshPressed))
        configureFlatButton(diagnosticsButton, symbol: "waveform.path.ecg",
                            tint: .controlAccentColor, action: #selector(diagnosticsPressed))
        configureFlatButton(copyButton, symbol: "doc.on.doc", action: #selector(copyPressed))
        configureFlatButton(saveButton, symbol: "square.and.arrow.down", action: #selector(savePressed))
        let actions = NSStackView(views: [refreshButton, diagnosticsButton, copyButton, saveButton])
        actions.spacing = 14
        actions.alignment = .centerY

        let note = NSTextField(labelWithString:
            "Displayed, copied, and saved logs redact credentials and private identifiers. Review before sharing.")
        note.font = .systemFont(ofSize: 11)
        note.textColor = .secondaryLabelColor

        let line = separator()
        let stack = NSStackView(views: [heading, line, scroll, actions, note])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)
        for view in [heading, line, scroll, note] {
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -20),
            stack.topAnchor.constraint(equalTo: root.topAnchor, constant: 20),
            stack.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -20),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 300)
        ])
        return root
    }

    func refresh() {
        guard !operationInProgress else { return }
        operationInProgress = true
        let started = Date()
        HubAppLog.shared.record("refresh.requested", category: "logs")
        refreshButton.isEnabled = false
        diagnosticsButton.isEnabled = false
        copyButton.isEnabled = false
        saveButton.isEnabled = false
        statusLabel.stringValue = "Loading logs…"
        controller.logs { [weak self] text in
            guard let self else { return }
            let appText = HubAppLog.shared.recentText()
            let combined = Self.shareableText([
                "== app and import diagnostics ==\n\(appText)",
                "== Hub service logs ==\n\(text)"
            ].joined(separator: "\n"))
            self.latestText = combined
            self.textView.string = combined
            let appAvailable = appText != HubAppLog.unavailableText
            self.statusLabel.stringValue = appAvailable
                ? "Updated just now"
                : "App diagnostics unavailable"
            self.refreshButton.isEnabled = true
            self.diagnosticsButton.isEnabled = true
            self.copyButton.isEnabled = !combined.isEmpty
            self.saveButton.isEnabled = !combined.isEmpty
            self.operationInProgress = false
            HubAppLog.shared.record("refresh.completed", category: "logs", fields: [
                "app_bytes": String(appText.utf8.count),
                "app_available": appAvailable ? "true" : "false",
                "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000)),
                "service_bytes": String(text.utf8.count)
            ])
        }
    }

    @objc private func refreshPressed() { refresh() }

    @objc private func diagnosticsPressed() {
        guard !operationInProgress else { return }
        operationInProgress = true
        HubAppLog.shared.record("full_diagnostics.requested", category: "logs")
        refreshButton.isEnabled = false
        diagnosticsButton.isEnabled = false
        copyButton.isEnabled = false
        saveButton.isEnabled = false
        statusLabel.stringValue = "Running diagnostics…"
        textView.string = "Running database, credential, connection, and service checks…"
        controller.runFullDiagnostics { [weak self] report in
            guard let self else { return }
            let combined = Self.shareableText([
                "== app and import diagnostics ==\n\(HubAppLog.shared.recentText())",
                "== full Hub diagnostics ==\n\(report)"
            ].joined(separator: "\n"))
            self.latestText = combined
            self.textView.string = combined
            self.statusLabel.stringValue = Self.diagnosticsStatus(for: report)
            self.refreshButton.isEnabled = true
            self.diagnosticsButton.isEnabled = true
            self.copyButton.isEnabled = true
            self.saveButton.isEnabled = true
            self.operationInProgress = false
            HubAppLog.shared.record("full_diagnostics.completed", category: "logs")
        }
    }

    @objc private func copyPressed() {
        guard !latestText.isEmpty else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(Self.shareableText(latestText), forType: .string)
        statusLabel.stringValue = "Redacted logs copied"
        HubAppLog.shared.record("copy.completed", category: "logs", fields: [
            "bytes": String(latestText.utf8.count)
        ])
    }

    @objc private func savePressed() {
        guard !latestText.isEmpty, let window else { return }
        let report = Self.shareableText(latestText)
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "teslatlas-hub-logs.txt"
        panel.canCreateDirectories = true
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let destination = panel.url else { return }
            do {
                try HubAppLog.writePrivateReport(report, to: destination)
                self?.statusLabel.stringValue = "Redacted logs saved"
                HubAppLog.shared.record("save.completed", category: "logs", fields: [
                    "bytes": String(report.utf8.count)
                ])
            } catch {
                HubAppLog.shared.record("save.failed", category: "logs", level: "ERROR", fields: [
                    "error_code": HubAppLog.errorCode(error)
                ])
                NSAlert(error: error).runModal()
            }
        }
    }

    private static func shareableText(_ text: String) -> String {
        HubShareRedactor.redact(text)
    }

    static func diagnosticsStatus(for report: String) -> String {
        report.contains(" (failed) ==") ? "Diagnostics found issues" : "Diagnostics complete"
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

    private func spacer() -> NSView {
        let view = NSView()
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        return view
    }
}

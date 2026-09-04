// SPDX-License-Identifier: AGPL-3.0-only

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
        super.init(window: HubSheetStyle.makeWindow(contentSize: HubMetrics.logsSheetSize))
        window?.contentView = contentView()
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func contentView() -> NSView {
        let root = HubModalRootView()
        configureButton(copyButton, symbol: "doc.on.doc", style: .neutral,
                        action: #selector(copyPressed))
        configureButton(saveButton, symbol: "arrow.down.to.line", style: .neutral,
                        action: #selector(savePressed))
        let closeButton = HubModalChrome.closeButton(target: self, action: #selector(closePressed))
        let header = HubModalChrome.header(title: "Logs", trailing: [copyButton, saveButton, closeButton],
                                           identifier: "hub.logs.header")

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.drawsBackground = true
        scroll.backgroundColor = HubPalette.elevated
        scroll.documentView = textView
        textView.isEditable = false
        textView.isSelectable = true
        textView.drawsBackground = true
        textView.backgroundColor = HubPalette.elevated
        textView.textColor = HubPalette.foreground.withAlphaComponent(0.90)
        textView.font = .monospacedSystemFont(ofSize: 10.5, weight: .regular)
        textView.textContainerInset = NSSize(width: 14, height: 12)
        textView.isHorizontallyResizable = false
        textView.textContainer?.widthTracksTextView = true

        configureButton(refreshButton, symbol: "arrow.clockwise", style: .flatAccent,
                        action: #selector(refreshPressed))
        configureButton(diagnosticsButton, symbol: "waveform.path.ecg", style: .flatAccent,
                        action: #selector(diagnosticsPressed))
        statusLabel.font = .systemFont(ofSize: 11)
        statusLabel.textColor = HubPalette.mutedForeground
        let privacy = NSTextField(wrappingLabelWithString:
            "Displayed, copied, and saved logs redact credentials and private identifiers. Review before sharing.")
        privacy.font = .systemFont(ofSize: 10.5)
        privacy.textColor = HubPalette.mutedForeground
        let supportControls = NSStackView(views: [refreshButton, diagnosticsButton, statusLabel, privacy])
        supportControls.isHidden = true
        root.addSubview(supportControls)

        root.addSubview(header)
        root.addSubview(scroll)
        header.translatesAutoresizingMaskIntoConstraints = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            header.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            header.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            header.topAnchor.constraint(equalTo: root.topAnchor),
            scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: header.bottomAnchor),
            scroll.bottomAnchor.constraint(equalTo: root.bottomAnchor)
        ])
        return root
    }

    func refresh() {
        guard !operationInProgress else { return }
        operationInProgress = true
        let started = Date()
        HubAppLog.shared.record("refresh.requested", category: "logs")
        setActionsEnabled(false)
        statusLabel.stringValue = "Loading logs…"

        if controller.previewMode {
            finishRefresh(serviceText: Self.previewLogText, started: started)
            return
        }
        controller.logs { [weak self] text in
            self?.finishRefresh(serviceText: text, started: started)
        }
    }

    private func finishRefresh(serviceText: String, started: Date) {
        let appText = controller.previewMode ? "" : HubAppLog.shared.recentText()
        let combined = controller.previewMode
            ? Self.shareableText(serviceText)
            : Self.shareableText([
                "== app and import diagnostics ==\n\(appText)",
                "== Hub service logs ==\n\(serviceText)"
            ].joined(separator: "\n"))
        latestText = combined
        textView.string = Self.numberedPresentation(combined)
        statusLabel.stringValue = "Updated just now"
        setActionsEnabled(!combined.isEmpty)
        operationInProgress = false
        HubAppLog.shared.record("refresh.completed", category: "logs", fields: [
            "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000)),
            "service_bytes": String(serviceText.utf8.count)
        ])
    }

    @objc private func refreshPressed() { refresh() }

    @objc private func diagnosticsPressed() {
        guard !operationInProgress else { return }
        operationInProgress = true
        setActionsEnabled(false)
        statusLabel.stringValue = "Running diagnostics…"
        textView.string = "Running database, credential, connection, and service checks…"
        controller.runFullDiagnostics { [weak self] report in
            guard let self else { return }
            let combined = Self.shareableText([
                "== app and import diagnostics ==\n\(HubAppLog.shared.recentText())",
                "== full Hub diagnostics ==\n\(report)"
            ].joined(separator: "\n"))
            self.latestText = combined
            self.textView.string = Self.numberedPresentation(combined)
            self.statusLabel.stringValue = Self.diagnosticsStatus(for: report)
            self.setActionsEnabled(true)
            self.operationInProgress = false
        }
    }

    @objc private func copyPressed() {
        guard !latestText.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(Self.shareableText(latestText), forType: .string)
        statusLabel.stringValue = "Redacted logs copied"
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
            } catch {
                HubUIPresentation.presentError(error)
            }
        }
    }

    private func setActionsEnabled(_ enabled: Bool) {
        refreshButton.isEnabled = !operationInProgress || enabled
        diagnosticsButton.isEnabled = !operationInProgress || enabled
        copyButton.isEnabled = enabled
        saveButton.isEnabled = enabled
    }

    private static func shareableText(_ text: String) -> String {
        HubShareRedactor.redact(text)
    }

    static func numberedPresentation(_ text: String) -> String {
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        let visible = lines.last?.isEmpty == true ? Array(lines.dropLast()) : Array(lines)
        let width = max(2, String(visible.count).count)
        return visible.enumerated().map { index, line in
            String(format: "%0*d  %@", width, index + 1, String(line))
        }.joined(separator: "\n")
    }

    static func diagnosticsStatus(for report: String) -> String {
        report.contains(" (failed) ==") ? "Diagnostics found issues" : "Diagnostics complete"
    }

    @objc private func closePressed() { window?.close() }

    private func configureButton(_ button: HubActionButton,
                                 symbol: String,
                                 style: HubButtonStyle,
                                 action: Selector) {
        button.target = self
        button.action = action
        button.hubStyle = style
        button.hubFont = .systemFont(ofSize: 12, weight: .medium)
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: button.title)
        button.imagePosition = .imageLeading
        button.heightAnchor.constraint(equalToConstant: 28).isActive = true
    }

    private static let previewLogText = """
    2026-09-04 09:41:22.104  hub    INFO   service started (pid 4821)
    2026-09-04 09:41:22.310  fleet  INFO   region=eu client=teslatlas-hub
    2026-09-04 09:41:23.001  fleet  INFO   token valid, expires_in=3600s
    2026-09-04 09:41:24.556  stream INFO   connected to fleet telemetry
    2026-09-04 09:42:01.882  db     INFO   wrote 412 rows (drive, charge, position)
    2026-09-04 09:47:11.020  fleet  WARN   backoff: transient 429, retrying in 5s
    2026-09-04 09:47:16.204  fleet  INFO   stream resumed
    2026-09-04 10:32:09.771  db     INFO   checkpoint complete, size=1.24 GB
    2026-09-04 10:38:44.019  hub    INFO   token refreshed (fleet)
    """
}

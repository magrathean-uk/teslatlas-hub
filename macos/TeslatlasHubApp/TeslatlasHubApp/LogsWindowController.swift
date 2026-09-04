// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

final class LogsWindowController: NSWindowController {
    typealias SavePanelPresenter = (NSSavePanel, NSWindow,
                                    @escaping (NSApplication.ModalResponse) -> Void) -> Void

    private let controller: HubController
    private let appLog: HubAppLogging
    private let savePanelPresenter: SavePanelPresenter
    private let textView = HubReportTextView()
    private let scroll = NSScrollView()
    private var utilityToolbar: HubUtilityToolbar?
    var scrollViewForTesting: NSScrollView { scroll }
    var textViewForTesting: NSTextView { textView }
    private let statusLabel = NSTextField(labelWithString: "Loading logs…")
    let secondaryActionsMenu = NSMenu(title: "Log actions")
    private let moreButton = HubActionButton(title: "More", target: nil, action: nil)
    private let refreshItem = NSMenuItem(title: "Refresh", action: #selector(refreshPressed), keyEquivalent: "")
    private let diagnosticsItem = NSMenuItem(title: "Run Diagnostics", action: #selector(diagnosticsPressed), keyEquivalent: "")
    private let copyButton = HubActionButton(title: "Copy", target: nil, action: nil)
    private let saveButton = HubActionButton(title: "Save…", target: nil, action: nil)
    private var latestText = ""
    private var operationInProgress = false

    init(controller: HubController,
         appLog: HubAppLogging = HubAppLog.shared,
         savePanelPresenter: @escaping SavePanelPresenter = LogsWindowController.presentSavePanel) {
        self.controller = controller
        self.appLog = appLog
        self.savePanelPresenter = savePanelPresenter
        super.init(window: HubUtilityWindowStyle.makeWindow(title: "Logs", size: HubMetrics.logsSheetSize,
                                                           minimum: NSSize(width: 554, height: 300)))
        window?.contentView = contentView()
        utilityToolbar = HubUtilityToolbar(identifier: "hub.logs.toolbar", buttons: [copyButton, saveButton, moreButton])
        window?.toolbar = utilityToolbar?.toolbar
        window?.toolbarStyle = .expanded
        if controller.previewMode {
            renderLogs(Self.previewLogText, status: "Preview fixture")
        } else {
            refresh()
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func contentView() -> NSView {
        let root = HubSurfaceView(fill: .elevated)
        configureButton(copyButton, symbol: "doc.on.doc", style: .neutral,
                        action: #selector(copyPressed))
        configureButton(saveButton, symbol: "arrow.down.to.line", style: .neutral,
                        action: #selector(savePressed))
        scroll.identifier = NSUserInterfaceItemIdentifier("hub.logs.scroll")
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
        textView.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        textView.textContainerInset = NSSize(width: 14, height: 12)
        textView.minSize = .zero
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = true
        textView.textContainer?.heightTracksTextView = false
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineSpacing = 3
        textView.defaultParagraphStyle = paragraph

        configureButton(moreButton, symbol: "ellipsis.circle", style: .flat,
                        action: #selector(morePressed))
        moreButton.imagePosition = .imageOnly
        moreButton.toolTip = "More log actions"
        secondaryActionsMenu.autoenablesItems = false
        for item in [refreshItem, diagnosticsItem] {
            item.target = self
            secondaryActionsMenu.addItem(item)
        }
        statusLabel.font = .systemFont(ofSize: 11)
        statusLabel.textColor = HubPalette.mutedForeground
        let privacy = NSTextField(wrappingLabelWithString:
            "Displayed, copied, and saved logs redact credentials and private identifiers. Review before sharing.")
        privacy.font = .systemFont(ofSize: 10.5)
        privacy.textColor = HubPalette.mutedForeground
        let actionRow = NSStackView(views: [NSView(), statusLabel])
        actionRow.spacing = 8
        let supportControls = NSStackView(views: [actionRow, privacy])
        supportControls.orientation = .vertical
        supportControls.alignment = .leading
        supportControls.spacing = 4
        supportControls.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(supportControls)
        root.addSubview(scroll)
        scroll.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: root.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: supportControls.topAnchor, constant: -6),
            supportControls.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 14),
            supportControls.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -14),
            supportControls.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -10),
            actionRow.widthAnchor.constraint(equalTo: supportControls.widthAnchor),
            privacy.widthAnchor.constraint(equalTo: supportControls.widthAnchor)
        ])
        return root
    }

    func refresh() {
        guard !operationInProgress else { return }
        if controller.previewMode {
            renderLogs(Self.previewLogText, status: "Preview fixture")
            return
        }
        operationInProgress = true
        let started = Date()
        appLog.record("refresh.requested", category: "logs", level: "INFO", fields: [:])
        setActionsEnabled(false)
        statusLabel.stringValue = "Loading logs…"

        controller.logs { [weak self] text in
            self?.finishRefresh(serviceText: text, started: started)
        }
    }

    private func finishRefresh(serviceText: String, started: Date) {
        let appText = appLog.recentText(maximumBytes: 256 * 1024)
        let combined = Self.shareableText([
            "== app and import diagnostics ==\n\(appText)",
            "== Hub service logs ==\n\(serviceText)"
        ].joined(separator: "\n"))
        renderLogs(combined, status: "Updated just now")
        appLog.record("refresh.completed", category: "logs", level: "INFO", fields: [
            "duration_ms": String(Int(Date().timeIntervalSince(started) * 1000)),
            "service_bytes": String(serviceText.utf8.count)
        ])
    }

    func renderLogs(_ redactedText: String, status: String = "Updated just now") {
        let combined = Self.shareableText(redactedText)
        latestText = combined
        textView.string = Self.numberedPresentation(combined)
        window?.contentView?.layoutSubtreeIfNeeded()
        textView.fitDocument()
        textView.scrollToBeginningOfDocument(nil)
        statusLabel.stringValue = status
        setActionsEnabled(!combined.isEmpty)
        operationInProgress = false
    }

    @objc private func refreshPressed() { refresh() }

    @objc private func morePressed() {
        secondaryActionsMenu.popUp(positioning: nil,
                                   at: NSPoint(x: 0, y: moreButton.bounds.maxY + 4), in: moreButton)
    }

    @objc private func diagnosticsPressed() {
        guard !controller.previewMode else { return }
        guard !operationInProgress else { return }
        operationInProgress = true
        setActionsEnabled(false)
        statusLabel.stringValue = "Running diagnostics…"
        textView.string = "Running database, credential, connection, and service checks…"
        controller.runFullDiagnostics { [weak self] report in
            guard let self else { return }
            let combined = Self.shareableText([
                "== app and import diagnostics ==\n\(self.appLog.recentText(maximumBytes: 256 * 1024))",
                "== full Hub diagnostics ==\n\(report)"
            ].joined(separator: "\n"))
            self.latestText = combined
            self.textView.string = Self.numberedPresentation(combined)
            self.textView.fitDocument()
            self.statusLabel.stringValue = Self.diagnosticsStatus(for: report)
            self.setActionsEnabled(true)
            self.operationInProgress = false
        }
    }

    @objc private func copyPressed() {
        guard !controller.previewMode else { return }
        guard !latestText.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(Self.shareableText(latestText), forType: .string)
        statusLabel.stringValue = "Redacted logs copied"
    }

    @objc private func savePressed() {
        guard !controller.previewMode else { return }
        guard !latestText.isEmpty, let window else { return }
        let report = Self.shareableText(latestText)
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "teslatlas-hub-logs.txt"
        panel.canCreateDirectories = true
        savePanelPresenter(panel, window) { [weak self] response in
            guard response == .OK, let destination = panel.url else { return }
            do {
                try HubAppLog.writePrivateReport(report, to: destination)
                self?.statusLabel.stringValue = "Redacted logs saved"
            } catch {
                HubUIPresentation.presentError(error)
            }
        }
    }

    private static func presentSavePanel(_ panel: NSSavePanel,
                                         for window: NSWindow,
                                         completion: @escaping (NSApplication.ModalResponse) -> Void) {
        panel.beginSheetModal(for: window, completionHandler: completion)
    }

    private func setActionsEnabled(_ enabled: Bool) {
        refreshItem.isEnabled = !operationInProgress || enabled
        diagnosticsItem.isEnabled = !operationInProgress || enabled
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

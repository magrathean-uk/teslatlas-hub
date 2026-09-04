// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

final class DiagnosticsWindowController: NSWindowController, NSWindowDelegate {
    private let controller: HubController
    private let onDismiss: () -> Void
    private let statusDetail = NSTextField(labelWithString: "")
    private let rowsStack = NSStackView()
    private let rowsContainer = HubFlippedSurfaceView(fill: .card)
    private let rowsCard = HubCardView()
    private let rawTextView = HubReportTextView()
    private var utilityToolbar: HubUtilityToolbar?
    private let rawScroll = NSScrollView()
    private let rawDisclosure = HubActionButton(title: "Show raw redacted report", target: nil, action: nil)
    private let runButton = HubActionButton(title: "Run Again", target: nil, action: nil)
    private let copyButton = HubActionButton(title: "Copy Report", target: nil, action: nil)
    private let saveButton = HubActionButton(title: "Save Report…", target: nil, action: nil)
    private var latestReport: String?

    init(controller: HubController, onDismiss: @escaping () -> Void = {}) {
        self.controller = controller
        self.onDismiss = onDismiss
        super.init(window: HubUtilityWindowStyle.makeWindow(title: "Diagnostics", size: HubMetrics.diagnosticsSheetSize,
                                                           minimum: NSSize(width: 485, height: 360)))
        window?.contentView = contentView()
        window?.delegate = self
        utilityToolbar = HubUtilityToolbar(identifier: "hub.diagnostics.toolbar", buttons: [runButton])
        window?.toolbar = utilityToolbar?.toolbar
        window?.toolbarStyle = .expanded
        showInitialSummary()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func contentView() -> NSView {
        let root = HubSurfaceView(fill: .background)
        configureButton(runButton, symbol: "arrow.clockwise", style: .neutral,
                        action: #selector(runPressed))

        statusDetail.font = .systemFont(ofSize: 12.5)
        statusDetail.textColor = HubPalette.mutedForeground

        rowsStack.orientation = .vertical
        rowsStack.alignment = .leading
        rowsStack.spacing = 0
        rowsStack.translatesAutoresizingMaskIntoConstraints = false
        rowsCard.translatesAutoresizingMaskIntoConstraints = false
        rowsCard.addSubview(rowsStack)
        NSLayoutConstraint.activate([
            rowsStack.leadingAnchor.constraint(equalTo: rowsCard.leadingAnchor),
            rowsStack.trailingAnchor.constraint(equalTo: rowsCard.trailingAnchor),
            rowsStack.topAnchor.constraint(equalTo: rowsCard.topAnchor),
            rowsStack.bottomAnchor.constraint(equalTo: rowsCard.bottomAnchor)
        ])

        rowsContainer.identifier = NSUserInterfaceItemIdentifier("hub.diagnostics.rows-document")
        rowsContainer.translatesAutoresizingMaskIntoConstraints = false
        rowsContainer.addSubview(rowsCard)
        NSLayoutConstraint.activate([
            rowsCard.leadingAnchor.constraint(equalTo: rowsContainer.leadingAnchor),
            rowsCard.trailingAnchor.constraint(equalTo: rowsContainer.trailingAnchor),
            rowsCard.topAnchor.constraint(equalTo: rowsContainer.topAnchor),
            rowsCard.bottomAnchor.constraint(equalTo: rowsContainer.bottomAnchor)
        ])
        let rowsScroll = NSScrollView()
        rowsScroll.identifier = NSUserInterfaceItemIdentifier("hub.diagnostics.rows-scroll")
        rowsScroll.hasVerticalScroller = true
        rowsScroll.autohidesScrollers = true
        rowsScroll.drawsBackground = false
        rowsScroll.borderType = .noBorder
        rowsScroll.documentView = rowsContainer
        rowsContainer.widthAnchor.constraint(equalTo: rowsScroll.contentView.widthAnchor).isActive = true

        rawDisclosure.target = self
        rawDisclosure.action = #selector(toggleRawReport)
        rawDisclosure.hubStyle = .flat
        rawDisclosure.hubFont = .systemFont(ofSize: 11.5, weight: .medium)
        rawTextView.isEditable = false
        rawTextView.identifier = NSUserInterfaceItemIdentifier("hub.diagnostics.raw-report")
        rawTextView.isSelectable = true
        rawTextView.font = .monospacedSystemFont(ofSize: 10.5, weight: .regular)
        rawTextView.textContainerInset = NSSize(width: 8, height: 8)
        rawTextView.isVerticallyResizable = true
        rawTextView.isHorizontallyResizable = false
        rawTextView.autoresizingMask = [.width]
        rawTextView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        rawTextView.textContainer?.widthTracksTextView = true
        rawScroll.hasVerticalScroller = true
        rawScroll.borderType = .noBorder
        rawScroll.documentView = rawTextView
        rawScroll.heightAnchor.constraint(equalToConstant: 105).isActive = true
        rawScroll.isHidden = true

        configureButton(copyButton, symbol: "doc.on.doc", style: .neutral,
                        action: #selector(copyPressed))
        configureButton(saveButton, symbol: "arrow.down.to.line", style: .neutral,
                        action: #selector(savePressed))
        let reportActions = NSStackView(views: [rawDisclosure, NSView(), copyButton, saveButton])
        reportActions.alignment = .centerY
        reportActions.spacing = 6
        reportActions.isHidden = controller.previewMode

        let privacy = NSTextField(wrappingLabelWithString:
            "Displayed, copied, and saved reports redact credentials and private identifiers. Review before sharing.")
        privacy.font = .systemFont(ofSize: 10.5)
        privacy.textColor = HubPalette.mutedForeground
        privacy.maximumNumberOfLines = 2
        privacy.isHidden = controller.previewMode

        let body = NSStackView(views: [statusDetail, rowsScroll, reportActions, rawScroll, privacy])
        body.orientation = .vertical
        body.alignment = .leading
        body.spacing = 10
        body.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(body)
        for view in [statusDetail, rowsScroll, reportActions, rawScroll, privacy] {
            view.widthAnchor.constraint(equalTo: body.widthAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            body.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 14),
            body.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -14),
            body.topAnchor.constraint(equalTo: root.topAnchor, constant: 14),
            body.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -14),
            rowsScroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 100)
        ])
        return root
    }

    private func showInitialSummary() {
        if controller.previewMode {
            let rows = Self.previewRows
            latestReport = rows.map { "== \($0.title) ==\n\($0.detail)" }
                .joined(separator: "\n\n")
            rawTextView.string = latestReport ?? ""
            statusDetail.stringValue = "\(rows.count) of \(rows.count) checks passed."
            render(rows: rows)
            copyButton.isEnabled = true
            saveButton.isEnabled = true
        } else {
            let summary = controller.diagnostics()
            statusDetail.stringValue = summary.isEmpty
                ? "Run diagnostics to collect a redacted report."
                : summary.joined(separator: " · ")
            render(rows: [])
            rawTextView.string = "No diagnostic report has been run."
            copyButton.isEnabled = false
            saveButton.isEnabled = false
        }
    }

    @objc private func runPressed() {
        guard !controller.previewMode else { return }
        runButton.isEnabled = false
        copyButton.isEnabled = false
        saveButton.isEnabled = false
        latestReport = nil
        rawTextView.string = ""
        rawDisclosure.title = "Show raw redacted report"
        rawScroll.isHidden = true
        statusDetail.stringValue = "Running checks…"
        render(rows: [])
        controller.runFullDiagnostics { [weak self] report in
            guard let self else { return }
            DispatchQueue.main.async {
                let safeReport = HubShareRedactor.redact(report)
                self.latestReport = safeReport
                self.rawTextView.string = safeReport
                let rows = HubDiagnosticsPresentation.rows(from: safeReport)
                self.render(rows: rows)
                let passed = rows.filter { $0.outcome == .passed }.count
                self.statusDetail.stringValue = rows.isEmpty
                    ? "No structured checks were returned."
                    : "\(passed) of \(rows.count) checks passed."
                self.runButton.isEnabled = true
                self.copyButton.isEnabled = !safeReport.isEmpty
                self.saveButton.isEnabled = !safeReport.isEmpty
            }
        }
    }

    private func render(rows: [HubDiagnosticRow]) {
        rowsStack.arrangedSubviews.forEach {
            rowsStack.removeArrangedSubview($0)
            $0.removeFromSuperview()
        }
        if rows.isEmpty {
            let empty = NSTextField(wrappingLabelWithString: "Run diagnostics to view completed checks.")
            empty.textColor = HubPalette.mutedForeground
            empty.font = .systemFont(ofSize: 12)
            let holder = NSStackView(views: [empty])
            holder.edgeInsets = NSEdgeInsets(top: 14, left: 14, bottom: 14, right: 14)
            rowsStack.addArrangedSubview(holder)
            holder.widthAnchor.constraint(equalTo: rowsStack.widthAnchor).isActive = true
        } else {
            for (index, row) in rows.enumerated() {
                if index > 0 {
                    let line = HubModalChrome.divider()
                    rowsStack.addArrangedSubview(line)
                    line.widthAnchor.constraint(equalTo: rowsStack.widthAnchor).isActive = true
                }
                let view = diagnosticRow(for: row)
                rowsStack.addArrangedSubview(view)
                view.widthAnchor.constraint(equalTo: rowsStack.widthAnchor).isActive = true
            }
        }
        rowsContainer.needsLayout = true
    }

    private func diagnosticRow(for row: HubDiagnosticRow) -> NSView {
        let view = NSView()
        view.identifier = NSUserInterfaceItemIdentifier("hub.diagnostics.row")
        let icon = NSImageView(image: NSImage(
            systemSymbolName: row.outcome == .failed ? "xmark.circle" : "checkmark.circle",
            accessibilityDescription: nil
        ) ?? NSImage())
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 16, weight: .medium)
        icon.imageScaling = .scaleProportionallyDown
        icon.contentTintColor = row.outcome == .failed ? HubPalette.danger : HubPalette.success
        icon.widthAnchor.constraint(equalToConstant: 17).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 17).isActive = true
        let title = NSTextField(labelWithString: row.title)
        title.font = .systemFont(ofSize: 12.5, weight: .medium)
        title.textColor = HubPalette.foreground
        let detail = NSTextField(labelWithString: row.detail.isEmpty ? "No additional detail." : row.detail)
        detail.font = .systemFont(ofSize: 11.5)
        detail.textColor = HubPalette.mutedForeground
        detail.lineBreakMode = .byTruncatingTail
        let labels = NSStackView(views: [title, detail])
        labels.orientation = .vertical
        labels.alignment = .leading
        labels.spacing = 2
        let stack = NSStackView(views: [icon, labels])
        stack.alignment = .centerY
        stack.spacing = 11
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 14),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -14),
            stack.topAnchor.constraint(equalTo: view.topAnchor, constant: 9),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -9),
            view.heightAnchor.constraint(equalToConstant: 52)
        ])
        return view
    }

    @objc private func toggleRawReport() {
        let visible = rawScroll.isHidden
        rawScroll.isHidden = !visible
        rawDisclosure.title = visible ? "Hide raw redacted report" : "Show raw redacted report"
        window?.contentView?.layoutSubtreeIfNeeded()
        rawTextView.fitDocument()
    }

    @objc private func copyPressed() {
        guard !controller.previewMode else { return }
        guard let latestReport else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(latestReport, forType: .string)
        statusDetail.stringValue = "Redacted report copied."
    }

    @objc private func savePressed() {
        guard !controller.previewMode else { return }
        guard let latestReport, let window else { return }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "teslatlas-hub-diagnostics.txt"
        panel.canCreateDirectories = true
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let destination = panel.url else { return }
            do {
                try HubAppLog.writePrivateReport(latestReport, to: destination)
                self?.statusDetail.stringValue = "Redacted report saved."
            } catch {
                HubUIPresentation.presentError(error)
            }
        }
    }

    func windowWillClose(_ notification: Notification) { onDismiss() }

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

    private static let previewRows: [HubDiagnosticRow] = [
        .init(title: "Environment doctor", detail: "Runtime and permissions OK", outcome: .passed),
        .init(title: "Preflight", detail: "Configuration is valid", outcome: .passed),
        .init(title: "Service status", detail: "Running", outcome: .passed),
        .init(title: "Database", detail: "Integrity check passed", outcome: .passed),
        .init(title: "Credentials", detail: "Fleet token valid", outcome: .passed),
        .init(title: "Connection", detail: "Reached Tesla Fleet endpoint", outcome: .passed),
        .init(title: "Recent logs", detail: "No errors in the last hour", outcome: .passed)
    ]
}

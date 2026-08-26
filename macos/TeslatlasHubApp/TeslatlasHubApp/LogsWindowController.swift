import AppKit

final class LogsWindowController: NSWindowController {
    private let controller: HubController
    private let textView = NSTextView()
    private let statusLabel = NSTextField(labelWithString: "Loading logs…")
    private let refreshButton = NSButton(title: "Refresh", target: nil, action: nil)
    private let copyButton = NSButton(title: "Copy", target: nil, action: nil)
    private let saveButton = NSButton(title: "Save…", target: nil, action: nil)
    private var latestText = ""

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
        configureFlatButton(copyButton, symbol: "doc.on.doc", action: #selector(copyPressed))
        configureFlatButton(saveButton, symbol: "square.and.arrow.down", action: #selector(savePressed))
        let actions = NSStackView(views: [refreshButton, copyButton, saveButton])
        actions.spacing = 14
        actions.alignment = .centerY

        let note = NSTextField(labelWithString:
            "Copy and Save redact credential values and shorten your home-folder path.")
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

    private func refresh() {
        refreshButton.isEnabled = false
        copyButton.isEnabled = false
        saveButton.isEnabled = false
        statusLabel.stringValue = "Loading logs…"
        controller.logs { [weak self] text in
            guard let self else { return }
            self.latestText = text
            self.textView.string = text
            self.statusLabel.stringValue = "Updated just now"
            self.refreshButton.isEnabled = true
            self.copyButton.isEnabled = !text.isEmpty
            self.saveButton.isEnabled = !text.isEmpty
        }
    }

    @objc private func refreshPressed() { refresh() }

    @objc private func copyPressed() {
        guard !latestText.isEmpty else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(Self.shareableText(latestText), forType: .string)
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
                try report.write(to: destination, atomically: true, encoding: .utf8)
                self?.statusLabel.stringValue = "Redacted logs saved"
            } catch {
                NSAlert(error: error).runModal()
            }
        }
    }

    private static func shareableText(_ text: String) -> String {
        HubShareRedactor.redact(text)
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
        button.contentTintColor = tint
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

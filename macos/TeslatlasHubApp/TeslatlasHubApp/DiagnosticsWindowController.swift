import AppKit

final class DiagnosticsWindowController: NSWindowController {
    private let controller: HubController
    private let textView = NSTextView()

    init(controller: HubController) {
        self.controller = controller
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 620, height: 390), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.title = "Run Diagnostics"
        super.init(window: window)
        window.contentView = contentView()
        window.center()
        refresh()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func contentView() -> NSView {
        let root = NSView()
        textView.isEditable = false
        textView.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.documentView = textView
        let button = NSButton(title: "Run Diagnostics", target: self, action: #selector(refreshPressed))
        button.bezelStyle = .rounded
        let stack = NSStackView(views: [scroll, button])
        stack.orientation = .vertical
        stack.alignment = .trailing
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -20),
            stack.topAnchor.constraint(equalTo: root.topAnchor, constant: 20),
            stack.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -20)
        ])
        return root
    }

    private func refresh() {
        textView.string = controller.diagnostics().joined(separator: "\n")
    }

    @objc private func refreshPressed() { refresh() }
}

import AppKit

final class LogsWindowController: NSWindowController {
    private let controller: HubController
    private let textView = NSTextView()

    init(controller: HubController) {
        self.controller = controller
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 760, height: 470), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
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
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.documentView = textView
        textView.isEditable = false
        textView.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        let refresh = NSButton(title: "Refresh", target: self, action: #selector(refreshPressed))
        refresh.bezelStyle = .rounded
        let stack = NSStackView(views: [scroll, refresh])
        stack.orientation = .vertical
        stack.alignment = .trailing
        stack.spacing = 12
        stack.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -20),
            stack.topAnchor.constraint(equalTo: root.topAnchor, constant: 20),
            stack.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -20),
            scroll.widthAnchor.constraint(greaterThanOrEqualToConstant: 500),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 300)
        ])
        return root
    }

    private func refresh() {
        controller.logs { [weak self] text in self?.textView.string = text }
    }

    @objc private func refreshPressed() { refresh() }
}

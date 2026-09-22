// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

/// A utility destination rendered inside the main Hub window. The top-level
/// navigation remains visible while Back returns to the owning Hub section.
final class HubEmbeddedUtilityPage: HubSurfaceView {
    private let onBack: () -> Void
    private var backButton: HubActionButton?

    init(symbol: String,
         title: String,
         subtitle: String,
         body: NSView,
         actions: [HubActionButton] = [],
         onBack: @escaping () -> Void) {
        self.onBack = onBack
        super.init(fill: .background)
        let identifierSlug = title.lowercased()
            .replacingOccurrences(of: " & ", with: "-")
            .replacingOccurrences(of: " ", with: "-")
        identifier = NSUserInterfaceItemIdentifier("hub.embedded." + identifierSlug)

        let back = HubActionButton(title: "Back", target: self, action: #selector(backPressed))
        backButton = back
        back.hubStyle = .neutral
        back.hubFont = HubTypography.action
        back.image = NSImage(systemSymbolName: "chevron.left", accessibilityDescription: "Back")
        back.imagePosition = .imageLeading
        back.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
        back.widthAnchor.constraint(greaterThanOrEqualToConstant: HubMetrics.actionMinimumWidth).isActive = true

        let actionStack = NSStackView(views: actions)
        actionStack.spacing = 8
        actionStack.alignment = .centerY
        let actionRow = NSStackView(views: [back, NSView(), actionStack])
        actionRow.alignment = .centerY
        actionRow.spacing = 8

        let header = HubPageHeaderView(symbol: symbol, title: title, subtitle: subtitle)
        body.translatesAutoresizingMaskIntoConstraints = false
        body.setContentHuggingPriority(.defaultLow, for: .vertical)
        body.setContentCompressionResistancePriority(.defaultLow, for: .vertical)

        let content = NSStackView(views: [actionRow, header, body])
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 16
        content.setCustomSpacing(20, after: header)
        content.translatesAutoresizingMaskIntoConstraints = false
        addSubview(content)
        NSLayoutConstraint.activate([
            content.centerXAnchor.constraint(equalTo: centerXAnchor),
            content.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor,
                                             constant: HubMetrics.pageInset),
            content.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor,
                                              constant: -HubMetrics.pageInset),
            content.widthAnchor.constraint(equalToConstant: HubMetrics.contentWidth),
            content.topAnchor.constraint(equalTo: topAnchor, constant: 20),
            content.bottomAnchor.constraint(equalTo: bottomAnchor,
                                            constant: -HubMetrics.pageInset),
            actionRow.widthAnchor.constraint(equalTo: content.widthAnchor),
            header.widthAnchor.constraint(equalTo: content.widthAnchor),
            body.widthAnchor.constraint(equalTo: content.widthAnchor)
        ])
    }

    func setNavigationEnabled(_ enabled: Bool) { backButton?.isEnabled = enabled }

    func focusInitialResponder(in window: NSWindow?) {
        guard let backButton, backButton.isEnabled else { return }
        window?.initialFirstResponder = backButton
        window?.makeFirstResponder(backButton)
    }

    var backButtonForTesting: NSButton? { backButton }

    @objc private func backPressed() { onBack() }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

enum HubUtilityWindowStyle {
    static func makeWindow(title: String, size: NSSize, minimum: NSSize) -> NSWindow {
        let window = NSWindow(contentRect: NSRect(origin: .zero, size: size),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = title
        window.isReleasedWhenClosed = false
        window.isMovable = true
        window.contentMinSize = minimum
        window.backgroundColor = HubPalette.background
        window.identifier = NSUserInterfaceItemIdentifier("hub.utility." + title)
        if !HubUIPresentation.isSilentTestHost {
            window.setFrameAutosaveName("Hub.Utility." + title)
        }
        return window
    }
}

enum HubOnboardingSheetStyle {
    static func makeWindow(contentSize: NSSize, dismissible: Bool) -> NSWindow {
        let window = NSWindow(contentRect: NSRect(origin: .zero, size: contentSize),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "Teslatlas Hub"
        window.titleVisibility = .visible
        window.titlebarAppearsTransparent = false
        window.isReleasedWhenClosed = false
        window.isMovable = true
        window.backgroundColor = HubPalette.background
        window.minSize = NSSize(width: 820, height: 640)
        window.standardWindowButton(.closeButton)?.isEnabled = dismissible
        return window
    }
}

/// Native toolbar chrome; buttons retain their shared measured icon/label layout.
final class HubUtilityToolbar: NSObject, NSToolbarDelegate {
    private let buttons: [HubActionButton]
    private let identifiers: [NSToolbarItem.Identifier]
    let toolbar: NSToolbar

    init(identifier: String, buttons: [HubActionButton]) {
        self.buttons = buttons
        self.identifiers = buttons.enumerated().map {
            NSToolbarItem.Identifier(identifier + ".action." + String($0.offset))
        }
        toolbar = NSToolbar(identifier: NSToolbar.Identifier(identifier))
        super.init()
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        toolbar.allowsUserCustomization = false
        toolbar.autosavesConfiguration = false
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.flexibleSpace] + identifiers
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.flexibleSpace] + identifiers
    }

    func toolbar(_ toolbar: NSToolbar, itemForItemIdentifier identifier: NSToolbarItem.Identifier,
                 willBeInsertedIntoToolbar flag: Bool) -> NSToolbarItem? {
        guard let index = identifiers.firstIndex(of: identifier) else { return nil }
        let button = buttons[index]
        let item = NSToolbarItem(itemIdentifier: identifier)
        item.label = button.title
        item.view = button
        item.isBordered = false
        button.setContentCompressionResistancePriority(.required, for: .horizontal)
        return item
    }
}

/// Wrapping log/report document whose height follows the text layout on resize.
final class HubReportTextView: NSTextView {
    override func setFrameSize(_ newSize: NSSize) {
        super.setFrameSize(newSize)
        if abs(newSize.width - (textContainer?.containerSize.width ?? 0) - textContainerInset.width * 2) > 0.5 {
            fitDocument()
        }
    }

    func fitDocument() {
        guard let container = textContainer, let manager = layoutManager else { return }
        let width = enclosingScrollView?.contentSize.width ?? frame.width
        container.containerSize = NSSize(width: max(1, width - textContainerInset.width * 2),
                                         height: .greatestFiniteMagnitude)
        manager.ensureLayout(for: container)
        let height = max(enclosingScrollView?.contentSize.height ?? 0,
                         ceil(manager.usedRect(for: container).height) + textContainerInset.height * 2)
        super.setFrameSize(NSSize(width: width, height: height))
    }
}

// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

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
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.isReleasedWhenClosed = false
        window.isMovable = false
        window.backgroundColor = HubPalette.background
        // A sheet owns its Step header and explicit Cancel action. No imitation X
        // and no fullSizeContentView overlapping the window-server titlebar.
        window.standardWindowButton(.closeButton)?.isHidden = true
        window.standardWindowButton(.miniaturizeButton)?.isHidden = true
        window.standardWindowButton(.zoomButton)?.isHidden = true
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

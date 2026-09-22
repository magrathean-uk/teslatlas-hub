// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

struct HubNavigationActions {
    let select: (HubMainSection) -> Void
    let diagnostics: () -> Void
    let logs: () -> Void
    let serviceDetails: () -> Void
    let importTeslaMate: () -> Void
    let connectTesla: () -> Void
    let accountMenu: () -> NSMenu
    let appearance: () -> Void
}

private struct HubNavigationItem {
    let section: HubMainSection
    let title: String
    let symbol: String
}

/// One native toolbar control keeps every destination on the same baseline.
final class HubNavigationBar: NSView {
    private let actions: HubNavigationActions
    private let stack = NSStackView()
    private var buttons: [HubActionButton] = []
    private var selectedSection: HubMainSection = .dashboard
    private let items = [
        HubNavigationItem(section: .dashboard, title: "Overview", symbol: "house"),
        HubNavigationItem(section: .vehicles, title: "Vehicles", symbol: "car.side"),
        HubNavigationItem(section: .activity, title: "Activity", symbol: "list.bullet.rectangle"),
        HubNavigationItem(section: .settings, title: "Settings", symbol: "gearshape")
    ]

    init(actions: HubNavigationActions) {
        self.actions = actions
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("hub.navigation")
        stack.orientation = .horizontal
        stack.alignment = .centerY
        stack.spacing = 4
        stack.translatesAutoresizingMaskIntoConstraints = false
        for (index, item) in items.enumerated() {
            let button = HubActionButton(title: item.title, target: self, action: #selector(selectionChanged(_:)))
            button.tag = index
            button.image = NSImage(systemSymbolName: item.symbol,
                                   accessibilityDescription: item.title)
            button.imagePosition = .imageLeading
            button.hubFont = .systemFont(ofSize: 13, weight: .medium)
            button.horizontalInset = 16
            button.iconBoxSize = 18
            button.hubStyle = .flat
            button.toolTip = "Show \(item.title)"
            button.setAccessibilityLabel(item.title)
            button.setAccessibilityRole(.radioButton)
            button.wantsLayer = true
            button.layer?.cornerRadius = 8
            button.layer?.cornerCurve = .continuous
            button.translatesAutoresizingMaskIntoConstraints = false
            button.widthAnchor.constraint(equalToConstant: 120).isActive = true
            button.heightAnchor.constraint(equalToConstant: 36).isActive = true
            buttons.append(button)
            stack.addArrangedSubview(button)
        }
        stack.setAccessibilityElement(false)
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
            heightAnchor.constraint(equalToConstant: 36)
        ])
        select(.dashboard)
    }

    func select(_ section: HubMainSection) {
        selectedSection = section
        updateSelectionAppearance()
    }

    func apply(snapshot: HubSnapshot, enabled: Bool) {
        for button in buttons {
            button.isEnabled = enabled
            button.setAccessibilityHelp("\(snapshot.health.title). Show \(button.title).")
        }
    }

    func focus(_ section: HubMainSection, in window: NSWindow?) {
        guard let index = items.firstIndex(where: { $0.section == section }),
              buttons[index].isEnabled else { return }
        window?.initialFirstResponder = buttons[index]
        window?.makeFirstResponder(buttons[index])
    }

    @objc private func selectionChanged(_ sender: HubActionButton) {
        guard items.indices.contains(sender.tag) else { return }
        select(items[sender.tag].section)
        actions.select(items[sender.tag].section)
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateSelectionAppearance()
    }

    private func updateSelectionAppearance() {
        for (index, button) in buttons.enumerated() {
            let selected = items[index].section == selectedSection
            button.hubStyle = selected ? .navigationSelected : .flat
            button.updateHubAppearance()
            button.setAccessibilityValue(selected ? "Selected" : "")
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

final class HubMainToolbar: NSObject, NSToolbarDelegate {
    private enum Item { static let navigation = NSToolbarItem.Identifier("hub.toolbar.navigation") }
    private let navigationBar: HubNavigationBar
    let toolbar = NSToolbar(identifier: "hub.main.toolbar")

    init(navigationBar: HubNavigationBar) {
        self.navigationBar = navigationBar
        super.init()
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        toolbar.allowsUserCustomization = false
        toolbar.autosavesConfiguration = false
        toolbar.centeredItemIdentifier = Item.navigation
    }

    func apply(snapshot: HubSnapshot, enabled: Bool) {
        navigationBar.apply(snapshot: snapshot, enabled: enabled)
    }

    func showAccountMenuForPreview() {}

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.flexibleSpace, Item.navigation]
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.flexibleSpace, Item.navigation, .flexibleSpace]
    }

    func toolbar(_ toolbar: NSToolbar, itemForItemIdentifier identifier: NSToolbarItem.Identifier,
                 willBeInsertedIntoToolbar flag: Bool) -> NSToolbarItem? {
        guard identifier == Item.navigation else { return nil }
        let item = NSToolbarItem(itemIdentifier: identifier)
        item.label = "Navigation"
        item.paletteLabel = "Navigation"
        item.view = navigationBar
        item.isBordered = false
        navigationBar.widthAnchor.constraint(equalToConstant: 492).isActive = true
        return item
    }
}

/// Keeps onboarding in the same native titlebar geometry as the ready app while
/// reserving the navigation area without exposing destinations before setup.
final class HubOnboardingToolbar: NSObject, NSToolbarDelegate {
    private enum Item { static let placeholder = NSToolbarItem.Identifier("hub.toolbar.onboarding") }
    private let placeholder = NSView()
    let toolbar = NSToolbar(identifier: "hub.onboarding.toolbar")

    override init() {
        super.init()
        placeholder.setAccessibilityElement(false)
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        toolbar.allowsUserCustomization = false
        toolbar.autosavesConfiguration = false
        toolbar.centeredItemIdentifier = Item.placeholder
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.flexibleSpace, Item.placeholder]
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.flexibleSpace, Item.placeholder, .flexibleSpace]
    }

    func toolbar(_ toolbar: NSToolbar, itemForItemIdentifier identifier: NSToolbarItem.Identifier,
                 willBeInsertedIntoToolbar flag: Bool) -> NSToolbarItem? {
        guard identifier == Item.placeholder else { return nil }
        let item = NSToolbarItem(itemIdentifier: identifier)
        item.label = "Setup"
        item.view = placeholder
        item.isBordered = false
        placeholder.widthAnchor.constraint(equalToConstant: 492).isActive = true
        placeholder.heightAnchor.constraint(equalToConstant: 36).isActive = true
        return item
    }
}

final class HubPageHeaderView: NSView {
    init(symbol: String, title: String, subtitle: String) {
        super.init(frame: .zero)
        let tile = HubIconTileView(symbol: symbol, accessibilityDescription: title,
                                   size: 48, symbolSize: 24, weight: .medium, radius: 12)
        let heading = NSTextField(labelWithString: title)
        heading.font = HubTypography.heading
        let detail = NSTextField(labelWithString: subtitle)
        detail.font = HubTypography.body
        detail.textColor = .secondaryLabelColor
        let copy = NSStackView(views: [heading, detail])
        copy.orientation = .vertical
        copy.alignment = .leading
        copy.spacing = 2
        let row = NSStackView(views: [tile, copy])
        row.alignment = .centerY
        row.spacing = 16
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor),
            row.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor),
            row.topAnchor.constraint(equalTo: topAnchor),
            row.bottomAnchor.constraint(equalTo: bottomAnchor)
        ])
        setAccessibilityElement(true)
        setAccessibilityLabel(title)
        setAccessibilityHelp(subtitle)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

private final class HubPageRowView: HubStatusRowView {
    var tone: HubStatusTone? {
        get { statusTone }
        set { statusTone = newValue }
    }

    init(symbol: String, title: String, detail: String = "", showsChevron: Bool = true,
         target: AnyObject? = nil, action: Selector? = nil) {
        super.init(symbol: symbol, title: title, detail: detail, showsChevron: showsChevron)
        if let action {
            let button = HubActionButton(title: "", target: target, action: action)
            button.hubStyle = .flat
            button.setAccessibilityLabel(title)
            button.setAccessibilityHelp(detail)
            button.translatesAutoresizingMaskIntoConstraints = false
            addSubview(button)
            NSLayoutConstraint.activate([
                button.leadingAnchor.constraint(equalTo: leadingAnchor),
                button.trailingAnchor.constraint(equalTo: trailingAnchor),
                button.topAnchor.constraint(equalTo: topAnchor),
                button.bottomAnchor.constraint(equalTo: bottomAnchor)
            ])
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

private func hubGroupedCard(_ rows: [NSView]) -> NSView {
    let card = HubCardView()
    let stack = NSStackView()
    stack.orientation = .vertical
    stack.spacing = 0
    for (index, row) in rows.enumerated() {
        if index > 0 {
            let line = NSBox()
            line.boxType = .separator
            stack.addArrangedSubview(line)
        }
        stack.addArrangedSubview(row)
        row.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
    }
    stack.translatesAutoresizingMaskIntoConstraints = false
    card.addSubview(stack)
    NSLayoutConstraint.activate([
        stack.leadingAnchor.constraint(equalTo: card.leadingAnchor),
        stack.trailingAnchor.constraint(equalTo: card.trailingAnchor),
        stack.topAnchor.constraint(equalTo: card.topAnchor),
        stack.bottomAnchor.constraint(equalTo: card.bottomAnchor)
    ])
    return card
}

class HubCompactPageView: HubSurfaceView {
    let content = NSStackView()
    private var contentBottomConstraint: NSLayoutConstraint!

    init(header: NSView) {
        super.init(fill: .background)
        content.orientation = .vertical
        content.alignment = .leading
        content.spacing = 16
        content.translatesAutoresizingMaskIntoConstraints = false
        content.addArrangedSubview(header)
        header.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
        addSubview(content)
        contentBottomConstraint = content.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor,
                                                                   constant: -HubMetrics.pageInset)
        NSLayoutConstraint.activate([
            content.centerXAnchor.constraint(equalTo: centerXAnchor),
            content.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: HubMetrics.pageInset),
            content.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -HubMetrics.pageInset),
            content.widthAnchor.constraint(equalToConstant: HubMetrics.contentWidth),
            content.topAnchor.constraint(equalTo: topAnchor, constant: HubMetrics.pageTopInset),
            contentBottomConstraint
        ])
    }

    func pinContentToBottom() {
        contentBottomConstraint.isActive = false
        contentBottomConstraint = content.bottomAnchor.constraint(equalTo: bottomAnchor,
                                                                   constant: -HubMetrics.pageInset)
        contentBottomConstraint.isActive = true
    }

    func addSection(title: String? = nil, view: NSView) {
        if let title {
            let label = NSTextField(labelWithString: title)
            label.font = .systemFont(ofSize: 14, weight: .medium)
            label.textColor = .secondaryLabelColor
            content.addArrangedSubview(label)
            content.setCustomSpacing(7, after: label)
        }
        content.addArrangedSubview(view)
        view.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

final class HubActivityView: HubCompactPageView, NSSearchFieldDelegate {
    private let actions: HubNavigationActions
    private let rowsHost = NSStackView()
    private let search = NSSearchField()
    private var entries: [HubActivity] = []
    private var renderedEntries: [String]?

    init(actions: HubNavigationActions) {
        self.actions = actions
        let header = HubPageHeaderView(symbol: "list.bullet.rectangle", title: "Activity",
                                       subtitle: "Recent Hub events on this Mac.")
        super.init(header: header)

        search.placeholderString = "Search activity"
        search.controlSize = .regular
        search.delegate = self
        search.sendsSearchStringImmediately = true
        search.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
        let logs = HubActionButton(title: "Open Logs…", target: self, action: #selector(openLogs))
        logs.hubStyle = .neutral
        logs.hubFont = HubTypography.action
        logs.image = NSImage(systemSymbolName: "doc.text.magnifyingglass",
                             accessibilityDescription: "Open detailed logs")
        logs.imagePosition = .imageLeading
        logs.heightAnchor.constraint(equalToConstant: 32).isActive = true
        let controls = NSStackView(views: [search, logs])
        controls.spacing = 12
        controls.alignment = .centerY
        content.addArrangedSubview(controls)
        controls.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true

        rowsHost.orientation = .vertical
        rowsHost.spacing = 0
        let card = HubCardView()
        rowsHost.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(rowsHost)
        NSLayoutConstraint.activate([
            rowsHost.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            rowsHost.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            rowsHost.topAnchor.constraint(equalTo: card.topAnchor),
            rowsHost.bottomAnchor.constraint(equalTo: card.bottomAnchor)
        ])
        addSection(title: "Recent", view: card)

        content.addArrangedSubview(NSView())
        let lock = NSImageView(image: NSImage(systemSymbolName: "lock",
                                              accessibilityDescription: nil) ?? NSImage())
        lock.contentTintColor = HubPalette.mutedForeground
        lock.widthAnchor.constraint(equalToConstant: 16).isActive = true
        let privacy = NSTextField(labelWithString: "Sensitive values are redacted from logs.")
        privacy.font = HubTypography.label
        privacy.textColor = HubPalette.mutedForeground
        let version = NSTextField(labelWithString: "Teslatlas Hub \(HubRelease.bundledVersion)")
        version.font = HubTypography.label
        version.textColor = HubPalette.mutedForeground
        let footer = NSStackView(views: [lock, privacy, NSView(), version])
        footer.spacing = 8
        footer.alignment = .centerY
        content.addArrangedSubview(footer)
        footer.widthAnchor.constraint(equalTo: content.widthAnchor).isActive = true
        pinContentToBottom()
    }

    func apply(snapshot: HubSnapshot, activity: [HubActivity]) {
        entries = activity.isEmpty ? snapshot.activity : activity
        renderEntries()
    }

    func controlTextDidChange(_ notification: Notification) { renderEntries() }

    private func renderEntries() {
        let query = search.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let visible = entries.filter { query.isEmpty || $0.message.localizedCaseInsensitiveContains(query) }
        let signature = [query] + visible.map { $0.message + "|" + $0.age }
        guard renderedEntries != signature else { return }
        if renderedEntries != nil { HubMotion.transition(rowsHost) }
        renderedEntries = signature
        rowsHost.arrangedSubviews.forEach {
            rowsHost.removeArrangedSubview($0)
            $0.removeFromSuperview()
        }
        if visible.isEmpty {
            addActivityRow(symbol: "clock", title: query.isEmpty ? "No recent activity" : "No matching events",
                           detail: query.isEmpty ? "Events will appear here as they happen." : "Try another search.")
            return
        }
        for (index, entry) in visible.enumerated() {
            if index > 0 {
                let line = NSBox()
                line.boxType = .separator
                rowsHost.addArrangedSubview(line)
            }
            addActivityRow(symbol: "clock", title: entry.message, detail: entry.age)
        }
    }

    private func addActivityRow(symbol: String, title: String, detail: String) {
        let row = HubPageRowView(symbol: symbol, title: title, detail: detail, showsChevron: false)
        rowsHost.addArrangedSubview(row)
        row.widthAnchor.constraint(equalTo: rowsHost.widthAnchor).isActive = true
    }

    @objc private func openLogs() { actions.logs() }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

final class HubSettingsView: HubCompactPageView {
    private let actions: HubNavigationActions
    private var accountRow: HubPageRowView!
    private var databaseRow: HubPageRowView!
    private var serviceRow: HubPageRowView!
    private var importRow: HubPageRowView!

    init(actions: HubNavigationActions) {
        self.actions = actions
        let header = HubPageHeaderView(symbol: "gearshape", title: "Settings",
                                       subtitle: "Manage this Hub and its connections.")
        super.init(header: header)

        accountRow = HubPageRowView(symbol: "person", title: "Tesla account",
                                    detail: "",
                                    target: self, action: #selector(accountPressed(_:)))
        databaseRow = HubPageRowView(symbol: "cylinder", title: "Local database",
                                     detail: "", showsChevron: false)
        importRow = HubPageRowView(symbol: "square.and.arrow.down",
                                   title: "Import TeslaMate history",
                                   detail: "Import your existing TeslaMate data.",
                                   target: self, action: #selector(importPressed))
        importRow.identifier = NSUserInterfaceItemIdentifier("hub.settings.import-teslamate")
        addSection(title: "Connections & data",
                   view: hubGroupedCard([accountRow, databaseRow, importRow]))
        serviceRow = HubPageRowView(symbol: "gearshape.2", title: "Background service",
                                    detail: "",
                                    target: self, action: #selector(servicePressed))
        let diagnostics = HubPageRowView(symbol: "stethoscope", title: "Diagnostics",
                                         detail: "Check system status and logs.",
                                         target: self, action: #selector(diagnosticsPressed))
        addSection(title: "This Mac", view: hubGroupedCard([serviceRow, diagnostics]))
        let appearance = HubPageRowView(symbol: "circle.lefthalf.filled", title: "Appearance",
                                        detail: "",
                                        target: self, action: #selector(appearancePressed))
        addSection(view: hubGroupedCard([appearance]))
        let about = HubPageRowView(symbol: "info.circle", title: "About Teslatlas Hub",
                                   detail: "Version \(HubRelease.bundledVersion)",
                                   target: self, action: #selector(aboutPressed))
        addSection(view: hubGroupedCard([about]))
    }

    func apply(snapshot: HubSnapshot) {
        accountRow.value = snapshot.accountDisplay
        databaseRow.value = snapshot.database
        let hideImport = !snapshot.shouldOfferTeslaMateImport
        importRow.isHidden = hideImport
        if let stack = importRow.superview as? NSStackView,
           let index = stack.arrangedSubviews.firstIndex(of: importRow), index > 0 {
            stack.arrangedSubviews[index - 1].isHidden = hideImport
        }
        serviceRow.value = snapshot.health.title
        serviceRow.tone = snapshot.health == .running ? .success
            : (snapshot.health == .degraded ? .danger : .warning)
    }

    @objc private func accountPressed(_ sender: NSButton) {
        let menu = actions.accountMenu()
        menu.popUp(positioning: nil, at: NSPoint(x: sender.bounds.minX, y: sender.bounds.maxY), in: sender)
    }
    @objc private func importPressed() { actions.importTeslaMate() }
    @objc private func diagnosticsPressed() { actions.diagnostics() }
    @objc private func servicePressed() { actions.serviceDetails() }
    @objc private func appearancePressed() { actions.appearance() }
    @objc private func aboutPressed() { NSApp.orderFrontStandardAboutPanel(nil) }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

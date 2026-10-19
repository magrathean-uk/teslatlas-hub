// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

struct HubServiceDetail: Equatable {
    let label: String
    let value: String
}

struct HubServiceLifecycleActions {
    let start: () -> Void
    let stop: () -> Void
    let restart: () -> Void
    /// Remove Background Service: the non-destructive choice (data and logs stay).
    let remove: () -> Void
    /// Delete Hub and Data: the owner's confirmed, user-level deletion.
    let deleteHubAndData: () -> Void

    init(start: @escaping () -> Void,
         stop: @escaping () -> Void,
         restart: @escaping () -> Void,
         remove: @escaping () -> Void = {},
         deleteHubAndData: @escaping () -> Void = {}) {
        self.start = start
        self.stop = stop
        self.restart = restart
        self.remove = remove
        self.deleteHubAndData = deleteHubAndData
    }
}

final class ServiceDetailsWindowController: NSWindowController, NSWindowDelegate {
    typealias ConfirmationPresenter = (NSAlert, NSApplication.ModalResponse) -> NSApplication.ModalResponse

    private let controller: HubController
    private let lifecycleActions: HubServiceLifecycleActions?
    private let mutationAllowed: () -> Bool
    private let onDismiss: () -> Void
    private let errorPresenter: (Error) -> Void
    private let confirmationPresenter: ConfirmationPresenter
    private let rowsStack = NSStackView()
    private let lifecycleCard = HubCardView()
    private let lifecycleDetail = NSTextField(wrappingLabelWithString: "")
    private let pendingSurface = HubSurfaceView(fill: .elevated)
    private let pendingProgress = NSProgressIndicator()
    private let pendingTitle = NSTextField(labelWithString: "")
    private let pendingDetail = NSTextField(wrappingLabelWithString: "")
    private let startStopButton = HubActionButton(title: HubL10n.text("hub.HubDashboardView.96.1352", fallback: "Stop Hub…"), target: nil, action: nil)
    private let restartButton = HubActionButton(title: HubL10n.text("hub.HubDashboardView.97.1353", fallback: "Restart Hub"), target: nil, action: nil)
    private let removeButton = HubActionButton(title: HubL10n.text("hub.service.remove.button", fallback: "Remove Background Service…"), target: nil, action: nil)
    private let deleteDataButton = HubActionButton(title: HubL10n.text("hub.ServiceDetailsWindowController.38.2169", fallback: "Delete Hub and Data…"), target: nil, action: nil)
    private var mutationsEnabled = true
    private var embeddedBody: NSView?
    private var serviceHealth: HubHealth

    init(snapshot: HubSnapshot,
         controller: HubController,
         lifecycleActions: HubServiceLifecycleActions? = nil,
         mutationAllowed: @escaping () -> Bool = { true },
         onDismiss: @escaping () -> Void = {},
         embedded: Bool = false,
         errorPresenter: @escaping (Error) -> Void = HubUIPresentation.presentError,
         confirmationPresenter: @escaping ConfirmationPresenter = { alert, silentResponse in
             HubUIPresentation.response(to: alert, silentResponse: silentResponse)
         }) {
        self.controller = controller
        self.lifecycleActions = lifecycleActions
        self.mutationAllowed = mutationAllowed
        self.onDismiss = onDismiss
        self.errorPresenter = errorPresenter
        self.confirmationPresenter = confirmationPresenter
        self.serviceHealth = snapshot.health
        super.init(window: embedded ? nil : HubUtilityWindowStyle.makeWindow(
            title: HubL10n.text("hub.AppDelegate.132.43", fallback: "Service Details"), size: HubMetrics.serviceDetailsSheetSize,
            minimum: NSSize(width: 450, height: 380)
        ))
        window?.title = HubL10n.text("hub.AppDelegate.132.43", fallback: "Service Details")
        window?.delegate = self
        let body = contentView()
        window?.contentView = body
        embeddedBody = embedded ? body : nil
        update(snapshot: snapshot)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func makeEmbeddedPage(onBack: @escaping () -> Void) -> NSView {
        let body = window?.contentView ?? embeddedBody ?? NSView()
        embeddedBody = nil
        if let hostWindow = window {
            hostWindow.delegate = nil
            hostWindow.contentView = NSView()
            hostWindow.close()
            window = nil
        }
        return HubEmbeddedUtilityPage(
            symbol: "slider.horizontal.3",
            title: HubL10n.text("hub.ServiceDetailsWindowController.104.2180", fallback: "Service details"),
            subtitle: HubL10n.text("hub.ServiceDetailsWindowController.105.2181", fallback: "Inspect and manage the local Hub service."),
            body: body,
            onBack: onBack
        )
    }

    static func details(for snapshot: HubSnapshot) -> [HubServiceDetail] {
        [
            .init(label: HubL10n.text("hub.ServiceDetailsWindowController.113.2182", fallback: "Version"), value: HubL10n.format("hub.HubDashboardView.279.1374", fallback: "Teslatlas Hub %1$@", arguments: [String(describing: snapshot.version)])),
            .init(label: HubL10n.text("hub.ServiceDetailsWindowController.114.2184", fallback: "Service"), value: snapshot.checkingStatus ? "Checking Hub" : (snapshot.health == .running ? HubL10n.text("hub.ServiceDetailsWindowController.114.2185", fallback: "Active") : snapshot.service)),
            .init(label: HubL10n.text("hub.ServiceDetailsWindowController.115.2186", fallback: "Provider"), value: snapshot.provider?.displayName ?? HubL10n.text("hub.HubController.393.343", fallback: "Not configured")),
            .init(label: HubL10n.text("hub.HubController.1162.479", fallback: "Tesla account"), value: snapshot.accountDisplay),
            .init(label: HubL10n.text("hub.DiagnosticsWindowController.374.248", fallback: "Database"), value: snapshot.database),
            .init(label: HubL10n.text("hub.ServiceDetailsWindowController.118.2190", fallback: "Data folder"),
                  value: snapshot.dataDirectory.map {
                      ($0.path as NSString).abbreviatingWithTildeInPath
                  } ?? HubL10n.text("hub.ServiceDetailsWindowController.121.2191", fallback: "Not available"))
        ]
    }

    func update(snapshot: HubSnapshot) {
        serviceHealth = snapshot.health
        rowsStack.arrangedSubviews.forEach {
            rowsStack.removeArrangedSubview($0)
            $0.removeFromSuperview()
        }
        for (index, detail) in Self.details(for: snapshot).enumerated() {
            if index > 0 {
                let line = HubModalChrome.divider()
                rowsStack.addArrangedSubview(line)
                line.widthAnchor.constraint(equalTo: rowsStack.widthAnchor).isActive = true
            }
            let row = ServiceDetailRowView(detail: detail)
            rowsStack.addArrangedSubview(row)
            row.widthAnchor.constraint(equalTo: rowsStack.widthAnchor).isActive = true
        }
        updateLifecycleButtons()
    }

    func setMutationsEnabled(_ enabled: Bool) {
        mutationsEnabled = enabled
        updateMutationButtons()
    }

    func setServiceTransition(_ transition: HubServiceTransition?) {
        guard let transition else {
            pendingProgress.stopAnimation(nil)
            pendingSurface.isHidden = true
            return
        }
        showPending(title: transition.title, detail: transition.subtitle)
    }

    private func contentView() -> NSView {
        let root = HubSurfaceView(fill: .background)

        rowsStack.orientation = .vertical
        rowsStack.alignment = .leading
        rowsStack.spacing = 0
        rowsStack.translatesAutoresizingMaskIntoConstraints = false
        let detailsCard = HubCardView()
        detailsCard.identifier = NSUserInterfaceItemIdentifier("hub.service.details")
        detailsCard.addSubview(rowsStack)
        NSLayoutConstraint.activate([
            rowsStack.leadingAnchor.constraint(equalTo: detailsCard.leadingAnchor),
            rowsStack.trailingAnchor.constraint(equalTo: detailsCard.trailingAnchor),
            rowsStack.topAnchor.constraint(equalTo: detailsCard.topAnchor),
            rowsStack.bottomAnchor.constraint(equalTo: detailsCard.bottomAnchor)
        ])

        let lifecycleTitle = NSTextField(labelWithString: HubL10n.text("hub.ServiceDetailsWindowController.177.2193", fallback: "Service controls"))
        lifecycleTitle.font = HubTypography.emphasis
        lifecycleTitle.textColor = HubPalette.foreground
        lifecycleDetail.font = HubTypography.body
        lifecycleDetail.textColor = HubPalette.mutedForeground
        lifecycleDetail.maximumNumberOfLines = 0
        configureButton(startStopButton, symbol: "stop.fill", style: .flatDanger,
                        action: #selector(startStopPressed))
        startStopButton.identifier = NSUserInterfaceItemIdentifier("hub.service.start-stop")
        configureButton(restartButton, symbol: "arrow.clockwise", style: .neutral,
                        action: #selector(restartPressed))
        restartButton.identifier = NSUserInterfaceItemIdentifier("hub.service.restart")
        configureButton(removeButton, symbol: nil, style: .neutral,
                        action: #selector(removePressed))
        removeButton.identifier = NSUserInterfaceItemIdentifier("hub.service.remove")
        let lifecycleButtons = NSStackView(views: [startStopButton, restartButton, removeButton])
        lifecycleButtons.alignment = .centerY
        lifecycleButtons.spacing = 8
        let lifecycleContents = NSStackView(views: [lifecycleTitle, lifecycleDetail, lifecycleButtons])
        lifecycleContents.orientation = .vertical
        lifecycleContents.alignment = .leading
        lifecycleContents.spacing = 0
        lifecycleContents.setCustomSpacing(4, after: lifecycleTitle)
        lifecycleContents.setCustomSpacing(12, after: lifecycleDetail)
        lifecycleContents.translatesAutoresizingMaskIntoConstraints = false
        lifecycleCard.identifier = NSUserInterfaceItemIdentifier("hub.service.lifecycle")
        lifecycleCard.addSubview(lifecycleContents)
        NSLayoutConstraint.activate([
            lifecycleContents.leadingAnchor.constraint(equalTo: lifecycleCard.leadingAnchor,
                                                       constant: HubMetrics.rowHorizontalInset),
            lifecycleContents.trailingAnchor.constraint(equalTo: lifecycleCard.trailingAnchor,
                                                        constant: -HubMetrics.rowHorizontalInset),
            lifecycleContents.topAnchor.constraint(equalTo: lifecycleCard.topAnchor, constant: 14),
            lifecycleContents.bottomAnchor.constraint(equalTo: lifecycleCard.bottomAnchor, constant: -14)
        ])

        pendingSurface.identifier = NSUserInterfaceItemIdentifier("hub.service.pending")
        pendingSurface.layer?.cornerRadius = HubMetrics.controlRadius
        pendingProgress.style = .spinning
        pendingProgress.controlSize = .small
        pendingProgress.setAccessibilityLabel("Service operation progress")
        pendingTitle.font = HubTypography.emphasis
        pendingTitle.textColor = HubPalette.foreground
        pendingDetail.font = HubTypography.caption
        pendingDetail.textColor = HubPalette.mutedForeground
        pendingDetail.maximumNumberOfLines = 0
        let pendingCopy = NSStackView(views: [pendingTitle, pendingDetail])
        pendingCopy.orientation = .vertical
        pendingCopy.alignment = .leading
        pendingCopy.spacing = 2
        let pendingContents = NSStackView(views: [pendingProgress, pendingCopy])
        pendingContents.alignment = .centerY
        pendingContents.spacing = 10
        pendingContents.translatesAutoresizingMaskIntoConstraints = false
        pendingSurface.addSubview(pendingContents)
        NSLayoutConstraint.activate([
            pendingContents.leadingAnchor.constraint(equalTo: pendingSurface.leadingAnchor, constant: 12),
            pendingContents.trailingAnchor.constraint(equalTo: pendingSurface.trailingAnchor, constant: -12),
            pendingContents.topAnchor.constraint(equalTo: pendingSurface.topAnchor, constant: 10),
            pendingContents.bottomAnchor.constraint(equalTo: pendingSurface.bottomAnchor, constant: -10)
        ])
        pendingSurface.isHidden = true

        configureButton(deleteDataButton, symbol: "trash", style: .flatDanger,
                        action: #selector(deleteDataPressed))
        deleteDataButton.identifier = NSUserInterfaceItemIdentifier("hub.service.delete-data")
        let dangerTitle = NSTextField(labelWithString: HubL10n.text("hub.service.delete.title", fallback: "Delete Hub and Data"))
        dangerTitle.font = .systemFont(ofSize: 13, weight: .semibold)
        dangerTitle.textColor = HubPalette.foreground
        let dangerDetail = NSTextField(wrappingLabelWithString:
            HubL10n.text("hub.service.delete.detail", fallback: "Stops the background service and permanently deletes the Hub's data, configuration, sign-in details and logs from this Mac. The app itself stays until you move it to the Trash."))
        dangerDetail.font = .systemFont(ofSize: 12.5)
        dangerDetail.textColor = HubPalette.mutedForeground
        dangerDetail.maximumNumberOfLines = 0
        let dangerContents = NSStackView(views: [dangerTitle, dangerDetail, deleteDataButton])
        dangerContents.orientation = .vertical
        dangerContents.alignment = .leading
        dangerContents.spacing = 0
        dangerContents.setCustomSpacing(4, after: dangerTitle)
        dangerContents.setCustomSpacing(11, after: dangerDetail)
        dangerContents.translatesAutoresizingMaskIntoConstraints = false
        let dangerCard = HubDangerCardView()
        dangerCard.identifier = NSUserInterfaceItemIdentifier("hub.service.danger")
        dangerCard.addSubview(dangerContents)
        NSLayoutConstraint.activate([
            dangerContents.leadingAnchor.constraint(equalTo: dangerCard.leadingAnchor, constant: HubMetrics.rowHorizontalInset),
            dangerContents.trailingAnchor.constraint(equalTo: dangerCard.trailingAnchor, constant: -HubMetrics.rowHorizontalInset),
            dangerContents.topAnchor.constraint(equalTo: dangerCard.topAnchor, constant: 12),
            dangerContents.bottomAnchor.constraint(equalTo: dangerCard.bottomAnchor, constant: -12)
        ])
        dangerDetail.widthAnchor.constraint(equalTo: dangerContents.widthAnchor).isActive = true

        dangerCard.isHidden = !controller.allowsServiceRemoval
        removeButton.isHidden = !controller.allowsServiceRemoval
        let body = NSStackView(views: [detailsCard, pendingSurface, lifecycleCard, dangerCard])
        body.orientation = .vertical
        body.alignment = .leading
        body.spacing = 16
        body.translatesAutoresizingMaskIntoConstraints = false
        let scroll = NSScrollView()
        scroll.identifier = NSUserInterfaceItemIdentifier("hub.service.scroll")
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        scroll.translatesAutoresizingMaskIntoConstraints = false
        let document = HubFlippedSurfaceView(fill: .background)
        document.translatesAutoresizingMaskIntoConstraints = false
        document.addSubview(body)
        scroll.documentView = document
        root.addSubview(scroll)
        for view in [detailsCard, pendingSurface, lifecycleCard, dangerCard] {
            view.widthAnchor.constraint(equalTo: body.widthAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: root.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            document.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
            body.leadingAnchor.constraint(equalTo: document.leadingAnchor),
            body.trailingAnchor.constraint(equalTo: document.trailingAnchor),
            body.centerXAnchor.constraint(equalTo: document.centerXAnchor),
            body.topAnchor.constraint(equalTo: document.topAnchor, constant: 0),
            body.bottomAnchor.constraint(equalTo: document.bottomAnchor, constant: 0)
        ])
        updateLifecycleButtons()
        return root
    }

    private func configureButton(_ button: HubActionButton,
                                 symbol: String?,
                                 style: HubButtonStyle,
                                 action: Selector) {
        button.target = self
        button.action = action
        button.hubStyle = style
        button.hubFont = HubTypography.action
        button.image = symbol.flatMap { NSImage(systemSymbolName: $0, accessibilityDescription: button.title) }
        button.imagePosition = symbol == nil ? .noImage : .imageLeading
        button.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        true
    }

    func windowWillClose(_ notification: Notification) {
        onDismiss()
    }

    @objc private func startStopPressed() {
        guard mutationsEnabled, mutationAllowed(), let lifecycleActions else {
            NSSound.beep()
            return
        }
        serviceHealth == .stopped ? lifecycleActions.start() : lifecycleActions.stop()
    }

    @objc private func restartPressed() {
        guard mutationsEnabled, mutationAllowed(), let lifecycleActions else {
            NSSound.beep()
            return
        }
        lifecycleActions.restart()
    }

    /// Asks first (Cancel is the default); then the lifecycle path, with the same
    /// pending surface and gate as Stop.
    @objc private func removePressed() {
        guard mutationsEnabled, mutationAllowed(), let lifecycleActions else {
            NSSound.beep()
            return
        }
        guard confirmationPresenter(Self.removeConfirmation(), .alertFirstButtonReturn)
            == .alertSecondButtonReturn else { return }
        lifecycleActions.remove()
    }

    /// The window only forwards: the main window owns the two confirmations, the gate
    /// and the transition (section 2.3.10.4).
    @objc private func deleteDataPressed() {
        guard mutationsEnabled, mutationAllowed(), let lifecycleActions else {
            NSSound.beep()
            return
        }
        lifecycleActions.deleteHubAndData()
    }

    private func showPending(title: String, detail: String) {
        pendingTitle.stringValue = title
        pendingDetail.stringValue = detail
        pendingProgress.setAccessibilityValue(title)
        pendingSurface.setAccessibilityElement(true)
        pendingSurface.setAccessibilityRole(.group)
        pendingSurface.setAccessibilityLabel(title)
        pendingSurface.setAccessibilityValue(detail)
        pendingSurface.isHidden = false
        pendingProgress.startAnimation(nil)
    }

    private func updateMutationButtons() {
        let enabled = mutationsEnabled
        let available = enabled && lifecycleActions != nil
        deleteDataButton.isEnabled = available
        removeButton.isEnabled = available
        startStopButton.isEnabled = available
        restartButton.isEnabled = available
    }

    private func updateLifecycleButtons() {
        let available = lifecycleActions != nil && serviceHealth != .needsInstall
        lifecycleCard.isHidden = !available
        switch serviceHealth {
        case .stopped:
            lifecycleDetail.stringValue = HubL10n.text("hub.ServiceDetailsWindowController.456.2214", fallback: "The background collector is stopped. Start it to resume collection.")
            startStopButton.title = HubL10n.text("hub.HubDashboardView.119.1362", fallback: "Start Hub")
            startStopButton.image = NSImage(systemSymbolName: "play.fill",
                                            accessibilityDescription: "Start Hub")
            startStopButton.hubStyle = .primary
            restartButton.isHidden = true
        case .running:
            lifecycleDetail.stringValue = HubL10n.text("hub.ServiceDetailsWindowController.463.2218", fallback: "The background collector is running on this Mac.")
            startStopButton.title = HubL10n.text("hub.HubDashboardView.96.1352", fallback: "Stop Hub…")
            startStopButton.image = NSImage(systemSymbolName: "stop.fill",
                                            accessibilityDescription: "Stop Hub")
            startStopButton.hubStyle = .flatDanger
            restartButton.hubStyle = .neutral
            restartButton.isHidden = false
        case .degraded:
            lifecycleDetail.stringValue = HubL10n.text("hub.ServiceDetailsWindowController.471.2222", fallback: "Hub needs attention. Restart it after reviewing diagnostics.")
            startStopButton.title = HubL10n.text("hub.HubDashboardView.96.1352", fallback: "Stop Hub…")
            startStopButton.image = NSImage(systemSymbolName: "stop.fill",
                                            accessibilityDescription: "Stop Hub")
            startStopButton.hubStyle = .flatDanger
            restartButton.hubStyle = .primary
            restartButton.isHidden = false
        case .needsInstall:
            lifecycleDetail.stringValue = HubL10n.text("hub.ServiceDetailsWindowController.479.2226", fallback: "Set up Hub before using service controls.")
            restartButton.isHidden = true
        }
        startStopButton.setAccessibilityHelp(lifecycleDetail.stringValue)
        restartButton.setAccessibilityHelp("Restart the background Hub service.")
        updateMutationButtons()
    }

    /// Remove Background Service: Cancel first and answering Return.
    static func removeConfirmation() -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = HubL10n.text("hub.service.remove.confirm_title", fallback: "Remove the background service?")
        alert.informativeText = HubL10n.text("hub.service.remove.confirm_detail", fallback: "Teslatlas Hub stops and no longer starts at login. Your Hub data, configuration and logs stay. Start Hub sets it up again.")
        alert.addButton(withTitle: HubL10n.text("hub.ImportSheetController.112.1706", fallback: "Cancel"))
        alert.addButton(withTitle: HubL10n.text("hub.service.remove.confirm_button", fallback: "Remove"))
        alert.buttons[0].keyEquivalent = "\r"
        alert.buttons[1].keyEquivalent = ""
        return alert
    }

    /// First confirmation of Delete Hub and Data: Cancel first and answering Return.
    static func deleteHubAndDataExplanation() -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = HubL10n.text("hub.service.delete.explain_title", fallback: "Delete Teslatlas Hub and all its data?")
        alert.informativeText = HubL10n.text("hub.service.delete.explain_detail", fallback: "Teslatlas Hub stops, its background service is removed, and it no longer starts at login. Then the Hub's database, configuration, sign-in details and logs are permanently deleted from this Mac, together with the app's own settings and caches.\n\nBackups and exports saved outside the Hub folder are not touched. If the Hub folder holds a backup, an export or a link to another location, nothing is deleted and the app tells you why.\n\nThe app itself stays. Move it to the Trash afterwards.")
        alert.addButton(withTitle: HubL10n.text("hub.ImportSheetController.112.1706", fallback: "Cancel"))
        alert.addButton(withTitle: HubL10n.text("hub.service.delete.continue", fallback: "Continue…"))
        alert.buttons[0].keyEquivalent = "\r"
        alert.buttons[1].keyEquivalent = ""
        return alert
    }

    /// Second confirmation: kept keys for its text; Cancel first and answering Return.
    static func deleteDataConfirmation() -> NSAlert {
        let confirmation = NSAlert()
        confirmation.alertStyle = .critical
        confirmation.messageText = HubL10n.text("hub.ServiceDetailsWindowController.490.2228", fallback: "Permanently delete Hub data?")
        confirmation.informativeText = HubL10n.text("hub.ServiceDetailsWindowController.491.2229", fallback: "This removes the Hub database and configuration. This cannot be undone.")
        confirmation.addButton(withTitle: HubL10n.text("hub.ImportSheetController.112.1706", fallback: "Cancel"))
        confirmation.addButton(withTitle: HubL10n.text("hub.service.delete.permanently", fallback: "Delete Permanently"))
        confirmation.buttons[0].keyEquivalent = "\r"
        confirmation.buttons[1].keyEquivalent = ""
        return confirmation
    }
}

private final class ServiceDetailRowView: NSView {
    init(detail: HubServiceDetail) {
        super.init(frame: .zero)
        let label = NSTextField(labelWithString: detail.label)
        label.font = HubTypography.emphasis
        label.textColor = HubPalette.foreground
        let value = NSTextField(labelWithString: detail.value)
        value.font = HubTypography.action
        value.textColor = HubPalette.mutedForeground
        value.lineBreakMode = .byTruncatingMiddle
        value.alignment = .right
        value.toolTip = detail.value
        let stack = NSStackView(views: [label, NSView(), value])
        stack.alignment = .centerY
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            label.widthAnchor.constraint(equalToConstant: 104),
            value.widthAnchor.constraint(lessThanOrEqualToConstant: 286),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: HubMetrics.rowHorizontalInset),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -HubMetrics.rowHorizontalInset),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 9),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -9),
            heightAnchor.constraint(equalToConstant: HubMetrics.rowHeight)
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

private final class HubDangerCardView: NSView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.cornerRadius = HubMetrics.cardRadius
        layer?.cornerCurve = .continuous
        layer?.borderWidth = 0.5
        updateLayer()
    }

    override func updateLayer() {
        let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        layer?.backgroundColor = HubPalette.danger.withAlphaComponent(isDark ? 0.15 : 0.05).cgColor
        layer?.borderColor = HubPalette.danger.withAlphaComponent(isDark ? 0.45 : 0.30).cgColor
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        updateLayer()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }
}

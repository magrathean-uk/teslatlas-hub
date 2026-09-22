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
}

final class ServiceDetailsWindowController: NSWindowController, NSWindowDelegate {
    typealias ConfirmationPresenter = (NSAlert, NSApplication.ModalResponse) -> NSApplication.ModalResponse

    private let controller: HubController
    private let lifecycleActions: HubServiceLifecycleActions?
    private let mutationAllowed: () -> Bool
    private let onMutationStateChanged: (Bool) -> Void
    private let onChanged: () -> Void
    private let onDismiss: () -> Void
    private let errorPresenter: (Error) -> Void
    private let confirmationPresenter: ConfirmationPresenter
    private let rowsStack = NSStackView()
    private let lifecycleCard = HubCardView()
    private let lifecycleDetail = NSTextField(wrappingLabelWithString: "")
    private let startStopButton = HubActionButton(title: "Stop Hub…", target: nil, action: nil)
    private let restartButton = HubActionButton(title: "Restart Hub", target: nil, action: nil)
    private let updateButton = HubActionButton(title: "Update Service…", target: nil, action: nil)
    private let uninstallButton = HubActionButton(title: "Uninstall Hub…", target: nil, action: nil)
    private let deleteDataButton = HubActionButton(title: "Delete Hub and Data…", target: nil, action: nil)
    private var mutationsEnabled = true
    private var mutationPending = false
    private var embeddedBody: NSView?
    private var serviceHealth: HubHealth

    init(snapshot: HubSnapshot,
         controller: HubController,
         lifecycleActions: HubServiceLifecycleActions? = nil,
         mutationAllowed: @escaping () -> Bool = { true },
         onMutationStateChanged: @escaping (Bool) -> Void = { _ in },
         onChanged: @escaping () -> Void,
         onDismiss: @escaping () -> Void = {},
         embedded: Bool = false,
         errorPresenter: @escaping (Error) -> Void = HubUIPresentation.presentError,
         confirmationPresenter: @escaping ConfirmationPresenter = { alert, silentResponse in
             HubUIPresentation.response(to: alert, silentResponse: silentResponse)
         }) {
        self.controller = controller
        self.lifecycleActions = lifecycleActions
        self.mutationAllowed = mutationAllowed
        self.onMutationStateChanged = onMutationStateChanged
        self.onChanged = onChanged
        self.onDismiss = onDismiss
        self.errorPresenter = errorPresenter
        self.confirmationPresenter = confirmationPresenter
        self.serviceHealth = snapshot.health
        super.init(window: embedded ? nil : HubUtilityWindowStyle.makeWindow(
            title: "Service Details", size: HubMetrics.serviceDetailsSheetSize,
            minimum: NSSize(width: 450, height: 380)
        ))
        window?.title = "Service Details"
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
            title: "Service details",
            subtitle: "Inspect and manage the local Hub service.",
            body: body,
            onBack: onBack
        )
    }

    static func details(for snapshot: HubSnapshot) -> [HubServiceDetail] {
        [
            .init(label: "Version", value: "Teslatlas Hub \(snapshot.version)"),
            .init(label: "Service", value: snapshot.health == .running ? "Active" : snapshot.service),
            .init(label: "Provider", value: snapshot.provider?.displayName ?? "Not configured"),
            .init(label: "Tesla account", value: snapshot.accountDisplay),
            .init(label: "Database", value: snapshot.database),
            .init(label: "Data folder",
                  value: snapshot.dataDirectory.map {
                      ($0.path as NSString).abbreviatingWithTildeInPath
                  } ?? "Not available")
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

    var mutationInProgress: Bool { mutationPending }

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

        let lifecycleTitle = NSTextField(labelWithString: "Service controls")
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
        let lifecycleButtons = NSStackView(views: [startStopButton, restartButton])
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

        configureButton(uninstallButton, symbol: nil, style: .destructive,
                        action: #selector(uninstallPressed))
        let dangerTitle = NSTextField(labelWithString: "Uninstall Hub")
        dangerTitle.font = .systemFont(ofSize: 13, weight: .semibold)
        dangerTitle.textColor = HubPalette.foreground
        let dangerDetail = NSTextField(wrappingLabelWithString:
            "Stops the service and removes it from this Mac. Your collected data folder is left in place unless you delete it manually.")
        dangerDetail.font = .systemFont(ofSize: 12.5)
        dangerDetail.textColor = HubPalette.mutedForeground
        dangerDetail.maximumNumberOfLines = 0
        let dangerContents = NSStackView(views: [dangerTitle, dangerDetail, uninstallButton])
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

        configureButton(updateButton, symbol: "arrow.down.circle", style: .flatAccent,
                        action: #selector(updateServicePressed))
        configureButton(deleteDataButton, symbol: "trash", style: .flatDanger,
                        action: #selector(deleteDataPressed))
        deleteDataButton.identifier = NSUserInterfaceItemIdentifier("hub.service.delete-data")
        let maintenance = NSStackView(views: [updateButton, deleteDataButton])
        maintenance.orientation = .vertical
        maintenance.alignment = .leading
        maintenance.spacing = 4
        dangerCard.isHidden = !controller.allowsServiceInstallation
        maintenance.isHidden = !controller.allowsServiceInstallation
        let body = NSStackView(views: [detailsCard, lifecycleCard, dangerCard, maintenance])
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
        for view in [detailsCard, lifecycleCard, dangerCard] {
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
        guard !mutationPending else { NSSound.beep(); return false }
        return true
    }

    func windowWillClose(_ notification: Notification) {
        onDismiss()
    }

    @objc private func startStopPressed() {
        guard mutationsEnabled, !mutationPending, mutationAllowed(), let lifecycleActions else {
            NSSound.beep()
            return
        }
        serviceHealth == .stopped ? lifecycleActions.start() : lifecycleActions.stop()
    }

    @objc private func restartPressed() {
        guard mutationsEnabled, !mutationPending, mutationAllowed(), let lifecycleActions else {
            NSSound.beep()
            return
        }
        lifecycleActions.restart()
    }

    @objc private func updateServicePressed() {
        guard beginMutation() else { return }
        controller.installService { [self] result in
            endMutation()
            switch result {
            case .success: onChanged()
            case let .failure(error): errorPresenter(error)
            }
        }
    }

    @objc private func uninstallPressed() {
        let choice = NSAlert()
        choice.alertStyle = .warning
        choice.messageText = "Uninstall Teslatlas Hub?"
        choice.informativeText = "The background service and logs will be removed. Your Hub database and configuration are preserved by default."
        choice.addButton(withTitle: "Uninstall, Keep Data")
        choice.addButton(withTitle: "Delete Data…")
        choice.addButton(withTitle: "Cancel")
        let response = confirmationPresenter(choice, .alertThirdButtonReturn)
        guard response != .alertThirdButtonReturn else { return }

        let deleteData = response == .alertSecondButtonReturn
        if deleteData {
            guard confirmationPresenter(Self.deleteDataConfirmation(), .alertFirstButtonReturn)
                == .alertSecondButtonReturn else { return }
        }
        uninstall(deleteData: deleteData)
    }

    @objc private func deleteDataPressed() {
        guard confirmationPresenter(Self.deleteDataConfirmation(), .alertSecondButtonReturn)
            == .alertSecondButtonReturn else { return }
        uninstall(deleteData: true)
    }

    private func uninstall(deleteData: Bool) {
        guard beginMutation() else { return }
        controller.uninstallService(deleteData: deleteData) { [self] result in
            endMutation()
            switch result {
            case .success:
                onChanged()
                onDismiss()
            case let .failure(error): errorPresenter(error)
            }
        }
    }

    private func beginMutation() -> Bool {
        guard mutationsEnabled, !mutationPending, mutationAllowed() else {
            NSSound.beep()
            return false
        }
        mutationPending = true
        updateMutationButtons()
        onMutationStateChanged(true)
        return true
    }

    private func endMutation() {
        guard mutationPending else { return }
        mutationPending = false
        onMutationStateChanged(false)
        updateMutationButtons()
    }

    private func updateMutationButtons() {
        let enabled = mutationsEnabled && !mutationPending
        updateButton.isEnabled = enabled
        uninstallButton.isEnabled = enabled
        deleteDataButton.isEnabled = enabled
        startStopButton.isEnabled = enabled && lifecycleActions != nil
        restartButton.isEnabled = enabled && lifecycleActions != nil
        window?.standardWindowButton(.closeButton)?.isEnabled = !mutationPending
    }

    private func updateLifecycleButtons() {
        let available = lifecycleActions != nil && serviceHealth != .needsInstall
        lifecycleCard.isHidden = !available
        switch serviceHealth {
        case .stopped:
            lifecycleDetail.stringValue = "The background collector is stopped. Start it to resume collection."
            startStopButton.title = "Start Hub"
            startStopButton.image = NSImage(systemSymbolName: "play.fill",
                                            accessibilityDescription: "Start Hub")
            startStopButton.hubStyle = .primary
            restartButton.isHidden = true
        case .running:
            lifecycleDetail.stringValue = "The background collector is running on this Mac."
            startStopButton.title = "Stop Hub…"
            startStopButton.image = NSImage(systemSymbolName: "stop.fill",
                                            accessibilityDescription: "Stop Hub")
            startStopButton.hubStyle = .flatDanger
            restartButton.hubStyle = .neutral
            restartButton.isHidden = false
        case .degraded:
            lifecycleDetail.stringValue = "Hub needs attention. Restart it after reviewing diagnostics."
            startStopButton.title = "Stop Hub…"
            startStopButton.image = NSImage(systemSymbolName: "stop.fill",
                                            accessibilityDescription: "Stop Hub")
            startStopButton.hubStyle = .flatDanger
            restartButton.hubStyle = .primary
            restartButton.isHidden = false
        case .needsInstall:
            lifecycleDetail.stringValue = "Set up Hub before using service controls."
            restartButton.isHidden = true
        }
        startStopButton.setAccessibilityHelp(lifecycleDetail.stringValue)
        restartButton.setAccessibilityHelp("Restart the background Hub service.")
        updateMutationButtons()
    }

    static func deleteDataConfirmation() -> NSAlert {
        let confirmation = NSAlert()
        confirmation.alertStyle = .critical
        confirmation.messageText = "Permanently delete Hub data?"
        confirmation.informativeText = "This removes the Hub database and configuration. This cannot be undone."
        confirmation.addButton(withTitle: "Cancel")
        confirmation.addButton(withTitle: "Delete Data and Uninstall")
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

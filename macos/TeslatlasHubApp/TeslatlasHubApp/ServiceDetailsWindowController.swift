// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

struct HubServiceDetail: Equatable {
    let label: String
    let value: String
}

final class ServiceDetailsWindowController: NSWindowController, NSWindowDelegate {
    typealias ConfirmationPresenter = (NSAlert, NSApplication.ModalResponse) -> NSApplication.ModalResponse

    private let controller: HubController
    private let mutationAllowed: () -> Bool
    private let onMutationStateChanged: (Bool) -> Void
    private let onChanged: () -> Void
    private let onDismiss: () -> Void
    private let errorPresenter: (Error) -> Void
    private let confirmationPresenter: ConfirmationPresenter
    private let rowsStack = NSStackView()
    private let updateButton = HubActionButton(title: "Update Service…", target: nil, action: nil)
    private let uninstallButton = HubActionButton(title: "Uninstall Hub…", target: nil, action: nil)
    private let deleteDataButton = HubActionButton(title: "Delete Hub and Data…", target: nil, action: nil)
    private var mutationsEnabled = true
    private var mutationPending = false

    init(snapshot: HubSnapshot,
         controller: HubController,
         mutationAllowed: @escaping () -> Bool = { true },
         onMutationStateChanged: @escaping (Bool) -> Void = { _ in },
         onChanged: @escaping () -> Void,
         onDismiss: @escaping () -> Void = {},
         errorPresenter: @escaping (Error) -> Void = HubUIPresentation.presentError,
         confirmationPresenter: @escaping ConfirmationPresenter = { alert, silentResponse in
             HubUIPresentation.response(to: alert, silentResponse: silentResponse)
         }) {
        self.controller = controller
        self.mutationAllowed = mutationAllowed
        self.onMutationStateChanged = onMutationStateChanged
        self.onChanged = onChanged
        self.onDismiss = onDismiss
        self.errorPresenter = errorPresenter
        self.confirmationPresenter = confirmationPresenter
        super.init(window: HubUtilityWindowStyle.makeWindow(title: "Service Details", size: HubMetrics.serviceDetailsSheetSize,
                                                           minimum: NSSize(width: 450, height: 380)))
        window?.title = "Service Details"
        window?.delegate = self
        window?.contentView = contentView()
        update(snapshot: snapshot)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

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
            dangerContents.leadingAnchor.constraint(equalTo: dangerCard.leadingAnchor, constant: 14),
            dangerContents.trailingAnchor.constraint(equalTo: dangerCard.trailingAnchor, constant: -14),
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
        maintenance.isHidden = controller.previewMode
        let body = NSStackView(views: [detailsCard, dangerCard, maintenance])
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
        for view in [detailsCard, dangerCard] {
            view.widthAnchor.constraint(equalTo: body.widthAnchor).isActive = true
        }
        NSLayoutConstraint.activate([
            scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            scroll.topAnchor.constraint(equalTo: root.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: root.bottomAnchor),
            document.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
            body.leadingAnchor.constraint(equalTo: document.leadingAnchor, constant: 14),
            body.trailingAnchor.constraint(equalTo: document.trailingAnchor, constant: -14),
            body.topAnchor.constraint(equalTo: document.topAnchor, constant: 14),
            body.bottomAnchor.constraint(equalTo: document.bottomAnchor, constant: -14)
        ])
        return root
    }

    private func configureButton(_ button: HubActionButton,
                                 symbol: String?,
                                 style: HubButtonStyle,
                                 action: Selector) {
        button.target = self
        button.action = action
        button.hubStyle = style
        button.hubFont = .systemFont(ofSize: 12, weight: .medium)
        button.image = symbol.flatMap { NSImage(systemSymbolName: $0, accessibilityDescription: button.title) }
        button.imagePosition = symbol == nil ? .noImage : .imageLeading
        button.heightAnchor.constraint(equalToConstant: 28).isActive = true
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard !mutationPending else { NSSound.beep(); return false }
        return true
    }

    func windowWillClose(_ notification: Notification) {
        onDismiss()
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
        window?.standardWindowButton(.closeButton)?.isEnabled = !mutationPending
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
        label.font = .systemFont(ofSize: 12.5, weight: .medium)
        label.textColor = HubPalette.foreground
        let value = NSTextField(labelWithString: detail.value)
        value.font = .systemFont(ofSize: 12)
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
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 9),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -9),
            heightAnchor.constraint(equalToConstant: 37)
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

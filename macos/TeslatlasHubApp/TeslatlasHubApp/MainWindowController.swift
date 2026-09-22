// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

final class MainWindowController: NSWindowController, NSWindowDelegate {
    private let controller: HubController
    private let heroDot = NSImageView()
    private let heroTitle = NSTextField(labelWithString: "")
    private let heroSubtitle = NSTextField(labelWithString: "")
    private let heroStateIcon = NSImageView()
    private let heroProgress = NSProgressIndicator()
    private let serviceValue = NSTextField(labelWithString: "")
    private let accountValue = NSTextField(labelWithString: "")
    private let databaseValue = NSTextField(labelWithString: "")
    private let vehicleControlName = NSTextField(labelWithString: "Vehicle")
    private let vehicleControlStatus = NSTextField(labelWithString: "")
    private let vehicleSelector = NSPopUpButton()
    private let serviceDot = NSImageView()
    private let activityStack = NSStackView()
    private let versionLabel = NSTextField(labelWithString: "")
    private let titlebarTitle = NSTextField(labelWithString: "Teslatlas Hub")
    private let stopButton = HubActionButton(title: "Stop Hub", target: nil, action: nil)
    private let restartButton = HubActionButton(title: "Restart", target: nil, action: nil)
    private let installButton = HubActionButton(title: "Set Up Hub", target: nil, action: nil)
    private let heroDiagnosticsButton = HubActionButton(title: "Run Diagnostics", target: nil, action: nil)
    let connectButton = HubActionButton(title: "Connect Tesla", target: nil, action: nil)
    let importButton = HubActionButton(title: "Import", target: nil, action: nil)
    let detailsButton = HubActionButton(title: "Service Details", target: nil, action: nil)
    private var vehicleActionButtons: [NSButton] = []
    private var vehicleControlSectionViews: [NSView] = []
    private var vehicleControlSectionHeightConstraints: [NSLayoutConstraint] = []
    private var vehicleCardHeightConstraint: NSLayoutConstraint?
    private var controlVehicles: [HubControlVehicle] = []
    private var selectedControlVehicleID: UUID?
    private var vehicleControlPending = false
    private var vehicleControlOutcomeUnknown = false
    private(set) var accountWorkflowActive = false
    private var serviceDetailsMutationPending = false
    private var diagnosticsOperationPending = false
    private var mainToolbar: HubMainToolbar?
    private(set) var detailsWindow: ServiceDetailsWindowController?
    private var modalState = HubModalState()
    private var activeModalController: NSWindowController?
    private var logsCloseObserver: NSObjectProtocol?
    private(set) var activeOnboardingIdentifier: UUID?
    private var onInitialRefresh: ((HubSnapshot) -> Void)?
    private var refreshTimer: Timer?
    private var dashboardRefreshToken: UUID?
    private var refreshPending = false
    private var presentationGeneration: UInt64 = 0
    private var serviceTransition: HubServiceTransition?
    private var serviceTransitionToken: UUID?
    private var serviceTransitionDeadlineWorkItem: DispatchWorkItem?
    private let serviceTransitionTimeout: TimeInterval
    private let serviceTransitionPollInterval: TimeInterval
    private let errorPresenter: (Error) -> Void
    private var navigationBar: HubNavigationBar!
    private let pageContainer = NSView()
    private(set) var dashboardView: HubDashboardView!
    private(set) var vehiclesView: HubVehiclesView!
    private(set) var activityPage: HubActivityView!
    private(set) var settingsPage: HubSettingsView!
    private(set) var selectedSection: HubMainSection = .dashboard
    private var embeddedDetailController: NSWindowController?
    private var embeddedDetailView: NSView?
    private var embeddedParentSection: HubMainSection?
    private weak var embeddedReturnResponder: NSView?
    private var displayOptionsObserver: NSObjectProtocol?
    private var appearancePreference = HubAppearancePreference()
    private var sessionActivity = HubSessionActivityStore(limit: 3, now: Date.init)
    private var lastPresentedSnapshot: HubSnapshot

    var activeModalKind: HubModalKind? { modalState.active }

    convenience init(controller: HubController,
                     onInitialRefresh: ((HubSnapshot) -> Void)? = nil) {
        self.init(controller: controller,
                  serviceTransitionTimeout: 60,
                  serviceTransitionPollInterval: 0.2,
                  errorPresenter: HubUIPresentation.presentError,
                  onInitialRefresh: onInitialRefresh)
    }

    init(controller: HubController,
         serviceTransitionTimeout: TimeInterval,
         serviceTransitionPollInterval: TimeInterval,
         errorPresenter: @escaping (Error) -> Void,
         onInitialRefresh: ((HubSnapshot) -> Void)? = nil) {
        self.controller = controller
        self.onInitialRefresh = onInitialRefresh
        self.serviceTransitionTimeout = max(0.001, serviceTransitionTimeout)
        self.serviceTransitionPollInterval = max(0.001, serviceTransitionPollInterval)
        self.errorPresenter = errorPresenter
        self.lastPresentedSnapshot = controller.snapshot
        let window = NSWindow(contentRect: NSRect(origin: .zero, size: HubMetrics.windowSize),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "Teslatlas Hub"
        window.backgroundColor = HubPalette.background
        window.isOpaque = true
        window.minSize = NSSize(width: 820, height: 640)
        super.init(window: window)
        window.delegate = self
        detailsButton.target = self
        detailsButton.action = #selector(detailsPressed)
        connectButton.target = self
        connectButton.action = #selector(connectTeslaPressed)
        configureFlatButton(connectButton, symbol: "person.badge.key")
        importButton.target = self
        importButton.action = #selector(importPressed)
        configureFlatButton(importButton, symbol: "square.and.arrow.down")
        dashboardView = HubDashboardView(actions: makeDashboardActions())
        vehiclesView = HubVehiclesView(actions: makeVehicleActions())
        navigationBar = HubNavigationBar(actions: makeNavigationActions())
        activityPage = HubActivityView(actions: makeNavigationActions())
        settingsPage = HubSettingsView(actions: makeNavigationActions())
        mainToolbar = HubMainToolbar(navigationBar: navigationBar)
        configureTitlebar(window)
        window.contentView = makeContentView()
        window.contentMinSize = NSSize(width: 820, height: 590)
        appearancePreference.apply(to: window)
        displayOptionsObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification,
            object: nil,
            queue: .main
        ) { _ in
            HubAccessibility.refreshDisplayOptions()
        }
        window.center()
        update()
        let refreshTimer = Timer(timeInterval: 15, repeats: true) { [weak self] _ in
            guard let self,
                  NSApp.isActive,
                  self.window?.isVisible == true,
                  self.serviceTransition == nil,
                  !self.accountWorkflowActive,
                  !self.serviceDetailsMutationPending,
                  !self.diagnosticsOperationPending else { return }
            self.update()
        }
        RunLoop.main.add(refreshTimer, forMode: .common)
        self.refreshTimer = refreshTimer
    }

    deinit {
        refreshTimer?.invalidate()
        serviceTransitionDeadlineWorkItem?.cancel()
        if let displayOptionsObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(displayOptionsObserver)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func configureTitlebar(_ window: NSWindow) {
        window.titleVisibility = .visible
        window.toolbar = mainToolbar?.toolbar
        window.toolbarStyle = .unified
        window.titlebarSeparatorStyle = .automatic
    }

    private func makeContentView() -> NSView {
        pageContainer.translatesAutoresizingMaskIntoConstraints = true
        pageContainer.autoresizingMask = [.width, .height]
        dashboardView.translatesAutoresizingMaskIntoConstraints = false
        vehiclesView.translatesAutoresizingMaskIntoConstraints = false
        activityPage.translatesAutoresizingMaskIntoConstraints = false
        settingsPage.translatesAutoresizingMaskIntoConstraints = false
        pageContainer.addSubview(dashboardView)
        pageContainer.addSubview(vehiclesView)
        pageContainer.addSubview(activityPage)
        pageContainer.addSubview(settingsPage)
        NSLayoutConstraint.activate([
            dashboardView.leadingAnchor.constraint(equalTo: pageContainer.leadingAnchor),
            dashboardView.trailingAnchor.constraint(equalTo: pageContainer.trailingAnchor),
            dashboardView.topAnchor.constraint(equalTo: pageContainer.topAnchor),
            dashboardView.bottomAnchor.constraint(equalTo: pageContainer.bottomAnchor),
            vehiclesView.leadingAnchor.constraint(equalTo: pageContainer.leadingAnchor),
            vehiclesView.trailingAnchor.constraint(equalTo: pageContainer.trailingAnchor),
            vehiclesView.topAnchor.constraint(equalTo: pageContainer.topAnchor),
            vehiclesView.bottomAnchor.constraint(equalTo: pageContainer.bottomAnchor),
            activityPage.leadingAnchor.constraint(equalTo: pageContainer.leadingAnchor),
            activityPage.trailingAnchor.constraint(equalTo: pageContainer.trailingAnchor),
            activityPage.topAnchor.constraint(equalTo: pageContainer.topAnchor),
            activityPage.bottomAnchor.constraint(equalTo: pageContainer.bottomAnchor),
            settingsPage.leadingAnchor.constraint(equalTo: pageContainer.leadingAnchor),
            settingsPage.trailingAnchor.constraint(equalTo: pageContainer.trailingAnchor),
            settingsPage.topAnchor.constraint(equalTo: pageContainer.topAnchor),
            settingsPage.bottomAnchor.constraint(equalTo: pageContainer.bottomAnchor)
        ])
        selectMainSection(.dashboard)
        return pageContainer
    }

    private func makeDashboardActions() -> HubDashboardActions {
        HubDashboardActions(
            start: { [weak self] in self?.startPressed() },
            stop: { [weak self] in self?.stopPressed() },
            restart: { [weak self] in self?.restartPressed() },
            setup: { [weak self] in self?.connectTeslaPressed() },
            diagnostics: { [weak self] in self?.diagnosticsPressed() },
            vehicle: HubVehicleCardActions(
                select: { [weak self] id in
                    self?.selectVehicle(id)
                    self?.selectMainSection(.vehicles)
                },
                command: { [weak self] command, id in self?.vehicleCommand(command, vehicleID: id) }
            ),
            serviceDetails: { [weak self] in self?.showEmbeddedServiceDetails() },
            dataFolder: { [weak self] in self?.folderPressed() }
        )
    }

    private func makeVehicleActions() -> HubVehicleCardActions {
        HubVehicleCardActions(
            select: { [weak self] id in self?.selectVehicle(id) },
            command: { [weak self] command, id in self?.vehicleCommand(command, vehicleID: id) }
        )
    }

    private func makeServiceLifecycleActions() -> HubServiceLifecycleActions {
        HubServiceLifecycleActions(
            start: { [weak self] in self?.startPressed() },
            stop: { [weak self] in self?.stopPressed() },
            restart: { [weak self] in self?.restartPressed() }
        )
    }

    private func makeNavigationActions() -> HubNavigationActions {
        HubNavigationActions(
            select: { [weak self] section in self?.selectMainSection(section) },
            diagnostics: { [weak self] in self?.showEmbeddedDiagnostics() },
            logs: { [weak self] in self?.showEmbeddedLogs() },
            serviceDetails: { [weak self] in self?.showEmbeddedServiceDetails() },
            importTeslaMate: { [weak self] in self?.importPressed() },
            connectTesla: { [weak self] in self?.connectTeslaPressed() },
            accountMenu: { [weak self] in self?.teslaAccountMenu() ?? NSMenu() },
            appearance: { [weak self] in self?.appearancePressed() }
        )
    }

    var navigationAvailable: Bool {
        serviceTransition == nil && !vehicleControlPending
            && !accountWorkflowActive && !serviceDetailsMutationPending
            && !diagnosticsOperationPending
    }

    var operationPreventsQuit: Bool {
        serviceTransition != nil || vehicleControlPending
            || serviceDetailsMutationPending || diagnosticsOperationPending
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        guard !operationPreventsQuit else { NSSound.beep(); return false }
        return true
    }

    var canImportTeslaMate: Bool { navigationAvailable && lastPresentedSnapshot.shouldOfferTeslaMateImport }

    func selectMainSection(_ section: HubMainSection) {
        guard navigationAvailable else { return }
        if selectedSection != section || embeddedDetailView != nil {
            HubMotion.transition(pageContainer, forward: embeddedDetailView == nil ? nil : false)
        }
        clearEmbeddedDetail()
        let changed = selectedSection != section
        selectedSection = section
        dashboardView?.isHidden = section != .dashboard
        vehiclesView?.isHidden = section != .vehicles
        activityPage?.isHidden = section != .activity
        settingsPage?.isHidden = section != .settings
        navigationBar?.select(section)
        updateDefaultButton()
        let selectedView: NSView?
        switch section {
        case .dashboard: selectedView = dashboardView
        case .vehicles: selectedView = vehiclesView
        case .activity: selectedView = activityPage
        case .settings: selectedView = settingsPage
        }
        if changed { selectedView?.layoutSubtreeIfNeeded() }
        if changed { navigationBar?.focus(section, in: window) }
    }

    private func updateDefaultButton() {
        let button = selectedSection == .dashboard ? dashboardView?.defaultButton : nil
        window?.defaultButtonCell = button?.isEnabled == true ? button?.cell as? NSButtonCell : nil
    }

    func configurePreviewScene(_ scene: HubPreviewScene) {
        guard controller.previewMode else { return }
        switch scene {
        case .welcome, .choose, .provider, .fleet, .legacy, .migration,
             .migrationConnected, .importing, .verify, .finish, .finishMigration:
            _ = showFirstRunOnboarding()
        case .dashboard:
            selectMainSection(.dashboard)
        case .vehicles:
            selectMainSection(.vehicles)
        case .activity:
            selectMainSection(.activity)
        case .settings:
            selectMainSection(.settings)
        case .diagnostics:
            selectMainSection(.vehicles)
            showEmbeddedDiagnostics()
        case .logs:
            selectMainSection(.activity)
            showEmbeddedLogs()
        case .serviceDetails:
            selectMainSection(.settings)
            showEmbeddedServiceDetails()
        case .manageMenu:
            selectMainSection(.vehicles)
            DispatchQueue.main.async { [weak self] in
                self?.mainToolbar?.showAccountMenuForPreview()
            }
        }
    }

    private func heroView() -> NSView {
        let hero = NSStackView()
        hero.orientation = .vertical
        hero.alignment = .centerX
        hero.spacing = 8

        heroDot.image = NSApplication.shared.applicationIconImage
        heroDot.imageScaling = .scaleProportionallyDown
        heroDot.setAccessibilityLabel("Teslatlas Hub")
        heroDot.widthAnchor.constraint(equalToConstant: 56).isActive = true
        heroDot.heightAnchor.constraint(equalToConstant: 56).isActive = true
        hero.addArrangedSubview(heroDot)
        heroTitle.font = .systemFont(ofSize: 24, weight: .semibold)
        hero.addArrangedSubview(heroTitle)

        let subtitleIndicator = NSView()
        subtitleIndicator.translatesAutoresizingMaskIntoConstraints = false
        heroStateIcon.imageScaling = .scaleProportionallyDown
        heroStateIcon.contentTintColor = .secondaryLabelColor
        heroStateIcon.translatesAutoresizingMaskIntoConstraints = false
        heroProgress.style = .spinning
        heroProgress.controlSize = .small
        heroProgress.isDisplayedWhenStopped = false
        heroProgress.isHidden = true
        heroProgress.translatesAutoresizingMaskIntoConstraints = false
        subtitleIndicator.addSubview(heroStateIcon)
        subtitleIndicator.addSubview(heroProgress)
        NSLayoutConstraint.activate([
            subtitleIndicator.widthAnchor.constraint(equalToConstant: 20),
            subtitleIndicator.heightAnchor.constraint(equalToConstant: 20),
            heroStateIcon.centerXAnchor.constraint(equalTo: subtitleIndicator.centerXAnchor),
            heroStateIcon.centerYAnchor.constraint(equalTo: subtitleIndicator.centerYAnchor),
            heroStateIcon.widthAnchor.constraint(equalToConstant: 20),
            heroStateIcon.heightAnchor.constraint(equalToConstant: 20),
            heroProgress.centerXAnchor.constraint(equalTo: subtitleIndicator.centerXAnchor),
            heroProgress.centerYAnchor.constraint(equalTo: subtitleIndicator.centerYAnchor),
            heroProgress.widthAnchor.constraint(equalToConstant: 20),
            heroProgress.heightAnchor.constraint(equalToConstant: 20)
        ])
        heroSubtitle.font = .systemFont(ofSize: 13)
        heroSubtitle.textColor = .secondaryLabelColor
        let subtitle = NSStackView(views: [subtitleIndicator, heroSubtitle])
        subtitle.spacing = 8
        subtitle.alignment = .centerY
        hero.addArrangedSubview(subtitle)

        let actions = NSStackView(views: [stopButton, restartButton,
                                          heroDiagnosticsButton, installButton])
        actions.spacing = 12
        actions.alignment = .centerY
        actions.setHuggingPriority(.required, for: .horizontal)
        actions.setClippingResistancePriority(.required, for: .horizontal)
        stopButton.target = self
        stopButton.action = #selector(stopPressed)
        configureFlatButton(stopButton, symbol: "stop.fill", tint: .systemRed)
        stopButton.controlSize = .large
        stopButton.widthAnchor.constraint(equalToConstant: 150).isActive = true
        stopButton.heightAnchor.constraint(equalToConstant: 36).isActive = true
        restartButton.target = self
        restartButton.action = #selector(restartPressed)
        configureFlatButton(restartButton, symbol: "arrow.clockwise", tint: .controlAccentColor)
        restartButton.controlSize = .large
        restartButton.widthAnchor.constraint(equalToConstant: 150).isActive = true
        restartButton.heightAnchor.constraint(equalToConstant: 36).isActive = true
        heroDiagnosticsButton.target = self
        heroDiagnosticsButton.action = #selector(diagnosticsPressed)
        configureFlatButton(heroDiagnosticsButton, symbol: "waveform.path.ecg")
        heroDiagnosticsButton.controlSize = .large
        heroDiagnosticsButton.widthAnchor.constraint(equalToConstant: 150).isActive = true
        heroDiagnosticsButton.heightAnchor.constraint(equalToConstant: 36).isActive = true
        installButton.target = self
        installButton.action = #selector(connectTeslaPressed)
        configurePrimaryButton(installButton, symbol: "person.badge.key")
        installButton.controlSize = .large
        installButton.widthAnchor.constraint(equalToConstant: 150).isActive = true
        installButton.heightAnchor.constraint(equalToConstant: 36).isActive = true
        hero.addArrangedSubview(actions)
        return hero
    }

    private func vehicleCardView() -> NSView {
        vehicleControlName.font = .systemFont(ofSize: 16, weight: .semibold)
        vehicleControlStatus.textColor = .secondaryLabelColor
        vehicleControlStatus.font = .systemFont(ofSize: 12)
        vehicleSelector.target = self
        vehicleSelector.action = #selector(vehicleSelectionChanged)
        vehicleSelector.controlSize = .regular
        vehicleSelector.isBordered = false
        vehicleSelector.isHidden = true
        vehicleSelector.widthAnchor.constraint(greaterThanOrEqualToConstant: 180).isActive = true
        let identity = NSStackView(views: [vehicleControlName, vehicleSelector, vehicleControlStatus])
        identity.orientation = .vertical
        identity.alignment = .leading
        identity.spacing = 1
        let vehicleIcon = imageView("car.fill", .secondaryLabelColor)
        vehicleIcon.widthAnchor.constraint(equalToConstant: 32).isActive = true
        vehicleIcon.heightAnchor.constraint(equalToConstant: 32).isActive = true
        let heading = NSStackView(views: [vehicleIcon, identity, spacer()])
        heading.spacing = 10
        heading.alignment = .centerY

        let actions: [(HubVehicleControl, String, String)] = [
            (.climateStart, "Start Climate", "fan.fill"),
            (.climateStop, "Stop Climate", "fan.slash.fill"),
            (.wake, "Wake", "power"),
            (.lock, "Lock", "lock.fill"),
            (.unlock, "Unlock", "lock.open.fill"),
            (.flashLights, "Flash", "light.beacon.max.fill"),
            (.honkHorn, "Honk", "horn.fill")
        ]
        vehicleActionButtons = actions.map { action, title, symbol in
            vehicleActionButton(action, title: title, symbol: symbol)
        }
        let firstRow = NSStackView(views: Array(vehicleActionButtons.prefix(2)))
        let secondRow = NSStackView(views: Array(vehicleActionButtons.dropFirst(2)))
        for row in [firstRow, secondRow] {
            row.spacing = 10
            row.alignment = .centerY
            row.distribution = .fillEqually
        }
        let firstRowHeightConstraint = firstRow.heightAnchor.constraint(equalToConstant: 40)
        let secondRowHeightConstraint = secondRow.heightAnchor.constraint(equalToConstant: 50)
        NSLayoutConstraint.activate([firstRowHeightConstraint, secondRowHeightConstraint])
        for button in vehicleActionButtons.prefix(2) {
            button.heightAnchor.constraint(equalTo: firstRow.heightAnchor).isActive = true
        }
        for button in vehicleActionButtons.dropFirst(2) {
            button.heightAnchor.constraint(equalTo: secondRow.heightAnchor).isActive = true
        }
        let actionSeparator = separator()
        vehicleControlSectionViews = [actionSeparator, firstRow, secondRow]
        vehicleControlSectionHeightConstraints = [firstRowHeightConstraint,
                                                  secondRowHeightConstraint]
            + actionSeparator.constraints.filter {
                $0.firstAttribute == .height && $0.secondItem == nil
            }
        let stack = NSStackView(views: [heading, actionSeparator, firstRow, secondRow])
        stack.orientation = .vertical
        stack.alignment = .centerX
        stack.spacing = 6
        stack.edgeInsets = NSEdgeInsets(top: 12, left: 16, bottom: 12, right: 16)
        for row in [heading, actionSeparator, firstRow, secondRow] {
            row.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32).isActive = true
        }
        let container = NSView()
        stack.translatesAutoresizingMaskIntoConstraints = false
        container.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: container.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            stack.topAnchor.constraint(equalTo: container.topAnchor),
            stack.bottomAnchor.constraint(equalTo: container.bottomAnchor)
        ])
        return container
    }

    private func vehicleActionButton(_ action: HubVehicleControl,
                                     title: String,
                                     symbol: String) -> NSButton {
        let button = compactButton(title, symbol, #selector(vehicleCardButtonPressed(_:)))
        button.identifier = NSUserInterfaceItemIdentifier(action.rawValue)
        button.controlSize = .large
        button.isBordered = false
        button.font = .systemFont(ofSize: 13, weight: .medium)
        button.imagePosition = action == .climateStart || action == .climateStop ? .imageLeading : .imageAbove
        button.imageHugsTitle = true
        return button
    }

    private func statusRows() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.spacing = 0
        let views: [NSView] = [
            statusRow("Service", serviceValue, serviceDot, "gearshape.fill"), separator(),
            statusRow("Tesla account", accountValue, nil, "person.fill"), separator(),
            statusRow("Database", databaseValue, nil, "cylinder.fill")
        ]
        for view in views {
            stack.addArrangedSubview(view)
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        return stack
    }

    private func statusRow(_ name: String, _ value: NSTextField, _ dot: NSImageView?, _ symbol: String) -> NSView {
        statusRow(NSTextField(labelWithString: name), value, dot, symbol)
    }

    private func statusRow(_ name: NSTextField, _ value: NSTextField, _ dot: NSImageView?, _ symbol: String) -> NSView {
        let icon = imageView(symbol, .secondaryLabelColor)
        icon.widthAnchor.constraint(equalToConstant: 22).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 22).isActive = true
        name.font = .systemFont(ofSize: 13, weight: .medium)
        name.widthAnchor.constraint(equalToConstant: 230).isActive = true
        value.font = .systemFont(ofSize: 13)
        var views: [NSView] = [icon, name, value, spacer()]
        if let dot {
            dot.image = NSImage(systemSymbolName: "circle.fill", accessibilityDescription: "Service status")
            dot.setAccessibilityLabel("Service status")
            dot.imageScaling = .scaleProportionallyDown
            dot.widthAnchor.constraint(equalToConstant: 11).isActive = true
            dot.heightAnchor.constraint(equalToConstant: 11).isActive = true
            views.append(dot)
        }
        let row = NSStackView(views: views)
        row.spacing = 10
        row.alignment = .centerY
        row.edgeInsets = NSEdgeInsets(top: 0, left: 18, bottom: 0, right: 16)
        row.heightAnchor.constraint(equalToConstant: 34).isActive = true
        return row
    }

    private func activityView() -> NSView {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 0
        let heading = sectionLabel("Latest activity")
        stack.addArrangedSubview(heading)
        stack.setCustomSpacing(7, after: heading)
        let line = separator()
        stack.addArrangedSubview(line)
        line.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        activityStack.orientation = .vertical
        activityStack.alignment = .leading
        activityStack.spacing = 0
        stack.addArrangedSubview(activityStack)
        activityStack.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        return stack
    }

    private func footerView() -> NSView {
        versionLabel.font = .systemFont(ofSize: 11)
        versionLabel.textColor = .secondaryLabelColor
        detailsButton.target = self
        detailsButton.action = #selector(detailsPressed)
        configureFlatButton(detailsButton, symbol: "info.circle")
        let footer = NSStackView(views: [detailsButton,
                                         compactButton("Run Diagnostics", "waveform.path.ecg", #selector(diagnosticsPressed)),
                                         compactButton("Show Data Folder", "folder", #selector(folderPressed)),
                                         spacer(), versionLabel])
        footer.spacing = 10
        footer.alignment = .centerY
        return footer
    }

    private func update() {
        guard serviceTransition == nil else {
            refreshPending = true
            return
        }
        guard dashboardRefreshToken == nil else {
            refreshPending = true
            return
        }
        let refreshToken = UUID()
        dashboardRefreshToken = refreshToken
        let refreshGeneration = presentationGeneration
        controller.refresh { [weak self] snapshot in
            guard let self else { return }
            defer {
                if self.dashboardRefreshToken == refreshToken {
                    self.dashboardRefreshToken = nil
                    if self.refreshPending {
                        self.refreshPending = false
                        DispatchQueue.main.async { [weak self] in self?.update() }
                    }
                }
            }
            if refreshGeneration != self.presentationGeneration
                || self.serviceTransition != nil {
                if let transition = self.serviceTransition {
                    self.applyServiceTransitionPresentation(transition)
                }
                let callback = self.onInitialRefresh
                self.onInitialRefresh = nil
                if let callback {
                    DispatchQueue.main.async { callback(snapshot) }
                }
                return
            }
            self.applySnapshotPresentation(snapshot)
            let callback = self.onInitialRefresh
            self.onInitialRefresh = nil
            if let callback {
                DispatchQueue.main.async { callback(snapshot) }
            }
        }
    }

    private func applySnapshotPresentation(_ snapshot: HubSnapshot) {
        lastPresentedSnapshot = snapshot
        dashboardView.setInteractionsEnabled(serviceTransition == nil
                                             && !accountWorkflowActive
                                             && !serviceDetailsMutationPending)
        let vehicleControlsEnabled = acceptedVehicleControlsEnabled(for: snapshot)
        dashboardView.setVehicleControlsEnabled(vehicleControlsEnabled)
        dashboardView.apply(snapshot: snapshot, transition: nil, activity: sessionActivity.activities)
        activityPage.apply(snapshot: snapshot, activity: sessionActivity.activities)
        settingsPage.apply(snapshot: snapshot)
        heroDot.image = NSApplication.shared.applicationIconImage
        heroDot.isHidden = false
        heroProgress.stopAnimation(nil)
        heroProgress.isHidden = true
        heroStateIcon.isHidden = false
        heroTitle.stringValue = snapshot.health.title
        switch snapshot.health {
        case .running:
            heroStateIcon.image = NSImage(systemSymbolName: "checkmark.shield",
                                          accessibilityDescription: "Running")
            heroStateIcon.contentTintColor = .secondaryLabelColor
            heroSubtitle.stringValue = "Hub runs in the background. You can close this window."
        case .stopped:
            heroStateIcon.image = NSImage(systemSymbolName: "pause.circle",
                                          accessibilityDescription: "Stopped")
            heroStateIcon.contentTintColor = .secondaryLabelColor
            heroSubtitle.stringValue = "Vehicle data is not being collected."
        case .needsInstall:
            heroStateIcon.image = NSImage(systemSymbolName: "person.badge.key",
                                          accessibilityDescription: "Setup required")
            heroStateIcon.contentTintColor = .secondaryLabelColor
            heroSubtitle.stringValue = "Choose how Hub connects to Tesla."
        case .degraded:
            heroStateIcon.image = NSImage(systemSymbolName: "exclamationmark.triangle",
                                          accessibilityDescription: "Attention needed")
            heroStateIcon.contentTintColor = .systemOrange
            heroSubtitle.stringValue = "Open diagnostics for details."
        }
        serviceValue.stringValue = snapshot.service
        accountValue.stringValue = snapshot.accountDisplay
        updateVehicleSelection(snapshot)
        vehiclesView.apply(snapshot: snapshot,
                           selectedVehicleID: selectedControlVehicleID,
                           enabled: vehicleControlsEnabled
                               && serviceTransition == nil
                               && !accountWorkflowActive
                               && !serviceDetailsMutationPending
                               && !diagnosticsOperationPending)
        databaseValue.stringValue = snapshot.database
        versionLabel.stringValue = HubRelease.bundledVersion
        serviceDot.contentTintColor = snapshot.health.color

        stopButton.isHidden = snapshot.health == .needsInstall
        installButton.isHidden = snapshot.health != .needsInstall
        restartButton.isHidden = snapshot.health != .degraded
        heroDiagnosticsButton.isHidden = snapshot.health != .degraded
        let mutableActionsAvailable = serviceTransition == nil
            && !accountWorkflowActive
            && !serviceDetailsMutationPending
            && !diagnosticsOperationPending
        detailsWindow?.update(snapshot: snapshot)
        detailsWindow?.setMutationsEnabled(mutableActionsAvailable)
        let accountActionsAvailable = mutableActionsAvailable
            && !vehicleControlPending
        stopButton.isEnabled = mutableActionsAvailable
        installButton.isEnabled = mutableActionsAvailable
        restartButton.isEnabled = mutableActionsAvailable
        heroDiagnosticsButton.isEnabled = mutableActionsAvailable
        connectButton.isEnabled = accountActionsAvailable
        importButton.isEnabled = accountActionsAvailable && snapshot.shouldOfferTeslaMateImport
        detailsButton.isEnabled = !accountWorkflowActive
        connectButton.isHidden = false
        importButton.isHidden = !snapshot.shouldOfferTeslaMateImport
        if snapshot.account == "Connected" {
            connectButton.title = "Manage Tesla"
            connectButton.image = NSImage(systemSymbolName: "person.crop.circle.badge.checkmark",
                                          accessibilityDescription: "Manage Tesla")
            connectButton.action = #selector(manageTeslaPressed(_:))
        } else {
            connectButton.title = "Connect Tesla"
            connectButton.image = NSImage(systemSymbolName: "person.badge.key",
                                          accessibilityDescription: "Connect Tesla")
            connectButton.action = #selector(connectTeslaPressed)
        }
        navigationBar.apply(snapshot: snapshot, enabled: accountActionsAvailable)
        mainToolbar?.apply(snapshot: snapshot, enabled: accountActionsAvailable)
        let controlsAvailable = !controller.previewMode
            && snapshot.health == .running
            && snapshot.account == "Connected"
            && selectedControlVehicleID != nil
            && !vehicleControlPending
            && !vehicleControlOutcomeUnknown
            && !accountWorkflowActive
            && !serviceDetailsMutationPending
            && !diagnosticsOperationPending
        let controlsVisible = snapshot.provider == .fleet
        if controlsVisible {
            NSLayoutConstraint.activate(vehicleControlSectionHeightConstraints)
        } else {
            NSLayoutConstraint.deactivate(vehicleControlSectionHeightConstraints)
        }
        vehicleControlSectionViews.forEach { $0.isHidden = !controlsVisible }
        vehicleCardHeightConstraint?.constant = controlsVisible ? 174 : 72
        vehicleActionButtons.forEach {
            $0.isHidden = !controlsVisible
            $0.isEnabled = controlsVisible && (controlsAvailable || controller.previewMode)
        }
        stopButton.keyEquivalent = snapshot.health == .stopped ? "\r" : ""
        installButton.keyEquivalent = snapshot.health == .needsInstall ? "\r" : ""
        heroDiagnosticsButton.keyEquivalent = snapshot.health == .degraded ? "\r" : ""
        switch snapshot.health {
        case .needsInstall:
            window?.defaultButtonCell = installButton.cell as? NSButtonCell
        case .stopped:
            window?.defaultButtonCell = stopButton.cell as? NSButtonCell
        case .degraded:
            window?.defaultButtonCell = heroDiagnosticsButton.cell as? NSButtonCell
        case .running:
            window?.defaultButtonCell = nil
        }
        if snapshot.health == .stopped {
            stopButton.title = "Start Hub"
            stopButton.action = #selector(startPressed)
            configurePrimaryButton(stopButton, symbol: "play.fill")
        } else {
            stopButton.title = "Stop Hub…"
            stopButton.action = #selector(stopPressed)
            configureFlatButton(stopButton, symbol: "stop.fill", tint: .systemRed)
        }
        restartButton.title = "Restart Hub"
        configureFlatButton(restartButton, symbol: "arrow.clockwise",
                            tint: .controlAccentColor)
        heroDiagnosticsButton.title = "Run Diagnostics"
        configureFlatButton(heroDiagnosticsButton, symbol: "waveform.path.ecg")
        installButton.title = "Set Up Hub"
        configurePrimaryButton(installButton, symbol: "person.badge.key")

        activityStack.arrangedSubviews.forEach {
            activityStack.removeArrangedSubview($0)
            $0.removeFromSuperview()
        }
        if snapshot.activity.isEmpty {
            let empty = NSTextField(labelWithString: "No recent app actions. See Logs for collector activity.")
            empty.textColor = .secondaryLabelColor
            empty.heightAnchor.constraint(equalToConstant: 30).isActive = true
            activityStack.addArrangedSubview(empty)
        } else {
            for (index, entry) in snapshot.activity.prefix(3).enumerated() {
                if index > 0 {
                    let line = separator()
                    activityStack.addArrangedSubview(line)
                    line.widthAnchor.constraint(equalTo: activityStack.widthAnchor).isActive = true
                }
                let message = NSTextField(labelWithString: entry.message)
                let age = NSTextField(labelWithString: entry.age)
                age.textColor = .secondaryLabelColor
                let row = NSStackView(views: [message, spacer(), age])
                row.spacing = 9
                row.alignment = .centerY
                row.heightAnchor.constraint(equalToConstant: 30).isActive = true
                activityStack.addArrangedSubview(row)
                row.widthAnchor.constraint(equalTo: activityStack.widthAnchor).isActive = true
            }
        }
        updateDefaultButton()
    }

    private func compactButton(_ title: String, _ symbol: String, _ action: Selector) -> NSButton {
        let button = HubActionButton(title: title, target: self, action: action)
        configureFlatButton(button, symbol: symbol)
        button.controlSize = .regular
        return button
    }

    private func configureFlatButton(_ button: NSButton,
                                     symbol: String,
                                     tint: NSColor = .labelColor) {
        button.isBordered = false
        (button as? HubActionButton)?.hubStyle = .flat
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: button.title)
        button.imagePosition = .imageLeading
        button.imageHugsTitle = true
        button.contentTintColor = .labelColor
        button.font = .systemFont(ofSize: 13, weight: .medium)
        button.attributedTitle = NSAttributedString(
            string: button.title,
            attributes: [
                .foregroundColor: NSColor.labelColor,
                .font: NSFont.systemFont(ofSize: 13, weight: .medium)
            ]
        )
        button.focusRingType = .default
    }

    private func configurePrimaryButton(_ button: NSButton, symbol: String) {
        button.isBordered = false
        (button as? HubActionButton)?.hubStyle = .primary
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: button.title)
        button.imagePosition = .imageLeading
        button.imageHugsTitle = true
        button.contentTintColor = .white
        button.font = .systemFont(ofSize: 13, weight: .semibold)
        button.attributedTitle = NSAttributedString(
            string: button.title,
            attributes: [
                .foregroundColor: NSColor.white,
                .font: NSFont.systemFont(ofSize: 13, weight: .semibold)
            ]
        )
        button.focusRingType = .default
        (button as? HubActionButton)?.updateHubAppearance()
    }

    private func sectionLabel(_ title: String) -> NSTextField {
        let label = NSTextField(labelWithString: title)
        label.font = .systemFont(ofSize: 14, weight: .semibold)
        return label
    }

    private func imageView(_ symbol: String, _ color: NSColor) -> NSImageView {
        let view = NSImageView(image: NSImage(systemSymbolName: symbol, accessibilityDescription: symbol) ?? NSImage())
        view.contentTintColor = color
        view.imageScaling = .scaleProportionallyDown
        view.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 17, weight: .regular)
        return view
    }

    private func updateVehicleSelection(_ snapshot: HubSnapshot) {
        let menuChanged = controlVehicles.map { $0.id.uuidString + "\u{0}" + $0.displayName }
            != snapshot.controlVehicles.map { $0.id.uuidString + "\u{0}" + $0.displayName }
        controlVehicles = snapshot.controlVehicles
        let availableIDs = Set(controlVehicles.map(\.id))
        if let selectedControlVehicleID,
           !availableIDs.isEmpty,
           !availableIDs.contains(selectedControlVehicleID) {
            self.selectedControlVehicleID = nil
        }
        if selectedControlVehicleID == nil {
            selectedControlVehicleID = snapshot.controlVehicleID ?? controlVehicles.first?.id
        }

        if menuChanged {
            vehicleSelector.removeAllItems()
            for vehicle in controlVehicles {
                let item = NSMenuItem(title: vehicle.displayName, action: nil, keyEquivalent: "")
                item.representedObject = vehicle.id.uuidString
                vehicleSelector.menu?.addItem(item)
            }
        }
        if let selectedControlVehicleID,
           let index = controlVehicles.firstIndex(where: { $0.id == selectedControlVehicleID }) {
            vehicleSelector.selectItem(at: index)
        }

        let hasMultipleChoices = controlVehicles.count > 1
        vehicleSelector.isHidden = !hasMultipleChoices
        vehicleControlName.isHidden = hasMultipleChoices
        if let selected = selectedControlVehicle {
            vehicleControlName.stringValue = selected.displayName
            vehicleControlStatus.stringValue = selected.status
        } else {
            vehicleControlName.stringValue = snapshot.vehicleName
            vehicleControlStatus.stringValue = snapshot.vehicle
        }
    }

    private var selectedControlVehicle: HubControlVehicle? {
        guard let selectedControlVehicleID else { return nil }
        return controlVehicles.first { $0.id == selectedControlVehicleID }
    }

    @objc private func vehicleSelectionChanged() {
        guard vehicleSelector.indexOfSelectedItem >= 0,
              let value = vehicleSelector.selectedItem?.representedObject as? String,
              let vehicleID = UUID(uuidString: value),
              let selected = controlVehicles.first(where: { $0.id == vehicleID }) else { return }
        selectedControlVehicleID = vehicleID
        vehicleControlName.stringValue = selected.displayName
        vehicleControlStatus.stringValue = selected.status
    }

    private func selectVehicle(_ vehicleID: UUID) {
        guard controlVehicles.contains(where: { $0.id == vehicleID }) else { return }
        selectedControlVehicleID = vehicleID
        dashboardView.selectVehicle(id: vehicleID)
        vehiclesView.selectVehicle(id: vehicleID)
    }

    func vehicleCommand(_ action: HubVehicleControl, vehicleID: UUID) {
        guard serviceTransition == nil, !accountWorkflowActive,
              !serviceDetailsMutationPending, !diagnosticsOperationPending,
              let vehicle = controlVehicles.first(where: { $0.id == vehicleID }) else { return }
        selectVehicle(vehicleID)
        confirmVehicleControl(action, vehicle: vehicle)
    }

    private func acceptedVehicleControlsEnabled(for snapshot: HubSnapshot) -> Bool {
        Self.acceptedVehicleControlsEnabled(
            for: snapshot,
            serviceTransitionActive: serviceTransition != nil,
            accountWorkflowActive: accountWorkflowActive,
            serviceDetailsMutationPending: serviceDetailsMutationPending,
            vehicleControlPending: vehicleControlPending,
            vehicleControlOutcomeUnknown: vehicleControlOutcomeUnknown
        ) && !diagnosticsOperationPending
    }

    static func acceptedVehicleControlsEnabled(
        for snapshot: HubSnapshot,
        serviceTransitionActive: Bool,
        accountWorkflowActive: Bool,
        serviceDetailsMutationPending: Bool,
        vehicleControlPending: Bool,
        vehicleControlOutcomeUnknown: Bool
    ) -> Bool {
        snapshot.health == .running
            && snapshot.account == "Connected"
            && snapshot.provider == .fleet
            && !serviceTransitionActive
            && !accountWorkflowActive
            && !serviceDetailsMutationPending
            && !vehicleControlPending
            && !vehicleControlOutcomeUnknown
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

    private func showError(_ error: Error) { errorPresenter(error) }

    static func stopHubConfirmation() -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Stop collecting vehicle data?"
        alert.informativeText = "Hub will stop running. Your existing history stays safe, and you can start Hub again anytime."
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: "Stop Hub")
        alert.buttons[0].keyEquivalent = "\r"
        alert.buttons[1].keyEquivalent = ""
        return alert
    }

    private func beginServiceTransition(_ transition: HubServiceTransition) -> UUID? {
        guard serviceTransition == nil else { return nil }
        presentationGeneration &+= 1
        serviceTransition = transition
        let token = UUID()
        serviceTransitionToken = token
        serviceTransitionDeadlineWorkItem?.cancel()
        serviceTransitionDeadlineWorkItem = nil
        detailsWindow?.setMutationsEnabled(false)
        (embeddedDetailView as? HubEmbeddedUtilityPage)?.setNavigationEnabled(false)
        applyServiceTransitionPresentation(transition)
        detailsWindow?.setServiceTransition(transition)
        HubAccessibility.announce(transition.title, from: embeddedDetailView ?? pageContainer)
        return token
    }

    /// The service controller owns the command and any failure cleanup. Do not
    /// let the UI independently expire that transaction: doing so would unlock
    /// a second Start/Restart while the first command can still be booting out
    /// its failed LaunchAgent. This deadline only bounds the status settlement
    /// that begins after the controller has completed successfully.
    private func armServiceTransitionDeadline(_ transition: HubServiceTransition, token: UUID) {
        guard serviceTransition == transition,
              serviceTransitionToken == token else { return }
        serviceTransitionDeadlineWorkItem?.cancel()
        let deadline = DispatchWorkItem { [weak self] in
            self?.serviceTransitionExpired(transition, token: token)
        }
        serviceTransitionDeadlineWorkItem = deadline
        DispatchQueue.main.asyncAfter(deadline: .now() + serviceTransitionTimeout,
                                      execute: deadline)
    }

    private func applyServiceTransitionPresentation(_ transition: HubServiceTransition) {
        dashboardView.setInteractionsEnabled(false)
        dashboardView.setVehicleControlsEnabled(false)
        dashboardView.apply(snapshot: lastPresentedSnapshot,
                            transition: transition,
                            activity: sessionActivity.activities)
        heroDot.isHidden = false
        heroStateIcon.isHidden = true
        heroProgress.isHidden = false
        heroProgress.setAccessibilityLabel(transition.title)
        heroProgress.setAccessibilityValue(transition.subtitle)
        heroProgress.startAnimation(nil)
        heroTitle.stringValue = transition.title
        heroStateIcon.image = NSImage(systemSymbolName: transition.symbol,
                                      accessibilityDescription: transition.title)
        heroStateIcon.contentTintColor = .controlAccentColor
        heroSubtitle.stringValue = transition.subtitle
        serviceValue.stringValue = transition.service
        serviceDot.contentTintColor = .controlAccentColor
        stopButton.isHidden = true
        restartButton.isHidden = true
        installButton.isHidden = true
        heroDiagnosticsButton.isHidden = true
        stopButton.keyEquivalent = ""
        installButton.keyEquivalent = ""
        heroDiagnosticsButton.keyEquivalent = ""
        window?.defaultButtonCell = nil
        connectButton.isEnabled = false
        importButton.isEnabled = false
        navigationBar.apply(snapshot: lastPresentedSnapshot, enabled: false)
        mainToolbar?.apply(snapshot: lastPresentedSnapshot, enabled: false)
        vehiclesView.apply(snapshot: lastPresentedSnapshot,
                           selectedVehicleID: selectedControlVehicleID,
                           enabled: false)
        detailsButton.isEnabled = false
        vehicleActionButtons.forEach { $0.isEnabled = false }
    }

    private func serviceCommandFailed(_ error: Error,
                                      transition: HubServiceTransition,
                                      token: UUID) {
        guard serviceTransition == transition,
              serviceTransitionToken == token else { return }
        finishServiceTransition(with: controller.snapshot,
                                transition: transition,
                                token: token,
                                announceCompletion: false)
        HubAccessibility.announce(
            "\(transition.title.replacingOccurrences(of: "…", with: "")) failed: \(error.localizedDescription)",
            from: embeddedDetailView ?? pageContainer
        )
        showError(error)
    }

    private func settleServiceTransition(_ transition: HubServiceTransition,
                                         expectedHealth: HubHealth,
                                         token: UUID) {
        guard serviceTransition == transition,
              serviceTransitionToken == token else { return }
        armServiceTransitionDeadline(transition, token: token)
        probeServiceTransition(transition, expectedHealth: expectedHealth, token: token)
    }

    private func probeServiceTransition(_ transition: HubServiceTransition,
                                        expectedHealth: HubHealth,
                                        token: UUID) {
        guard serviceTransition == transition,
              serviceTransitionToken == token else { return }
        controller.refresh { [weak self] snapshot in
            guard let self,
                  self.serviceTransition == transition,
                  self.serviceTransitionToken == token else { return }
            if snapshot.health == expectedHealth {
                self.finishServiceTransition(with: snapshot,
                                             transition: transition,
                                             token: token)
                return
            }
            self.applyServiceTransitionPresentation(transition)
            DispatchQueue.main.asyncAfter(deadline: .now() + self.serviceTransitionPollInterval) {
                [weak self] in
                self?.probeServiceTransition(transition,
                                             expectedHealth: expectedHealth,
                                             token: token)
            }
        }
    }

    private func serviceTransitionExpired(_ transition: HubServiceTransition, token: UUID) {
        guard serviceTransition == transition,
              serviceTransitionToken == token else { return }
        finishServiceTransition(with: controller.snapshot,
                                transition: transition,
                                token: token,
                                announceCompletion: false)
        let action = transition == .stopping ? "stop" : "start"
        let error = HubActionError.commandFailed(
            "Hub did not finish the \(action) operation. Its current status is shown; open diagnostics for details."
        )
        HubAccessibility.announce(error.localizedDescription, from: embeddedDetailView ?? pageContainer)
        showError(error)
    }

    private func finishServiceTransition(with snapshot: HubSnapshot,
                                         transition: HubServiceTransition,
                                         token: UUID,
                                         announceCompletion: Bool = true) {
        guard serviceTransition == transition,
              serviceTransitionToken == token else { return }
        serviceTransitionDeadlineWorkItem?.cancel()
        serviceTransitionDeadlineWorkItem = nil
        serviceTransitionToken = nil
        serviceTransition = nil
        dashboardRefreshToken = nil
        refreshPending = false
        detailsWindow?.setMutationsEnabled(!accountWorkflowActive
                                           && !serviceDetailsMutationPending)
        detailsWindow?.setServiceTransition(nil)
        (embeddedDetailView as? HubEmbeddedUtilityPage)?.setNavigationEnabled(
            !accountWorkflowActive && !serviceDetailsMutationPending
        )
        applySnapshotPresentation(snapshot)
        if announceCompletion {
            HubAccessibility.announce(transition.completionAnnouncement,
                                      from: embeddedDetailView ?? pageContainer)
        }
    }

    func settleStartedHubFromOnboarding() {
        guard serviceTransition == nil else { return }
        guard let token = beginServiceTransition(.starting) else { return }
        armServiceTransitionDeadline(.starting, token: token)
        DispatchQueue.main.asyncAfter(deadline: .now() + serviceTransitionPollInterval) {
            [weak self] in
            self?.probeServiceTransition(.starting, expectedHealth: .running, token: token)
        }
    }

    static func vehicleControlConfirmation(_ action: HubVehicleControl,
                                           vehicleName: String) -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "\(action.title) for \(vehicleName)?"
        alert.informativeText = "Teslatlas Hub will send this command once."
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: action.title)
        return alert
    }

    static func vehicleControlOutcomeIsUnknown(_ error: Error) -> Bool {
        if let actionError = error as? HubActionError,
           case .commandTimedOut = actionError {
            return true
        }
        let message = error.localizedDescription.lowercased()
        return message.contains("timed out") || message.contains("outcome is ambiguous")
    }

    static func unknownVehicleControlOutcomeAlert() -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Command outcome unknown"
        alert.informativeText = "Check the vehicle. Do not repeat the command from this app session."
        return alert
    }

    private func confirmVehicleControl(_ action: HubVehicleControl, vehicle: HubControlVehicle) {
        guard let window, !vehicleControlPending else { return }
        let alert = Self.vehicleControlConfirmation(action, vehicleName: vehicle.displayName)
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertSecondButtonReturn else { return }
            self?.runVehicleControl(action, vehicleID: vehicle.id, vehicleName: vehicle.displayName)
        }
    }

    private func runVehicleControl(_ action: HubVehicleControl,
                                   vehicleID: UUID,
                                   vehicleName: String) {
        guard !vehicleControlPending,
              controlVehicles.contains(where: { $0.id == vehicleID }) else {
            showError(HubActionError.commandFailed("The selected vehicle is no longer configured."))
            return
        }
        vehicleControlPending = true
        dashboardView.setVehicleControlsEnabled(false)
        dashboardView.apply(snapshot: controller.snapshot, transition: nil, activity: sessionActivity.activities)
        connectButton.isEnabled = false
        importButton.isEnabled = false
        navigationBar.apply(snapshot: lastPresentedSnapshot, enabled: false)
        mainToolbar?.apply(snapshot: lastPresentedSnapshot, enabled: false)
        vehiclesView.apply(snapshot: lastPresentedSnapshot,
                           selectedVehicleID: selectedControlVehicleID,
                           enabled: false)
        vehicleActionButtons.forEach { $0.isEnabled = false }
        HubAccessibility.announce("\(action.title) for \(vehicleName) is pending.",
                                  from: selectedSection == .vehicles ? vehiclesView : dashboardView)
        controller.performVehicleControl(action, vehicleID: vehicleID) { [weak self] result in
            guard let self else { return }
            self.vehicleControlPending = false
            switch result {
            case .success:
                self.sessionActivity.record(.vehicleCommandAccepted(action,
                                                                    vehicle: vehicleName))
                self.update()
                HubAccessibility.announce("\(action.title) accepted for \(vehicleName).",
                                          from: self.selectedSection == .vehicles
                                              ? self.vehiclesView : self.dashboardView)
                let accepted = NSAlert()
                accepted.messageText = "Command accepted"
                accepted.informativeText = action.acceptedMessage
                HubUIPresentation.presentInformation(accepted)
            case let .failure(error):
                if Self.vehicleControlOutcomeIsUnknown(error) {
                    self.vehicleControlOutcomeUnknown = true
                    self.update()
                    HubAccessibility.announce(
                        "\(action.title) outcome is unknown. Check \(vehicleName) before trying again.",
                        from: self.selectedSection == .vehicles ? self.vehiclesView : self.dashboardView
                    )
                    HubUIPresentation.presentInformation(Self.unknownVehicleControlOutcomeAlert())
                    return
                }
                self.update()
                HubAccessibility.announce("\(action.title) failed: \(error.localizedDescription)",
                                          from: self.selectedSection == .vehicles
                                              ? self.vehiclesView : self.dashboardView)
                self.showError(error)
            }
        }
    }

    @objc private func importPressed() {
        guard lastPresentedSnapshot.shouldOfferTeslaMateImport else { return }
        showOnboarding(route: .migration)
    }

    func showImport() { importPressed() }

    func toggleSidebar() {}

    @objc private func appearancePressed() {
        let isDark = window?.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        _ = appearancePreference.toggle(currentIsDark: isDark)
        if let window {
            if let content = window.contentView { HubMotion.transition(content) }
            appearancePreference.apply(to: window)
        }
    }

    @objc private func logsPressed() {
        showEmbeddedLogs()
    }

    func showEmbeddedLogs() {
        guard canPresentEmbeddedDetail else { NSSound.beep(); return }
        let logs = LogsWindowController(controller: controller, embedded: true)
        let page = logs.makeEmbeddedPage { [weak self] in self?.dismissEmbeddedDetail() }
        presentEmbeddedDetail(logs, page: page, from: .activity)
    }

    func showEmbeddedDiagnostics() {
        guard canPresentEmbeddedDetail else { NSSound.beep(); return }
        let diagnostics = DiagnosticsWindowController(
            controller: controller,
            embedded: true,
            onOperationStateChanged: { [weak self] pending in
                self?.setDiagnosticsOperationPending(pending)
            }
        )
        let page = diagnostics.makeEmbeddedPage { [weak self] in self?.dismissEmbeddedDetail() }
        presentEmbeddedDetail(diagnostics, page: page, from: .settings)
    }

    private func setDiagnosticsOperationPending(_ pending: Bool) {
        diagnosticsOperationPending = pending
        applySnapshotPresentation(lastPresentedSnapshot)
        (embeddedDetailView as? HubEmbeddedUtilityPage)?.setNavigationEnabled(!pending)
    }

    func showEmbeddedServiceDetails() {
        guard canPresentEmbeddedDetail else { NSSound.beep(); return }
        let details = ServiceDetailsWindowController(
            snapshot: controller.snapshot,
            controller: controller,
            lifecycleActions: makeServiceLifecycleActions(),
            mutationAllowed: { [weak self] in
                guard let self else { return false }
                return self.serviceTransition == nil
                    && !self.accountWorkflowActive && !self.serviceDetailsMutationPending
                    && !self.diagnosticsOperationPending
            },
            onMutationStateChanged: { [weak self] in
                self?.setServiceDetailsMutationPending($0)
            },
            onChanged: { [weak self] in self?.update() },
            onDismiss: { [weak self] in self?.dismissEmbeddedDetail() },
            embedded: true
        )
        details.setMutationsEnabled(serviceTransition == nil
                                    && !accountWorkflowActive && !serviceDetailsMutationPending
                                    && !diagnosticsOperationPending)
        detailsWindow = details
        let page = details.makeEmbeddedPage { [weak self] in self?.dismissEmbeddedDetail() }
        presentEmbeddedDetail(details, page: page, from: .settings)
    }

    private var canPresentEmbeddedDetail: Bool {
        navigationAvailable && activeModalKind == nil
    }

    private func presentEmbeddedDetail(_ controller: NSWindowController,
                                       page: NSView,
                                       from section: HubMainSection) {
        HubMotion.transition(pageContainer, forward: true)
        embeddedReturnResponder = normalizedResponderView()
        clearEmbeddedDetail()
        selectedSection = section
        embeddedParentSection = section
        embeddedDetailController = controller
        embeddedDetailView = page
        [dashboardView, vehiclesView, activityPage, settingsPage].forEach { $0?.isHidden = true }
        page.translatesAutoresizingMaskIntoConstraints = false
        pageContainer.addSubview(page)
        NSLayoutConstraint.activate([
            page.leadingAnchor.constraint(equalTo: pageContainer.leadingAnchor),
            page.trailingAnchor.constraint(equalTo: pageContainer.trailingAnchor),
            page.topAnchor.constraint(equalTo: pageContainer.topAnchor),
            page.bottomAnchor.constraint(equalTo: pageContainer.bottomAnchor)
        ])
        navigationBar.select(section)
        window?.defaultButtonCell = nil
        page.layoutSubtreeIfNeeded()
        (page as? HubEmbeddedUtilityPage)?.focusInitialResponder(in: window)
    }

    private func dismissEmbeddedDetail() {
        guard navigationAvailable else { return }
        let section = embeddedParentSection ?? selectedSection
        let returnResponder = embeddedReturnResponder
        selectMainSection(section)
        embeddedReturnResponder = nil
        if let returnResponder, returnResponder.window === window,
           window?.makeFirstResponder(returnResponder) == true {
            window?.initialFirstResponder = returnResponder
        } else {
            navigationBar.focus(section, in: window)
        }
    }

    private func normalizedResponderView() -> NSView? {
        if let editor = window?.firstResponder as? NSTextView,
           let delegate = editor.delegate as? NSView {
            return delegate
        }
        return window?.firstResponder as? NSView
    }

    private func clearEmbeddedDetail() {
        embeddedDetailView?.removeFromSuperview()
        if detailsWindow === embeddedDetailController { detailsWindow = nil }
        embeddedDetailView = nil
        embeddedDetailController = nil
        embeddedParentSection = nil
    }

    @discardableResult
    func showLogs() -> LogsWindowController? {
        if let logs = activeModalController as? LogsWindowController,
           modalState.active == .logs {
            logs.window?.makeKeyAndOrderFront(nil)
            return logs
        }
        guard let logs = presentPrimaryModal(kind: .logs, controller: {
            LogsWindowController(controller: self.controller)
        }) as? LogsWindowController, let logsWindow = logs.window else { return nil }
        removeLogsCloseObserver()
        logsCloseObserver = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: logsWindow, queue: .main
        ) { [weak self, weak logs] _ in
            guard let self, let logs,
                  self.activeModalController === logs,
                  self.modalState.active == .logs else { return }
            self.removeLogsCloseObserver()
            self.dismissPrimaryModal(kind: .logs)
        }
        return logs
    }

    @objc private func connectTeslaPressed() {
        showOnboarding(route: .provider)
    }

    @objc private func manageTeslaPressed(_ sender: NSButton) {
        let menu = teslaAccountMenu()
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: sender.bounds.height + 4), in: sender)
    }

    private func teslaAccountMenu() -> NSMenu {
        let menu = NSMenu(title: "Tesla account")
        if lastPresentedSnapshot.account == "Connected" {
            menu.addItem(menuItem("Use Fleet API", action: #selector(useFleetPressed)))
            menu.addItem(menuItem("Use Legacy token", action: #selector(useLegacyPressed)))
            menu.addItem(menuItem("Migrate from TeslaMate…", action: #selector(migrateTeslaMatePressed)))
            menu.addItem(.separator())
            menu.addItem(menuItem("Disconnect Tesla…", action: #selector(disconnectTeslaPressed)))
        } else {
            menu.addItem(menuItem("Connect Tesla Account…", action: #selector(connectTeslaPressed)))
            menu.addItem(menuItem("Migrate from TeslaMate…", action: #selector(migrateTeslaMatePressed)))
        }
        return menu
    }

    private func menuItem(_ title: String, action: Selector) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        return item
    }

    @objc private func useFleetPressed() { showOnboarding(route: .fleet) }

    @objc private func useLegacyPressed() { showOnboarding(route: .legacy) }

    @objc private func migrateTeslaMatePressed() { showOnboarding(route: .migration) }

    @objc private func disconnectTeslaPressed() {
        let alert = Self.disconnectConfirmation()
        guard HubUIPresentation.response(to: alert) == .alertSecondButtonReturn else { return }
        controller.signOutTeslaAccount { [weak self] result in
            switch result {
            case .success: self?.update()
            case let .failure(error): self?.showError(error)
            }
        }
    }

    @discardableResult
    func showFirstRunOnboarding() -> OnboardingWindowController? {
        showOnboarding(route: .welcome,
                       dismissalPolicy: .firstRun,
                       previewRoute: controller.onboardingPreviewRoute)
    }

    @discardableResult
    func showOnboarding(route: HubOnboardingRoute,
                        dismissalPolicy: HubOnboardingDismissalPolicy = .accountManagement,
                        previewRoute: String? = nil)
        -> OnboardingWindowController? {
        guard serviceTransition == nil, !vehicleControlPending,
              !serviceDetailsMutationPending else {
            NSSound.beep()
            return nil
        }
        if let onboarding = activeModalController as? OnboardingWindowController,
           modalState.active == .onboarding {
            if onboarding.dismissalPolicy == .firstRun || onboarding.dismissalPolicy == dismissalPolicy {
                setAccountWorkflowActive(true)
                if previewRoute == nil { onboarding.navigate(to: route) }
                onboarding.window?.makeKey()
                return onboarding
            }
            guard let identifier = activeOnboardingIdentifier else { return nil }
            _ = terminateOnboarding(identifier: identifier, closeWindow: true)
        }
        let identifier = UUID()
        let modal = presentPrimaryModal(kind: .onboarding) {
            return OnboardingWindowController(
                controller: self.controller,
                resumeMigrationHandoverPhase: self.controller.pendingMigrationHandoverPhase,
                initialRoute: route,
                previewRoute: previewRoute,
                dismissalPolicy: dismissalPolicy,
                closesWindowOnCancel: !HubUIPresentation.isSilentTestHost,
                onDismiss: { [weak self] in
                    self?.handleOnboardingDismissal(identifier: identifier)
                }
            ) { [weak self] completion in
                self?.completeOnboarding(identifier: identifier, completion: completion)
            }
        }
        guard let onboarding = modal as? OnboardingWindowController else {
            return nil
        }
        activeOnboardingIdentifier = identifier
        setAccountWorkflowActive(true)
        if previewRoute == nil {
            onboarding.navigate(to: route)
        }
        NSApp.activate(ignoringOtherApps: true)
        return onboarding
    }

    func handleOnboardingDismissal(identifier: UUID) {
        _ = terminateOnboarding(identifier: identifier, closeWindow: false)
    }

    func completeOnboarding(identifier: UUID, completion: HubOnboardingCompletion) {
        guard terminateOnboarding(identifier: identifier,
                                 closeWindow: true,
                                 refreshAfterDeactivation: completion != .hubStarted) else { return }
        applySnapshotPresentation(controller.snapshot)
        if completion == .hubStarted {
            settleStartedHubFromOnboarding()
        }
    }

    @discardableResult
    private func terminateOnboarding(identifier: UUID,
                                    closeWindow: Bool,
                                    refreshAfterDeactivation: Bool = true) -> Bool {
        guard activeOnboardingIdentifier == identifier,
              modalState.active == .onboarding else { return false }
        let sheet = activeModalController?.window
        activeOnboardingIdentifier = nil
        setAccountWorkflowActive(false, refreshOnDeactivation: refreshAfterDeactivation)
        dismissPrimaryModal(kind: .onboarding)
        if closeWindow { sheet?.close() }
        return true
    }

    private func presentPrimaryModal(kind: HubModalKind,
                                     controller make: () -> NSWindowController) -> NSWindowController? {
        guard let parent = window else { return nil }
        if modalState.active == kind {
            activeModalController?.window?.makeKeyAndOrderFront(nil)
            return activeModalController
        }
        guard dismissActivePrimaryModalForReplacement() else { return nil }
        guard case .present = modalState.request(kind) else { return nil }
        let controller = make()
        guard let sheet = controller.window else { return nil }
        activeModalController = controller
        if kind == .onboarding {
            sheet.appearance = parent.appearance
            sheet.setFrame(parent.frame, display: false)
            parent.orderOut(nil)
            if !HubUIPresentation.isSilentTestHost { controller.showWindow(nil) }
            sheet.makeKeyAndOrderFront(nil)
        } else {
            sheet.appearance = parent.appearance
            let origin = NSPoint(x: parent.frame.midX - sheet.frame.width / 2,
                                 y: parent.frame.midY - sheet.frame.height / 2)
            sheet.setFrameOrigin(origin)
            if !HubUIPresentation.isSilentTestHost { controller.showWindow(nil) }
        }
        return controller
    }

    private func dismissActivePrimaryModalForReplacement() -> Bool {
        guard let activeKind = modalState.active else { return true }
        guard let activeController = activeModalController else { return false }
        switch (activeKind, activeController) {
        case let (.onboarding, onboarding as OnboardingWindowController):
            guard onboarding.dismissalPolicy == .accountManagement,
                  let onboardingWindow = onboarding.window,
                  onboarding.windowShouldClose(onboardingWindow),
                  let identifier = activeOnboardingIdentifier else { return false }
            return terminateOnboarding(identifier: identifier, closeWindow: true)
        case (.logs, _):
            removeLogsCloseObserver()
        case (.diagnostics, _):
            break
        case let (.serviceDetails, details as ServiceDetailsWindowController):
            guard !details.mutationInProgress else { return false }
            let oldWindow = details.window
            detailsWindow = nil
            dismissPrimaryModal(kind: .serviceDetails)
            oldWindow?.close()
            return true
        default:
            return false
        }
        let sheet = activeController.window
        dismissPrimaryModal(kind: activeKind)
        sheet?.close()
        return true
    }

    private func dismissPrimaryModal(kind: HubModalKind) {
        guard modalState.active == kind else { return }
        let sheet = activeModalController?.window
        activeModalController = nil
        modalState.dismiss(kind)
        if let sheet, let parent = window {
            if sheet.sheetParent === parent { parent.endSheet(sheet) }
            if kind != .onboarding || !HubUIPresentation.isSilentTestHost {
                sheet.orderOut(nil)
            }
            if kind == .onboarding, !HubUIPresentation.isSilentTestHost {
                parent.makeKeyAndOrderFront(nil)
            }
        }
    }

    private func removeLogsCloseObserver() {
        if let logsCloseObserver { NotificationCenter.default.removeObserver(logsCloseObserver) }
        logsCloseObserver = nil
    }

    private func setAccountWorkflowActive(_ active: Bool,
                                          refreshOnDeactivation: Bool = true) {
        accountWorkflowActive = active
        let mutableActionsAvailable = serviceTransition == nil
            && !active && !serviceDetailsMutationPending
        stopButton.isEnabled = mutableActionsAvailable
        restartButton.isEnabled = mutableActionsAvailable
        installButton.isEnabled = mutableActionsAvailable
        heroDiagnosticsButton.isEnabled = mutableActionsAvailable
        connectButton.isEnabled = mutableActionsAvailable
        importButton.isEnabled = mutableActionsAvailable
            && lastPresentedSnapshot.shouldOfferTeslaMateImport
        navigationBar.apply(snapshot: lastPresentedSnapshot,
                            enabled: mutableActionsAvailable && !vehicleControlPending)
        mainToolbar?.apply(snapshot: lastPresentedSnapshot,
                           enabled: mutableActionsAvailable && !vehicleControlPending)
        let vehicleControlsEnabled = acceptedVehicleControlsEnabled(for: lastPresentedSnapshot)
        vehiclesView.apply(snapshot: lastPresentedSnapshot,
                           selectedVehicleID: selectedControlVehicleID,
                           enabled: vehicleControlsEnabled)
        detailsButton.isEnabled = !active
        detailsWindow?.setMutationsEnabled(!active)
        dashboardView.setInteractionsEnabled(mutableActionsAvailable)
        dashboardView.setVehicleControlsEnabled(vehicleControlsEnabled)
        dashboardView.apply(snapshot: lastPresentedSnapshot,
                            transition: serviceTransition,
                            activity: sessionActivity.activities)
        if active {
            vehicleActionButtons.forEach { $0.isEnabled = false }
        } else if refreshOnDeactivation {
            update()
        }
    }

    private func setServiceDetailsMutationPending(_ pending: Bool) {
        serviceDetailsMutationPending = pending
        (embeddedDetailView as? HubEmbeddedUtilityPage)?.setNavigationEnabled(!pending)
        let mutableActionsAvailable = serviceTransition == nil
            && !pending && !accountWorkflowActive
        stopButton.isEnabled = mutableActionsAvailable
        restartButton.isEnabled = mutableActionsAvailable
        installButton.isEnabled = mutableActionsAvailable
        heroDiagnosticsButton.isEnabled = mutableActionsAvailable
        connectButton.isEnabled = mutableActionsAvailable && !vehicleControlPending
        importButton.isEnabled = mutableActionsAvailable && !vehicleControlPending
            && lastPresentedSnapshot.shouldOfferTeslaMateImport
        navigationBar.apply(snapshot: lastPresentedSnapshot,
                            enabled: mutableActionsAvailable && !vehicleControlPending)
        mainToolbar?.apply(snapshot: lastPresentedSnapshot,
                           enabled: mutableActionsAvailable && !vehicleControlPending)
        let vehicleControlsEnabled = acceptedVehicleControlsEnabled(for: lastPresentedSnapshot)
        vehiclesView.apply(snapshot: lastPresentedSnapshot,
                           selectedVehicleID: selectedControlVehicleID,
                           enabled: vehicleControlsEnabled)
        dashboardView.setInteractionsEnabled(mutableActionsAvailable)
        dashboardView.setVehicleControlsEnabled(vehicleControlsEnabled)
        dashboardView.apply(snapshot: lastPresentedSnapshot,
                            transition: serviceTransition,
                            activity: sessionActivity.activities)
        if pending {
            vehicleActionButtons.forEach { $0.isEnabled = false }
        } else {
            update()
        }
    }

    static func disconnectConfirmation() -> NSAlert {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Disconnect Tesla from Hub?"
        alert.informativeText = "Hub will stop and remove its stored Fleet and Legacy credentials. Your collected data stays on this Mac."
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: "Disconnect")
        alert.buttons[0].keyEquivalent = "\r"
        alert.buttons[1].keyEquivalent = ""
        return alert
    }

    @objc private func startPressed() {
        guard serviceTransition == nil, !accountWorkflowActive,
              !serviceDetailsMutationPending, !diagnosticsOperationPending else { return }
        guard let token = beginServiceTransition(.starting) else { return }
        controller.startHub { [weak self] result in
            switch result {
            case .success:
                self?.sessionActivity.record(.hubStarted)
                self?.settleServiceTransition(.starting, expectedHealth: .running, token: token)
            case let .failure(error):
                self?.serviceCommandFailed(error, transition: .starting, token: token)
            }
        }
    }

    @objc private func stopPressed() {
        guard serviceTransition == nil, !accountWorkflowActive,
              !serviceDetailsMutationPending, !diagnosticsOperationPending, let window else { return }
        let alert = Self.stopHubConfirmation()
        alert.beginSheetModal(for: window) { [weak self] response in
            guard response == .alertSecondButtonReturn else { return }
            self?.stopHubAfterConfirmation()
        }
    }

    private func stopHubAfterConfirmation() {
        guard serviceTransition == nil else { return }
        guard let token = beginServiceTransition(.stopping) else { return }
        controller.stopHub { [weak self] result in
            switch result {
            case .success:
                self?.sessionActivity.record(.hubStopped)
                self?.settleServiceTransition(.stopping, expectedHealth: .stopped, token: token)
            case let .failure(error):
                self?.serviceCommandFailed(error, transition: .stopping, token: token)
            }
        }
    }

    @objc private func restartPressed() {
        guard serviceTransition == nil, !accountWorkflowActive,
              !serviceDetailsMutationPending, !diagnosticsOperationPending else { return }
        guard let token = beginServiceTransition(.restarting) else { return }
        controller.restartHub { [weak self] result in
            switch result {
            case .success:
                self?.sessionActivity.record(.hubRestarted)
                self?.settleServiceTransition(.restarting, expectedHealth: .running, token: token)
            case let .failure(error):
                self?.serviceCommandFailed(error, transition: .restarting, token: token)
            }
        }
    }

    @objc private func vehicleCardButtonPressed(_ sender: NSButton) {
        guard serviceTransition == nil, !accountWorkflowActive,
              !serviceDetailsMutationPending, !diagnosticsOperationPending else { return }
        guard let rawValue = sender.identifier?.rawValue,
              let action = HubVehicleControl(rawValue: rawValue) else { return }
        guard let selectedControlVehicleID else { return }
        vehicleCommand(action, vehicleID: selectedControlVehicleID)
    }

    @objc private func detailsPressed() {
        showEmbeddedServiceDetails()
    }

    @discardableResult
    func showServiceDetails() -> ServiceDetailsWindowController? {
        guard serviceTransition == nil, !vehicleControlPending, !diagnosticsOperationPending else {
            NSSound.beep()
            return nil
        }
        if let detailsWindow = activeModalController as? ServiceDetailsWindowController,
           modalState.active == .serviceDetails {
            detailsWindow.update(snapshot: controller.snapshot)
            detailsWindow.setMutationsEnabled(!accountWorkflowActive && !serviceDetailsMutationPending
                                              && !diagnosticsOperationPending)
            detailsWindow.window?.makeKeyAndOrderFront(nil)
            return detailsWindow
        }
        let details = presentPrimaryModal(kind: .serviceDetails) {
            ServiceDetailsWindowController(
                snapshot: self.controller.snapshot,
                controller: self.controller,
                lifecycleActions: self.makeServiceLifecycleActions(),
                mutationAllowed: { [weak self] in
                    guard let self else { return false }
                    return self.serviceTransition == nil
                        && !self.accountWorkflowActive && !self.serviceDetailsMutationPending
                        && !self.diagnosticsOperationPending
                },
                onMutationStateChanged: { [weak self] in
                    self?.setServiceDetailsMutationPending($0)
                },
                onChanged: { [weak self] in self?.update() },
                onDismiss: { [weak self] in self?.dismissServiceDetails() }
            )
        } as? ServiceDetailsWindowController
        detailsWindow = details
        return details
    }

    private func dismissServiceDetails() {
        guard modalState.active == .serviceDetails else { return }
        detailsWindow = nil
        dismissPrimaryModal(kind: .serviceDetails)
    }

    @discardableResult
    func showDiagnostics() -> DiagnosticsWindowController? {
        if let diagnostics = activeModalController as? DiagnosticsWindowController,
           modalState.active == .diagnostics {
            diagnostics.window?.makeKeyAndOrderFront(nil)
            return diagnostics
        }
        return presentPrimaryModal(kind: .diagnostics) {
            DiagnosticsWindowController(controller: self.controller) { [weak self] in
                self?.dismissPrimaryModal(kind: .diagnostics)
            }
        } as? DiagnosticsWindowController
    }

    @objc private func diagnosticsPressed() { showEmbeddedDiagnostics() }

    @objc private func folderPressed() { controller.showDataFolder() }
}

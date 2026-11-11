// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

/// Backups in Service details (M06-T04): the last good generation and the next run as
/// the service reports them (`backup-status`), Back up now with its progress, and the
/// schedule settings bound to the `[backup]` configuration. The recovery password is
/// written only to its owner-only file; it is never kept in preferences or logged.
/// While refused credentials block backups the service's message is shown; the app
/// offers no purge action. English only.
final class BackupSettingsView: NSView {
    typealias FolderChooser = (NSOpenPanel, NSWindow?, @escaping (URL?) -> Void) -> Void
    typealias Scheduler = (TimeInterval, @escaping () -> Void) -> Void

    private let backend: HubMaintenanceBackend
    private let mutationAllowed: () -> Bool
    private let chooseFolder: FolderChooser
    private let schedule: Scheduler
    private let pollInterval: TimeInterval
    private let date: (Date) -> String

    let rowsStack = NSStackView()
    let blockedLabel = NSTextField(wrappingLabelWithString: "")
    let progressIndicator = NSProgressIndicator()
    let progressLabel = NSTextField(wrappingLabelWithString: "")
    let backUpNowButton = HubActionButton(title: "Back up now", target: nil, action: nil)
    let enabledCheckbox = NSButton(checkboxWithTitle: "Back up on a schedule", target: nil, action: nil)
    let intervalPopup = NSPopUpButton()
    let keepField = NSTextField(string: "")
    let keepStepper = NSStepper()
    let destinationLabel = NSTextField(labelWithString: "")
    let chooseFolderButton = HubActionButton(title: "Choose…", target: nil, action: nil)
    let passwordField = NSSecureTextField(string: "")
    let saveButton = HubActionButton(title: "Save schedule", target: nil, action: nil)
    let noteLabel = NSTextField(wrappingLabelWithString: "")

    private(set) var runState: HubBackupRunState = .idle
    private(set) var status: HubBackupStatus?
    private(set) var settings = HubBackupSchedule()
    private var mutationsEnabled = true
    private var pollGeneration = 0

    init(backend: HubMaintenanceBackend,
         mutationAllowed: @escaping () -> Bool = { true },
         pollInterval: TimeInterval = 5,
         chooseFolder: @escaping FolderChooser = { panel, window, completion in
             guard let window else { completion(nil); return }
             panel.beginSheetModal(for: window) { response in
                 completion(response == .OK ? panel.url : nil)
             }
         },
         schedule: @escaping Scheduler = { delay, work in
             DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
         },
         date: @escaping (Date) -> String = HubCollectionStatus.defaultDateText) {
        self.backend = backend
        self.mutationAllowed = mutationAllowed
        self.pollInterval = pollInterval
        self.chooseFolder = chooseFolder
        self.schedule = schedule
        self.date = date
        super.init(frame: .zero)
        identifier = NSUserInterfaceItemIdentifier("hub.backup")
        translatesAutoresizingMaskIntoConstraints = false
        build()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    // MARK: Loading

    /// Reads the configuration and asks the service for its backup state.
    func reload() {
        switch backend.loadBackupSchedule() {
        case let .success(loaded): show(settings: loaded)
        case let .failure(error): noteLabel.stringValue = error.localizedDescription
        }
        refreshStatus()
    }

    func refreshStatus(completion: (() -> Void)? = nil) {
        backend.readBackupStatus { [weak self] result in
            guard let self else { return }
            switch result {
            case let .success(status):
                self.show(status: status)
                // A scheduled run, or one this window no longer waits for, is shown as running.
                if status.running, !self.runState.isBusy {
                    self.setRunState(.stillRunning)
                    self.pollUntilFinished(startedAt: nil)
                }
            case let .failure(error):
                self.status = nil
                self.showRows([("Backups", HubServiceRefusal.parse(error.localizedDescription)?.message
                                    ?? error.localizedDescription)])
                self.blockedLabel.isHidden = true
            }
            completion?()
        }
    }

    // MARK: Back up now

    @objc func backUpNowPressed() {
        guard mutationsEnabled, mutationAllowed(), !runState.isBusy else { NSSound.beep(); return }
        let started = Date()
        setRunState(.running)
        backend.backUpNow { [weak self] outcome in
            guard let self else { return }
            if outcome == .stillRunning {
                self.setRunState(.stillRunning)
                self.pollUntilFinished(startedAt: started)
            } else {
                self.setRunState(outcome)
                self.refreshStatus()
            }
        }
    }

    /// Asks `backup-status` until the service's run has ended, then shows how it ended.
    private func pollUntilFinished(startedAt: Date?) {
        pollGeneration += 1
        let generation = pollGeneration
        schedule(pollInterval) { [weak self] in
            guard let self, generation == self.pollGeneration else { return }
            self.backend.readBackupStatus { [weak self] result in
                guard let self, generation == self.pollGeneration else { return }
                switch result {
                case let .success(status) where status.running:
                    self.show(status: status)
                    self.pollUntilFinished(startedAt: startedAt)
                case let .success(status):
                    self.show(status: status)
                    self.setRunState(Self.endState(of: status, since: startedAt))
                case .failure:
                    // The service may be restarting; keep showing the run and ask again.
                    self.pollUntilFinished(startedAt: startedAt)
                }
            }
        }
    }

    static func endState(of status: HubBackupStatus, since start: Date?) -> HubBackupRunState {
        if let blocked = status.blockedText { return .blocked(message: blocked) }
        let attemptIsThisRun = status.lastAttempt.map { attempt in start.map { attempt >= $0 } ?? true } ?? false
        if let code = status.lastFailureCode, attemptIsThisRun {
            return .failed(message: status.lastFailureMessage ?? code)
        }
        return .finished(generation: status.lastGeneration)
    }

    private func setRunState(_ state: HubBackupRunState) {
        runState = state
        progressLabel.stringValue = state.text ?? ""
        progressLabel.isHidden = state.text == nil
        if state.isBusy {
            progressIndicator.isHidden = false
            progressIndicator.startAnimation(nil)
        } else {
            progressIndicator.stopAnimation(nil)
            progressIndicator.isHidden = true
        }
        progressLabel.textColor = {
            switch state {
            case .failed, .blocked: return HubPalette.danger
            default: return HubPalette.mutedForeground
            }
        }()
        progressLabel.setAccessibilityLabel(state.text)
        updateButtons()
    }

    // MARK: Schedule

    @objc func chooseFolderPressed() {
        guard mutationsEnabled, mutationAllowed() else { NSSound.beep(); return }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        panel.message = "Choose a folder for encrypted Hub backups, outside the Hub data folder."
        chooseFolder(panel, window) { [weak self] url in
            guard let self, let url else { return }
            self.settings.destination = url.path
            self.destinationLabel.stringValue = (url.path as NSString).abbreviatingWithTildeInPath
            self.noteLabel.stringValue = ""
        }
    }

    @objc func controlsChanged() {
        keepField.integerValue = keepStepper.integerValue
        noteLabel.stringValue = ""
    }

    @objc func keepFieldChanged() {
        keepStepper.integerValue = keepField.integerValue
        noteLabel.stringValue = ""
    }

    /// The settings as the controls show them (the password is not part of them).
    func editedSettings() -> HubBackupSchedule {
        var edited = settings
        edited.enabled = enabledCheckbox.state == .on
        edited.intervalHours = intervalPopup.selectedItem?.tag ?? settings.intervalHours
        edited.keep = keepField.integerValue
        return edited
    }

    @objc func savePressed() {
        guard mutationsEnabled, mutationAllowed() else { NSSound.beep(); return }
        let edited = editedSettings()
        let password = passwordField.stringValue.isEmpty ? nil : passwordField.stringValue
        if let problem = edited.problem(dataDirectory: backend.dataDirectory, newPassword: password) {
            noteLabel.stringValue = problem
            noteLabel.textColor = HubPalette.danger
            return
        }
        switch backend.saveBackupSchedule(edited, password: password) {
        case let .success(saved):
            passwordField.stringValue = ""
            show(settings: saved)
            noteLabel.stringValue = "Saved. The schedule applies when the Hub restarts."
            noteLabel.textColor = HubPalette.mutedForeground
        case let .failure(error):
            passwordField.stringValue = ""
            noteLabel.stringValue = error.localizedDescription
            noteLabel.textColor = HubPalette.danger
        }
    }

    func setMutationsEnabled(_ enabled: Bool) {
        mutationsEnabled = enabled
        updateButtons()
    }

    private func updateButtons() {
        backUpNowButton.isEnabled = mutationsEnabled && !runState.isBusy
        saveButton.isEnabled = mutationsEnabled
        chooseFolderButton.isEnabled = mutationsEnabled
    }

    // MARK: Showing

    private func show(settings loaded: HubBackupSchedule) {
        settings = loaded
        enabledCheckbox.state = loaded.enabled ? .on : .off
        intervalPopup.removeAllItems()
        var choices = HubBackupSchedule.intervalChoices
        if !choices.contains(loaded.intervalHours) { choices.append(loaded.intervalHours); choices.sort() }
        for hours in choices {
            intervalPopup.addItem(withTitle: Self.intervalTitle(hours))
            intervalPopup.lastItem?.tag = hours
        }
        intervalPopup.selectItem(withTag: loaded.intervalHours)
        keepStepper.integerValue = loaded.keep
        keepField.integerValue = loaded.keep
        destinationLabel.stringValue = loaded.destination.map { ($0 as NSString).abbreviatingWithTildeInPath }
            ?? "No folder chosen"
        passwordField.placeholderString = loaded.passwordFile == nil
            ? "Recovery password (at least 12 characters)"
            : "Saved. Type a new one to replace it"
    }

    static func intervalTitle(_ hours: Int) -> String {
        switch hours {
        case 1: return "Every hour"
        case 24: return "Every day"
        case 48: return "Every 2 days"
        case 168: return "Every week"
        default: return "Every \(hours) hours"
        }
    }

    private func show(status: HubBackupStatus) {
        self.status = status
        showRows(status.rows(date: date))
        if let blocked = status.blockedText {
            blockedLabel.stringValue = blocked
            blockedLabel.isHidden = false
        } else {
            blockedLabel.isHidden = true
        }
    }

    private func showRows(_ rows: [(label: String, value: String)]) {
        rowsStack.arrangedSubviews.forEach {
            rowsStack.removeArrangedSubview($0)
            $0.removeFromSuperview()
        }
        for row in rows {
            let label = NSTextField(labelWithString: row.label)
            label.font = HubTypography.label
            label.textColor = HubPalette.foreground
            let value = NSTextField(labelWithString: row.value)
            value.font = HubTypography.caption
            value.textColor = HubPalette.mutedForeground
            value.lineBreakMode = .byTruncatingMiddle
            value.toolTip = row.value
            let line = NSStackView(views: [label, value])
            line.spacing = 8
            label.widthAnchor.constraint(equalToConstant: 120).isActive = true
            rowsStack.addArrangedSubview(line)
        }
    }

    // MARK: Layout

    private func build() {
        let title = NSTextField(labelWithString: "Backups")
        title.font = HubTypography.emphasis
        title.textColor = HubPalette.foreground
        let detail = NSTextField(wrappingLabelWithString:
            "Encrypted copies of the Hub's history. The recovery password stays in an owner-only file on this Mac.")
        detail.font = HubTypography.caption
        detail.textColor = HubPalette.mutedForeground

        rowsStack.orientation = .vertical
        rowsStack.alignment = .leading
        rowsStack.spacing = 4
        rowsStack.identifier = NSUserInterfaceItemIdentifier("hub.backup.status")

        blockedLabel.identifier = NSUserInterfaceItemIdentifier("hub.backup.blocked")
        blockedLabel.font = HubTypography.caption
        blockedLabel.textColor = HubPalette.danger
        blockedLabel.isSelectable = true
        blockedLabel.isHidden = true

        configure(backUpNowButton, id: "hub.backup.now", symbol: "externaldrive.badge.timemachine",
                  style: .primary, action: #selector(backUpNowPressed))
        progressIndicator.style = .spinning
        progressIndicator.controlSize = .small
        progressIndicator.isDisplayedWhenStopped = false
        progressIndicator.isHidden = true
        progressIndicator.identifier = NSUserInterfaceItemIdentifier("hub.backup.progress-indicator")
        progressLabel.identifier = NSUserInterfaceItemIdentifier("hub.backup.progress")
        progressLabel.font = HubTypography.caption
        progressLabel.textColor = HubPalette.mutedForeground
        progressLabel.isHidden = true
        let runRow = NSStackView(views: [backUpNowButton, progressIndicator, progressLabel])
        runRow.alignment = .centerY
        runRow.spacing = 8

        let scheduleTitle = NSTextField(labelWithString: "Schedule")
        scheduleTitle.font = HubTypography.label
        scheduleTitle.textColor = HubPalette.foreground
        enabledCheckbox.identifier = NSUserInterfaceItemIdentifier("hub.backup.enabled")
        enabledCheckbox.target = self
        enabledCheckbox.action = #selector(controlsChanged)
        intervalPopup.identifier = NSUserInterfaceItemIdentifier("hub.backup.interval")
        intervalPopup.target = self
        intervalPopup.action = #selector(controlsChanged)
        keepStepper.identifier = NSUserInterfaceItemIdentifier("hub.backup.keep-stepper")
        keepStepper.minValue = Double(HubBackupSchedule.keepRange.lowerBound)
        keepStepper.maxValue = Double(HubBackupSchedule.keepRange.upperBound)
        keepStepper.increment = 1
        keepStepper.valueWraps = false
        keepStepper.target = self
        keepStepper.action = #selector(controlsChanged)
        keepField.identifier = NSUserInterfaceItemIdentifier("hub.backup.keep")
        keepField.alignment = .right
        keepField.target = self
        keepField.action = #selector(keepFieldChanged)
        keepField.widthAnchor.constraint(equalToConstant: 52).isActive = true
        let keepLabel = NSTextField(labelWithString: "Keep")
        keepLabel.font = HubTypography.caption
        let keepSuffix = NSTextField(labelWithString: "backups")
        keepSuffix.font = HubTypography.caption
        let cadenceRow = NSStackView(views: [intervalPopup, keepLabel, keepField, keepStepper, keepSuffix])
        cadenceRow.alignment = .centerY
        cadenceRow.spacing = 6

        destinationLabel.identifier = NSUserInterfaceItemIdentifier("hub.backup.destination")
        destinationLabel.font = HubTypography.caption
        destinationLabel.textColor = HubPalette.mutedForeground
        destinationLabel.lineBreakMode = .byTruncatingMiddle
        configure(chooseFolderButton, id: "hub.backup.choose-folder", symbol: "folder",
                  style: .neutral, action: #selector(chooseFolderPressed))
        let folderLabel = NSTextField(labelWithString: "Folder")
        folderLabel.font = HubTypography.caption
        let folderRow = NSStackView(views: [folderLabel, destinationLabel, chooseFolderButton])
        folderRow.alignment = .centerY
        folderRow.spacing = 6
        destinationLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        passwordField.identifier = NSUserInterfaceItemIdentifier("hub.backup.password")
        passwordField.placeholderString = "Recovery password (at least 12 characters)"
        passwordField.setAccessibilityLabel("Recovery password")
        passwordField.widthAnchor.constraint(equalToConstant: 280).isActive = true
        configure(saveButton, id: "hub.backup.save", symbol: nil, style: .neutral, action: #selector(savePressed))
        let passwordRow = NSStackView(views: [passwordField, saveButton])
        passwordRow.alignment = .centerY
        passwordRow.spacing = 8

        noteLabel.identifier = NSUserInterfaceItemIdentifier("hub.backup.note")
        noteLabel.font = HubTypography.caption
        noteLabel.textColor = HubPalette.mutedForeground

        let divider = HubModalChrome.divider()
        let contents = NSStackView(views: [title, detail, rowsStack, blockedLabel, runRow, divider,
                                           scheduleTitle, enabledCheckbox, cadenceRow, folderRow, passwordRow, noteLabel])
        contents.orientation = .vertical
        contents.alignment = .leading
        contents.spacing = 8
        contents.setCustomSpacing(4, after: title)
        contents.setCustomSpacing(12, after: runRow)
        contents.translatesAutoresizingMaskIntoConstraints = false
        let card = HubCardView()
        card.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(contents)
        addSubview(card)
        NSLayoutConstraint.activate([
            card.leadingAnchor.constraint(equalTo: leadingAnchor),
            card.trailingAnchor.constraint(equalTo: trailingAnchor),
            card.topAnchor.constraint(equalTo: topAnchor),
            card.bottomAnchor.constraint(equalTo: bottomAnchor),
            contents.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: HubMetrics.rowHorizontalInset),
            contents.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -HubMetrics.rowHorizontalInset),
            contents.topAnchor.constraint(equalTo: card.topAnchor, constant: 14),
            contents.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -14),
            divider.widthAnchor.constraint(equalTo: contents.widthAnchor)
        ])
        for view in [detail, blockedLabel, noteLabel, folderRow, rowsStack] {
            view.widthAnchor.constraint(lessThanOrEqualTo: contents.widthAnchor).isActive = true
        }
        show(settings: settings)
        setRunState(.idle)
    }

    private func configure(_ button: HubActionButton, id: String, symbol: String?,
                           style: HubButtonStyle, action: Selector) {
        button.identifier = NSUserInterfaceItemIdentifier(id)
        button.target = self
        button.action = action
        button.hubStyle = style
        button.hubFont = HubTypography.action
        button.image = symbol.flatMap { NSImage(systemSymbolName: $0, accessibilityDescription: button.title) }
        button.imagePosition = symbol == nil ? .noImage : .imageLeading
        button.heightAnchor.constraint(equalToConstant: HubMetrics.compactControlHeight).isActive = true
    }
}

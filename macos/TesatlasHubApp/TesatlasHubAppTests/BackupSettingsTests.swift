// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import Security
import XCTest
@testable import Tesatlas_Hub

/// A maintenance backend that records calls and answers when told to. Shared with
/// DeviceListTests.
final class FakeMaintenanceBackend: HubMaintenanceBackend {
    var dataDirectory: URL? = URL(fileURLWithPath: "/Hub/data", isDirectory: true)
    var vehicleNames: [UUID: String] = [:]
    var schedule = HubBackupSchedule()
    var statusAnswers: [Result<HubBackupStatus, Error>] = []
    var statusCalls = 0
    var backUpNowCompletion: ((HubBackupRunState) -> Void)?
    var backUpNowCalls = 0
    var saved: [(HubBackupSchedule, String?)] = []
    var saveResult: Result<HubBackupSchedule, Error>?
    var devicesAnswer: Result<[HubPairedDevice], Error> = .success([])
    var deviceListCalls = 0
    var revoked: [UUID] = []
    var revokeCompletion: ((Result<Void, Error>) -> Void)?

    func loadBackupSchedule() -> Result<HubBackupSchedule, Error> { .success(schedule) }

    func saveBackupSchedule(_ schedule: HubBackupSchedule, password: String?) -> Result<HubBackupSchedule, Error> {
        saved.append((schedule, password))
        var stored = schedule
        if password != nil { stored.passwordFile = "/Hub/backup-password" }
        self.schedule = stored
        return saveResult ?? .success(stored)
    }

    func readBackupStatus(completion: @escaping (Result<HubBackupStatus, Error>) -> Void) {
        statusCalls += 1
        completion(statusAnswers.isEmpty ? .success(HubBackupStatus()) : statusAnswers.removeFirst())
    }

    func backUpNow(completion: @escaping (HubBackupRunState) -> Void) {
        backUpNowCalls += 1
        backUpNowCompletion = completion
    }

    func listPairedDevices(completion: @escaping (Result<[HubPairedDevice], Error>) -> Void) {
        deviceListCalls += 1
        completion(devicesAnswer)
    }

    func revokeDevice(_ id: UUID, completion: @escaping (Result<Void, Error>) -> Void) {
        revoked.append(id)
        revokeCompletion = completion
    }
}

/// Records the commands the controller would run, answering each from a script.
final class RecordingMaintenanceRunner: HubCommandRunning {
    private(set) var arguments: [[String]] = []
    var answer: (([String]) -> Result<String, Error>) = { _ in .success("{}") }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        self.arguments.append(arguments)
        completion(answer(arguments))
    }
}

/// An injected password store: records each item, or fails when told to. Never a keychain.
final class FakePasswordStore: HubBackupPasswordStore {
    var items: [String: [UInt8]] = [:]
    var failure: Error?
    var stores = 0
    /// Runs inside `store`, before the item is written (a file changing meanwhile).
    var duringStore: (() -> Void)?

    func store(_ password: [UInt8], account: String) throws {
        stores += 1
        duringStore?()
        if let failure { throw failure }
        items[account] = password
    }

    /// Thrown by `compare`, as the service refuses an equal but unusable item.
    var compareFailure: Error?

    func compare(_ password: [UInt8], account: String) throws -> HubBackupPasswordComparison {
        if let compareFailure { throw compareFailure }
        guard let item = items[account] else { return .missing }
        return item == password ? .equal : .different
    }

    func hasPassword(account: String) throws -> Bool { items[account] != nil }
}

private extension Result {
    var failureError: Failure? {
        if case let .failure(error) = self { return error }
        return nil
    }
}

/// M06-T04: Back up now with its progress, the last good generation, the schedule
/// settings bound to `[backup]`, and the refused-credentials block. No window is shown.
final class BackupSettingsTests: XCTestCase {
    private let fixedDate: (Date) -> String = { "T\(Int($0.timeIntervalSince1970))" }

    private func statusJSON(_ fields: [String: Any]) -> String {
        String(data: try! JSONSerialization.data(withJSONObject: fields), encoding: .utf8)!
    }

    private func labels(_ view: NSView) -> [String] {
        ((view as? NSTextField).map { [$0.stringValue] } ?? []) + view.subviews.flatMap(labels)
    }

    private func buttons(_ view: NSView) -> [NSButton] {
        ((view as? NSButton).map { [$0] } ?? []) + view.subviews.flatMap(buttons)
    }

    private func makeView(_ backend: FakeMaintenanceBackend,
                          mutationAllowed: @escaping () -> Bool = { true },
                          scheduled: @escaping (TimeInterval, @escaping () -> Void) -> Void = { _, _ in })
        -> BackupSettingsView {
        BackupSettingsView(backend: backend, mutationAllowed: mutationAllowed, pollInterval: 1,
                           chooseFolder: { _, _, completion in completion(URL(fileURLWithPath: "/Volumes/Backup/Hub")) },
                           schedule: scheduled, date: fixedDate)
    }

    // MARK: Status

    func testStatusReportShowsTheLastGoodGenerationAndTheNextRun() throws {
        let report = statusJSON(["enabled": true, "configured": true, "running": false, "paused": false,
                                 "lastSuccessMs": 1_700_000_000_000, "lastGeneration": "generation-7",
                                 "nextRunMs": 1_700_086_400_000, "blocked": false])
        let status = try XCTUnwrap(HubBackupStatus(report: report))
        XCTAssertEqual(status.lastGeneration, "generation-7")
        XCTAssertEqual(status.lastGoodText(date: fixedDate), "T1700000000 (generation-7)")
        XCTAssertEqual(status.nextRunText(date: fixedDate), "T1700086400")
        XCTAssertNil(status.blockedText)

        let backend = FakeMaintenanceBackend()
        backend.statusAnswers = [.success(status)]
        let view = makeView(backend)
        view.reload()
        let shown = labels(view.rowsStack)
        XCTAssertTrue(shown.contains("Last good backup"))
        XCTAssertTrue(shown.contains("T1700000000 (generation-7)"))
        XCTAssertTrue(shown.contains("T1700086400"))
        XCTAssertTrue(view.blockedLabel.isHidden)

        var off = HubBackupStatus()
        XCTAssertEqual(off.nextRunText(), "Scheduled backups are off")
        off.enabled = true
        XCTAssertEqual(off.nextRunText(), "Not scheduled: choose a folder and a password")
        XCTAssertEqual(off.lastGoodText(), "None yet")
        XCTAssertNil(HubBackupStatus(report: "not json"))
    }

    // MARK: Back up now

    func testBackUpNowShowsProgressThenTheNewGeneration() throws {
        let backend = FakeMaintenanceBackend()
        let view = makeView(backend)
        view.reload()
        view.backUpNowButton.performClick(nil)
        XCTAssertEqual(backend.backUpNowCalls, 1)
        XCTAssertEqual(view.runState, .running)
        XCTAssertFalse(view.progressIndicator.isHidden)
        XCTAssertEqual(view.progressLabel.stringValue, "Backing up…")
        XCTAssertFalse(view.backUpNowButton.isEnabled, "one run at a time")
        view.backUpNowButton.performClick(nil)
        XCTAssertEqual(backend.backUpNowCalls, 1)

        var finished = HubBackupStatus()
        finished.lastGeneration = "generation-8"
        finished.lastSuccess = Date(timeIntervalSince1970: 1_700_000_000)
        backend.statusAnswers = [.success(finished)]
        let completion = try XCTUnwrap(backend.backUpNowCompletion)
        completion(.finished(generation: "generation-8"))
        XCTAssertEqual(view.runState, .finished(generation: "generation-8"))
        XCTAssertEqual(view.progressLabel.stringValue, "Backup finished: generation-8")
        XCTAssertTrue(view.progressIndicator.isHidden)
        XCTAssertTrue(view.backUpNowButton.isEnabled)
        XCTAssertTrue(labels(view.rowsStack).contains("T1700000000 (generation-8)"), "the status is read again")
    }

    func testARunLongerThanTheCommandWaitIsShownAsStillRunningNotAFailure() throws {
        // The CLI forwarding wait (180 s) ended while the service still runs the backup.
        let channel = HubActionError.commandExited(1, "Error: control_channel_failed: the control channel answer did not arrive")
        XCTAssertEqual(HubBackupRunState.outcome(of: .failure(channel)), .stillRunning)
        XCTAssertEqual(HubBackupRunState.outcome(of: .failure(HubActionError.commandTimedOut)), .stillRunning)
        let overlap = HubActionError.commandExited(1, "backup_running: a backup is already running; try again when it has finished")
        XCTAssertEqual(HubBackupRunState.outcome(of: .failure(overlap)), .stillRunning)
        let failed = HubActionError.commandExited(1, "Error: backup_failed: the destination is full")
        XCTAssertEqual(HubBackupRunState.outcome(of: .failure(failed)), .failed(message: "the destination is full"))
        XCTAssertEqual(HubBackupRunState.outcome(of: .success(#"{"generation":"g-1","trigger":"manual"}"#)),
                       .finished(generation: "g-1"))

        let backend = FakeMaintenanceBackend()
        var pending: [() -> Void] = []
        let view = makeView(backend, scheduled: { _, work in pending.append(work) })
        view.backUpNowButton.performClick(nil)
        try XCTUnwrap(backend.backUpNowCompletion)(.stillRunning)
        XCTAssertEqual(view.runState, .stillRunning)
        XCTAssertEqual(view.progressLabel.stringValue, "The backup is still running in the Hub service.")
        XCTAssertFalse(view.progressIndicator.isHidden)
        XCTAssertNotEqual(view.progressLabel.textColor, HubPalette.danger)
        XCTAssertFalse(view.backUpNowButton.isEnabled)

        var running = HubBackupStatus()
        running.running = true
        var done = HubBackupStatus()
        done.lastGeneration = "generation-9"
        // One stamp: the service records a run's attempt and success as the same instant, and two
        // Date() reads can differ when the Mac is busy between them.
        let finishedAt = Date()
        done.lastSuccess = finishedAt
        done.lastAttempt = finishedAt
        backend.statusAnswers = [.success(running), .failure(HubActionError.commandTimedOut), .success(done)]
        for _ in 0..<3 {
            XCTAssertEqual(pending.count, 1)
            pending.removeFirst()()
        }
        XCTAssertTrue(pending.isEmpty, "polling stops when the run has ended")
        XCTAssertEqual(view.runState, .finished(generation: "generation-9"))
        XCTAssertTrue(view.backUpNowButton.isEnabled)
    }

    /// REV2-CX-M06-T06 finding 1: the command times out, the service restarts during the
    /// run and reports only the earlier generation (or nothing). That is not a success.
    func testATimedOutRunLostToAServiceRestartIsUnconfirmedNotFinished() throws {
        let earlier = Date(timeIntervalSince1970: 1_700_000_000)
        var previous = HubBackupStatus()
        previous.lastAttempt = earlier
        previous.lastSuccess = earlier
        previous.lastGeneration = "generation-A"
        var staleFailure = previous
        staleFailure.lastFailureCode = "backup_failed"
        staleFailure.lastFailureMessage = "the destination is full"
        staleFailure.lastSuccess = earlier.addingTimeInterval(-3600)

        for (name, afterRestart) in [("earlier generation", previous), ("no state", HubBackupStatus()),
                                     ("earlier failure", staleFailure)] {
            let backend = FakeMaintenanceBackend()
            var pending: [() -> Void] = []
            let view = makeView(backend, scheduled: { _, work in pending.append(work) })
            view.backUpNowButton.performClick(nil)
            try XCTUnwrap(backend.backUpNowCompletion)(.stillRunning)
            backend.statusAnswers = [.failure(HubActionError.commandTimedOut), .success(afterRestart)]
            for _ in 0..<2 {
                XCTAssertEqual(pending.count, 1, name)
                pending.removeFirst()()
            }
            XCTAssertTrue(pending.isEmpty, name)
            XCTAssertEqual(view.runState, .unconfirmed, name)
            XCTAssertFalse(view.progressLabel.stringValue.contains("finished"), name)
            XCTAssertEqual(view.progressLabel.stringValue,
                           "The backup was interrupted or its result is unknown. The last good backup is unchanged.", name)
            XCTAssertTrue(view.backUpNowButton.isEnabled, name)
            if afterRestart.lastGeneration != nil {
                let lastGood = afterRestart.lastGoodText(date: fixedDate)
                XCTAssertTrue(lastGood.hasSuffix("(generation-A)"), name)
                XCTAssertTrue(labels(view.rowsStack).contains(lastGood), "\(name): the last good backup stays shown")
            }
        }
    }

    func testOnlyTheWatchedRunsOwnSuccessEndsTheWaitAsFinished() {
        let earlier = Date(timeIntervalSince1970: 1_700_000_000)
        let requested = earlier.addingTimeInterval(600)
        let later = requested.addingTimeInterval(5)
        func status(attempt: Date?, success: Date?, generation: String?, failure: String? = nil) -> HubBackupStatus {
            var status = HubBackupStatus()
            status.lastAttempt = attempt
            status.lastSuccess = success
            status.lastGeneration = generation
            status.lastFailureCode = failure
            return status
        }
        let mine = HubBackupRunWatch(requestedAt: requested)
        XCTAssertEqual(mine.endState(of: status(attempt: later, success: later, generation: "generation-B")),
                       .finished(generation: "generation-B"))
        XCTAssertEqual(mine.endState(of: status(attempt: earlier, success: earlier, generation: "generation-A")), .unconfirmed)
        XCTAssertEqual(mine.endState(of: status(attempt: later, success: earlier, generation: "generation-A", failure: "backup_failed")),
                       .failed(message: "backup_failed"))
        XCTAssertEqual(mine.endState(of: status(attempt: later, success: later, generation: nil)), .unconfirmed,
                       "success needs the run's generation")

        // A run already going on (a scheduled one, or one that held the slot): its outcome is
        // an attempt later than the one reported while it ran.
        var scheduled = HubBackupRunWatch()
        var running = status(attempt: earlier, success: earlier, generation: "generation-A")
        running.running = true
        scheduled.observe(running)
        XCTAssertEqual(scheduled.endState(of: status(attempt: earlier, success: earlier, generation: "generation-A")), .unconfirmed)
        XCTAssertEqual(scheduled.endState(of: status(attempt: later, success: later, generation: "generation-C")),
                       .finished(generation: "generation-C"))
        var overlapped = HubBackupRunWatch(requestedAt: requested)
        overlapped.observe(running)
        XCTAssertEqual(overlapped.endState(of: status(attempt: requested.addingTimeInterval(-60), success: requested.addingTimeInterval(-60),
                                                      generation: "generation-D")),
                       .finished(generation: "generation-D"), "the run that held the slot finished")
        XCTAssertEqual(HubBackupRunWatch().endState(of: status(attempt: later, success: later, generation: "generation-E")),
                       .unconfirmed, "no run was seen")
    }

    func testAScheduledRunInProgressIsShownWhenTheSectionOpens() {
        let backend = FakeMaintenanceBackend()
        var running = HubBackupStatus()
        running.running = true
        running.enabled = true
        running.configured = true
        backend.statusAnswers = [.success(running)]
        var pending = 0
        let view = makeView(backend, scheduled: { _, _ in pending += 1 })
        view.reload()
        XCTAssertEqual(view.runState, .stillRunning)
        XCTAssertTrue(labels(view.rowsStack).contains("A backup is running now"))
        XCTAssertEqual(pending, 1)
        XCTAssertFalse(view.backUpNowButton.isEnabled)
    }

    func testRefusedCredentialsShowTheServiceMessageAndOfferNoPurge() throws {
        let message = "backups wait until the refused credentials are removed: run tesatlas-hub purge-refused-credentials --confirm"
        let report = statusJSON(["enabled": true, "configured": true, "running": false,
                                 "lastFailureCode": "backup_blocked_by_refused_credentials",
                                 "lastFailureMessage": message, "blocked": true,
                                 "lastGeneration": "generation-3", "lastSuccessMs": 1_700_000_000_000])
        let status = try XCTUnwrap(HubBackupStatus(report: report))
        XCTAssertEqual(status.blockedText, message)
        XCTAssertNil(status.lastResultText(), "the block is not shown twice")

        let backend = FakeMaintenanceBackend()
        backend.statusAnswers = [.success(status)]
        let view = makeView(backend)
        view.reload()
        XCTAssertFalse(view.blockedLabel.isHidden)
        XCTAssertEqual(view.blockedLabel.stringValue, message)
        XCTAssertTrue(view.blockedLabel.stringValue.contains("purge-refused-credentials"))
        XCTAssertTrue(labels(view.rowsStack).contains("T1700000000 (generation-3)"), "the last good generation stays shown")
        XCTAssertFalse(buttons(view).contains { $0.title.lowercased().contains("purge") }, "no purge action")

        let refusal = HubActionError.commandExited(1, "Error: backup_blocked_by_refused_credentials: \(message)")
        XCTAssertEqual(HubBackupRunState.outcome(of: .failure(refusal)), .blocked(message: message))
        view.backUpNowButton.performClick(nil)
        try XCTUnwrap(backend.backUpNowCompletion)(.blocked(message: message))
        XCTAssertEqual(view.progressLabel.stringValue, message)
        XCTAssertFalse(buttons(view).contains { $0.title.lowercased().contains("purge") })
    }

    func testAClosedOwnerGateReachesNoBackupAction() {
        let backend = FakeMaintenanceBackend()
        let view = makeView(backend, mutationAllowed: { false })
        view.backUpNowButton.performClick(nil)
        view.saveButton.performClick(nil)
        XCTAssertEqual(backend.backUpNowCalls, 0)
        XCTAssertTrue(backend.saved.isEmpty)
        view.setMutationsEnabled(false)
        XCTAssertFalse(view.backUpNowButton.isEnabled)
        XCTAssertFalse(view.saveButton.isEnabled)
        XCTAssertFalse(view.chooseFolderButton.isEnabled)
    }

    // MARK: Schedule settings

    func testScheduleIsReadFromAndWrittenToTheBackupSection() {
        let config = """
        data_dir = "/Hub/data"

        [backup]
        enabled = true # on
        destination = "/Volumes/Backup/Hub \\"A\\""
        password_file = '/Hub/backup-password'
        interval_hours = 12
        keep = 5
        future_key = 1

        [geocoder]
        enabled = false

        """
        let schedule = HubBackupSchedule(config: config)
        XCTAssertEqual(schedule, HubBackupSchedule(enabled: true, destination: "/Volumes/Backup/Hub \"A\"",
                                                   passwordFile: "/Hub/backup-password", intervalHours: 12, keep: 5))
        var changed = schedule
        changed.enabled = false
        changed.intervalHours = 48
        changed.destination = "/Volumes/Other"
        let written = changed.applied(to: config)
        XCTAssertEqual(HubBackupSchedule(config: written), changed)
        XCTAssertTrue(written.contains("future_key = 1"), "unknown keys stay")
        XCTAssertTrue(written.contains("[geocoder]\nenabled = false"), "other tables stay")
        XCTAssertTrue(written.hasPrefix("data_dir = \"/Hub/data\"\n"))
        XCTAssertTrue(written.hasSuffix("\n"))

        let fresh = HubBackupSchedule(enabled: true, destination: "/Volumes/B", passwordFile: "/Hub/p",
                                      intervalHours: 24, keep: 7).applied(to: "data_dir = \"/Hub/data\"\n")
        XCTAssertEqual(fresh, """
        data_dir = "/Hub/data"

        [backup]
        enabled = true
        destination = "/Volumes/B"
        password_file = "/Hub/p"
        interval_hours = 24
        keep = 7

        """)
        XCTAssertEqual(HubBackupSchedule(config: "data_dir = \"/x\"\n"), HubBackupSchedule(), "defaults: off, 24 h, keep 7")
        XCTAssertFalse(fresh.contains("password ="), "the password is never in the configuration")
    }

    func testScheduleChecksMatchTheService() {
        let data = URL(fileURLWithPath: "/Hub/data", isDirectory: true)
        XCTAssertNil(HubBackupSchedule().problem(dataDirectory: data))
        XCTAssertNotNil(HubBackupSchedule(intervalHours: 0).problem(dataDirectory: data))
        XCTAssertNotNil(HubBackupSchedule(keep: 1001).problem(dataDirectory: data))
        XCTAssertEqual(HubBackupSchedule(destination: "/Hub/data/backups").problem(dataDirectory: data),
                       "Choose a backup folder outside the Hub data folder.")
        XCTAssertNil(HubBackupSchedule(destination: "/Hub/data-backups").problem(dataDirectory: data))
        XCTAssertNotNil(HubBackupSchedule(enabled: true, destination: "/Volumes/B").problem(dataDirectory: data))
        XCTAssertNil(HubBackupSchedule(enabled: true, destination: "/Volumes/B")
            .problem(dataDirectory: data, newPassword: "twelve chars"))
        XCTAssertNotNil(HubBackupSchedule().problem(dataDirectory: data, newPassword: "short"))
    }

    func testSaveWritesTheScheduleAndKeepsThePasswordOutOfPreferences() {
        let backend = FakeMaintenanceBackend()
        backend.schedule = HubBackupSchedule(intervalHours: 24, keep: 7)
        let view = makeView(backend)
        view.reload()
        XCTAssertEqual(view.enabledCheckbox.state, .off)
        XCTAssertEqual(view.intervalPopup.selectedItem?.tag, 24)
        XCTAssertEqual(view.destinationLabel.stringValue, "No folder chosen")

        view.chooseFolderButton.performClick(nil)
        XCTAssertEqual(view.destinationLabel.stringValue, "/Volumes/Backup/Hub")
        view.enabledCheckbox.state = .on
        view.intervalPopup.selectItem(withTag: 6)
        view.keepStepper.integerValue = 10
        view.controlsChanged()
        XCTAssertEqual(view.keepField.integerValue, 10)

        let password = "M06-T04 \(UUID().uuidString)"
        view.passwordField.stringValue = "too short"
        view.saveButton.performClick(nil)
        XCTAssertTrue(backend.saved.isEmpty)
        XCTAssertTrue(view.noteLabel.stringValue.contains("at least 12"))

        view.passwordField.stringValue = password
        view.saveButton.performClick(nil)
        XCTAssertEqual(backend.saved.count, 1)
        XCTAssertEqual(backend.saved.first?.0, HubBackupSchedule(enabled: true, destination: "/Volumes/Backup/Hub",
                                                                 intervalHours: 6, keep: 10))
        XCTAssertEqual(backend.saved.first?.1, password)
        XCTAssertEqual(view.passwordField.stringValue, "", "the field is cleared once saved")
        XCTAssertEqual(view.noteLabel.stringValue, "Saved. The schedule applies when the Hub restarts.")
        XCTAssertEqual(view.settings.passwordFile, "/Hub/backup-password")
        let preferences = UserDefaults.standard.dictionaryRepresentation().values.map { "\($0)" }.joined()
        XCTAssertFalse(preferences.contains(password), "the password is never kept in preferences")

        // Inside the data folder: refused before anything is written.
        view.chooseFolderButton.performClick(nil)
        backend.dataDirectory = URL(fileURLWithPath: "/Volumes/Backup", isDirectory: true)
        view.saveButton.performClick(nil)
        XCTAssertEqual(backend.saved.count, 1)
        XCTAssertEqual(view.noteLabel.stringValue, "Choose a backup folder outside the Hub data folder.")
    }

    // MARK: Keychain (M06-T12)

    private func hubFolder(_ name: String) throws -> (home: URL, folder: URL, config: URL, controller: HubController) {
        let home = HubTestScratch.fixtureRoot(name)
        let folder = home.appendingPathComponent("Library/Application Support/Tesatlas Hub", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let config = folder.appendingPathComponent("config.toml")
        try Data("data_dir = \"/Hub/data\"\n".utf8).write(to: config)
        let controller = HubController(environment: [:], commandRunner: RecordingMaintenanceRunner(),
                                       launchAgent: nil, dataRemover: nil, homeDirectory: home,
                                       serviceInstalledOverride: true)
        return (home, folder, config, controller)
    }

    func testThePasswordGoesToTheKeychainAndNoFileIsWritten() throws {
        let hub = try hubFolder("BackupSettings")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        let password = "correct horse battery staple"
        let saved = try backend.saveBackupSchedule(HubBackupSchedule(enabled: true, destination: "/Volumes/B"),
                                                   password: password).get()
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        XCTAssertEqual(store.items, [name.path: Array(password.utf8)], "one item, named by the configured path")
        XCTAssertFalse(FileManager.default.fileExists(atPath: name.path), "no plaintext file is written")
        XCTAssertEqual(saved.passwordFile, name.path)
        let written = try String(contentsOf: hub.config, encoding: .utf8)
        XCTAssertFalse(written.contains(password))
        XCTAssertEqual(HubBackupSchedule(config: written).passwordFile, name.path)
        XCTAssertEqual(written.components(separatedBy: "password_source = \"keychain\"").count, 2,
                       "the section names the Keychain as the password source once (M06-T13)")
        XCTAssertEqual(try backend.loadBackupSchedule().get(), saved)

        // A short password changes nothing; a failing Keychain is reported and changes nothing.
        let short = backend.saveBackupSchedule(saved, password: "short")
        XCTAssertEqual(short.failureError as? HubMaintenanceError, .passwordTooShort)
        // REV2-CX-M06-T17 CR-8: a new password ending in a line break is refused before the
        // service is asked, and the configuration and item stay as they were.
        for ending in ["\n", "\r", "\r\n"] {
            let lineEnd = backend.saveBackupSchedule(saved, password: String(repeating: "a", count: 1023) + ending)
            XCTAssertEqual(lineEnd.failureError as? HubMaintenanceError, .passwordEndsInLineBreak)
            XCTAssertEqual(lineEnd.failureError?.localizedDescription,
                           "The recovery password must not end in a line break. Remove it and save again.")
        }
        XCTAssertEqual(store.items, [name.path: Array(password.utf8)])
        XCTAssertEqual(try String(contentsOf: hub.config, encoding: .utf8), written)
        store.failure = HubMaintenanceError.passwordFileUnwritable
        let refused = backend.saveBackupSchedule(saved, password: "another long password")
        XCTAssertEqual(refused.failureError as? HubMaintenanceError, .passwordFileUnwritable)
        XCTAssertEqual(refused.failureError?.localizedDescription, "The recovery password could not be saved.")
        XCTAssertEqual(store.items, [name.path: Array(password.utf8)])
        XCTAssertEqual(try String(contentsOf: hub.config, encoding: .utf8), written)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: hub.folder.path), ["config.toml"])
    }

    func testAnEarlierPasswordFileMovesIntoTheKeychainAndIsRemoved() throws {
        let hub = try hubFolder("BackupMigration")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        let password = "correct horse battery staple"
        let config = "data_dir = \"/Hub/data\"\n\n[backup]\nenabled = true\ndestination = \"/Volumes/B\"\n"
            + "password_file = \(HubController.tomlBasicString(name.path))\n"
        try Data(config.utf8).write(to: hub.config)
        func writeLegacy(mode: Int) throws {
            try Data("\(password)\n".utf8).write(to: name)
            try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: mode)], ofItemAtPath: name.path)
        }

        // A failing Keychain leaves the file and the configuration as they are.
        let store = FakePasswordStore()
        store.failure = HubMaintenanceError.passwordFileUnwritable
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        try writeLegacy(mode: 0o600)
        XCTAssertEqual(try backend.loadBackupSchedule().get().passwordFile, name.path)
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))

        // A file another account could read is not moved.
        store.failure = nil
        try writeLegacy(mode: 0o640)
        _ = try backend.loadBackupSchedule().get()
        XCTAssertEqual(store.items, [:])
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))

        // The owner-only file moves (its line end dropped) and is removed.
        try writeLegacy(mode: 0o600)
        let loaded = try backend.loadBackupSchedule().get()
        XCTAssertEqual(store.items, [name.path: Array(password.utf8)])
        XCTAssertFalse(FileManager.default.fileExists(atPath: name.path))
        XCTAssertEqual(loaded.passwordFile, name.path, "the configuration keeps naming the item")
        XCTAssertEqual(try String(contentsOf: hub.config, encoding: .utf8), config)

        // Saving the schedule without a password also moves a file left behind.
        let saver = FakePasswordStore()
        let second = HubControllerMaintenance(controller: hub.controller, passwordStore: saver)
        try writeLegacy(mode: 0o600)
        _ = try second.saveBackupSchedule(loaded, password: nil).get()
        XCTAssertEqual(saver.items, [name.path: Array(password.utf8)])
        XCTAssertTrue(try String(contentsOf: hub.config, encoding: .utf8).contains("password_source = \"keychain\""))
        XCTAssertFalse(FileManager.default.fileExists(atPath: name.path))

        // A file the configuration does not name as the app's own is never touched.
        let elsewhere = hub.folder.appendingPathComponent("other-password")
        try Data(password.utf8).write(to: elsewhere)
        try Data(HubBackupSchedule(enabled: true, destination: "/Volumes/B", passwordFile: elsewhere.path)
            .applied(to: config).utf8).write(to: hub.config)
        let untouched = FakePasswordStore()
        _ = try HubControllerMaintenance(controller: hub.controller, passwordStore: untouched).loadBackupSchedule().get()
        XCTAssertEqual(untouched.items, [:])
        XCTAssertTrue(FileManager.default.fileExists(atPath: elsewhere.path))
    }

    /// M06-T13: the app's `[backup]` section names the Keychain as the password source,
    /// replacing any other source and leaving the rest of the file as it was.
    func testSavedScheduleNamesTheKeychainPasswordSource() throws {
        let hub = try hubFolder("BackupPasswordSource")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let config = "data_dir = \"/Hub/data\"\n\n[backup]\n# chosen by hand\n  password_source = \"file\"\n"
            + "enabled = false\n\n[pairing]\npassword_source = \"file\"\n"
        try Data(config.utf8).write(to: hub.config)
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        let schedule = HubBackupSchedule(enabled: true, destination: "/Volumes/B")
        _ = try backend.saveBackupSchedule(schedule, password: "correct horse battery staple").get()
        let written = try String(contentsOf: hub.config, encoding: .utf8)
        let backupTable = written.components(separatedBy: "[pairing]")[0]
        XCTAssertTrue(backupTable.contains("  password_source = \"keychain\"\n"), written)
        XCTAssertFalse(backupTable.contains("\"file\""), written)
        XCTAssertTrue(written.hasSuffix("[pairing]\npassword_source = \"file\"\n"), "another table is untouched")
        XCTAssertTrue(written.contains("# chosen by hand"))
        XCTAssertEqual(store.items.count, 1, "the Keychain item itself is unchanged in shape")

        // Saving again changes nothing; a section without the key gets it right under the header.
        _ = try backend.saveBackupSchedule(try backend.loadBackupSchedule().get(), password: nil).get()
        XCTAssertEqual(try String(contentsOf: hub.config, encoding: .utf8), written)
        XCTAssertEqual(HubControllerMaintenance.namingKeychainSource(in: "[backup]\nkeep = 7\n"),
                       "[backup]\npassword_source = \"keychain\"\nkeep = 7\n")
        XCTAssertEqual(HubControllerMaintenance.namingKeychainSource(in: "data_dir = \"/d\"\n"), "data_dir = \"/d\"\n")
    }

    // MARK: M06-T17: the service binary owns the item; source, migration and refusals

    private func backupConfig(_ hub: (home: URL, folder: URL, config: URL, controller: HubController),
                              _ lines: String) throws -> String {
        let config = "data_dir = \"/Hub/data\"\n\n[backup]\nenabled = true\ndestination = \"/Volumes/B\"\n" + lines
        try Data(config.utf8).write(to: hub.config)
        return config
    }

    private func legacyFile(_ name: URL, _ text: String = "correct horse battery staple\n", mode: Int = 0o600) throws {
        try? FileManager.default.removeItem(at: name)
        try Data(text.utf8).write(to: name)
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: mode)], ofItemAtPath: name.path)
    }

    /// The app hands the password to the bundled service on standard input only; it never
    /// stores it itself and never puts it in an argument (DEC-123 option A).
    func testTheServiceStoreHandsThePasswordOnStandardInputOnly() throws {
        var calls: [([String], String?)] = []
        var answer = "{\"backupPassword\":\"stored\"}\n"
        let store = HubServiceBackupPasswordStore { arguments, input in
            calls.append((arguments, input))
            return .success(answer)
        }
        let password = "correct horse battery staple"
        try store.store(Array(password.utf8), account: "/Hub/backup-password")
        XCTAssertEqual(calls.last?.0, ["backup-password", "store", "--account", "/Hub/backup-password"])
        XCTAssertEqual(calls.last?.1, password + "\n")
        XCTAssertFalse(calls.last!.0.contains { $0.contains(password) })
        answer = "{\"backupPassword\":\"different\"}"
        XCTAssertEqual(try store.compare(Array(password.utf8), account: "unattended-backup"), .different)
        answer = "{\"backupPassword\":\"present\"}"
        XCTAssertTrue(try store.hasPassword(account: "unattended-backup"))
        XCTAssertNil(calls.last?.1, "status sends no password")
        answer = "{\"backupPassword\":\"stored-elsewhere\"}"
        XCTAssertThrowsError(try store.store(Array(password.utf8), account: "/Hub/backup-password"))
        let refusing = HubServiceBackupPasswordStore { _, _ in
            .failure(HubActionError.commandFailed("the Keychain refused to save the backup password"))
        }
        XCTAssertThrowsError(try refusing.store(Array(password.utf8), account: "/Hub/backup-password")) { error in
            guard case .passwordNotSaved = error as? HubMaintenanceError else { return XCTFail("\(error)") }
            XCTAssertFalse(error.localizedDescription.contains(password))
        }
    }

    /// REV-CX-M06-T13 findings 1 and 2: an explicit file source is never migrated, its file
    /// never removed and the source never rewritten by an ordinary save.
    func testAnExplicitFileSourceIsNeverMigratedOrRewritten() throws {
        let hub = try hubFolder("BackupFileSource")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        let config = try backupConfig(hub, "password_source = \"file\"\npassword_file = \(HubController.tomlBasicString(name.path))\n")
        try legacyFile(name)
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        let loaded = try backend.loadBackupSchedule().get()
        XCTAssertEqual(loaded.passwordSource, "file")
        XCTAssertNil(loaded.passwordNotice)
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))
        XCTAssertEqual(store.stores, 0)
        XCTAssertEqual(try String(contentsOf: hub.config, encoding: .utf8), config)
        var edited = loaded
        edited.intervalHours = 12
        XCTAssertNil(edited.problem(dataDirectory: nil))
        _ = try backend.saveBackupSchedule(edited, password: nil).get()
        let written = try String(contentsOf: hub.config, encoding: .utf8)
        XCTAssertTrue(written.contains("password_source = \"file\""), written)
        XCTAssertTrue(written.contains("interval_hours = 12"), written)
        XCTAssertEqual(HubBackupSchedule(config: written).passwordFile, name.path)
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))
        XCTAssertEqual(store.stores, 0)

        // A custom file path keeps its source and path through a save without a password.
        let custom = try backupConfig(hub, "password_source = \"file\"\npassword_file = \"/custom/password\"\n")
        _ = try backend.saveBackupSchedule(HubBackupSchedule(config: custom), password: nil).get()
        let kept = try String(contentsOf: hub.config, encoding: .utf8)
        XCTAssertEqual(kept.split(separator: "\n").filter { $0.contains("password_") },
                       ["password_source = \"file\"", "password_file = \"/custom/password\""], kept)
        XCTAssertEqual(store.stores, 0)
    }

    /// REV-CX-M06-T13 finding 2: an unknown source is shown on load and refused on save
    /// before anything changes.
    func testAnUnknownSourceIsShownAndRefusedBeforeAnyChange() throws {
        let hub = try hubFolder("BackupUnknownSource")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let config = try backupConfig(hub, "password_source = \"vault\"\n")
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        let loaded = try backend.loadBackupSchedule().get()
        XCTAssertTrue(loaded.passwordNotice?.contains("vault") == true)
        let saved = backend.saveBackupSchedule(loaded, password: "correct horse battery staple")
        XCTAssertEqual(saved.failureError as? HubMaintenanceError, .passwordSourceUnknown("vault"))
        XCTAssertEqual(store.stores, 0)
        XCTAssertEqual(try String(contentsOf: hub.config, encoding: .utf8), config)
    }

    /// REV-CX-M06-T13 finding 3: quoted keys are the same TOML key; the writer leaves one.
    func testQuotedSourceKeysAreOneKeyAndTheSavedSectionHasOne() throws {
        let hub = try hubFolder("BackupQuotedSource")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        for key in ["\"password_source\"", "'password_source'"] {
            _ = try backupConfig(hub, "\(key) = \"file\"\n\"keep\" = 4\n")
            let loaded = try backend.loadBackupSchedule().get()
            XCTAssertEqual(loaded.passwordSource, "file", key)
            XCTAssertEqual(loaded.keep, 4, key)
            _ = try backend.saveBackupSchedule(loaded, password: "correct horse battery staple").get()
            let written = try String(contentsOf: hub.config, encoding: .utf8)
            let sources = written.split(separator: "\n").filter { $0.contains("password_source") }
            XCTAssertEqual(sources, ["password_source = \"keychain\""], written)
            XCTAssertEqual(written.split(separator: "\n").filter { $0.contains("keep") }.count, 1, written)
            XCTAssertEqual(HubBackupSchedule(config: written).passwordSource, "keychain")
        }
        XCTAssertEqual(HubControllerMaintenance.naming(passwordSource: "keychain",
                                                       in: "[backup]\n\"password_source\" = \"file\"\npassword_source = \"x\"\n"),
                       "[backup]\npassword_source = \"keychain\"\n")
    }

    /// REV-CX-M06-T13 finding 4: a Keychain source without a path keeps its saved password
    /// through ordinary editing.
    func testAPathlessKeychainScheduleKeepsItsPasswordThroughEditing() throws {
        let hub = try hubFolder("BackupPathlessKeychain")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        _ = try backupConfig(hub, "password_source = \"keychain\"\n")
        let store = FakePasswordStore()
        store.items["unattended-backup"] = Array("correct horse battery staple".utf8)
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        var loaded = try backend.loadBackupSchedule().get()
        XCTAssertTrue(loaded.passwordStored)
        XCTAssertNil(loaded.passwordFile)
        loaded.intervalHours = 6
        XCTAssertNil(loaded.problem(dataDirectory: nil), "no password is demanded again")
        _ = try backend.saveBackupSchedule(loaded, password: nil).get()
        let written = try String(contentsOf: hub.config, encoding: .utf8)
        XCTAssertFalse(written.contains("password_file"), written)
        XCTAssertTrue(written.contains("password_source = \"keychain\""), written)
        XCTAssertEqual(store.stores, 0)
        // A new password replaces the same item.
        _ = try backend.saveBackupSchedule(loaded, password: "another long recovery password").get()
        XCTAssertEqual(store.items, ["unattended-backup": Array("another long recovery password".utf8)])
        XCTAssertFalse(try String(contentsOf: hub.config, encoding: .utf8).contains("password_file"))
    }

    /// REV-CX-M06-T12 finding 4: the app applies the service's ACL refusal.
    func testMigrationRefusesAnACLGrantAndAcceptsADenyOnlyList() throws {
        let hub = try hubFolder("BackupMigrationACL")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        _ = try backupConfig(hub, "password_file = \(HubController.tomlBasicString(name.path))\n")
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        func acl(_ entry: String) throws {
            let chmod = Process()
            chmod.executableURL = URL(fileURLWithPath: "/bin/chmod")
            chmod.arguments = ["+a", entry, name.path]
            try chmod.run()
            chmod.waitUntilExit()
            XCTAssertEqual(chmod.terminationStatus, 0)
        }
        for grant in ["group:everyone allow read", "group:everyone allow write"] {
            try legacyFile(name)
            try acl(grant)
            let loaded = try backend.loadBackupSchedule().get()
            XCTAssertEqual(loaded.passwordNotice, HubMaintenanceError.passwordFileRefused.localizedDescription, grant)
            XCTAssertEqual(store.stores, 0, grant)
            XCTAssertTrue(FileManager.default.fileExists(atPath: name.path), grant)
        }
        try legacyFile(name)
        try acl("group:everyone deny write")
        XCTAssertNil(try backend.loadBackupSchedule().get().passwordNotice)
        XCTAssertEqual(store.items, [name.path: Array("correct horse battery staple".utf8)])
        XCTAssertFalse(FileManager.default.fileExists(atPath: name.path))
    }

    /// REV-CX-M06-T12 findings 3, 5 and 6: an existing item wins over a reappearing file, an
    /// equal item finishes an interrupted move, and every failure or half-done move is shown.
    func testAnExistingItemWinsAndFailedOrHalfDoneMovesAreShown() throws {
        let hub = try hubFolder("BackupMigrationOrder")
        defer {
            try? FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o700)], ofItemAtPath: hub.folder.path)
            try? FileManager.default.removeItem(at: hub.home)
        }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        let config = try backupConfig(hub, "password_file = \(HubController.tomlBasicString(name.path))\n")
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)

        // Another password in the item: the item is used, the file stays, the conflict is shown.
        store.items[name.path] = Array("the newer saved password".utf8)
        try legacyFile(name)
        XCTAssertEqual(try backend.loadBackupSchedule().get().passwordNotice,
                       HubMaintenanceError.passwordFileConflict.localizedDescription)
        XCTAssertEqual(store.items[name.path], Array("the newer saved password".utf8))
        XCTAssertEqual(store.stores, 0)
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))

        // The same password in the item: an interrupted move is finished without a write.
        store.items[name.path] = Array("correct horse battery staple".utf8)
        XCTAssertNil(try backend.loadBackupSchedule().get().passwordNotice)
        XCTAssertEqual(store.stores, 0)
        XCTAssertFalse(FileManager.default.fileExists(atPath: name.path))

        // A store that fails keeps the file and says so.
        store.items = [:]
        store.failure = HubMaintenanceError.passwordNotSaved("the keychain is locked")
        try legacyFile(name)
        XCTAssertTrue(try backend.loadBackupSchedule().get().passwordNotice?.contains("the keychain is locked") == true)
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))

        // Saved but not removable: both copies remain and that is shown; the next open,
        // with the folder writable again, finishes the move.
        store.failure = nil
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o500)], ofItemAtPath: hub.folder.path)
        XCTAssertEqual(try backend.loadBackupSchedule().get().passwordNotice,
                       HubMaintenanceError.passwordFileNotRemoved.localizedDescription)
        XCTAssertEqual(store.items[name.path], Array("correct horse battery staple".utf8))
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))
        try FileManager.default.setAttributes([.posixPermissions: NSNumber(value: 0o700)], ofItemAtPath: hub.folder.path)
        XCTAssertNil(try backend.loadBackupSchedule().get().passwordNotice)
        XCTAssertFalse(FileManager.default.fileExists(atPath: name.path))
        XCTAssertEqual(try String(contentsOf: hub.config, encoding: .utf8), config)

        // The section shows the notice when it opens.
        let fake = FakeMaintenanceBackend()
        fake.schedule.passwordNotice = HubMaintenanceError.passwordFileConflict.localizedDescription
        let view = makeView(fake)
        view.reload()
        XCTAssertEqual(view.noteLabel.stringValue, HubMaintenanceError.passwordFileConflict.localizedDescription)
    }

    /// REV-CX-M06-T12 finding 7: a file that grows or is replaced during the move is left
    /// in place and reported; only the held, unchanged file is ever removed.
    func testMigrationRefusesAFileThatGrowsOrIsReplaced() throws {
        let hub = try hubFolder("BackupMigrationIdentity")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        _ = try backupConfig(hub, "password_file = \(HubController.tomlBasicString(name.path))\n")
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)

        try legacyFile(name)
        store.duringStore = {
            let handle = try! FileHandle(forWritingTo: name)
            handle.seekToEndOfFile()
            handle.write(Data("more".utf8))
            try! handle.close()
        }
        XCTAssertEqual(try backend.loadBackupSchedule().get().passwordNotice,
                       HubMaintenanceError.passwordFileChanged.localizedDescription)
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))

        store.items = [:]
        try legacyFile(name)
        let replacement = hub.folder.appendingPathComponent("replacement")
        store.duringStore = {
            try! Data("someone else's file".utf8).write(to: replacement)
            _ = rename(replacement.path, name.path)
        }
        XCTAssertEqual(try backend.loadBackupSchedule().get().passwordNotice,
                       HubMaintenanceError.passwordFileChanged.localizedDescription)
        XCTAssertEqual(try String(contentsOf: name, encoding: .utf8), "someone else's file", "the other file stays")
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: hub.folder.path).sorted(),
                       ["backup-password", "config.toml"], "nothing is left aside")
    }

    /// REV-CX-M06-T17 CR-2: a basic quoted key with escapes is the same TOML key, as the
    /// service's loader reads it: an escaped file source is never migrated, an escaped
    /// unknown source is refused before anything changes, and a save leaves one key.
    func testEscapedSourceKeysAreTheSameKeyAsTheServiceReads() throws {
        let hub = try hubFolder("BackupEscapedSource")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        let escaped = #""password_source""#
        XCTAssertEqual(HubBackupSchedule.keyName(of: "\(escaped) = \"file\""), "password_source")
        XCTAssertEqual(HubBackupSchedule.keyName(of: #""pass\x77ord_source" = 1"#), "password_source")
        XCTAssertEqual(HubBackupSchedule.keyName(of: #""password\q_source" = 1"#), #""password\q_source""#)

        let config = try backupConfig(hub, "\(escaped) = \"file\"\npassword_file = \(HubController.tomlBasicString(name.path))\n")
        try legacyFile(name)
        let loaded = try backend.loadBackupSchedule().get()
        XCTAssertEqual(loaded.passwordSource, "file")
        XCTAssertNil(loaded.passwordNotice)
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path), "an explicit file source is never moved")
        XCTAssertEqual(store.stores, 0)
        XCTAssertEqual(try String(contentsOf: hub.config, encoding: .utf8), config)
        var edited = loaded
        edited.intervalHours = 12
        _ = try backend.saveBackupSchedule(edited, password: nil).get()
        var written = try String(contentsOf: hub.config, encoding: .utf8)
        XCTAssertEqual(written.split(separator: "\n").filter { HubBackupSchedule.keyName(of: String($0)) == "password_source" },
                       ["\(escaped) = \"file\""], written)
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))
        // Typing a password is the owner's switch: still one source key, now the Keychain.
        _ = try backend.saveBackupSchedule(edited, password: "correct horse battery staple").get()
        written = try String(contentsOf: hub.config, encoding: .utf8)
        XCTAssertEqual(written.split(separator: "\n").filter { HubBackupSchedule.keyName(of: String($0)) == "password_source" },
                       ["password_source = \"keychain\""], written)

        let unknown = try backupConfig(hub, "\(escaped) = \"vault\"\n")
        XCTAssertTrue(try backend.loadBackupSchedule().get().passwordNotice?.contains("vault") == true)
        let refused = backend.saveBackupSchedule(HubBackupSchedule(config: unknown), password: "correct horse battery staple")
        XCTAssertEqual(refused.failureError as? HubMaintenanceError, .passwordSourceUnknown("vault"))
        XCTAssertEqual(try String(contentsOf: hub.config, encoding: .utf8), unknown)
    }

    /// REV-CX-M06-T17 CR-3: a writer that changes the file between the rename aside and the
    /// removal is caught: the changed file is put back and kept, and the change is shown.
    func testAChangeAtTheRenameBoundaryKeepsTheFile() throws {
        let hub = try hubFolder("BackupRenameBoundary")
        defer {
            HubHeldPasswordFile.afterRenameAside = nil
            try? FileManager.default.removeItem(at: hub.home)
        }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        _ = try backupConfig(hub, "password_file = \(HubController.tomlBasicString(name.path))\n")
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        for change in ["append", "rewrite"] {
            store.items = [:]
            try legacyFile(name)
            HubHeldPasswordFile.afterRenameAside = { path in
                let handle = FileHandle(forUpdatingAtPath: path)!
                if change == "append" { handle.seekToEndOfFile() }
                handle.write(Data((change == "append" ? "more" : "CORRECT").utf8))
                handle.closeFile()
            }
            XCTAssertEqual(try backend.loadBackupSchedule().get().passwordNotice,
                           HubMaintenanceError.passwordFileChanged.localizedDescription, change)
            let kept = try String(contentsOf: name, encoding: .utf8)
            XCTAssertEqual(kept, change == "append" ? "correct horse battery staple\nmore" : "CORRECT horse battery staple\n", change)
            XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: hub.folder.path).sorted(),
                           ["backup-password", "config.toml"], change)
        }
    }

    /// REV-CX-M06-T17 CR-4: a move interrupted after the rename aside is found on the next
    /// open and finished with the same checks; one that cannot finish, or a rename back that
    /// fails, is shown with the path of the file left aside.
    func testAnInterruptedMoveIsFoundAndFinishedOrShown() throws {
        let hub = try hubFolder("BackupInterruptedMove")
        defer {
            HubHeldPasswordFile.afterRenameAside = nil
            try? FileManager.default.removeItem(at: hub.home)
        }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        _ = try backupConfig(hub, "password_file = \(HubController.tomlBasicString(name.path))\n")
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        let aside = hub.folder.appendingPathComponent(".backup-password.removing-0F0F0F0F-1111-2222-3333-444455556666")
        func listing() throws -> [String] { try FileManager.default.contentsOfDirectory(atPath: hub.folder.path).sorted() }

        // Stopped after the rename, the item saved: the next open removes the aside file.
        store.items[name.path] = Array("correct horse battery staple".utf8)
        try legacyFile(aside)
        var loaded = try backend.loadBackupSchedule().get()
        XCTAssertNil(loaded.passwordNotice)
        XCTAssertTrue(loaded.passwordStored)
        XCTAssertEqual(try listing(), ["config.toml"])

        // An aside file that differs from the item stays and is named.
        store.items[name.path] = Array("the newer saved password".utf8)
        try legacyFile(aside)
        loaded = try backend.loadBackupSchedule().get()
        XCTAssertEqual(loaded.passwordNotice, HubMaintenanceError.passwordFileLeftAside(aside.path).localizedDescription)
        XCTAssertTrue(FileManager.default.fileExists(atPath: aside.path))
        try FileManager.default.removeItem(at: aside)

        // A rename back that fails (the name taken meanwhile) names where the file is.
        store.items = [:]
        try legacyFile(name)
        HubHeldPasswordFile.afterRenameAside = { path in
            let handle = FileHandle(forUpdatingAtPath: path)!
            handle.seekToEndOfFile()
            handle.write(Data("more".utf8))
            handle.closeFile()
            FileManager.default.createFile(atPath: name.path, contents: Data("new".utf8))
        }
        loaded = try backend.loadBackupSchedule().get()
        HubHeldPasswordFile.afterRenameAside = nil
        let left = try listing().filter { $0.hasPrefix(".backup-password.removing-") }
        XCTAssertEqual(left.count, 1)
        XCTAssertTrue(loaded.passwordNotice?.contains(left[0]) == true, loaded.passwordNotice ?? "")
        XCTAssertEqual(try String(contentsOf: hub.folder.appendingPathComponent(left[0]), encoding: .utf8),
                       "correct horse battery staple\nmore", "the changed file is kept")
        try FileManager.default.removeItem(at: name)

        // The next open finds it again; under a file source it is only shown, never moved.
        _ = try backupConfig(hub, "password_source = \"file\"\npassword_file = \"/custom/password\"\n")
        loaded = try backend.loadBackupSchedule().get()
        XCTAssertTrue(loaded.passwordNotice?.contains(left[0]) == true, loaded.passwordNotice ?? "")
        XCTAssertEqual(try listing().filter { $0.hasPrefix(".backup-password.removing-") }, left)
    }

    /// REV-CX-M06-T17 CR-6: an item equal to the file but refused by the service as unusable
    /// keeps the file and shows the refusal.
    func testAnEqualButUnusableItemKeepsTheFile() throws {
        let hub = try hubFolder("BackupEqualUnusable")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        _ = try backupConfig(hub, "password_file = \(HubController.tomlBasicString(name.path))\n")
        let store = FakePasswordStore()
        store.items[name.path] = Array("short".utf8)
        store.compareFailure = HubMaintenanceError.passwordNotSaved("the backup password in the Keychain cannot be read")
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        try legacyFile(name, "short\n")
        let loaded = try backend.loadBackupSchedule().get()
        XCTAssertTrue(loaded.passwordNotice?.contains("cannot be read") == true, loaded.passwordNotice ?? "")
        XCTAssertTrue(FileManager.default.fileExists(atPath: name.path))
        XCTAssertEqual(store.stores, 0)
    }

    /// REV-CX-M06-T17 CR-7: a password in use with a CR, LF or NUL inside moves with its exact
    /// bytes, and the standard input the service reads gives those bytes back.
    func testALegacyPasswordWithLineEndsOrNulMovesExactly() throws {
        let hub = try hubFolder("BackupLegacyControl")
        defer { try? FileManager.default.removeItem(at: hub.home) }
        let name = hub.folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        _ = try backupConfig(hub, "password_file = \(HubController.tomlBasicString(name.path))\n")
        let store = FakePasswordStore()
        let backend = HubControllerMaintenance(controller: hub.controller, passwordStore: store)
        for password in ["correct horse\nbattery staple", "correct horse\rbattery staple", "correct horse\u{0}battery staple",
                         "correct horse battery staple\n"] {
            store.items = [:]
            try legacyFile(name, password + "\n")
            XCTAssertNil(try backend.loadBackupSchedule().get().passwordNotice, password)
            XCTAssertEqual(store.items[name.path], Array(password.utf8), password)
            XCTAssertFalse(FileManager.default.fileExists(atPath: name.path), password)
        }
        // The service drops one final LF or CR LF from standard input.
        func served(_ input: String) -> [UInt8] {
            var bytes = Array(input.utf8)
            if bytes.last == UInt8(ascii: "\n") {
                bytes.removeLast()
                if bytes.last == UInt8(ascii: "\r") { bytes.removeLast() }
            }
            return bytes
        }
        for password in ["plain recovery words", "ends in CR\r", "ends in LF\n", "ends in CR LF\r\n", "inner\nline\u{0}"] {
            let input = try XCTUnwrap(HubServiceBackupPasswordStore.standardInput(for: Array(password.utf8)))
            XCTAssertEqual(served(input), Array(password.utf8), password.debugDescription)
        }
    }

    func testTheControllerRunsTheForwardedBackupVerbs() throws {
        let home = HubTestScratch.fixtureRoot("BackupVerbs")
        defer { try? FileManager.default.removeItem(at: home) }
        let runner = RecordingMaintenanceRunner()
        runner.answer = { arguments in
            arguments.last == "backup-status"
                ? .success(#"{"enabled":false,"configured":false,"running":false,"lastGeneration":"g-2"}"#)
                : .success(#"{"status":"complete","generation":"g-3","trigger":"manual"}"#)
        }
        let controller = HubController(environment: [:], commandRunner: runner, launchAgent: nil,
                                       dataRemover: nil, homeDirectory: home, serviceInstalledOverride: true)
        let backend = HubControllerMaintenance(controller: controller)
        let status = expectation(description: "status")
        backend.readBackupStatus { result in
            XCTAssertEqual(try? result.get().lastGeneration, "g-2")
            status.fulfill()
        }
        let run = expectation(description: "run")
        backend.backUpNow { state in
            XCTAssertEqual(state, .finished(generation: "g-3"))
            run.fulfill()
        }
        wait(for: [status, run], timeout: 2)
        XCTAssertEqual(runner.arguments.map { Array($0.dropFirst(2)) }, [["backup-status"], ["backup-now"]])
        XCTAssertTrue(runner.arguments.allSatisfy { $0.first == "--config" && $0[1].hasSuffix("config.toml") })
        XCTAssertFalse(runner.arguments.joined().contains { $0.contains("password") && $0 != "--password-file" },
                       "no password on the command line")
    }
}

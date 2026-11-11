// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

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
        done.lastSuccess = Date()
        done.lastAttempt = Date()
        backend.statusAnswers = [.success(running), .failure(HubActionError.commandTimedOut), .success(done)]
        for _ in 0..<3 {
            XCTAssertEqual(pending.count, 1)
            pending.removeFirst()()
        }
        XCTAssertTrue(pending.isEmpty, "polling stops when the run has ended")
        XCTAssertEqual(view.runState, .finished(generation: "generation-9"))
        XCTAssertTrue(view.backUpNowButton.isEnabled)
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
        let message = "backups wait until the refused credentials are removed: run teslatlas-hub purge-refused-credentials --confirm"
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

    func testThePasswordFileIsOwnerOnlyAndTheConfigurationNamesIt() throws {
        let home = HubTestScratch.fixtureRoot("BackupSettings")
        let folder = home.appendingPathComponent("Library/Application Support/Teslatlas Hub", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let config = folder.appendingPathComponent("config.toml")
        try Data("data_dir = \"/Hub/data\"\n".utf8).write(to: config)
        let controller = HubController(environment: [:], commandRunner: RecordingMaintenanceRunner(),
                                       launchAgent: nil, dataRemover: nil, homeDirectory: home,
                                       serviceInstalledOverride: true)
        let backend = HubControllerMaintenance(controller: controller)
        let password = "correct horse battery staple"
        let saved = try backend.saveBackupSchedule(HubBackupSchedule(enabled: true, destination: "/Volumes/B"),
                                                   password: password).get()
        let file = folder.appendingPathComponent(HubControllerMaintenance.passwordFileName)
        XCTAssertEqual(saved.passwordFile, file.path)
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), password)
        let mode = try XCTUnwrap(FileManager.default.attributesOfItem(atPath: file.path)[.posixPermissions] as? NSNumber)
        XCTAssertEqual(mode.intValue & 0o777, 0o600)
        let written = try String(contentsOf: config, encoding: .utf8)
        XCTAssertFalse(written.contains(password))
        XCTAssertEqual(HubBackupSchedule(config: written).passwordFile, file.path)
        XCTAssertEqual(try backend.loadBackupSchedule().get(), saved)
        XCTAssertThrowsError(try HubControllerMaintenance.writePasswordFile("short", to: file))
        XCTAssertEqual(try String(contentsOf: file, encoding: .utf8), password, "a refused password changes nothing")
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: folder.path).filter { $0.hasSuffix(".tmp") }
        XCTAssertEqual(leftovers, [])
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

// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import Darwin
import XCTest
@testable import Teslatlas_Hub

/// RD1, the plan table, the unload poll, the launchctl adapter and Delete Hub and Data's
/// order, against a scripted agent, launchctl, preflight and remover on a manual clock
/// and scheduler. Nothing here reaches the real launchd, home or preferences.
final class HubServiceLifecycleTests: XCTestCase {
    // MARK: - launchctl adapter

    func testDefaultLaunchctlRunnerRefusesUnderXCTestWithoutSpawning() {
        var spawns = 0
        let runner = HubLaunchctlRunner(spawn: { _, _ in spawns += 1 })
        var result: Result<String, Error>?
        runner.run(arguments: ["print", "gui/501/x"], timeout: 30) { result = $0 }
        guard case .failure? = result else { return XCTFail("a test host must not run launchctl") }
        XCTAssertEqual(spawns, 0)
    }

    func testDefaultLaunchctlRunnerForwardsBoundedDeadlineToSpawn() {
        var recorded: [HubLaunchctlInvocation] = []
        var completions: [(Result<String, Error>) -> Void] = []
        var now: UInt64 = 5_000_000_000
        let runner = HubLaunchctlRunner(
            spawn: { invocation, completion in recorded.append(invocation); completions.append(completion) },
            environment: [:],
            uptime: { now }
        )
        XCTAssertEqual(runner.cleanupReserve, 1.0, accuracy: 0.0001)

        var outcomes: [Result<String, Error>] = []
        runner.run(arguments: ["print", "S"], timeout: 30) { outcomes.append($0) }
        // The spawn runs later (queue delay); the deadline must not renew.
        now += 7_000_000_000
        runner.run(arguments: ["enable", "S"], timeout: 12.5) { outcomes.append($0) }
        let first = recorded[0]
        XCTAssertEqual(first.executable, URL(fileURLWithPath: "/bin/launchctl"))
        XCTAssertEqual(first.arguments, ["print", "S"])
        XCTAssertEqual(first.waitLimit, 29, accuracy: 0.0001)
        XCTAssertEqual(first.terminationGrace, 0.25, accuracy: 0.0001)
        XCTAssertEqual(first.outputDrainTimeout, 0.25, accuracy: 0.0001)
        XCTAssertEqual(first.waitDeadlineUptimeNanoseconds, 5_000_000_000 + 29_000_000_000)
        XCTAssertEqual(first.finalDeadlineUptimeNanoseconds, 5_000_000_000 + 30_000_000_000,
                       "the whole budget (wait plus cleanup) ends at entry plus the caller's timeout")
        let second = recorded[1]
        XCTAssertEqual(second.waitLimit, 11.5, accuracy: 0.0001)
        XCTAssertEqual(second.waitDeadlineUptimeNanoseconds, 12_000_000_000 + 11_500_000_000,
                       "the deadline is fixed at adapter entry")
        XCTAssertEqual(second.finalDeadlineUptimeNanoseconds, 12_000_000_000 + 12_500_000_000)

        // At or below the cleanup reserve nothing is spawned.
        for timeout in [1.0, 0.5, 0.0, -3.0] {
            runner.run(arguments: ["print", "S"], timeout: timeout) { outcomes.append($0) }
        }
        XCTAssertEqual(recorded.count, 2)
        XCTAssertEqual(outcomes.count, 4)
        for outcome in outcomes {
            guard case let .failure(error) = outcome, case HubActionError.commandTimedOut = error else {
                return XCTFail("a budget at or below the reserve is a timeout without a spawn")
            }
        }
        completions[0](.success("a")); completions[0](.success("b"))
        XCTAssertEqual(outcomes.count, 5, "an adapter completion is delivered once")
    }

    // MARK: - Run limit on an injected clock

    /// A manual monotonic clock for the executor. A wait on a semaphore ends when the
    /// semaphore is signalled or when the clock has reached the deadline; the decision
    /// never depends on how long the host really took (the 2 ms poll only re-reads the
    /// clock). The executor's waits and signals are recorded as events, and the test
    /// side blocks on events with a generous real-time watchdog that only detects a hang.
    private final class ManualProcessClock: @unchecked Sendable {
        enum Event: Equatable {
            case wait(deadline: UInt64)
            case terminate
            case kill
        }

        private let condition = NSCondition()
        private var current: UInt64
        private var recorded: [Event] = []

        /// Starts at the real uptime, so a deadline derived from it is also sensible if the
        /// injection were ignored.
        init(start: UInt64 = DispatchTime.now().uptimeNanoseconds) { current = start }

        var now: UInt64 { condition.lock(); defer { condition.unlock() }; return current }
        var events: [Event] { condition.lock(); defer { condition.unlock() }; return recorded }

        func advance(to value: UInt64) {
            condition.lock()
            current = max(current, value)
            condition.broadcast()
            condition.unlock()
        }

        private func record(_ event: Event) {
            condition.lock()
            recorded.append(event)
            condition.broadcast()
            condition.unlock()
        }

        /// Blocks until the events satisfy the predicate. The watchdog is real time and
        /// generous: it detects a hang, it is never an acceptance bound.
        @discardableResult
        func await(watchdog: TimeInterval = 30, _ predicate: ([Event]) -> Bool) -> Bool {
            let limit = Date().addingTimeInterval(watchdog)
            condition.lock(); defer { condition.unlock() }
            while !predicate(recorded) {
                if !condition.wait(until: limit) { return predicate(recorded) }
            }
            return true
        }

        var primitives: HubProcessPrimitives {
            HubProcessPrimitives(
                now: { [self] in now },
                wait: { [self] semaphore, deadline in
                    record(.wait(deadline: deadline))
                    while true {
                        if semaphore.wait(timeout: .now() + 0.002) == .success { return .success }
                        if now >= deadline { return .timedOut }
                    }
                },
                terminate: { [self] process in record(.terminate); process.terminate() },
                kill: { [self] pid in record(.kill); _ = Darwin.kill(pid, SIGKILL) }
            )
        }
    }

    private static func nanoseconds(_ seconds: TimeInterval) -> UInt64 { UInt64(seconds * 1_000_000_000) }

    /// Waits (real-time watchdog only) for a file the child creates once it is ready.
    private func awaitFile(_ url: URL, file: StaticString = #filePath, line: UInt = #line) {
        let limit = Date().addingTimeInterval(30)
        while !FileManager.default.fileExists(atPath: url.path) {
            guard Date() < limit else { return XCTFail("the child never signalled readiness", file: file, line: line) }
            Thread.sleep(forTimeInterval: 0.002)
        }
    }

    private func readinessMarker() -> URL {
        HubTestScratch.directory
            .appendingPathComponent("teslatlas-hub-ready-\(UUID().uuidString)")
    }

    /// A child that ignores SIGTERM and says when the trap is installed (the disposition
    /// survives the exec into sleep), so TERM can only be ignored, never lost to a race.
    private func termIgnoringChild(marker: URL) -> (executable: String, arguments: [String]) {
        ("/bin/sh", ["-c", "trap '' TERM; : > \"$1\"; exec /bin/sleep 60", "sh", marker.path])
    }

    private final class OutcomeBox: @unchecked Sendable {
        private let lock = NSLock()
        private var value: Result<String, Error>?
        var outcome: Result<String, Error>? { lock.lock(); defer { lock.unlock() }; return value }
        func set(_ result: Result<String, Error>) { lock.lock(); value = result; lock.unlock() }
    }

    private func assertTimedOut(_ outcome: Result<String, Error>?, file: StaticString = #filePath, line: UInt = #line) {
        guard case let .failure(error)? = outcome, case HubActionError.commandTimedOut = error else {
            return XCTFail("expected a timeout: \(String(describing: outcome))", file: file, line: line)
        }
    }

    /// The child is gone and was reaped: the termination handler ran (it precedes the
    /// completion), nothing is running, and the pid no longer exists.
    private func assertReaped(_ process: Process, signal expected: Int32,
                              file: StaticString = #filePath, line: UInt = #line) {
        let pid = process.processIdentifier
        XCTAssertGreaterThan(pid, 0, "the child was spawned", file: file, line: line)
        XCTAssertFalse(process.isRunning, "the child must be gone", file: file, line: line)
        XCTAssertEqual(Darwin.kill(pid, 0), -1, file: file, line: line)
        XCTAssertEqual(errno, ESRCH, "the child must be reaped", file: file, line: line)
        XCTAssertEqual(process.terminationReason, .uncaughtSignal, file: file, line: line)
        XCTAssertEqual(process.terminationStatus, expected, "ended by signal \(process.terminationStatus)",
                       file: file, line: line)
    }

    func testLaunchctlChildIsTerminatedAndReapedWhenTheInjectedClockPassesItsRunLimit() throws {
        let clock = ManualProcessClock()
        let process = Process()
        defer { Self.killSurvivor(process) }
        let entry = clock.now
        let runner = HubLaunchctlRunner(
            spawn: HubLaunchctlRunner.processSpawn(executable: URL(fileURLWithPath: "/bin/sleep"),
                                                   processFactory: { process }, primitives: clock.primitives),
            terminationGrace: 0.025, outputDrainTimeout: 0.025, environment: [:], uptime: { clock.now })
        let finished = expectation(description: "settled")
        let box = OutcomeBox()
        // Run limit 1 s; the ordinary wait ends 0.1 s earlier (2 x grace + 2 x drain reserve).
        runner.run(arguments: ["60"], timeout: 1.0) { box.set($0); finished.fulfill() }
        let waitDeadline = entry + Self.nanoseconds(0.9)

        // The child is running and the executor is waiting on the injected clock.
        XCTAssertTrue(clock.await { $0 == [.wait(deadline: waitDeadline)] }, "\(clock.events)")
        XCTAssertTrue(process.isRunning)
        XCTAssertNil(box.outcome, "nothing is decided while the clock has not reached the limit")
        XCTAssertEqual(clock.events, [.wait(deadline: waitDeadline)], "no signal before the limit")

        clock.advance(to: waitDeadline)
        wait(for: [finished], timeout: 30)

        assertTimedOut(box.outcome)
        let grace = waitDeadline + Self.nanoseconds(0.025)
        XCTAssertEqual(clock.events, [.wait(deadline: waitDeadline), .terminate,
                                      .wait(deadline: grace), .wait(deadline: grace)],
                       "TERM at the limit, acknowledged by the reap; no KILL for a child that obeys")
        assertReaped(process, signal: SIGTERM)
    }

    func testLaunchctlRunnerKillsATermIgnoringChildWhenTheInjectedClockPassesItsGrace() throws {
        let clock = ManualProcessClock()
        let process = Process()
        defer { Self.killSurvivor(process) }
        let marker = readinessMarker()
        defer { try? FileManager.default.removeItem(at: marker) }
        let child = termIgnoringChild(marker: marker)
        let entry = clock.now
        let runner = HubLaunchctlRunner(
            spawn: HubLaunchctlRunner.processSpawn(executable: URL(fileURLWithPath: child.executable),
                                                   processFactory: { process }, primitives: clock.primitives),
            terminationGrace: 0.025, outputDrainTimeout: 0.025, environment: [:], uptime: { clock.now })
        let finished = expectation(description: "settled")
        let box = OutcomeBox()
        runner.run(arguments: child.arguments, timeout: 1.0) { box.set($0); finished.fulfill() }
        let waitDeadline = entry + Self.nanoseconds(0.9)
        let grace = waitDeadline + Self.nanoseconds(0.025)

        XCTAssertTrue(clock.await { $0 == [.wait(deadline: waitDeadline)] }, "\(clock.events)")
        awaitFile(marker)

        // The limit: TERM is sent and, being ignored, is not acknowledged by an exit.
        clock.advance(to: waitDeadline)
        XCTAssertTrue(clock.await { $0 == [.wait(deadline: waitDeadline), .terminate, .wait(deadline: grace)] },
                      "\(clock.events)")
        XCTAssertTrue(process.isRunning, "the child ignored TERM")
        XCTAssertNil(box.outcome)

        // Only when the injected clock passes the grace is the child killed and reaped.
        let afterKill = grace + Self.nanoseconds(0.025)
        clock.advance(to: grace)
        wait(for: [finished], timeout: 30)

        assertTimedOut(box.outcome)
        XCTAssertEqual(clock.events, [.wait(deadline: waitDeadline), .terminate, .wait(deadline: grace),
                                      .kill, .wait(deadline: afterKill), .wait(deadline: afterKill)])
        assertReaped(process, signal: SIGKILL)
    }

    func testRunLimitHoldsWhenTheHostDelaysPreparationPastTheDeadline() throws {
        // A loaded host: preparation alone outlasts the wait deadline. The clock is moved
        // past it inside the process factory, so the budget is spent before any spawn.
        let clock = ManualProcessClock()
        let process = Process()
        defer { Self.killSurvivor(process) }
        let entry = clock.now
        let runner = HubLaunchctlRunner(
            spawn: HubLaunchctlRunner.processSpawn(
                executable: URL(fileURLWithPath: "/bin/sleep"),
                processFactory: { clock.advance(to: entry + Self.nanoseconds(0.95)); return process },
                primitives: clock.primitives),
            terminationGrace: 0.025, outputDrainTimeout: 0.025, environment: [:], uptime: { clock.now })
        let finished = expectation(description: "settled")
        let box = OutcomeBox()
        runner.run(arguments: ["60"], timeout: 1.0) { box.set($0); finished.fulfill() }
        wait(for: [finished], timeout: 30)

        assertTimedOut(box.outcome)
        XCTAssertEqual(process.processIdentifier, 0, "an expired spawn is refused")
        XCTAssertEqual(clock.events, [], "no wait and no signal: the child never existed")
    }

    func testExecutorWaitsUntilTheAbsoluteDeadlineItWasGivenNotAFreshTimeout() throws {
        // The caller's wait deadline is absolute and partly spent; `timeout` is 60 s.
        let clock = ManualProcessClock()
        let process = Process()
        defer { Self.killSurvivor(process) }
        let deadline = clock.now + Self.nanoseconds(0.5)
        let finished = expectation(description: "settled")
        let box = OutcomeBox()
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/bin/sleep"),
            arguments: ["60"],
            timeout: 60,
            waitDeadline: DispatchTime(uptimeNanoseconds: deadline),
            terminationGrace: 0.05,
            outputDrainTimeout: 0.05,
            processFactory: { process },
            primitives: clock.primitives
        ) { box.set($0); finished.fulfill() }

        XCTAssertTrue(clock.await { $0 == [.wait(deadline: deadline)] },
                      "the wait ends at the given deadline, never now plus the timeout: \(clock.events)")
        clock.advance(to: deadline)
        wait(for: [finished], timeout: 30)
        assertTimedOut(box.outcome)
        assertReaped(process, signal: SIGTERM)
    }

    func testExecutorRefusesToSpawnOnceTheCallersBudgetIsSpent() {
        // The wait deadline is already behind us: the child is never started.
        let process = Process()
        let finished = expectation(description: "refused")
        var outcome: Result<String, Error>?
        let past = DispatchTime(uptimeNanoseconds: DispatchTime.now().uptimeNanoseconds - 1_000_000_000)
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/bin/sleep"),
            arguments: ["60"],
            timeout: 60,
            waitDeadline: past,
            finalDeadline: past,
            terminationGrace: 0.05,
            outputDrainTimeout: 0.05,
            processFactory: { process }
        ) { result in outcome = result; finished.fulfill() }
        wait(for: [finished], timeout: 5)
        defer { Self.killSurvivor(process) }
        guard case let .failure(error)? = outcome, case HubActionError.commandTimedOut = error else {
            return XCTFail("an expired spawn is a timeout: \(String(describing: outcome))")
        }
        XCTAssertEqual(process.processIdentifier, 0, "no child may be started after the deadline")
    }

    func testExecutorRefusesToSpawnWhenPreparationConsumesTheBudget() {
        // Preparation (a slow process factory) moves the injected clock past the wait deadline.
        let clock = ManualProcessClock()
        let process = Process()
        defer { Self.killSurvivor(process) }
        let start = clock.now
        let finished = expectation(description: "refused after slow preparation")
        let box = OutcomeBox()
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/bin/sleep"),
            arguments: ["60"],
            timeout: 60,
            waitDeadline: DispatchTime(uptimeNanoseconds: start + Self.nanoseconds(0.1)),
            finalDeadline: DispatchTime(uptimeNanoseconds: start + Self.nanoseconds(0.2)),
            terminationGrace: 0.05,
            outputDrainTimeout: 0.05,
            processFactory: { clock.advance(to: start + Self.nanoseconds(0.3)); return process },
            primitives: clock.primitives
        ) { box.set($0); finished.fulfill() }
        wait(for: [finished], timeout: 30)
        assertTimedOut(box.outcome)
        XCTAssertEqual(process.processIdentifier, 0, "an expired preparation must not start the child")
        XCTAssertEqual(clock.events, [])
    }

    func testExecutorCapsCleanupWaitsByTheRemainingBudget() throws {
        // The child ignores TERM. Its grace and drain are 5 s each, but the caller's whole
        // budget ends 0.2 s after the wait deadline: no cleanup wait may reach past it.
        let clock = ManualProcessClock()
        let process = Process()
        defer { Self.killSurvivor(process) }
        let marker = readinessMarker()
        defer { try? FileManager.default.removeItem(at: marker) }
        let child = termIgnoringChild(marker: marker)
        let start = clock.now
        let waitDeadline = start + Self.nanoseconds(1.5)
        let finalDeadline = start + Self.nanoseconds(1.7)
        let finished = expectation(description: "capped cleanup")
        let box = OutcomeBox()
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: child.executable),
            arguments: child.arguments,
            timeout: 60,
            waitDeadline: DispatchTime(uptimeNanoseconds: waitDeadline),
            finalDeadline: DispatchTime(uptimeNanoseconds: finalDeadline),
            terminationGrace: 5,
            outputDrainTimeout: 5,
            processFactory: { process },
            primitives: clock.primitives
        ) { box.set($0); finished.fulfill() }

        XCTAssertTrue(clock.await { $0 == [.wait(deadline: waitDeadline)] }, "\(clock.events)")
        awaitFile(marker)
        clock.advance(to: waitDeadline)
        // TERM's grace is 5 s, but only 0.2 s of the budget remain.
        XCTAssertTrue(clock.await { $0 == [.wait(deadline: waitDeadline), .terminate, .wait(deadline: finalDeadline)] },
                      "the TERM grace is cut to the remaining budget: \(clock.events)")
        clock.advance(to: finalDeadline)
        wait(for: [finished], timeout: 30)

        assertTimedOut(box.outcome)
        let events = clock.events
        XCTAssertTrue(events.contains(.kill), "the TERM-ignoring child is still killed: \(events)")
        for case let .wait(deadline) in events.dropFirst() {
            XCTAssertLessThanOrEqual(deadline, finalDeadline + 1_000_000,
                                     "a cleanup wait renewed the budget: \(events)")
        }
        assertReaped(process, signal: SIGKILL)
    }

    // MARK: - Quiescence (F6-3)

    func testExecutorDoesNotReportAChildFinishedWhileItCanStillRun() throws {
        // TERM and KILL are not delivered (the primitives are inert), so the child cannot be reaped
        // within the budget. The executor must not report it finished: it reports "not quiescent" and
        // the quiescence turns true only once the child has really gone.
        let clock = ManualProcessClock()
        let process = Process()
        defer { Self.killSurvivor(process) }
        let marker = readinessMarker()
        defer { try? FileManager.default.removeItem(at: marker) }
        let child = termIgnoringChild(marker: marker)
        var primitives = clock.primitives
        primitives.terminate = { _ in }
        primitives.kill = { _ in }
        let start = clock.now
        let waitDeadline = start + Self.nanoseconds(1.5)
        let finalDeadline = start + Self.nanoseconds(1.7)
        let finished = expectation(description: "reported")
        let box = OutcomeBox()
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: child.executable),
            arguments: child.arguments,
            timeout: 60,
            waitDeadline: DispatchTime(uptimeNanoseconds: waitDeadline),
            finalDeadline: DispatchTime(uptimeNanoseconds: finalDeadline),
            terminationGrace: 5,
            outputDrainTimeout: 5,
            processFactory: { process },
            primitives: primitives
        ) { box.set($0); finished.fulfill() }

        XCTAssertTrue(clock.await { $0 == [.wait(deadline: waitDeadline)] }, "\(clock.events)")
        awaitFile(marker)
        // The injected clock is the only clock: the child never dies, so every cleanup wait must end by
        // time passing. The loop moves the injected time (5 ms per step); the real 30 s only detects a hang.
        clock.advance(to: waitDeadline)
        let hangLimit = Date().addingTimeInterval(30)
        while box.outcome == nil, Date() < hangLimit {
            clock.advance(to: clock.now + Self.nanoseconds(0.005))
            Thread.sleep(forTimeInterval: 0.002)
        }
        wait(for: [finished], timeout: 5)

        guard case let .failure(error)? = box.outcome,
              case let HubActionError.commandNotQuiescent(underlying, quiescence) = error else {
            return XCTFail("a child that could not be reaped was reported as finished: \(String(describing: box.outcome))")
        }
        guard case HubActionError.commandTimedOut = underlying else {
            return XCTFail("the first failure is kept: \(underlying)")
        }
        XCTAssertFalse(quiescence.isQuiescent)
        XCTAssertTrue(process.isRunning, "the child is still alive and could still write")
        let gone = expectation(description: "quiescent")
        quiescence.whenQuiescent { gone.fulfill() }
        XCTAssertTrue(process.isRunning)
        Darwin.kill(process.processIdentifier, SIGKILL)
        wait(for: [gone], timeout: 30)
        XCTAssertTrue(quiescence.isQuiescent)
        XCTAssertFalse(process.isRunning, "quiescent only after the child was reaped")
    }

    func testQuiescenceIsReachedOnlyWhenTheChildIsReapedAndWaitersRunOnce() {
        let idle = HubChildQuiescence()
        XCTAssertTrue(idle.isQuiescent, "no child was ever started")
        var calls = 0
        idle.whenQuiescent { calls += 1 }
        XCTAssertEqual(calls, 1, "an already quiescent state calls back at once")

        let quiescence = HubChildQuiescence()
        quiescence.childMayRun()
        var order: [String] = []
        quiescence.whenQuiescent { order.append("first") }
        quiescence.whenQuiescent { order.append("second") }
        XCTAssertFalse(quiescence.isQuiescent, "a launched child may still write")
        XCTAssertEqual(order, [])
        quiescence.childReaped()
        XCTAssertTrue(quiescence.isQuiescent)
        XCTAssertEqual(order, ["first", "second"], "each waiter runs once, in order")
        quiescence.childReaped()
        XCTAssertEqual(order, ["first", "second"], "a repeated event runs nobody again")
        quiescence.whenQuiescent { order.append("late") }
        XCTAssertEqual(order, ["first", "second", "late"])
    }

    func testExecutorSpawnFailureIsAPlainFailureBecauseNoChildExists() {
        // The process never started: nothing can write, so the failure is not held as "not quiescent".
        let finished = expectation(description: "reported")
        let box = OutcomeBox()
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: "/nonexistent/e1fix7-no-such-binary"),
            arguments: [],
            timeout: 5
        ) { box.set($0); finished.fulfill() }
        wait(for: [finished], timeout: 10)
        guard case let .failure(error)? = box.outcome else { return XCTFail("a missing binary cannot succeed") }
        if case HubActionError.commandNotQuiescent = error { XCTFail("a child that never started was held: \(error)") }
    }

    func testExecutorStillReportsAnOrdinaryTimeoutOnceTheChildIsReaped() throws {
        // The signals work: the child is gone when the failure is reported, so it is the plain timeout
        // (not "not quiescent") and the quiescence is already true.
        let clock = ManualProcessClock()
        let process = Process()
        defer { Self.killSurvivor(process) }
        let marker = readinessMarker()
        defer { try? FileManager.default.removeItem(at: marker) }
        let child = termIgnoringChild(marker: marker)
        let start = clock.now
        let waitDeadline = start + Self.nanoseconds(1.5)
        let finished = expectation(description: "reported")
        let box = OutcomeBox()
        HubProcessExecutor.run(
            executable: URL(fileURLWithPath: child.executable),
            arguments: child.arguments,
            timeout: 60,
            waitDeadline: DispatchTime(uptimeNanoseconds: waitDeadline),
            terminationGrace: 0.05,
            outputDrainTimeout: 0.05,
            processFactory: { process },
            primitives: clock.primitives
        ) { box.set($0); finished.fulfill() }
        XCTAssertTrue(clock.await { $0 == [.wait(deadline: waitDeadline)] }, "\(clock.events)")
        awaitFile(marker)
        clock.advance(to: waitDeadline)
        // The grace wait starts from the injected time at which the executor reached it: wait for it.
        XCTAssertTrue(clock.await { $0.count >= 3 && $0.contains(.terminate) }, "\(clock.events)")
        clock.advance(to: waitDeadline + Self.nanoseconds(0.06))
        XCTAssertTrue(clock.await { $0.contains(.kill) }, "\(clock.events)")
        wait(for: [finished], timeout: 30)
        assertTimedOut(box.outcome)
        assertReaped(process, signal: SIGKILL)
    }

    private static func killSurvivor(_ process: Process) {
        if process.isRunning { Darwin.kill(process.processIdentifier, SIGKILL) }
    }

    // MARK: - Plan table

    func testCommandPlanTable() {
        let domain = "gui/501", service = "gui/501/com.teslatlas.hub", plist = "/p/com.teslatlas.hub.plist"
        func plan(_ action: HubServiceAction, loaded: Bool, changed: Bool = false) -> [[String]] {
            LaunchctlServiceController.commandPlan(action: action, loaded: loaded, plistChanged: changed,
                                                   domain: domain, service: service, plist: plist)
        }
        let enable = ["enable", service], bootout = ["bootout", service]
        let bootstrap = ["bootstrap", domain, plist], disable = ["disable", service]
        XCTAssertEqual(plan(.start, loaded: false), [enable, bootstrap])
        XCTAssertEqual(plan(.start, loaded: false, changed: true), [enable, bootstrap])
        XCTAssertEqual(plan(.start, loaded: true), [enable, ["kickstart", service]])
        XCTAssertEqual(plan(.start, loaded: true, changed: true), [enable, bootout, bootstrap])
        XCTAssertEqual(plan(.restart, loaded: true), [enable, bootout, bootstrap])
        XCTAssertEqual(plan(.restart, loaded: true, changed: true), [enable, bootout, bootstrap])
        XCTAssertEqual(plan(.restart, loaded: false), [enable, bootstrap])
        XCTAssertEqual(plan(.stop, loaded: true), [disable, bootout])
        XCTAssertEqual(plan(.stop, loaded: false), [disable])
        XCTAssertEqual(plan(.pause, loaded: true), [bootout])
        XCTAssertEqual(plan(.pause, loaded: false), [])
        XCTAssertEqual(plan(.remove, loaded: true), [bootout, enable])
        XCTAssertEqual(plan(.remove, loaded: false), [enable])
        XCTAssertEqual(plan(.delete, loaded: true), [disable, bootout, enable])
        XCTAssertEqual(plan(.delete, loaded: false), [disable, enable])

        // The same rows, executed.
        var rig = LifecycleRig()
        rig.launchctl.loaded = false
        _ = rig.run("start")
        XCTAssertEqual(rig.launchctlEvents, ["print", "enable", "bootstrap", "print"])
        rig = LifecycleRig()
        rig.agent.change = .unchanged
        _ = rig.run("start")
        XCTAssertEqual(rig.launchctlEvents, ["print", "enable", "bootstrap", "print"])
        rig = LifecycleRig(loaded: true)
        rig.agent.change = .unchanged
        _ = rig.run("start")
        XCTAssertEqual(rig.launchctlEvents, ["print", "enable", "kickstart", "print"])
        rig = LifecycleRig(loaded: true)
        rig.agent.change = .replaced
        _ = rig.run("start")
        XCTAssertEqual(rig.launchctlEvents, ["print", "enable", "bootout", "print", "bootstrap", "print"])
        rig = LifecycleRig(loaded: true)
        _ = rig.run("restart")
        XCTAssertEqual(rig.launchctlEvents, ["print", "enable", "bootout", "print", "bootstrap", "print"])
        rig = LifecycleRig()
        _ = rig.run("restart")
        XCTAssertEqual(rig.launchctlEvents, ["print", "enable", "bootstrap", "print"])
        rig = LifecycleRig(loaded: true)
        _ = rig.run("stop")
        XCTAssertEqual(rig.launchctlEvents, ["print", "disable", "bootout", "print"])
        rig = LifecycleRig()
        _ = rig.run("stop")
        XCTAssertEqual(rig.launchctlEvents, ["print", "disable"])
        rig = LifecycleRig(loaded: true)
        _ = rig.run("pause")
        XCTAssertEqual(rig.launchctlEvents, ["print", "bootout", "print"])
        rig = LifecycleRig()
        _ = rig.run("pause")
        XCTAssertEqual(rig.launchctlEvents, ["print"])
    }

    func testStartAndRestartEnsureThenPreflightBeforeFirstLaunchctl() {
        for action in ["start", "restart"] {
            let rig = LifecycleRig()
            _ = rig.run(action)
            XCTAssertEqual(rig.events,
                           ["ensure(config)", "preflight", "launchctl print", "launchctl enable",
                            "prepareLogs", "launchctl bootstrap", "launchctl print"], action)
        }
    }

    func testPreflightFailureWithoutLoadedRecordIssuesNoLaunchctlAndReportsMessage() {
        // No record, then a recorded-unloaded record.
        for pausedFirst in [false, true] {
            let rig = LifecycleRig()
            if pausedFirst {
                _ = rig.run("pause")
                rig.events.removeAll()
            }
            rig.preflightResult = .failure(HubActionError.commandFailed("collector interval is zero"))
            let result = rig.run("start")
            guard case let .failure(error)? = result else { return XCTFail("expected the preflight failure") }
            XCTAssertTrue(error.localizedDescription.contains("collector interval is zero"))
            XCTAssertEqual(rig.events, ["ensure(config)", "preflight"], "no launchctl command may run")
            XCTAssertEqual(rig.completionCount, pausedFirst ? 2 : 1)
        }
    }

    func testEnsureFailureWithoutLoadedRecordIssuesNoLaunchctl() {
        for pausedFirst in [false, true] {
            let rig = LifecycleRig()
            if pausedFirst {
                _ = rig.run("pause")
                rig.events.removeAll()
            }
            rig.agent.ensureError = HubActionError.commandFailed("Move Teslatlas Hub to the Applications folder")
            let result = rig.run("restart")
            guard case let .failure(error)? = result else { return XCTFail("expected the writer failure") }
            XCTAssertTrue(error.localizedDescription.contains("Applications folder"))
            XCTAssertEqual(rig.events, ["ensure(config)"])
        }
    }

    func testHeldLoadedRecordStartRecoversAfterEnsurePreflightOrQueryFailure() {
        enum Failure: CaseIterable { case ensure, preflight, queryError, queryTimeout }
        for failure in Failure.allCases {
            for recoveryFindsLoaded in [false, true] {
                let label = "\(failure) recoveryLoaded=\(recoveryFindsLoaded)"
                let rig = LifecycleRig(loaded: true)
                _ = rig.run("pause")
                XCTAssertTrue(rig.controller.restartDutyOutstanding, label)
                rig.events.removeAll()
                rig.launchctl.removeAllCalls()
                var queryError: Error?
                var expectedText = ""
                switch failure {
                case .ensure:
                    rig.agent.ensureError = HubActionError.commandFailed("writer failed")
                    expectedText = "writer failed"
                case .preflight:
                    rig.preflightResult = .failure(HubActionError.commandFailed("preflight failed"))
                    expectedText = "preflight failed"
                case .queryError:
                    queryError = HubActionError.commandFailed("print failed")
                    expectedText = "print failed"
                case .queryTimeout:
                    queryError = HubActionError.commandTimedOut
                    expectedText = "timed out"
                }
                // The ordinary Start's own query is print#0 when it gets that far; the
                // recovery's own query follows it.
                let recoveryQuery = queryError == nil ? 0 : 1
                rig.launchctl.override = { arguments, index in
                    guard arguments.first == "print" else { return nil }
                    if let queryError, index == 0 { return .result(.failure(queryError)) }
                    if index == recoveryQuery {
                        return .result(recoveryFindsLoaded ? .success("") : .failure(LifecycleRig.unloadedFailure))
                    }
                    return nil
                }
                let result = rig.run("start")
                guard case let .failure(error)? = result else { XCTFail(label); continue }
                XCTAssertTrue(error.localizedDescription.contains(expectedText), "\(label): \(error.localizedDescription)")
                XCTAssertNotNil(error as? HubServiceRestartReport, label)
                let commands = rig.launchctlEvents
                XCTAssertFalse(commands.contains("bootout"), "\(label): no second bootout")
                if recoveryFindsLoaded {
                    XCTAssertEqual(commands.filter { $0 == "kickstart -k" }.count, 1, label)
                    XCTAssertFalse(commands.contains("bootstrap"), label)
                } else {
                    XCTAssertEqual(commands.filter { $0 == "bootstrap" }.count, 1, label)
                    XCTAssertFalse(commands.contains("kickstart -k"), label)
                }
                XCTAssertEqual(rig.events.filter { $0.hasPrefix("ensure") }.count, 1, "\(label): recovery never ensures")
                XCTAssertEqual(rig.events.filter { $0 == "preflight" }.count, failure == .ensure ? 0 : 1,
                               "\(label): recovery never preflights")
                XCTAssertFalse(rig.controller.restartDutyOutstanding, "\(label): the record is cleared after settlement")
                XCTAssertEqual(rig.completionCount, 2, "\(label): one completion per call")
                XCTAssertEqual(rig.events.last, "launchctl print", "\(label): completion follows the final confirmation")
            }
        }
    }

    func testStopAndPauseNeverCallAgentOrPreflight() {
        for action in ["stop", "pause"] {
            for loaded in [false, true] {
                let rig = LifecycleRig(loaded: loaded)
                _ = rig.run(action)
                XCTAssertFalse(rig.events.contains { $0.hasPrefix("ensure") || $0 == "preflight" || $0 == "prepareLogs" },
                               "\(action) \(rig.events)")
            }
        }
    }

    func testPrepareLogsRunsImmediatelyBeforeEachBootstrapOnly() {
        // Ordinary and recovery bootstraps: always the event right before.
        let ordinary = LifecycleRig()
        _ = ordinary.run("start")
        let index = ordinary.events.firstIndex(of: "launchctl bootstrap")!
        XCTAssertEqual(ordinary.events[index - 1], "prepareLogs")

        let recovery = LifecycleRig(loaded: true)
        recovery.launchctl.failNext("bootstrap", with: HubActionError.commandFailed("bootstrap failed"))
        _ = recovery.run("restart")
        for (position, event) in recovery.events.enumerated() where event == "launchctl bootstrap" {
            XCTAssertEqual(recovery.events[position - 1], "prepareLogs")
        }
        XCTAssertEqual(recovery.events.filter { $0 == "prepareLogs" }.count, 2)

        // Never on stop, pause or remove.
        for action in ["stop", "pause", "remove"] {
            let rig = LifecycleRig(loaded: true)
            _ = rig.run(action)
            XCTAssertFalse(rig.events.contains("prepareLogs"), action)
        }
    }

    // MARK: - Unload poll

    func testUnloadPollCompletesAt65And200And245SecondsOfFakeTime() {
        for seconds in [65.0, 200.0, 245.0] {
            for action in ["stop", "restart"] {
                let rig = LifecycleRig(loaded: true)
                rig.launchctl.unloadDelay = seconds
                var result: Result<String, Error>?
                rig.start(action) { result = $0 }
                rig.scheduler.advance(by: seconds - 1)
                XCTAssertNil(result, "\(action) must still be waiting at \(seconds - 1) s")
                rig.scheduler.advance(by: 2)
                guard case .success? = result else { return XCTFail("\(action) at \(seconds) s: \(String(describing: result))") }
            }
        }
    }

    func testUnloadPollFailsAt300SecondsWithStopMessage() {
        let rig = LifecycleRig(loaded: true)
        rig.launchctl.unloadDelay = .infinity
        var result: Result<String, Error>?
        rig.start("stop") { result = $0 }
        rig.scheduler.advance(by: 299)
        XCTAssertNil(result)
        rig.scheduler.advance(by: 2)
        guard case let .failure(error)? = result else { return XCTFail("the deadline must fail the stop") }
        XCTAssertTrue(error.localizedDescription.contains("Hub service did not finish stopping."))
        XCTAssertTrue(error.localizedDescription.contains("will not start at login"),
                      "an intentional stop says the Hub is already disabled")
        XCTAssertTrue(rig.launchctlEvents.contains("disable"))

        // A pause has no login sentence.
        let pause = LifecycleRig(loaded: true)
        pause.launchctl.unloadDelay = .infinity
        var pauseResult: Result<String, Error>?
        pause.start("pause") { pauseResult = $0 }
        pause.scheduler.advance(by: 301)
        guard case let .failure(pauseError)? = pauseResult else { return XCTFail("pause deadline") }
        XCTAssertFalse(pauseError.localizedDescription.contains("login"))
    }

    func testUnloadPollNeverOutlivesTheStopShare() {
        // A runner that overruns its limits (the initial print takes 100 s) leaves the
        // 390 s stop share shorter than a full 300 s poll: the share binds.
        let rig = LifecycleRig(loaded: true)
        let start = rig.clock.now
        rig.launchctl.unloadDelay = .infinity
        rig.launchctl.elapse = { arguments in arguments.first == "print" && rig.launchctl.calls.count == 1 ? 100 : 0 }
        var result: Result<String, Error>?
        var failedAt: TimeInterval = 0
        rig.start("pause") { result = $0; failedAt = rig.clock.now }
        rig.scheduler.advance(by: 450)
        guard case let .failure(error)? = result else { return XCTFail("the poll must end at the share") }
        XCTAssertTrue(error.localizedDescription.contains("Hub service did not finish stopping."),
                      "the poll itself reports the deadline, not a command time-out: \(error.localizedDescription)")
        XCTAssertGreaterThanOrEqual(failedAt - start, 389)
        XCTAssertLessThan(failedAt - start, 392, "the poll ended at the 390 s share, not at 100 + 300 s")
    }

    func testPollSpacingIsTenthSecondThenOneSecond() {
        let rig = LifecycleRig(loaded: true)
        rig.launchctl.unloadDelay = 40
        rig.start("pause") { _ in }
        rig.scheduler.advance(by: 60)
        let delays = rig.scheduler.delays
        XCTAssertGreaterThan(delays.count, 100)
        XCTAssertTrue(delays.prefix(100).allSatisfy { abs($0 - 0.1) < 0.001 }, "0.1 s for the first 10 s")
        XCTAssertTrue(delays.dropFirst(101).allSatisfy { abs($0 - 1.0) < 0.001 }, "then 1 s")
    }

    func testRemoveBootsOutPollsEnablesThenUnlinks() {
        let rig = LifecycleRig(loaded: true)
        let result = rig.run("remove")
        guard case .success? = result else { return XCTFail("remove failed") }
        XCTAssertEqual(rig.events, ["launchctl print", "launchctl bootout", "launchctl print",
                                    "launchctl enable", "removeInstalled"])
        let unloaded = LifecycleRig()
        _ = unloaded.run("remove")
        XCTAssertEqual(unloaded.events, ["launchctl print", "launchctl enable", "removeInstalled"])
    }

    // MARK: - Restart duty (RD1)

    private enum DutyCase: CaseIterable {
        case unloadCheckErrors, stopShareExpires, bootoutFailsStaysLoaded
        case logPreparationThrows, bootstrapFails, enableFails
    }

    private func dutyRig(_ duty: DutyCase, action: String, recoveryFindsLoaded: Bool) -> LifecycleRig {
        let rig = LifecycleRig(loaded: true)
        rig.agent.change = action == "start" ? .replaced : .unchanged
        let t0 = rig.clock.now
        /// The recovery's own query is the print with this ordinal (nil: after the poll, by time).
        func answerRecovery(printOrdinal: Int?) {
            let previous = rig.launchctl.override
            var timedAnswered = false
            rig.launchctl.override = { [unowned rig] arguments, index in
                if arguments.first == "print" {
                    var isRecoveryQuery = printOrdinal.map { index == $0 } ?? false
                    if printOrdinal == nil, !timedAnswered, rig.clock.now >= t0 + 300 {
                        // The first query after the poll deadline is the recovery's own.
                        timedAnswered = true
                        isRecoveryQuery = true
                    }
                    if isRecoveryQuery {
                        return .result(recoveryFindsLoaded ? .success("") : .failure(LifecycleRig.unloadedFailure))
                    }
                }
                return previous?(arguments, index)
            }
        }
        switch duty {
        case .unloadCheckErrors:
            // print#0 query, print#1 first unload check (error), print#2 recovery's own.
            rig.launchctl.override = { arguments, index in
                arguments.first == "print" && index == 1
                    ? .result(.failure(HubActionError.commandFailed("unload check failed"))) : nil
            }
            answerRecovery(printOrdinal: 2)
        case .stopShareExpires:
            rig.launchctl.unloadDelay = .infinity
            answerRecovery(printOrdinal: nil)
        case .bootoutFailsStaysLoaded:
            rig.launchctl.override = { arguments, _ in
                arguments.first == "bootout" ? .result(.failure(HubActionError.commandFailed("bootout failed"))) : nil
            }
            answerRecovery(printOrdinal: 1)
        case .logPreparationThrows:
            rig.agent.prepareLogsErrors = [HubActionError.commandFailed("log preparation failed")]
            answerRecovery(printOrdinal: 2)
        case .bootstrapFails:
            rig.launchctl.failNext("bootstrap", with: HubActionError.commandFailed("bootstrap failed"))
            answerRecovery(printOrdinal: 2)
        case .enableFails:
            rig.launchctl.failNext("enable", with: HubActionError.commandFailed("enable failed"))
            answerRecovery(printOrdinal: 1)
        }
        return rig
    }

    func testRestartDutyWhenUnloadCheckErrors() { assertDuty(.unloadCheckErrors) }
    func testRestartDutyWhenStopShareExpires() { assertDuty(.stopShareExpires) }
    func testRestartDutyWhenBootoutFailsAndJobStaysLoaded() { assertDuty(.bootoutFailsStaysLoaded) }
    func testRestartDutyWhenLogPreparationThrows() { assertDuty(.logPreparationThrows) }
    func testRestartDutyWhenBootstrapFails() { assertDuty(.bootstrapFails) }
    func testRestartDutyWhenEnableFails() { assertDuty(.enableFails) }

    private func assertDuty(_ duty: DutyCase, file: StaticString = #filePath, line: UInt = #line) {
        let originalText: [DutyCase: String] = [
            .unloadCheckErrors: "unload check failed", .stopShareExpires: "Hub service did not finish stopping",
            .bootoutFailsStaysLoaded: "bootout failed", .logPreparationThrows: "log preparation failed",
            .bootstrapFails: "bootstrap failed", .enableFails: "enable failed"
        ]
        for action in ["start", "restart"] {
            for recoveryFindsLoaded in [false, true] {
                let label = "\(duty) \(action) recoveryLoaded=\(recoveryFindsLoaded)"
                let rig = dutyRig(duty, action: action, recoveryFindsLoaded: recoveryFindsLoaded)
                var result: Result<String, Error>?
                rig.start(action) { result = $0 }
                rig.scheduler.advance(by: 400)
                guard case let .failure(error)? = result else {
                    XCTFail("\(label): expected the original failure", file: file, line: line)
                    continue
                }
                XCTAssertEqual(rig.completionCount, 1, label, file: file, line: line)
                guard let report = error as? HubServiceRestartReport else {
                    XCTFail("\(label): the failure carries the recovery report", file: file, line: line)
                    continue
                }
                XCTAssertTrue(report.localizedDescription.hasPrefix(originalText[duty]!),
                              "\(label): the action's own error comes first: \(report.localizedDescription)", file: file, line: line)
                XCTAssertEqual(report.outcome, .restarted(appliesNewDefinition: recoveryFindsLoaded), label, file: file, line: line)

                let commands = rig.launchctlEvents
                func count(_ name: String) -> Int { commands.filter { $0 == name }.count }
                XCTAssertEqual(count("bootout"), duty == .enableFails ? 0 : 1,
                               "\(label): the recovery never stops again \(commands)", file: file, line: line)
                if recoveryFindsLoaded {
                    XCTAssertEqual(count("kickstart -k"), 1, "\(label) \(commands)", file: file, line: line)
                    XCTAssertEqual(count("bootstrap"), duty == .bootstrapFails ? 1 : 0, label, file: file, line: line)
                    XCTAssertEqual(count("enable"), 1, label, file: file, line: line)
                    XCTAssertEqual(Array(commands.suffix(2)), ["kickstart -k", "print"], label, file: file, line: line)
                } else {
                    XCTAssertEqual(count("kickstart -k"), 0, label, file: file, line: line)
                    XCTAssertEqual(count("bootstrap"), duty == .bootstrapFails ? 2 : 1,
                                   "\(label): exactly one recovery bootstrap \(commands)", file: file, line: line)
                    XCTAssertEqual(count("enable"), 2, label, file: file, line: line)
                    XCTAssertEqual(Array(commands.suffix(2)), ["bootstrap", "print"], label, file: file, line: line)
                }
                let ordinaryPrepare = (duty == .bootstrapFails || duty == .logPreparationThrows) ? 1 : 0
                XCTAssertEqual(rig.events.filter { $0 == "prepareLogs" }.count,
                               ordinaryPrepare + (recoveryFindsLoaded ? 0 : 1),
                               "\(label): the recovery retries preparation only on the unloaded branch", file: file, line: line)
                XCTAssertEqual(rig.events.filter { $0 == "preflight" }.count, 1, label, file: file, line: line)
                XCTAssertEqual(rig.events.filter { $0.hasPrefix("ensure") }.count, 1, label, file: file, line: line)
                XCTAssertFalse(rig.controller.restartDutyOutstanding, label, file: file, line: line)
            }
        }
    }

    func testRepeatedLogPreparationFailureIsReportedAsUnconfirmed() {
        let rig = LifecycleRig(loaded: true)
        rig.agent.prepareLogsErrors = [HubActionError.commandFailed("first"), HubActionError.commandFailed("second")]
        rig.launchctl.override = { arguments, index in
            arguments.first == "print" && index == 2 ? .result(.failure(LifecycleRig.unloadedFailure)) : nil
        }
        var result: Result<String, Error>?
        rig.start("restart") { result = $0 }
        rig.scheduler.advance(by: 5)
        guard case let .failure(error)? = result, let report = error as? HubServiceRestartReport else {
            return XCTFail("expected a restart report")
        }
        XCTAssertTrue(report.isUnconfirmed)
        XCTAssertTrue(report.localizedDescription.hasPrefix("first"))
        XCTAssertTrue(report.localizedDescription.contains("could not be confirmed"))
        XCTAssertEqual(rig.events.filter { $0 == "prepareLogs" }.count, 2)
    }

    func testFailedLoadedQueryStopsNothingAndStartsNothing() {
        let rig = LifecycleRig(loaded: true)
        rig.launchctl.override = { arguments, index in
            arguments.first == "print" && index == 0 ? .result(.failure(HubActionError.commandFailed("print failed"))) : nil
        }
        let result = rig.run("restart")
        guard case let .failure(error)? = result else { return XCTFail("expected the query error") }
        XCTAssertEqual(error.localizedDescription, "print failed")
        XCTAssertEqual(rig.launchctlEvents, ["print"])
        XCTAssertFalse(rig.controller.restartDutyOutstanding)
    }

    func testUnloadedStartFailureHasNoRecovery() {
        let rig = LifecycleRig()
        rig.launchctl.failNext("bootstrap", with: HubActionError.commandFailed("bootstrap failed"))
        let result = rig.run("start")
        guard case let .failure(error)? = result else { return XCTFail("expected the bootstrap failure") }
        XCTAssertEqual(error.localizedDescription, "bootstrap failed")
        XCTAssertNil(error as? HubServiceRestartReport)
        XCTAssertEqual(rig.launchctlEvents, ["print", "enable", "bootstrap"], "no automatic recovery or retry")
    }

    func testCommandDeadlinesAndShares() {
        // Print, enable and bootout take their full 30 s limits and the job stays loaded:
        // the poll still gets its full 300 s, and the recovery then has its own 120 s.
        let rig = LifecycleRig(loaded: true)
        let start = rig.clock.now
        rig.launchctl.unloadDelay = .infinity
        rig.launchctl.elapse = { [unowned rig] _ in
            let calls = rig.launchctl.calls.count
            if calls <= 3 { return 30 }
            return rig.clock.now >= start + 389 ? 30 : 0
        }
        var result: Result<String, Error>?
        rig.start("restart") { result = $0 }
        rig.scheduler.advance(by: 700)
        guard case let .failure(error)? = result, error is HubServiceRestartReport else {
            return XCTFail("expected a failed restart with a recovery report: \(String(describing: result))")
        }
        let calls = rig.launchctl.calls
        XCTAssertEqual(calls.prefix(3).map { $0.arguments.first }, ["print", "enable", "bootout"])
        XCTAssertTrue(calls.allSatisfy { $0.timeout <= HubLaunchAgentContract.launchctlRunLimit })
        XCTAssertTrue(calls.allSatisfy { $0.timeout > 0 })
        XCTAssertTrue(calls.prefix(3).allSatisfy { $0.timeout == 30 })
        let recovery = Array(calls.suffix(3))
        XCTAssertEqual(recovery.map { $0.arguments.first }, ["print", "kickstart", "print"])
        XCTAssertTrue(recovery.allSatisfy { $0.timeout == 30 }, "the recovery's own share is intact")
        XCTAssertGreaterThanOrEqual(rig.completedAt - start, 390 + 90,
                                    "the poll kept its full 300 s, then the recovery ran 3 x 30 s")
        XCTAssertLessThanOrEqual(rig.completedAt - start, HubLaunchAgentContract.replaceServiceBudget)
    }

    func testMinimumRemainingShareShrinksTheCommandTimeout() {
        let rig = LifecycleRig(loaded: true)
        rig.launchctl.unloadDelay = .infinity
        rig.launchctl.elapse = { arguments in arguments.first == "print" ? 25 : 0 }
        rig.start("pause") { _ in }
        rig.scheduler.advance(by: 700)
        XCTAssertTrue(rig.launchctl.calls.contains { $0.timeout < 30 && $0.timeout > 0 },
                      "min(30, remaining share) must shrink near the end of the share")
        XCTAssertTrue(rig.launchctl.calls.allSatisfy { $0.timeout <= 30 })
    }

    func testSynchronousLogWorkIsExcludedFromCommandShares() {
        let rig = LifecycleRig(loaded: false)
        rig.agent.onPrepareLogs = { [unowned rig] in rig.clock.now += 1_000 }
        let result = rig.run("start")
        guard case .success? = result else { return XCTFail("log work must not consume the start share: \(String(describing: result))") }
        XCTAssertEqual(rig.launchctl.calls.last?.arguments.first, "print")
        XCTAssertGreaterThan(rig.launchctl.calls.last?.timeout ?? 0, 0)
    }

    func testRecoveryExcludesSlowLogPreparationFromItsShare() {
        // A pause held the loaded record and unloaded the job: the recovery bootstraps.
        let rig = LifecycleRig(loaded: true)
        _ = rig.run("pause")
        XCTAssertTrue(rig.controller.restartDutyOutstanding)
        rig.launchctl.removeAllCalls()
        rig.events = []
        // The fake log preparation takes far longer than the whole recovery share (120 s).
        rig.agent.onPrepareLogs = { [unowned rig] in rig.clock.now += 1_000 }
        let result = rig.recover()
        guard case let .success(note)? = result else {
            return XCTFail("slow log preparation must not consume the recovery share: \(String(describing: result))")
        }
        XCTAssertFalse(note.isEmpty, "the Hub was started again")
        XCTAssertEqual(rig.launchctlEvents, ["print", "enable", "bootstrap", "print"])
        XCTAssertTrue(rig.launchctl.calls.allSatisfy { $0.timeout > 0 })
        XCTAssertFalse(rig.controller.restartDutyOutstanding)
    }

    func testRecoveryNeverEnsuresOrPreflights() {
        let rig = LifecycleRig(loaded: true)
        rig.launchctl.failNext("bootout", with: HubActionError.commandFailed("bootout failed"))
        _ = rig.run("restart")
        XCTAssertEqual(rig.events.filter { $0.hasPrefix("ensure") }.count, 1)
        XCTAssertEqual(rig.events.filter { $0 == "preflight" }.count, 1)
    }

    func testCompletionRunsExactlyOnceAfterRecovery() {
        let rig = LifecycleRig(loaded: true)
        rig.launchctl.failNext("bootout", with: HubActionError.commandFailed("bootout failed"))
        var order: [String] = []
        rig.controller.run(arguments: ["service", "restart"], owner: rig.owner) { _ in
            order.append("completion@\(rig.launchctlEvents.count)")
        }
        XCTAssertEqual(order.count, 1)
        // The completion saw the whole recovery: print, kickstart -k, print after the failure.
        XCTAssertEqual(rig.launchctlEvents.suffix(3), ["print", "kickstart -k", "print"])
        XCTAssertEqual(order, ["completion@\(rig.launchctlEvents.count)"])
    }

    func testPauseRecordLifecycle() {
        // A pause holds its loaded record on success and on failure.
        let success = LifecycleRig(loaded: true)
        _ = success.run("pause")
        XCTAssertTrue(success.controller.restartDutyOutstanding)
        let failure = LifecycleRig(loaded: true)
        failure.launchctl.failNext("bootout", with: HubActionError.commandFailed("bootout failed"))
        _ = failure.run("pause")
        XCTAssertTrue(failure.controller.restartDutyOutstanding)

        // A failed initial query records nothing.
        let none = LifecycleRig(loaded: true)
        none.launchctl.override = { arguments, index in
            arguments.first == "print" && index == 0 ? .result(.failure(HubActionError.commandFailed("print failed"))) : nil
        }
        _ = none.run("pause")
        XCTAssertFalse(none.controller.restartDutyOutstanding)

        // A recorded-unloaded record owes nothing.
        let unloaded = LifecycleRig(loaded: false)
        _ = unloaded.run("pause")
        XCTAssertFalse(unloaded.controller.restartDutyOutstanding)

        // A second pause keeps the first record, whichever it was.
        let second = LifecycleRig(loaded: true)
        _ = second.run("pause")
        _ = second.run("pause")
        XCTAssertTrue(second.controller.restartDutyOutstanding, "the job is unloaded now; the first record stands")
        let secondUnloaded = LifecycleRig(loaded: false)
        _ = secondUnloaded.run("pause")
        secondUnloaded.launchctl.loaded = true
        _ = secondUnloaded.run("pause")
        XCTAssertFalse(secondUnloaded.controller.restartDutyOutstanding, "the first record (unloaded) stands")

        // Start, Restart and the typed recovery clear it once settled.
        for clearing in ["start", "restart", "recover"] {
            let rig = LifecycleRig(loaded: true)
            _ = rig.run("pause")
            XCTAssertTrue(rig.controller.restartDutyOutstanding)
            if clearing == "recover" {
                rig.recover()
            } else {
                _ = rig.run(clearing)
            }
            XCTAssertFalse(rig.controller.restartDutyOutstanding, clearing)
        }

        // Stop, Remove and Delete clear it before their first command.
        for clearing in ["stop", "remove", "delete"] {
            let rig = LifecycleRig(loaded: true)
            _ = rig.run("pause")
            var heldAtFirstCommand: Bool?
            rig.launchctl.removeAllCalls()
            rig.launchctl.beforeFirstCall = { [unowned rig] in heldAtFirstCommand = rig.controller.restartDutyOutstanding }
            if clearing == "delete" {
                rig.deleteHubAndData()
            } else {
                _ = rig.run(clearing)
            }
            XCTAssertEqual(heldAtFirstCommand, false, clearing)
        }
    }

    func testRecoverAfterPauseWithoutRecordIssuesNothing() {
        for pausedFirst in [false, true] {
            let rig = LifecycleRig(loaded: false)
            if pausedFirst { _ = rig.run("pause") }
            rig.events.removeAll()
            let result = rig.recover()
            guard case let .success(note)? = result else { return XCTFail("nothing owed is not a failure") }
            XCTAssertEqual(note, "")
            XCTAssertEqual(rig.events, [], "no command may be issued")
        }
    }

    func testReleaseRestartRecordClearsWithoutACommand() {
        let rig = LifecycleRig(loaded: true)
        _ = rig.run("pause")
        var settled = 0
        rig.controller.whenRestartDutySettled { settled += 1 }
        XCTAssertEqual(settled, 0)
        rig.events.removeAll()
        rig.controller.releaseRestartRecord(owner: rig.owner)
        XCTAssertEqual(settled, 1)
        XCTAssertFalse(rig.controller.restartDutyOutstanding)
        XCTAssertEqual(rig.events, [])
        var immediately = 0
        rig.controller.whenRestartDutySettled { immediately += 1 }
        XCTAssertEqual(immediately, 1)
    }

    func testUnconfirmedRecoveryReportsByRefreshedStatus() throws {
        for running in [true, false] {
            let home = HubTestScratch.directory
                .appendingPathComponent("teslatlas-unconfirmed-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
            defer { try? FileManager.default.removeItem(at: home) }
            let status = #"{"status":"ok","version":"\#(HubRelease.bundledVersion)","database":{"path":"/tmp/hub/catalogue.sqlite3","bytes":1},"ready":true,"provider":"fleet","vehicles":[],"credentials":{"present":true}}"#
            let service = UnconfirmedRestartService(running: running)
            let controller = HubController(commandRunner: StatusOnlyRunner(output: status),
                                           serviceRunner: service,
                                           homeDirectory: home,
                                           serviceInstalledOverride: true)
            let done = expectation(description: "restart reported")
            controller.restartHub { result in
                guard case let .failure(error) = result else { XCTFail("expected a failure"); done.fulfill(); return }
                let text = error.localizedDescription
                XCTAssertTrue(text.hasPrefix("bootstrap failed"))
                XCTAssertTrue(text.contains("could not be confirmed"))
                XCTAssertEqual(text.contains("The Hub did not restart. Choose Start Hub."), !running,
                               "the stopped text appears only when status shows the Hub stopped")
                done.fulfill()
            }
            wait(for: [done], timeout: 3)
            XCTAssertEqual(service.stopCalls, 0, "a running job gets no additional stop")
        }
    }

    func testDeleteIsNotReachableThroughStringArguments() {
        let rig = LifecycleRig(loaded: true)
        let result = rig.run("delete")
        guard case let .failure(error)? = result else { return XCTFail("string delete must be refused") }
        XCTAssertTrue(error.localizedDescription.contains("Unknown service action"))
        XCTAssertEqual(rig.events, [])
        XCTAssertEqual(rig.remover.checks, 0)
        XCTAssertEqual(rig.remover.deletes, 0)
    }

    // MARK: - Delete Hub and Data

    func testDeleteSequenceLoaded() {
        let rig = LifecycleRig(loaded: true)
        let result = rig.deleteHubAndData()
        guard case .success? = result else { return XCTFail("delete failed: \(String(describing: result))") }
        XCTAssertEqual(rig.events, ["check", "validateRemovable", "launchctl disable", "launchctl print",
                                    "launchctl bootout", "launchctl print", "removeInstalled", "check",
                                    "delete", "launchctl enable"])
    }

    func testDeleteSequenceUnloaded() {
        let rig = LifecycleRig(loaded: false)
        let result = rig.deleteHubAndData()
        guard case .success? = result else { return XCTFail("delete failed") }
        XCTAssertEqual(rig.events, ["check", "validateRemovable", "launchctl disable", "launchctl print",
                                    "removeInstalled", "check", "delete", "launchctl enable"])
    }

    func testDeleteRefusalsChangeNothing() {
        let refused = HubDataRemovalRefusal.item("~/Library/Application Support/Teslatlas Hub/x", .link)
        for variant in 0..<2 {
            let rig = LifecycleRig(loaded: true)
            if variant == 0 { rig.remover.checkErrors = [refused] } else {
                rig.agent.validateError = HubActionError.commandFailed("The background service file was not written by Teslatlas Hub")
            }
            let result = rig.deleteHubAndData()
            guard case .failure? = result else { return XCTFail("a refusal must complete with it") }
            XCTAssertEqual(rig.launchctlEvents, [], "no launchctl command may run")
            XCTAssertFalse(rig.events.contains("removeInstalled"))
            XCTAssertEqual(rig.remover.deletes, 0)
        }
    }

    func testDeleteStopFailuresDeleteNothing() {
        // disable fails
        let disable = LifecycleRig(loaded: true)
        disable.launchctl.failNext("disable", with: HubActionError.commandFailed("disable refused"))
        guard case let .failure(disableError)? = disable.deleteHubAndData() else { return XCTFail("disable") }
        XCTAssertTrue(disableError.localizedDescription.contains("The background service could not be stopped (disable refused). Nothing was deleted."))
        // bootout fails
        let bootout = LifecycleRig(loaded: true)
        bootout.launchctl.failNext("bootout", with: HubActionError.commandFailed("bootout refused"))
        guard case let .failure(bootoutError)? = bootout.deleteHubAndData() else { return XCTFail("bootout") }
        XCTAssertTrue(bootoutError.localizedDescription.contains("Nothing was deleted."))
        // deadline
        let deadline = LifecycleRig(loaded: true)
        deadline.launchctl.unloadDelay = .infinity
        var result: Result<HubDataRemovalReport, Error>?
        deadline.startDelete { result = $0 }
        deadline.scheduler.advance(by: 301)
        guard case let .failure(deadlineError)? = result else { return XCTFail("deadline") }
        XCTAssertTrue(deadlineError.localizedDescription.contains("Hub service did not finish stopping."))
        XCTAssertTrue(deadlineError.localizedDescription.contains("Nothing was deleted. The Hub will not start at login. Try again in a few minutes."))
        for rig in [disable, bootout, deadline] {
            XCTAssertFalse(rig.events.contains("removeInstalled"))
            XCTAssertEqual(rig.remover.deletes, 0)
            XCTAssertEqual(rig.remover.checks, 1, "only the read-only first check ran")
        }
    }

    func testDeleteRemoveFailureDeletesNothing() {
        let rig = LifecycleRig(loaded: true)
        rig.agent.removeError = HubActionError.commandFailed("permission denied")
        guard case let .failure(error)? = rig.deleteHubAndData() else { return XCTFail("remove failure") }
        XCTAssertTrue(error.localizedDescription.contains("could not be removed (permission denied). Nothing was deleted."))
        XCTAssertEqual(rig.remover.deletes, 0)
        XCTAssertEqual(rig.remover.checks, 1)
        XCTAssertFalse(rig.events.contains("launchctl enable"))
    }

    func testDeleteSecondCheckRefusalCallsNeitherDeleteNorEnable() {
        let rig = LifecycleRig(loaded: true)
        rig.remover.checkErrors = [nil, HubDataRemovalRefusal.item("~/Library/Application Support/Teslatlas Hub/data/b", .backup)]
        guard case let .failure(error)? = rig.deleteHubAndData() else { return XCTFail("second check") }
        XCTAssertTrue(error.localizedDescription.contains("The background service was removed, but no data was deleted, because"))
        XCTAssertTrue(error.localizedDescription.contains("Start Hub sets the service up again."))
        XCTAssertEqual(rig.remover.deletes, 0)
        XCTAssertFalse(rig.launchctlEvents.contains("enable"), "the override stays disabled")
        XCTAssertTrue(rig.events.contains("removeInstalled"))
    }

    func testDeleteFinalEnableFailureIsAWarning() {
        let rig = LifecycleRig(loaded: true)
        rig.launchctl.failNext("enable", with: HubActionError.commandFailed("enable refused"))
        rig.remover.report = HubDataRemovalReport(deleted: ["D1"], kept: [], warnings: [])
        guard case let .success(report)? = rig.deleteHubAndData() else { return XCTFail("a final enable failure is not a failure") }
        XCTAssertEqual(report.deleted, ["D1"])
        XCTAssertTrue(report.warnings.contains(HubDataRemovalReport.finalEnableFailedWarning))
        XCTAssertEqual(rig.remover.deletes, 1)
    }

    func testDeleteNeverEnsuresPreparesPreflightsOrStarts() {
        for loaded in [false, true] {
            let rig = LifecycleRig(loaded: loaded)
            _ = rig.deleteHubAndData()
            XCTAssertFalse(rig.events.contains { $0.hasPrefix("ensure") || $0 == "preflight" || $0 == "prepareLogs" })
            XCTAssertFalse(rig.launchctlEvents.contains("bootstrap"))
            XCTAssertFalse(rig.launchctlEvents.contains("kickstart"))
            XCTAssertFalse(rig.launchctlEvents.contains("kickstart -k"))
        }
    }

    // MARK: - Serialized lifecycle operations (R-E1F2-1)

    private func reconcilingController(_ rig: LifecycleRig) -> HubController {
        HubController(environment: [:],
                      commandRunner: StatusOnlyRunner(output: "{}"),
                      serviceRunner: rig.controller,
                      launchAgent: rig.agent,
                      dataRemover: nil,
                      homeDirectory: HubTestScratch.directory,
                      serviceInstalledOverride: true)
    }

    private func waitUntilHeld(_ held: HeldLaunchctlCall) {
        let deadline = Date().addingTimeInterval(3)
        while held.pending == nil, Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
    }

    func testReconcileRestartsOnlyAChangedLoadedJobOnTheRealController() {
        // The real controller's reconcile (HubControllerTests covers the controller above a fake
        // service): a changed plist restarts a loaded job and never starts an unloaded one.
        let cases: [(change: HubLaunchAgentChange, loaded: Bool, restarts: Bool)] = [
            (.unchanged, true, false), (.replaced, false, false), (.created, false, false), (.replaced, true, true)
        ]
        for scenario in cases {
            let rig = LifecycleRig(loaded: scenario.loaded)
            rig.agent.change = scenario.change
            rig.agent.installed = true
            var outcome: HubReconcileOutcome?
            rig.controller.reconcile(owner: rig.owner, ensure: { try rig.agent.ensureInstalled(requireConfig: false) }) { outcome = $0 }
            rig.scheduler.advance(by: 0)
            guard case .completed? = outcome else {
                XCTFail("reconcile completes: \(String(describing: outcome)) for \(scenario)")
                continue
            }
            let starts = rig.launchctlEvents.filter { $0 == "bootstrap" || $0.hasPrefix("kickstart") }
            let stops = rig.launchctlEvents.filter { $0 == "bootout" }
            XCTAssertEqual(!stops.isEmpty, scenario.restarts, "\(scenario): \(rig.launchctlEvents)")
            XCTAssertEqual(!starts.isEmpty, scenario.restarts,
                           "an unloaded or unchanged job is never started: \(scenario): \(rig.launchctlEvents)")
        }
    }

    func testReconcileRestartCannotInterleaveWithDeleteAndRestartTheHub() {
        // Reconcile's Restart is mid-flight (its bootout answer is held) when Delete is asked.
        // Delete must wait for the whole restart, then unload the job and remove the data
        // with the job unloaded, and nothing may start the job again afterwards.
        let rig = LifecycleRig(loaded: true)
        rig.agent.change = .replaced
        rig.agent.installed = true
        var loadedWhenDataWasRemoved: [Bool] = []
        var commandsBeforeDataRemoval = -1
        rig.remover.onDelete = {
            loadedWhenDataWasRemoved.append(rig.launchctl.loaded)
            commandsBeforeDataRemoval = rig.launchctlEvents.count
        }
        let held = rig.launchctl.holdNext("bootout")
        let controller = reconcilingController(rig)
        let reconciled = expectation(description: "reconcile finished")
        controller.reconcileLaunchAgent { reconciled.fulfill() }
        waitUntilHeld(held)
        XCTAssertNotNil(held.pending, "the restart must have reached its bootout")

        var deleteResult: Result<HubDataRemovalReport, Error>?
        rig.startDelete { deleteResult = $0 }
        XCTAssertNil(deleteResult, "Delete waits for the restart that is already in flight")
        XCTAssertFalse(rig.launchctlEvents.contains("disable"), "no Delete command while the restart is in flight")

        held.complete(.failure(HubActionError.commandFailed("bootout failed")))
        wait(for: [reconciled], timeout: 5)
        guard case .success? = deleteResult else { return XCTFail("Delete failed: \(String(describing: deleteResult))") }
        XCTAssertEqual(loadedWhenDataWasRemoved, [false], "the job is unloaded when the data is removed")
        XCTAssertFalse(rig.launchctl.loaded, "the job stays unloaded afterwards")
        let afterRemoval = Array(rig.launchctlEvents.dropFirst(max(0, commandsBeforeDataRemoval)))
        XCTAssertFalse(afterRemoval.contains { $0 == "bootstrap" || $0 == "kickstart" || $0 == "kickstart -k" },
                       "nothing starts the job after the data is gone: \(afterRemoval)")
        XCTAssertFalse(rig.controller.restartDutyOutstanding, "no restart duty survives Delete")
    }

    func testReconcileAfterDeleteAddsNoWorkButAfterARefusedDeleteStillRuns() {
        let deleted = LifecycleRig(loaded: true)
        deleted.agent.change = .replaced
        deleted.agent.installed = true
        guard case .success? = deleted.deleteHubAndData() else { return XCTFail("Delete failed") }
        deleted.events = []
        deleted.launchctl.removeAllCalls()
        let controller = reconcilingController(deleted)
        let done = expectation(description: "reconcile after delete")
        controller.reconcileLaunchAgent { done.fulfill() }
        wait(for: [done], timeout: 5)
        XCTAssertEqual(deleted.events, [], "once Delete owns the lifecycle, reconcile adds no work")
        XCTAssertFalse(deleted.controller.restartDutyOutstanding)

        let refused = LifecycleRig(loaded: true)
        refused.agent.change = .replaced
        refused.agent.installed = true
        refused.remover.checkErrors = [HubDataRemovalRefusal.item("~/Library/Application Support/Teslatlas Hub/x", .link)]
        guard case .failure? = refused.deleteHubAndData() else { return XCTFail("Delete should be refused") }
        refused.events = []
        let again = reconcilingController(refused)
        let ran = expectation(description: "reconcile after refused delete")
        again.reconcileLaunchAgent { ran.fulfill() }
        wait(for: [ran], timeout: 5)
        XCTAssertTrue(refused.events.contains("ensure"), "a refused Delete releases the lifecycle: \(refused.events)")
    }

    func testLifecycleOperationsRunOneAtATimeInSubmissionOrder() {
        let rig = LifecycleRig(loaded: true)
        let held = rig.launchctl.holdNext("bootout")
        var order: [String] = []
        rig.start("restart") { _ in order.append("restart") }
        XCTAssertNotNil(held.pending, "the restart is in flight at its bootout")
        let inFlight = rig.launchctlEvents
        rig.start("stop") { _ in order.append("stop") }
        rig.start("pause") { _ in order.append("pause") }
        XCTAssertEqual(rig.launchctlEvents, inFlight,
                       "a queued operation issues no command while an earlier one is in flight")
        XCTAssertEqual(order, [])

        held.complete(.failure(HubActionError.commandFailed("bootout failed")))
        XCTAssertEqual(order, ["restart", "stop", "pause"], "operations complete in submission order")
        let commands = rig.launchctlEvents
        let restartEnd = commands.firstIndex(of: "disable")
        XCTAssertNotNil(restartEnd)
        XCTAssertEqual(Array(commands.prefix(inFlight.count)), inFlight)
        // The restart's recovery (kickstart -k) ends before the stop's first command.
        let recovery = commands.firstIndex(of: "kickstart -k")
        XCTAssertNotNil(recovery)
        XCTAssertLessThan(recovery ?? .max, restartEnd ?? 0, "the stop begins only after the restart has finished")
    }

    // MARK: - Quit

    func testTerminationDecisionDefersForARestartDutyEvenWithAnIdleLifecycle() {
        // The owner's own duty (a restart owed a start) can outlive its lifecycle owner: the decision must not
        // rely on the background-owner term alone.
        XCTAssertEqual(AppDelegate.terminationReply(windows: [], lifecycle: .idle, restartDutyOutstanding: true), .terminateLater)
        XCTAssertEqual(AppDelegate.terminationReply(windows: [], lifecycle: .idle, restartDutyOutstanding: false), .terminateNow)
        let background = HubLifecycleCapabilities(publication: 1, state: .open, owner: .reconcile, phase: nil)
        XCTAssertTrue(background.owner?.isBackground == true)
        XCTAssertEqual(AppDelegate.terminationReply(windows: [], lifecycle: background, restartDutyOutstanding: false), .terminateLater)
    }

    func testQuitWaitsForOutstandingRestartDuty() throws {
        // Reconcile's restart owes a start while the stop is in flight: no UI transition.
        let rig = LifecycleRig(loaded: true)
        rig.agent.change = .replaced
        rig.agent.installed = true
        let held = rig.launchctl.holdNext("bootout")
        let controller = HubController(environment: [:],
                                       commandRunner: StatusOnlyRunner(output: "{}"),
                                       serviceRunner: rig.controller,
                                       launchAgent: rig.agent,
                                       dataRemover: nil,
                                       homeDirectory: HubTestScratch.directory,
                                       serviceInstalledOverride: true)
        let reconciled = expectation(description: "reconcile finished")
        controller.reconcileLaunchAgent { reconciled.fulfill() }
        let deadline = Date().addingTimeInterval(3)
        while held.pending == nil, Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        XCTAssertNotNil(held.pending, "the restart must have reached its bootout")
        XCTAssertTrue(controller.restartDutyOutstanding)

        var replies: [Bool] = []
        let delegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { replies.append($0) })
        delegate.hubController = controller
        XCTAssertEqual(delegate.applicationShouldTerminate(NSApplication.shared), .terminateLater)
        XCTAssertEqual(replies, [], "no reply before the duty settles")

        held.complete(.failure(HubActionError.commandFailed("bootout failed")))
        wait(for: [reconciled], timeout: 3)
        let replyDeadline = Date().addingTimeInterval(3)
        while replies.isEmpty, Date() < replyDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        XCTAssertEqual(replies, [true], "exactly one reply, after settlement")
        XCTAssertFalse(controller.restartDutyOutstanding)

        // With no controller on the host (the test host has none) the delegate answers now.
        XCTAssertEqual(AppDelegate(keyWindow: { nil }).applicationShouldTerminate(NSApplication.shared), .terminateNow)
    }

    /// Records the termination replies with the thread each one arrived on.
    private final class ReplyLog {
        private let lock = NSLock()
        private var entries: [(value: Bool, onMain: Bool)] = []
        func record(_ value: Bool) {
            lock.lock(); entries.append((value, Thread.isMainThread)); lock.unlock()
        }
        var all: [(value: Bool, onMain: Bool)] { lock.lock(); defer { lock.unlock() }; return entries }
    }

    func testQuitReplyArrivesOnMainThreadWhenTheRestartSettlesOnTheLifecycleQueue() throws {
        // Production runs every step on the service queue: the settled callback fires there.
        let rig = LifecycleRig(loaded: true, realQueue: true)
        rig.agent.change = .replaced
        rig.agent.installed = true
        let held = rig.launchctl.holdNext("bootout")
        let controller = HubController(environment: [:],
                                       commandRunner: StatusOnlyRunner(output: "{}"),
                                       serviceRunner: rig.controller,
                                       launchAgent: rig.agent,
                                       dataRemover: nil,
                                       homeDirectory: HubTestScratch.directory,
                                       serviceInstalledOverride: true)
        let reconciled = expectation(description: "reconcile finished")
        controller.reconcileLaunchAgent { reconciled.fulfill() }
        let deadline = Date().addingTimeInterval(3)
        while held.pending == nil, Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        XCTAssertNotNil(held.pending, "the restart must have reached its bootout")
        XCTAssertTrue(controller.restartDutyOutstanding)

        let log = ReplyLog()
        let delegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { log.record($0) })
        delegate.hubController = controller
        XCTAssertEqual(delegate.applicationShouldTerminate(NSApplication.shared), .terminateLater)
        // The restart fails and settles on a thread that is not the main thread.
        DispatchQueue.global().async { held.complete(.failure(HubActionError.commandFailed("bootout failed"))) }
        wait(for: [reconciled], timeout: 5)
        let replyDeadline = Date().addingTimeInterval(5)
        while log.all.isEmpty, Date() < replyDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        let replies = log.all
        XCTAssertEqual(replies.map(\.value), [true], "exactly one reply")
        XCTAssertEqual(replies.map(\.onMain), [true], "NSApplication.reply must be called on the main thread")
    }

    func testQuitReplyNeverRunsInsideApplicationShouldTerminate() {
        // The restart finishes between the check and the registration: the service calls
        // back at once. The reply must still follow the `.terminateLater` return.
        let controller = HubController(environment: [:],
                                       commandRunner: StatusOnlyRunner(output: "{}"),
                                       serviceRunner: SettlesDuringRegistrationService(),
                                       launchAgent: nil,
                                       dataRemover: nil,
                                       homeDirectory: HubTestScratch.directory,
                                       serviceInstalledOverride: true)
        let log = ReplyLog()
        let delegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { log.record($0) })
        delegate.hubController = controller
        XCTAssertEqual(delegate.applicationShouldTerminate(NSApplication.shared), .terminateLater)
        XCTAssertTrue(log.all.isEmpty, "no reply before applicationShouldTerminate has returned")
        let deadline = Date().addingTimeInterval(3)
        while log.all.isEmpty, Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        XCTAssertEqual(log.all.map(\.value), [true])
        XCTAssertEqual(log.all.map(\.onMain), [true])
    }

    func testQuitAfterReleasedRecordTerminatesNow() {
        let rig = LifecycleRig(loaded: true)
        _ = rig.run("pause")
        XCTAssertTrue(rig.controller.restartDutyOutstanding)
        // The custody site's refused history-only start policy releases the record.
        rig.controller.releaseRestartRecord(owner: rig.owner)
        let controller = HubController(environment: [:],
                                       commandRunner: StatusOnlyRunner(output: "{}"),
                                       serviceRunner: rig.controller,
                                       launchAgent: nil,
                                       dataRemover: nil,
                                       homeDirectory: HubTestScratch.directory,
                                       serviceInstalledOverride: true)
        var replies: [Bool] = []
        let delegate = AppDelegate(keyWindow: { nil }, replyToTerminate: { replies.append($0) })
        delegate.hubController = controller
        XCTAssertEqual(delegate.applicationShouldTerminate(NSApplication.shared), .terminateNow)
        XCTAssertEqual(replies, [], "no reply callback is registered when nothing is owed")
    }
}

// MARK: - Scripted collaborators

final class ManualClock {
    var now: TimeInterval = 10_000
}

final class ManualScheduler {
    private let clock: ManualClock
    private var pending: [(due: TimeInterval, sequence: Int, action: () -> Void)] = []
    private var sequence = 0
    private(set) var delays: [TimeInterval] = []

    init(clock: ManualClock) { self.clock = clock }

    func schedule(_ delay: TimeInterval, _ action: @escaping () -> Void) {
        delays.append(delay)
        sequence += 1
        pending.append((clock.now + delay, sequence, action))
    }

    /// Runs everything due within `seconds` of fake time, in order, moving the clock.
    func advance(by seconds: TimeInterval) {
        let target = clock.now + seconds
        while let next = pending.filter({ $0.due <= target }).min(by: { ($0.due, $0.sequence) < ($1.due, $1.sequence) }) {
            pending.removeAll { $0.sequence == next.sequence }
            clock.now = max(clock.now, next.due)
            next.action()
        }
        clock.now = max(clock.now, target)
    }
}

final class FakeLaunchAgent: HubLaunchAgentManaging {
    var events: LifecycleEvents
    var installed = true
    var change: HubLaunchAgentChange = .created
    var ensureError: Error?
    var prepareLogsErrors: [Error] = []
    var validateError: Error?
    var removeError: Error?
    var onPrepareLogs: (() -> Void)?

    init(events: LifecycleEvents) { self.events = events }

    var isInstalled: Bool { installed }

    func ensureInstalled(requireConfig: Bool) throws -> HubLaunchAgentChange {
        events.append(requireConfig ? "ensure(config)" : "ensure")
        if let ensureError { throw ensureError }
        return change
    }

    func prepareLogsForBootstrap() throws {
        events.append("prepareLogs")
        onPrepareLogs?()
        if !prepareLogsErrors.isEmpty { throw prepareLogsErrors.removeFirst() }
    }

    func validateRemovable() throws {
        events.append("validateRemovable")
        if let validateError { throw validateError }
    }

    func removeInstalled() throws {
        events.append("removeInstalled")
        if let removeError { throw removeError }
    }
}

final class FakeRemover: HubDataRemoving {
    var events: LifecycleEvents
    var checkErrors: [Error?] = []
    var report = HubDataRemovalReport(deleted: ["D1"], kept: [], warnings: [])
    /// Runs inside `delete`, at the moment the data would be removed.
    var onDelete: (() -> Void)?
    private(set) var checks = 0
    private(set) var deletes = 0

    init(events: LifecycleEvents) { self.events = events }

    func check() throws -> HubDataRemovalPlan {
        events.append("check")
        checks += 1
        if !checkErrors.isEmpty, let error = checkErrors.removeFirst() { throw error }
        return HubDataRemovalPlan(entries: [], preferencesPresent: false)
    }

    func delete(_ plan: HubDataRemovalPlan) -> HubDataRemovalReport {
        events.append("delete")
        deletes += 1
        onDelete?()
        return report
    }
}

final class LifecycleEvents {
    private let lock = NSLock()
    private var values: [String] = []
    var all: [String] {
        lock.lock(); defer { lock.unlock() }; return values
    }
    func append(_ value: String) { lock.lock(); values.append(value); lock.unlock() }
    func removeAll() { lock.lock(); values.removeAll(); lock.unlock() }
}

/// A held launchctl answer the test completes by hand.
final class HeldLaunchctlCall {
    fileprivate(set) var pending: ((Result<String, Error>) -> Void)?
    func complete(_ result: Result<String, Error>) {
        let completion = pending
        pending = nil
        completion?(result)
    }
}

final class FakeLaunchctl: HubLaunchctlRunning {
    enum Behavior {
        case result(Result<String, Error>)
        case hold(HeldLaunchctlCall)
    }

    let events: LifecycleEvents
    let clock: ManualClock
    private(set) var calls: [(arguments: [String], timeout: TimeInterval)] = []
    var loaded: Bool
    /// After `bootout`, the job stays loaded for this long.
    var unloadDelay: TimeInterval = 0
    var override: ((_ arguments: [String], _ ordinal: Int) -> Behavior?)?
    var elapse: ((_ arguments: [String]) -> TimeInterval)?
    var beforeFirstCall: (() -> Void)?
    private var unloadAt: TimeInterval?
    private var failures: [String: [Error]] = [:]
    private var holds: [String: HeldLaunchctlCall] = [:]

    init(events: LifecycleEvents, clock: ManualClock, loaded: Bool) {
        self.events = events
        self.clock = clock
        self.loaded = loaded
    }

    func removeAllCalls() { calls.removeAll() }
    func failNext(_ command: String, with error: Error) { failures[command, default: []].append(error) }
    func holdNext(_ command: String) -> HeldLaunchctlCall {
        let held = HeldLaunchctlCall()
        holds[command] = held
        return held
    }

    static var unloadedFailure: Error { LifecycleRig.unloadedFailure }

    func run(arguments: [String], timeout: TimeInterval,
             completion: @escaping (Result<String, Error>) -> Void) {
        if calls.isEmpty { beforeFirstCall?() }
        calls.append((arguments, timeout))
        let name = arguments.first ?? ""
        let label = name == "kickstart" && arguments.contains("-k") ? "kickstart -k" : name
        events.append("launchctl " + label)
        let ordinal = calls.filter { $0.arguments.first == name }.count - 1
        if let elapse { clock.now += elapse(arguments) }
        if let held = holds.removeValue(forKey: name) {
            held.pending = completion
            return
        }
        if let behavior = override?(arguments, ordinal) {
            switch behavior {
            case let .result(result): completion(result)
            case let .hold(held): held.pending = completion
            }
            return
        }
        if var queue = failures[name], !queue.isEmpty {
            let error = queue.removeFirst()
            failures[name] = queue
            completion(.failure(error))
            return
        }
        switch name {
        case "print":
            if loaded, let at = unloadAt, clock.now >= at { loaded = false; unloadAt = nil }
            completion(loaded ? .success("") : .failure(LifecycleRig.unloadedFailure))
        case "bootout":
            unloadAt = unloadDelay.isInfinite ? nil : clock.now + unloadDelay
            if unloadDelay == 0 { loaded = false; unloadAt = nil }
            completion(.success(""))
        case "bootstrap", "kickstart":
            loaded = true
            unloadAt = nil
            completion(.success(""))
        default:
            completion(.success(""))
        }
    }
}

/// A controller over scripted collaborators, a manual clock and scheduler and a
/// synchronous executor: every step runs inline unless a launchctl call is held.
final class LifecycleRig {
    static var unloadedFailure: Error {
        HubActionError.commandExited(113, "Could not find service \"com.teslatlas.hub\" in domain for user gui: \(getuid())")
    }

    let recorder = LifecycleEvents()
    let clock = ManualClock()
    let scheduler: ManualScheduler
    let agent: FakeLaunchAgent
    let launchctl: FakeLaunchctl
    let remover: FakeRemover
    var preflightResult: Result<String, Error> = .success("")
    private(set) var completionCount = 0
    private(set) var completedAt: TimeInterval = 0
    private(set) var controller: LaunchctlServiceController!
    /// The owner of every operation the rig starts; `otherOwner()` makes a second one.
    let owner = makeLifecycleToken(.start)
    func otherOwner(_ family: HubLifecycleFamily = .diagnostics) -> HubLifecycleToken { makeLifecycleToken(family) }

    /// `realQueue`: the controller keeps its own serial queue, as in production, so its
    /// callbacks fire off the main thread.
    init(loaded: Bool = false, realQueue: Bool = false) {
        scheduler = ManualScheduler(clock: clock)
        agent = FakeLaunchAgent(events: recorder)
        launchctl = FakeLaunchctl(events: recorder, clock: clock, loaded: loaded)
        remover = FakeRemover(events: recorder)
        controller = LaunchctlServiceController(
            agent: agent,
            launchctl: launchctl,
            preflight: { [unowned self] completion in
                self.recorder.append("preflight")
                completion(self.preflightResult)
            },
            dataRemover: remover,
            uid: getuid(),
            plist: URL(fileURLWithPath: "/fixture/Library/LaunchAgents/com.teslatlas.hub.plist"),
            clock: { [unowned self] in self.clock.now },
            schedule: { [unowned self] delay, action in self.scheduler.schedule(delay, action) },
            executor: realQueue ? nil : { $0() }
        )
    }

    var events: [String] {
        get { recorder.all }
        set { if newValue.isEmpty { recorder.removeAll() } }
    }

    /// The launchctl commands, without service, domain or plist arguments.
    var launchctlEvents: [String] {
        recorder.all.filter { $0.hasPrefix("launchctl ") }.map { String($0.dropFirst("launchctl ".count)) }
    }

    @discardableResult
    func run(_ action: String) -> Result<String, Error>? {
        var result: Result<String, Error>?
        start(action) { result = $0 }
        scheduler.advance(by: 0)
        return result
    }

    func start(_ action: String, completion: @escaping (Result<String, Error>) -> Void) {
        controller.run(arguments: ["service", action], owner: owner) { [self] result in
            completionCount += 1
            completedAt = clock.now
            completion(result)
        }
    }

    @discardableResult
    func recover() -> Result<String, Error>? {
        var result: Result<String, Error>?
        controller.recoverAfterPause(owner: owner) { result = $0 }
        return result
    }

    @discardableResult
    func deleteHubAndData() -> Result<HubDataRemovalReport, Error>? {
        var result: Result<HubDataRemovalReport, Error>?
        startDelete { result = $0 }
        scheduler.advance(by: 0)
        return result
    }

    func startDelete(_ completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        controller.deleteHubAndData(owner: owner, completion: completion)
    }
}

private final class StatusOnlyRunner: HubCommandRunning {
    private let output: String
    init(output: String) { self.output = output }
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        completion(.success(output))
    }
}

/// A service that owes a restart when asked, and has settled by the time it is asked to call back.
private final class SettlesDuringRegistrationService: HubServiceControlling {
    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) { completion(.success("")) }
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool { false }
    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) { completion(.unloaded) }
    var restartDutyOutstanding: Bool { true }
    func whenRestartDutySettled(_ callback: @escaping () -> Void) { callback() }
    func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) { completion(.success("")) }
    func releaseRestartRecord(owner: HubLifecycleToken) {}
    func reconcile(owner: HubLifecycleToken, ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) { completion(.completed) }
    func deleteHubAndData(owner: HubLifecycleToken, completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        completion(.failure(HubActionError.preview))
    }
}

/// A service whose Restart fails with a recovery that could not be confirmed.
private final class UnconfirmedRestartService: HubServiceControlling {
    private let running: Bool
    private(set) var stopCalls = 0

    init(running: Bool) { self.running = running }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        if arguments.last == "stop" { stopCalls += 1 }
        completion(.failure(HubServiceRestartReport(original: HubActionError.commandFailed("bootstrap failed"),
                                                    outcome: .unconfirmed)))
    }
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        run(arguments: arguments, completion: completion)
    }
    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool { false }

    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) {
        completion(running ? .loaded : .unloaded)
    }

    var restartDutyOutstanding: Bool { false }
    func whenRestartDutySettled(_ callback: @escaping () -> Void) { callback() }
    func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) { completion(.success("")) }
    func releaseRestartRecord(owner: HubLifecycleToken) {}
    func reconcile(owner: HubLifecycleToken, ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) { completion(.completed) }
    func deleteHubAndData(owner: HubLifecycleToken, completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        completion(.failure(HubActionError.preview))
    }
}

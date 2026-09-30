// SPDX-License-Identifier: AGPL-3.0-only
import Foundation
import Darwin

/// Only typed, non-sensitive evidence enters this development-only journal.
final class DevelopmentEventLog {
    enum Kind: String { case command, control, preflight }
    enum Action: String { case start, stop, restart, migrate, doctor, preflight, status, other }
    enum Outcome: String { case start, complete, failed, timeout }
    private static let registryLock = NSLock()
    private static var registry: [String: DevelopmentEventLog] = [:]
    private let directory: URL
    private let queue = DispatchQueue(label: "eu.teslatlas.hub.dev-journal", qos: .utility)
    private let lock = NSLock()
    private var pending = 0
    private var dropped: UInt64 = 0
    private let session = UUID().uuidString
    private let limit: Int
    static func shared(directory: URL) -> DevelopmentEventLog {
        registryLock.lock(); defer { registryLock.unlock() }
        if let found = registry[directory.path] { return found }
        let logger = DevelopmentEventLog(directory: directory)
        registry[directory.path] = logger
        return logger
    }
    init(directory: URL, limit: Int = 10 * 1024 * 1024) { self.directory = directory; self.limit = limit }
    static func action(arguments: [String]) -> Action {
        for (name, action) in [("migrate", Action.migrate), ("doctor", .doctor), ("serve-preflight", .preflight), ("status", .status)] where arguments.contains(name) { return action }
        return .other
    }
    func record(kind: Kind, action: Action, outcome: Outcome, operation: UUID, durationMs: UInt64 = 0, exitCode: Int32? = nil) {
        lock.lock()
        guard pending < 64 else { dropped &+= 1; lock.unlock(); return }
        pending += 1; lock.unlock()
        let utcMs = UInt64(max(0, Date().timeIntervalSince1970 * 1000))
        queue.async { [self] in
            lock.lock(); let lost = dropped; lock.unlock()
            var frame: [String: Any] = ["schema": 1, "utc_ms": utcMs, "session": session, "pid": getpid(), "operation": operation.uuidString, "kind": kind.rawValue, "action": action.rawValue, "outcome": outcome.rawValue, "duration_ms": durationMs, "dropped_total": lost]
            if let exitCode { frame["exit_code"] = exitCode }
            do { try append(JSONSerialization.data(withJSONObject: frame, options: [.sortedKeys])) }
            catch { lock.lock(); dropped &+= 1; lock.unlock() }
            lock.lock(); pending -= 1; lock.unlock()
        }
    }
    func completion(kind: Kind, action: Action, operation: UUID, started: UInt64, result: Result<String, Error>) {
        var outcome: Outcome = .complete
        var code: Int32?
        if case let .failure(error) = result {
            outcome = .failed
            if case HubActionError.commandTimedOut = error { outcome = .timeout }
            if case let HubActionError.commandExited(status, _) = error { code = status }
        }
        record(kind: kind, action: action, outcome: outcome, operation: operation,
               durationMs: (DispatchTime.now().uptimeNanoseconds - started) / 1_000_000, exitCode: code)
    }
    @discardableResult func drain(timeout: TimeInterval = 0.5) -> Bool {
        let done = DispatchSemaphore(value: 0)
        queue.async { done.signal() }
        return done.wait(timeout: .now() + timeout) == .success
    }
    static func drainAll() {
        registryLock.lock(); let journals = Array(registry.values); registryLock.unlock()
        for journal in journals { journal.drain(timeout: 0.25) }
    }
    private func ownedFile(_ dir: Int32, _ name: String, create: Bool) throws -> Int32 {
        let fd = openat(dir, name, O_RDWR | O_APPEND | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK | (create ? O_CREAT : 0), mode_t(0o600))
        guard fd >= 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
        var info = stat()
        guard fstat(fd, &info) == 0, info.st_mode & S_IFMT == S_IFREG, info.st_uid == getuid(), info.st_mode & 0o777 == 0o600, info.st_nlink == 1 else {
            close(fd); throw POSIXError(.EPERM)
        }
        return fd
    }
    static func validateDirectoryReadOnly(_ directory: URL,
                                         allowOwnedGroupWritableAncestors: Bool = true,
                                         afterOpeningComponent: ((String) -> Void)? = nil) throws -> Int32 {
        guard directory.path.hasPrefix("/"), directory.standardizedFileURL.path == directory.path else { throw POSIXError(.EPERM) }
        let components = directory.pathComponents.dropFirst()
        let flags = O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW
        var descriptor = open("/", flags)
        guard descriptor >= 0 else { throw POSIXError(.EPERM) }
        do {
            func validate(_ leaf: Bool) throws {
                var entry = stat()
                guard fstat(descriptor, &entry) == 0, entry.st_mode & S_IFMT == S_IFDIR,
                      entry.st_uid == 0 || entry.st_uid == getuid(), entry.st_mode & 0o002 == 0,
                      entry.st_mode & 0o020 == 0 || (allowOwnedGroupWritableAncestors && entry.st_uid == getuid() && entry.st_gid == getgid()),
                      !leaf || (entry.st_uid == getuid() && entry.st_mode & 0o777 == 0o700) else { throw POSIXError(.EPERM) }
            }
            try validate(components.isEmpty)
            var opened = ""
            for (index, component) in components.enumerated() {
                guard component != ".", component != ".." else { throw POSIXError(.EPERM) }
                let next = openat(descriptor, component, flags)
                guard next >= 0 else { throw POSIXError(.EPERM) }
                close(descriptor); descriptor = next
                try validate(index == components.count - 1)
                opened += "/" + component
                afterOpeningComponent?(opened)
            }
            return descriptor
        } catch {
            close(descriptor)
            throw error
        }
    }
    private func append(_ payload: Data) throws {
        let dir = try Self.validateDirectoryReadOnly(directory)
        defer { close(dir) }
        let journalLock = try ownedFile(dir, ".appkit-events.lock", create: true)
        defer { close(journalLock) }
        guard flock(journalLock, LOCK_EX) == 0 else { throw POSIXError(.EIO) }
        defer { _ = flock(journalLock, LOCK_UN) }
        func name(_ i: Int) -> String { "appkit-events.\(i).jsonl" }
        for i in 0..<5 {
            let fd: Int32
            do { fd = try ownedFile(dir, name(i), create: false) }
            catch let error as POSIXError where error.code == .ENOENT { continue }
            var entry = stat(); _ = fstat(fd, &entry); close(fd)
            if Date().timeIntervalSince1970 - Double(entry.st_mtimespec.tv_sec) > 7 * 86400 { guard unlinkat(dir, name(i), 0) == 0 else { throw POSIXError(.EIO) } }
        }
        let current = try ownedFile(dir, name(0), create: true)
        var currentInfo = stat(); _ = fstat(current, &currentInfo); close(current)
        if currentInfo.st_size + off_t(payload.count + 1) > limit {
            if unlinkat(dir, name(4), 0) != 0 && errno != ENOENT { throw POSIXError(.EIO) }
            for i in (0..<4).reversed() {
                if renameat(dir, name(i), dir, name(i + 1)) != 0 && errno != ENOENT { throw POSIXError(.EIO) }
            }
        }
        let fd = try ownedFile(dir, name(0), create: true); defer { close(fd) }
        var bytes = payload; bytes.append(10)
        try bytes.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let n = write(fd, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                guard n > 0 else { if errno == EINTR { continue }; throw POSIXError(.EIO) }
                offset += n
            }
        }
    }
}

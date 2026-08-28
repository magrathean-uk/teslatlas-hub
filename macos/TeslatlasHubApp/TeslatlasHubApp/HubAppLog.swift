import Foundation
import OSLog
import Darwin

final class HubAppLog {
    static let shared = HubAppLog()
    static let unavailableText = "App diagnostics are unavailable or the log path is unsafe.\n"

    private static let maximumFileBytes = 1024 * 1024
    private static let retainedFileBytes = 512 * 1024
    private static let maximumLineBytes = 16 * 1024
    private static let maximumUnifiedLogBytes = 512
    private let lock = NSLock()
    private let fileURL: URL
    private let logger = Logger(subsystem: Bundle.main.bundleIdentifier ?? "eu.teslatlas.hub.app",
                                category: "diagnostics")

    init(fileURL: URL? = nil) {
        self.fileURL = fileURL ?? Self.defaultLogURL(
            environment: ProcessInfo.processInfo.environment,
            homeDirectory: FileManager.default.homeDirectoryForCurrentUser,
            temporaryDirectory: FileManager.default.temporaryDirectory,
            processIdentifier: getpid()
        )
    }

    static func defaultLogURL(environment: [String: String],
                              homeDirectory: URL,
                              temporaryDirectory: URL,
                              processIdentifier: Int32) -> URL {
        let isTesting = environment["XCTestConfigurationFilePath"] != nil
            || environment["XCTestBundlePath"] != nil
            || environment["XCInjectBundleInto"] != nil
        if isTesting {
            return temporaryDirectory
                .appendingPathComponent("TeslatlasHubTests-\(processIdentifier)", isDirectory: true)
                .appendingPathComponent("app.log")
        }
        return homeDirectory.appendingPathComponent("Library/Logs/Teslatlas Hub/app.log")
    }

    func record(_ name: String,
                category: String,
                level: String = "INFO",
                fields: [String: String] = [:]) {
        let safeFields = fields
            .sorted { $0.key < $1.key }
            .map { key, value in
                singleLine(HubShareRedactor.redact("\(key)=\(value)"))
            }
            .joined(separator: " ")
        let suffix = safeFields.isEmpty ? "" : " \(safeFields)"
        let normalizedLevel = singleLine(level).uppercased()
        let line = Self.boundedLine(
            "\(Self.timestamp()) [\(normalizedLevel)] \(singleLine(category)) \(singleLine(name))\(suffix)\n"
        )
        let unifiedLine = Self.boundedLine(line, maximumBytes: Self.maximumUnifiedLogBytes)
        let unifiedMessage = unifiedLine.trimmingCharacters(in: .newlines)
        switch normalizedLevel {
        case "ERROR": logger.error("\(unifiedMessage, privacy: .public)")
        case "WARN", "WARNING": logger.warning("\(unifiedMessage, privacy: .public)")
        case "DEBUG": logger.debug("\(unifiedMessage, privacy: .public)")
        default: logger.info("\(unifiedMessage, privacy: .public)")
        }

        lock.lock()
        defer { lock.unlock() }
        append(line)
    }

    func recentText(maximumBytes: Int = 256 * 1024) -> String {
        lock.lock()
        defer { lock.unlock() }
        return Self.regularFileTail(of: fileURL,
                                    maximumBytes: maximumBytes,
                                    hardLimit: Self.maximumFileBytes)
            ?? Self.unavailableText
    }

    static func errorCode(_ error: Error) -> String {
        if let error = error as? TeslaAuthError {
            switch error {
            case .cancelled: return "cancelled"
            case .randomGeneration: return "random_generation"
            case .invalidAuthorizationURL: return "invalid_authorization_url"
            case .invalidCallback: return "invalid_callback"
            case .stateMismatch: return "state_mismatch"
            case .invalidIssuer: return "invalid_issuer"
            case .invalidResponse: return "invalid_response"
            case .exchangeFailed: return "exchange_failed"
            }
        }
        if let error = error as? HubActionError {
            switch error {
            case .preview: return "preview_read_only"
            case .missingResource: return "missing_resource"
            case .untrustedInstaller: return "untrusted_installer"
            case .commandFailed: return "command_failed"
            case let .commandExited(status, _): return "command_exited_\(status)"
            case .commandTimedOut: return "command_timed_out"
            }
        }
        let foundationError = error as NSError
        switch foundationError.domain {
        case NSCocoaErrorDomain: return "cocoa_\(foundationError.code)"
        case NSURLErrorDomain: return "url_\(foundationError.code)"
        case NSPOSIXErrorDomain: return "posix_\(foundationError.code)"
        default: return String(describing: type(of: error))
        }
    }

    static func writePrivateReport(_ text: String, to destination: URL) throws {
        let descriptor = Darwin.open(
            destination.path,
            O_WRONLY | O_CREAT | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK,
            S_IRUSR | S_IWUSR
        )
        guard descriptor >= 0 else { throw CocoaError(.fileWriteUnknown) }
        defer { Darwin.close(descriptor) }
        var information = stat()
        guard fstat(descriptor, &information) == 0,
              information.st_mode & S_IFMT == S_IFREG,
              information.st_uid == getuid(),
              fchmod(descriptor, S_IRUSR | S_IWUSR) == 0,
              ftruncate(descriptor, 0) == 0 else {
            throw CocoaError(.fileWriteUnknown)
        }
        try write(Data(text.utf8), to: descriptor)
        guard fsync(descriptor) == 0 else { throw CocoaError(.fileWriteUnknown) }
    }

    private func append(_ line: String) {
        let manager = FileManager.default
        let directory = fileURL.deletingLastPathComponent()
        do {
            try manager.createDirectory(at: directory,
                                        withIntermediateDirectories: true,
                                        attributes: [.posixPermissions: 0o700])
            guard let directoryDescriptor = Self.ownedDirectoryDescriptor(at: directory) else {
                return
            }
            defer { Darwin.close(directoryDescriptor) }
            guard fchmod(directoryDescriptor, S_IRWXU) == 0 else { return }
            let descriptor = Darwin.openat(
                directoryDescriptor,
                fileURL.lastPathComponent,
                O_RDWR | O_APPEND | O_CREAT | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK,
                S_IRUSR | S_IWUSR
            )
            guard descriptor >= 0 else { return }
            defer { Darwin.close(descriptor) }
            var information = stat()
            guard fstat(descriptor, &information) == 0,
                  information.st_mode & S_IFMT == S_IFREG,
                  information.st_uid == getuid(),
                  information.st_size >= 0 else { return }
            guard fchmod(descriptor, S_IRUSR | S_IWUSR) == 0 else { return }

            if information.st_size + off_t(line.utf8.count) > off_t(Self.maximumFileBytes) {
                let retainedBytes = min(Int(information.st_size), Self.retainedFileBytes)
                let retained = try Self.read(descriptor: descriptor,
                                             offset: information.st_size - off_t(retainedBytes),
                                             maximumBytes: retainedBytes)
                guard ftruncate(descriptor, 0) == 0 else { return }
                try Self.write(retained, to: descriptor)
            }
            try Self.write(Data(line.utf8), to: descriptor)
        } catch {
            logger.error("Could not persist Hub app diagnostics: \(String(describing: type(of: error)), privacy: .public)")
        }
    }

    private func singleLine(_ value: String) -> String {
        value.replacingOccurrences(of: "\n", with: " ")
            .replacingOccurrences(of: "\r", with: " ")
    }

    private static func timestamp() -> String {
        ISO8601DateFormatter().string(from: Date())
    }

    private static func boundedLine(_ line: String,
                                    maximumBytes: Int = maximumLineBytes) -> String {
        let data = Data(line.utf8)
        guard data.count > maximumBytes else { return line }
        let marker = Data(" [truncated]\n".utf8)
        var bounded = Data(data.prefix(maximumBytes - marker.count))
        bounded.append(marker)
        return String(decoding: bounded, as: UTF8.self)
    }

    static func regularFileTail(of url: URL,
                                maximumBytes: Int,
                                hardLimit: Int = maximumFileBytes) -> String? {
        let boundedMaximum = min(maximumBytes, hardLimit)
        guard boundedMaximum > 0 else { return nil }
        guard let directoryDescriptor = ownedDirectoryDescriptor(
            at: url.deletingLastPathComponent()
        ) else { return nil }
        defer { Darwin.close(directoryDescriptor) }
        let descriptor = Darwin.openat(
            directoryDescriptor,
            url.lastPathComponent,
            O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK
        )
        guard descriptor >= 0 else { return nil }
        defer { Darwin.close(descriptor) }
        var information = stat()
        guard fstat(descriptor, &information) == 0,
              information.st_mode & S_IFMT == S_IFREG,
              information.st_size >= 0 else { return nil }
        let offset = max(off_t(0), information.st_size - off_t(boundedMaximum))
        guard let data = try? read(descriptor: descriptor,
                                  offset: offset,
                                  maximumBytes: boundedMaximum) else { return nil }
        return String(decoding: data, as: UTF8.self)
    }

    static func regularFileData(of url: URL, maximumBytes: Int) -> Data? {
        guard maximumBytes > 0 else { return nil }
        guard let directoryDescriptor = ownedDirectoryDescriptor(
            at: url.deletingLastPathComponent()
        ) else { return nil }
        defer { Darwin.close(directoryDescriptor) }
        let descriptor = Darwin.openat(
            directoryDescriptor,
            url.lastPathComponent,
            O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK
        )
        guard descriptor >= 0 else { return nil }
        defer { Darwin.close(descriptor) }
        var information = stat()
        guard fstat(descriptor, &information) == 0,
              information.st_mode & S_IFMT == S_IFREG,
              information.st_size >= 0,
              information.st_size <= off_t(maximumBytes) else { return nil }
        return try? read(descriptor: descriptor,
                         offset: 0,
                         maximumBytes: Int(information.st_size))
    }

    private static func ownedDirectoryDescriptor(at url: URL) -> Int32? {
        let descriptor = Darwin.open(
            url.path,
            O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK
        )
        guard descriptor >= 0 else { return nil }
        var information = stat()
        guard fstat(descriptor, &information) == 0,
              information.st_mode & S_IFMT == S_IFDIR,
              information.st_uid == getuid(),
              information.st_mode & 0o022 == 0 else {
            Darwin.close(descriptor)
            return nil
        }
        return descriptor
    }

    private static func read(descriptor: Int32,
                             offset: off_t,
                             maximumBytes: Int) throws -> Data {
        guard Darwin.lseek(descriptor, offset, SEEK_SET) >= 0 else {
            throw CocoaError(.fileReadUnknown)
        }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: min(16 * 1024, max(1, maximumBytes)))
        while data.count < maximumBytes {
            let requested = min(buffer.count, maximumBytes - data.count)
            let count = buffer.withUnsafeMutableBytes { bytes in
                Darwin.read(descriptor, bytes.baseAddress, requested)
            }
            if count == 0 { break }
            if count < 0 {
                if errno == EINTR { continue }
                throw CocoaError(.fileReadUnknown)
            }
            data.append(contentsOf: buffer.prefix(count))
        }
        return data
    }

    private static func write(_ data: Data, to descriptor: Int32) throws {
        try data.withUnsafeBytes { bytes in
            var written = 0
            while written < bytes.count {
                let count = Darwin.write(descriptor,
                                         bytes.baseAddress?.advanced(by: written),
                                         bytes.count - written)
                if count < 0 {
                    if errno == EINTR { continue }
                    throw CocoaError(.fileWriteUnknown)
                }
                guard count > 0 else { throw CocoaError(.fileWriteUnknown) }
                written += count
            }
        }
    }
}

import Foundation
import OSLog

final class HubAppLog {
    static let shared = HubAppLog()

    private static let maximumFileBytes = 1024 * 1024
    private static let retainedFileBytes = 512 * 1024
    private static let maximumLineBytes = 16 * 1024
    private static let maximumUnifiedLogBytes = 512
    private let lock = NSLock()
    private let fileURL: URL
    private let logger = Logger(subsystem: Bundle.main.bundleIdentifier ?? "eu.teslatlas.hub.app",
                                category: "diagnostics")

    init(fileURL: URL? = nil) {
        self.fileURL = fileURL ?? FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/Teslatlas Hub/app.log")
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
        let line = Self.boundedLine(
            "\(Self.timestamp()) [\(singleLine(level))] \(singleLine(category)) \(singleLine(name))\(suffix)\n"
        )
        let unifiedLine = Self.boundedLine(line, maximumBytes: Self.maximumUnifiedLogBytes)
        logger.info("\(unifiedLine.trimmingCharacters(in: .newlines), privacy: .public)")

        lock.lock()
        defer { lock.unlock() }
        append(line)
    }

    func recentText(maximumBytes: Int = 256 * 1024) -> String {
        lock.lock()
        defer { lock.unlock() }
        guard maximumBytes > 0,
              let values = try? fileURL.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey]),
              values.isRegularFile == true,
              values.isSymbolicLink != true,
              let handle = try? FileHandle(forReadingFrom: fileURL) else {
            return "No app diagnostics are available yet.\n"
        }
        defer { try? handle.close() }
        do {
            let size = try handle.seekToEnd()
            let boundedMaximum = UInt64(min(maximumBytes, Self.maximumFileBytes))
            try handle.seek(toOffset: size > boundedMaximum ? size - boundedMaximum : 0)
            let data = try handle.read(upToCount: Int(boundedMaximum)) ?? Data()
            return String(decoding: data, as: UTF8.self)
        } catch {
            return "App diagnostics could not be read.\n"
        }
    }

    static func errorCode(_ error: Error) -> String {
        guard let error = error as? HubActionError else {
            return String(describing: type(of: error))
        }
        switch error {
        case .preview: return "preview_read_only"
        case .missingResource: return "missing_resource"
        case .untrustedInstaller: return "untrusted_installer"
        case .commandFailed: return "command_failed"
        case let .commandExited(status, _): return "command_exited_\(status)"
        case .commandTimedOut: return "command_timed_out"
        }
    }

    private func append(_ line: String) {
        let manager = FileManager.default
        let directory = fileURL.deletingLastPathComponent()
        do {
            try manager.createDirectory(at: directory,
                                        withIntermediateDirectories: true,
                                        attributes: [.posixPermissions: 0o700])
            try manager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
            if manager.fileExists(atPath: fileURL.path) {
                let values = try fileURL.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])
                guard values.isRegularFile == true, values.isSymbolicLink != true else { return }
            }
            if let size = (try? manager.attributesOfItem(atPath: fileURL.path)[.size]) as? NSNumber,
               size.intValue + line.utf8.count > Self.maximumFileBytes {
                let data = try Self.tailData(of: fileURL, maximumBytes: Self.retainedFileBytes)
                try data.write(to: fileURL, options: .atomic)
            }
            if !manager.fileExists(atPath: fileURL.path) {
                guard manager.createFile(atPath: fileURL.path,
                                         contents: nil,
                                         attributes: [.posixPermissions: 0o600]) else { return }
            }
            let handle = try FileHandle(forWritingTo: fileURL)
            try handle.seekToEnd()
            try handle.write(contentsOf: Data(line.utf8))
            try handle.close()
            try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fileURL.path)
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

    private static func tailData(of url: URL, maximumBytes: Int) throws -> Data {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        let size = try handle.seekToEnd()
        let boundedMaximum = UInt64(max(0, maximumBytes))
        try handle.seek(toOffset: size > boundedMaximum ? size - boundedMaximum : 0)
        return try handle.read(upToCount: Int(boundedMaximum)) ?? Data()
    }
}

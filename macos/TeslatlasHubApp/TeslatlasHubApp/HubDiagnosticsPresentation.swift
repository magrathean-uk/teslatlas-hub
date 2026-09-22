// SPDX-License-Identifier: AGPL-3.0-only

import Foundation

enum HubDiagnosticOutcome: Equatable {
    case passed
    case failed
}

struct HubDiagnosticRow: Equatable {
    let title: String
    let detail: String
    let outcome: HubDiagnosticOutcome
}

enum HubDiagnosticsPresentation {
    static func rows(from report: String) -> [HubDiagnosticRow] {
        report.components(separatedBy: "\n\n").compactMap(parseSection)
    }

    private static func parseSection(_ section: String) -> HubDiagnosticRow? {
        let lines = section.components(separatedBy: .newlines)
        guard let heading = lines.first?.trimmingCharacters(in: .whitespaces),
              heading.hasPrefix("== "), heading.hasSuffix(" ==") else { return nil }

        let rawTitle = String(heading.dropFirst(3).dropLast(3))
        let failedMarker = " (failed)"
        let isFailed = rawTitle.hasSuffix(failedMarker)
        let title = isFailed ? String(rawTitle.dropLast(failedMarker.count)) : rawTitle
        guard !title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }

        let detailLines = lines.dropFirst().compactMap { line -> String? in
            let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty || isDurationLine(trimmed) ? nil : trimmed
        }
        let detail = summarizedDetail(from: detailLines)

        return HubDiagnosticRow(title: displayTitle(for: title), detail: detail,
                                outcome: isFailed ? .failed : .passed)
    }

    private static func isDurationLine(_ line: String) -> Bool {
        line.hasPrefix("Duration:") || line.hasPrefix("Read duration:")
    }

    private static func summarizedDetail(from lines: [String]) -> String {
        guard let first = lines.first else { return "" }
        guard first.hasPrefix("{") || first.hasPrefix("[") else { return first }
        let data = Data(lines.joined(separator: "\n").utf8)
        guard let value = try? JSONSerialization.jsonObject(with: data) else {
            return first.hasPrefix("{") ? "Structured report available in raw details." : first
        }
        if let object = value as? [String: Any] {
            var fields: [String] = []
            for (key, label) in [("status", "Status"), ("message", "Message"),
                                 ("provider", "Provider"), ("version", "Version")] {
                if let value = object[key] as? String, !value.isEmpty {
                    fields.append("\(label): \(value)")
                }
            }
            for (key, label) in [("ready", "Ready"), ("compatible", "Compatible")] {
                if let value = object[key] as? Bool {
                    fields.append("\(label): \(value ? "yes" : "no")")
                }
            }
            if !fields.isEmpty { return fields.prefix(3).joined(separator: " · ") }
        } else if let values = value as? [Any] {
            return "\(values.count) structured result\(values.count == 1 ? "" : "s")."
        }
        return "Structured report available in raw details."
    }

    private static func displayTitle(for rawTitle: String) -> String {
        let normalized = rawTitle.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        let prefix = normalized.split(separator: " —", maxSplits: 1, omittingEmptySubsequences: true).first ?? ""
        switch prefix.lowercased() {
        case "doctor": return "Environment doctor"
        case "preflight": return "Preflight"
        case "status": return "Status"
        case "recent logs": return "Recent logs"
        case "service pause": return "Service pause"
        case "service state check": return "Service state check"
        case "service resume": return "Service resume"
        case "support metadata": return "Support metadata"
        default:
            guard let first = normalized.first else { return normalized }
            return first.uppercased() + normalized.dropFirst()
        }
    }
}

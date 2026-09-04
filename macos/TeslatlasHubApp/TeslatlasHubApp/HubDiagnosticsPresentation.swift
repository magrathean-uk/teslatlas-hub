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

        let detail = lines.dropFirst().first { line in
            let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
            return !trimmed.isEmpty && !isDurationLine(trimmed)
        }?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

        return HubDiagnosticRow(title: displayTitle(for: title), detail: detail,
                                outcome: isFailed ? .failed : .passed)
    }

    private static func isDurationLine(_ line: String) -> Bool {
        line.hasPrefix("Duration:") || line.hasPrefix("Read duration:")
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

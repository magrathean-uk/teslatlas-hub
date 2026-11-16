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
    static func rows(from report: String, bundle: Bundle = .main) -> [HubDiagnosticRow] {
        report.components(separatedBy: "\n\n").compactMap { parseSection($0, bundle: bundle) }
    }

    private static func parseSection(_ section: String, bundle: Bundle) -> HubDiagnosticRow? {
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

        return HubDiagnosticRow(title: displayTitle(for: title, bundle: bundle), detail: detail,
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
            return first.hasPrefix("{") ? HubL10n.text("hub.HubDiagnosticsPresentation.51.1418", fallback: "Structured report available in raw details.") : first
        }
        if let object = value as? [String: Any] {
            var fields: [String] = []
            for (key, label) in [("status", HubL10n.text("hub.HubDiagnosticsPresentation.status_label", fallback: "Status")),
                                 ("message", HubL10n.text("hub.HubDiagnosticsPresentation.message_label", fallback: "Message")),
                                 ("provider", HubL10n.text("hub.ServiceDetailsWindowController.115.2186", fallback: "Provider")),
                                 ("version", HubL10n.text("hub.ServiceDetailsWindowController.113.2182", fallback: "Version"))] {
                if let value = object[key] as? String, !value.isEmpty {
                    fields.append(HubL10n.format("hub.HubDiagnosticsPresentation.58.1427", fallback: "%1$@: %2$@", arguments: [String(describing: label), String(describing: value)]))
                }
            }
            for (key, label) in [("ready", HubL10n.text("hub.HubDiagnosticsPresentation.ready_label", fallback: "Ready")),
                                 ("compatible", HubL10n.text("hub.HubDiagnosticsPresentation.compatible_label", fallback: "Compatible"))] {
                if let value = object[key] as? Bool {
                    let answer = value
                        ? HubL10n.text("hub.HubDiagnosticsPresentation.boolean_yes", fallback: "yes")
                        : HubL10n.text("hub.HubDiagnosticsPresentation.boolean_no", fallback: "no")
                    fields.append(HubL10n.format("hub.HubDiagnosticsPresentation.58.1427", fallback: "%1$@: %2$@", arguments: [label, answer]))
                }
            }
            if !fields.isEmpty { return fields.prefix(3).joined(separator: " · ") }
        } else if let values = value as? [Any] {
            return HubL10n.plural("hub.HubDiagnosticsPresentation.structured_results",
                                  count: values.count, one: "%d structured result.",
                                  other: "%d structured results.")
        }
        return HubL10n.text("hub.HubDiagnosticsPresentation.51.1418", fallback: "Structured report available in raw details.")
    }

    private static func displayTitle(for rawTitle: String, bundle: Bundle) -> String {
        let normalized = rawTitle.split(whereSeparator: \.isWhitespace).joined(separator: " ")
        let prefix = normalized.split(separator: " —", maxSplits: 1, omittingEmptySubsequences: true).first ?? ""
        switch prefix.lowercased() {
        case "doctor": return HubL10n.text("hub.DiagnosticsWindowController.371.242", fallback: "Environment doctor", bundle: bundle)
        case "preflight": return HubL10n.text("hub.HubDiagnosticsPresentation.preflight_label", fallback: "Preflight", bundle: bundle)
        case "status": return HubL10n.text("hub.HubDiagnosticsPresentation.status_label", fallback: "Status", bundle: bundle)
        case "recent logs": return HubL10n.text("hub.DiagnosticsWindowController.377.254", fallback: "Recent logs", bundle: bundle)
        case "service recovery":
            return HubL10n.text("hub.HubDiagnosticsPresentation.service_recovery", fallback: "Service recovery", bundle: bundle)
        case "service pause": return HubL10n.text("hub.HubDiagnosticsPresentation.81.1444", fallback: "Service pause", bundle: bundle)
        case "service state check": return HubL10n.text("hub.HubDiagnosticsPresentation.82.1446", fallback: "Service state check", bundle: bundle)
        case "service resume": return HubL10n.text("hub.HubDiagnosticsPresentation.83.1448", fallback: "Service resume", bundle: bundle)
        case "support metadata": return HubL10n.text("hub.HubDiagnosticsPresentation.84.1450", fallback: "Support metadata", bundle: bundle)
        default:
            guard let first = normalized.first else { return normalized }
            return first.uppercased() + normalized.dropFirst()
        }
    }
}

/// What Status shows about collection: the Hub's own login, its credential state and
/// the collection window, as the running service reports them in `status` and
/// `window status` (docs/guides/collection-paths.md, docs/operations/collection-windows.md).
///
/// Every field is read defensively: a missing, renamed or unknown value is shown as
/// unknown or by its code, never guessed. The app only reads; it holds no token and
/// offers no Fleet action (Fleet is parked).
struct HubCollectionStatus: Equatable {
    /// The Hub's own Tesla login, from `legacyCredentials`.
    enum Login: Equatable {
        case own
        case signedOut
        /// A stored pair the service refuses because it is not the Hub's own login.
        case notOwn(code: String)
        case unknown
    }

    /// The state of that login, from `legacyCredentials`, `credentialStore`,
    /// `collectionAttention` and the window's close reason.
    enum Credential: Equatable {
        case valid(expiresAt: Date?)
        case reloginNeeded
        case ownerAPIRefused
        case refused(code: String)
        case exposureSuspected
        case needsAttention(code: String)
        case notSignedIn
        case unknown
    }

    /// The collection window, from `window status`.
    enum Window: Equatable {
        case open(expiresAt: Date?)
        case pending(startsAt: Date?, expiresAt: Date?)
        case closing(expiresAt: Date?)
        case closed(reason: String?)
        case untrusted
        case faulted
        /// The service waits for `restore-recovery-credentials` (`credentialRecoveryPending`).
        case recoveryPending
        /// The process on the control channel is not the admitted Hub (`service_unauthenticated`).
        case serviceUnauthenticated
        case serviceNotRunning
        case unknown
    }

    /// How the app came by the `window status` answer.
    enum WindowRead {
        case report(String)
        case failed(Error)
        /// Not asked: the service is not loaded, so the command would only be refused.
        case serviceNotLoaded
    }

    var login: Login
    var credential: Credential
    var window: Window
    /// The last sign-in refusal the service gave, when it is one Status should keep
    /// showing (`sign_in_unavailable`).
    var signInRefusal: String?

    /// The `status` fields relied on (listed so a renamed field is easy to find).
    static let statusFields = [
        "legacyCredentials.present", "legacyCredentials.storeState", "legacyCredentials.expiresAt",
        "credentialStore.exposureSuspected", "collectionAttention"
    ]
    /// The `window status` fields relied on; snake_case spellings are accepted too.
    static let windowFields = ["phase", "closeReason", "deadline", "startsAt", "trusted", "faulted",
                               "credentialRecoveryPending"]

    /// Store states that mean the stored pair is not the Hub's own login.
    static let notOwnCodes: Set<String> = ["source_credential_disabled", "unknown_origin", "invalid_origin"]
    /// Store states and attention codes that mean the Hub must sign in again.
    static let reloginCodes: Set<String> = [
        "fresh_sign_in_required", "refresh_token_spent", "relogin_needed",
        "collector_auth_sign_in_required"
    ]

    init(login: Login, credential: Credential, window: Window, signInRefusal: String? = nil) {
        self.login = login
        self.credential = credential
        self.window = window
        self.signInRefusal = signInRefusal
    }

    init(statusOutput: String?, window read: WindowRead, signInRefusal: String? = nil) {
        let root = statusOutput.flatMap(Self.jsonObject)
        let window = Self.window(from: read)
        let closeReason: String?
        if case let .closed(reason) = window { closeReason = reason } else { closeReason = nil }
        let legacy = root?["legacyCredentials"] as? [String: Any]
        let store = root?["credentialStore"] as? [String: Any]
        let attention = root?["collectionAttention"] as? String
        let present = legacy?["present"] as? Bool
        let storeState = legacy?["storeState"] as? String ?? legacy?["store_state"] as? String

        let login: Login
        switch (present, storeState) {
        case (nil, _): login = .unknown
        case (false?, _): login = .signedOut
        case let (true?, code?) where Self.notOwnCodes.contains(code): login = .notOwn(code: code)
        case (true?, _): login = .own
        }

        let credential: Credential
        if root == nil || present == nil {
            credential = .unknown
        } else if attention == "collector_api_refused" || closeReason == "owner_api_refused"
                    || storeState == "owner_api_refused" {
            credential = .ownerAPIRefused
        } else if (store?["exposureSuspected"] as? Bool ?? store?["exposure_suspected"] as? Bool) == true {
            credential = .exposureSuspected
        } else if closeReason == "relogin_needed" || attention.map(Self.reloginCodes.contains) == true
                    || storeState.map(Self.reloginCodes.contains) == true {
            credential = .reloginNeeded
        } else if let storeState, Self.notOwnCodes.contains(storeState) {
            credential = .refused(code: storeState)
        } else if present == true, storeState == "ok" {
            credential = .valid(expiresAt: Self.date(legacy?["expiresAt"] ?? legacy?["expires_at"]))
        } else if present == true {
            credential = .needsAttention(code: storeState ?? "unknown")
        } else {
            credential = .notSignedIn
        }
        self.init(login: login, credential: credential, window: window,
                  signInRefusal: signInRefusal == "sign_in_unavailable" ? signInRefusal : nil)
    }

    // MARK: Reading

    private static func jsonObject(_ output: String) -> [String: Any]? {
        // The whole output first; then the last line that is a JSON object, since a
        // command may print progress lines before its report.
        if let data = output.data(using: .utf8),
           let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            return root
        }
        for line in output.split(whereSeparator: { $0.isNewline }).reversed() {
            if let data = String(line).data(using: .utf8),
               let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                return root
            }
        }
        return nil
    }

    static func window(from read: WindowRead) -> Window {
        switch read {
        case .serviceNotLoaded:
            return .serviceNotRunning
        case let .failed(error):
            let text = error.localizedDescription
            if text.contains("service_not_running") { return .serviceNotRunning }
            if text.contains("service_unauthenticated") { return .serviceUnauthenticated }
            return .unknown
        case let .report(output):
            if output.contains("service_not_running"), jsonObject(output)?["phase"] == nil {
                return .serviceNotRunning
            }
            guard let root = jsonObject(output) else { return .unknown }
            let value = { (camel: String, snake: String) -> Any? in root[camel] ?? root[snake] }
            if value("credentialRecoveryPending", "credential_recovery_pending") as? Bool == true {
                return .recoveryPending
            }
            if value("faulted", "faulted") as? Bool == true { return .faulted }
            let deadline = date(value("deadline", "expires_at") ?? root["expiresAt"])
            switch root["phase"] as? String {
            case "open": return .open(expiresAt: deadline)
            case "pending": return .pending(startsAt: date(value("startsAt", "starts_at")), expiresAt: deadline)
            case "closing": return .closing(expiresAt: deadline)
            case "closed":
                if value("trusted", "trusted") as? Bool == false { return .untrusted }
                return .closed(reason: value("closeReason", "close_reason") as? String)
            default: return .unknown
            }
        }
    }

    /// Unix seconds, or milliseconds for a value too large to be seconds.
    static func date(_ value: Any?) -> Date? {
        guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        let raw = number.doubleValue
        guard raw > 0 else { return nil }
        return Date(timeIntervalSince1970: raw > 100_000_000_000 ? raw / 1000 : raw)
    }

    // MARK: Words

    static func defaultDateText(_ date: Date) -> String {
        DateFormatter.localizedString(from: date, dateStyle: .medium, timeStyle: .short)
    }

    func loginText() -> String {
        switch login {
        case .own: return HubL10n.text("hub.collection.loginOwn", fallback: "Signed in with the Hub's own login")
        case .signedOut: return HubL10n.text("hub.collection.loginSignedOut", fallback: "Signed out")
        case .notOwn: return HubL10n.text("hub.collection.loginNotOwn", fallback: "Stored login is not the Hub's own")
        case .unknown: return HubL10n.text("hub.collection.notReported", fallback: "Not reported")
        }
    }

    func credentialText(date: (Date) -> String = HubCollectionStatus.defaultDateText) -> String {
        switch credential {
        case let .valid(expiresAt?): return HubL10n.format("hub.collection.credValidUntil", fallback: "Valid until %1$@", arguments: [date(expiresAt)])
        case .valid(nil): return HubL10n.text("hub.collection.credValid", fallback: "Valid")
        case .reloginNeeded: return HubL10n.text("hub.collection.credRelogin", fallback: "Re-login needed: sign in again in the next open window")
        case .ownerAPIRefused: return HubL10n.text("hub.collection.credOwnerRefused", fallback: "Owner API refused: Tesla refused this login")
        case let .refused(code): return HubL10n.format("hub.collection.credRefused", fallback: "Refused (%1$@)", arguments: [code])
        case .exposureSuspected: return HubL10n.text("hub.collection.credExposure", fallback: "Exposure suspected: sign in again in the next open window")
        case let .needsAttention(code): return HubL10n.format("hub.collection.credAttention", fallback: "Needs attention (%1$@)", arguments: [code])
        case .notSignedIn: return HubL10n.text("hub.collection.credNotSignedIn", fallback: "Not signed in")
        case .unknown: return HubL10n.text("hub.collection.notReported", fallback: "Not reported")
        }
    }

    func windowText(date: (Date) -> String = HubCollectionStatus.defaultDateText) -> String {
        switch window {
        case let .open(expiresAt?): return HubL10n.format("hub.collection.windowOpenUntil", fallback: "Open until %1$@", arguments: [date(expiresAt)])
        case .open(nil): return HubL10n.text("hub.collection.windowOpen", fallback: "Open")
        case let .pending(startsAt, expiresAt):
            switch (startsAt, expiresAt) {
            case let (start?, end?):
                return HubL10n.format("hub.collection.windowPendingFromUntil", fallback: "Pending from %1$@ until %2$@", arguments: [date(start), date(end)])
            case let (start?, nil):
                return HubL10n.format("hub.collection.windowPendingFrom", fallback: "Pending from %1$@", arguments: [date(start)])
            case let (nil, end?):
                return HubL10n.format("hub.collection.windowPendingUntil", fallback: "Pending until %1$@", arguments: [date(end)])
            case (nil, nil):
                return HubL10n.text("hub.collection.windowPending", fallback: "Pending")
            }
        case let .closing(expiresAt?): return HubL10n.format("hub.collection.windowClosingExpiry", fallback: "Closing (expiry %1$@)", arguments: [date(expiresAt)])
        case .closing(nil): return HubL10n.text("hub.collection.windowClosing", fallback: "Closing")
        case let .closed(reason?): return HubL10n.format("hub.collection.windowClosedReason", fallback: "Closed (%1$@)", arguments: [Self.closeReasonText(reason)])
        case .closed(nil): return HubL10n.text("hub.collection.windowClosed", fallback: "Closed")
        case .untrusted: return HubL10n.text("hub.collection.windowUntrusted", fallback: "Closed (no trusted issuer)")
        case .faulted: return HubL10n.text("hub.collection.windowFaulted", fallback: "Closed (window state needs a new trust document)")
        case .recoveryPending:
            return HubL10n.text("hub.collection.windowRecovery", fallback: "Closed (waiting for credential recovery: run restore-recovery-credentials)")
        case .serviceUnauthenticated: return HubL10n.text("hub.collection.windowUnauthenticated", fallback: "Unknown: the control channel is not this Hub")
        case .serviceNotRunning: return HubL10n.text("hub.collection.windowNotRunning", fallback: "Unknown: Hub is not running")
        case .unknown: return HubL10n.text("hub.collection.notReported", fallback: "Not reported")
        }
    }

    static func closeReasonText(_ code: String) -> String {
        switch code {
        case "expired": return HubL10n.text("hub.collection.reasonExpired", fallback: "expired")
        case "operator_close": return HubL10n.text("hub.collection.reasonOperator", fallback: "closed by the operator")
        case "relogin_needed": return HubL10n.text("hub.collection.reasonRelogin", fallback: "re-login needed")
        case "owner_api_refused": return HubL10n.text("hub.collection.reasonOwnerRefused", fallback: "Owner API refused")
        case "clock_invalid": return HubL10n.text("hub.collection.reasonClock", fallback: "clock check failed")
        case "reboot": return HubL10n.text("hub.collection.reasonReboot", fallback: "the Mac restarted")
        case "state_unverifiable": return HubL10n.text("hub.collection.reasonUnverifiable", fallback: "state could not be verified")
        case "claim_lost": return HubL10n.text("hub.collection.reasonClaimLost", fallback: "collection claim lost")
        default: return code
        }
    }

    static var fleetText: String { HubL10n.text("hub.collection.fleet", fallback: "Parked: no Fleet action is available") }
    static var signInUnavailableText: String { HubL10n.text("hub.collection.signInUnavailable", fallback: "Sign-in unavailable on this Hub") }

    /// The labelled rows Status shows, in order.
    func rows(date: (Date) -> String = HubCollectionStatus.defaultDateText) -> [(label: String, value: String)] {
        var rows: [(label: String, value: String)] = [
            (HubL10n.text("hub.collection.labelLogin", fallback: "Tesla login"), loginText()),
            (HubL10n.text("hub.collection.labelCredential", fallback: "Credential state"), credentialText(date: date)),
            (HubL10n.text("hub.collection.labelWindow", fallback: "Collection window"), windowText(date: date)),
            (HubL10n.text("hub.collection.labelFleet", fallback: "Fleet"), Self.fleetText)
        ]
        if signInRefusal == "sign_in_unavailable" {
            rows.insert((HubL10n.text("hub.collection.labelSignIn", fallback: "Sign-in"), Self.signInUnavailableText), at: 2)
        }
        return rows
    }

    /// The same rows as diagnostic lines.
    func diagnosticLines(date: (Date) -> String = HubCollectionStatus.defaultDateText) -> [String] {
        rows(date: date).map { "\($0.label): \($0.value)" }
    }
}

// SPDX-License-Identifier: AGPL-3.0-only

import CoreFoundation
import Foundation

struct HubSourceCheckResult: Equatable {
    /// Source application release of the reviewed source set this Hub admits;
    /// it must match the `requiredVersion` of a successful source check.
    static let requiredSourceRelease = "4.4.0"
    /// Reason code of a successful check against that reviewed set.
    static let compatibleReasonCode = "v4_4_compatible_schema"

    let compatible: Bool
    let message: String
    let reasonCode: String
    let requiredVersion: String
    var schemaRecognised: Bool? = nil
    var schemaSetLabel: String? = nil

    var mayImport: Bool {
        compatible && schemaRecognised == true && Self.validLabel(schemaSetLabel) != nil
    }

    var recognitionKey: String? {
        switch schemaRecognised {
        case true?: return schemaSetLabel == nil ? nil : "hub.import.recognised"
        case false?: return "hub.import.notRecognised"
        case nil: return nil
        }
    }

    var recognitionTitle: String {
        guard let key = recognitionKey else { return message }
        if let label = schemaSetLabel, schemaRecognised == true {
            return HubL10n.format(key, fallback: "Hub recognises this database (schema set %1$@).", arguments: [label])
        }
        return HubL10n.text(key, fallback: "Hub does not recognise this database's schema, so it cannot import it. The migration guide explains what to check.")
    }

    private static func validLabel(_ value: String?) -> String? {
        guard let value, value.utf8.count == 12, value.range(of: #"^[0-9a-f]{12}$"#, options: .regularExpression) != nil else { return nil }
        return value
    }

    /// A refusal remains diagnostic evidence; a failed process never admits an import.
    /// Failures with pending child quiescence stay errors at the lifecycle boundary.
    static func fromCommandResult(_ result: Result<String, Error>,
                                  historyOnly: Bool) -> HubSourceCheckResult? {
        switch result {
        case let .success(output):
            return parse(output, historyOnly: historyOnly, processSucceeded: true)
        case let .failure(HubActionError.commandExited(_, output)):
            return parse(output, historyOnly: historyOnly, processSucceeded: false)
        case .failure:
            return nil
        }
    }

    static func parse(_ output: String, historyOnly: Bool = false,
                      processSucceeded: Bool = true) -> HubSourceCheckResult? {
        for line in output.split(whereSeparator: \.isNewline) {
            guard let data = String(line).data(using: .utf8),
                  let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let status = root["status"] as? String,
                  let reason = root["reasonCode"] as? String,
                  let required = root["requiredVersion"] as? String,
                  let guidance = root["guidance"] as? String else { continue }
            let failure = status == "incompatible" || status == "unavailable"
            let scopeValid = root["credentialScope"] as? String == "history-only"
                && root["applicationVersionStatus"] as? String == "unknown"
            if historyOnly && !failure && !scopeValid { continue }
            let recognised = (root["sourceSchemaRecognised"] as? NSNumber).flatMap {
                CFGetTypeID($0) == CFBooleanGetTypeID() ? $0.boolValue : nil
            }
            let label = validLabel(root["schemaSetLabel"] as? String)
            return HubSourceCheckResult(
                compatible: processSucceeded && status == "compatible"
                    && reason == compatibleReasonCode && required == requiredSourceRelease
                    && (!historyOnly || scopeValid) && recognised == true && label != nil,
                message: guidance, reasonCode: reason, requiredVersion: required,
                schemaRecognised: recognised, schemaSetLabel: label)
        }
        return nil
    }
}

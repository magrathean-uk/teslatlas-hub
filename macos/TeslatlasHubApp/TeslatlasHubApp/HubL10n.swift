// SPDX-License-Identifier: AGPL-3.0-only

import Foundation

/// Presentation text only. Protocol values, command output, and diagnostic parsers
/// must continue to use their stable, untranslated representations.
enum HubL10n {
    static func text(_ key: String, fallback: String, bundle: Bundle = .main) -> String {
        let localized = bundle.localizedString(forKey: key, value: fallback, table: "Localizable")
        return localized == key ? fallback : localized
    }

    static func format(_ key: String, fallback: String, arguments: [CVarArg],
                       bundle: Bundle = .main, locale: Locale = .current) -> String {
        let localized = text(key, fallback: fallback, bundle: bundle)
        guard !arguments.isEmpty else {
            return formatIsSafe(localized, argumentCount: 0) ? localized : fallback
        }
        let template = formatIsSafe(localized, argumentCount: arguments.count) ? localized : fallback
        return String(format: template, locale: locale, arguments: arguments)
    }

    /// Uses the locale's plural rules from Localizable.stringsdict. The count
    /// remains numeric so Foundation can select one, few, many, or other.
    static func plural(_ key: String, count: Int, one: String, other: String,
                       bundle: Bundle = .main) -> String {
        let localized = text(key, fallback: other, bundle: bundle)
        let missing = localized == other
        let template = missing && count == 1 ? one : localized
        return String(format: template, locale: missing ? Locale(identifier: "en") : locale(for: bundle),
                      arguments: [count])
    }

    static func plural(_ key: String, first: Int, count: Int, one: String, other: String,
                       bundle: Bundle = .main) -> String {
        let localized = text(key, fallback: other, bundle: bundle)
        let missing = localized == other
        let template = missing && count == 1 ? one : localized
        return String(format: template, locale: missing ? Locale(identifier: "en") : locale(for: bundle),
                      arguments: [first, count])
    }

    private static func locale(for bundle: Bundle) -> Locale {
        let language = bundle.preferredLocalizations.first
            ?? bundle.localizations.first
            ?? Locale.current.identifier
        return Locale(identifier: language)
    }

    /// All translated arguments in this catalog are positional object arguments.
    /// Reject a missing, extra, or different conversion before Foundation formats it.
    private static func formatIsSafe(_ template: String, argumentCount: Int) -> Bool {
        guard argumentCount > 0 else { return !template.contains("%") }
        let characters = Array(template)
        var used = Set<Int>()
        var cursor = 0
        while cursor < characters.count {
            guard characters[cursor] == "%" else { cursor += 1; continue }
            cursor += 1
            guard cursor < characters.count else { return false }
            if characters[cursor] == "%" { cursor += 1; continue }
            let numberStart = cursor
            while cursor < characters.count, characters[cursor].isNumber { cursor += 1 }
            guard cursor > numberStart, cursor < characters.count, characters[cursor] == "$" else {
                return false
            }
            guard let index = Int(String(characters[numberStart..<cursor])), index > 0,
                  index <= argumentCount else { return false }
            cursor += 1
            guard cursor < characters.count, characters[cursor] == "@" else { return false }
            used.insert(index)
            cursor += 1
        }
        return used == Set(1...argumentCount)
    }
}

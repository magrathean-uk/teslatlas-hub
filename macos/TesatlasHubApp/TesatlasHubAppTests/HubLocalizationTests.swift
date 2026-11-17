// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Tesatlas_Hub

final class HubLocalizationTests: XCTestCase {
    func testEveryBundledLanguageContainsTheCompleteGUIAndPluralTables() throws {
        let resources = try XCTUnwrap(Bundle.main.resourceURL)
        let languages = Bundle.main.localizations.filter { $0 != "Base" }
        XCTAssertEqual(languages.count, 56)
        let english = resources.appendingPathComponent("en.lproj")
        func table(_ directory: URL, _ name: String) throws -> [String: Any] {
            let data = try Data(contentsOf: directory.appendingPathComponent(name))
            return try XCTUnwrap(PropertyListSerialization.propertyList(
                from: data, format: nil
            ) as? [String: Any])
        }
        let englishKeys = Set(try table(english, "Localizable.strings").keys)
        let pluralKeys = Set(try table(english, "Localizable.stringsdict").keys)
        // M09-T12 adds the Fleet set-up lines in English only: every other language falls back to
        // English for them until they are translated.
        let englishOnly = englishKeys.filter { $0.hasPrefix("hub.fleetSetup.") }
        XCTAssertEqual(englishOnly.count, 114)
        XCTAssertEqual(englishKeys.count, 737 + 114, "M02-T22 adds six first-start lines and drops four, then three window-state lines")
        XCTAssertEqual(pluralKeys.count, 6)
        for language in languages {
            let directory = resources.appendingPathComponent("\(language).lproj")
            let strings = try table(directory, "Localizable.strings")
            let plurals = try table(directory, "Localizable.stringsdict")
            XCTAssertEqual(Set(strings.keys).subtracting(englishOnly), englishKeys.subtracting(englishOnly), language)
            XCTAssertEqual(Set(plurals.keys), pluralKeys, language)
            XCTAssertTrue(strings.values.allSatisfy {
                guard let text = $0 as? String else { return false }
                return !text.isEmpty
            }, language)
            for (key, value) in plurals {
                let entry = try XCTUnwrap(value as? [String: Any])
                let rule = try XCTUnwrap(entry["count"] as? [String: String])
                XCTAssertFalse(try XCTUnwrap(rule["other"]).isEmpty, "\(language): \(key)")
            }
        }
    }

    func testTheUnconfirmedSignInLineIsTranslatedInEveryLocale() throws {
        let resources = try XCTUnwrap(Bundle.main.resourceURL)
        let key = "hub.signIn.unconfirmed"
        func table(_ language: String) throws -> [String: String] {
            let url = resources.appendingPathComponent("\(language).lproj/Localizable.strings")
            let plist = try PropertyListSerialization.propertyList(from: Data(contentsOf: url), format: nil)
            return try XCTUnwrap(plist as? [String: String])
        }
        let english = try XCTUnwrap(try table("en")[key])
        XCTAssertEqual(english, "Hub did not confirm the sign-in in time, but it may have been stored. "
                       + "Check Hub's status before signing in again.")
        let languages = Bundle.main.localizations.filter { $0 != "Base" }
        XCTAssertEqual(languages.count, 56)
        for language in languages {
            let value = try XCTUnwrap(try table(language)[key], language)
            XCTAssertFalse(value.isEmpty, language)
            if language.hasPrefix("en") {
                XCTAssertEqual(value, english, "\(language) follows English in its own spelling")
            } else {
                XCTAssertNotEqual(value, english, "\(language): not English copied in")
            }
        }
    }

    func testTheOwnerSignInFirstLinesAreTranslatedInEveryLocale() throws {
        let resources = try XCTUnwrap(Bundle.main.resourceURL)
        let title = "hub.OnboardingWindowController.752.1958"
        let subtitle = "hub.OnboardingWindowController.606.1928"
        let removed = "hub.OnboardingWindowController.895.1994"
        func table(_ language: String) throws -> [String: String] {
            let url = resources.appendingPathComponent("\(language).lproj/Localizable.strings")
            let plist = try PropertyListSerialization.propertyList(from: Data(contentsOf: url), format: nil)
            return try XCTUnwrap(plist as? [String: String])
        }
        let english = try table("en")
        XCTAssertEqual(english[title], "Owner sign-in")
        XCTAssertFalse(try XCTUnwrap(english[subtitle]).localizedCaseInsensitiveContains("recommended"))
        let languages = Bundle.main.localizations.filter { $0 != "Base" }
        XCTAssertEqual(languages.count, 56)
        for language in languages {
            let strings = try table(language)
            XCTAssertNil(strings[removed], "\(language): the unused v4.2 line is gone")
            for key in [title, subtitle] {
                let value = try XCTUnwrap(strings[key], "\(language): \(key)")
                XCTAssertFalse(value.isEmpty, "\(language): \(key)")
                if !language.hasPrefix("en") {
                    XCTAssertNotEqual(value, english[key], "\(language): \(key) is not English copied in")
                }
            }
        }
    }

    func testTheOwnerSignInNameIsTranslatedInEveryLocale() throws {
        let resources = try XCTUnwrap(Bundle.main.resourceURL)
        let name = "hub.HubController.250.318"
        let card = "hub.OnboardingWindowController.752.1958"
        let menu = "hub.MainWindowController.1153.1841"
        let removal = "hub.MainWindowController.1429.1847"
        let pageTitle = "hub.OnboardingWindowController.592.1921"
        let removed = ["hub.OnboardingWindowController.788.1970",
                       "hub.OnboardingWindowController.1500.2071",
                       "hub.TeslaAuthWindowController.36.2241",
                       "hub.TeslaAuthWindowController.37.2242"]
        func table(_ language: String) throws -> [String: String] {
            let url = resources.appendingPathComponent("\(language).lproj/Localizable.strings")
            let plist = try PropertyListSerialization.propertyList(from: Data(contentsOf: url), format: nil)
            return try XCTUnwrap(plist as? [String: String])
        }
        let english = try table("en")
        XCTAssertEqual(english[name], "Owner sign-in")
        XCTAssertEqual(english[card], "Owner sign-in")
        XCTAssertEqual(english[menu], "Use owner sign-in")
        XCTAssertEqual(english[removal],
                       "Hub will stop and remove its stored Fleet and owner sign-in credentials. Your collected data stays on this Mac.")
        XCTAssertEqual(english[pageTitle], "Sign in with Tesla")
        let languages = Bundle.main.localizations.filter { $0 != "Base" }
        XCTAssertEqual(languages.count, 56)
        for language in languages {
            let strings = try table(language)
            for key in removed { XCTAssertNil(strings[key], "\(language): \(key) is gone") }
            XCTAssertEqual(strings[name], strings[card], "\(language): one name for the provider")
            for key in [name, menu, removal, pageTitle] {
                let value = try XCTUnwrap(strings[key], "\(language): \(key)")
                XCTAssertFalse(value.isEmpty, "\(language): \(key)")
                if !language.hasPrefix("en") {
                    XCTAssertNotEqual(value, english[key], "\(language): \(key) is not English copied in")
                }
                XCTAssertFalse(value.localizedCaseInsensitiveContains("legacy token"), "\(language): \(key)")
            }
        }
    }

    func testTheFreshMacSignInStartLinesAreTranslatedInEveryLocale() throws {
        // M02-T21: the owner sign-in on a fresh Mac shows the setup page, the open-window line,
        // the startup deadline, the handover refusal and one new line, for a removal of an
        // unfinished start that failed (round 2), in every locale with its placeholder.
        let resources = try XCTUnwrap(Bundle.main.resourceURL)
        let expected = [
            "hub.OnboardingWindowController.558.1914": "Setting up Hub…",
            "hub.HubController.1591.587": "Sign-in needs an open collection window. Open a window, then sign in again.",
            "hub.HubController.2678.902": "Hub did not become ready before the startup deadline.",
            "hub.HubController.2457.838": "Finish the source application handover before starting Hub.",
            "hub.signIn.startWithdrawFailed": "Hub did not start, and removing its unfinished start failed: %1$@ Press Connect Tesla to finish removing it."
        ]
        func table(_ language: String) throws -> [String: String] {
            let url = resources.appendingPathComponent("\(language).lproj/Localizable.strings")
            let plist = try PropertyListSerialization.propertyList(from: Data(contentsOf: url), format: nil)
            return try XCTUnwrap(plist as? [String: String])
        }
        let english = try table("en")
        for (key, text) in expected { XCTAssertEqual(english[key], text, key) }
        let languages = Bundle.main.localizations.filter { $0 != "Base" }
        XCTAssertEqual(languages.count, 56)
        for language in languages {
            let strings = try table(language)
            for key in expected.keys {
                let value = try XCTUnwrap(strings[key], "\(language): \(key)")
                XCTAssertFalse(value.isEmpty, "\(language): \(key)")
                if !language.hasPrefix("en") {
                    XCTAssertNotEqual(value, english[key], "\(language): \(key) is not English copied in")
                }
            }
            let withdrawal = try XCTUnwrap(strings["hub.signIn.startWithdrawFailed"])
            XCTAssertEqual(withdrawal.components(separatedBy: "%1$@").count, 2, "\(language): one placeholder")
            XCTAssertEqual(withdrawal.filter { $0 == "%" }.count, 1, "\(language): no other format")
        }
    }

    func testTheFirstStartSignInPageLinesAreTranslatedInEveryLocale() throws {
        // M02-T22 (DEC-125): the idle notice, the Done action, the jargon-free sign-in page and the
        // Fleet line without vehicle commands, each in every locale; the four lines they replace are gone.
        let resources = try XCTUnwrap(Bundle.main.resourceURL)
        let expected = [
            "hub.signIn.idle.title": "Hub is set up and running on this Mac",
            "hub.signIn.idle.detail": "Tesla sign-in opens only during a collection window, and you will be told when one opens. Closing this window leaves Hub running.",
            "hub.signIn.pageSubtitle": "Sign in with Tesla while a collection window is open.",
            "hub.signIn.privacy": "Your Tesla password goes only to Tesla. Hub keeps its own login, encrypted on this Mac.",
            "hub.signIn.fleetDetail": "Tesla's official streaming API, with your own Fleet API app.",
            "hub.signIn.done": "Done",
            // Repair round: a faulted or unreadable window and the close note, each in every locale.
            "hub.signIn.windowFaulted": "Hub's collection window needs a new trust document before it can open, so Tesla sign-in is not available. Nothing was changed.",
            "hub.signIn.windowStatusInvalid": "Hub's answer about its collection window could not be read, so Tesla sign-in is not available. Nothing was changed; try again in a moment.",
            "hub.setup.closeKeepsRunning": "Closing this window leaves Hub running."
        ]
        let replaced = ["hub.OnboardingWindowController.610.1930", "hub.OnboardingWindowController.798.1975",
                        "hub.OnboardingWindowController.749.1957", "hub.OnboardingWindowController.788.1970"]
        func table(_ language: String) throws -> [String: String] {
            let url = resources.appendingPathComponent("\(language).lproj/Localizable.strings")
            let plist = try PropertyListSerialization.propertyList(from: Data(contentsOf: url), format: nil)
            return try XCTUnwrap(plist as? [String: String])
        }
        let english = try table("en")
        for (key, text) in expected { XCTAssertEqual(english[key], text, key) }
        let languages = Bundle.main.localizations.filter { $0 != "Base" }
        XCTAssertEqual(languages.count, 56)
        for language in languages {
            let strings = try table(language)
            for key in replaced { XCTAssertNil(strings[key], "\(language): \(key) is replaced") }
            for key in expected.keys {
                let value = try XCTUnwrap(strings[key], "\(language): \(key)")
                XCTAssertFalse(value.isEmpty, "\(language): \(key)")
                XCTAssertFalse(value.contains("%"), "\(language): \(key) has no format")
                if language.hasPrefix("en") {
                    XCTAssertEqual(value, english[key], "\(language) follows English in its own spelling")
                } else {
                    XCTAssertNotEqual(value, english[key], "\(language): \(key) is not English copied in")
                }
            }
        }
    }

    func testTranslatedStatusTextDoesNotChangeConnectionOrImportDecisions() {
        var snapshot = HubSnapshot.previewRunning
        snapshot.account = "Verbunden"
        snapshot.database = "Datenbank bereit"
        XCTAssertEqual(snapshot.accountState, .connected)
        XCTAssertFalse(snapshot.shouldOfferSourceImport)

        snapshot.account = "Nicht eingerichtet"
        snapshot.accountState = .notConfigured
        snapshot.database = "Wartet auf Einrichtung"
        snapshot.databaseState = .awaitingSetup
        snapshot.health = .stopped
        XCTAssertTrue(snapshot.shouldOfferSourceImport)
    }

    func testTranslatedButtonTitleCannotChangeStartAction() throws {
        var starts = 0
        var stops = 0
        let actions = HubDashboardActions(start: { starts += 1 }, stop: { stops += 1 },
                                          restart: {}, setup: {}, diagnostics: {},
                                          vehicle: HubVehicleCardActions(select: { _ in }, command: { _, _ in }),
                                          serviceDetails: {}, dataFolder: {})
        let dashboard = HubDashboardView(actions: actions)
        var snapshot = HubSnapshot.previewRunning
        snapshot.health = .stopped
        dashboard.apply(snapshot: snapshot, transition: nil, activity: [])
        let startTitle = HubL10n.text("hub.HubDashboardView.119.1362", fallback: "Start Hub")
        let start = try XCTUnwrap(allButtons(in: dashboard).first { $0.title == startTitle })
        start.title = "Hub starten"
        start.performClick(nil)
        XCTAssertEqual(starts, 1)
        XCTAssertEqual(stops, 0)
    }

    func testLocalizedBusyMessageCannotChangeOnboardingOperation() throws {
        let controller = HubController(environment: ["TESATLAS_HUB_UI_PREVIEW": "1"])
        let onboarding = OnboardingWindowController(controller: controller,
                                                    previewRoute: "migration", onComplete: { _ in })
        onboarding.setBusy(true, message: "Verbinden…", operation: .connecting)
        let button = try XCTUnwrap(allButtons(in: onboarding.window?.contentView)
            .first { $0.title == "Verbinden…" })
        XCTAssertFalse(button.isEnabled)
        onboarding.setBusy(false)
    }

    func testMissingAndInvalidFormatsFallBackSafely() throws {
        let folder = HubTestScratch.directory
            .appendingPathComponent("hub-localization-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let english = folder.appendingPathComponent("en.lproj", isDirectory: true)
        try FileManager.default.createDirectory(at: english, withIntermediateDirectories: true)
        try "\"greeting\" = \"Bonjour %2$@, %1$@\";\n\"invalid\" = \"Bad %2$@\";\n\"plain\" = \"Prêt\";\n\"unexpected\" = \"Bad %@\";\n"
            .write(to: english.appendingPathComponent("Localizable.strings"),
                   atomically: true, encoding: .utf8)
        let bundle = try XCTUnwrap(Bundle(url: folder))
        XCTAssertEqual(HubL10n.text("absent", fallback: "English", bundle: bundle), "English")
        XCTAssertEqual(HubL10n.format("greeting", fallback: "Hello %1$@, %2$@",
                                       arguments: ["Alice", "Bob"], bundle: bundle), "Bonjour Bob, Alice")
        XCTAssertEqual(HubL10n.format("invalid", fallback: "Hello %1$@",
                                       arguments: ["Alice"], bundle: bundle), "Hello Alice")
        XCTAssertEqual(HubL10n.format("plain", fallback: "Ready", arguments: [], bundle: bundle), "Prêt")
        XCTAssertEqual(HubL10n.format("unexpected", fallback: "100% ready", arguments: [], bundle: bundle),
                       "100% ready")
        XCTAssertEqual(HubL10n.format("missing", fallback: "100% ready", arguments: [], bundle: bundle),
                       "100% ready")
    }

    func testEnglishPluralResourcesSelectByNumericCount() throws {
        let resources = try XCTUnwrap(Bundle.main.resourceURL)
        let bundle = try XCTUnwrap(Bundle(url: resources.appendingPathComponent("en.lproj")))
        XCTAssertEqual(HubL10n.plural("hub.HubController.3748.1243", count: 1,
                                      one: "%d vehicle", other: "%d vehicles", bundle: bundle), "1 vehicle")
        XCTAssertEqual(HubL10n.plural("hub.HubController.3748.1243", count: 3,
                                      one: "%d vehicle", other: "%d vehicles", bundle: bundle), "3 vehicles")
        XCTAssertEqual(HubL10n.plural("hub.DiagnosticsWindowController.182.223", first: 1,
                                      count: 1, one: "%1$d of %2$d check passed.",
                                      other: "%1$d of %2$d checks passed.", bundle: bundle),
                       "1 of 1 check passed.")
    }

    func testPluralUsesBundleLocaleAndDenominatorWhenArgumentsAreReordered() throws {
        let folder = HubTestScratch.directory
            .appendingPathComponent("hub-plurals-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let french = folder.appendingPathComponent("fr.lproj", isDirectory: true)
        try FileManager.default.createDirectory(at: french, withIntermediateDirectories: true)
        let plural: [String: Any] = [
            "vehicles": [
                "NSStringLocalizedFormatKey": "%#@value@",
                "value": ["NSStringFormatSpecTypeKey": "NSStringPluralRuleType",
                          "NSStringFormatValueTypeKey": "d",
                          "one": "%d véhicule", "other": "%d véhicules"]
            ],
            "checks": [
                "NSStringLocalizedFormatKey": "%2$#@value@",
                "value": ["NSStringFormatSpecTypeKey": "NSStringPluralRuleType",
                          "NSStringFormatValueTypeKey": "d",
                          "one": "%2$d vérification, %1$d validée",
                          "other": "%2$d vérifications, %1$d validées"]
            ]
        ]
        let data = try PropertyListSerialization.data(fromPropertyList: plural,
                                                      format: .xml, options: 0)
        try data.write(to: french.appendingPathComponent("Localizable.stringsdict"))
        let bundle = try XCTUnwrap(Bundle(url: folder))
        XCTAssertEqual(HubL10n.plural("vehicles", count: 0, one: "%d vehicle",
                                      other: "%d vehicles", bundle: bundle), "0 véhicule")
        XCTAssertEqual(HubL10n.plural("vehicles", count: 3, one: "%d vehicle",
                                      other: "%d vehicles", bundle: bundle), "3 véhicules")
        XCTAssertEqual(HubL10n.plural("checks", first: 0, count: 0,
                                      one: "%1$d of %2$d check passed.",
                                      other: "%1$d of %2$d checks passed.", bundle: bundle),
                       "0 vérification, 0 validée")
        XCTAssertEqual(HubL10n.plural("checks", first: 1, count: 2,
                                      one: "%1$d of %2$d check passed.",
                                      other: "%1$d of %2$d checks passed.", bundle: bundle),
                       "2 vérifications, 1 validées")
    }

    private func allButtons(in view: NSView?) -> [NSButton] {
        guard let view else { return [] }
        let current = (view as? NSButton).map { [$0] } ?? []
        return current + view.subviews.flatMap(allButtons(in:))
    }
}

// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Teslatlas_Hub

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
        XCTAssertEqual(englishKeys.count, 730)
        XCTAssertEqual(pluralKeys.count, 6)
        for language in languages {
            let directory = resources.appendingPathComponent("\(language).lproj")
            let strings = try table(directory, "Localizable.strings")
            let plurals = try table(directory, "Localizable.stringsdict")
            XCTAssertEqual(Set(strings.keys), englishKeys, language)
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
        let signIn = "hub.OnboardingWindowController.788.1970"
        let removed = ["hub.OnboardingWindowController.1500.2071",
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
        XCTAssertEqual(english[signIn], "Sign in with Tesla")
        let languages = Bundle.main.localizations.filter { $0 != "Base" }
        XCTAssertEqual(languages.count, 56)
        for language in languages {
            let strings = try table(language)
            for key in removed { XCTAssertNil(strings[key], "\(language): \(key) is gone") }
            XCTAssertEqual(strings[pageTitle], strings[signIn], "\(language): the page is titled as its button")
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
        let controller = HubController(environment: ["TESLATLAS_HUB_UI_PREVIEW": "1"])
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

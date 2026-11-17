// SPDX-License-Identifier: AGPL-3.0-only

import AppKit
import XCTest
@testable import Tesatlas_Hub

/// M09-T12: the Mac app's Fleet onboarding against the repaired service's contract
/// (`.plan/work/M09-T19a/interface.md`, sections 1 to 7, 10 and 11), through a fake runner:
/// prerequisites, details, public key, registration, the Tesla sign-in with its binding and
/// `state` check, finding the car, the consent page and only then `setup-fleet activate`.
/// No Hub binary, Tesla page, network, Keychain or pasteboard is used; no window is shown.
final class FleetSignInFlowTests: XCTestCase {
    private static let secret = "fictional-fleet-secret/7Qx-91"
    private static let clientID = "c0ffee00-1111-4222-8333-444455556666"
    private static let redirect = "https://example.com/tesla/callback"
    private static let domain = "example.com"
    private static let code = "EU_fictional_code_5b2"

    override func setUp() {
        super.setUp()
        HubFleetOnboarding.isOffered = true
    }

    override func tearDown() {
        HubFleetOnboarding.isOffered = HubFleetOnboarding.offeredByDefault
        super.tearDown()
    }

    // MARK: The first set-up, end to end

    func testAFirstSetUpRunsTheContractInOrderAndPollsNothingBeforeConsent() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        let window = fixture.window

        // Prerequisites: Continue needs every item ticked.
        XCTAssertEqual(window.fleetStepForTests, .prerequisites)
        XCTAssertFalse(window.continueEnabledForTests)
        window.tickFleetPrerequisitesForTests()
        XCTAssertTrue(window.continueEnabledForTests)
        window.continueForTests()
        XCTAssertEqual(window.fleetStepForTests, .details)

        // Details: the Hub becomes a Fleet Hub with collection off, and runs.
        window.enterFleetDetailsForTests(clientID: Self.clientID, region: .eu,
                                         redirectURI: Self.redirect, domain: Self.domain)
        window.continueForTests()
        try fixture.waitUntil { window.fleetStepForTests == .publicKey && !window.isBusyForTests }
        XCTAssertTrue(fixture.configText.contains("provider = \"fleet\""))
        XCTAssertTrue(fixture.configText.contains("interval_seconds = 0"))
        XCTAssertEqual(fixture.service.calls, ["service start"])
        XCTAssertEqual(fixture.controller.savedFleetApplication,
                       HubFleetApplication(clientID: Self.clientID, region: .eu, redirectURI: Self.redirect,
                                           domain: Self.domain))

        // Public key: saved where the owner chose, with nothing sent to Tesla.
        let keyPath = fixture.home.appendingPathComponent("Desktop/com.tesla.3p.public-key.pem")
        window.presentFleetKeySavePanel = { panel, _, done in
            XCTAssertEqual(panel.nameFieldStringValue, "com.tesla.3p.public-key.pem")
            done(keyPath)
        }
        window.saveFleetPublicKeyForTests()
        try fixture.waitUntil { window.fleetPublicKeyForTests != nil }
        XCTAssertEqual(fixture.runner.call("public-key").map { Array($0.arguments.suffix(4)) },
                       ["setup-fleet", "public-key", "--output", keyPath.path])
        XCTAssertTrue(fixture.labels().contains {
            $0.contains("https://example.com/.well-known/appspecific/com.tesla.3p.public-key.pem")
        })

        // Registration: the secret goes on standard input only and the field is cleared.
        XCTAssertFalse(window.continueEnabledForTests, "registration needs the client secret")
        window.enterFleetSecretForTests(Self.secret)
        XCTAssertEqual(window.continueTitleForTests, "Register Domain")
        window.continueForTests()
        XCTAssertEqual(window.fleetSecretForTests, "", "cleared as the call starts")
        try fixture.waitUntil { window.fleetStepForTests == .signIn && !window.isBusyForTests }
        let register = try XCTUnwrap(fixture.runner.call("register"))
        XCTAssertTrue(register.sensitive)
        XCTAssertEqual(try Self.object(register.stdin), [
            "clientId": Self.clientID, "clientSecret": Self.secret, "domain": Self.domain, "region": "eu"
        ])
        XCTAssertEqual(fixture.controller.savedFleetApplication?.registered, true)

        // Sign in: begin, the browser leg on the registered redirect URI, then the redeem.
        var browser: (flow: FleetOAuthFlow, done: (Result<TeslaAuthCodeGrant, Error>) -> Void)?
        window.presentFleetSignIn = { flow, done in browser = (flow, done) }
        XCTAssertFalse(window.continueEnabledForTests, "the sign-in needs the client secret again")
        window.enterFleetSecretForTests(Self.secret)
        XCTAssertEqual(window.continueTitleForTests, "Sign in with Tesla")
        window.continueForTests()
        try fixture.waitUntil { browser != nil }
        let begin = try XCTUnwrap(fixture.runner.call("begin"))
        XCTAssertFalse(begin.sensitive)
        XCTAssertEqual(try Self.object(begin.stdin), [
            "clientId": Self.clientID, "redirectUri": Self.redirect, "region": "eu"
        ])
        XCTAssertNil(fixture.runner.call("redeem"), "nothing is redeemed before the callback")
        let flow = try XCTUnwrap(browser?.flow)
        XCTAssertEqual(flow.redirectURI, Self.redirect)
        XCTAssertTrue(flow.isCallback(URL(string: Self.redirect + "?code=x&state=y")!))
        browser?.done(Self.grant(flow, url: "\(Self.redirect)?code=\(Self.code)&state=\(flow.state)&issuer=https%3A%2F%2Fauth.tesla.com%2Foauth2%2Fv3"))
        XCTAssertEqual(window.fleetSecretForTests, "", "cleared as the redeem starts")
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests }
        let redeem = try XCTUnwrap(fixture.runner.call("redeem"))
        XCTAssertTrue(redeem.sensitive)
        XCTAssertEqual(Array(redeem.arguments.suffix(2)), ["setup-fleet", "--all-vehicles"])
        XCTAssertEqual(try Self.object(redeem.stdin), [
            "code": Self.code, "redirectUri": Self.redirect, "clientId": Self.clientID,
            "clientSecret": Self.secret, "region": "eu", "binding": fixture.runner.binding
        ])

        // Consent: nothing is polled until the owner accepts.
        XCTAssertNil(fixture.runner.call("activate"))
        XCTAssertTrue(fixture.configText.contains("interval_seconds = 0"))
        XCTAssertTrue(fixture.view(identifier: "onboarding.fleet.consent") != nil)
        let shown = fixture.labels().joined(separator: "\n")
        XCTAssertTrue(shown.contains("every 30 seconds"), "the cadence is shown")
        XCTAssertTrue(shown.contains("$0.002"), "the price is shown")
        XCTAssertTrue(shown.contains("$10.00"), "the monthly cap is shown")
        XCTAssertEqual(window.continueTitleForTests, "Start Collecting")
        XCTAssertFalse(window.continueEnabledForTests)
        window.continueForTests()
        XCTAssertNil(fixture.runner.call("activate"), "Start Collecting does nothing without consent")
        window.acceptFleetConsentForTests()
        XCTAssertTrue(window.continueEnabledForTests)
        window.continueForTests()
        try fixture.waitUntil { window.currentRoute == .verify }
        XCTAssertNotNil(fixture.runner.call("activate"))

        // The order of section 10, and the secret only where it belongs.
        XCTAssertEqual(fixture.runner.fleetVerbs, ["public-key", "register", "begin", "redeem", "activate"])
        for call in fixture.runner.calls {
            XCTAssertFalse(call.arguments.joined(separator: " ").contains(Self.secret), "never an argument")
            if let stdin = call.stdin, stdin.contains(Self.secret) {
                XCTAssertTrue(call.sensitive, "the secret travels only as sensitive input: \(call.arguments)")
                XCTAssertFalse(stdin.replacingOccurrences(of: "\\/", with: "").contains("\\"),
                               "no escape but \\/ reaches the service")
            }
        }
        XCTAssertFalse(fixture.filesContain(Self.secret), "the client secret is never written to disk")
        XCTAssertFalse(fixture.filesContain(Self.code), "the code is never written to disk")
        XCTAssertFalse(UserDefaults.standard.dictionaryRepresentation().values
            .contains { "\($0)".contains(Self.secret) }, "nor to a preference")
    }

    // MARK: The browser leg

    func testACallbackWithAnotherStateIsRefusedBeforeTheRedeem() throws {
        let fixture = try Fixture(state: .awaitingSignIn)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .signIn }
        var browser: (flow: FleetOAuthFlow, done: (Result<TeslaAuthCodeGrant, Error>) -> Void)?
        window.presentFleetSignIn = { flow, done in browser = (flow, done) }
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        try fixture.waitUntil { browser != nil }
        let flow = try XCTUnwrap(browser?.flow)
        browser?.done(Self.grant(flow, url: "\(Self.redirect)?code=\(Self.code)&state=abandoned-attempt-state-000000000000"))
        XCTAssertNil(fixture.runner.call("redeem"))
        XCTAssertEqual(window.errorMessageForTests, TeslaAuthError.stateMismatch.localizedDescription)
        XCTAssertEqual(window.fleetStepForTests, .signIn)
    }

    func testTheCallbackIsTheRegisteredURLUpToItsDefaultPortAndRootPathAndNothingElse() throws {
        let flow = try Self.flow()
        // The same URL under RFC 3986 section 6.2: case of scheme and host, the scheme's default port.
        for accepted in ["\(Self.redirect)?code=1&state=2", "HTTPS://Example.COM/tesla/callback?code=1",
                         "https://example.com:443/tesla/callback"] {
            XCTAssertTrue(flow.isCallback(URL(string: accepted)!), accepted)
        }
        for refused in ["https://example.com/tesla/callback/more?code=1", "https://example.com/tesla?code=1",
                        "http://example.com/tesla/callback?code=1", "https://www.example.com/tesla/callback",
                        "https://example.com:8443/tesla/callback", "tesla://auth/callback?code=1",
                        "https://user@example.com/tesla/callback",
                        "https://user:password@example.com/tesla/callback?code=1",
                        "https://example.com:80/tesla/callback", "https://example.org/tesla/callback",
                        "https://example.com.evil.test/tesla/callback", "https://example.com/tesla/callback/"] {
            XCTAssertFalse(flow.isCallback(URL(string: refused)!), refused)
        }
        // A registered address without a path is the same URL as its root path, as WebKit reports it.
        XCTAssertTrue(FleetOAuthFlow.matches(URL(string: "https://example.com/?code=1")!, redirectURI: "https://example.com"))
        XCTAssertTrue(FleetOAuthFlow.matches(URL(string: "https://example.com?code=1")!, redirectURI: "https://example.com/"))
        XCTAssertTrue(FleetOAuthFlow.matches(URL(string: "http://example.com:80/cb")!, redirectURI: "http://example.com/cb"))
        XCTAssertTrue(FleetOAuthFlow.matches(URL(string: "https://example.com/cb")!, redirectURI: "https://example.com:443/cb"))
        // Every other address is refused, each difference on its own.
        for (refused, registered) in [
            ("https://example.com:8443/cb", "https://example.com/cb"),     // another port
            ("https://example.com/cb", "https://example.com:8443/cb"),     // the registered port is not the default
            ("http://example.com/cb", "https://example.com/cb"),           // another scheme
            ("http://example.com:443/cb", "https://example.com/cb"),       // another scheme on the same port
            ("https://example.net/cb", "https://example.com/cb"),          // another host
            ("https://hub.example.com/cb", "https://example.com/cb"),      // a subdomain
            ("https://owner@example.com/cb", "https://example.com/cb"),    // user information
            ("https://example.com/cb/next", "https://example.com/cb"),     // a longer path
            ("https://example.com/cb/", "https://example.com/cb"),         // `/cb/` is not `/cb`
            ("https://example.com/cb", "https://example.com/cb/"),
            ("https://example.com/CB", "https://example.com/cb"),          // the path keeps its case
            ("https://example.com/c%62", "https://example.com/cb")         // and its encoding
        ] {
            XCTAssertFalse(FleetOAuthFlow.matches(URL(string: refused)!, redirectURI: registered), "\(refused) for \(registered)")
        }
        // The owner sign-in keeps its own callback, and does not take the Fleet one.
        let owner = try TeslaOAuthFlow()
        XCTAssertTrue(owner.isCallback(URL(string: "tesla://auth/callback?code=1")!))
        XCTAssertFalse(owner.isCallback(URL(string: Self.redirect)!))
        XCTAssertEqual(TeslaOAuthFlow.redirectURI, "tesla://auth/callback")

        XCTAssertEqual(try flow.callback(URL(string: "\(Self.redirect)?error=login_cancelled")!), .cancelled)
        XCTAssertThrowsError(try flow.callback(URL(string: "\(Self.redirect)?error=access_denied")!)) {
            XCTAssertEqual(($0 as? HubFleetRefusal)?.code, "fleet_access_denied")
        }
        XCTAssertThrowsError(try flow.callback(URL(string: "\(Self.redirect)?code=1&state=other-state-value-0000")!)) {
            XCTAssertEqual($0 as? TeslaAuthError, .stateMismatch)
        }
        XCTAssertEqual(try flow.callback(URL(string: "\(Self.redirect)?code=abc&state=\(flow.state)")!),
                       .code(TeslaAuthCodeGrant(code: "abc", codeVerifier: "", redirectURI: Self.redirect)))

        // The window takes the Fleet flow without loading anything.
        let auth = try TeslaAuthWindowController(loadAuthorizationURL: false, flow: flow) { _ in }
        auth.window?.orderOut(nil)
    }

    func testTheAuthoriseAddressMustBeTeslasAndNameThisApplication() throws {
        func address(host: String = "auth.tesla.com", scheme: String = "https",
                     client: String = FleetSignInFlowTests.clientID,
                     redirect: String = FleetSignInFlowTests.redirect) -> URL {
            var components = URLComponents(string: "\(scheme)://\(host)/oauth2/v3/authorize")!
            components.queryItems = [URLQueryItem(name: "response_type", value: "code"),
                                     URLQueryItem(name: "client_id", value: client),
                                     URLQueryItem(name: "redirect_uri", value: redirect),
                                     URLQueryItem(name: "state", value: String(repeating: "s", count: 43))]
            return components.url!
        }
        XCTAssertNoThrow(try FleetOAuthFlow(authorizationURL: address(), redirectURI: Self.redirect, clientID: Self.clientID))
        XCTAssertNoThrow(try FleetOAuthFlow(authorizationURL: address(host: "auth.tesla.cn"),
                                            redirectURI: Self.redirect, clientID: Self.clientID))
        for bad in [address(host: "auth.example.com"), address(scheme: "http"), address(client: "another"),
                    address(redirect: "https://example.com/other")] {
            XCTAssertThrowsError(try FleetOAuthFlow(authorizationURL: bad, redirectURI: Self.redirect,
                                                    clientID: Self.clientID), bad.absoluteString)
        }
    }

    // MARK: Waiting states, also after a quit

    func testEachWaitingStateShowsItsNextStepAndContinuesAfterAQuit() throws {
        let signIn = try Fixture(state: .awaitingSignIn)
        try signIn.waitUntil { signIn.window.fleetStepForTests == .signIn }
        XCTAssertEqual(signIn.window.fleetNoticeForTests,
                       "Hub is waiting for your Tesla sign-in. Sign in with Tesla to continue.")
        XCTAssertEqual(signIn.window.continueTitleForTests, "Sign in with Tesla")
        signIn.tearDown()

        let vehicles = try Fixture(state: .awaitingVehicles)
        try vehicles.waitUntil { vehicles.window.fleetStepForTests == .vehicles }
        XCTAssertEqual(vehicles.window.fleetNoticeForTests,
                       "You are signed in, and Hub has not found your car yet. Find your car to continue.")
        XCTAssertEqual(vehicles.window.continueTitleForTests, "Find My Car")
        vehicles.window.continueForTests()
        try vehicles.waitUntil { vehicles.window.fleetStepForTests == .consent && !vehicles.window.isBusyForTests }
        XCTAssertEqual(vehicles.runner.call("discover").map { Array($0.arguments.suffix(3)) }, ["setup-fleet", "discover", "--all-vehicles"])
        XCTAssertNil(vehicles.runner.call("redeem"), "no new sign-in")
        XCTAssertNil(vehicles.runner.call("activate"))
        vehicles.tearDown()

        let activation = try Fixture(state: .awaitingActivation)
        defer { activation.tearDown() }
        try activation.waitUntil { activation.window.fleetStepForTests == .consent }
        XCTAssertEqual(activation.window.fleetNoticeForTests,
                       "You are signed in and your car is found. Hub polls nothing until you accept the terms below.")
        XCTAssertNil(activation.runner.call("activate"))
        activation.window.acceptFleetConsentForTests()
        activation.window.continueForTests()
        try activation.waitUntil { activation.window.currentRoute == .verify }
        XCTAssertEqual(activation.runner.fleetVerbs, ["activate"])
    }

    func testPreflightStatesAreReadByTheServiceFields() {
        func state(_ json: String, fleet: Bool = true) -> HubFleetSetupState? {
            HubFleetSetupState.parse(json, configuredFleet: fleet)
        }
        XCTAssertEqual(state(#"{"status":"ready","state":"awaiting_sign_in","version":"1","provider":"fleet","collectorEnabled":false}"#), .awaitingSignIn)
        XCTAssertEqual(state(#"{"status":"ready","state":"awaiting_vehicles","provider":"fleet","collectorEnabled":false}"#), .awaitingVehicles)
        XCTAssertEqual(state(#"{"status":"ready","state":"awaiting_activation","provider":"fleet","collectorEnabled":false}"#), .awaitingActivation)
        XCTAssertEqual(state(#"{"status":"ready","state":"idle","version":"1"}"#), .idle)
        XCTAssertEqual(state(#"{"status":"ready","state":"idle","version":"1"}"#, fleet: false), .notFleet)
        XCTAssertEqual(state(#"{"status":"ready","version":"1","provider":"fleet"}"#), .collecting)
        XCTAssertEqual(state(#"{"status":"ready","state":"awaiting_sign_in","provider":"legacy","collectorEnabled":false}"#), .notFleet)
        XCTAssertNil(state("Fleet credentials are unusable: the key store is missing"))
        // An unknown or malformed state is that, with the service's text: never "not Fleet" or collecting.
        XCTAssertEqual(state(#"{"status":"ready","state":"awaiting_payment","provider":"fleet","message":"Fixed service text."}"#),
                       .unknown(state: "awaiting_payment", serviceText: "Fixed service text."))
        XCTAssertEqual(state(#"{"status":"ready","state":"awaiting_payment"}"#, fleet: false),
                       .unknown(state: "awaiting_payment", serviceText: #"{"status":"ready","state":"awaiting_payment"}"#))
        XCTAssertEqual(state(#"{"status":"ready","state":7,"provider":"fleet"}"#),
                       .unknown(state: "7", serviceText: #"{"status":"ready","state":7,"provider":"fleet"}"#))
        XCTAssertEqual(state(#"{"status":"ready","state":null,"provider":"fleet"}"#),
                       .unknown(state: "null", serviceText: #"{"status":"ready","state":null,"provider":"fleet"}"#))
    }

    func testAnUnknownStateIsShownWithTheServicesTextAndOffersNoNextStep() throws {
        let fixture = try Fixture(state: .awaitingSignIn)
        defer { fixture.tearDown() }
        fixture.runner.preflight = #"{"status":"ready","state":"awaiting_payment","provider":"fleet","message":"Fixed text for a newer state."}"#
        let window = fixture.window
        try fixture.waitUntil { window.fleetStateUnknownForTests != nil && !window.isBusyForTests }
        XCTAssertEqual(window.fleetStateUnknownForTests,
                       "Hub reports a set-up state this version of the app does not know (awaiting_payment), so it offers no next step. Hub's answer: Fixed text for a newer state.")
        XCTAssertNotNil(fixture.view(identifier: "onboarding.fleet.state-unknown"))
        XCTAssertNil(window.fleetNoticeForTests)
        XCTAssertFalse(window.continueEnabledForTests, "no next step")
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        XCTAssertEqual(fixture.runner.fleetVerbs, [], "nothing is sent from an unknown state")
    }

    // MARK: The figures in effect (note 3)

    func testTheConsentPageStatesTheDefaultFiguresWhenNothingOverridesThem() throws {
        let fixture = try Fixture(state: .awaitingActivation)
        defer { fixture.tearDown() }
        try fixture.waitUntil { fixture.window.fleetStepForTests == .consent && !fixture.window.isBusyForTests }
        let shown = fixture.labels().joined(separator: "\n")
        for figure in ["a driving car every 30 seconds", "a charging car every 2 minutes", "an awake car every 2 minutes",
                       "a car that is updating every 5 minutes", "checks a sleeping car every 10 minutes",
                       "lists your cars every hour", "$1 for every 500 vehicle-data requests ($0.002 each)",
                       "A state check or a list of your cars is billed at the same price.",
                       "when its requests reach $10.00", "around day 14 of a month"] {
            XCTAssertTrue(shown.contains(figure), figure)
        }
    }

    func testTheConsentPageStatesTheCapAndPollingOverridesOfThisHub() throws {
        let fixture = try Fixture(state: .awaitingActivation, config: """

            [collector.fleet_polling]
            driving_poll_seconds = 60   # once a minute
            updating_poll_seconds = 600
            state_poll_seconds = 900
            monthly_cap_cents = 2000

            """)
        defer { fixture.tearDown() }
        try fixture.waitUntil { fixture.window.fleetStepForTests == .consent && !fixture.window.isBusyForTests }
        let shown = fixture.labels().joined(separator: "\n")
        for figure in ["a driving car every minute", "a charging car every 2 minutes",
                       "a car that is updating every 10 minutes", "checks a sleeping car every 15 minutes",
                       "lists your cars every hour", "when its requests reach $20.00"] {
            XCTAssertTrue(shown.contains(figure), figure)
        }
        XCTAssertFalse(shown.contains("$10.00"), "not the default cap")
        XCTAssertFalse(shown.contains("day 14"), "the default's day is not claimed for other figures")
        XCTAssertNotNil(fixture.view(identifier: "onboarding.fleet.consent-accept"))
    }

    func testAFigureThatCannotBeReadExactlyIsNamedAndNothingCanBeAccepted() throws {
        let fixture = try Fixture(state: .awaitingActivation, config: "\n[collector.fleet_polling]\nmonthly_cap_cents = 2_000\n")
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests }
        let shown = fixture.labels().joined(separator: "\n")
        XCTAssertTrue(shown.contains("Hub cannot read the setting collector.fleet_polling.monthly_cap_cents"), shown)
        XCTAssertFalse(shown.contains("$10.00") || shown.contains("$20.00"), "no figure is stated")
        XCTAssertNil(fixture.view(identifier: "onboarding.fleet.consent-accept"), "no acceptance is offered")
        window.acceptFleetConsentForTests()
        XCTAssertFalse(window.continueEnabledForTests)
        window.continueForTests()
        XCTAssertNil(fixture.runner.call("activate"))
    }

    func testTheFiguresAreReadExactlyOrTheSettingIsNamed() {
        func reading(_ table: String) -> HubFleetConsentReading {
            HubFleetConsentFigures.reading(in: "data_dir = \"/tmp/hub\"\n\n[collector]\nprovider = \"fleet\"\n" + table)
        }
        XCTAssertEqual(HubFleetConsentFigures.reading(in: nil), .figures(.serviceDefaults))
        XCTAssertEqual(reading(""), .figures(.serviceDefaults))
        var expected = HubFleetConsentFigures.serviceDefaults
        expected.chargingSeconds = 300
        expected.onlineSeconds = 240
        expected.inventorySeconds = 7_200
        expected.monthlyCapCents = 0
        XCTAssertEqual(reading("\n[collector.fleet_polling]\ncharging_poll_seconds = 300\nonline_poll_seconds=240\n"
                               + "inventory_poll_seconds = 7200 # two hours\nmonthly_cap_cents = 0\n\n[geocoder]\nenabled = false\n"),
                       .figures(expected))
        let table = "collector.fleet_polling"
        for (text, setting) in [
            ("\n[collector.fleet_polling]\nmonthly_cap_cents = 2_000\n", "\(table).monthly_cap_cents"),
            ("\n[collector.fleet_polling]\nmonthly_cap_cents = 0x10\n", "\(table).monthly_cap_cents"),
            ("\n[collector.fleet_polling]\ndriving_poll_seconds = 030\n", "\(table).driving_poll_seconds"),
            ("\n[collector.fleet_polling]\ndriving_poll_seconds = 0\n", "\(table).driving_poll_seconds"),
            ("\n[collector.fleet_polling]\ndriving_poll_seconds = -5\n", "\(table).driving_poll_seconds"),
            ("\n[collector.fleet_polling]\ndriving_poll_seconds = 1e2\n", "\(table).driving_poll_seconds"),
            ("\n[collector.fleet_polling]\nwake_poll_seconds = 60\n", "\(table).wake_poll_seconds"),
            ("\n[collector.fleet_polling]\ncharging_poll_seconds = 60\ncharging_poll_seconds = 90\n", "\(table).charging_poll_seconds"),
            ("\n[collector.fleet_polling]\n\"monthly_cap_cents\" = 100\n", "\(table).\"monthly_cap_cents\""),
            ("\n[ collector.fleet_polling ]\nmonthly_cap_cents = 100\n", table),
            ("\n[collector.fleet_polling]\n[collector.fleet_polling]\n", table),
            ("fleet_polling.monthly_cap_cents = 100\n", table),
            ("fleet_polling = { monthly_cap_cents = 100 }\n", table),
            ("\n[collector.fleet_polling]\nmonthly_cap_cents\n", table)
        ] {
            XCTAssertEqual(reading(text), .unreadable(setting: setting), text)
        }
    }

    // MARK: Refusals

    func testAnUnknownRefusalIsShownWithTheServicesOwnMessageAndAKnownOneWithTheAppsLine() throws {
        let fixture = try Fixture(state: .awaitingSignIn)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .signIn }
        window.presentFleetSignIn = { _, _ in XCTFail("no browser after a refused begin") }

        fixture.runner.results["begin"] = [.failure(HubActionError.commandExited(
            1, "fleet_future_refusal: Fixed text the service wrote for a code this app does not know."))]
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        try fixture.waitUntil { !window.isBusyForTests && window.errorMessageForTests != nil }
        XCTAssertEqual(window.errorMessageForTests, "Fixed text the service wrote for a code this app does not know.")

        fixture.runner.results["begin"] = [.failure(HubActionError.commandExited(
            1, "window_closed: sign-in and vehicle discovery need an open collection window"))]
        window.continueForTests()
        try fixture.waitUntil { !window.isBusyForTests && window.errorMessageForTests?.hasPrefix("This step") == true }
        XCTAssertEqual(window.errorMessageForTests,
                       "This step talks to Tesla, so it needs an open collection window. Try again while a window is open.")
        XCTAssertNil(fixture.runner.call("redeem"))
    }

    func testRefusalCodesAreReadFromTheServiceLine() {
        func message(_ text: String) -> String {
            HubFleetRefusal.from(HubActionError.commandExited(1, text)).localizedDescription
        }
        XCTAssertEqual(message("sign_in_binding_invalid: the binding is not the newest attempt"),
                       "This Tesla sign-in is no longer the current one. Sign in with Tesla again.")
        XCTAssertEqual(message("fleet_scope_missing_vehicle_location: the grant lacks vehicle_location"),
                       "Tesla did not grant vehicle information and vehicle location. Sign in with Tesla again and allow both on Tesla's consent page.")
        for known in ["1xx", "2xx", "3xx", "4xx", "5xx"] {
            XCTAssertEqual(message("fleet_partner_response_\(known): Tesla's answer was not understood"),
                           "Tesla's answer to the registration was not understood. Try again later.", known)
        }
        // Only the five codes the contract names are known: any other keeps the service's message.
        for unknown in ["fleet_partner_response_future", "fleet_partner_response_6xx", "fleet_partner_response_"] {
            XCTAssertEqual(message("\(unknown): Fixed text the service wrote for a newer code."),
                           "Fixed text the service wrote for a newer code.", unknown)
        }
        XCTAssertEqual(message("keyring_unsafe: the key store is not owner-only"), "the key store is not owner-only")
        XCTAssertEqual((HubFleetRefusal.from(HubActionError.commandExited(1, "control_channel_failed: lost"))
            as? HubFleetRefusal)?.outcomeUnknown, true)
        XCTAssertEqual(message("Hub command failed."), "Hub command failed.", "a line without a code stays as it is")
    }

    func testALostRedeemAnswerReadsTheStateAndContinuesFromIt() throws {
        let fixture = try Fixture(state: .awaitingSignIn)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .signIn }
        var browser: (flow: FleetOAuthFlow, done: (Result<TeslaAuthCodeGrant, Error>) -> Void)?
        window.presentFleetSignIn = { flow, done in browser = (flow, done) }
        fixture.runner.results["redeem"] = [.failure(HubActionError.commandExited(
            1, "control_channel_failed: the request or answer was lost"))]
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        try fixture.waitUntil { browser != nil }
        // The service stored the login and a car meanwhile: preflight names the next step.
        fixture.runner.preflight = Fixture.preflight("awaiting_activation")
        let flow = try XCTUnwrap(browser?.flow)
        browser?.done(Self.grant(flow, url: "\(Self.redirect)?code=\(Self.code)&state=\(flow.state)"))
        try fixture.waitUntil { window.fleetStepForTests == .consent }
        XCTAssertEqual(window.errorMessageForTests,
                       "Hub did not confirm the answer. Hub has checked where the set-up stands; continue from the step shown.")
        XCTAssertNil(fixture.runner.call("activate"))
    }

    func testAFailedDiscoveryAfterTheSignInLeadsToFindingTheCarNotANewSignIn() throws {
        let fixture = try Fixture(state: .awaitingSignIn)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .signIn }
        var browser: (flow: FleetOAuthFlow, done: (Result<TeslaAuthCodeGrant, Error>) -> Void)?
        window.presentFleetSignIn = { flow, done in browser = (flow, done) }
        fixture.runner.results["redeem"] = [.success(#"{"status":"signed_in","slot":"fleet_api","origin":"fleet_oauth","signIn":"exchanged","aftercare":"complete","vehicleDiscovery":"failed","discoveryCode":"discovery_rate_limited","discoveryMessage":"Tesla rate-limited the vehicle list; run setup-fleet discover later."}"#)]
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        try fixture.waitUntil { browser != nil }
        let flow = try XCTUnwrap(browser?.flow)
        browser?.done(Self.grant(flow, url: "\(Self.redirect)?code=\(Self.code)&state=\(flow.state)"))
        try fixture.waitUntil { window.fleetStepForTests == .vehicles && !window.isBusyForTests }
        XCTAssertEqual(window.fleetNoticeForTests, "Tesla asked Hub to wait. Find your car again in a few minutes.")
        window.continueForTests()
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests }
        XCTAssertEqual(fixture.runner.fleetVerbs, ["begin", "redeem", "discover"])
    }

    // MARK: Inputs and the card

    func testInputsCarryNoEscapeTheServiceRefusesAndNoTokenKey() throws {
        let application = HubFleetApplication(clientID: Self.clientID, region: .na, redirectURI: Self.redirect,
                                              domain: Self.domain)
        for bad in ["with\"quote", "with\\backslash", "with\nnewline", "with\u{7}bell", "with\u{85}nel", ""] {
            XCTAssertThrowsError(try HubController.fleetRegisterInput(application, clientSecret: bad), bad)
        }
        let input = try HubController.fleetRegisterInput(application, clientSecret: "ok/secret-é")
        let text = input.withBytes { String(decoding: $0, as: UTF8.self) }
        input.wipe()
        XCTAssertFalse(text.replacingOccurrences(of: "\\/", with: "").contains("\\"), text)
        XCTAssertFalse(text.lowercased().contains("token"))
        XCTAssertTrue(input.isWiped)
        XCTAssertFalse(try HubController.fleetBeginInput(application).lowercased().contains("secret"))

        XCTAssertThrowsError(try HubFleetApplication.validated(clientID: "id", region: .eu,
                                                               redirectURI: "https://example.com/cb?x=1", domain: "example.com"))
        XCTAssertThrowsError(try HubFleetApplication.validated(clientID: "id", region: .eu,
                                                               redirectURI: "https://example.com/cb#f", domain: "example.com"))
        XCTAssertThrowsError(try HubFleetApplication.validated(clientID: "id", region: .eu,
                                                               redirectURI: Self.redirect, domain: "https://example.com"))
        XCTAssertEqual(try HubFleetApplication.validated(clientID: " id ", region: .cn, redirectURI: Self.redirect,
                                                         domain: "Example.COM").domain, "example.com")
    }

    func testTheFleetCardIsHiddenWhileTheBuildSwitchIsOff() throws {
        XCTAssertFalse(HubFleetOnboarding.offeredByDefault)
        HubFleetOnboarding.isOffered = false
        let onboarding = OnboardingWindowController(
            controller: HubController(environment: ["TESATLAS_HUB_UI_PREVIEW": "1"]),
            previewRoute: "provider", onComplete: { _ in })
        defer { onboarding.close() }
        let titles = Fixture.descendants(onboarding.window?.contentView)
            .compactMap { ($0 as? HubChoiceRowButton)?.title }
        XCTAssertEqual(titles, ["Owner sign-in"])
        var state = HubOnboardingState(route: .provider, provider: .fleet)
        state.advance()
        XCTAssertEqual(state.route, .legacy, "the owner sign-in stays the route")

        HubFleetOnboarding.isOffered = true
        let offered = OnboardingWindowController(
            controller: HubController(environment: ["TESATLAS_HUB_UI_PREVIEW": "1"]),
            previewRoute: "provider", onComplete: { _ in })
        defer { offered.close() }
        let shown = Fixture.descendants(offered.window?.contentView).compactMap { ($0 as? NSTextField)?.stringValue }
        XCTAssertEqual(Fixture.descendants(offered.window?.contentView).compactMap { ($0 as? HubChoiceRowButton)?.title },
                       ["Owner sign-in", "Fleet API"], "the owner sign-in stays first")
        XCTAssertTrue(shown.contains { $0.contains("your own Tesla developer application and a domain") })
    }

    func testNoRouteBuildsAFleetScreenWhileTheSwitchIsOff() throws {
        HubFleetOnboarding.isOffered = false
        func fleetViews(_ onboarding: OnboardingWindowController) -> [String] {
            Fixture.descendants(onboarding.window?.contentView)
                .compactMap { $0.identifier?.rawValue }
                .filter { $0.hasPrefix("onboarding.fleet") }
        }
        // The preview environment, by route name and by catalogue scene.
        for environment in [["TESATLAS_HUB_UI_PREVIEW": "1", "TESATLAS_HUB_ONBOARDING_PREVIEW": "fleet"],
                            ["TESATLAS_HUB_PREVIEW_SCENE": "r14-fleet"]] {
            let controller = HubController(environment: environment)
            XCTAssertEqual(controller.onboardingPreviewRoute, "fleet")
            let preview = OnboardingWindowController(controller: controller,
                                                     previewRoute: controller.onboardingPreviewRoute,
                                                     onComplete: { _ in })
            defer { preview.close() }
            XCTAssertEqual(preview.currentRoute, .legacy, "\(environment)")
            XCTAssertEqual(fleetViews(preview), [], "\(environment)")
        }
        // An explicit first route, and navigation.
        let explicit = OnboardingWindowController(
            controller: HubController(environment: ["TESATLAS_HUB_UI_PREVIEW": "1"]),
            initialRoute: .fleet, onComplete: { _ in })
        defer { explicit.close() }
        XCTAssertEqual(explicit.currentRoute, .legacy)
        XCTAssertEqual(fleetViews(explicit), [])
        explicit.navigate(to: .fleet)
        XCTAssertEqual(explicit.currentRoute, .legacy)
        XCTAssertEqual(fleetViews(explicit), [])

        // The same routes build the Fleet screen once test code switches Fleet on.
        HubFleetOnboarding.isOffered = true
        explicit.navigate(to: .fleet)
        XCTAssertEqual(explicit.currentRoute, .fleet)
        XCTAssertFalse(fleetViews(explicit).isEmpty)
    }

    // MARK: Helpers

    private static func flow() throws -> FleetOAuthFlow {
        var components = URLComponents(string: "https://auth.tesla.com/oauth2/v3/authorize")!
        components.queryItems = [URLQueryItem(name: "response_type", value: "code"),
                                 URLQueryItem(name: "client_id", value: clientID),
                                 URLQueryItem(name: "redirect_uri", value: redirect),
                                 URLQueryItem(name: "state", value: "fictional-state-0123456789-abcdefghijklmnop")]
        return try FleetOAuthFlow(authorizationURL: components.url!, redirectURI: redirect, clientID: clientID)
    }

    /// What the window controller delivers for one callback navigation.
    private static func grant(_ flow: FleetOAuthFlow, url: String) -> Result<TeslaAuthCodeGrant, Error> {
        Result {
            switch try flow.callback(URL(string: url)!) {
            case .cancelled: throw TeslaAuthError.cancelled
            case let .code(grant): return grant
            }
        }
    }

    private static func object(_ text: String?) throws -> [String: String] {
        let data = Data(try XCTUnwrap(text).utf8)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: String])
    }
}

/// The M09-T19a contract, faked: each `setup-fleet` verb answers from `results` or the
/// contract's success shape.
private final class FleetFlowRunner: HubCommandRunning {
    struct Call {
        let arguments: [String]
        let stdin: String?
        let sensitive: Bool
        var verb: String? { FleetFlowRunner.verb(arguments) }
    }

    private let lock = NSLock()
    private var recorded: [Call] = []
    var results: [String: [Result<String, Error>]] = [:]
    var preflight = Fixture.preflight(nil)
    let binding = UUID().uuidString.lowercased()

    var calls: [Call] { lock.lock(); defer { lock.unlock() }; return recorded }
    var fleetVerbs: [String] { calls.compactMap(\.verb) }
    func call(_ verb: String) -> Call? { calls.last { $0.verb == verb } }

    static func verb(_ arguments: [String]) -> String? {
        guard let index = arguments.firstIndex(of: "setup-fleet") else { return nil }
        let next = arguments.dropFirst(index + 1).first
        if let next, ["begin", "discover", "activate", "register", "public-key"].contains(next) { return next }
        return "redeem"
    }

    private func answer(_ arguments: [String], stdin: String?, sensitive: Bool,
                        completion: @escaping (Result<String, Error>) -> Void) {
        lock.lock(); recorded.append(Call(arguments: arguments, stdin: stdin, sensitive: sensitive)); lock.unlock()
        if let verb = Self.verb(arguments) {
            if var queue = results[verb], !queue.isEmpty {
                let result = queue.removeFirst()
                results[verb] = queue
                completion(result)
                return
            }
            completion(.success(success(verb, stdin: stdin)))
        } else if arguments.contains("preflight") {
            completion(.success(preflight))
        } else if arguments.contains("window") {
            completion(.success(#"{"phase":"open","trusted":true,"faulted":false,"closeReason":null,"seq":1}"#))
        } else if arguments.contains("status") {
            completion(.success(#"{"status":"ok","version":"\#(HubRelease.bundledVersion)","ready":false,"provider":"fleet","vehicles":[],"credentials":{"present":false}}"#))
        } else {
            completion(.success("{}"))
        }
    }

    private func success(_ verb: String, stdin: String?) -> String {
        switch verb {
        case "public-key":
            let path = calls.last?.arguments.last ?? ""
            return #"{"status":"public_key_saved","path":"\#(path)","publishAt":"/.well-known/appspecific/com.tesla.3p.public-key.pem"}"#
        case "register":
            return #"{"status":"partner_registered","registration":"registered","domain":"example.com","region":"eu"}"#
        case "begin":
            let input = (stdin.flatMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) } as? [String: String]) ?? [:]
            var components = URLComponents(string: "https://auth.tesla.com/oauth2/v3/authorize")!
            components.queryItems = [
                URLQueryItem(name: "response_type", value: "code"),
                URLQueryItem(name: "client_id", value: input["clientId"]),
                URLQueryItem(name: "redirect_uri", value: input["redirectUri"]),
                URLQueryItem(name: "scope", value: "openid offline_access vehicle_device_data vehicle_location"),
                URLQueryItem(name: "state", value: "fictional-state-0123456789-abcdefghijklmnop")
            ]
            let object: [String: Any] = ["status": "sign_in_begun", "slot": "fleet_api",
                                         "authorizeUrl": components.url!.absoluteString, "binding": binding,
                                         "expiresAt": Int(Date().timeIntervalSince1970) + 600]
            return String(decoding: try! JSONSerialization.data(withJSONObject: object), as: UTF8.self)
        case "redeem":
            return #"{"status":"signed_in","slot":"fleet_api","origin":"fleet_oauth","signIn":"exchanged","aftercare":"complete","vehicleDiscovery":"configured","vehiclesConfigured":1}"#
        case "discover":
            return #"{"status":"vehicles_configured","slot":"fleet_api","vehiclesConfigured":1}"#
        case "activate":
            return #"{"status":"collection_activated","provider":"fleet","collectionIntervalSeconds":300}"#
        default:
            return "{}"
        }
    }

    func run(arguments: [String], completion: @escaping (Result<String, Error>) -> Void) {
        answer(arguments, stdin: nil, sensitive: false, completion: completion)
    }

    func run(arguments: [String], sensitiveInput: HubSensitiveInput,
             completion: @escaping (Result<String, Error>) -> Void) {
        let text = sensitiveInput.withBytes { String(decoding: $0, as: UTF8.self) }
        sensitiveInput.wipe()
        answer(arguments, stdin: text, sensitive: true, completion: completion)
    }

    func run(arguments: [String], stdin: String, completion: @escaping (Result<String, Error>) -> Void) {
        answer(arguments, stdin: stdin, sensitive: false, completion: completion)
    }

    func run(arguments: [String], onOutputLine: @escaping (String) -> Void,
             completion: @escaping (Result<String, Error>) -> Void) {
        answer(arguments, stdin: nil, sensitive: false, completion: completion)
    }

    func run(arguments: [String], timeout: TimeInterval,
             completion: @escaping (Result<String, Error>) -> Void) {
        answer(arguments, stdin: nil, sensitive: false, completion: completion)
    }
}

private final class FleetFlowService: HubServiceControlling {
    private(set) var calls: [String] = []
    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) { completion(.unloaded) }
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        calls.append(arguments.joined(separator: " "))
        completion(.success(""))
    }
    func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        completion(.success(""))
    }
    func releaseRestartRecord(owner: HubLifecycleToken) {}
    func restartRecordHeld(by owner: HubLifecycleToken) -> Bool { false }
    func deleteHubAndData(owner: HubLifecycleToken,
                          completion: @escaping (Result<HubDataRemovalReport, Error>) -> Void) {
        completion(.failure(HubActionError.preview))
    }
    func reconcile(owner: HubLifecycleToken, ensure: @escaping () throws -> HubLaunchAgentChange,
                   completion: @escaping (HubReconcileOutcome) -> Void) { completion(.completed) }
    var restartDutyOutstanding: Bool { false }
    func whenRestartDutySettled(_ callback: @escaping () -> Void) { callback() }
}

private final class Fixture {
    let home: URL
    let runner: FleetFlowRunner
    let service: FleetFlowService
    let controller: HubController
    let window: OnboardingWindowController

    static func preflight(_ state: String?) -> String {
        guard let state else { return #"{"status":"ready","state":"idle","version":"1"}"# }
        return #"{"status":"ready","state":"\#(state)","version":"1","provider":"fleet","collectorEnabled":false}"#
    }

    /// A fresh Mac, or a Fleet Hub left in `state` by an earlier run of the app. `config` is
    /// appended to that Hub's configuration.
    init(state: HubFleetSetupState? = nil, config: String = "") throws {
        home = HubTestScratch.directory.appendingPathComponent("tesatlas-fleet-flow-\(UUID().uuidString)", isDirectory: true)
        let folder = home.appendingPathComponent("Library/Application Support/Tesatlas Hub", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: home.appendingPathComponent("Desktop"), withIntermediateDirectories: true)
        let runner = FleetFlowRunner()
        let service = FleetFlowService()
        let controller = HubController(commandRunner: runner, serviceRunner: service, homeDirectory: home,
                                       serviceInstalledOverride: state != nil)
        if let state {
            try ("data_dir = \"/tmp/hub\"\n\n[collector]\nprovider = \"fleet\"\ninterval_seconds = 0\n" + config)
                .write(to: folder.appendingPathComponent("config.toml"), atomically: true, encoding: .utf8)
            try controller.saveFleetApplication(HubFleetApplication(
                clientID: "c0ffee00-1111-4222-8333-444455556666", region: .eu,
                redirectURI: "https://example.com/tesla/callback", domain: "example.com", registered: true))
            switch state {
            case .awaitingSignIn: runner.preflight = Self.preflight("awaiting_sign_in")
            case .awaitingVehicles: runner.preflight = Self.preflight("awaiting_vehicles")
            case .awaitingActivation: runner.preflight = Self.preflight("awaiting_activation")
            default: break
            }
        }
        self.runner = runner
        self.service = service
        self.controller = controller
        window = OnboardingWindowController(controller: controller, initialRoute: .fleet, onComplete: { _ in })
    }

    var configText: String {
        (try? String(contentsOf: controller.maintenanceConfigPath, encoding: .utf8)) ?? ""
    }

    func tearDown() {
        window.close()
        try? FileManager.default.removeItem(at: home)
    }

    func waitUntil(timeout: TimeInterval = 5, _ condition: () -> Bool,
                   file: StaticString = #filePath, line: UInt = #line) throws {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() {
            guard Date() < deadline else {
                XCTFail("condition not reached; commands \(runner.calls.map { $0.verb ?? $0.arguments.last ?? "" }), "
                        + "error \(window.errorMessageForTests ?? "none")", file: file, line: line)
                throw HubActionError.commandTimedOut
            }
            RunLoop.main.run(until: Date().addingTimeInterval(0.01))
        }
    }

    static func descendants(_ view: NSView?) -> [NSView] {
        guard let view else { return [] }
        return [view] + view.subviews.flatMap { descendants($0) }
    }

    func labels() -> [String] {
        Self.descendants(window.window?.contentView).compactMap { ($0 as? NSTextField)?.stringValue }
    }

    func view(identifier: String) -> NSView? {
        Self.descendants(window.window?.contentView).first { $0.identifier?.rawValue == identifier }
    }

    /// Whether any file under the test home holds `text`.
    func filesContain(_ text: String) -> Bool {
        guard let walker = FileManager.default.enumerator(at: home, includingPropertiesForKeys: nil) else { return false }
        for case let url as URL in walker {
            if let data = try? Data(contentsOf: url), data.range(of: Data(text.utf8)) != nil { return true }
        }
        return false
    }
}

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
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
        let register = try XCTUnwrap(fixture.runner.call("register"))
        XCTAssertTrue(register.sensitive)
        XCTAssertEqual(try Self.object(register.stdin), [
            "clientId": Self.clientID, "clientSecret": Self.secret, "domain": Self.domain, "region": "eu"
        ])
        XCTAssertEqual(fixture.controller.savedFleetApplication?.registered, true)

        // Consent comes before the first request Tesla bills: nothing is begun, redeemed or listed.
        XCTAssertEqual(fixture.runner.fleetVerbs, ["public-key", "register"])
        XCTAssertNotNil(fixture.view(identifier: "onboarding.fleet.consent"))
        let terms = fixture.labels().joined(separator: "\n")
        XCTAssertTrue(terms.contains("every 30 seconds"), "the cadence is shown")
        XCTAssertTrue(terms.contains("$0.002"), "the price is shown")
        XCTAssertTrue(terms.contains("$10.00"), "the monthly cap is shown")
        XCTAssertTrue(terms.contains("Setting up makes a few list requests, which Tesla bills at the same price as a state check"),
                      "set-up's own billed lists are stated")
        XCTAssertEqual(window.continueTitleForTests, "Accept and Continue")
        XCTAssertFalse(window.continueEnabledForTests)
        window.continueForTests()
        XCTAssertEqual(window.fleetStepForTests, .consent, "Continue does nothing until the owner accepts")
        XCTAssertNil(fixture.controller.fleetConsentRecord)
        window.acceptFleetConsentForTests()
        window.continueForTests()
        try fixture.waitUntil { !window.isBusyForTests }
        XCTAssertEqual(window.fleetStepForTests, .signIn)
        XCTAssertEqual(fixture.controller.fleetConsentRecord?.figures, .serviceDefaults)
        XCTAssertEqual(fixture.runner.fleetVerbs, ["public-key", "register"], "accepting sends nothing")

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
        try fixture.waitUntil { window.fleetStepForTests == .start && !window.isBusyForTests }
        let redeem = try XCTUnwrap(fixture.runner.call("redeem"))
        XCTAssertTrue(redeem.sensitive)
        XCTAssertEqual(Array(redeem.arguments.suffix(2)), ["setup-fleet", "--all-vehicles"])
        XCTAssertEqual(try Self.object(redeem.stdin), [
            "code": Self.code, "redirectUri": Self.redirect, "clientId": Self.clientID,
            "clientSecret": Self.secret, "region": "eu", "binding": fixture.runner.binding
        ])

        // The last page: the car and the accepted figures; nothing is polled until Start Collecting.
        XCTAssertNil(fixture.runner.call("activate"))
        XCTAssertTrue(fixture.configText.contains("interval_seconds = 0"))
        XCTAssertNotNil(fixture.view(identifier: "onboarding.fleet.start-page"))
        let shown = fixture.labels().joined(separator: "\n")
        XCTAssertTrue(shown.contains("Hub found 1 car on your Tesla account."), shown)
        XCTAssertTrue(shown.contains("You accepted these figures on"))
        XCTAssertTrue(shown.contains("every 30 seconds") && shown.contains("$10.00"), "the accepted figures")
        XCTAssertEqual(window.continueTitleForTests, "Start Collecting")
        XCTAssertTrue(window.continueEnabledForTests)
        window.continueForTests()
        try fixture.waitUntil { window.currentRoute == .verify }
        XCTAssertNotNil(fixture.runner.call("activate"))

        // The order of section 10 with the consent before the sign-in, and the secret only where it belongs.
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
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
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
        let signIn = try Fixture(state: .awaitingSignIn, consented: true)
        try signIn.waitUntil { signIn.window.fleetStepForTests == .signIn && !signIn.window.isBusyForTests }
        XCTAssertEqual(signIn.window.fleetNoticeForTests,
                       "Hub is waiting for your Tesla sign-in. Sign in with Tesla to continue.")
        XCTAssertEqual(signIn.window.continueTitleForTests, "Sign in with Tesla")
        signIn.tearDown()

        let vehicles = try Fixture(state: .awaitingVehicles, consented: true)
        try vehicles.waitUntil { vehicles.window.fleetStepForTests == .vehicles && !vehicles.window.isBusyForTests }
        XCTAssertEqual(vehicles.window.fleetNoticeForTests,
                       "You are signed in, and Hub has not found your car yet. Find your car to continue.")
        XCTAssertEqual(vehicles.window.continueTitleForTests, "Find My Car")
        vehicles.window.continueForTests()
        try vehicles.waitUntil { vehicles.window.fleetStepForTests == .start && !vehicles.window.isBusyForTests }
        XCTAssertEqual(vehicles.runner.call("discover").map { Array($0.arguments.suffix(3)) }, ["setup-fleet", "discover", "--all-vehicles"])
        XCTAssertNil(vehicles.runner.call("redeem"), "no new sign-in")
        XCTAssertNil(vehicles.runner.call("activate"))
        vehicles.tearDown()

        let activation = try Fixture(state: .awaitingActivation, consented: true)
        defer { activation.tearDown() }
        try activation.waitUntil { activation.window.fleetStepForTests == .start && !activation.window.isBusyForTests }
        XCTAssertEqual(activation.window.fleetNoticeForTests,
                       "You are signed in and your car is found. Hub polls nothing until you start collecting.")
        XCTAssertEqual(activation.window.continueTitleForTests, "Start Collecting")
        XCTAssertNil(activation.runner.call("activate"))
        activation.window.continueForTests()
        try activation.waitUntil { activation.window.currentRoute == .verify }
        XCTAssertEqual(activation.runner.fleetVerbs, ["activate"])
    }

    func testAReturnToAnUnregisteredApplicationContinuesAtThePublicKeyAndTheRegistration() throws {
        let fixture = try Fixture(state: .awaitingSignIn, registered: false)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .publicKey && !window.isBusyForTests }
        XCTAssertEqual(window.fleetNoticeForTests,
                       "Hub is waiting for your Tesla sign-in. Sign in with Tesla to continue.")
        XCTAssertNotNil(fixture.view(identifier: "onboarding.fleet.save-key"))
        window.enterFleetSecretForTests(Self.secret)
        XCTAssertEqual(window.continueTitleForTests, "Register Domain")
        window.continueForTests()
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
        XCTAssertEqual(fixture.runner.fleetVerbs, ["register"], "the registration, not a sign-in")
        XCTAssertEqual(fixture.controller.savedFleetApplication?.registered, true)
    }

    // MARK: Hub not running (note 4)

    func testAStoppedInstalledServiceIsStartedThroughTheStartActionBeforeTheRouteContinues() throws {
        let fixture = try Fixture(state: .awaitingVehicles, consented: true, serviceStopped: true)
        defer { fixture.tearDown() }
        try fixture.waitUntil { fixture.window.fleetStepForTests == .vehicles && !fixture.window.isBusyForTests }
        XCTAssertEqual(fixture.service.calls, ["service start"])
        XCTAssertNil(fixture.window.errorMessageForTests)
    }

    func testAStepRefusedBecauseHubIsNotRunningStartsHubAndContinues() throws {
        let fixture = try Fixture(state: .awaitingVehicles, consented: true)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .vehicles && !window.isBusyForTests }
        fixture.runner.results["discover"] = [.failure(HubActionError.commandExited(
            1, "service_not_running: the Hub service is not running"))]
        window.continueForTests()
        try fixture.waitUntil { window.fleetStepForTests == .start && !window.isBusyForTests }
        XCTAssertEqual(fixture.service.calls, ["service start"])
        XCTAssertEqual(fixture.runner.fleetVerbs, ["discover", "discover"], "the step runs again once Hub runs")
    }

    func testASignInRefusedBecauseHubIsNotRunningStartsHubAndAsksForTheSecretAgain() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .signIn && !window.isBusyForTests }
        window.presentFleetSignIn = { _, _ in XCTFail("no browser after a refused begin") }
        fixture.runner.results["begin"] = [.failure(HubActionError.commandExited(
            1, "service_not_running: the Hub service is not running"))]
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        try fixture.waitUntil { fixture.service.calls == ["service start"] && !window.isBusyForTests }
        XCTAssertEqual(window.fleetStepForTests, .signIn)
        XCTAssertEqual(window.fleetNoticeForTests, "Hub was not running, so Hub started it. Continue from this step.")
        XCTAssertEqual(fixture.runner.fleetVerbs, ["begin"], "a step that carried the secret is not repeated")
        XCTAssertEqual(window.fleetSecretForTests, "")
        XCTAssertFalse(window.continueEnabledForTests, "the owner types the secret again")
    }

    func testAFailedStartIsShownWithTheServicesText() throws {
        let fixture = try Fixture(state: .awaitingVehicles, consented: true, serviceStopped: true)
        defer { fixture.tearDown() }
        fixture.service.startResults = [.failure(HubActionError.commandFailed("Fixed text the service wrote for its start."))]
        let window = fixture.window
        try fixture.waitUntil { window.errorMessageForTests != nil && !window.isBusyForTests }
        XCTAssertTrue(window.errorMessageForTests?.contains("Fixed text the service wrote for its start.") == true,
                      window.errorMessageForTests ?? "")
        XCTAssertEqual(fixture.runner.fleetVerbs, [])
    }

    // MARK: Evidence of consent (B4)

    func testAFleetHubWithAPositiveIntervalAndNoConsentRecordCannotResumeThroughTheRedeemOrDiscovery() throws {
        for state in [HubFleetSetupState.awaitingSignIn, .awaitingVehicles] {
            let fixture = try Fixture(state: state, interval: 300)
            defer { fixture.tearDown() }
            let window = fixture.window
            try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
            // The existing preparation set the interval to 0 first, for this installed Fleet Hub.
            XCTAssertTrue(fixture.configText.contains("interval_seconds = 0"), "\(state): \(fixture.configText)")
            XCTAssertFalse(fixture.configText.contains("interval_seconds = 300"), "\(state)")
            XCTAssertEqual(fixture.service.calls, ["service pause", "service start"], "\(state)")
            XCTAssertEqual(fixture.runner.fleetVerbs, [], "\(state)")

            window.acceptFleetConsentForTests()
            window.continueForTests()
            try fixture.waitUntil { !window.isBusyForTests }

            if state == .awaitingSignIn {
                XCTAssertEqual(window.fleetStepForTests, .signIn)
                var browser: (flow: FleetOAuthFlow, done: (Result<TeslaAuthCodeGrant, Error>) -> Void)?
                window.presentFleetSignIn = { flow, done in browser = (flow, done) }
                window.enterFleetSecretForTests(Self.secret)
                window.continueForTests()
                try fixture.waitUntil { browser != nil }
                let flow = try XCTUnwrap(browser?.flow)
                browser?.done(Self.grant(flow, url: "\(Self.redirect)?code=\(Self.code)&state=\(flow.state)"))
            } else {
                XCTAssertEqual(window.fleetStepForTests, .vehicles)
                window.continueForTests()
            }
            try fixture.waitUntil { window.fleetStepForTests == .start && !window.isBusyForTests }
            // The login and the car are stored with interval 0: nothing collects before Start Collecting.
            XCTAssertTrue(fixture.configText.contains("interval_seconds = 0"), "\(state)")
            XCTAssertNil(fixture.runner.call("activate"), "\(state)")
        }
    }

    /// R1: a failed preparation of an unconsented Fleet Hub with a positive interval leaves Hub
    /// stopped, shows the service's text and offers only to try the preparation again.
    func testAFailedPreparationLeavesHubStoppedAndOffersToTryAgain() throws {
        let fixture = try Fixture(state: .awaitingSignIn, interval: 300, pauseFails: "Fixed text the service wrote for its pause.")
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetPreparationFailedForTests != nil && !window.isBusyForTests }
        let shown = try XCTUnwrap(window.fleetPreparationFailedForTests)
        XCTAssertTrue(shown.hasPrefix("Fixed text the service wrote for its pause."), shown)
        XCTAssertTrue(shown.contains("Hub is left stopped"), shown)
        XCTAssertNotNil(fixture.view(identifier: "onboarding.fleet.prepare-failed"))
        XCTAssertEqual(fixture.service.calls, ["service pause"])
        XCTAssertEqual(fixture.service.recoveries, 0, "nothing starts Hub with interval 300")
        XCTAssertTrue(fixture.configText.contains("interval_seconds = 300"))
        XCTAssertEqual(window.continueTitleForTests, "Try Again")
        XCTAssertTrue(window.continueEnabledForTests)
        XCTAssertEqual(fixture.runner.fleetVerbs, [])

        window.continueForTests()
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
        XCTAssertNil(window.fleetPreparationFailedForTests)
        XCTAssertEqual(fixture.service.calls, ["service pause", "service pause", "service start"])
        XCTAssertEqual(fixture.service.recoveries, 0)
        XCTAssertTrue(fixture.configText.contains("interval_seconds = 0"))
        XCTAssertEqual(fixture.runner.fleetVerbs, [])
    }

    func testAMatchingConsentRecordKeepsTheIntervalOfASignInAfterASignOut() throws {
        let fixture = try Fixture(state: .awaitingSignIn, interval: 300, consented: true)
        defer { fixture.tearDown() }
        try fixture.waitUntil { fixture.window.fleetStepForTests == .signIn && !fixture.window.isBusyForTests }
        XCTAssertTrue(fixture.configText.contains("interval_seconds = 300"))
        XCTAssertEqual(fixture.service.calls, [], "neither paused nor restarted")
        XCTAssertEqual(fixture.window.continueTitleForTests, "Sign in with Tesla", "consent is not asked again")
    }

    func testFiguresThatDifferFromTheAcceptedOnesAskForConsentAgain() throws {
        let fixture = try Fixture(state: .awaitingVehicles, interval: 300, consented: true,
                                  figures: FleetFlowRunner.figures { $0["monthlyCapCents"] = 2_000 })
        defer { fixture.tearDown() }
        let window = fixture.window
        XCTAssertEqual(fixture.controller.fleetConsentRecord?.figures, .serviceDefaults, "accepted at $10.00")
        XCTAssertFalse(fixture.controller.fleetConsentInEffect)
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
        XCTAssertTrue(fixture.configText.contains("interval_seconds = 0"))
        XCTAssertTrue(fixture.labels().joined().contains("when its requests reach $20.00"))
        window.continueForTests()
        XCTAssertEqual(fixture.runner.fleetVerbs, [])
        window.acceptFleetConsentForTests()
        window.continueForTests()
        try fixture.waitUntil { !window.isBusyForTests }
        XCTAssertEqual(window.fleetStepForTests, .vehicles)
        XCTAssertEqual(fixture.controller.fleetConsentRecord?.figures.monthlyCapCents, 2_000)
        XCTAssertTrue(fixture.controller.fleetConsentInEffect)
    }

    /// R3: figures that change while the consent page is open are never recorded without having
    /// been shown and ticked. The page shows the new figures, clears the tick and asks again.
    func testFiguresThatChangeWhileThePageIsOpenAreShownAndAskedForAgain() throws {
        let fixture = try Fixture(state: .awaitingActivation)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
        XCTAssertTrue(fixture.labels().joined().contains("when its requests reach $10.00"))
        fixture.changeFigures(monthlyCapCents: 2_000)
        window.acceptFleetConsentForTests()
        window.continueForTests()
        try fixture.waitUntil { !window.isBusyForTests }
        try fixture.waitUntil { fixture.labels().joined().contains("when its requests reach $20.00") && !window.isBusyForTests }
        XCTAssertNil(fixture.controller.fleetConsentRecord, "the $10.00 shown is not what is in effect; $20.00 was not shown")
        XCTAssertEqual(window.fleetStepForTests, .consent)
        XCTAssertEqual(window.fleetNoticeForTests, OnboardingWindowController.fleetFiguresChangedText)
        XCTAssertFalse(window.continueEnabledForTests, "the tick is cleared")
        window.continueForTests()
        XCTAssertNil(fixture.controller.fleetConsentRecord)

        window.acceptFleetConsentForTests()
        window.continueForTests()
        try fixture.waitUntil { !window.isBusyForTests }

        try fixture.waitUntil { window.fleetStepForTests == .start && !window.isBusyForTests }
        XCTAssertEqual(fixture.controller.fleetConsentRecord?.figures.monthlyCapCents, 2_000)
        XCTAssertEqual(fixture.runner.fleetVerbs.filter { $0 != "figures" }, [])
    }

    func testTheConsentRecordHoldsTheTimeAndTheFiguresAndNoSecret() throws {
        let fixture = try Fixture()
        defer { fixture.tearDown() }
        let accepted = Date(timeIntervalSince1970: 1_795_000_000)
        try fixture.controller.recordFleetConsent(.serviceDefaults, at: accepted)
        let url = fixture.controller.maintenanceConfigPath.deletingLastPathComponent()
            .appendingPathComponent("fleet-consent.json")
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        XCTAssertEqual(Set(object.keys), ["acceptedAt", "figures"])
        XCTAssertEqual(object["acceptedAt"] as? Int, 1_795_000_000)
        // Exactly the answer of `setup-fleet figures` (interface section 12's example).
        let answer = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(FleetFlowRunner.figures { _ in }.utf8)) as? NSDictionary)
        XCTAssertEqual(object["figures"] as? NSDictionary, answer)
        XCTAssertEqual(fixture.controller.fleetConsentRecord,
                       HubFleetConsentRecord(acceptedAt: accepted, figures: .serviceDefaults))
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        XCTAssertEqual((attributes[.posixPermissions] as? NSNumber)?.intValue, 0o600)
    }

    // MARK: Consent before anything Tesla bills (B3)

    func testNoBeginRedeemDiscoverOrActivateRunsBeforeConsentOnAnyReturn() throws {
        let resumes: [(HubFleetSetupState, HubFleetStep, String)] = [
            (.awaitingSignIn, .signIn, "Sign in with Tesla"),
            (.awaitingVehicles, .vehicles, "Find My Car"),
            (.awaitingActivation, .start, "Start Collecting"),
            (.collecting, .signIn, "Sign in with Tesla")
        ]
        for (state, resume, title) in resumes {
            let fixture = try Fixture(state: state)
            defer { fixture.tearDown() }
            let window = fixture.window
            window.presentFleetSignIn = { _, _ in XCTFail("no browser before consent: \(state)") }
            try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
            // Every button the page offers before acceptance, and every billed verb called directly.
            window.enterFleetSecretForTests(Self.secret)
            window.continueForTests()
            XCTAssertEqual(window.fleetStepForTests, .consent, "\(state)")
            try Self.expectRefused { fixture.controller.discoverFleetVehicles(completion: $0) }
            try Self.expectRefused { fixture.controller.activateFleetCollection(completion: $0) }
            try Self.expectRefused { done in
                fixture.controller.beginFleetSignIn(try! HubFleetApplication.validated(
                    clientID: Self.clientID, region: .eu, redirectURI: Self.redirect, domain: Self.domain)) { done($0.map { _ in 0 }) }
            }
            XCTAssertEqual(fixture.runner.fleetVerbs, [], "nothing billed or started before consent: \(state)")
            XCTAssertTrue(fixture.configText.contains("interval_seconds = 0"), "\(state)")

            // Accepting continues at the step the waiting state names, and only then may it run.
            window.acceptFleetConsentForTests()
            window.continueForTests()
            try fixture.waitUntil { !window.isBusyForTests }
            XCTAssertEqual(window.fleetStepForTests, resume, "\(state)")
            XCTAssertEqual(window.continueTitleForTests, title, "\(state)")
            XCTAssertNotNil(fixture.controller.fleetConsentRecord, "\(state)")
            XCTAssertEqual(fixture.runner.fleetVerbs, [], "accepting sends nothing: \(state)")
        }
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

    // MARK: The secret's lifetime (B1): each attempted operation owns it for exactly its own life

    private final class Leg {
        var flow: FleetOAuthFlow?
        var done: ((Result<TeslaAuthCodeGrant, Error>) -> Void)?
        var buffer: HubFleetSecret?
    }

    /// A consented Hub waiting for the sign-in, an attempt begun and its Tesla window open.
    private func openAttempt(_ fixture: Fixture, secret: String = FleetSignInFlowTests.secret) throws -> Leg {
        let window = fixture.window
        if window.fleetStepForTests != .signIn || window.isBusyForTests {
            try fixture.waitUntil { window.fleetStepForTests == .signIn && !window.isBusyForTests }
        }
        let leg = Leg()
        window.presentFleetSignIn = { flow, done in leg.flow = flow; leg.done = done }
        window.enterFleetSecretForTests(secret)
        window.continueForTests()
        XCTAssertEqual(window.fleetSecretForTests, "", "the field is cleared as begin starts")
        try fixture.waitUntil { leg.done != nil }
        leg.buffer = window.fleetSecretsForTests.last
        XCTAssertEqual(leg.buffer?.isWiped, false, "held while the Tesla window is open")
        XCTAssertTrue(window.fleetSignInPendingForTests)
        return leg
    }

    private func assertEnded(_ fixture: Fixture, _ leg: Leg?, file: StaticString = #filePath, line: UInt = #line) {
        let window = fixture.window
        XCTAssertFalse(window.fleetSignInPendingForTests, "the attempt ended", file: file, line: line)
        XCTAssertEqual(window.fleetSecretForTests, "", "no control holds the secret", file: file, line: line)
        XCTAssertTrue(window.fleetSecretsForTests.allSatisfy(\.isWiped), "every secret buffer is wiped", file: file, line: line)
        XCTAssertEqual(leg?.buffer?.isWiped ?? true, true, file: file, line: line)
    }

    /// After an ended attempt, its late callback redeems nothing.
    private func assertLateCallbackIgnored(_ fixture: Fixture, _ leg: Leg, file: StaticString = #filePath, line: UInt = #line) throws {
        let flow = try XCTUnwrap(leg.flow)
        leg.done?(Self.grant(flow, url: "\(Self.redirect)?code=\(Self.code)&state=\(flow.state)"))
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        XCTAssertNil(fixture.runner.call("redeem"), "a late callback of an ended attempt is ignored", file: file, line: line)
    }

    func testTheSecretIsWipedWhenTheSignInSucceeds() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let leg = try openAttempt(fixture)
        let flow = try XCTUnwrap(leg.flow)
        leg.done?(Self.grant(flow, url: "\(Self.redirect)?code=\(Self.code)&state=\(flow.state)"))
        try fixture.waitUntil { fixture.window.fleetStepForTests == .start && !fixture.window.isBusyForTests }
        XCTAssertNotNil(fixture.runner.call("redeem"))
        assertEnded(fixture, leg)
    }

    func testTheSecretIsWipedWhenBeginIsRefused() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .signIn && !window.isBusyForTests }
        window.presentFleetSignIn = { _, _ in XCTFail("no browser after a refused begin") }
        fixture.runner.results["begin"] = [.failure(HubActionError.commandExited(1, "sign_in_busy: another sign-in holds the vault"))]
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        try fixture.waitUntil { !window.isBusyForTests && window.errorMessageForTests != nil }
        assertEnded(fixture, nil)
        XCTAssertEqual(window.fleetSecretsForTests.count, 1)
    }

    func testTheSecretIsWipedWhenTheBeginAnswerIsInvalid() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .signIn && !window.isBusyForTests }
        window.presentFleetSignIn = { _, _ in XCTFail("no browser after an invalid begin answer") }
        fixture.runner.results["begin"] = [.success(#"{"status":"sign_in_begun","slot":"fleet_api","authorizeUrl":"https://auth.example.com/","binding":"x","expiresAt":1}"#)]
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        try fixture.waitUntil { !window.isBusyForTests && window.errorMessageForTests != nil }
        XCTAssertEqual(window.errorMessageForTests,
                       "Hub did not confirm the answer. Hub has checked where the set-up stands; continue from the step shown.")
        assertEnded(fixture, nil)
    }

    func testTheSecretIsWipedWhenTheSignInWindowCannotBeBuilt() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .signIn && !window.isBusyForTests }
        window.presentFleetSignIn = { _, _ in throw TeslaAuthError.invalidAuthorizationURL }
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        try fixture.waitUntil { !window.isBusyForTests && window.errorMessageForTests != nil }
        XCTAssertEqual(window.errorMessageForTests, TeslaAuthError.invalidAuthorizationURL.localizedDescription)
        assertEnded(fixture, nil)
        XCTAssertNil(fixture.runner.call("redeem"))
    }

    func testTheSecretIsWipedWhenTheCallbackHasAnotherState() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let leg = try openAttempt(fixture)
        let flow = try XCTUnwrap(leg.flow)
        leg.done?(Self.grant(flow, url: "\(Self.redirect)?code=\(Self.code)&state=abandoned-attempt-state-000000000000"))
        XCTAssertEqual(fixture.window.errorMessageForTests, TeslaAuthError.stateMismatch.localizedDescription)
        assertEnded(fixture, leg)
        XCTAssertNil(fixture.runner.call("redeem"))
    }

    func testTheSecretIsWipedWhenAccessIsDenied() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let leg = try openAttempt(fixture)
        let flow = try XCTUnwrap(leg.flow)
        leg.done?(Self.grant(flow, url: "\(Self.redirect)?error=access_denied&state=\(flow.state)"))
        XCTAssertEqual(fixture.window.errorMessageForTests,
                       "Tesla did not grant vehicle information and vehicle location. Sign in with Tesla again and allow both on Tesla's consent page.")
        assertEnded(fixture, leg)
        XCTAssertNil(fixture.runner.call("redeem"))
    }

    func testTheSecretIsWipedWhenTheNavigationFails() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let leg = try openAttempt(fixture)
        leg.done?(.failure(URLError(.notConnectedToInternet)))
        XCTAssertNotNil(fixture.window.errorMessageForTests)
        assertEnded(fixture, leg)
        XCTAssertNil(fixture.runner.call("redeem"))
    }

    func testTheSecretIsWipedWhenTheSignInIsCancelled() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let leg = try openAttempt(fixture)
        leg.done?(.failure(TeslaAuthError.cancelled))
        assertEnded(fixture, leg)
        XCTAssertNil(fixture.runner.call("redeem"))
    }

    func testTheSecretIsWipedWhenTheWindowCloses() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let leg = try openAttempt(fixture)
        fixture.window.close()
        assertEnded(fixture, leg)
        try assertLateCallbackIgnored(fixture, leg)
    }

    func testTheSecretIsWipedOnBack() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let leg = try openAttempt(fixture)
        fixture.window.backForTests()
        XCTAssertEqual(fixture.window.fleetStepForTests, .publicKey)
        assertEnded(fixture, leg)
        try assertLateCallbackIgnored(fixture, leg)
    }

    func testTheSecretIsWipedWhenANewerAttemptStarts() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let first = try openAttempt(fixture)
        let second = try openAttempt(fixture, secret: Self.secret + "-second")
        XCTAssertEqual(first.buffer?.isWiped, true, "the newer attempt ended the first")
        XCTAssertEqual(second.buffer?.isWiped, false)
        try assertLateCallbackIgnored(fixture, first)
        let flow = try XCTUnwrap(second.flow)
        second.done?(Self.grant(flow, url: "\(Self.redirect)?code=\(Self.code)&state=\(flow.state)"))
        try fixture.waitUntil { fixture.window.fleetStepForTests == .start && !fixture.window.isBusyForTests }
        XCTAssertEqual(try Self.object(fixture.runner.call("redeem")?.stdin)["clientSecret"], Self.secret + "-second")
        XCTAssertEqual(fixture.runner.fleetVerbs.filter { $0 == "redeem" }.count, 1)
        assertEnded(fixture, second)
    }

    func testTheSecretIsWipedOnQuit() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let leg = try openAttempt(fixture)
        // The handler NSApplication.willTerminateNotification runs (posting the real notification
        // in the test host would also reach AppKit's and WebKit's own observers).
        fixture.window.fleetApplicationWillTerminate()
        assertEnded(fixture, leg)
        try assertLateCallbackIgnored(fixture, leg)
    }

    func testALocallyInvalidSecretIsClearedAndWipedAndNothingRuns() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .signIn && !window.isBusyForTests }
        window.enterFleetSecretForTests("with\"quote")
        window.continueForTests()
        XCTAssertEqual(window.errorMessageForTests,
                       "Enter the client secret of your Tesla developer application. A secret with a quote, a backslash or a control character cannot be used.")
        assertEnded(fixture, nil)
        XCTAssertEqual(window.fleetSecretsForTests.count, 1)
        XCTAssertEqual(fixture.runner.fleetVerbs, [])
    }

    func testTheRegistrationOwnsTheSecretForItsCallAndOtherCommandsClearTheField() throws {
        let fixture = try Fixture(state: .awaitingSignIn, registered: false)
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .publicKey && !window.isBusyForTests }
        // Saving the public key is a command of its own: a secret typed before it is cleared.
        window.enterFleetSecretForTests(Self.secret)
        window.presentFleetKeySavePanel = { _, _, done in done(fixture.home.appendingPathComponent("Desktop/key.pem")) }
        window.saveFleetPublicKeyForTests()
        try fixture.waitUntil { window.fleetPublicKeyForTests != nil && !window.isBusyForTests }
        XCTAssertEqual(window.fleetSecretForTests, "")
        XCTAssertFalse(window.continueEnabledForTests, "the registration needs the secret typed again")
        window.enterFleetSecretForTests(Self.secret)
        window.continueForTests()
        XCTAssertEqual(window.fleetSecretForTests, "", "cleared as the registration starts")
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
        XCTAssertEqual(window.fleetSecretsForTests.count, 1)
        XCTAssertTrue(window.fleetSecretsForTests.allSatisfy(\.isWiped), "wiped when the registration ends")
    }

    // MARK: The figures in effect (note 3)

    func testTheConsentPageStatesTheDefaultFiguresWhenNothingOverridesThem() throws {
        let fixture = try Fixture(state: .awaitingActivation)
        defer { fixture.tearDown() }
        try fixture.waitUntil { fixture.window.fleetStepForTests == .consent && !fixture.window.isBusyForTests && fixture.window.fleetConsentAnswerForTests != nil }
        let shown = fixture.labels().joined(separator: "\n")
        for figure in ["a driving car every 30 seconds", "a charging car every 2 minutes", "an awake car every 2 minutes",
                       "a car that is updating every 5 minutes", "checks a sleeping car every 10 minutes",
                       "lists your cars every hour", "$1 for every 500 vehicle-data requests ($0.002 each)",
                       "A state check or a list of your cars is billed at the same price.",
                       "when its requests reach $10.00", "around day 14 of a month"] {
            XCTAssertTrue(shown.contains(figure), figure)
        }
    }

    /// R2: the page shows exactly what `setup-fleet figures` states; the configuration file is
    /// not read for figures (it has no polling table here at all).
    func testTheConsentPageShowsTheFiguresTheServiceStates() throws {
        let fixture = try Fixture(state: .awaitingActivation, figures: FleetFlowRunner.figures {
            $0["pollingSeconds"] = ["driving": 60, "charging": 120, "updating": 600, "online": 120,
                                    "stateCheck": 900, "inventory": 3_600]
            $0["monthlyCapCents"] = 2_000
        })
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
        XCTAssertFalse(fixture.configText.contains("fleet_polling"))
        let shown = fixture.labels().joined(separator: "\n")
        for figure in ["a driving car every minute", "a charging car every 2 minutes",
                       "a car that is updating every 10 minutes", "checks a sleeping car every 15 minutes",
                       "lists your cars every hour", "when its requests reach $20.00",
                       "Collection is off: Hub polls nothing until you choose Start Collecting."] {
            XCTAssertTrue(shown.contains(figure), figure)
        }
        XCTAssertFalse(shown.contains("$10.00"), "not the default cap")
        XCTAssertFalse(shown.contains("day 14"), "the default's day is not claimed for other figures")
        XCTAssertNotNil(fixture.view(identifier: "onboarding.fleet.consent-accept"))
        let read = try XCTUnwrap(fixture.runner.calls.last { $0.verb == "figures" })
        XCTAssertEqual(read.arguments, ["--config", fixture.controller.maintenanceConfigPath.path, "setup-fleet", "figures"])
        XCTAssertNil(read.stdin)
    }

    /// A list Tesla does not bill (`state` is null) and a list price of its own each have a line.
    func testTheListPriceIsStatedAsTheServiceStatesIt() throws {
        let free = try Fixture(state: .awaitingActivation, figures: FleetFlowRunner.figures {
            $0["requestsPerDollar"] = ["data": 500, "state": NSNull()] as [String: Any]
        })
        defer { free.tearDown() }
        try free.waitUntil { free.window.fleetStepForTests == .consent && free.window.fleetConsentAnswerForTests != nil }
        var shown = free.labels().joined(separator: "\n")
        XCTAssertTrue(shown.contains("Tesla does not bill a state check or a list of your cars."), shown)
        XCTAssertFalse(shown.contains("Setting up makes a few list requests"), "no billed list is claimed")

        let own = try Fixture(state: .awaitingActivation, figures: FleetFlowRunner.figures {
            $0["requestsPerDollar"] = ["data": 500, "state": 250]
        })
        defer { own.tearDown() }
        try own.waitUntil { own.window.fleetStepForTests == .consent && own.window.fleetConsentAnswerForTests != nil }
        shown = own.labels().joined(separator: "\n")
        XCTAssertTrue(shown.contains("A state check or a list of your cars is billed at $1 for every 250 requests ($0.004 each)."), shown)
    }

    /// R2: a field the page has no line for is named, and nothing can be accepted.
    func testAFigureThePageHasNoLineForIsNamedAndNothingCanBeAccepted() throws {
        let fixture = try Fixture(state: .awaitingActivation, figures: FleetFlowRunner.figures {
            $0["pollingSeconds"] = ["driving": 30, "charging": 120, "updating": 300, "online": 120,
                                    "stateCheck": 600, "inventory": 3_600, "parked": 900]
        })
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
        let shown = fixture.labels().joined(separator: "\n")
        XCTAssertTrue(shown.contains("Hub states a figure this page cannot show (pollingSeconds.parked)"), shown)
        XCTAssertFalse(shown.contains("$10.00"), "no figure is stated")
        XCTAssertNil(fixture.view(identifier: "onboarding.fleet.consent-accept"), "no acceptance is offered")
        window.acceptFleetConsentForTests()
        XCTAssertFalse(window.continueEnabledForTests)
        window.continueForTests()
        XCTAssertNil(fixture.controller.fleetConsentRecord)
        XCTAssertNil(fixture.runner.call("activate"))
    }

    /// R2: a failed `setup-fleet figures` is shown with the service's text and offers no acceptance.
    func testAFailedFiguresVerbOffersNoAcceptance() throws {
        let fixture = try Fixture(state: .awaitingActivation)
        fixture.runner.figuresAnswer = .failure(HubActionError.commandExited(
            1, "config_invalid: Fixed text the service wrote about its configuration."))
        defer { fixture.tearDown() }
        let window = fixture.window
        try fixture.waitUntil { window.fleetStepForTests == .consent && !window.isBusyForTests && window.fleetConsentAnswerForTests != nil }
        let shown = fixture.labels().joined(separator: "\n")
        XCTAssertTrue(shown.contains("Hub could not state its figures"), shown)
        XCTAssertTrue(shown.contains("Hub's answer: Fixed text the service wrote about its configuration."), shown)
        XCTAssertFalse(shown.contains("$10.00"), "no figure is stated")
        XCTAssertNil(fixture.view(identifier: "onboarding.fleet.consent-accept"))
        window.acceptFleetConsentForTests()
        XCTAssertFalse(window.continueEnabledForTests)
        window.continueForTests()
        XCTAssertNil(fixture.controller.fleetConsentRecord)
        XCTAssertFalse(fixture.controller.fleetConsentInEffect)
    }

    /// R2: a record matches only an equal answer. The collection interval is the switch the
    /// consent governs (0 before `activate`), not a figure of the cost, and is not compared.
    func testARecordMatchesOnlyAnEqualAnswer() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
        defer { fixture.tearDown() }
        try fixture.waitUntil { fixture.window.fleetStepForTests == .signIn && !fixture.window.isBusyForTests }
        func confirmed(_ answer: String) throws -> Bool {
            fixture.runner.figuresAnswer = .success(answer)
            var outcome: Bool?
            fixture.controller.confirmFleetConsent { outcome = $0 }
            try fixture.waitUntil { outcome != nil }
            return outcome ?? false
        }
        XCTAssertTrue(try confirmed(FleetFlowRunner.figures { _ in }))
        XCTAssertTrue(try confirmed(FleetFlowRunner.figures { $0["collectionIntervalSeconds"] = 300 }))
        let polling: [String: Any] = ["driving": 31, "charging": 120, "updating": 300, "online": 120,
                                      "stateCheck": 600, "inventory": 3_600]
        let dataPrice: [String: Any] = ["data": 400, "state": 500]
        let listFree: [String: Any] = ["data": 500, "state": NSNull()]
        let changed: [(name: String, key: String, value: Any)] = [
            ("cap", "monthlyCapCents", 1_001), ("driving", "pollingSeconds", polling),
            ("data price", "requestsPerDollar", dataPrice), ("list not billed", "requestsPerDollar", listFree),
            ("unknown field", "wakeRequestsPerDollar", 500)
        ]
        for change in changed {
            XCTAssertFalse(try confirmed(FleetFlowRunner.figures { $0[change.key] = change.value }), change.name)
            XCTAssertFalse(fixture.controller.fleetConsentInEffect, change.name)
            try Self.expectRefused { fixture.controller.discoverFleetVehicles(completion: $0) }
        }
        fixture.runner.figuresAnswer = .failure(HubActionError.commandExited(1, "config_unreadable: Fixed text."))
        try Self.expectRefused { fixture.controller.activateFleetCollection(completion: $0) }
        XCTAssertEqual(fixture.runner.fleetVerbs, [], "no billed verb and no activate without an equal answer")
    }

    func testTheFiguresAreReadByNameFromTheServicesAnswer() {
        func reading(_ change: (inout [String: Any]) -> Void) -> HubFleetConsentReading {
            HubFleetConsentFigures.reading(result: .success(FleetFlowRunner.figures(change)))
        }
        XCTAssertEqual(reading { _ in }, .figures(.serviceDefaults))
        var free = HubFleetConsentFigures.serviceDefaults
        free.stateRequestsPerDollar = nil
        XCTAssertEqual(reading { $0["requestsPerDollar"] = ["data": 500, "state": NSNull()] as [String: Any] }, .figures(free))
        XCTAssertEqual(reading { $0["wakeRequestsPerDollar"] = 500 }, .unreadable(field: "wakeRequestsPerDollar"))
        XCTAssertEqual(reading { $0["provider"] = "legacy" }, .unreadable(field: "provider"))
        XCTAssertEqual(reading { $0["monthlyCapCents"] = nil }, .unreadable(field: "monthlyCapCents"))
        XCTAssertEqual(reading { $0["monthlyCapCents"] = true }, .unreadable(field: "monthlyCapCents"))
        XCTAssertEqual(reading { $0["requestsPerDollar"] = ["data": 500] }, .unreadable(field: "requestsPerDollar.state"))
        XCTAssertEqual(reading { $0["pollingSeconds"] = ["driving": 0, "charging": 120, "updating": 300, "online": 120,
                                                         "stateCheck": 600, "inventory": 3_600] },
                       .unreadable(field: "pollingSeconds.driving"))
        XCTAssertEqual(HubFleetConsentFigures.reading(result: .failure(HubActionError.commandExited(
            1, "fleet_provider_not_selected: Fixed text."))), .failed(message: "Fixed text."))
        XCTAssertEqual(HubFleetConsentFigures.reading(result: .success("not json")),
                       .failed(message: "Hub's answer about its figures could not be read."))
    }

    // MARK: Refusals

    func testAnUnknownRefusalIsShownWithTheServicesOwnMessageAndAKnownOneWithTheAppsLine() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
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
        XCTAssertEqual(window.fleetSecretForTests, "", "the refused attempt ended with its secret")
        XCTAssertFalse(window.continueEnabledForTests, "a new attempt needs the secret typed again")
        window.enterFleetSecretForTests(Self.secret)
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

    /// The refusals the built Hub really prints for every `setup-fleet` verb, written by the
    /// command-line test `setup_fleet_refusals_are_the_contract_lines` (M09-T24).
    func testEveryRefusalTheBuiltHubPrintsIsReadWithItsCode() throws {
        struct Printed: Decodable { let verb: String; let code: String; let stderr: String }
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("Fixtures/fleet-refusal-lines.json")
        let printed = try JSONDecoder().decode([Printed].self, from: Data(contentsOf: url))
        XCTAssertEqual(Set(printed.map(\.verb)), ["setup-fleet begin", "setup-fleet", "setup-fleet discover",
                                                   "setup-fleet activate", "setup-fleet register", "setup-fleet public-key"])
        for entry in printed {
            XCTAssertEqual(HubFleetRefusal.parse(entry.stderr)?.code, entry.code, "\(entry.verb): \(entry.stderr)")
            XCTAssertEqual((HubFleetRefusal.from(HubActionError.commandExited(1, entry.stderr)) as? HubFleetRefusal)?.code,
                           entry.code, "\(entry.verb): \(entry.stderr)")
        }
    }

    func testALostRedeemAnswerReadsTheStateAndContinuesFromIt() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
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
        try fixture.waitUntil { window.fleetStepForTests == .start }
        XCTAssertEqual(window.errorMessageForTests,
                       "Hub did not confirm the answer. Hub has checked where the set-up stands; continue from the step shown.")
        XCTAssertNil(fixture.runner.call("activate"))
    }

    func testAFailedDiscoveryAfterTheSignInLeadsToFindingTheCarNotANewSignIn() throws {
        let fixture = try Fixture(state: .awaitingSignIn, consented: true)
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
        try fixture.waitUntil { window.fleetStepForTests == .start && !window.isBusyForTests }
        XCTAssertEqual(fixture.runner.fleetVerbs, ["begin", "redeem", "discover"])
    }

    // MARK: Inputs and the card

    func testInputsCarryNoEscapeTheServiceRefusesAndNoTokenKey() throws {
        let application = HubFleetApplication(clientID: Self.clientID, region: .na, redirectURI: Self.redirect,
                                              domain: Self.domain)
        for bad in ["with\"quote", "with\\backslash", "with\nnewline", "with\u{7}bell", "with\u{85}nel", "with\u{7F}del", ""] {
            XCTAssertThrowsError(try HubController.fleetRegisterInput(application, clientSecret: HubFleetSecret(bad)), bad)
        }
        let secret = HubFleetSecret("ok/secret-é")
        let input = try HubController.fleetRegisterInput(application, clientSecret: secret)
        let text = input.withBytes { String(decoding: $0, as: UTF8.self) }
        input.wipe()
        XCTAssertEqual(text, #"{"clientId":"c0ffee00-1111-4222-8333-444455556666","clientSecret":"ok/secret-é","domain":"example.com","region":"na"}"#,
                       "plain UTF-8 with no escape at all")
        XCTAssertFalse(text.contains("\\"), text)
        XCTAssertFalse(text.lowercased().contains("token"))
        XCTAssertTrue(input.isWiped)
        secret.wipe()
        XCTAssertTrue(secret.isWiped)
        XCTAssertEqual(secret.withBytes { $0.count }, 0, "a wiped secret yields no bytes")
        XCTAssertThrowsError(try HubController.fleetRegisterInput(application, clientSecret: secret), "a wiped secret is not sent")
        let foreign = HubFleetApplication(clientID: "quoted\"id", region: .eu, redirectURI: Self.redirect, domain: Self.domain)
        XCTAssertThrowsError(try HubController.fleetRegisterInput(foreign, clientSecret: HubFleetSecret("ok"))) {
            XCTAssertEqual(($0 as? HubFleetRefusal)?.code, "fleet_partner_settings_invalid")
        }
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

    /// Runs one controller verb and expects the refusal for a missing consent.
    private static func expectRefused(_ verb: (@escaping (Result<Int, Error>) -> Void) -> Void,
                                      file: StaticString = #filePath, line: UInt = #line) throws {
        var outcome: Result<Int, Error>?
        verb { outcome = $0 }
        let deadline = Date().addingTimeInterval(2)
        while outcome == nil && Date() < deadline { RunLoop.main.run(until: Date().addingTimeInterval(0.01)) }
        guard case let .failure(error)? = outcome else { return XCTFail("not refused: \(String(describing: outcome))", file: file, line: line) }
        XCTAssertEqual(error.localizedDescription,
                       "Accept the cost of polling on the consent page first. Nothing was sent to Tesla.", file: file, line: line)
    }

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

    /// The answer of `setup-fleet figures`; interface section 12's example unless a test sets it.
    var figuresAnswer: Result<String, Error> = .success(FleetFlowRunner.figures { _ in })

    var calls: [Call] { lock.lock(); defer { lock.unlock() }; return recorded }
    /// The verbs that act; the read-only `figures` is left out.
    var fleetVerbs: [String] { calls.compactMap(\.verb).filter { $0 != "figures" } }

    /// Interface section 12's example answer, changed by `change`.
    static func figures(_ change: (inout [String: Any]) -> Void) -> String {
        var answer: [String: Any] = [
            "provider": "fleet", "collectionIntervalSeconds": 0,
            "pollingSeconds": ["driving": 30, "charging": 120, "updating": 300, "online": 120,
                               "stateCheck": 600, "inventory": 3_600],
            "requestsPerDollar": ["data": 500, "state": 500], "monthlyCapCents": 1_000
        ]
        change(&answer)
        return String(decoding: try! JSONSerialization.data(withJSONObject: answer, options: [.sortedKeys]), as: UTF8.self)
    }
    func call(_ verb: String) -> Call? { calls.last { $0.verb == verb } }

    static func verb(_ arguments: [String]) -> String? {
        guard let index = arguments.firstIndex(of: "setup-fleet") else { return nil }
        let next = arguments.dropFirst(index + 1).first
        if let next, ["begin", "discover", "activate", "register", "public-key", "figures"].contains(next) { return next }
        return "redeem"
    }

    private func answer(_ arguments: [String], stdin: String?, sensitive: Bool,
                        completion: @escaping (Result<String, Error>) -> Void) {
        lock.lock(); recorded.append(Call(arguments: arguments, stdin: stdin, sensitive: sensitive)); lock.unlock()
        if Self.verb(arguments) == "figures" {
            completion(figuresAnswer)
            return
        }
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
    /// What launchd reports for the installed service; a test sets `.unloaded` for a stopped one.
    var loaded: HubServiceLoadState = .loaded
    /// Answers for `service start`, first to last; then success.
    var startResults: [Result<String, Error>] = []
    /// Answers for `service pause`, first to last; then success.
    var pauseResults: [Result<String, Error>] = []
    /// Typed recoveries (a restart after a failed pause site), counted apart from `calls`.
    private(set) var recoveries = 0
    func loadedState(completion: @escaping (HubServiceLoadState) -> Void) { completion(loaded) }
    func run(arguments: [String], owner: HubLifecycleToken,
             completion: @escaping (Result<String, Error>) -> Void) {
        calls.append(arguments.joined(separator: " "))
        if arguments.last == "start", !startResults.isEmpty {
            completion(startResults.removeFirst())
            return
        }
        if arguments.last == "pause", !pauseResults.isEmpty {
            completion(pauseResults.removeFirst())
            return
        }
        if arguments.last == "start" || arguments.last == "pause" { loaded = arguments.last == "start" ? .loaded : .unloaded }
        completion(.success(""))
    }
    func recoverAfterPause(owner: HubLifecycleToken, completion: @escaping (Result<String, Error>) -> Void) {
        recoveries += 1
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
    init(state: HubFleetSetupState? = nil, config: String = "", interval: Int = 0, consented: Bool = false,
         registered: Bool = true, serviceStopped: Bool = false, pauseFails: String? = nil,
         figures: String? = nil) throws {
        home = HubTestScratch.directory.appendingPathComponent("tesatlas-fleet-flow-\(UUID().uuidString)", isDirectory: true)
        let folder = home.appendingPathComponent("Library/Application Support/Tesatlas Hub", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: home.appendingPathComponent("Desktop"), withIntermediateDirectories: true)
        let runner = FleetFlowRunner()
        let service = FleetFlowService()
        if serviceStopped { service.loaded = .unloaded }
        if let pauseFails { service.pauseResults = [.failure(HubActionError.commandFailed(pauseFails))] }
        if let figures { runner.figuresAnswer = .success(figures) }
        let controller = HubController(commandRunner: runner, serviceRunner: service, homeDirectory: home,
                                       serviceInstalledOverride: state != nil)
        if let state {
            try ("data_dir = \"/tmp/hub\"\n\n[collector]\nprovider = \"fleet\"\ninterval_seconds = \(interval)\n" + config)
                .write(to: folder.appendingPathComponent("config.toml"), atomically: true, encoding: .utf8)
            try controller.saveFleetApplication(HubFleetApplication(
                clientID: "c0ffee00-1111-4222-8333-444455556666", region: .eu,
                redirectURI: "https://example.com/tesla/callback", domain: "example.com", registered: registered))
            switch state {
            case .awaitingSignIn: runner.preflight = Self.preflight("awaiting_sign_in")
            case .awaitingVehicles: runner.preflight = Self.preflight("awaiting_vehicles")
            case .awaitingActivation: runner.preflight = Self.preflight("awaiting_activation")
            case .collecting: runner.preflight = #"{"status":"ready","version":"1","provider":"fleet"}"#
            default: break
            }
            if consented { try controller.recordFleetConsent(.serviceDefaults) }
        }
        self.runner = runner
        self.service = service
        self.controller = controller
        window = OnboardingWindowController(controller: controller, initialRoute: .fleet, onComplete: { _ in })
    }

    /// Changes the figures the service states, as an edit of its configuration would.
    func changeFigures(monthlyCapCents: Int) {
        runner.figuresAnswer = .success(FleetFlowRunner.figures { $0["monthlyCapCents"] = monthlyCapCents })
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

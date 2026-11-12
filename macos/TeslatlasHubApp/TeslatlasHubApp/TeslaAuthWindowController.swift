// SPDX-FileCopyrightText: 2021 Adrian Kumpf
// SPDX-License-Identifier: MIT

import AppKit
import CryptoKit
import Foundation
import Security
import WebKit

// OAuth parameters and callback semantics are derived from Tesla Auth v0.15.0
// (MIT), commit 68da1f850e9cb87ac0e54c608d5a2e90d3ad1608.
// Modified for Teslatlas Hub on 2026-08-22, 2026-08-25 and 2026-11-11.

/// The authorisation material of the Hub's own sign-in. The app never exchanges it and
/// never sees a token: it hands the code, its PKCE verifier and the redirect URI to
/// `teslatlas-hub setup`, and the running service performs the exchange inside an open
/// collection window.
struct TeslaAuthCodeGrant: Equatable {
    let code: String
    let codeVerifier: String
    let redirectURI: String
}

enum TeslaAuthError: LocalizedError, Equatable {
    case cancelled
    case randomGeneration
    case invalidAuthorizationURL
    case invalidCallback
    case stateMismatch
    case invalidIssuer
    // Kept for the diagnostic codes: the app itself never exchanges a code.
    case invalidResponse
    case exchangeFailed

    var errorDescription: String? {
        switch self {
        case .cancelled: return HubL10n.text("hub.TeslaAuthWindowController.30.2235", fallback: "Tesla login was cancelled.")
        case .randomGeneration: return HubL10n.text("hub.TeslaAuthWindowController.31.2236", fallback: "Secure Tesla login state could not be generated.")
        case .invalidAuthorizationURL: return HubL10n.text("hub.TeslaAuthWindowController.32.2237", fallback: "Tesla authorization URL could not be created.")
        case .invalidCallback: return HubL10n.text("hub.TeslaAuthWindowController.33.2238", fallback: "Tesla returned an incomplete login callback.")
        case .stateMismatch: return HubL10n.text("hub.TeslaAuthWindowController.34.2239", fallback: "Tesla login state did not match. Start login again.")
        case .invalidIssuer: return HubL10n.text("hub.TeslaAuthWindowController.35.2240", fallback: "Tesla returned an invalid account issuer.")
        case .invalidResponse, .exchangeFailed: return nil
        }
    }
}

enum TeslaCallbackOutcome: Equatable {
    case cancelled
    case code(TeslaAuthCodeGrant)
}

struct TeslaOAuthFlow {
    static let clientID = "ownerapi"
    static let redirectURI = "tesla://auth/callback"
    static let authorizationEndpoint = URL(string: "https://auth.tesla.com/oauth2/v3/authorize")!
    static let scopes = "openid email offline_access"

    let state: String
    let verifier: String
    let authorizationURL: URL

    init() throws {
        try self.init(state: Self.randomURLSafe(byteCount: 32),
                      verifier: Self.randomURLSafe(byteCount: 64))
    }

    init(state: String, verifier: String) throws {
        guard !state.isEmpty, (43...128).contains(verifier.count) else {
            throw TeslaAuthError.randomGeneration
        }
        self.state = state
        self.verifier = verifier
        let challenge = Self.base64URL(Data(SHA256.hash(data: Data(verifier.utf8))))
        var components = URLComponents(url: Self.authorizationEndpoint, resolvingAgainstBaseURL: false)
        components?.queryItems = [
            URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "client_id", value: Self.clientID),
            URLQueryItem(name: "redirect_uri", value: Self.redirectURI),
            URLQueryItem(name: "scope", value: Self.scopes),
            URLQueryItem(name: "state", value: state),
            URLQueryItem(name: "code_challenge", value: challenge),
            URLQueryItem(name: "code_challenge_method", value: "S256")
        ]
        guard let url = components?.url else { throw TeslaAuthError.invalidAuthorizationURL }
        authorizationURL = url
    }

    static func isCallback(_ url: URL) -> Bool {
        url.scheme?.lowercased() == "tesla"
            && url.host?.lowercased() == "auth"
            && url.path == "/callback"
    }

    func callback(_ url: URL) throws -> TeslaCallbackOutcome {
        guard Self.isCallback(url), let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            throw TeslaAuthError.invalidCallback
        }
        var query: [String: String] = [:]
        for item in components.queryItems ?? [] { query[item.name] = item.value }
        if query["error"] == "login_cancelled" { return .cancelled }
        guard let code = query["code"], !code.isEmpty,
              let callbackState = query["state"], !callbackState.isEmpty,
              let issuer = query["issuer"], !issuer.isEmpty else {
            throw TeslaAuthError.invalidCallback
        }
        guard Self.constantTimeEqual(callbackState, state) else { throw TeslaAuthError.stateMismatch }
        guard let issuerURL = URL(string: issuer), issuerURL.host != nil else {
            throw TeslaAuthError.invalidIssuer
        }
        return .code(TeslaAuthCodeGrant(code: code, codeVerifier: verifier,
                                         redirectURI: Self.redirectURI))
    }

    private static func randomURLSafe(byteCount: Int) throws -> String {
        var bytes = [UInt8](repeating: 0, count: byteCount)
        let status = bytes.withUnsafeMutableBytes { buffer in
            SecRandomCopyBytes(kSecRandomDefault, buffer.count, buffer.baseAddress!)
        }
        guard status == errSecSuccess else {
            throw TeslaAuthError.randomGeneration
        }
        return base64URL(Data(bytes))
    }

    private static func base64URL(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    private static func constantTimeEqual(_ left: String, _ right: String) -> Bool {
        let left = Array(left.utf8)
        let right = Array(right.utf8)
        var difference = UInt64(left.count ^ right.count)
        for index in 0..<max(left.count, right.count) {
            let lhs = index < left.count ? left[index] : 0
            let rhs = index < right.count ? right[index] : 0
            difference |= UInt64(lhs ^ rhs)
        }
        return difference == 0
    }
}

final class TeslaAuthWindowController: NSWindowController, NSWindowDelegate, WKNavigationDelegate {
    private let flow: TeslaOAuthFlow
    private let webView: WKWebView
    private let completion: (Result<TeslaAuthCodeGrant, Error>) -> Void
    private var finished = false
    private var callbackConsumed = false

    init(loadAuthorizationURL: Bool = true,
         completion: @escaping (Result<TeslaAuthCodeGrant, Error>) -> Void) throws {
        flow = try TeslaOAuthFlow()
        self.completion = completion
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        webView = WKWebView(frame: .zero, configuration: configuration)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 760, height: 720),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = HubL10n.text("hub.TeslaAuthWindowController.296.2280", fallback: "Connect Tesla Account")
        window.minSize = NSSize(width: 620, height: 560)
        super.init(window: window)
        window.delegate = self
        webView.navigationDelegate = self
        window.contentView = webView
        window.center()
        if loadAuthorizationURL {
            webView.load(URLRequest(url: flow.authorizationURL,
                                    cachePolicy: .reloadIgnoringLocalAndRemoteCacheData,
                                    timeoutInterval: 30))
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = navigationAction.request.url, TeslaOAuthFlow.isCallback(url) else {
            decisionHandler(.allow)
            return
        }
        decisionHandler(.cancel)
        guard !callbackConsumed else { return }
        callbackConsumed = true
        do {
            switch try flow.callback(url) {
            case .cancelled:
                finish(.failure(TeslaAuthError.cancelled))
            case let .code(grant):
                // No exchange here: the grant goes to the Hub service, which exchanges it.
                finish(.success(grant))
            }
        } catch {
            finish(.failure(error))
        }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
                 withError error: Error) {
        guard !callbackConsumed else { return }
        guard !Self.isBenignNavigationInterruption(error) else { return }
        finish(.failure(error))
    }

    static func isBenignNavigationInterruption(_ error: Error) -> Bool {
        let error = error as NSError
        return (error.domain == NSURLErrorDomain && error.code == NSURLErrorCancelled)
            || (error.domain == "WebKitErrorDomain" && error.code == 102)
    }

    func windowWillClose(_ notification: Notification) {
        guard !finished else { return }
        finish(.failure(TeslaAuthError.cancelled), closeWindow: false)
    }

    private func finish(_ result: Result<TeslaAuthCodeGrant, Error>, closeWindow: Bool = true) {
        guard !finished else { return }
        finished = true
        if closeWindow { window?.close() }
        completion(result)
    }
}

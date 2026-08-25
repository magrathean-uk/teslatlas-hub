import AppKit
import CryptoKit
import Foundation
import Security
import WebKit

// OAuth parameters and callback semantics are derived from Tesla Auth v0.15.0
// (MIT), commit 68da1f850e9cb87ac0e54c608d5a2e90d3ad1608.
struct TeslaAuthTokens {
    let accessToken: String
    let refreshToken: String
}

enum TeslaAuthError: LocalizedError, Equatable {
    case cancelled
    case randomGeneration
    case invalidAuthorizationURL
    case invalidCallback
    case stateMismatch
    case invalidIssuer
    case invalidResponse
    case exchangeFailed

    var errorDescription: String? {
        switch self {
        case .cancelled: return "Tesla login was cancelled."
        case .randomGeneration: return "Secure Tesla login state could not be generated."
        case .invalidAuthorizationURL: return "Tesla authorization URL could not be created."
        case .invalidCallback: return "Tesla returned an incomplete login callback."
        case .stateMismatch: return "Tesla login state did not match. Start login again."
        case .invalidIssuer: return "Tesla returned an invalid account issuer."
        case .invalidResponse: return "Tesla returned an invalid token response."
        case .exchangeFailed: return "Tesla token exchange failed."
        }
    }
}

struct TeslaTokenExchangeRequest: Equatable {
    let url: URL
    let body: Data
}

enum TeslaCallbackOutcome: Equatable {
    case cancelled
    case exchange(TeslaTokenExchangeRequest)
}

struct TeslaOAuthFlow {
    static let clientID = "ownerapi"
    static let redirectURI = "tesla://auth/callback"
    static let authorizationEndpoint = URL(string: "https://auth.tesla.com/oauth2/v3/authorize")!
    static let tokenEndpoint = URL(string: "https://auth.tesla.com/oauth2/v3/token")!
    static let chinaTokenEndpoint = URL(string: "https://auth.tesla.cn/oauth2/v3/token")!
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
        let tokenURL = issuerURL.host?.lowercased() == Self.chinaTokenEndpoint.host
            ? Self.chinaTokenEndpoint : Self.tokenEndpoint
        let body = Self.formBody([
            URLQueryItem(name: "grant_type", value: "authorization_code"),
            URLQueryItem(name: "code", value: code),
            URLQueryItem(name: "redirect_uri", value: Self.redirectURI),
            URLQueryItem(name: "client_id", value: Self.clientID),
            URLQueryItem(name: "code_verifier", value: verifier)
        ])
        return .exchange(TeslaTokenExchangeRequest(url: tokenURL, body: body))
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

    private static func formBody(_ items: [URLQueryItem]) -> Data {
        var components = URLComponents()
        components.queryItems = items
        return Data((components.percentEncodedQuery ?? "").utf8)
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

struct TeslaTokenResponseBuffer {
    static let maximumBytes = 64 * 1024
    private(set) var data = Data()

    mutating func append(_ chunk: Data) -> Bool {
        guard chunk.count <= Self.maximumBytes - data.count else { return false }
        data.append(chunk)
        return true
    }
}

private final class TeslaTokenExchange: NSObject, URLSessionDataDelegate {
    private var session: URLSession?
    private var completion: ((Result<TeslaAuthTokens, Error>) -> Void)?
    private var responseAccepted = false
    private var responseBody = TeslaTokenResponseBuffer()
    private var completed = false
    private let delegateQueue: OperationQueue

    override init() {
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        delegateQueue = queue
        super.init()
    }

    func start(_ exchange: TeslaTokenExchangeRequest,
               completion: @escaping (Result<TeslaAuthTokens, Error>) -> Void) {
        var request = URLRequest(url: exchange.url, timeoutInterval: 30)
        request.httpMethod = "POST"
        request.httpBody = exchange.body
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")

        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 30
        configuration.timeoutIntervalForResource = 30
        configuration.httpShouldSetCookies = false
        configuration.httpCookieAcceptPolicy = .never
        self.completion = completion
        let session = URLSession(configuration: configuration,
                                 delegate: self,
                                 delegateQueue: delegateQueue)
        self.session = session
        session.dataTask(with: request).resume()
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask,
                    didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        guard let response = response as? HTTPURLResponse,
              (200...299).contains(response.statusCode),
              response.expectedContentLength < 0 ||
                response.expectedContentLength <= Int64(TeslaTokenResponseBuffer.maximumBytes) else {
            completionHandler(.cancel)
            finish(.failure(TeslaAuthError.exchangeFailed))
            return
        }
        responseAccepted = true
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask,
                    didReceive data: Data) {
        guard !completed else { return }
        guard responseBody.append(data) else {
            dataTask.cancel()
            finish(.failure(TeslaAuthError.exchangeFailed))
            return
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    didCompleteWithError error: Error?) {
        guard !completed else { return }
        guard error == nil, responseAccepted else {
            finish(.failure(TeslaAuthError.exchangeFailed))
            return
        }
        struct TokenResponse: Decodable {
            let accessToken: String
            let refreshToken: String
            let expiresIn: Double

            enum CodingKeys: String, CodingKey {
                case accessToken = "access_token"
                case refreshToken = "refresh_token"
                case expiresIn = "expires_in"
            }
        }
        guard let token = try? JSONDecoder().decode(TokenResponse.self, from: responseBody.data),
              !token.accessToken.isEmpty, !token.refreshToken.isEmpty, token.expiresIn > 0 else {
            finish(.failure(TeslaAuthError.invalidResponse))
            return
        }
        finish(.success(TeslaAuthTokens(accessToken: token.accessToken,
                                        refreshToken: token.refreshToken)))
    }

    private func finish(_ result: Result<TeslaAuthTokens, Error>) {
        guard !completed else { return }
        completed = true
        let callback = completion
        completion = nil
        session?.finishTasksAndInvalidate()
        session = nil
        callback?(result)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

final class TeslaAuthWindowController: NSWindowController, NSWindowDelegate, WKNavigationDelegate {
    private let flow: TeslaOAuthFlow
    private let webView: WKWebView
    private let completion: (Result<TeslaAuthTokens, Error>) -> Void
    private var exchange: TeslaTokenExchange?
    private var finished = false
    private var callbackConsumed = false

    init(completion: @escaping (Result<TeslaAuthTokens, Error>) -> Void) throws {
        flow = try TeslaOAuthFlow()
        self.completion = completion
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        webView = WKWebView(frame: .zero, configuration: configuration)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 760, height: 720),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable],
                              backing: .buffered, defer: false)
        window.title = "Connect Tesla Account"
        window.minSize = NSSize(width: 620, height: 560)
        super.init(window: window)
        window.delegate = self
        webView.navigationDelegate = self
        window.contentView = webView
        window.center()
        webView.load(URLRequest(url: flow.authorizationURL,
                                cachePolicy: .reloadIgnoringLocalAndRemoteCacheData,
                                timeoutInterval: 30))
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
            case let .exchange(request):
                showProgress()
                let exchange = TeslaTokenExchange()
                self.exchange = exchange
                exchange.start(request) { [weak self] result in
                    DispatchQueue.main.async { self?.finish(result) }
                }
            }
        } catch {
            finish(.failure(error))
        }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
                 withError error: Error) {
        guard !callbackConsumed else { return }
        finish(.failure(error))
    }

    func windowWillClose(_ notification: Notification) {
        guard !finished else { return }
        finish(.failure(TeslaAuthError.cancelled), closeWindow: false)
    }

    private func showProgress() {
        webView.loadHTMLString("""
        <!doctype html><meta charset="utf-8"><style>
        body{font:15px -apple-system;margin:0;display:grid;place-items:center;height:100vh;color:#333}
        </style><p>Finishing secure Tesla login…</p>
        """, baseURL: nil)
    }

    private func finish(_ result: Result<TeslaAuthTokens, Error>, closeWindow: Bool = true) {
        guard !finished else { return }
        finished = true
        exchange = nil
        if closeWindow { window?.close() }
        completion(result)
    }
}

import Foundation
import XCTest

@testable import TeslatlasCurrentHub

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif
#if os(Linux)
  import Glibc
#endif

final class CurrentHubTransportTests: XCTestCase {
  func testTransportUsesProfileResponseBoundAndIsolatedState() {
    XCTAssertEqual(CurrentHubURLSessionTransport.defaultMaximumResponseBytes, 1_048_576)
    let configuration = CurrentHubURLSessionTransport.isolatedConfiguration()
    XCTAssertFalse(configuration.httpShouldSetCookies)
    XCTAssertNil(configuration.httpCookieStorage)
    XCTAssertNil(configuration.urlCredentialStorage)
    XCTAssertNil(configuration.urlCache)
  }

  func testExplicitTrustRejectsMalformedAnchorAndLeafPin() throws {
    XCTAssertThrowsError(
      try CurrentHubURLSessionTransport(
        trustedCertificateAuthoritiesDER: [Data("not a certificate".utf8)]
      )
    )
    XCTAssertThrowsError(
      try CurrentHubURLSessionTransport(expectedLeafCertificateSHA256: "short")
    )
  }

  func testTransportAcceptsBodyAtLimit() async throws {
    let transport = CurrentHubURLSessionTransport(
      configuration: CurrentHubURLProtocol.configuration(),
      maximumResponseBytes: 4
    )
    let response = try await transport.send(
      CurrentHubURLProtocol.request(path: "/exact", status: 200, body: Data("1234".utf8))
    )
    XCTAssertEqual(response.body, Data("1234".utf8))
  }

  func testTransportRejectsChunkBeyondLimit() async throws {
    let transport = CurrentHubURLSessionTransport(
      configuration: CurrentHubURLProtocol.configuration(),
      maximumResponseBytes: 4
    )
    do {
      _ = try await transport.send(
        CurrentHubURLProtocol.request(path: "/large", status: 200, body: Data("12345".utf8), contentLength: false)
      )
      XCTFail("Expected bounded rejection")
    } catch let error as CurrentHubError {
      XCTAssertEqual(error, .invalidResponse(statusCode: 200, requestID: nil, reason: "response body exceeds 4 bytes"))
    }
  }

  func testTransportRejectsRedirectWithoutFollowing() async throws {
    let transport = CurrentHubURLSessionTransport(
      configuration: CurrentHubURLProtocol.configuration(),
      maximumResponseBytes: 4
    )
    let response = try await transport.send(
      CurrentHubURLProtocol.request(
        path: "/redirect",
        status: 302,
        body: Data(),
        extraHeaders: ["Location": "https://attacker.invalid/"]
      )
    )
    XCTAssertEqual(response.statusCode, 302)
    XCTAssertEqual(response.finalURL.path, "/redirect")
  }

  func testTransportCancellationSurfacesCancellationError() async throws {
    let transport = CurrentHubURLSessionTransport(
      configuration: CurrentHubURLProtocol.configuration(delay: 5),
      maximumResponseBytes: 4
    )
    let task = Task {
      try await transport.send(
        CurrentHubURLProtocol.request(path: "/slow", status: 200, body: Data("1234".utf8))
      )
    }
    try await Task.sleep(for: .milliseconds(30))
    task.cancel()
    do {
      _ = try await task.value
      XCTFail("Expected cancellation")
    } catch is CancellationError {}
  }

  #if os(macOS)
  func testPublicClientMapsRealLoopback401WithoutRetryOrSecretLeak() async throws {
    let loopbackHost = "::1"
    let server = try CurrentHubTransportServer(mode: "auth-401", host: loopbackHost)
    defer { server.stop() }
    let transport = CurrentHubLoopbackTransport(serverURL: server.url)
    let client = try await CurrentHubClient.connect(
      endpoint: CurrentHubTestData.endpoint,
      expectedHubID: CurrentHubTestData.hubID,
      credentialStore: MemoryCurrentHubCredentialStore(try currentHubCredential()),
      transport: transport
    )

    do {
      _ = try await client.vehicles()
      XCTFail("Expected the real loopback 401 to be mapped")
    } catch let error as CurrentHubError {
      XCTAssertEqual(error, .unauthorized(requestID: "loopback-auth"))
      let description = String(describing: error)
      XCTAssertFalse(description.contains(CurrentHubTestData.tokenA))
      XCTAssertFalse(description.contains(String(repeating: "s", count: 64)))
    }

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 2, "discovery plus one authenticated request")
    XCTAssertEqual(requests.last?.url?.path, "/v1/vehicles")
    XCTAssertEqual(
      requests.last?.value(forHTTPHeaderField: "Authorization"),
      "Bearer \(CurrentHubTestData.tokenA)"
    )
    XCTAssertTrue(server.waitForNaturalExit(), "fixture must close after one request")
  }
  #endif

  func testPublicClientMapsLoopbackTransportFailureWithoutSecretLeak() async throws {
    let host = "[::1]"
    let transport = CurrentHubLoopbackTransport(
      serverURL: URL(string: "http://\(host):1/request")!
    )
    let client = try await CurrentHubClient.connect(
      endpoint: CurrentHubTestData.endpoint,
      expectedHubID: CurrentHubTestData.hubID,
      credentialStore: MemoryCurrentHubCredentialStore(try currentHubCredential()),
      transport: transport
    )

    do {
      _ = try await client.vehicles()
      XCTFail("Expected the unavailable loopback endpoint to fail")
    } catch let error as CurrentHubError {
      guard case .transportFailure = error else {
        return XCTFail("Expected a typed transport failure, got \(error)")
      }
      XCTAssertFalse(String(describing: error).contains(CurrentHubTestData.tokenA))
    }

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 2)
  }

  #if canImport(FoundationNetworking)
    func testLinuxDefaultTransportUsesBoundedCurlPathForBareBearerError() async throws {
      let server = try CurrentHubTransportServer(mode: "error-oversize")
      defer { server.stop() }
      do {
        _ = try await CurrentHubURLSessionTransport(maximumResponseBytes: 1_048_576)
          .send(URLRequest(url: server.url))
        XCTFail("Expected bounded error response rejection")
      } catch let error as CurrentHubError {
        XCTAssertEqual(
          error,
          .invalidResponse(
            statusCode: 401,
            requestID: nil,
            reason: "response body exceeds 1048576 bytes"
          )
        )
      }
    }

    func testLinuxDefaultTransportRejectsLargeHeadersAndRedirects() async throws {
      let headerServer = try CurrentHubTransportServer(mode: "large-header")
      defer { headerServer.stop() }
      do {
        _ = try await CurrentHubURLSessionTransport()
          .send(URLRequest(url: headerServer.url))
        XCTFail("Expected bounded header rejection")
      } catch let error as CurrentHubError {
        XCTAssertEqual(
          error,
          .invalidResponse(
            statusCode: 200,
            requestID: nil,
            reason: "response headers exceed 65536 bytes"
          )
        )
      }

      let redirectServer = try CurrentHubTransportServer(mode: "redirect")
      defer { redirectServer.stop() }
      let response = try await CurrentHubURLSessionTransport()
        .send(URLRequest(url: redirectServer.url))
      XCTAssertEqual(response.statusCode, 302)
      XCTAssertEqual(response.finalURL.path, "/request")
    }

    func testLinuxDefaultTransportCancellationBeforeAndDuringTransfer() async throws {
      let before = Task {
        withUnsafeCurrentTask { $0?.cancel() }
        return try await CurrentHubURLSessionTransport()
          .send(URLRequest(url: URL(string: "http://127.0.0.1:1/request")!))
      }
      do {
        _ = try await before.value
        XCTFail("Expected pre-start cancellation")
      } catch is CancellationError {}

      let server = try CurrentHubTransportServer(mode: "cancel-during")
      defer { server.stop() }
      let during = Task {
        try await CurrentHubURLSessionTransport()
          .send(URLRequest(url: server.url))
      }
      try await Task.sleep(for: .milliseconds(80))
      during.cancel()
      do {
        _ = try await during.value
        XCTFail("Expected in-transfer cancellation")
      } catch is CancellationError {}
    }

    func testLinuxDefaultTransportHonoursRequestTimeout() async throws {
      let server = try CurrentHubTransportServer(mode: "timeout")
      defer { server.stop() }
      var request = URLRequest(url: server.url)
      request.timeoutInterval = 0.05
      let started = ContinuousClock.now
      do {
        _ = try await CurrentHubURLSessionTransport().send(request)
        XCTFail("Expected request timeout")
      } catch let error as CurrentHubError {
        XCTAssertEqual(error, .transportFailure(code: 28))
        XCTAssertLessThan(started.duration(to: .now), .seconds(1))
      }
    }

    func testLinuxDefaultTransportConcurrentOperationLifetimes() async throws {
      let count = 12
      let server = try CurrentHubTransportServer(mode: "exact", count: count)
      defer { server.stop() }
      let bodies = try await withThrowingTaskGroup(of: Data.self) { group in
        for _ in 0..<count {
          group.addTask {
            try await CurrentHubURLSessionTransport()
              .send(URLRequest(url: server.url)).body
          }
        }
        var output: [Data] = []
        for try await body in group { output.append(body) }
        return output
      }
      XCTAssertEqual(bodies.count, count)
      XCTAssertTrue(bodies.allSatisfy { $0 == Data("1234".utf8) })
    }

    func testLinuxDefaultTransportExposesOnlyFinalHeadersAfterInformationalResponse() async throws {
      let server = try CurrentHubTransportServer(mode: "informational")
      defer { server.stop() }

      let response = try await CurrentHubURLSessionTransport()
        .send(URLRequest(url: server.url))

      XCTAssertEqual(response.statusCode, 200)
      XCTAssertEqual(response.body, Data("{}".utf8))
      XCTAssertEqual(response.header("Content-Type"), "application/json")
      XCTAssertEqual(response.header("Cache-Control"), "no-store")
      XCTAssertEqual(response.header("ETag"), CurrentHubTestData.eTag)
      XCTAssertNil(response.header("Link"))
    }

    func testLinuxDefaultTransportRejectsConflictingCaseInsensitiveSingletonHeaders() async throws {
      let server = try CurrentHubTransportServer(mode: "conflicting-singleton")
      defer { server.stop() }

      do {
        _ = try await CurrentHubURLSessionTransport()
          .send(URLRequest(url: server.url))
        XCTFail("Expected conflicting singleton rejection")
      } catch let error as CurrentHubError {
        XCTAssertEqual(
          error,
          .invalidResponse(
            statusCode: 200,
            requestID: nil,
            reason: "conflicting content-type response headers"
          )
        )
      }
    }

    func testLinuxDefaultTransportCombinesRepeatedListValuedHeaders() async throws {
      let server = try CurrentHubTransportServer(mode: "repeated-list-fields")
      defer { server.stop() }

      let response = try await CurrentHubURLSessionTransport()
        .send(URLRequest(url: server.url))

      XCTAssertEqual(response.statusCode, 401)
      XCTAssertEqual(
        response.header("WWW-Authenticate"),
        #"Bearer realm="hub", Basic realm="fallback""#
      )
      XCTAssertEqual(response.header("Cache-Control"), "private, no-store")
    }

    func testLinuxPublicClientMapsRepeatedAuthenticationChallengesToUnauthorized() async throws {
      let server = try CurrentHubTransportServer(mode: "repeated-list-fields")
      defer { server.stop() }
      let transport = CurrentHubLoopbackTransport(serverURL: server.url)
      let client = try await CurrentHubClient.connect(
        endpoint: CurrentHubTestData.endpoint,
        expectedHubID: CurrentHubTestData.hubID,
        credentialStore: MemoryCurrentHubCredentialStore(try currentHubCredential()),
        transport: transport
      )

      await assertCurrentHubError(try await client.vehicles()) { error in
        XCTAssertEqual(error, .unauthorized(requestID: "repeated-auth"))
      }
    }

    func testLinuxDefaultTransportDoesNotPromoteTrailersToResponseHeaders() async throws {
      let server = try CurrentHubTransportServer(mode: "trailers")
      defer { server.stop() }

      let response = try await CurrentHubURLSessionTransport()
        .send(URLRequest(url: server.url))

      XCTAssertEqual(response.statusCode, 200)
      XCTAssertEqual(response.body, Data("{}".utf8))
      XCTAssertEqual(response.header("ETag"), CurrentHubTestData.eTag)
      XCTAssertNil(response.header("X-Trailer-Only"))
    }

    #if os(Linux)
    func testLinuxDefaultTransportUsesSSL_CERT_FILEForRealOpenSSLTrust() async throws {
      let server = try CurrentHubTLSTestServer()
      defer { server.stop() }

      let previous = getenv("SSL_CERT_FILE").map { String(cString: $0) }
      defer {
        if let previous {
          _ = setenv("SSL_CERT_FILE", previous, 1)
        } else {
          _ = unsetenv("SSL_CERT_FILE")
        }
      }
      _ = setenv("SSL_CERT_FILE", server.certificatePath, 1)

      var request = URLRequest(url: server.url)
      request.timeoutInterval = 5
      let response = try await CurrentHubURLSessionTransport()
        .send(request)

      XCTAssertEqual(response.statusCode, 200)
      XCTAssertEqual(response.body, Data("{}".utf8))
    }
    #endif
  #endif
}

#if os(macOS) || os(Linux)
private final class CurrentHubTransportServer: @unchecked Sendable {
    let process = Process()
    let url: URL

    init(mode: String, count: Int = 1, host: String = "127.0.0.1") throws {
      guard let script = Bundle.module.url(
        forResource: "transport_server",
        withExtension: "py",
        subdirectory: "Fixtures"
      ) else { throw CurrentHubError.invalidRequest("transport test server resource is missing") }
      let output = Pipe()
      process.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
      process.arguments = [script.path, mode, String(count), host]
      process.standardOutput = output
      process.standardError = Pipe()
      try process.run()
      let data = output.fileHandleForReading.availableData
      let urlHost = host.contains(":") ? "[\(host)]" : host
      guard let line = String(data: data, encoding: .utf8)?.split(separator: "\n").first,
        let port = Int(line),
        let url = URL(string: "http://\(urlHost):\(port)/request")
      else {
        process.terminate()
        throw CurrentHubError.invalidRequest("transport test server did not publish a port")
      }
      self.url = url
    }

    func stop() {
      if process.isRunning { process.terminate() }
      process.waitUntilExit()
    }

    /// Returns whether the fixture exited on its own after closing its listener
    /// and accepted connection. A bounded poll keeps a broken request path from
    /// turning this cleanup assertion into an unbounded test hang.
    func waitForNaturalExit(timeout: TimeInterval = 1) -> Bool {
      let deadline = Date().addingTimeInterval(timeout)
      while process.isRunning && Date() < deadline {
        Thread.sleep(forTimeInterval: 0.01)
      }
      return !process.isRunning && process.terminationStatus == 0
    }

    deinit {
      if process.isRunning { process.terminate() }
    }
}
#endif

#if os(Linux)
private final class CurrentHubTLSTestServer: @unchecked Sendable {
  let process = Process()
  let url: URL
  let certificatePath: String
  private let directory: URL

  init() throws {
    let fileManager = FileManager.default
    directory = fileManager.temporaryDirectory
      .appendingPathComponent("teslatlas-current-hub-tls-\(UUID().uuidString)", isDirectory: true)
    try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)

    let certificateURL = directory.appendingPathComponent("server.pem")
    let keyURL = directory.appendingPathComponent("server.key")
    certificatePath = certificateURL.path
    try Self.createCertificate(certificateURL: certificateURL, keyURL: keyURL)

    guard let script = Bundle.module.url(
      forResource: "tls_server",
      withExtension: "py",
      subdirectory: "Fixtures"
    ) else {
      throw CurrentHubError.invalidRequest("TLS transport test server resource is missing")
    }

    let output = Pipe()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
    process.arguments = [script.path, certificateURL.path, keyURL.path]
    process.standardOutput = output
    process.standardError = Pipe()
    try process.run()

    let data = output.fileHandleForReading.availableData
    guard let line = String(data: data, encoding: .utf8)?.split(separator: "\n").first,
      let port = Int(line),
      let url = URL(string: "https://127.0.0.1:\(port)/request")
    else {
      process.terminate()
      process.waitUntilExit()
      throw CurrentHubError.invalidRequest("TLS transport test server did not publish a port")
    }
    self.url = url
  }

  func stop() {
    guard process.isRunning else {
      try? FileManager.default.removeItem(at: directory)
      return
    }
    process.terminate()
    let deadline = Date().addingTimeInterval(0.5)
    while process.isRunning && Date() < deadline {
      Thread.sleep(forTimeInterval: 0.01)
    }
    if process.isRunning {
      _ = Glibc.kill(process.processIdentifier, SIGKILL)
    }
    process.waitUntilExit()
    try? FileManager.default.removeItem(at: directory)
  }

  deinit { stop() }

  private static func createCertificate(certificateURL: URL, keyURL: URL) throws {
    let process = Process()
    let error = Pipe()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/openssl")
    process.arguments = [
      "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256", "-days", "1",
      "-keyout", keyURL.path,
      "-out", certificateURL.path,
      "-subj", "/CN=127.0.0.1",
      "-addext", "subjectAltName=IP:127.0.0.1",
    ]
    process.standardOutput = Pipe()
    process.standardError = error
    try process.run()
    process.waitUntilExit()
    guard process.terminationStatus == 0 else {
      let message = String(
        data: error.fileHandleForReading.readDataToEndOfFile(),
        encoding: .utf8
      ) ?? "unknown openssl failure"
      throw CurrentHubError.invalidRequest("TLS certificate generation failed: \(message)")
    }
  }
}
#endif

private actor CurrentHubLoopbackTransport: CurrentHubHTTPTransport {
    private let serverURL: URL
    private let production = CurrentHubURLSessionTransport()
    private var capturedRequests: [URLRequest] = []

    init(serverURL: URL) { self.serverURL = serverURL }

    func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse {
      capturedRequests.append(request)
      if request.url?.path == "/.well-known/teslatlas-hub" {
        return CurrentHubHTTPResponse(
          statusCode: 200,
          headers: ["Content-Type": "application/json"],
          body: try CurrentHubTestData.fixture("discovery"),
          finalURL: try XCTUnwrap(request.url)
        )
      }
      var loopbackRequest = request
      loopbackRequest.url = serverURL
      let response = try await production.send(loopbackRequest)
      return CurrentHubHTTPResponse(
        statusCode: response.statusCode,
        headers: response.headers,
        body: response.body,
        finalURL: try XCTUnwrap(request.url)
      )
    }

    func requests() -> [URLRequest] { capturedRequests }
}

private final class CurrentHubURLProtocol: URLProtocol, @unchecked Sendable {
  private static let lock = NSLock()
  private nonisolated(unsafe) static var responseDelay: TimeInterval = 0

  static func configuration(delay: TimeInterval = 0) -> URLSessionConfiguration {
    lock.withLock { responseDelay = delay }
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [CurrentHubURLProtocol.self]
    return configuration
  }

  static func request(
    path: String,
    status: Int,
    body: Data,
    contentLength: Bool = true,
    extraHeaders: [String: String] = [:]
  ) -> URLRequest {
    var request = URLRequest(url: URL(string: "https://hub.invalid\(path)")!)
    request.setValue(String(status), forHTTPHeaderField: "X-Stub-Status")
    request.setValue(body.base64EncodedString(), forHTTPHeaderField: "X-Stub-Body")
    request.setValue(contentLength ? "1" : "0", forHTTPHeaderField: "X-Stub-Length")
    for (name, value) in extraHeaders {
      request.setValue(value, forHTTPHeaderField: "X-Stub-\(name)")
    }
    return request
  }

  override class func canInit(with request: URLRequest) -> Bool { true }
  override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

  override func startLoading() {
    let work: @Sendable () -> Void = { [weak self] in self?.respond() }
    let delay = Self.lock.withLock { Self.responseDelay }
    if delay > 0 {
      DispatchQueue.global().asyncAfter(deadline: .now() + delay, execute: work)
    } else { work() }
  }

  override func stopLoading() {}

  private func respond() {
    guard let url = request.url,
      let status = request.value(forHTTPHeaderField: "X-Stub-Status").flatMap(Int.init),
      let bodyText = request.value(forHTTPHeaderField: "X-Stub-Body"),
      let body = Data(base64Encoded: bodyText)
    else { return }
    var headers: [String: String] = [:]
    if request.value(forHTTPHeaderField: "X-Stub-Length") == "1" {
      headers["Content-Length"] = String(body.count)
    }
    if let location = request.value(forHTTPHeaderField: "X-Stub-Location") {
      headers["Location"] = location
    }
    let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
    client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
    if !body.isEmpty { client?.urlProtocol(self, didLoad: body) }
    client?.urlProtocolDidFinishLoading(self)
  }
}

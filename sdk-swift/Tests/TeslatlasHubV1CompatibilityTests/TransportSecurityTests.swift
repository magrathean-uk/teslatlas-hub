import Foundation
import XCTest

@testable import TeslatlasHubV1Compatibility

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class HubV1TransportSecurityTests: XCTestCase {
  func testBoundedAccumulatorRejectsChunkBeforeStoringPastLimit() {
    var accumulator = HubV1BoundedBodyAccumulator(maximumBytes: 4)

    XCTAssertTrue(accumulator.append(Data("12".utf8)))
    XCTAssertTrue(accumulator.append(Data("34".utf8)))
    XCTAssertFalse(accumulator.append(Data("5".utf8)))
    XCTAssertEqual(accumulator.data, Data("1234".utf8))
  }

  func testDefaultConfigurationCarriesNoAmbientCredentialsOrCookies() {
    let configuration = HubV1URLSessionTransport.isolatedConfiguration()

    XCTAssertNil(configuration.identifier)
    XCTAssertFalse(configuration.httpShouldSetCookies)
    XCTAssertNil(configuration.httpCookieStorage)
    XCTAssertNil(configuration.urlCredentialStorage)
    XCTAssertNil(configuration.urlCache)
    XCTAssertEqual(
      configuration.requestCachePolicy,
      .reloadIgnoringLocalCacheData
    )
  }

  func testRedirectDelegateRejectsEveryRedirect() throws {
    let configuration = URLSessionConfiguration.ephemeral
    let session = URLSession(configuration: configuration)
    let task = session.dataTask(
      with: try XCTUnwrap(URL(string: "https://hub.example.invalid/start"))
    )
    let response = try XCTUnwrap(
      HTTPURLResponse(
        url: try XCTUnwrap(URL(string: "https://hub.example.invalid/start")),
        statusCode: 302,
        httpVersion: "HTTP/1.1",
        headerFields: ["Location": "https://attacker.example.invalid/"]
      )
    )
    let redirected = URLRequest(
      url: try XCTUnwrap(URL(string: "https://attacker.example.invalid/"))
    )
    let box = RedirectResultBox()

    HubV1RejectRedirectsDelegate().urlSession(
      session,
      task: task,
      willPerformHTTPRedirection: response,
      newRequest: redirected
    ) { request in
      box.store(request)
    }

    XCTAssertTrue(box.wasCalled)
    XCTAssertNil(box.request)
    session.invalidateAndCancel()
  }

  func testEntityTagRejectsWeakOrNonHubFormats() {
    XCTAssertThrowsError(try HubV1EntityTag(rawValue: "W/\"abc\""))
    XCTAssertThrowsError(try HubV1EntityTag(rawValue: "\"abc\""))
    XCTAssertThrowsError(
      try HubV1EntityTag(
        rawValue: "\"0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef\""
      )
    )
  }

  func testTransportRejectsResponseAboveConfiguredLimit() async throws {
    let configuration = HubV1StubURLProtocol.configuration()
    let transport = HubV1URLSessionTransport(
      configuration: configuration,
      maximumResponseBytes: 4
    )
    let request = HubV1StubURLProtocol.request(
      url: HubV1TestData.discoveryURL,
      statusCode: 200,
      body: Data("12345".utf8)
    )

    await assertThrowsHubV1Error(try await transport.send(request)) { error in
      XCTAssertEqual(
        error,
        .invalidResponse(
          statusCode: 200,
          requestID: nil,
          reason: "response body exceeds 4 bytes"
        )
      )
    }
  }
}

private final class RedirectResultBox: @unchecked Sendable {
  private let lock = NSLock()
  private var storedWasCalled = false
  private var storedRequest: URLRequest?

  var wasCalled: Bool {
    lock.withLock { storedWasCalled }
  }

  var request: URLRequest? {
    lock.withLock { storedRequest }
  }

  func store(_ request: URLRequest?) {
    lock.withLock {
      storedWasCalled = true
      storedRequest = request
    }
  }
}

private final class HubV1StubURLProtocol: URLProtocol {
  private static let statusHeader = "X-Test-Status"
  private static let bodyHeader = "X-Test-Body"

  static func configuration() -> URLSessionConfiguration {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [HubV1StubURLProtocol.self]
    return configuration
  }

  static func request(url: URL, statusCode: Int, body: Data) -> URLRequest {
    var request = URLRequest(url: url)
    request.setValue(String(statusCode), forHTTPHeaderField: statusHeader)
    request.setValue(body.base64EncodedString(), forHTTPHeaderField: bodyHeader)
    return request
  }

  override class func canInit(with request: URLRequest) -> Bool { true }

  override class func canonicalRequest(for request: URLRequest) -> URLRequest {
    request
  }

  override func startLoading() {
    guard let url = request.url,
      let status = request.value(forHTTPHeaderField: Self.statusHeader).flatMap(Int.init),
      let encoded = request.value(forHTTPHeaderField: Self.bodyHeader),
      let body = Data(base64Encoded: encoded),
      let response = HTTPURLResponse(
        url: url,
        statusCode: status,
        httpVersion: "HTTP/1.1",
        headerFields: [
          "Content-Type": "application/json",
          "Content-Length": String(body.count),
        ]
      )
    else {
      client?.urlProtocol(self, didFailWithError: TestSupportError.noResponse)
      return
    }

    client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
    client?.urlProtocol(self, didLoad: body)
    client?.urlProtocolDidFinishLoading(self)
  }

  override func stopLoading() {}
}

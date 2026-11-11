import Foundation
import XCTest

@testable import TeslatlasHubSDK

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class TransportSecurityTests: XCTestCase {
  func testBoundedAccumulatorRejectsChunkBeforeStoringPastLimit() {
    var accumulator = TeslatlasBoundedBodyAccumulator(maximumBytes: 4)

    XCTAssertTrue(accumulator.append(Data("12".utf8)))
    XCTAssertTrue(accumulator.append(Data("34".utf8)))
    XCTAssertFalse(accumulator.append(Data("5".utf8)))
    XCTAssertEqual(accumulator.data, Data("1234".utf8))
  }

  func testDefaultSessionConfigurationDoesNotPersistAmbientState() {
    let configuration = URLSessionTeslatlasTransport.isolatedConfiguration()

    XCTAssertEqual(configuration.identifier, nil)
    XCTAssertEqual(configuration.httpShouldSetCookies, false)
    XCTAssertNil(configuration.httpCookieStorage)
    XCTAssertNil(configuration.urlCredentialStorage)
    XCTAssertNil(configuration.urlCache)
    XCTAssertEqual(
      configuration.requestCachePolicy,
      .reloadIgnoringLocalCacheData
    )
  }

  func testRedirectDelegateRejectsEveryRedirect() throws {
    let session = URLSession(configuration: .ephemeral)
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
    let box = TeslatlasRedirectResultBox()

    TeslatlasRejectRedirectsDelegate().urlSession(
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

  func testTransportRejectsResponseBodyAboveConfiguredLimit() async throws {
    let session = URLSession(
      configuration: StubURLProtocol.configuration()
    )
    let transport = URLSessionTeslatlasTransport(
      session: session,
      maximumResponseBytes: 4
    )

    do {
      _ = try await transport.send(
        StubURLProtocol.request(
          url: try XCTUnwrap(URL(string: "https://hub.invalid/v1")),
          statusCode: 200,
          body: Data("12345".utf8),
          includesContentLength: false
        )
      )
      XCTFail("Expected oversized response rejection")
    } catch let error as TeslatlasSDKError {
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

  func testTransportAcceptsResponseBodyAtConfiguredLimit() async throws {
    let session = URLSession(
      configuration: StubURLProtocol.configuration()
    )
    let transport = URLSessionTeslatlasTransport(
      session: session,
      maximumResponseBytes: 4
    )

    let response = try await transport.send(
      StubURLProtocol.request(
        url: try XCTUnwrap(URL(string: "https://hub.invalid/v1")),
        statusCode: 200,
        body: Data("1234".utf8)
      )
    )

    XCTAssertEqual(response.body, Data("1234".utf8))
  }
}

private final class TeslatlasRedirectResultBox: @unchecked Sendable {
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

private final class StubURLProtocol: URLProtocol {
  private static let responseHeader = "X-Teslatlas-Stub-Response"
  private static let bodyHeader = "X-Teslatlas-Stub-Body"
  private static let includesLengthHeader = "X-Teslatlas-Stub-Includes-Length"

  static func configuration() -> URLSessionConfiguration {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [StubURLProtocol.self]
    return configuration
  }

  static func request(
    url: URL,
    statusCode: Int,
    body: Data,
    includesContentLength: Bool = true
  ) -> URLRequest {
    var request = URLRequest(url: url)
    request.setValue(String(statusCode), forHTTPHeaderField: responseHeader)
    request.setValue(body.base64EncodedString(), forHTTPHeaderField: bodyHeader)
    request.setValue(
      String(includesContentLength),
      forHTTPHeaderField: includesLengthHeader
    )
    return request
  }

  override class func canInit(with request: URLRequest) -> Bool { true }

  override class func canonicalRequest(for request: URLRequest) -> URLRequest {
    request
  }

  override func startLoading() {
    guard
      let url = request.url,
      let statusText = request.value(forHTTPHeaderField: Self.responseHeader),
      let statusCode = Int(statusText),
      let bodyText = request.value(forHTTPHeaderField: Self.bodyHeader),
      let body = Data(base64Encoded: bodyText),
      let includesContentLengthText = request.value(
        forHTTPHeaderField: Self.includesLengthHeader
      )
    else {
      client?.urlProtocol(self, didFailWithError: StubError.invalidRequest)
      return
    }
    let includesContentLength = includesContentLengthText == "true"
    guard
      let response = HTTPURLResponse(
        url: url,
        statusCode: statusCode,
        httpVersion: "HTTP/1.1",
        headerFields: includesContentLength
          ? ["Content-Length": String(body.count)]
          : [:]
      )
    else {
      client?.urlProtocol(self, didFailWithError: StubError.invalidRequest)
      return
    }
    client?.urlProtocol(
      self,
      didReceive: response,
      cacheStoragePolicy: .notAllowed
    )
    client?.urlProtocol(self, didLoad: body)
    client?.urlProtocolDidFinishLoading(self)
  }

  override func stopLoading() {}
}

private enum StubError: Error {
  case invalidRequest
}

import Foundation
import XCTest

@testable import TeslatlasHubSDK

final class TransportSecurityTests: XCTestCase {
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

  func testTransportRejectsResponseBodyAboveConfiguredLimit() async throws {
    let session = URLSession(
      configuration: StubURLProtocol.configuration(
        statusCode: 200,
        body: Data("12345".utf8),
        includesContentLength: false
      )
    )
    let transport = URLSessionTeslatlasTransport(
      session: session,
      maximumResponseBytes: 4
    )

    do {
      _ = try await transport.send(
        URLRequest(url: try XCTUnwrap(URL(string: "https://hub.invalid/v1")))
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
      configuration: StubURLProtocol.configuration(
        statusCode: 200,
        body: Data("1234".utf8)
      )
    )
    let transport = URLSessionTeslatlasTransport(
      session: session,
      maximumResponseBytes: 4
    )

    let response = try await transport.send(
      URLRequest(url: try XCTUnwrap(URL(string: "https://hub.invalid/v1")))
    )

    XCTAssertEqual(response.body, Data("1234".utf8))
  }
}

private final class StubURLProtocol: URLProtocol, @unchecked Sendable {
  private static let responseHeader = "X-Teslatlas-Stub-Response"
  private static let bodyHeader = "X-Teslatlas-Stub-Body"

  static func configuration(
    statusCode: Int,
    body: Data,
    includesContentLength: Bool = true
  )
    -> URLSessionConfiguration
  {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [StubURLProtocol.self]
    configuration.httpAdditionalHeaders = [
      responseHeader: String(statusCode),
      bodyHeader: body.base64EncodedString(),
      "X-Teslatlas-Stub-Includes-Length": String(includesContentLength),
    ]
    return configuration
  }

  override class func canInit(with request: URLRequest) -> Bool { true }

  override class func canonicalRequest(for request: URLRequest) -> URLRequest {
    request
  }

  override func startLoading() {
    guard
      let url = request.url,
      let headers = request.allHTTPHeaderFields,
      let statusText = headers[Self.responseHeader],
      let statusCode = Int(statusText),
      let bodyText = headers[Self.bodyHeader],
      let body = Data(base64Encoded: bodyText),
      let includesContentLengthText = headers[
        "X-Teslatlas-Stub-Includes-Length"
      ]
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

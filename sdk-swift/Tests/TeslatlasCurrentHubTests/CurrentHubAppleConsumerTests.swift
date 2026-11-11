import Foundation
import XCTest

@testable import TeslatlasCurrentHub

#if canImport(Darwin)

/// Exercises the public current-Hub client through Apple's URLSession path.
/// The protocol fixture keeps this test deterministic and simulator-safe; an
/// owner-provided Hub remains a separate live acceptance lane.
final class CurrentHubAppleConsumerTests: XCTestCase {
  func testAppleURLSessionClientLifecycleRunsOnNativeRuntime() async throws {
    let eTag = CurrentHubTestData.eTag
    AppleConsumerURLProtocol.install([
      AppleConsumerResponse(
        path: "/.well-known/teslatlas-hub",
        body: try CurrentHubTestData.fixture("discovery")
      ),
      AppleConsumerResponse(
        path: "/healthz", body: try CurrentHubTestData.fixture("health")
      ),
      AppleConsumerResponse(
        path: "/readyz", body: try CurrentHubTestData.fixture("ready")
      ),
      AppleConsumerResponse(
        path: "/v1/vehicles", body: try CurrentHubTestData.fixture("vehicles")
      ),
      AppleConsumerResponse(
        path: "/v1/vehicles/\(CurrentHubTestData.vehicleID.uuidString.lowercased())/current",
        body: try CurrentHubTestData.fixture("current")
      ),
      AppleConsumerResponse(
        path: "/v1/vehicles/\(CurrentHubTestData.vehicleID.uuidString.lowercased())/drives",
        headers: [
          "Content-Type": "application/json",
          "Cache-Control": "no-store",
          "ETag": eTag,
        ],
        body: try CurrentHubTestData.fixture("drives")
      ),
    ])
    defer { AppleConsumerURLProtocol.reset() }

    let transport = CurrentHubURLSessionTransport(
      configuration: AppleConsumerURLProtocol.configuration()
    )
    let store = MemoryCurrentHubCredentialStore(try currentHubCredential())
    let client = try await CurrentHubClient.connect(
      endpoint: CurrentHubTestData.endpoint,
      expectedHubID: CurrentHubTestData.hubID,
      credentialStore: store,
      transport: transport
    )

    let discovery = await client.discoveryDocument()
    let health = try await client.health()
    let readiness = try await client.readiness()
    XCTAssertEqual(discovery.hubID, CurrentHubTestData.hubID)
    XCTAssertEqual(health.status, "ok")
    XCTAssertEqual(readiness.status, "ready")
    let vehicles = try await client.vehicles()
    XCTAssertEqual(vehicles.count, 1)
    let vehicleID = try XCTUnwrap(vehicles.first?.vehicleID)
    let current = try await client.current(vehicleID: vehicleID)
    XCTAssertEqual(current.vehicleID, vehicleID)
    guard case .modified(let page, let receivedETag) = try await client.drives(
      vehicleID: vehicleID,
      query: CurrentHubDriveQuery(limit: 2)
    ) else {
      return XCTFail("Expected a modified drive page")
    }
    XCTAssertEqual(page.items.count, 1)
    XCTAssertEqual(receivedETag.rawValue, eTag)
    let requests = AppleConsumerURLProtocol.requestSummaries()
    guard requests.count == 6 else {
      return XCTFail("Expected six URLSession requests, got \(requests.count)")
    }
    XCTAssertEqual(
      requests.map(\.path),
      [
        "/.well-known/teslatlas-hub",
        "/healthz",
        "/readyz",
        "/v1/vehicles",
        "/v1/vehicles/\(CurrentHubTestData.vehicleID.uuidString.lowercased())/current",
        "/v1/vehicles/\(CurrentHubTestData.vehicleID.uuidString.lowercased())/drives",
      ]
    )
    XCTAssertEqual(requests.map(\.method), Array(repeating: "GET", count: 6))
    XCTAssertEqual(
      requests.map(\.authorizationPresent),
      [
        false, false, false, true, true, true,
      ]
    )
    XCTAssertEqual(
      requests.map(\.authorizationLength),
      [nil, nil, nil, 71, 71, 71]
    )
    XCTAssertTrue(requests[5].query?.contains("limit=2") == true)
  }
}

private struct AppleConsumerRequestSummary: Equatable, Sendable {
  let method: String
  let path: String
  let query: String?
  let authorizationPresent: Bool
  let authorizationLength: Int?
}

private struct AppleConsumerResponse: Sendable {
  let path: String
  let statusCode: Int
  let headers: [String: String]
  let body: Data

  init(
    path: String,
    statusCode: Int = 200,
    headers: [String: String] = ["Content-Type": "application/json"],
    body: Data
  ) {
    self.path = path
    self.statusCode = statusCode
    self.headers = headers
    self.body = body
  }
}

private final class AppleConsumerURLProtocol: URLProtocol, @unchecked Sendable {
  private static let lock = NSLock()
  private nonisolated(unsafe) static var responses: [AppleConsumerResponse] = []
  private nonisolated(unsafe) static var requests: [AppleConsumerRequestSummary] = []

  static func configuration() -> URLSessionConfiguration {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.httpShouldSetCookies = false
    configuration.httpCookieStorage = nil
    configuration.urlCredentialStorage = nil
    configuration.urlCache = nil
    configuration.protocolClasses = [Self.self]
    return configuration
  }

  static func install(_ values: [AppleConsumerResponse]) {
    lock.withLock {
      responses = values
      requests.removeAll()
    }
  }

  static func reset() {
    lock.withLock {
      responses.removeAll()
      requests.removeAll()
    }
  }

  static func requestSummaries() -> [AppleConsumerRequestSummary] {
    lock.withLock { requests }
  }

  override class func canInit(with request: URLRequest) -> Bool {
    guard let url = request.url else { return false }
    return url.scheme == CurrentHubTestData.endpoint.scheme
      && url.host == CurrentHubTestData.endpoint.host
      && Self.expectedPaths.contains(url.path)
  }

  override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

  override func startLoading() {
    let response: AppleConsumerResponse? = Self.lock.withLock {
      Self.requests.append(
        AppleConsumerRequestSummary(
          method: request.httpMethod ?? "",
          path: request.url?.path ?? "",
          query: request.url?.query,
          authorizationPresent: request.value(forHTTPHeaderField: "Authorization") != nil,
          authorizationLength: request.value(forHTTPHeaderField: "Authorization")?.utf8.count
        )
      )
      guard !Self.responses.isEmpty else { return nil }
      return Self.responses.removeFirst()
    }
    guard let response, let url = request.url else {
      client?.urlProtocol(
        self,
        didFailWithError: CurrentHubError.invalidResponse(
          statusCode: 0, requestID: nil, reason: "Apple URLProtocol fixture exhausted"
        )
      )
      return
    }
    guard response.path == url.path else {
      client?.urlProtocol(
        self,
        didFailWithError: CurrentHubError.invalidResponse(
          statusCode: 0, requestID: nil,
          reason: "Apple URLProtocol fixture route mismatch"
        )
      )
      return
    }
    let http = HTTPURLResponse(
      url: url,
      statusCode: response.statusCode,
      httpVersion: "HTTP/1.1",
      headerFields: response.headers
    )!
    client?.urlProtocol(self, didReceive: http, cacheStoragePolicy: .notAllowed)
    if !response.body.isEmpty {
      client?.urlProtocol(self, didLoad: response.body)
    }
    client?.urlProtocolDidFinishLoading(self)
  }

  override func stopLoading() {}

  private static let expectedPaths: Set<String> = [
    "/.well-known/teslatlas-hub", "/healthz", "/readyz", "/v1/vehicles",
    "/v1/vehicles/\(CurrentHubTestData.vehicleID.uuidString.lowercased())/current",
    "/v1/vehicles/\(CurrentHubTestData.vehicleID.uuidString.lowercased())/drives",
  ]
}

#endif

import Foundation
import XCTest

@testable import TeslatlasHubV1Compatibility

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

enum HubV1TestData {
  static let discoveryURL = URL(
    string: "https://hub.example.invalid/.well-known/teslatlas-hub"
  )!
  static let expectedHubID = UUID(
    uuidString: "018f18d2-6f45-7b3c-8a91-3c7286a10d42"
  )!
  static let vehicleID = UUID(
    uuidString: "11111111-2222-4333-8444-555555555555"
  )!
  static let bearer =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  static let strongETag =
    "\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\""

  static func fixture(_ name: String, extension fileExtension: String = "json") throws
    -> Data
  {
    guard
      let url = Bundle.module.url(
        forResource: name,
        withExtension: fileExtension,
        subdirectory: "Fixtures"
      )
    else {
      throw TestSupportError.missingFixture(name)
    }
    return try Data(contentsOf: url)
  }

  static func jsonObject(_ fixture: String) throws -> [String: Any] {
    guard
      let object = try JSONSerialization.jsonObject(
        with: try self.fixture(fixture)
      ) as? [String: Any]
    else {
      throw TestSupportError.invalidFixture(fixture)
    }
    return object
  }

  static func encoded(_ object: [String: Any]) throws -> Data {
    try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
  }
}

struct HubV1StubResponse: Sendable {
  let statusCode: Int
  let headers: [String: String]
  let body: Data
  let finalURL: URL?

  init(
    statusCode: Int = 200,
    headers: [String: String] = ["Content-Type": "application/json"],
    body: Data,
    finalURL: URL? = nil
  ) {
    self.statusCode = statusCode
    self.headers = headers
    self.body = body
    self.finalURL = finalURL
  }
}

actor ScriptedHubV1Transport: HubV1HTTPTransport {
  private var responses: [HubV1StubResponse]
  private var capturedRequests: [URLRequest] = []

  init(_ responses: [HubV1StubResponse]) {
    self.responses = responses
  }

  func send(_ request: URLRequest) async throws -> HubV1HTTPResponse {
    capturedRequests.append(request)
    guard !responses.isEmpty, let requestURL = request.url else {
      throw TestSupportError.noResponse
    }
    let stub = responses.removeFirst()
    return HubV1HTTPResponse(
      statusCode: stub.statusCode,
      headers: stub.headers,
      body: stub.body,
      finalURL: stub.finalURL ?? requestURL
    )
  }

  func requests() -> [URLRequest] {
    capturedRequests
  }
}

enum TestSupportError: Error {
  case missingFixture(String)
  case invalidFixture(String)
  case noResponse
}

func makeHubV1Client(
  discoveryFixture: String = "discovery-full",
  additionalResponses: [HubV1StubResponse] = []
) async throws -> (HubV1Client, ScriptedHubV1Transport) {
  let transport = ScriptedHubV1Transport(
    [HubV1StubResponse(body: try HubV1TestData.fixture(discoveryFixture))]
      + additionalResponses
  )
  let client = try await HubV1Client.connectForTesting(
    discoveryURL: HubV1TestData.discoveryURL,
    expectedHubID: HubV1TestData.expectedHubID,
    credential: try HubV1BearerCredential(HubV1TestData.bearer),
    transport: transport
  )
  return (client, transport)
}

func assertThrowsHubV1Error<T>(
  _ expression: @autoclosure () async throws -> T,
  file: StaticString = #filePath,
  line: UInt = #line,
  _ verify: (HubV1Error) -> Void
) async {
  do {
    _ = try await expression()
    XCTFail("Expected HubV1Error", file: file, line: line)
  } catch let error as HubV1Error {
    verify(error)
  } catch {
    XCTFail("Unexpected error: \(type(of: error))", file: file, line: line)
  }
}

import Foundation
import XCTest

@testable import TeslatlasHubV1Compatibility

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class ClientRecoveryAndValidationTests: XCTestCase {
  private static let otherETag = "\"" + String(repeating: "a", count: 64) + "\""

  func testRefreshIssuedBeforeMismatchCannotClearQuarantine() async throws {
    let started = expectation(description: "Earlier discovery is suspended")
    let transport = try GatedV1RevalidationTransport(onStarted: { started.fulfill() })
    let client = try await HubV1Client.connectForTesting(
      discoveryURL: HubV1TestData.discoveryURL,
      expectedHubID: HubV1TestData.expectedHubID,
      credential: HubV1BearerCredential(HubV1TestData.bearer),
      transport: transport
    )
    let earlier = Task { try await client.refreshDiscovery() }
    await fulfillment(of: [started], timeout: 2)
    await assertThrowsHubV1Error(try await client.refreshDiscovery()) {
      guard case .hubIdentityMismatch = $0 else { return XCTFail("Expected definitive mismatch") }
    }
    await transport.releaseEarlierDiscovery()
    await assertThrowsHubV1Error(try await earlier.value) {
      guard case .hubIdentityMismatch = $0 else { return XCTFail("Earlier request must not clear quarantine") }
    }
    await assertThrowsHubV1Error(try await client.vehicles()) {
      guard case .hubIdentityMismatch = $0 else { return XCTFail("Credential dispatch must stay quarantined") }
    }
    let quarantined = await transport.requests()
    XCTAssertEqual(quarantined.count, 3)
    XCTAssertTrue(quarantined.allSatisfy { $0.value(forHTTPHeaderField: "Authorization") == nil })
    _ = try await client.refreshDiscovery()
    _ = try await client.vehicles()
    let recovered = await transport.requests()
    XCTAssertEqual(recovered.count, 5)
    XCTAssertEqual(recovered.last?.value(forHTTPHeaderField: "Authorization"), "Bearer \(HubV1TestData.bearer)")
  }

  func testIdentityMismatchQuarantinesAllRoutesUntilExpectedIdentityRevalidation() async throws {
    var changed = try HubV1TestData.jsonObject("discovery-full")
    let replacementID = try XCTUnwrap(
      UUID(uuidString: "018f18d2-6f45-7b3c-8a91-3c7286a10d43")
    )
    changed["hub_id"] = replacementID.uuidString.lowercased()
    let mismatch = HubV1Error.hubIdentityMismatch(
      expected: HubV1TestData.expectedHubID,
      actual: replacementID
    )
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(body: try HubV1TestData.encoded(changed)),
        HubV1StubResponse(statusCode: 503, headers: [:], body: Data()),
        HubV1StubResponse(body: try HubV1TestData.fixture("discovery-full")),
        HubV1StubResponse(body: try HubV1TestData.fixture("vehicles")),
        HubV1StubResponse(body: try HubV1TestData.fixture("current-state")),
        HubV1StubResponse(
          headers: ["Content-Type": "application/json", "ETag": HubV1TestData.strongETag],
          body: try HubV1TestData.fixture("drives-page-1")
        ),
      ]
    )

    await assertThrowsHubV1Error(try await client.refreshDiscovery()) {
      XCTAssertEqual($0, mismatch)
    }
    await assertThrowsHubV1Error(try await client.vehicles()) {
      XCTAssertEqual($0, mismatch)
    }
    await assertThrowsHubV1Error(
      try await client.currentState(vehicleID: HubV1TestData.vehicleID)
    ) { XCTAssertEqual($0, mismatch) }
    await assertThrowsHubV1Error(
      try await client.drives(vehicleID: HubV1TestData.vehicleID)
    ) { XCTAssertEqual($0, mismatch) }
    let quarantinedRequests = await transport.requests()
    XCTAssertEqual(quarantinedRequests.count, 2)
    let retained = await client.discoveryDocument()
    XCTAssertEqual(retained.hubID, HubV1TestData.expectedHubID)

    await assertThrowsHubV1Error(try await client.refreshDiscovery()) {
      XCTAssertEqual($0, .serviceUnavailable(requestID: nil))
    }
    await assertThrowsHubV1Error(try await client.vehicles()) {
      XCTAssertEqual($0, mismatch)
    }
    let requestsAfterTransientFailure = await transport.requests()
    XCTAssertEqual(requestsAfterTransientFailure.count, 3)

    let revalidated = try await client.refreshDiscovery()
    XCTAssertEqual(revalidated.hubID, HubV1TestData.expectedHubID)
    let vehicles = try await client.vehicles()
    XCTAssertEqual(vehicles.count, 1)
    let state = try await client.currentState(vehicleID: HubV1TestData.vehicleID)
    XCTAssertEqual(state.vehicleID, HubV1TestData.vehicleID)
    let drives = try await client.drives(vehicleID: HubV1TestData.vehicleID)
    guard case .modified(let page, _) = drives else {
      return XCTFail("Expected a drive page after revalidation")
    }
    XCTAssertEqual(page.items.count, 1)

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 7)
    XCTAssertTrue(requests.prefix(4).allSatisfy {
      $0.url == HubV1TestData.discoveryURL
        && $0.value(forHTTPHeaderField: "Authorization") == nil
    })
    XCTAssertTrue(requests.suffix(3).allSatisfy {
      $0.value(forHTTPHeaderField: "Authorization") == "Bearer \(HubV1TestData.bearer)"
    })
  }

  func testTransientDiscoveryFailureDoesNotQuarantineAuthenticatedRoutes() async throws {
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(statusCode: 503, headers: [:], body: Data()),
        HubV1StubResponse(body: try HubV1TestData.fixture("vehicles")),
      ]
    )

    await assertThrowsHubV1Error(try await client.refreshDiscovery()) {
      XCTAssertEqual($0, .serviceUnavailable(requestID: nil))
    }
    let vehicles = try await client.vehicles()
    XCTAssertEqual(vehicles.count, 1)
    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 3)
    XCTAssertNil(requests[1].value(forHTTPHeaderField: "Authorization"))
    XCTAssertEqual(
      requests[2].value(forHTTPHeaderField: "Authorization"),
      "Bearer \(HubV1TestData.bearer)"
    )
  }

  func testUntrustedRefreshResponseDoesNotQuarantineOriginalOrigin() async throws {
    var changed = try HubV1TestData.jsonObject("discovery-full")
    changed["hub_id"] = "018f18d2-6f45-7b3c-8a91-3c7286a10d43"
    let untrustedURL = try XCTUnwrap(
      URL(string: "https://other.example.invalid/.well-known/teslatlas-hub")
    )
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(body: try HubV1TestData.encoded(changed), finalURL: untrustedURL),
        HubV1StubResponse(body: try HubV1TestData.fixture("vehicles")),
      ]
    )

    await assertThrowsHubV1Error(try await client.refreshDiscovery()) {
      XCTAssertEqual($0, .untrustedOrigin("https://other.example.invalid"))
    }
    let vehicles = try await client.vehicles()
    XCTAssertEqual(vehicles.count, 1)
    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 3)
    XCTAssertEqual(requests[2].url?.host, HubV1TestData.discoveryURL.host)
    XCTAssertEqual(
      requests[2].value(forHTTPHeaderField: "Authorization"),
      "Bearer \(HubV1TestData.bearer)"
    )
  }

  func testUnsolicitedNotModifiedResponseIsRejectedWithHTTPContext() async throws {
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          statusCode: 304,
          headers: ["ETag": HubV1TestData.strongETag, "X-Request-ID": "unsolicited-304"],
          body: Data()
        )
      ]
    )

    await assertThrowsHubV1Error(try await client.drives(vehicleID: HubV1TestData.vehicleID)) {
      XCTAssertEqual(
        $0,
        .invalidResponse(
          statusCode: 304,
          requestID: "unsolicited-304",
          reason: "304 drive response requires a sent conditional validator"
        )
      )
    }
    let requests = await transport.requests()
    XCTAssertNil(requests.last?.value(forHTTPHeaderField: "If-None-Match"))
  }

  func testNotModifiedResponseMustMatchSentStrongValidator() async throws {
    let tag = try HubV1EntityTag(rawValue: HubV1TestData.strongETag)
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          statusCode: 304,
          headers: ["ETag": Self.otherETag, "X-Request-ID": "mismatched-304"],
          body: Data()
        )
      ]
    )

    await assertThrowsHubV1Error(
      try await client.drives(vehicleID: HubV1TestData.vehicleID, ifNoneMatch: tag)
    ) {
      XCTAssertEqual(
        $0,
        .invalidResponse(
          statusCode: 304,
          requestID: "mismatched-304",
          reason: "304 drive response ETag does not match the sent validator"
        )
      )
    }
    let requests = await transport.requests()
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "If-None-Match"), tag.rawValue)
  }

  func testModifiedConditionalResponseCanReplaceSentValidator() async throws {
    let tag = try HubV1EntityTag(rawValue: HubV1TestData.strongETag)
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          headers: ["Content-Type": "application/json", "ETag": Self.otherETag],
          body: try HubV1TestData.fixture("drives-page-1")
        )
      ]
    )

    let result = try await client.drives(vehicleID: HubV1TestData.vehicleID, ifNoneMatch: tag)
    guard case .modified(let page, let returnedTag) = result else {
      return XCTFail("Expected a modified response with a new validator")
    }
    XCTAssertEqual(page.items.count, 1)
    XCTAssertEqual(returnedTag.rawValue, Self.otherETag)
  }

  func testMalformedDiscoveryShapeRetainsHTTPContext() async throws {
    var object = try HubV1TestData.jsonObject("discovery-full")
    object["unexpected"] = "body-marker-not-for-errors"
    let transport = ScriptedHubV1Transport([
      HubV1StubResponse(
        headers: ["Content-Type": "application/json", "X-Request-ID": "discovery-shape"],
        body: try HubV1TestData.encoded(object)
      )
    ])

    await assertThrowsHubV1Error(
      try await HubV1Client.connectForTesting(
        discoveryURL: HubV1TestData.discoveryURL,
        expectedHubID: HubV1TestData.expectedHubID,
        credential: try HubV1BearerCredential(HubV1TestData.bearer),
        transport: transport
      )
    ) {
      XCTAssertEqual(
        $0,
        .invalidResponse(
          statusCode: 200,
          requestID: "discovery-shape",
          reason: "discovery fields do not match the deployed binding"
        )
      )
    }
    let requests = await transport.requests()
    XCTAssertNil(requests.first?.value(forHTTPHeaderField: "Authorization"))
  }

  func testMalformedSuccessShapesRetainHTTPContext() async throws {
    var vehicles = try HubV1TestData.jsonObject("vehicles")
    vehicles.removeValue(forKey: "vehicles")
    var current = try HubV1TestData.jsonObject("current-state")
    current.removeValue(forKey: "car")
    var drives = try HubV1TestData.jsonObject("drives-page-1")
    drives.removeValue(forKey: "items")
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          headers: ["Content-Type": "application/json", "X-Request-ID": "vehicles-shape"],
          body: try HubV1TestData.encoded(vehicles)
        ),
        HubV1StubResponse(
          headers: ["Content-Type": "application/json", "X-Request-ID": "current-shape"],
          body: try HubV1TestData.encoded(current)
        ),
        HubV1StubResponse(
          headers: [
            "Content-Type": "application/json",
            "X-Request-ID": "drives-shape",
            "ETag": HubV1TestData.strongETag,
          ],
          body: try HubV1TestData.encoded(drives)
        ),
      ]
    )

    await assertThrowsHubV1Error(try await client.vehicles()) {
      XCTAssertEqual(
        $0,
        .invalidResponse(
          statusCode: 200,
          requestID: "vehicles-shape",
          reason: "vehicle list fields do not match the deployed binding"
        )
      )
    }
    await assertThrowsHubV1Error(
      try await client.currentState(vehicleID: HubV1TestData.vehicleID)
    ) {
      XCTAssertEqual(
        $0,
        .invalidResponse(
          statusCode: 200,
          requestID: "current-shape",
          reason: "current state fields do not match the deployed binding"
        )
      )
    }
    await assertThrowsHubV1Error(try await client.drives(vehicleID: HubV1TestData.vehicleID)) {
      XCTAssertEqual(
        $0,
        .invalidResponse(
          statusCode: 200,
          requestID: "drives-shape",
          reason: "drive page fields do not match the deployed binding"
        )
      )
    }
  }

  func testMalformedAPIErrorShapeRetainsHTTPContext() async throws {
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          statusCode: 400,
          headers: ["Content-Type": "application/json", "X-Request-ID": "error-shape"],
          body: try HubV1TestData.encoded(["error": NSNull()])
        )
      ]
    )

    await assertThrowsHubV1Error(try await client.drives(vehicleID: HubV1TestData.vehicleID)) {
      XCTAssertEqual(
        $0,
        .invalidResponse(
          statusCode: 400,
          requestID: "error-shape",
          reason: "API error field is not an object"
        )
      )
    }
  }

  func testInvalidReturnedCursorsAreContextualResponseErrors() async throws {
    for rawCursor in ["", "never-print-this-cursor\n"] {
      var page = try HubV1TestData.jsonObject("drives-page-1")
      page["next_cursor"] = rawCursor
      let (client, _) = try await makeHubV1Client(
        additionalResponses: [
          HubV1StubResponse(
            headers: [
              "Content-Type": "application/json",
              "ETag": HubV1TestData.strongETag,
              "X-Request-ID": "invalid-returned-cursor",
            ],
            body: try HubV1TestData.encoded(page)
          )
        ]
      )

      await assertThrowsHubV1Error(try await client.drives(vehicleID: HubV1TestData.vehicleID)) {
        XCTAssertEqual(
          $0,
          .invalidResponse(
            statusCode: 200,
            requestID: "invalid-returned-cursor",
            reason: "drive page next cursor is invalid"
          )
        )
      }
    }
  }

  func testInvalidCallerCursorsRemainRequestErrors() {
    for rawCursor in ["", "never-print-this-cursor\n"] {
      XCTAssertThrowsError(try HubV1DriveCursor(rawValue: rawCursor)) {
        XCTAssertEqual(
          $0 as? HubV1Error,
          .invalidRequest("drive cursor is empty or contains control characters")
        )
      }
    }
  }
}

private actor GatedV1RevalidationTransport: HubV1HTTPTransport {
  private let discovery: Data
  private let replacement: Data
  private let vehicles: Data
  private let onStarted: @Sendable () -> Void
  private var captured: [URLRequest] = []
  private var continuation: CheckedContinuation<Void, Never>?

  init(onStarted: @escaping @Sendable () -> Void) throws {
    self.onStarted = onStarted
    discovery = try HubV1TestData.fixture("discovery-full")
    vehicles = try HubV1TestData.fixture("vehicles")
    var changed = try HubV1TestData.jsonObject("discovery-full")
    changed["hub_id"] = "018f18d2-6f45-7b3c-8a91-3c7286a10d43"
    replacement = try HubV1TestData.encoded(changed)
  }

  func send(_ request: URLRequest) async throws -> HubV1HTTPResponse {
    captured.append(request)
    let index = captured.count
    if index == 2 {
      onStarted()
      await withCheckedContinuation { continuation = $0 }
    }
    let body = index == 3 ? replacement : index == 5 ? vehicles : discovery
    return HubV1HTTPResponse(
      statusCode: 200, headers: ["Content-Type": "application/json"], body: body,
      finalURL: request.url ?? HubV1TestData.discoveryURL
    )
  }

  func releaseEarlierDiscovery() {
    continuation?.resume()
    continuation = nil
  }

  func requests() -> [URLRequest] { captured }
}

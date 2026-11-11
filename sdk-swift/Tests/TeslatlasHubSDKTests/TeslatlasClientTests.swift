import Foundation
import XCTest

@testable import TeslatlasHubSDK

final class TeslatlasClientTests: XCTestCase {
  func testConnectAndCurrentStateSendProtocolAuthorizationAndDecodeFixture()
    async throws
  {
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: TestDocuments.discovery, eTag: "\"discovery\"", version: "1.0.0"),
      .json(
        status: 200, body: TestDocuments.currentState, eTag: "\"current-42\"", version: "1.2.0"),
    ])
    let client = try await TeslatlasClient.connect(
      discoveryURL: TestURLs.discovery,
      maximumProtocolVersion: try XCTUnwrap(TeslatlasProtocolVersion("1.2.0")),
      authorization: try BearerCredential("top-secret"),
      transport: transport
    )

    let result = try await client.currentState(vehicleID: "vehicle_demo_alpha")

    guard case .modified(let state, let eTag) = result else {
      return XCTFail("Expected modified state")
    }
    XCTAssertEqual(state.vehicleID, "vehicle_demo_alpha")
    XCTAssertEqual(state.revision, 42)
    XCTAssertEqual(state.batteryLevelPercent, 78)
    XCTAssertEqual(eTag.rawValue, "\"current-42\"")

    let requests = await transport.requests
    XCTAssertEqual(requests.count, 2)
    XCTAssertNil(requests[0].value(forHTTPHeaderField: "Authorization"))
    XCTAssertNil(
      requests[0].value(forHTTPHeaderField: "Teslatlas-Protocol-Version")
    )
    XCTAssertEqual(
      requests[1].url?.absoluteString,
      "https://hub.example.invalid/v1/vehicles/vehicle_demo_alpha/current"
    )
    XCTAssertEqual(
      requests[1].value(forHTTPHeaderField: "Authorization"),
      "Bearer top-secret"
    )
    XCTAssertEqual(
      requests[1].value(forHTTPHeaderField: "Teslatlas-Protocol-Version"),
      "1.2.0"
    )
  }

  func testConditionalGetReturnsNotModifiedAndPreservesOpaqueETag() async throws {
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: TestDocuments.discovery, eTag: "\"discovery\"", version: "1.0.0"),
      TeslatlasHTTPResponse(
        statusCode: 304,
        headers: TestHeaders.json(eTag: "W/\"opaque-value\"", version: "1.2.0"),
        body: Data()
      ),
    ])
    let client = try await makeClient(transport: transport)

    let result = try await client.currentState(
      vehicleID: "vehicle_demo_alpha",
      ifNoneMatch: EntityTag("W/\"opaque-value\"")
    )

    XCTAssertEqual(result, .notModified(EntityTag("W/\"opaque-value\"")))
    let requests = await transport.requests
    XCTAssertEqual(
      requests[1].value(forHTTPHeaderField: "If-None-Match"),
      "W/\"opaque-value\""
    )
  }

  func testProblemDetailsPreserveStableCodeRequestIDAndIgnoreExtensions()
    async throws
  {
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: TestDocuments.discovery, eTag: "\"discovery\"", version: "1.0.0"),
      TeslatlasHTTPResponse(
        statusCode: 400,
        headers: [
          "Content-Type": "application/problem+json",
          "Teslatlas-Protocol-Version": "1.2.0",
          "X-Request-ID": "request_demo_0001",
        ],
        body: Data(TestDocuments.problemWithExtension.utf8)
      ),
    ])
    let client = try await makeClient(transport: transport)

    do {
      _ = try await client.currentState(vehicleID: "vehicle_demo_alpha")
      XCTFail("Expected problem")
    } catch let TeslatlasSDKError.problem(problem) {
      XCTAssertEqual(problem.code, "invalid_cursor")
      XCTAssertEqual(problem.requestID, "request_demo_0001")
      XCTAssertFalse(problem.retryable)
      XCTAssertEqual(problem.status, 400)
    }
  }

  func testRefreshDiscoveryFailsClosedOnHubIdentityChange() async throws {
    let changed = TestDocuments.discovery.replacingOccurrences(
      of: "018f18d2-6f45-7b3c-8a91-3c7286a10d42",
      with: "018f18d2-6f45-7b3c-8a91-3c7286a10d43"
    )
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: TestDocuments.discovery, eTag: "\"first\"", version: "1.0.0"),
      .json(status: 200, body: changed, eTag: "\"second\"", version: "1.0.0"),
    ])
    let client = try await TeslatlasClient.connect(
      discoveryURL: TestURLs.discovery,
      maximumProtocolVersion: try XCTUnwrap(TeslatlasProtocolVersion("1.2.0")),
      additionalTrustedEndpointOrigins: [
        URL(string: "https://vpn.example.invalid")!
      ],
      authorization: try BearerCredential("test-token"),
      transport: transport
    )

    await assertThrowsErrorAsync(
      try await client.refreshDiscovery(
        from: URL(string: "https://vpn.example.invalid/.well-known/teslatlas-hub")!)
    ) { error in
      XCTAssertEqual(
        error as? TeslatlasDiscoveryError,
        .hubIdentityChanged(
          expected: "urn:uuid:018f18d2-6f45-7b3c-8a91-3c7286a10d42",
          actual: "urn:uuid:018f18d2-6f45-7b3c-8a91-3c7286a10d43"
        )
      )
    }
  }

  func testListVehiclesUsesOpaqueCursorAndBoundedLimitWithoutDecodingCursor()
    async throws
  {
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: TestDocuments.discovery, eTag: "\"discovery\"", version: "1.0.0"),
      .json(status: 200, body: TestDocuments.vehiclePage, eTag: "\"vehicles\"", version: "1.2.0"),
    ])
    let client = try await makeClient(transport: transport)
    let cursor = OpaqueCursor("abcDEF0123._~-xyz")

    let result = try await client.vehicles(
      page: PageRequest(cursor: cursor, limit: 25)
    )

    guard case .modified(let page, _) = result else {
      return XCTFail("Expected page")
    }
    XCTAssertEqual(page.nextCursor, cursor)
    XCTAssertEqual(page.items.map(\.vehicleID), ["vehicle_demo_alpha"])
    let request = await transport.requests[1]
    let components = try XCTUnwrap(
      URLComponents(url: XCTUnwrap(request.url), resolvingAgainstBaseURL: false)
    )
    XCTAssertEqual(
      components.queryItems,
      [
        URLQueryItem(name: "cursor", value: "abcDEF0123._~-xyz"),
        URLQueryItem(name: "limit", value: "25"),
      ]
    )
  }

  func testPageLimitAboveDiscoveredMaximumFailsBeforeNetwork() async throws {
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: TestDocuments.discovery, eTag: "\"discovery\"", version: "1.0.0")
    ])
    let client = try await makeClient(transport: transport)

    await assertThrowsErrorAsync(
      try await client.vehicles(page: PageRequest(limit: 501))
    ) { error in
      XCTAssertEqual(
        error as? TeslatlasSDKError,
        .limitExceeded(name: "limit", maximum: 500, actual: 501)
      )
    }
    let requestCount = await transport.requests.count
    XCTAssertEqual(requestCount, 1)
  }

  func testConnectRejectsCrossOriginAdvertisedAPIBeforeBearerCanLeaveOrigin()
    async throws
  {
    let malicious = TestDocuments.discovery.replacingOccurrences(
      of: "https://hub.example.invalid/v1",
      with: "https://attacker.example.invalid/v1"
    )
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: malicious, eTag: "\"discovery\"", version: "1.0.0")
    ])

    await assertThrowsErrorAsync(
      try await TeslatlasClient.connect(
        discoveryURL: TestURLs.discovery,
        maximumProtocolVersion: try XCTUnwrap(
          TeslatlasProtocolVersion("1.2.0")
        ),
        authorization: try BearerCredential("must-not-leak"),
        transport: transport
      )
    ) { error in
      XCTAssertEqual(
        error as? TeslatlasDiscoveryError,
        .untrustedEndpointOrigin("https://attacker.example.invalid")
      )
    }
    let requests = await transport.requests
    XCTAssertEqual(requests.count, 1)
    XCTAssertNil(requests[0].value(forHTTPHeaderField: "Authorization"))
  }

  func testExplicitTrustedOriginAllowsHubEndpointRoaming() async throws {
    let roaming = TestDocuments.discovery.replacingOccurrences(
      of: "https://hub.example.invalid/v1",
      with: "https://vpn.example.invalid/v1"
    )
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: roaming, eTag: "\"discovery\"", version: "1.0.0"),
      .json(status: 200, body: TestDocuments.currentState, eTag: "\"current\"", version: "1.2.0"),
    ])
    let client = try await TeslatlasClient.connect(
      discoveryURL: TestURLs.discovery,
      maximumProtocolVersion: try XCTUnwrap(TeslatlasProtocolVersion("1.2.0")),
      additionalTrustedEndpointOrigins: [
        URL(string: "https://vpn.example.invalid")!
      ],
      authorization: try BearerCredential("roaming-secret"),
      transport: transport
    )

    _ = try await client.currentState(vehicleID: "vehicle_demo_alpha")

    let requests = await transport.requests
    XCTAssertEqual(requests[1].url?.host, "vpn.example.invalid")
    XCTAssertEqual(
      requests[1].value(forHTTPHeaderField: "Authorization"),
      "Bearer roaming-secret"
    )
  }

  private func makeClient(transport: ScriptedHTTPTransport) async throws
    -> TeslatlasClient
  {
    try await TeslatlasClient.connect(
      discoveryURL: TestURLs.discovery,
      maximumProtocolVersion: try XCTUnwrap(TeslatlasProtocolVersion("1.2.0")),
      authorization: try BearerCredential("test-token"),
      transport: transport
    )
  }
}

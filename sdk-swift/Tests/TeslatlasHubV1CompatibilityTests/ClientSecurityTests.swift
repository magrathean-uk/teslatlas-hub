import Foundation
import XCTest

@testable import TeslatlasHubV1Compatibility

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class ClientSecurityTests: XCTestCase {
  func testDiscoveryIsFetchedWithoutBearerBeforeClientExists() async throws {
    let (client, transport) = try await makeHubV1Client()

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 1)
    XCTAssertEqual(requests[0].url, HubV1TestData.discoveryURL)
    XCTAssertEqual(requests[0].httpMethod, "GET")
    XCTAssertNil(requests[0].value(forHTTPHeaderField: "Authorization"))
    let discovery = await client.discoveryDocument()
    XCTAssertEqual(discovery.hubID, HubV1TestData.expectedHubID)
  }

  func testDiscoveryServiceUnavailableUsesBoundStatusAndRequestID() async throws {
    let transport = ScriptedHubV1Transport([
      HubV1StubResponse(
        statusCode: 503,
        headers: ["X-Request-ID": "request-discovery-1"],
        body: Data()
      )
    ])

    await assertThrowsHubV1Error(
      try await HubV1Client.connectForTesting(
        discoveryURL: HubV1TestData.discoveryURL,
        expectedHubID: HubV1TestData.expectedHubID,
        credential: try HubV1BearerCredential(HubV1TestData.bearer),
        transport: transport
      )
    ) { error in
      XCTAssertEqual(
        error,
        .serviceUnavailable(requestID: "request-discovery-1")
      )
    }

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 1)
    XCTAssertNil(requests[0].value(forHTTPHeaderField: "Authorization"))
  }

  func testNilExpectedIdentityFailsBeforeNetwork() async throws {
    let transport = ScriptedHubV1Transport([])

    await assertThrowsHubV1Error(
      try await HubV1Client.connectForTesting(
        discoveryURL: HubV1TestData.discoveryURL,
        expectedHubID: UUID(
          uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
        ),
        credential: try HubV1BearerCredential(HubV1TestData.bearer),
        transport: transport
      )
    ) { error in
      guard case .invalidRequest = error else {
        return XCTFail("Expected invalidRequest, got \(error)")
      }
    }

    let requests = await transport.requests()
    XCTAssertTrue(requests.isEmpty)
  }

  func testIdentityMismatchFailsBeforeBearerCanBeSent() async throws {
    let transport = ScriptedHubV1Transport([
      HubV1StubResponse(body: try HubV1TestData.fixture("discovery-full"))
    ])
    let wrongIdentity = try XCTUnwrap(
      UUID(uuidString: "018f18d2-6f45-7b3c-8a91-3c7286a10d43")
    )

    await assertThrowsHubV1Error(
      try await HubV1Client.connectForTesting(
        discoveryURL: HubV1TestData.discoveryURL,
        expectedHubID: wrongIdentity,
        credential: try HubV1BearerCredential(HubV1TestData.bearer),
        transport: transport
      )
    ) { error in
      XCTAssertEqual(
        error,
        .hubIdentityMismatch(
          expected: wrongIdentity,
          actual: HubV1TestData.expectedHubID
        )
      )
    }

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 1)
    XCTAssertNil(requests[0].value(forHTTPHeaderField: "Authorization"))
  }

  func testBindingMismatchFailsBeforeBearerCanBeSent() async throws {
    var object = try HubV1TestData.jsonObject("discovery-full")
    object["sourceUrl"] = "https://github.com/magrathean-uk/teslatlas-hub/tree/main"
    let transport = ScriptedHubV1Transport([
      HubV1StubResponse(body: try HubV1TestData.encoded(object))
    ])

    await assertThrowsHubV1Error(
      try await HubV1Client.connectForTesting(
        discoveryURL: HubV1TestData.discoveryURL,
        expectedHubID: HubV1TestData.expectedHubID,
        credential: try HubV1BearerCredential(HubV1TestData.bearer),
        transport: transport
      )
    ) { error in
      guard case .invalidDiscovery = error else {
        return XCTFail("Expected invalidDiscovery, got \(error)")
      }
    }

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 1)
    XCTAssertNil(requests[0].value(forHTTPHeaderField: "Authorization"))
  }

  func testUnknownDiscoveryFieldFailsClosedBeforeBearer() async throws {
    var object = try HubV1TestData.jsonObject("discovery-full")
    object["events"] = "/v1/events"
    let transport = ScriptedHubV1Transport([
      HubV1StubResponse(body: try HubV1TestData.encoded(object))
    ])

    await assertThrowsHubV1Error(
      try await HubV1Client.connectForTesting(
        discoveryURL: HubV1TestData.discoveryURL,
        expectedHubID: HubV1TestData.expectedHubID,
        credential: try HubV1BearerCredential(HubV1TestData.bearer),
        transport: transport
      )
    ) { error in
      guard case .invalidResponse = error else {
        return XCTFail("Expected invalidResponse, got \(error)")
      }
    }

    let captured = await transport.requests()
    let request = try XCTUnwrap(captured.first)
    XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
  }

  func testCrossOriginDiscoveryResponseIsRejectedWithoutBearer() async throws {
    let attacker = try XCTUnwrap(
      URL(string: "https://attacker.example.invalid/.well-known/teslatlas-hub")
    )
    let transport = ScriptedHubV1Transport([
      HubV1StubResponse(
        body: try HubV1TestData.fixture("discovery-full"),
        finalURL: attacker
      )
    ])

    await assertThrowsHubV1Error(
      try await HubV1Client.connectForTesting(
        discoveryURL: HubV1TestData.discoveryURL,
        expectedHubID: HubV1TestData.expectedHubID,
        credential: try HubV1BearerCredential(HubV1TestData.bearer),
        transport: transport
      )
    ) { error in
      XCTAssertEqual(error, .untrustedOrigin("https://attacker.example.invalid"))
    }

    let captured = await transport.requests()
    let request = try XCTUnwrap(captured.first)
    XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
  }

  func testIPv6LoopbackHTTPDiscoveryIsAccepted() async throws {
    let discoveryURL = try XCTUnwrap(
      URL(string: "http://[::1]/.well-known/teslatlas-hub")
    )
    let transport = ScriptedHubV1Transport([
      HubV1StubResponse(body: try HubV1TestData.fixture("discovery-full"))
    ])

    _ = try await HubV1Client.connectForTesting(
      discoveryURL: discoveryURL,
      expectedHubID: HubV1TestData.expectedHubID,
      credential: try HubV1BearerCredential(HubV1TestData.bearer),
      transport: transport
    )

    let requests = await transport.requests()
    XCTAssertEqual(requests.first?.url, discoveryURL)
    XCTAssertNil(requests.first?.value(forHTTPHeaderField: "Authorization"))
  }

  func testIPv4LoopbackRangeHTTPDiscoveryIsAccepted() async throws {
    let discoveryURL = try XCTUnwrap(
      URL(string: "http://127.2.3.4/.well-known/teslatlas-hub")
    )
    let transport = ScriptedHubV1Transport([
      HubV1StubResponse(body: try HubV1TestData.fixture("discovery-full"))
    ])

    _ = try await HubV1Client.connectForTesting(
      discoveryURL: discoveryURL,
      expectedHubID: HubV1TestData.expectedHubID,
      credential: try HubV1BearerCredential(HubV1TestData.bearer),
      transport: transport
    )

    let requests = await transport.requests()
    XCTAssertEqual(requests.first?.url, discoveryURL)
  }

  func testNonLoopbackHTTPDiscoveryIsRejectedBeforeNetwork() async throws {
    let transport = ScriptedHubV1Transport([])
    let insecure = try XCTUnwrap(
      URL(string: "http://hub.example.invalid/.well-known/teslatlas-hub")
    )

    await assertThrowsHubV1Error(
      try await HubV1Client.connectForTesting(
        discoveryURL: insecure,
        expectedHubID: HubV1TestData.expectedHubID,
        credential: try HubV1BearerCredential(HubV1TestData.bearer),
        transport: transport
      )
    ) { error in
      guard case .invalidDiscoveryURL = error else {
        return XCTFail("Expected invalidDiscoveryURL, got \(error)")
      }
    }
    let requests = await transport.requests()
    XCTAssertTrue(requests.isEmpty)
  }

  func testDriveCapabilityFailureOccursBeforeAuthenticatedRequest() async throws {
    let (client, transport) = try await makeHubV1Client(
      discoveryFixture: "discovery-base"
    )

    await assertThrowsHubV1Error(
      try await client.drives(vehicleID: HubV1TestData.vehicleID)
    ) { error in
      XCTAssertEqual(error, .capabilityUnavailable("query.drives"))
    }

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 1)
    XCTAssertNil(requests[0].value(forHTTPHeaderField: "Authorization"))
  }

  func testInvalidDriveRangeFailsBeforeAuthenticatedRequest() async throws {
    let (client, transport) = try await makeHubV1Client()

    await assertThrowsHubV1Error(
      try await client.drives(
        vehicleID: HubV1TestData.vehicleID,
        query: HubV1DriveQuery(fromMilliseconds: 10, toMilliseconds: 10)
      )
    ) { error in
      guard case .invalidRequest = error else {
        return XCTFail("Expected invalidRequest, got \(error)")
      }
    }

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 1)
  }

  func testRefreshIdentityChangeUsesNoBearerAndLeavesPinnedDocument() async throws {
    var changed = try HubV1TestData.jsonObject("discovery-full")
    changed["hub_id"] = "018f18d2-6f45-7b3c-8a91-3c7286a10d43"
    let transport = ScriptedHubV1Transport([
      HubV1StubResponse(body: try HubV1TestData.fixture("discovery-full")),
      HubV1StubResponse(body: try HubV1TestData.encoded(changed)),
    ])
    let client = try await HubV1Client.connectForTesting(
      discoveryURL: HubV1TestData.discoveryURL,
      expectedHubID: HubV1TestData.expectedHubID,
      credential: try HubV1BearerCredential(HubV1TestData.bearer),
      transport: transport
    )

    await assertThrowsHubV1Error(try await client.refreshDiscovery()) { error in
      guard case .hubIdentityMismatch = error else {
        return XCTFail("Expected identity mismatch, got \(error)")
      }
    }

    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 2)
    XCTAssertTrue(
      requests.allSatisfy {
        $0.value(forHTTPHeaderField: "Authorization") == nil
      }
    )
    let retained = await client.discoveryDocument()
    XCTAssertEqual(retained.hubID, HubV1TestData.expectedHubID)
  }

  func testCredentialAndCursorDescriptionsAreRedacted() throws {
    let token =
      "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd"
    let credential = try HubV1BearerCredential(token)
    let cursor = try HubV1DriveCursor(rawValue: "never-print-this-cursor")

    XCTAssertFalse(credential.description.contains(token))
    XCTAssertFalse(String(reflecting: credential).contains(token))
    XCTAssertFalse(cursor.description.contains("never-print-this-cursor"))
    XCTAssertFalse(cursor.debugDescription.contains("never-print-this-cursor"))
  }

  func testCredentialAcceptsUppercaseHexUsedByHubValidation() throws {
    _ = try HubV1BearerCredential(
      "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF"
    )
  }

  func testCredentialRejectsNonHubWireTokensBeforeNetwork() {
    XCTAssertThrowsError(try HubV1BearerCredential("paired-device-secret"))
    XCTAssertThrowsError(
      try HubV1BearerCredential(
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg"
      )
    )
    XCTAssertThrowsError(
      try HubV1BearerCredential(
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde"
      )
    )
  }
}

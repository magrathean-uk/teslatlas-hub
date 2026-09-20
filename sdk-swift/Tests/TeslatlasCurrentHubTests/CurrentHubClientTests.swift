import Foundation
import XCTest

@testable import TeslatlasCurrentHub

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class CurrentHubClientTests: XCTestCase {
  func testDiscoveryHealthAndReadinessUsePublicUnauthenticatedRoutes() async throws {
    let (client, transport, _) = try await makeCurrentHubClient(
      additionalResponses: [
        CurrentHubStubResponse(body: try CurrentHubTestData.fixture("health")),
        CurrentHubStubResponse(body: try CurrentHubTestData.fixture("ready")),
      ]
    )

    let discovery = await client.discoveryDocument()
    let health = try await client.health()
    let readiness = try await client.readiness()
    XCTAssertEqual(discovery.version, "2026.36.2")
    XCTAssertEqual(health.status, "ok")
    XCTAssertEqual(readiness.status, "ready")
    let requests = await transport.requests()
    XCTAssertEqual(requests.map { $0.url?.path }, [
      "/.well-known/teslatlas-hub", "/healthz", "/readyz",
    ])
    XCTAssertTrue(requests.allSatisfy {
      $0.value(forHTTPHeaderField: "Authorization") == nil
    })
  }

  func testVehiclesAndCurrentUseStoredBearerAndExactRoutes() async throws {
    let (client, transport, _) = try await makeCurrentHubClient(
      additionalResponses: [
        CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
        CurrentHubStubResponse(body: try CurrentHubTestData.fixture("current")),
      ]
    )

    let vehicles = try await client.vehicles()
    let current = try await client.current(vehicleID: CurrentHubTestData.vehicleID)
    XCTAssertEqual(vehicles.first?.vehicleID, CurrentHubTestData.vehicleID)
    XCTAssertEqual(current.vehicleID, CurrentHubTestData.vehicleID)
    let requests = await transport.requests()
    XCTAssertEqual(requests[1].url?.path, "/v1/vehicles")
    XCTAssertEqual(
      requests[2].url?.path,
      "/v1/vehicles/11111111-1111-4111-8111-111111111111/current"
    )
    XCTAssertEqual(requests[1].value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenA)")
  }

  func testDrivesEncodesExactBoundQueryAndRequiresNoStoreETag() async throws {
    let cursor = try CurrentHubDriveCursor(rawValue: "opaque+/= value")
    let (client, transport, _) = try await makeCurrentHubClient(
      additionalResponses: [
        CurrentHubStubResponse(
          headers: [
            "Content-Type": "application/json",
            "Cache-Control": "no-store",
            "ETag": CurrentHubTestData.eTag,
          ],
          body: try CurrentHubTestData.fixture("drives")
        )
      ]
    )

    let result = try await client.drives(
      vehicleID: CurrentHubTestData.vehicleID,
      query: CurrentHubDriveQuery(
        fromMilliseconds: 0,
        toMilliseconds: Int64.max,
        limit: 2,
        cursor: cursor
      )
    )
    guard case .modified(let page, let eTag) = result else {
      return XCTFail("Expected modified page")
    }
    XCTAssertEqual(page.items.first?.id, 101)
    XCTAssertEqual(eTag.rawValue, CurrentHubTestData.eTag)
    let driveRequests = await transport.requests()
    let request = try XCTUnwrap(driveRequests.last)
    XCTAssertEqual(
      URLComponents(url: try XCTUnwrap(request.url), resolvingAgainstBaseURL: false)?.queryItems,
      [
        URLQueryItem(name: "from_ms", value: "0"),
        URLQueryItem(name: "to_ms", value: "9223372036854775807"),
        URLQueryItem(name: "limit", value: "2"),
        URLQueryItem(name: "cursor", value: "opaque+/= value"),
      ]
    )
  }

  func testDrivesReturnsNotModifiedOnlyWithNoStoreAndStrongETag() async throws {
    let tag = try CurrentHubEntityTag(rawValue: CurrentHubTestData.eTag)
    let (client, transport, _) = try await makeCurrentHubClient(
      additionalResponses: [
        CurrentHubStubResponse(
          statusCode: 304,
          headers: ["Cache-Control": "no-store", "ETag": CurrentHubTestData.eTag],
          body: Data()
        )
      ]
    )

    let result = try await client.drives(vehicleID: CurrentHubTestData.vehicleID, ifNoneMatch: tag)
    let requests = await transport.requests()
    XCTAssertEqual(result, .notModified(eTag: tag))
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "If-None-Match"), CurrentHubTestData.eTag)
  }

  func testClaimSendsExactBodyWithoutBearerAndPersistsCredential() async throws {
    let invitation = try JSONDecoder().decode(
      CurrentHubInvitation.self,
      from: Data(
        #"{"endpoint":"https://hub.example.invalid","expiresAtMs":9223372036854775807,"pairingId":"11111111-1111-4111-8111-111111111111","pairingUri":"teslatlas-hub://pair?endpoint=https%3A%2F%2Fhub.example.invalid&pairing_id=11111111-1111-4111-8111-111111111111&secret=0000000000000000000000000000000000000000000000000000000000000000&tls_pin=0000000000000000000000000000000000000000000000000000000000000000","secret":"0000000000000000000000000000000000000000000000000000000000000000","tlsPin":"0000000000000000000000000000000000000000000000000000000000000000"}"#.utf8
      )
    )
    let (client, transport, store) = try await makeCurrentHubClient(
      credential: nil,
      additionalResponses: [
        CurrentHubStubResponse(body: futureClaimResponse(token: CurrentHubTestData.tokenA))
      ]
    )

    let credential = try await client.claim(invitation: invitation, deviceName: "Swift Test")

    XCTAssertEqual(credential.deviceID, CurrentHubTestData.hubID)
    let saved = try await store.loadCredential()
    let requests = await transport.requests()
    let leafPins = await transport.leafPins()
    XCTAssertEqual(saved?.deviceID, CurrentHubTestData.hubID)
    XCTAssertEqual(leafPins, [String(repeating: "0", count: 64)])
    let request = try XCTUnwrap(requests.last)
    XCTAssertEqual(request.url?.path, "/v1/pairings/11111111-1111-4111-8111-111111111111/claim")
    XCTAssertNil(request.value(forHTTPHeaderField: "Authorization"))
    let body = try XCTUnwrap(request.httpBody)
    let object = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: String])
    XCTAssertEqual(object, ["secret": String(repeating: "0", count: 64), "device_name": "Swift Test"])
  }

  func testClaimAcceptsInvitationWhosePublicURLHasATrailingSlash() async throws {
    let invitation = try JSONDecoder().decode(
      CurrentHubInvitation.self,
      from: Data(
        #"{"endpoint":"https://hub.example.invalid/","expiresAtMs":9223372036854775807,"pairingId":"11111111-1111-4111-8111-111111111111","pairingUri":"teslatlas-hub://pair?endpoint=https%3A%2F%2Fhub.example.invalid%2F&pairing_id=11111111-1111-4111-8111-111111111111&secret=0000000000000000000000000000000000000000000000000000000000000000&tls_pin=0000000000000000000000000000000000000000000000000000000000000000","secret":"0000000000000000000000000000000000000000000000000000000000000000","tlsPin":"0000000000000000000000000000000000000000000000000000000000000000"}"#.utf8
      )
    )
    let (client, transport, _) = try await makeCurrentHubClient(
      credential: nil,
      additionalResponses: [
        CurrentHubStubResponse(body: futureClaimResponse(token: CurrentHubTestData.tokenA))
      ]
    )

    _ = try await client.claim(invitation: invitation, deviceName: "Swift Test")
    let requests = await transport.requests()
    XCTAssertEqual(requests.last?.url?.path, "/v1/pairings/11111111-1111-4111-8111-111111111111/claim")
  }

  func testClaimRejectsTransportWithoutInvitationPinOwnershipBeforeSecretRequest() async throws {
    let invitation = try currentHubInvitation()
    let transport = UnpinnedCurrentHubTransport([
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("claim")),
    ])
    let store = MemoryCurrentHubCredentialStore()
    let client = try await CurrentHubClient.connectForTesting(
      endpoint: CurrentHubTestData.endpoint,
      expectedHubID: CurrentHubTestData.hubID,
      credentialStore: store,
      transport: transport
    )

    await assertCurrentHubError(
      try await client.claim(invitation: invitation, deviceName: "Swift Test")
    ) { error in
      guard case .invalidRequest(let reason) = error else {
        return XCTFail("Expected an invalid request")
      }
      XCTAssertTrue(reason.contains("invitation leaf pin"))
    }
    let requestCount = await transport.requests().count
    let saveCount = await store.saveCount()
    XCTAssertEqual(requestCount, 1)
    XCTAssertEqual(saveCount, 0)
  }

  func testClaimRejectsExpiredIssuedCredentialBeforeSaving() async throws {
    let expired = Data(
      #"{"device_id":"11111111-1111-4111-8111-111111111111","access_token":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","expires_at_ms":0}"#.utf8
    )
    let (client, _, store) = try await makeCurrentHubClient(
      credential: nil,
      additionalResponses: [CurrentHubStubResponse(body: expired)]
    )

    await assertCurrentHubError(
      try await client.claim(invitation: currentHubInvitation(), deviceName: "Swift Test")
    ) { error in
      guard case .invalidResponse(200, _, let reason) = error else {
        return XCTFail("Expected invalid response")
      }
      XCTAssertTrue(reason.contains("expired credential"))
    }
    let saved = try await store.loadCredential()
    let saveCount = await store.saveCount()
    XCTAssertNil(saved)
    XCTAssertEqual(saveCount, 0)
  }

  func testRotatePersistsNewBearerUsedByNextRequest() async throws {
    let claim = Data(
      #"{"device_id":"11111111-1111-4111-8111-111111111111","access_token":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","expires_at_ms":9223372036854775000}"#.utf8
    )
    let (client, transport, store) = try await makeCurrentHubClient(
      additionalResponses: [
        CurrentHubStubResponse(body: claim),
        CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
      ]
    )

    _ = try await client.rotateCredential()
    _ = try await client.vehicles()

    let saved = try await store.loadCredential()
    XCTAssertEqual(saved?.deviceID, CurrentHubTestData.hubID)
    let requests = await transport.requests()
    XCTAssertEqual(requests[1].value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenA)")
    XCTAssertEqual(requests[2].value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenB)")
  }

  func testRotationRejectsExpiredIssuedCredentialAndPreservesOldCredential() async throws {
    let expired = Data(
      #"{"device_id":"11111111-1111-4111-8111-111111111111","access_token":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","expires_at_ms":0}"#.utf8
    )
    let original = try currentHubCredential()
    let (client, _, store) = try await makeCurrentHubClient(
      credential: original,
      additionalResponses: [CurrentHubStubResponse(body: expired)]
    )

    await assertCurrentHubError(try await client.rotateCredential()) { error in
      guard case .invalidResponse(200, _, let reason) = error else {
        return XCTFail("Expected invalid response")
      }
      XCTAssertTrue(reason.contains("expired credential"))
    }
    let retained = try await store.loadCredential()
    XCTAssertEqual(retained?.deviceID, original.deviceID)
    XCTAssertEqual(retained?.expiresAtMilliseconds, original.expiresAtMilliseconds)
    let saveCount = await store.saveCount()
    XCTAssertEqual(saveCount, 0)
  }

  func testUnsupportedOperationMakesZeroOutgoingRequest() async throws {
    let (client, transport, _) = try await makeCurrentHubClient()
    let count = await transport.requests().count

    await assertCurrentHubError(try await client.require(.commands)) { error in
      XCTAssertEqual(error, .capabilityUnavailable("commands"))
    }

    let after = await transport.requests().count
    XCTAssertEqual(after, count)
  }

  func testExpiredCredentialFailsBeforeOutgoingAuthenticatedRequest() async throws {
    let expired = try CurrentHubCredential(
      deviceID: CurrentHubTestData.hubID,
      accessToken: CurrentHubTestData.tokenA,
      expiresAtMilliseconds: 0
    )
    let (client, transport, _) = try await makeCurrentHubClient(credential: expired)
    let before = await transport.requests().count

    await assertCurrentHubError(try await client.vehicles()) { error in
      XCTAssertEqual(error, .credentialExpired)
    }
    let after = await transport.requests().count
    XCTAssertEqual(after, before)
  }

  func testExpiredInvitationFailsBeforeSecretLeavesProcess() async throws {
    let invitation = try JSONDecoder().decode(
      CurrentHubInvitation.self,
      from: Data(
        #"{"endpoint":"https://hub.example.invalid","expiresAtMs":0,"pairingId":"11111111-1111-4111-8111-111111111111","pairingUri":"teslatlas-hub://pair?endpoint=https%3A%2F%2Fhub.example.invalid&pairing_id=11111111-1111-4111-8111-111111111111&secret=0000000000000000000000000000000000000000000000000000000000000000&tls_pin=0000000000000000000000000000000000000000000000000000000000000000","secret":"0000000000000000000000000000000000000000000000000000000000000000","tlsPin":"0000000000000000000000000000000000000000000000000000000000000000"}"#.utf8
      )
    )
    let (client, transport, _) = try await makeCurrentHubClient(credential: nil)
    let before = await transport.requests().count

    await assertCurrentHubError(
      try await client.claim(invitation: invitation, deviceName: "Swift Test")
    ) { error in
      XCTAssertEqual(error, .invitationExpired)
    }
    let after = await transport.requests().count
    XCTAssertEqual(after, before)
  }

  func testIdentityMismatchStopsBeforeCredentialRequest() async throws {
    let transport = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery"))
    ])
    let store = MemoryCurrentHubCredentialStore(try currentHubCredential())

    await assertCurrentHubError(
      try await CurrentHubClient.connectForTesting(
        endpoint: CurrentHubTestData.endpoint,
        expectedHubID: UUID(uuidString: "22222222-2222-4222-8222-222222222222")!,
        credentialStore: store,
        transport: transport
      )
    ) { error in
      guard case .hubIdentityMismatch = error else {
        return XCTFail("Expected identity mismatch")
      }
    }
    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 1)
    XCTAssertNil(requests.first?.value(forHTTPHeaderField: "Authorization"))
  }

  func testMissingNullableCurrentFieldIsRejectedInsteadOfBecomingNull() async throws {
    var object = try XCTUnwrap(
      JSONSerialization.jsonObject(with: CurrentHubTestData.fixture("current"))
        as? [String: Any]
    )
    object.removeValue(forKey: "outside_temp")
    let body = try JSONSerialization.data(withJSONObject: object)
    let (client, _, _) = try await makeCurrentHubClient(
      additionalResponses: [CurrentHubStubResponse(body: body)]
    )

    await assertCurrentHubError(
      try await client.current(vehicleID: CurrentHubTestData.vehicleID)
    ) { error in
      guard case .invalidResponse(_, _, let reason) = error else {
        return XCTFail("Expected an invalid response")
      }
      XCTAssertEqual(reason, "current state response is missing required fields")
    }
  }

  func testMissingNullableDriveFieldIsRejectedInsteadOfBecomingNull() async throws {
    var root = try XCTUnwrap(
      JSONSerialization.jsonObject(with: CurrentHubTestData.fixture("drives"))
        as? [String: Any]
    )
    var items = try XCTUnwrap(root["items"] as? [[String: Any]])
    items[0].removeValue(forKey: "distance_km")
    root["items"] = items
    let body = try JSONSerialization.data(withJSONObject: root)
    let (client, _, _) = try await makeCurrentHubClient(
      additionalResponses: [
        CurrentHubStubResponse(
          headers: [
            "Content-Type": "application/json",
            "Cache-Control": "no-store",
            "ETag": CurrentHubTestData.eTag,
          ],
          body: body
        )
      ]
    )

    await assertCurrentHubError(
      try await client.drives(vehicleID: CurrentHubTestData.vehicleID)
    ) { error in
      guard case .invalidResponse(_, _, let reason) = error else {
        return XCTFail("Expected an invalid response")
      }
      XCTAssertEqual(reason, "drive response is missing required fields")
    }
  }

  func testBareStatusErrorsMapWithoutRequiringJSON() async throws {
    for (status, expected) in [
      (401, CurrentHubError.unauthorized(requestID: "bare")),
      (404, CurrentHubError.notFound(requestID: "bare")),
      (503, CurrentHubError.serviceUnavailable(requestID: "bare")),
    ] {
      let (client, _, _) = try await makeCurrentHubClient(
        additionalResponses: [
          CurrentHubStubResponse(
            statusCode: status,
            headers: ["X-Request-ID": "bare"],
            body: Data()
          )
        ]
      )
      await assertCurrentHubError(try await client.vehicles()) { error in
        XCTAssertEqual(error, expected)
      }
    }
  }

  func testDuplicateDiscoveryCapabilitiesFailClosed() async throws {
    var root = try XCTUnwrap(
      JSONSerialization.jsonObject(with: CurrentHubTestData.fixture("discovery"))
        as? [String: Any]
    )
    var capabilities = try XCTUnwrap(root["capabilities"] as? [String])
    capabilities.append(capabilities[0])
    root["capabilities"] = capabilities
    let transport = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(body: try JSONSerialization.data(withJSONObject: root))
    ])

    await assertCurrentHubError(
      try await CurrentHubClient.connectForTesting(
        endpoint: CurrentHubTestData.endpoint,
        expectedHubID: CurrentHubTestData.hubID,
        credentialStore: MemoryCurrentHubCredentialStore(try currentHubCredential()),
        transport: transport
      )
    ) { error in
      guard case .invalidDiscovery = error else {
        return XCTFail("Expected invalid discovery")
      }
    }
  }
}

private func currentHubInvitation() throws -> CurrentHubInvitation {
  try JSONDecoder().decode(
    CurrentHubInvitation.self,
    from: Data(
      #"{"endpoint":"https://hub.example.invalid","expiresAtMs":9223372036854775807,"pairingId":"11111111-1111-4111-8111-111111111111","pairingUri":"teslatlas-hub://pair?endpoint=https%3A%2F%2Fhub.example.invalid&pairing_id=11111111-1111-4111-8111-111111111111&secret=0000000000000000000000000000000000000000000000000000000000000000&tls_pin=0000000000000000000000000000000000000000000000000000000000000000","secret":"0000000000000000000000000000000000000000000000000000000000000000","tlsPin":"0000000000000000000000000000000000000000000000000000000000000000"}"#.utf8
    )
  )
}

private func futureClaimResponse(token: String) -> Data {
  Data(
    """
    {"device_id":"11111111-1111-4111-8111-111111111111","access_token":"\(token)","expires_at_ms":9223372036854775000}
    """.utf8
  )
}

private actor UnpinnedCurrentHubTransport: CurrentHubHTTPTransport {
  private var responses: [CurrentHubStubResponse]
  private var captured: [URLRequest] = []

  init(_ responses: [CurrentHubStubResponse]) { self.responses = responses }

  func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse {
    captured.append(request)
    guard !responses.isEmpty, let url = request.url else {
      throw CurrentHubTestError.noResponse
    }
    let response = responses.removeFirst()
    return CurrentHubHTTPResponse(
      statusCode: response.statusCode,
      headers: response.headers,
      body: response.body,
      finalURL: response.finalURL ?? url
    )
  }

  func requests() -> [URLRequest] { captured }
}

import Foundation
import XCTest
import TeslatlasCurrentHub

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

/// These regressions use the public credential/client APIs. Their transports record
/// synthetic requests; they do not provide installed-Hub or native TLS evidence.
final class CurrentHubRepairRegressionTests: XCTestCase {
  func testIssuedCredentialSecretArchiveRestoresIntoFreshStoreAndClient() async throws {
    let firstStore = try RepairArchiveStore()
    let firstTransport = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try repairClaimResponse()),
    ])
    let firstClient = try await repairClient(store: firstStore, transport: firstTransport)
    let issued = try await firstClient.claim(invitation: repairInvitation(), deviceName: "Archive Test")
    let bytes = await firstStore.archivedBytes()
    let archive = try XCTUnwrap(bytes)

    let restored = try CurrentHubCredential.restoreSecretArchive(archive)
    XCTAssertEqual(restored.deviceID, issued.deviceID)
    XCTAssertEqual(restored.expiresAtMilliseconds, Int64.max)
    let restartedStore = try RepairArchiveStore(archive: archive)
    let transport = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
    ])
    let client = try await repairClient(store: restartedStore, transport: transport)
    _ = try await client.vehicles()
    let requests = await transport.requests()
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenB)")

    var diagnostic = ""
    dump(restored, to: &diagnostic)
    XCTAssertFalse(diagnostic.contains(CurrentHubTestData.tokenB))
    XCTAssertFalse(String(reflecting: restored).contains(CurrentHubTestData.tokenB))
  }

  func testSecretArchiveRestoreRejectsMalformedShapeVersionAndExpiredCredential() throws {
    let valid: [String: Any] = [
      "version": 1,
      "device_id": CurrentHubTestData.hubID.uuidString.lowercased(),
      "access_token": CurrentHubTestData.tokenB,
      "expires_at_ms": Int64.max,
    ]
    let malformed: [[String: Any]] = [
      ["version": 2],
      ["device_id": "00000000-0000-0000-0000-000000000000"],
      ["device_id": "ABCDEF01-ABCD-4BCD-8ABC-ABCDEFABCDEF"],
      ["access_token": String(repeating: "B", count: 64)],
      ["access_token": "short"],
      ["expires_at_ms": "9223372036854775807"],
      ["expires_at_ms": 1.5],
      ["expires_at_ms": UInt64.max],
      ["expires_at_ms": NSNull()],
      ["expires_at_ms": Int64.min],
    ]
    for replacement in malformed {
      let object = valid.merging(replacement) { _, new in new }
      let archive = try JSONSerialization.data(withJSONObject: object)
      XCTAssertThrowsError(try CurrentHubCredential.restoreSecretArchive(archive)) { error in
        XCTAssertFalse(String(describing: error).contains(CurrentHubTestData.tokenB))
      }
    }
    var missingExpiry = valid
    missingExpiry.removeValue(forKey: "expires_at_ms")
    XCTAssertThrowsError(
      try CurrentHubCredential.restoreSecretArchive(JSONSerialization.data(withJSONObject: missingExpiry))
    )
    let expired = try CurrentHubCredential(
      deviceID: CurrentHubTestData.hubID,
      accessToken: CurrentHubTestData.tokenB,
      expiresAtMilliseconds: 0
    )
    XCTAssertThrowsError(try CurrentHubCredential.restoreSecretArchive(expired.exportSecretArchive())) { error in
      XCTAssertEqual(error as? CurrentHubError, .credentialExpired)
    }
  }

  func testInvitationReflectionRedactsSecretAndPairingURI() throws {
    let invitation = try repairInvitation()
    var diagnostic = ""
    dump(invitation, to: &diagnostic)
    XCTAssertFalse(diagnostic.contains(CurrentHubTestData.tokenA))
    XCTAssertFalse(diagnostic.contains(invitation.pairingURI.absoluteString))
    XCTAssertFalse(String(reflecting: invitation).contains(CurrentHubTestData.tokenA))
    XCTAssertFalse(Mirror(reflecting: invitation).children.contains { $0.label == "secret" || $0.label == "pairingURI" })
    XCTAssertTrue(invitation.pairingURI.absoluteString.contains(CurrentHubTestData.tokenA))
  }

  func testIssuedCredentialAdmissionRetainsExactPackagedShapeAndFreshness() async throws {
    let valid: [String: Any] = [
      "device_id": CurrentHubTestData.hubID.uuidString.lowercased(),
      "access_token": CurrentHubTestData.tokenB,
      "expires_at_ms": Int64.max,
    ]
    let malformed: [[String: Any]] = [
      ["device_id": "ABCDEF01-ABCD-4BCD-8ABC-ABCDEFABCDEF"],
      ["device_id": "00000000-0000-0000-0000-000000000000"],
      ["access_token": String(repeating: "B", count: 64)],
      ["access_token": "short"],
      ["expires_at_ms": UInt64.max],
      ["expires_at_ms": 0],
    ]
    for rotating in [false, true] {
      for replacement in malformed {
        let body = try JSONSerialization.data(withJSONObject: valid.merging(replacement) { _, new in new })
        let (client, _, store) = try await makeCurrentHubClient(
          credential: rotating ? try currentHubCredential() : nil,
          additionalResponses: [CurrentHubStubResponse(body: body)]
        )
        if rotating {
          await assertCurrentHubError(try await client.rotateCredential()) { _ in }
        } else {
          await assertCurrentHubError(
            try await client.claim(invitation: repairInvitation(), deviceName: "Shape Test")
          ) { _ in }
        }
        let saves = await store.saveCount()
        let pending = await client.pendingCredentialForPersistence()
        XCTAssertEqual(saves, 0)
        XCTAssertNil(pending)
      }
    }
  }

  func testFailedClaimSaveRetainsCredentialAndRetryDoesNotClaimAgain() async throws {
    try await verifyPersistenceRetry(rotating: false)
  }

  func testFailedRotationSaveRetainsCredentialAndRetryDoesNotRotateAgain() async throws {
    try await verifyPersistenceRetry(rotating: true)
  }

  private func verifyPersistenceRetry(rotating: Bool) async throws {
    let initial = rotating ? try currentHubCredential() : nil
    let store = try RepairArchiveStore(initial: initial, failures: 1)
    let transport = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try repairClaimResponse()),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
    ])
    let client = try await repairClient(store: store, transport: transport)
    do {
      if rotating {
        _ = try await client.rotateCredential()
      } else {
        _ = try await client.claim(invitation: repairInvitation(), deviceName: "Persistence Test")
      }
      XCTFail("Issuance must not report success when its store save fails")
    } catch RepairStoreError.unavailable {}

    let pending = await client.pendingCredentialForPersistence()
    let credential = try XCTUnwrap(pending)
    let archive = try credential.exportSecretArchive()
    XCTAssertEqual(try CurrentHubCredential.restoreSecretArchive(archive).deviceID, CurrentHubTestData.hubID)
    let initialRequests = await transport.requests()
    XCTAssertEqual(initialRequests.count, 2)
    XCTAssertEqual(initialRequests.last?.url?.path, rotating
      ? "/v1/device/rotate"
      : "/v1/pairings/11111111-1111-4111-8111-111111111111/claim")
    XCTAssertEqual(initialRequests.last?.value(forHTTPHeaderField: "Authorization"), rotating ? "Bearer \(CurrentHubTestData.tokenA)" : nil)

    await assertCurrentHubError(try await client.rotateCredential()) { error in
      guard case .invalidRequest = error else { return XCTFail("Expected pending persistence rejection") }
    }
    await assertCurrentHubError(
      try await client.claim(invitation: repairInvitation(), deviceName: "Repeat Test")
    ) { error in
      guard case .invalidRequest = error else { return XCTFail("Expected pending persistence rejection") }
    }
    await assertCurrentHubError(try await client.vehicles()) { error in
      guard case .invalidRequest = error else { return XCTFail("Expected pending persistence rejection") }
    }
    let beforeRetry = await transport.requests()
    XCTAssertEqual(beforeRetry.count, 2)
    _ = try await client.retryCredentialPersistence()
    let afterRetry = await transport.requests()
    let saves = await store.saveCount()
    let remaining = await client.pendingCredentialForPersistence()
    XCTAssertEqual(afterRetry.count, 2, "Retry must perform only the failed store operation")
    XCTAssertEqual(saves, 2)
    XCTAssertNil(remaining)
    _ = try await client.vehicles()
    let requests = await transport.requests()
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenB)")
  }

  func testIdentityMismatchQuarantinesCredentialOperationsUntilUnauthenticatedRefresh() async throws {
    let changedID = UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
    let (client, transport, store) = try await makeCurrentHubClient(additionalResponses: [
      CurrentHubStubResponse(body: try repairDiscovery(hubID: changedID)),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("health")),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
    ])
    let mismatch = CurrentHubError.hubIdentityMismatch(expected: CurrentHubTestData.hubID, actual: changedID)
    await assertCurrentHubError(try await client.refreshDiscovery()) { XCTAssertEqual($0, mismatch) }
    await assertCurrentHubError(try await client.vehicles()) { XCTAssertEqual($0, mismatch) }
    await assertCurrentHubError(try await client.current(vehicleID: CurrentHubTestData.vehicleID)) { XCTAssertEqual($0, mismatch) }
    await assertCurrentHubError(try await client.drives(vehicleID: CurrentHubTestData.vehicleID)) { XCTAssertEqual($0, mismatch) }
    await assertCurrentHubError(try await client.rotateCredential()) { XCTAssertEqual($0, mismatch) }
    await assertCurrentHubError(
      try await client.claim(invitation: repairInvitation(), deviceName: "Quarantine Test")
    ) { XCTAssertEqual($0, mismatch) }
    let quarantinedRequests = await transport.requests()
    let retained = await client.discoveryDocument()
    let saved = try await store.loadCredential()
    XCTAssertEqual(quarantinedRequests.count, 2)
    XCTAssertEqual(retained.hubID, CurrentHubTestData.hubID)
    XCTAssertNotNil(saved)
    _ = try await client.health()
    _ = try await client.refreshDiscovery()
    _ = try await client.vehicles()
    let requests = await transport.requests()
    XCTAssertTrue(requests.dropLast().allSatisfy { $0.value(forHTTPHeaderField: "Authorization") == nil })
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenA)")
  }

  func testTransientDiscoveryFailureDoesNotQuarantineStoredCredential() async throws {
    let (client, transport, _) = try await makeCurrentHubClient(additionalResponses: [
      CurrentHubStubResponse(statusCode: 503, body: Data()),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
    ])
    await assertCurrentHubError(try await client.refreshDiscovery()) {
      XCTAssertEqual($0, .serviceUnavailable(requestID: nil))
    }
    _ = try await client.vehicles()
    let requests = await transport.requests()
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenA)")
  }

  func testUntrustedDiscoveryFinalOriginDoesNotQuarantineOriginalAuthenticationEndpoint() async throws {
    let (client, transport, _) = try await makeCurrentHubClient(additionalResponses: [
      CurrentHubStubResponse(
        body: try repairDiscovery(hubID: UUID()),
        finalURL: URL(string: "https://other.example.invalid/.well-known/teslatlas-hub")!
      ),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
    ])
    await assertCurrentHubError(try await client.refreshDiscovery()) { error in
      guard case .untrustedOrigin = error else { return XCTFail("Expected origin rejection") }
    }
    _ = try await client.vehicles()
    let requests = await transport.requests()
    XCTAssertEqual(requests.last?.url?.host, "hub.example.invalid")
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenA)")
  }

  func testSuspendedCredentialLoadCannotDispatchAfterMismatchAndRecovery() async throws {
    let started = expectation(description: "Credential load suspended")
    let store = try RepairSuspendingStore(onFirstLoad: { started.fulfill() })
    defer { Task { await store.releaseLoad() } }
    let transport = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try repairDiscovery(hubID: UUID())),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
    ])
    let client = try await repairClient(store: store, transport: transport)
    let waiting = Task { try await client.vehicles() }
    await fulfillment(of: [started], timeout: 2)
    await assertCurrentHubError(try await client.refreshDiscovery()) { error in
      guard case .hubIdentityMismatch = error else { return XCTFail("Expected identity mismatch") }
    }
    _ = try await client.refreshDiscovery()
    await store.releaseLoad()
    await assertCurrentHubError(try await waiting.value) { error in
      guard case .invalidRequest = error else { return XCTFail("Expected stale identity context rejection") }
    }
    let beforeRetry = await transport.requests()
    XCTAssertEqual(beforeRetry.count, 3)
    _ = try await client.vehicles()
    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 4)
  }

  func testSuspendedCredentialLoadCannotDispatchOldBearerAfterRotationCompletes() async throws {
    let started = expectation(description: "Old credential load suspended")
    let store = try RepairSuspendingStore(onFirstLoad: { started.fulfill() })
    defer { Task { await store.releaseLoad() } }
    let transport = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try repairClaimResponse()),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
    ])
    let client = try await repairClient(store: store, transport: transport)
    let waiting = Task { try await client.vehicles() }
    await fulfillment(of: [started], timeout: 2)
    _ = try await client.rotateCredential()
    await store.releaseLoad()
    await assertCurrentHubError(try await waiting.value) { error in
      guard case .invalidRequest = error else { return XCTFail("Expected stale credential context rejection") }
    }
    let beforeRetry = await transport.requests()
    XCTAssertEqual(beforeRetry.count, 2)
    _ = try await client.vehicles()
    let requests = await transport.requests()
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenB)")
  }

  func testDrivesRejectsUnsolicited304AndDifferentStrongETag() async throws {
    let sent = try CurrentHubEntityTag(rawValue: CurrentHubTestData.eTag)
    let other = "\"\(String(repeating: "f", count: 64))\""
    for (witness, returned) in [(nil, CurrentHubTestData.eTag), (Optional(sent), other)] {
      let (client, transport, _) = try await makeCurrentHubClient(additionalResponses: [
        CurrentHubStubResponse(
          statusCode: 304,
          headers: ["Cache-Control": "no-store", "ETag": returned],
          body: Data()
        )
      ])
      await assertCurrentHubError(
        try await client.drives(vehicleID: CurrentHubTestData.vehicleID, ifNoneMatch: witness)
      ) { error in
        guard case .invalidResponse(let status, _, _) = error else { return XCTFail("Expected invalid conditional result") }
        XCTAssertEqual(status, 304)
      }
      let requests = await transport.requests()
      XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "If-None-Match"), witness?.rawValue)
    }
  }

  func testClientRejectsEachMissingNullableCarKeyWhileStandaloneModelStaysPermissive() async throws {
    let car = repairCar()
    let nullableKeys = [
      "vin", "source_eid", "source_vid", "trim_badging", "marketing_name",
      "exterior_color", "wheel_type", "spoiler_type", "firmware_version", "efficiency_wh_per_km",
    ]
    for key in nullableKeys {
      var incomplete = car
      incomplete.removeValue(forKey: key)
      let body = try repairCurrent(car: incomplete)
      let standalone = try JSONDecoder().decode(CurrentHubCurrentState.self, from: body)
      XCTAssertNotNil(standalone.car)
      let (client, _, _) = try await makeCurrentHubClient(additionalResponses: [CurrentHubStubResponse(body: body)])
      await assertCurrentHubError(try await client.current(vehicleID: CurrentHubTestData.vehicleID)) { error in
        guard case .invalidResponse = error else { return XCTFail("Expected missing car field rejection for \(key)") }
      }
    }
    let (client, _, _) = try await makeCurrentHubClient(additionalResponses: [
      CurrentHubStubResponse(body: try repairCurrent(car: car)),
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("current")),
    ])
    let complete = try await client.current(vehicleID: CurrentHubTestData.vehicleID)
    let null = try await client.current(vehicleID: CurrentHubTestData.vehicleID)
    XCTAssertNotNil(complete.car)
    XCTAssertNil(complete.car?.vin)
    XCTAssertNil(null.car)
  }

}

private enum RepairStoreError: Error { case unavailable }

private actor RepairArchiveStore: CurrentHubCredentialStore {
  private var archive: Data?
  private var failures: Int
  private var saves = 0

  init(initial: CurrentHubCredential? = nil, failures: Int = 0, archive: Data? = nil) throws {
    self.archive = try archive ?? initial?.exportSecretArchive()
    self.failures = failures
  }

  func loadCredential() async throws -> CurrentHubCredential? {
    try archive.map(CurrentHubCredential.restoreSecretArchive)
  }

  func saveCredential(_ credential: CurrentHubCredential) async throws {
    saves += 1
    if failures > 0 {
      failures -= 1
      throw RepairStoreError.unavailable
    }
    archive = try credential.exportSecretArchive()
  }

  func archivedBytes() -> Data? { archive }
  func saveCount() -> Int { saves }
}

private actor RepairSuspendingStore: CurrentHubCredentialStore {
  private var archive: Data
  private let onFirstLoad: @Sendable () -> Void
  private var firstLoadStarted = false
  private var released = false
  private var continuation: CheckedContinuation<Void, Never>?

  init(onFirstLoad: @escaping @Sendable () -> Void) throws {
    archive = try currentHubCredential().exportSecretArchive()
    self.onFirstLoad = onFirstLoad
  }

  func loadCredential() async throws -> CurrentHubCredential? {
    let captured = try CurrentHubCredential.restoreSecretArchive(archive)
    if !firstLoadStarted {
      firstLoadStarted = true
      onFirstLoad()
      if !released {
        await withCheckedContinuation { continuation = $0 }
      }
    }
    return captured
  }

  func saveCredential(_ credential: CurrentHubCredential) async throws {
    archive = try credential.exportSecretArchive()
  }

  func releaseLoad() {
    released = true
    continuation?.resume()
    continuation = nil
  }
}

private func repairClient(
  store: any CurrentHubCredentialStore,
  transport: any CurrentHubHTTPTransport
) async throws -> CurrentHubClient {
  try await CurrentHubClient.connect(
    endpoint: CurrentHubTestData.endpoint,
    expectedHubID: CurrentHubTestData.hubID,
    credentialStore: store,
    transport: transport
  )
}

private func repairInvitation() throws -> CurrentHubInvitation {
  let secret = CurrentHubTestData.tokenA
  var uri = URLComponents()
  uri.scheme = "teslatlas-hub"
  uri.host = "pair"
  uri.queryItems = [
    URLQueryItem(name: "endpoint", value: CurrentHubTestData.endpoint.absoluteString),
    URLQueryItem(name: "pairing_id", value: CurrentHubTestData.hubID.uuidString.lowercased()),
    URLQueryItem(name: "secret", value: secret),
    URLQueryItem(name: "tls_pin", value: secret),
  ]
  let data = try JSONSerialization.data(withJSONObject: [
    "endpoint": CurrentHubTestData.endpoint.absoluteString,
    "pairingId": CurrentHubTestData.hubID.uuidString.lowercased(),
    "expiresAtMs": Int64.max,
    "tlsPin": secret,
    "pairingUri": try XCTUnwrap(uri.url).absoluteString,
    "secret": secret,
  ])
  return try JSONDecoder().decode(CurrentHubInvitation.self, from: data)
}

private func repairClaimResponse() throws -> Data {
  try JSONSerialization.data(withJSONObject: [
    "device_id": CurrentHubTestData.hubID.uuidString.lowercased(),
    "access_token": CurrentHubTestData.tokenB,
    "expires_at_ms": Int64.max,
  ])
}

private func repairDiscovery(hubID: UUID) throws -> Data {
  var object = try XCTUnwrap(
    JSONSerialization.jsonObject(with: CurrentHubTestData.fixture("discovery")) as? [String: Any]
  )
  object["hub_id"] = hubID.uuidString.lowercased()
  return try JSONSerialization.data(withJSONObject: object)
}

private func repairCurrent(car: [String: Any]) throws -> Data {
  var object = try XCTUnwrap(
    JSONSerialization.jsonObject(with: CurrentHubTestData.fixture("current")) as? [String: Any]
  )
  object["car"] = car
  return try JSONSerialization.data(withJSONObject: object)
}

private func repairCar() -> [String: Any] {
  [
    "id": 1, "name": "Synthetic car", "model": "3", "vin": NSNull(),
    "source_eid": NSNull(), "source_vid": NSNull(), "trim_badging": NSNull(),
    "marketing_name": NSNull(), "exterior_color": NSNull(), "wheel_type": NSNull(),
    "spoiler_type": NSNull(), "firmware_version": NSNull(), "efficiency_wh_per_km": NSNull(),
    "settings": [
      "enabled": true, "use_streaming_api": false, "suspend_after_idle_min": 15,
      "suspend_min": 21, "suspend_min_resolved": true, "req_not_unlocked": false,
      "free_supercharging": false, "lfp_battery": false,
    ],
  ]
}

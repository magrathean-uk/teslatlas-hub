import Foundation
import XCTest

@testable import TeslatlasCurrentHub

final class CurrentHubPersistenceRecoveryTests: XCTestCase {
  func testExpiredPendingClaimAllowsFreshInvitationWithoutChangingStoreUntilSave() async throws {
    let clock = PersistenceRecoveryClock()
    let store = PersistenceRecoveryStore(actions: [.writeThenThrow, .succeed])
    let transport = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try persistenceClaimResponse(token: CurrentHubTestData.tokenB, expiry: clock.now + 10)),
      CurrentHubStubResponse(body: try persistenceClaimResponse(token: persistenceTokenC, expiry: clock.now + 100)),
    ])
    let client = try await persistenceClient(store: store, transport: transport, clock: clock)
    await assertPersistenceStorageFailure(
      try await client.claim(invitation: persistenceInvitation(), deviceName: "First claim")
    )
    clock.advance(by: 20)
    await assertCurrentHubError(try await client.retryCredentialPersistence()) {
      XCTAssertEqual($0, .credentialExpired)
    }
    let expiredPending = await client.pendingCredentialForPersistence()
    let storedBeforeNewClaim = try await store.loadCredential()
    XCTAssertEqual(try persistenceToken(XCTUnwrap(expiredPending)), CurrentHubTestData.tokenB)
    XCTAssertEqual(try persistenceToken(XCTUnwrap(storedBeforeNewClaim)), CurrentHubTestData.tokenB)
    let savesBefore = await store.saveCount()
    XCTAssertEqual(savesBefore, 1, "An expired retry must neither save nor erase the stored credential")

    let newPairingID = UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
    _ = try await client.claim(
      invitation: persistenceInvitation(pairingID: newPairingID), deviceName: "Fresh claim"
    )
    let pending = await client.pendingCredentialForPersistence()
    let stored = try await store.loadCredential()
    let requests = await transport.requests()
    XCTAssertNil(pending)
    XCTAssertEqual(try persistenceToken(XCTUnwrap(stored)), persistenceTokenC)
    XCTAssertEqual(requests.map { $0.url?.path }, [
      "/.well-known/teslatlas-hub",
      "/v1/pairings/11111111-1111-4111-8111-111111111111/claim",
      "/v1/pairings/22222222-2222-4222-8222-222222222222/claim",
    ])
    XCTAssertTrue(requests.allSatisfy { $0.value(forHTTPHeaderField: "Authorization") == nil })
  }

  func testExpiredPendingCredentialIsNotRetiredWhileItsSaveIsActive() async throws {
    let clock = PersistenceRecoveryClock()
    let started = expectation(description: "Issued credential save is suspended")
    let store = PersistenceRecoveryStore(
      actions: [.suspendThenThrow, .succeed], onSuspendedSave: { started.fulfill() }
    )
    defer { Task { await store.releaseSave() } }
    let transport = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
      CurrentHubStubResponse(body: try persistenceClaimResponse(token: CurrentHubTestData.tokenB, expiry: clock.now + 10)),
      CurrentHubStubResponse(body: try persistenceClaimResponse(token: persistenceTokenC, expiry: clock.now + 100)),
    ])
    let client = try await persistenceClient(store: store, transport: transport, clock: clock)
    let originalInvitation = try persistenceInvitation()
    let issuance = Task { try await client.claim(invitation: originalInvitation, deviceName: "Suspended claim") }
    await fulfillment(of: [started], timeout: 2)
    clock.advance(by: 20)
    let freshInvitation = try persistenceInvitation(
      pairingID: UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
    )
    await assertCurrentHubError(try await client.claim(invitation: freshInvitation, deviceName: "Concurrent claim")) {
      XCTAssertEqual($0, .invalidRequest("credential issuance or persistence is already in progress"))
    }
    let pendingDuringSave = await client.pendingCredentialForPersistence()
    let requestsDuringSave = await transport.requests()
    XCTAssertEqual(try persistenceToken(XCTUnwrap(pendingDuringSave)), CurrentHubTestData.tokenB)
    XCTAssertEqual(requestsDuringSave.count, 2)
    await store.releaseSave()
    await assertPersistenceStorageFailure(try await issuance.value)
    _ = try await client.claim(invitation: freshInvitation, deviceName: "Fresh claim after failed save")
    let stored = try await store.loadCredential()
    let requests = await transport.requests()
    XCTAssertEqual(try persistenceToken(XCTUnwrap(stored)), persistenceTokenC)
    XCTAssertEqual(requests.count, 3)
  }

  func testWriteThenThrowClaimSaveRetainsPendingAndRetriesWithoutIssuingAgain() async throws {
    try await verifyWriteThenThrowRecovery(rotating: false)
  }

  func testWriteThenThrowRotationSaveRetainsPendingAndRetriesWithoutIssuingAgain() async throws {
    try await verifyWriteThenThrowRecovery(rotating: true)
  }

  func testCancellationOfClaimSaveAndRetryRetainsPendingAndReleasesMutationGate() async throws {
    try await verifyCancellationRecovery(rotating: false)
  }

  func testCancellationOfRotationSaveAndRetryRetainsPendingAndReleasesMutationGate() async throws {
    try await verifyCancellationRecovery(rotating: true)
  }

  private func verifyWriteThenThrowRecovery(rotating: Bool) async throws {
    let clock = PersistenceRecoveryClock()
    let store = PersistenceRecoveryStore(
      initial: rotating ? try currentHubCredential() : nil,
      actions: [.writeThenThrow, .succeed]
    )
    let transport = try persistenceTransport(expiry: clock.now + 100)
    let client = try await persistenceClient(store: store, transport: transport, clock: clock)
    await assertPersistenceStorageFailure(try await issuePersistenceCredential(client, rotating: rotating))
    let pending = await client.pendingCredentialForPersistence()
    let storedAfterThrow = try await store.loadCredential()
    XCTAssertEqual(try persistenceToken(XCTUnwrap(pending)), CurrentHubTestData.tokenB)
    XCTAssertEqual(try persistenceToken(XCTUnwrap(storedAfterThrow)), CurrentHubTestData.tokenB)
    try await assertFreshPendingBlocksRemoteIssuance(client, transport: transport)
    _ = try await client.retryCredentialPersistence()
    let saves = await store.saveCount()
    let requestsBeforeRead = await transport.requests()
    let cleared = await client.pendingCredentialForPersistence()
    XCTAssertEqual(saves, 2)
    XCTAssertEqual(requestsBeforeRead.count, 2)
    XCTAssertNil(cleared)
    _ = try await client.vehicles()
    let requests = await transport.requests()
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenB)")
  }

  private func verifyCancellationRecovery(rotating: Bool) async throws {
    let clock = PersistenceRecoveryClock()
    let store = PersistenceRecoveryStore(
      initial: rotating ? try currentHubCredential() : nil,
      actions: [.cancelBeforeWrite, .writeThenCancel, .succeed]
    )
    let transport = try persistenceTransport(expiry: clock.now + 100)
    let client = try await persistenceClient(store: store, transport: transport, clock: clock)
    await assertPersistenceCancellation(try await issuePersistenceCredential(client, rotating: rotating))
    let firstPending = await client.pendingCredentialForPersistence()
    let initialStored = try await store.loadCredential()
    XCTAssertEqual(try persistenceToken(XCTUnwrap(firstPending)), CurrentHubTestData.tokenB)
    if rotating {
      XCTAssertEqual(try persistenceToken(XCTUnwrap(initialStored)), CurrentHubTestData.tokenA)
    } else {
      XCTAssertNil(initialStored)
    }
    try await assertFreshPendingBlocksRemoteIssuance(client, transport: transport)

    await assertPersistenceCancellation(try await client.retryCredentialPersistence())
    let retriedPending = await client.pendingCredentialForPersistence()
    let writtenBeforeCancellation = try await store.loadCredential()
    let requestsAfterCancellation = await transport.requests()
    XCTAssertEqual(try persistenceToken(XCTUnwrap(retriedPending)), CurrentHubTestData.tokenB)
    XCTAssertEqual(retriedPending?.expiresAtMilliseconds, firstPending?.expiresAtMilliseconds)
    XCTAssertEqual(try persistenceToken(XCTUnwrap(writtenBeforeCancellation)), CurrentHubTestData.tokenB)
    XCTAssertEqual(requestsAfterCancellation.count, 2)
    _ = try await client.retryCredentialPersistence()
    let saves = await store.saveCount()
    let cleared = await client.pendingCredentialForPersistence()
    let requestsBeforeRead = await transport.requests()
    XCTAssertEqual(saves, 3, "Each retry must enter the store again after cancellation releases the gate")
    XCTAssertNil(cleared)
    XCTAssertEqual(requestsBeforeRead.count, 2)
    _ = try await client.vehicles()
    let requests = await transport.requests()
    XCTAssertEqual(requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer \(CurrentHubTestData.tokenB)")
  }
}

private let persistenceTokenC = String(repeating: "c", count: 64)
private enum PersistenceRecoveryError: Error { case storageUnavailable }
private enum PersistenceSaveAction: Sendable {
  case succeed, writeThenThrow, cancelBeforeWrite, writeThenCancel, suspendThenThrow
}

private actor PersistenceRecoveryStore: CurrentHubCredentialStore {
  private var stored: CurrentHubCredential?
  private var actions: [PersistenceSaveAction]
  private var saves = 0
  private let onSuspendedSave: (@Sendable () -> Void)?
  private var saveContinuation: CheckedContinuation<Void, Never>?
  private var saveReleased = false

  init(
    initial: CurrentHubCredential? = nil,
    actions: [PersistenceSaveAction],
    onSuspendedSave: (@Sendable () -> Void)? = nil
  ) {
    stored = initial
    self.actions = actions
    self.onSuspendedSave = onSuspendedSave
  }

  func loadCredential() async throws -> CurrentHubCredential? { stored }

  func saveCredential(_ credential: CurrentHubCredential) async throws {
    saves += 1
    let action = actions.isEmpty ? .succeed : actions.removeFirst()
    switch action {
    case .succeed: stored = credential
    case .writeThenThrow:
      stored = credential
      throw PersistenceRecoveryError.storageUnavailable
    case .cancelBeforeWrite: throw CancellationError()
    case .writeThenCancel:
      stored = credential
      throw CancellationError()
    case .suspendThenThrow:
      onSuspendedSave?()
      if !saveReleased {
        await withCheckedContinuation { saveContinuation = $0 }
      }
      throw PersistenceRecoveryError.storageUnavailable
    }
  }

  func saveCount() -> Int { saves }
  func releaseSave() {
    saveReleased = true
    saveContinuation?.resume()
    saveContinuation = nil
  }
}

private final class PersistenceRecoveryClock: @unchecked Sendable {
  private let lock = NSLock()
  private var value: Int64 = 9_000_000_000_000
  var now: Int64 {
    lock.lock()
    defer { lock.unlock() }
    return value
  }
  func advance(by milliseconds: Int64) {
    lock.lock()
    defer { lock.unlock() }
    value += milliseconds
  }
}

private func persistenceClient(
  store: any CurrentHubCredentialStore,
  transport: any CurrentHubHTTPTransport,
  clock: PersistenceRecoveryClock
) async throws -> CurrentHubClient {
  try await CurrentHubClient.connectForTesting(
    endpoint: CurrentHubTestData.endpoint,
    expectedHubID: CurrentHubTestData.hubID,
    credentialStore: store,
    transport: transport,
    nowMilliseconds: { clock.now }
  )
}

private func persistenceTransport(expiry: Int64) throws -> ScriptedCurrentHubTransport {
  ScriptedCurrentHubTransport([
    CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery")),
    CurrentHubStubResponse(body: try persistenceClaimResponse(token: CurrentHubTestData.tokenB, expiry: expiry)),
    CurrentHubStubResponse(body: try CurrentHubTestData.fixture("vehicles")),
  ])
}

private func persistenceClaimResponse(token: String, expiry: Int64) throws -> Data {
  try JSONSerialization.data(withJSONObject: [
    "device_id": CurrentHubTestData.hubID.uuidString.lowercased(),
    "access_token": token, "expires_at_ms": expiry,
  ])
}

private func persistenceInvitation(pairingID: UUID = CurrentHubTestData.hubID) throws -> CurrentHubInvitation {
  var uri = URLComponents()
  uri.scheme = "teslatlas-hub"
  uri.host = "pair"
  uri.queryItems = [
    URLQueryItem(name: "endpoint", value: CurrentHubTestData.endpoint.absoluteString),
    URLQueryItem(name: "pairing_id", value: pairingID.uuidString.lowercased()),
    URLQueryItem(name: "secret", value: CurrentHubTestData.tokenA),
    URLQueryItem(name: "tls_pin", value: CurrentHubTestData.tokenA),
  ]
  return try JSONDecoder().decode(CurrentHubInvitation.self, from: JSONSerialization.data(withJSONObject: [
    "endpoint": CurrentHubTestData.endpoint.absoluteString,
    "pairingId": pairingID.uuidString.lowercased(), "expiresAtMs": Int64.max,
    "tlsPin": CurrentHubTestData.tokenA, "secret": CurrentHubTestData.tokenA,
    "pairingUri": try XCTUnwrap(uri.url).absoluteString,
  ]))
}

private func persistenceToken(_ credential: CurrentHubCredential) throws -> String {
  let archive = try credential.exportSecretArchive()
  let object = try XCTUnwrap(JSONSerialization.jsonObject(with: archive) as? [String: Any])
  return try XCTUnwrap(object["access_token"] as? String)
}

private func issuePersistenceCredential(_ client: CurrentHubClient, rotating: Bool) async throws -> CurrentHubCredential {
  if rotating { return try await client.rotateCredential() }
  return try await client.claim(invitation: persistenceInvitation(), deviceName: "Recovery Test")
}

private func assertFreshPendingBlocksRemoteIssuance(
  _ client: CurrentHubClient, transport: ScriptedCurrentHubTransport
) async throws {
  await assertCurrentHubError(try await client.rotateCredential()) { error in
    guard case .invalidRequest = error else { return XCTFail("Expected pending credential rejection") }
  }
  await assertCurrentHubError(try await client.claim(invitation: persistenceInvitation(), deviceName: "Repeat claim")) { error in
    guard case .invalidRequest = error else { return XCTFail("Expected pending credential rejection") }
  }
  let requests = await transport.requests()
  XCTAssertEqual(requests.count, 2)
}

private func assertPersistenceStorageFailure<T>(_ expression: @autoclosure () async throws -> T) async {
  do {
    _ = try await expression()
    XCTFail("Expected the store's persistence failure")
  } catch PersistenceRecoveryError.storageUnavailable {
  } catch { XCTFail("Unexpected persistence error: \(error)") }
}

private func assertPersistenceCancellation<T>(_ expression: @autoclosure () async throws -> T) async {
  do {
    _ = try await expression()
    XCTFail("Expected CancellationError from the store")
  } catch is CancellationError {
  } catch { XCTFail("Unexpected cancellation error: \(error)") }
}

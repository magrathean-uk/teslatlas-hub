import Foundation
import XCTest

@testable import TeslatlasHubSDK

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class PublicSDKRemediationTests: XCTestCase {
  func testAuthenticationOriginMismatchBlocksCredentialsUntilThatOriginRecovers()
    async throws
  {
    let transport = ScriptedHTTPTransport([
      discoveryResponse(TestDocuments.readDiscovery),
      discoveryResponse(changedIdentityDiscovery),
      discoveryResponse(TestDocuments.readDiscovery),
      discoveryResponse(TestDocuments.readDiscovery),
      currentResponse(),
    ])
    let client = try await makeClient(transport)
    let mismatchURL = try XCTUnwrap(
      URL(string: "https://HUB.EXAMPLE.INVALID:443/.well-known/teslatlas-hub")
    )
    await assertThrowsErrorAsync(try await client.refreshDiscovery(from: mismatchURL)) {
      self.assertIdentityMismatch($0)
    }
    await assertThrowsErrorAsync(try await client.vehicles()) {
      self.assertIdentityMismatch($0)
    }
    await assertThrowsErrorAsync(try await client.currentState(vehicleID: "vehicle_demo_alpha")) {
      self.assertIdentityMismatch($0)
    }
    await assertThrowsErrorAsync(try await client.drives(vehicleID: "vehicle_demo_alpha")) {
      self.assertIdentityMismatch($0)
    }
    await assertThrowsErrorAsync(try await client.eventRequest()) {
      self.assertIdentityMismatch($0)
    }
    let blockedRequestCount = await transport.requests.count
    XCTAssertEqual(blockedRequestCount, 2)
    let retainedHubID = await client.discovery.hubID
    XCTAssertEqual(retainedHubID, expectedHubID)

    // A correct document at a different trusted origin cannot clear the replacement.
    try await client.refreshDiscovery(from: roamingDiscoveryURL)
    await assertThrowsErrorAsync(try await client.currentState(vehicleID: "vehicle_demo_alpha")) {
      self.assertIdentityMismatch($0)
    }
    let stillBlockedRequestCount = await transport.requests.count
    XCTAssertEqual(stillBlockedRequestCount, 3)

    try await client.refreshDiscovery(from: TestURLs.discovery)
    let result = try await client.currentState(vehicleID: "vehicle_demo_alpha")
    guard case .modified(let state, _) = result else {
      return XCTFail("Expected recovered authenticated query")
    }
    XCTAssertEqual(state.revision, 42)
    let requests = await transport.requests
    XCTAssertEqual(requests.count, 5)
    XCTAssertTrue(requests.prefix(4).allSatisfy {
      $0.value(forHTTPHeaderField: "Authorization") == nil
    })
    XCTAssertEqual(requests[4].value(forHTTPHeaderField: "Authorization"), "Bearer retained-token")
  }

  func testUnrelatedTrustedOriginMismatchLeavesAuthenticationOriginUsable() async throws {
    let transport = ScriptedHTTPTransport([
      discoveryResponse(TestDocuments.readDiscovery),
      discoveryResponse(changedIdentityDiscovery),
      currentResponse(),
    ])
    let client = try await makeClient(transport)
    await assertThrowsErrorAsync(try await client.refreshDiscovery(from: roamingDiscoveryURL)) {
      self.assertIdentityMismatch($0)
    }
    _ = try await client.currentState(vehicleID: "vehicle_demo_alpha")
    let requests = await transport.requests
    XCTAssertEqual(requests.count, 3)
    XCTAssertEqual(requests[2].url?.host, "hub.example.invalid")
    XCTAssertEqual(requests[2].value(forHTTPHeaderField: "Authorization"), "Bearer retained-token")
  }

  func testSeparateAuthenticationOriginsRequireSeparateRevalidation() async throws {
    var object = try discoveryObject()
    var endpoints = try XCTUnwrap(object["endpoints"] as? [String: Any])
    endpoints["events"] = "https://vpn.example.invalid/v1/events"
    object["endpoints"] = endpoints
    let splitDiscovery = String(
      decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self
    )
    let replacement = splitDiscovery.replacingOccurrences(of: expectedHubID, with: changedHubID)
    let transport = ScriptedHTTPTransport([
      discoveryResponse(splitDiscovery), discoveryResponse(replacement),
      discoveryResponse(replacement), discoveryResponse(splitDiscovery),
      currentResponse(), discoveryResponse(splitDiscovery),
    ])
    let client = try await makeClient(transport)
    await assertThrowsErrorAsync(try await client.refreshDiscovery(from: TestURLs.discovery)) {
      self.assertIdentityMismatch($0)
    }
    await assertThrowsErrorAsync(try await client.refreshDiscovery(from: roamingDiscoveryURL)) {
      self.assertIdentityMismatch($0)
    }
    await assertThrowsErrorAsync(try await client.currentState(vehicleID: "vehicle_demo_alpha")) {
      self.assertIdentityMismatch($0)
    }
    await assertThrowsErrorAsync(try await client.eventRequest()) {
      self.assertIdentityMismatch($0)
    }
    let allBlockedRequestCount = await transport.requests.count
    XCTAssertEqual(allBlockedRequestCount, 3)

    try await client.refreshDiscovery(from: TestURLs.discovery)
    let result = try await client.currentState(vehicleID: "vehicle_demo_alpha")
    guard case .modified(let state, _) = result else {
      return XCTFail("Expected revalidated API origin to resume")
    }
    XCTAssertEqual(state.revision, 42)
    await assertThrowsErrorAsync(try await client.eventRequest()) {
      self.assertIdentityMismatch($0)
    }
    let partiallyRecovered = await transport.requests
    XCTAssertEqual(partiallyRecovered.count, 5)
    XCTAssertEqual(partiallyRecovered[4].url?.host, "hub.example.invalid")
    XCTAssertEqual(partiallyRecovered[4].value(forHTTPHeaderField: "Authorization"), "Bearer retained-token")

    try await client.refreshDiscovery(from: roamingDiscoveryURL)
    let eventRequest = try await client.eventRequest()
    XCTAssertEqual(eventRequest.url?.host, "vpn.example.invalid")
    XCTAssertEqual(eventRequest.value(forHTTPHeaderField: "Authorization"), "Bearer retained-token")
    let requests = await transport.requests
    XCTAssertEqual(requests.count, 6)
    XCTAssertTrue(requests.enumerated().filter { $0.offset != 4 }.allSatisfy {
      $0.element.value(forHTTPHeaderField: "Authorization") == nil
    })
  }

  func testRefreshIssuedBeforeMismatchCannotClearTheNewQuarantine() async throws {
    let transport = PublicSDKGatedIdentityTransport(
      correct: discoveryResponse(TestDocuments.readDiscovery),
      changed: discoveryResponse(changedIdentityDiscovery),
      query: currentResponse()
    )
    let client = try await makeClient(transport)
    let earlierRefresh = Task { try await client.refreshDiscovery(from: TestURLs.discovery) }
    do {
      try await transport.waitUntilRefreshIsBlocked()
      await assertThrowsErrorAsync(try await client.refreshDiscovery(from: TestURLs.discovery)) {
        self.assertIdentityMismatch($0)
      }
    } catch {
      await transport.releaseRefresh()
      _ = try? await earlierRefresh.value
      throw error
    }
    await transport.releaseRefresh()
    try await earlierRefresh.value
    await assertThrowsErrorAsync(try await client.currentState(vehicleID: "vehicle_demo_alpha")) {
      self.assertIdentityMismatch($0)
    }
    let blockedRequestCount = await transport.requests.count
    XCTAssertEqual(blockedRequestCount, 3)
    try await client.refreshDiscovery(from: TestURLs.discovery)
    _ = try await client.currentState(vehicleID: "vehicle_demo_alpha")
    let requests = await transport.requests
    XCTAssertEqual(requests.count, 5)
    XCTAssertTrue(requests.prefix(4).allSatisfy {
      $0.value(forHTTPHeaderField: "Authorization") == nil
    })
    XCTAssertEqual(requests[4].value(forHTTPHeaderField: "Authorization"), "Bearer retained-token")
  }

  func testNewMismatchDuringRecoveryPreventsThatRecoveryFromClearingQuarantine() async throws {
    let transport = PublicSDKGatedIdentityTransport(
      correct: discoveryResponse(TestDocuments.readDiscovery),
      changed: discoveryResponse(changedIdentityDiscovery),
      query: currentResponse(), gatedDiscoveryIndex: 3, mismatchIndices: [2, 4]
    )
    let client = try await makeClient(transport)
    await assertThrowsErrorAsync(try await client.refreshDiscovery(from: TestURLs.discovery)) {
      self.assertIdentityMismatch($0)
    }
    let recovery = Task { try await client.refreshDiscovery(from: TestURLs.discovery) }
    do {
      try await transport.waitUntilRefreshIsBlocked()
      await assertThrowsErrorAsync(try await client.refreshDiscovery(from: TestURLs.discovery)) {
        self.assertIdentityMismatch($0)
      }
    } catch {
      await transport.releaseRefresh()
      _ = try? await recovery.value
      throw error
    }
    await transport.releaseRefresh()
    try await recovery.value
    await assertThrowsErrorAsync(try await client.currentState(vehicleID: "vehicle_demo_alpha")) {
      self.assertIdentityMismatch($0)
    }
    let blockedRequestCount = await transport.requests.count
    XCTAssertEqual(blockedRequestCount, 4)
    try await client.refreshDiscovery(from: TestURLs.discovery)
    _ = try await client.currentState(vehicleID: "vehicle_demo_alpha")
    let requests = await transport.requests
    XCTAssertEqual(requests.count, 6)
    XCTAssertTrue(requests.prefix(5).allSatisfy {
      $0.value(forHTTPHeaderField: "Authorization") == nil
    })
    XCTAssertEqual(requests[5].value(forHTTPHeaderField: "Authorization"), "Bearer retained-token")
  }

  func testTransientRefreshFailureLeavesLastTrustedIdentityUsable() async throws {
    let transport = PublicSDKFailureTransport([
      .success(discoveryResponse(TestDocuments.readDiscovery)),
      .failure(.noResponse),
      .success(currentResponse()),
    ])
    let client = try await makeClient(transport)
    await assertThrowsErrorAsync(try await client.refreshDiscovery(from: TestURLs.discovery)) {
      XCTAssertTrue($0 is TestTransportError)
    }
    _ = try await client.currentState(vehicleID: "vehicle_demo_alpha")
    let requests = await transport.requests
    XCTAssertEqual(requests.count, 3)
    XCTAssertNil(requests[1].value(forHTTPHeaderField: "Authorization"))
    XCTAssertEqual(requests[2].value(forHTTPHeaderField: "Authorization"), "Bearer retained-token")
  }

  func testMalformedPageBoundsAreRejectedDuringDiscoveryAcceptance() async throws {
    let malformed: [(String, Int)] = [
      ("max_page_size", 0), ("max_page_size", -1), ("max_page_size", Int.min),
      ("max_page_size", 99), ("default_page_size", 0),
      ("default_page_size", -1), ("default_page_size", 501),
    ]
    for (field, value) in malformed {
      let document = try replacingLimit(field, with: value)
      XCTAssertThrowsError(try HubDiscoveryDecoder.decode(Data(document.utf8))) {
        XCTAssertEqual($0 as? TeslatlasDiscoveryError, .invalidDocument("invalid page size limits"))
      }
      let transport = ScriptedHTTPTransport([discoveryResponse(document)])
      await assertThrowsErrorAsync(try await makeClient(transport)) {
        XCTAssertEqual($0 as? TeslatlasDiscoveryError, .invalidDocument("invalid page size limits"))
      }
      let requests = await transport.requests
      XCTAssertEqual(requests.count, 1)
      XCTAssertNil(requests[0].value(forHTTPHeaderField: "Authorization"))
    }
  }

  func testPublishedMinimumPageAndRetentionBoundsRemainAccepted() throws {
    var object = try discoveryObject()
    var limits = try XCTUnwrap(object["limits"] as? [String: Any])
    limits["max_page_size"] = 100
    limits["default_page_size"] = 1
    limits["max_sse_connections"] = 1
    limits["event_replay_retention_seconds"] = 86400
    limits["idempotency_retention_seconds"] = 86400
    object["limits"] = limits
    let document = try HubDiscoveryDecoder.decode(JSONSerialization.data(withJSONObject: object))
    XCTAssertEqual(document.limits.maximumPageSize, 100)
    XCTAssertEqual(document.limits.defaultPageSize, 1)
    XCTAssertEqual(document.limits.maximumSSEConnections, 1)
    XCTAssertEqual(document.limits.eventReplayRetentionSeconds, 86400)
    XCTAssertEqual(document.limits.idempotencyRetentionSeconds, 86400)
  }

  func testHistoryPageBoundsReturnErrorsWithoutDispatch() async throws {
    let transport = ScriptedHTTPTransport([discoveryResponse(TestDocuments.readDiscovery)])
    let client = try await makeClient(transport)
    for limit in [Int.min, -1, 0, 501, Int.max] {
      await assertThrowsErrorAsync(
        try await client.drives(vehicleID: "vehicle_demo_alpha", request: HistoryRequest(limit: limit))
      ) {
        XCTAssertEqual(
          $0 as? TeslatlasSDKError,
          .limitExceeded(name: "limit", maximum: 500, actual: limit)
        )
      }
    }
    let requestCount = await transport.requests.count
    XCTAssertEqual(requestCount, 1)
  }

  func testConditional304RequiresSentValidatorAndMatchingOpaqueValue() async throws {
    let cases: [(String?, String)] = [
      (nil, "\"representation\""),
      ("\"before\"", "\"after\""),
      ("W/\"before\"", "\"after\""),
      ("\"before\"", "W/\"after\""),
    ]
    for (sent, returned) in cases {
      let transport = ScriptedHTTPTransport([
        discoveryResponse(TestDocuments.readDiscovery),
        notModifiedResponse(returned),
      ])
      let client = try await makeClient(transport)
      await assertThrowsErrorAsync(
        try await client.currentState(
          vehicleID: "vehicle_demo_alpha", ifNoneMatch: sent.map { EntityTag($0) }
        )
      ) {
        XCTAssertEqual(
          $0 as? TeslatlasSDKError,
          .invalidResponse(
            statusCode: 304, requestID: nil,
            reason: "304 response requires a matching If-None-Match validator"
          )
        )
      }
      let request = await transport.requests[1]
      XCTAssertEqual(request.value(forHTTPHeaderField: "If-None-Match"), sent)
    }
  }

  func testConditional304UsesWeakComparisonAndPreservesReturnedTag() async throws {
    for sent in ["\"opaque value\\witness\"", "W/\"opaque value\\witness\""] {
      for returned in ["\"opaque value\\witness\"", "W/\"opaque value\\witness\""] {
        let transport = ScriptedHTTPTransport([
          discoveryResponse(TestDocuments.readDiscovery),
          notModifiedResponse(returned),
        ])
        let client = try await makeClient(transport)
        let result = try await client.currentState(
          vehicleID: "vehicle_demo_alpha", ifNoneMatch: EntityTag(sent)
        )
        XCTAssertEqual(result, .notModified(EntityTag(returned)))
        let request = await transport.requests[1]
        XCTAssertEqual(request.value(forHTTPHeaderField: "If-None-Match"), sent)
      }
    }
  }

  func testConditional304TreatsCanonicallyEquivalentTagBytesAsDistinct() async throws {
    let pairs = [
      ("W/\"\u{00E9}\"", "\"e\u{0301}\""),
      ("\"e\u{0301}\"", "W/\"\u{00E9}\""),
    ]
    for (sent, returned) in pairs {
      let transport = ScriptedHTTPTransport([
        discoveryResponse(TestDocuments.readDiscovery), notModifiedResponse(returned),
      ])
      let client = try await makeClient(transport)
      await assertThrowsErrorAsync(
        try await client.currentState(vehicleID: "vehicle_demo_alpha", ifNoneMatch: EntityTag(sent))
      ) {
        XCTAssertEqual(
          $0 as? TeslatlasSDKError,
          .invalidResponse(
            statusCode: 304, requestID: nil,
            reason: "304 response requires a matching If-None-Match validator"
          )
        )
      }
      let request = await transport.requests[1]
      XCTAssertTrue(
        request.value(forHTTPHeaderField: "If-None-Match")?.utf8.elementsEqual(sent.utf8) == true
      )
    }
  }

  func testConditional200CanReplaceTheSentRepresentation() async throws {
    let transport = ScriptedHTTPTransport([
      discoveryResponse(TestDocuments.readDiscovery), currentResponse(eTag: "\"new\""),
    ])
    let client = try await makeClient(transport)
    let result = try await client.currentState(
      vehicleID: "vehicle_demo_alpha", ifNoneMatch: EntityTag("\"old\"")
    )
    guard case .modified(let value, let tag) = result else {
      return XCTFail("Expected replacing response")
    }
    XCTAssertEqual(value.revision, 42)
    XCTAssertEqual(tag, EntityTag("\"new\""))
  }

  func testSuspendedQueryUsesIssuedVersionWhileSubsequentQueryUsesRefresh()
    async throws
  {
    for responseVersion in ["1.2.0", "1.1.0"] {
      let transport = PublicSDKGatedVersionTransport(
        initial: discoveryResponse(TestDocuments.readDiscovery),
        refreshed: discoveryResponse(lowerVersionDiscovery),
        firstQuery: currentResponse(version: responseVersion),
        nextQuery: currentResponse(version: "1.1.0")
      )
      let client = try await makeClient(transport)
      let query = Task { try await client.currentState(vehicleID: "vehicle_demo_alpha") }
      do {
        try await transport.waitUntilQueryIsBlocked()
        try await client.refreshDiscovery(from: TestURLs.discovery)
      } catch {
        await transport.releaseQuery()
        _ = try? await query.value
        throw error
      }
      let selected = await client.selectedProtocolVersion.description
      XCTAssertEqual(selected, "1.1.0")
      await transport.releaseQuery()
      if responseVersion == "1.2.0" {
        guard case .modified(let value, _) = try await query.value else {
          return XCTFail("Expected valid issued-version response")
        }
        XCTAssertEqual(value.revision, 42)
      } else {
        await assertThrowsErrorAsync(try await query.value) {
          XCTAssertEqual(
            $0 as? TeslatlasSDKError,
            .invalidResponse(
              statusCode: 200, requestID: nil,
              reason: "server selected an unexpected protocol version"
            )
          )
        }
      }
      _ = try await client.currentState(vehicleID: "vehicle_demo_alpha")
      let requests = await transport.requests
      XCTAssertEqual(requests.count, 4)
      XCTAssertEqual(requests[1].value(forHTTPHeaderField: "Teslatlas-Protocol-Version"), "1.2.0")
      XCTAssertNil(requests[2].value(forHTTPHeaderField: "Authorization"))
      XCTAssertEqual(requests[3].value(forHTTPHeaderField: "Teslatlas-Protocol-Version"), "1.1.0")
    }
  }

  private var expectedHubID: String { "urn:uuid:018f18d2-6f45-7b3c-8a91-3c7286a10d42" }
  private var changedHubID: String { "urn:uuid:018f18d2-6f45-7b3c-8a91-3c7286a10d43" }
  private var roamingDiscoveryURL: URL {
    URL(string: "https://vpn.example.invalid/.well-known/teslatlas-hub")!
  }
  private var changedIdentityDiscovery: String {
    TestDocuments.readDiscovery.replacingOccurrences(of: expectedHubID, with: changedHubID)
  }
  private var lowerVersionDiscovery: String {
    TestDocuments.readDiscovery
      .replacingOccurrences(of: "\"current_version\": \"1.2.0\"", with: "\"current_version\": \"1.1.0\"")
      .replacingOccurrences(of: "[\"1.0.0\", \"1.1.0\", \"1.2.0\"]", with: "[\"1.0.0\", \"1.1.0\"]")
  }

  private func assertIdentityMismatch(_ error: Error) {
    XCTAssertEqual(
      error as? TeslatlasDiscoveryError,
      .hubIdentityChanged(expected: expectedHubID, actual: changedHubID)
    )
  }

  private func makeClient(_ transport: any TeslatlasHTTPTransport) async throws -> TeslatlasClient {
    try await TeslatlasClient.connect(
      discoveryURL: TestURLs.discovery,
      maximumProtocolVersion: XCTUnwrap(TeslatlasProtocolVersion("1.2.0")),
      additionalTrustedEndpointOrigins: [roamingDiscoveryURL],
      authorization: BearerCredential("retained-token"),
      transport: transport
    )
  }

  private func discoveryResponse(_ document: String) -> TeslatlasHTTPResponse {
    .json(status: 200, body: document, eTag: "\"discovery\"", version: "1.0.0")
  }

  private func currentResponse(version: String = "1.2.0", eTag: String = "\"current\"")
    -> TeslatlasHTTPResponse
  {
    .json(status: 200, body: TestDocuments.currentState, eTag: eTag, version: version)
  }

  private func notModifiedResponse(_ eTag: String) -> TeslatlasHTTPResponse {
    TeslatlasHTTPResponse(
      statusCode: 304, headers: TestHeaders.json(eTag: eTag, version: "1.2.0"), body: Data()
    )
  }

  private func discoveryObject() throws -> [String: Any] {
    try XCTUnwrap(
      JSONSerialization.jsonObject(with: Data(TestDocuments.readDiscovery.utf8)) as? [String: Any]
    )
  }

  private func replacingLimit(_ field: String, with value: Int) throws -> String {
    var object = try discoveryObject()
    var limits = try XCTUnwrap(object["limits"] as? [String: Any])
    limits[field] = value
    object["limits"] = limits
    return String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self)
  }
}

private actor PublicSDKFailureTransport: TeslatlasHTTPTransport {
  private var responses: [Result<TeslatlasHTTPResponse, TestTransportError>]
  private(set) var requests: [URLRequest] = []

  init(_ responses: [Result<TeslatlasHTTPResponse, TestTransportError>]) {
    self.responses = responses
  }

  func send(_ request: URLRequest) async throws -> TeslatlasHTTPResponse {
    requests.append(request)
    guard !responses.isEmpty else { throw TestTransportError.noResponse }
    return try responses.removeFirst().get()
  }
}

private actor PublicSDKGatedIdentityTransport: TeslatlasHTTPTransport {
  private let correct: TeslatlasHTTPResponse
  private let changed: TeslatlasHTTPResponse
  private let query: TeslatlasHTTPResponse
  private let gatedDiscoveryIndex: Int
  private let mismatchIndices: Set<Int>
  private var discoveryCount = 0
  private var blockedRefresh: CheckedContinuation<Void, Never>?
  private var refreshReleased = false
  private(set) var requests: [URLRequest] = []

  init(
    correct: TeslatlasHTTPResponse, changed: TeslatlasHTTPResponse, query: TeslatlasHTTPResponse,
    gatedDiscoveryIndex: Int = 2, mismatchIndices: Set<Int> = [3]
  ) {
    self.correct = correct
    self.changed = changed
    self.query = query
    self.gatedDiscoveryIndex = gatedDiscoveryIndex
    self.mismatchIndices = mismatchIndices
  }

  func send(_ request: URLRequest) async throws -> TeslatlasHTTPResponse {
    requests.append(request)
    guard request.url?.path == TestURLs.discovery.path else { return query }
    discoveryCount += 1
    if discoveryCount == gatedDiscoveryIndex {
      if !refreshReleased {
        await withCheckedContinuation { blockedRefresh = $0 }
      }
      return correct
    }
    return mismatchIndices.contains(discoveryCount) ? changed : correct
  }

  func waitUntilRefreshIsBlocked() async throws {
    let deadline = ContinuousClock.now.advanced(by: .seconds(5))
    while blockedRefresh == nil {
      guard ContinuousClock.now < deadline else { throw TestTransportError.noResponse }
      try await Task.sleep(for: .milliseconds(1))
    }
  }

  func releaseRefresh() {
    refreshReleased = true
    let continuation = blockedRefresh
    blockedRefresh = nil
    continuation?.resume()
  }
}

private actor PublicSDKGatedVersionTransport: TeslatlasHTTPTransport {
  private let initial: TeslatlasHTTPResponse
  private let refreshed: TeslatlasHTTPResponse
  private let firstQuery: TeslatlasHTTPResponse
  private let nextQuery: TeslatlasHTTPResponse
  private var discoveryCount = 0
  private var queryCount = 0
  private var blockedQuery: CheckedContinuation<Void, Never>?
  private var queryReleased = false
  private(set) var requests: [URLRequest] = []

  init(
    initial: TeslatlasHTTPResponse, refreshed: TeslatlasHTTPResponse,
    firstQuery: TeslatlasHTTPResponse, nextQuery: TeslatlasHTTPResponse
  ) {
    self.initial = initial
    self.refreshed = refreshed
    self.firstQuery = firstQuery
    self.nextQuery = nextQuery
  }

  func send(_ request: URLRequest) async throws -> TeslatlasHTTPResponse {
    requests.append(request)
    if request.url?.path == TestURLs.discovery.path {
      discoveryCount += 1
      return discoveryCount == 1 ? initial : refreshed
    }
    queryCount += 1
    if queryCount == 1 {
      if !queryReleased {
        await withCheckedContinuation { blockedQuery = $0 }
      }
      return firstQuery
    }
    return nextQuery
  }

  func waitUntilQueryIsBlocked() async throws {
    let deadline = ContinuousClock.now.advanced(by: .seconds(5))
    while blockedQuery == nil {
      guard ContinuousClock.now < deadline else { throw TestTransportError.noResponse }
      try await Task.sleep(for: .milliseconds(1))
    }
  }

  func releaseQuery() {
    queryReleased = true
    let continuation = blockedQuery
    blockedQuery = nil
    continuation?.resume()
  }
}

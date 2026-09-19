import XCTest
import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
import TeslatlasCurrentHub

@testable import CurrentHubConsumer

#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

final class CurrentHubConsumerTests: XCTestCase {
  func testConfigurationRequiresInvitationForInMemorySession() throws {
    let data = Data("""
      {"endpoint":"https://hub.example","expectedHubID":"11111111-1111-4111-8111-111111111111","deviceName":"test"}
      """.utf8)

    XCTAssertThrowsError(try ConsumerConfig.decode(from: data))
  }

  func testConfigurationRequiresPairedRestartMarkers() throws {
    let data = Data("""
      {"endpoint":"https://hub.example","expectedHubID":"11111111-1111-4111-8111-111111111111","invitationPath":"/private/invitation.json","deviceName":"test","restartReadyPath":"/private/ready"}
      """.utf8)

    XCTAssertThrowsError(try ConsumerConfig.decode(from: data))
  }

  func testPrivateFileReaderRejectsGroupReadableAndOversizedFiles() throws {
    let fileURL = FileManager.default.temporaryDirectory
      .appendingPathComponent("current-hub-consumer-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: fileURL) }
    try Data("private".utf8).write(to: fileURL)

    XCTAssertEqual(chmod(fileURL.path, 0o640), 0)
    XCTAssertThrowsError(
      try OwnerOnlyFileReader.read(path: fileURL.path, maximumBytes: 64)
    ) { error in
      XCTAssertEqual(error as? ConsumerPrivateInputError, .insecurePermissions)
    }

    XCTAssertEqual(chmod(fileURL.path, 0o600), 0)
    XCTAssertThrowsError(
      try OwnerOnlyFileReader.read(path: fileURL.path, maximumBytes: 4)
    ) { error in
      XCTAssertEqual(error as? ConsumerPrivateInputError, .tooLarge)
    }
  }

  func testDrivePagerReusesWindowAndRejectsCursorCycle() async throws {
    let vehicleID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    let firstCursor = try CurrentHubDriveCursor(rawValue: "cursor-a")
    let pages = [
      ConsumerDrivePage(itemCount: 2, nextCursor: firstCursor),
      ConsumerDrivePage(itemCount: 1, nextCursor: firstCursor),
    ]
    let recorder = QueryRecorder()

    do {
      _ = try await ConsumerDrivePager.fetch(
        vehicleID: vehicleID,
        fromMilliseconds: 100,
        toMilliseconds: 200,
        limit: 2,
        maximumPages: 3
      ) { query in
        let index = await recorder.append(query)
        return pages[index]
      }
      XCTFail("Expected a cursor cycle")
    } catch let error as ConsumerError {
      XCTAssertEqual(error, .paginationCursorCycle)
    }

    let queries = await recorder.values()
    XCTAssertEqual(queries.count, 2)
    XCTAssertEqual(queries[0].fromMilliseconds, 100)
    XCTAssertEqual(queries[0].toMilliseconds, 200)
    XCTAssertEqual(queries[0].limit, 2)
    XCTAssertNil(queries[0].cursor)
    XCTAssertEqual(queries[1].fromMilliseconds, 100)
    XCTAssertEqual(queries[1].toMilliseconds, 200)
    XCTAssertEqual(queries[1].limit, 2)
    XCTAssertEqual(queries[1].cursor, firstCursor)
  }

  func testDrivePagerStopsAtMaximumPages() async throws {
    let vehicleID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    let firstCursor = try CurrentHubDriveCursor(rawValue: "cursor-a")
    let secondCursor = try CurrentHubDriveCursor(rawValue: "cursor-b")
    let pages = [
      ConsumerDrivePage(itemCount: 2, nextCursor: firstCursor),
      ConsumerDrivePage(itemCount: 2, nextCursor: secondCursor),
    ]
    let pageProvider = PageProvider(pages: pages)

    do {
      _ = try await ConsumerDrivePager.fetch(
        vehicleID: vehicleID,
        fromMilliseconds: nil,
        toMilliseconds: nil,
        limit: 2,
        maximumPages: 2
      ) { _ in
        try await pageProvider.next()
      }
      XCTFail("Expected the page bound to be enforced")
    } catch let error as ConsumerError {
      XCTAssertEqual(error, .paginationLimitExceeded)
    }
  }

  func testDrivePagerReportsPageAndConditionalCounts() async throws {
    let vehicleID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    let firstCursor = try CurrentHubDriveCursor(rawValue: "cursor-a")
    let pages = [
      ConsumerDrivePage(
        itemCount: 2,
        nextCursor: firstCursor,
        conditionalNotModified: true
      ),
      ConsumerDrivePage(
        itemCount: 1,
        nextCursor: nil,
        conditionalNotModified: true
      ),
    ]
    let provider = PageProvider(pages: pages)

    let result = try await ConsumerDrivePager.fetch(
      vehicleID: vehicleID,
      fromMilliseconds: nil,
      toMilliseconds: nil,
      limit: 2,
      maximumPages: 3
    ) { _ in
      try await provider.next()
    }

    XCTAssertEqual(result.pageCount, 2)
    XCTAssertEqual(result.driveCount, 3)
    XCTAssertEqual(result.pageItemCounts, [2, 1])
    XCTAssertEqual(result.conditionalNotModifiedCount, 2)
  }

  func testClearingCredentialStoreRejectsLateSave() async throws {
    let store = InMemoryCredentialStore()
    await store.clear()

    do {
      try await store.saveCredential(try credential(token: String(repeating: "a", count: 64)))
      XCTFail("Expected a closed store to reject a late save")
    } catch let error as ConsumerError {
      XCTAssertEqual(error, .sessionClosed)
    }
    let storedCredential = try await store.loadCredential()
    XCTAssertNil(storedCredential)
  }

  func testSafeErrorStatusOmitsSensitiveDetails() {
    let status = ConsumerStatus.forError(
      CurrentHubError.api(
        statusCode: 503,
        code: "service_unavailable",
        message: "secret bearer and invitation details",
        requestID: "private-request-id"
      )
    )
    XCTAssertEqual(status, "api_error_503")
    XCTAssertFalse(status.contains("secret"))
    XCTAssertFalse(status.contains("private-request-id"))
  }

  func testCancellationClearsAndClosesTheSession() async throws {
    let transport = RecordingConsumerTransport(
      responses: [Response(body: discoveryJSON())],
      delayNanoseconds: 0
    )
    let session = try await CurrentHubConsumerSession.connect(
      endpoint: TestData.endpoint,
      expectedHubID: TestData.hubID,
      transport: transport
    )
    let operation = Task {
      try await ConsumerLifecycle.execute(session: session) {
        try await Task.sleep(nanoseconds: 1_000_000_000)
        return true
      }
    }
    try await Task.sleep(nanoseconds: 20_000_000)
    operation.cancel()

    let result = await operation.result
    guard case .failure = result else {
      XCTFail("Expected cancellation")
      return
    }
    do {
      _ = try await session.health()
      XCTFail("Expected the cleaned-up session to reject new work")
    } catch let error as ConsumerError {
      XCTAssertEqual(error, .sessionClosed)
    }
  }

  func testCancelledQueuedOperationDoesNotReachTheTransport() async throws {
    let transport = RecordingConsumerTransport(
      responses: [
        Response(body: discoveryJSON()),
        Response(body: healthJSON()),
        Response(body: healthJSON()),
      ],
      delayNanoseconds: 100_000_000
    )
    let session = try await CurrentHubConsumerSession.connect(
      endpoint: TestData.endpoint,
      expectedHubID: TestData.hubID,
      transport: transport
    )
    let first = Task { try await session.health() }
    for _ in 0..<100 where await transport.activeRequestCount() == 0 {
      try await Task.sleep(nanoseconds: 1_000_000)
    }
    let second = Task { try await session.health() }
    second.cancel()

    _ = try await first.value
    let secondResult = await second.result
    guard case .failure = secondResult else {
      XCTFail("Expected the queued operation to be cancelled")
      return
    }
    let paths = await transport.paths()
    XCTAssertEqual(paths.count, 2)
    await session.closeAndClear()
  }

  func testSessionSerializesClaimAndRotation() async throws {
    let transport = RecordingConsumerTransport(
      responses: [
        Response(body: discoveryJSON()),
        Response(body: claimJSON(token: "a")),
        Response(body: claimJSON(token: "b")),
      ],
      delayNanoseconds: 20_000_000
    )
    let session = try await CurrentHubConsumerSession.connect(
      endpoint: TestData.endpoint,
      expectedHubID: TestData.hubID,
      transport: transport
    )
    let invitation = try invitation()

    let claim = Task {
      try await session.claim(invitation: invitation, deviceName: "consumer-test")
    }
    var claimRequestObserved = false
    for _ in 0..<100 {
      let paths = await transport.paths()
      if paths.contains("/v1/pairings/11111111-1111-4111-8111-111111111111/claim") {
        claimRequestObserved = true
        break
      }
      try await Task.sleep(nanoseconds: 1_000_000)
    }
    XCTAssertTrue(claimRequestObserved)
    let rotation = Task {
      try await session.rotateCredential()
    }
    _ = try await claim.value
    _ = try await rotation.value

    let maximumConcurrentRequests = await transport.maximumConcurrentRequests()
    let paths = await transport.paths()
    XCTAssertEqual(maximumConcurrentRequests, 1)
    XCTAssertEqual(
      paths,
      [
        "/.well-known/teslatlas-hub",
        "/v1/pairings/11111111-1111-4111-8111-111111111111/claim",
        "/v1/device/rotate",
      ]
    )
    await session.closeAndClear()
  }
}

private actor QueryRecorder {
  private var queries: [CurrentHubDriveQuery] = []

  func append(_ query: CurrentHubDriveQuery) -> Int {
    queries.append(query)
    return queries.count - 1
  }
  func count() -> Int { queries.count }
  func values() -> [CurrentHubDriveQuery] { queries }
}

private actor PageProvider {
  private let pages: [ConsumerDrivePage]
  private var index = 0

  init(pages: [ConsumerDrivePage]) { self.pages = pages }

  func next() throws -> ConsumerDrivePage {
    defer { index += 1 }
    guard index < pages.count else { throw ConsumerTestError.noResponse }
    return pages[index]
  }
}

private enum TestData {
  static let endpoint = URL(string: "https://hub.example.invalid")!
  static let hubID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
}

private struct Response: Sendable {
  let statusCode: Int = 200
  let headers: [String: String] = [
    "Content-Type": "application/json",
    "Cache-Control": "no-store",
    "ETag": "\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"",
  ]
  let body: Data
}

private actor RecordingConsumerTransport: CurrentHubInvitationPinningTransport {
  private var responses: [Response]
  private let delayNanoseconds: UInt64
  private var requestPaths: [String] = []
  private var activeRequests = 0
  private var maximumActiveRequests = 0

  init(responses: [Response], delayNanoseconds: UInt64) {
    self.responses = responses
    self.delayNanoseconds = delayNanoseconds
  }

  func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse {
    try await sendResponse(for: request)
  }

  func send(
    _ request: URLRequest,
    validatingLeafCertificateSHA256 _: String
  ) async throws -> CurrentHubHTTPResponse {
    try await sendResponse(for: request)
  }

  func paths() -> [String] { requestPaths }
  func maximumConcurrentRequests() -> Int { maximumActiveRequests }
  func activeRequestCount() -> Int { activeRequests }

  private func sendResponse(for request: URLRequest) async throws -> CurrentHubHTTPResponse {
    guard let url = request.url, !responses.isEmpty else {
      throw ConsumerTestError.noResponse
    }
    requestPaths.append(url.path)
    activeRequests += 1
    maximumActiveRequests = max(maximumActiveRequests, activeRequests)
    defer { activeRequests -= 1 }
    try await Task.sleep(nanoseconds: delayNanoseconds)
    let response = responses.removeFirst()
    return CurrentHubHTTPResponse(
      statusCode: response.statusCode,
      headers: response.headers,
      body: response.body,
      finalURL: url
    )
  }
}

private enum ConsumerTestError: Error { case noResponse }

private func credential(token: String) throws -> CurrentHubCredential {
  try CurrentHubCredential(
    deviceID: TestData.hubID,
    accessToken: token,
    expiresAtMilliseconds: 4_102_444_800_000
  )
}

private func discoveryJSON() -> Data {
  Data("""
    {"api_versions":["1.0"],"capabilities":["query.vehicles","query.current","query.drives","sync.packs"],"hub_id":"11111111-1111-4111-8111-111111111111","pack_format":"sqlite-zstd","protocol":"teslatlas-sync","protocol_major":1,"sourceUrl":"https://example.invalid/source","version":"2026.36.2"}
    """.utf8)
}

private func healthJSON() -> Data {
  Data("""
    {"status":"ok","version":"2026.36.2"}
    """.utf8)
}

private func claimJSON(token: String) -> Data {
  Data("""
    {"access_token":"\(String(repeating: token, count: 64))","device_id":"11111111-1111-4111-8111-111111111111","expires_at_ms":4102444800000}
    """.utf8)
}

private func invitation() throws -> CurrentHubInvitation {
  let secret = String(repeating: "0", count: 64)
  let pin = String(repeating: "1", count: 64)
  let data = Data("""
    {"endpoint":"https://hub.example.invalid","expiresAtMs":4102444800000,"pairingId":"11111111-1111-4111-8111-111111111111","pairingUri":"teslatlas-hub://pair?endpoint=https%3A%2F%2Fhub.example.invalid&pairing_id=11111111-1111-4111-8111-111111111111&secret=\(secret)&tls_pin=\(pin)","secret":"\(secret)","tlsPin":"\(pin)"}
    """.utf8)
  return try JSONDecoder().decode(CurrentHubInvitation.self, from: data)
}

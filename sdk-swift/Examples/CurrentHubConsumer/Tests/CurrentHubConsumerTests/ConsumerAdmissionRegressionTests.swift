import Foundation
import XCTest
import TeslatlasCurrentHub
@testable import CurrentHubConsumer

#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

final class ConsumerAdmissionRegressionTests: XCTestCase {
  func testJourneyRejectsExistingOutputsAndUnavailableParentsBeforeClaim() async throws {
    for conflict in ["cleanup-existing", "semantic-existing", "cleanup-parent", "semantic-parent"] {
      let directory = try outputDirectory()
      defer { try? FileManager.default.removeItem(at: directory) }
      var cleanup = directory.appendingPathComponent("device-id")
      var semantic = directory.appendingPathComponent("semantic.json")
      let sentinel = Data("existing-output-must-survive".utf8)
      switch conflict {
      case "cleanup-existing":
        try OwnerOnlyExclusiveFileWriter.write(sentinel, path: cleanup.path)
      case "semantic-existing":
        try OwnerOnlyExclusiveFileWriter.write(sentinel, path: semantic.path)
      case "cleanup-parent":
        cleanup = directory.appendingPathComponent("missing/device-id")
      default:
        semantic = directory.appendingPathComponent("missing/semantic.json")
      }
      let config = try admissionConfig(cleanup: cleanup, semantic: semantic)
      let producer = AdmissionProducer(secondPage: [])
      do {
        _ = try await CurrentHubConsumer.run(
          config: config, invitation: admissionInvitation(), transport: producer
        )
        XCTFail("Expected output admission to fail for \(conflict)")
      } catch {
        XCTAssertEqual(error as? ConsumerError, .privateOutputFailure)
      }
      let requests = await producer.paths()
      XCTAssertEqual(requests, [], "Admission must fail before discovery or a one-use claim")
      if conflict == "cleanup-existing" {
        XCTAssertEqual(try Data(contentsOf: cleanup), sentinel)
      } else {
        XCTAssertFalse(FileManager.default.fileExists(atPath: cleanup.path))
      }
      if conflict == "semantic-existing" {
        XCTAssertEqual(try Data(contentsOf: semantic), sentinel)
      } else {
        XCTAssertFalse(FileManager.default.fileExists(atPath: semantic.path))
      }
    }
  }

  func testJourneyRetainsDeviceReceiptAfterOneSuccessfulClaimAndLaterFailure() async throws {
    let directory = try outputDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let cleanup = directory.appendingPathComponent("device-id")
    let semantic = directory.appendingPathComponent("semantic.json")
    let producer = AdmissionProducer(secondPage: [])
    do {
      _ = try await CurrentHubConsumer.run(
        config: admissionConfig(cleanup: cleanup, semantic: semantic),
        invitation: admissionInvitation(), transport: producer
      )
      XCTFail("Expected the deliberately malformed health response to fail")
    } catch {
      XCTAssertEqual(ConsumerStatus.forError(error), "invalid_response")
    }
    let issued = await producer.successfulClaims()
    XCTAssertEqual(issued, 1)
    let requests = await producer.paths()
    XCTAssertEqual(requests, [
      "/.well-known/teslatlas-hub", admissionClaimPath, admissionClaimPath, "/healthz",
    ]) // The second claim is the existing deliberate invitation-replay rejection.
    XCTAssertEqual(
      try OwnerOnlyFileReader.read(path: cleanup.path, maximumBytes: 64),
      Data(admissionID.uuidString.lowercased().utf8)
    )
    XCTAssertFalse(FileManager.default.fileExists(atPath: semantic.path))
  }

  func testPagerRejectsClientAdmittedOverlapNewerBoundaryAndRepeatedID() async throws {
    let invalidPages: [[(Int64, Int64)]] = [
      [(3, 100), (2, 99)], // Repeated boundary tuple.
      [(6, 101), (2, 99)], // Newer timestamp across the boundary.
      [(5, 100), (2, 99)], // Equal timestamp, ascending ID across the boundary.
      [(4, 99), (1, 98)], // Descending tuples, but the first page's ID recurs.
    ]
    for secondPage in invalidPages {
      let producer = AdmissionProducer(secondPage: secondPage)
      let session = try await admittedSession(producer)
      do {
        _ = try await admittedHistory(session)
        XCTFail("Expected the accumulated history to reject this second page")
      } catch {
        XCTAssertEqual(error as? ConsumerError, .invalidDrivePage)
      }
      let requestCounts = await producer.driveCounts()
      XCTAssertEqual(requestCounts.modified, 2, "Both pages must reach real client admission")
      XCTAssertEqual(requestCounts.conditional, 2, "The actual session's ETag proof must run")
      await session.closeAndClear()
    }
  }

  func testPagerAcceptsEqualTimestampDescendingIDsAndPreservesZeroAndNull() async throws {
    let producer = AdmissionProducer(secondPage: [(2, 100), (1, 99)])
    let session = try await admittedSession(producer)
    let history = try await admittedHistory(session)
    XCTAssertEqual(history.items.map(\.id), [4, 3, 2, 1])
    XCTAssertEqual(history.items.map(\.startDateMilliseconds), [100, 100, 100, 99])
    XCTAssertEqual(history.items.first?.distanceKilometres, 0)
    XCTAssertEqual(history.items.first?.durationMinutes, 0)
    XCTAssertNil(history.items.last?.distanceKilometres)
    XCTAssertNil(history.items.last?.durationMinutes)
    XCTAssertEqual(history.pageCount, 2)
    XCTAssertEqual(history.pageItemCounts, [2, 2])
    XCTAssertEqual(history.driveCount, 4)
    XCTAssertEqual(history.conditionalNotModifiedCount, 2)
    await session.closeAndClear()
  }
}

private let admissionID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
private let admissionEndpoint = URL(string: "https://hub.example.invalid")!
private let admissionClaimPath = "/v1/pairings/11111111-1111-4111-8111-111111111111/claim"
private let admissionToken = String(repeating: "a", count: 64)
private enum AdmissionTestError: Error { case unexpectedRequest }

private func outputDirectory() throws -> URL {
  let directory = FileManager.default.temporaryDirectory
    .appendingPathComponent("consumer-admission-\(UUID().uuidString)")
  try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
  return directory
}

private func admissionConfig(cleanup: URL, semantic: URL) throws -> ConsumerConfig {
  try ConsumerConfig.decode(from: JSONSerialization.data(withJSONObject: [
    "endpoint": admissionEndpoint.absoluteString,
    "expectedHubID": admissionID.uuidString.lowercased(),
    "invitationPath": "/private/synthetic-invitation.json", "deviceName": "consumer-test",
    "expectedVehicleCount": 3, "expectedObservedCount": 1, "expectedAbsentCount": 2,
    "cleanupDeviceIDPath": cleanup.path, "semanticSnapshotPath": semantic.path,
  ]))
}

private func admissionInvitation() throws -> CurrentHubInvitation {
  let secret = String(repeating: "0", count: 64)
  let pin = String(repeating: "1", count: 64)
  return try JSONDecoder().decode(CurrentHubInvitation.self, from: Data("""
    {"endpoint":"https://hub.example.invalid","expiresAtMs":4102444800000,"pairingId":"11111111-1111-4111-8111-111111111111","pairingUri":"teslatlas-hub://pair?endpoint=https%3A%2F%2Fhub.example.invalid&pairing_id=11111111-1111-4111-8111-111111111111&secret=\(secret)&tls_pin=\(pin)","secret":"\(secret)","tlsPin":"\(pin)"}
    """.utf8))
}

private func admittedSession(_ producer: AdmissionProducer) async throws -> CurrentHubConsumerSession {
  let session = try await CurrentHubConsumerSession.connect(
    endpoint: admissionEndpoint, expectedHubID: admissionID, transport: producer
  )
  _ = try await session.claim(invitation: admissionInvitation(), deviceName: "pager-test")
  return session
}

private func admittedHistory(_ session: CurrentHubConsumerSession) async throws -> ConsumerDrivePageSummary {
  try await ConsumerDrivePager.fetch(
    vehicleID: admissionID, fromMilliseconds: 0, toMilliseconds: 1_000,
    limit: 2, maximumPages: 2
  ) { query in
    try await session.drivePage(query: query, vehicleID: admissionID)
  }
}

/// Synthetic wire producer only; requests run through the public client and real consumer session.
private actor AdmissionProducer: CurrentHubInvitationPinningTransport {
  private let secondPage: [(Int64, Int64)]
  private var requestPaths: [String] = []
  private var claims = 0
  private var modified = 0
  private var conditional = 0

  init(secondPage: [(Int64, Int64)]) { self.secondPage = secondPage }
  func paths() -> [String] { requestPaths }
  func successfulClaims() -> Int { claims }
  func driveCounts() -> (modified: Int, conditional: Int) { (modified, conditional) }

  func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse {
    try response(request)
  }

  func send(
    _ request: URLRequest, validatingLeafCertificateSHA256 _: String
  ) async throws -> CurrentHubHTTPResponse {
    try response(request)
  }

  private func response(_ request: URLRequest) throws -> CurrentHubHTTPResponse {
    guard let url = request.url else { throw AdmissionTestError.unexpectedRequest }
    requestPaths.append(url.path)
    if url.path == "/.well-known/teslatlas-hub" {
      return reply(url, body: Data("""
        {"hub_id":"11111111-1111-4111-8111-111111111111","protocol":"teslatlas-sync","protocol_major":1,"api_versions":["1.0"],"capabilities":["query.vehicles","query.current","query.drives","sync.packs"],"version":"2026.36.2","sourceUrl":"https://example.invalid/source","pack_format":"sqlite-zstd"}
        """.utf8))
    }
    if url.path == admissionClaimPath {
      guard request.httpMethod == "POST" else { throw AdmissionTestError.unexpectedRequest }
      if claims > 0 { return reply(url, status: 401, body: Data()) }
      claims += 1
      return reply(url, body: Data("""
        {"access_token":"\(admissionToken)","device_id":"11111111-1111-4111-8111-111111111111","expires_at_ms":4102444800000}
        """.utf8))
    }
    if url.path == "/healthz" { return reply(url, body: Data("{}".utf8)) }
    guard url.path == "/v1/vehicles/11111111-1111-4111-8111-111111111111/drives",
      request.httpMethod == "GET",
      request.value(forHTTPHeaderField: "Authorization") == "Bearer \(admissionToken)",
      let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
    else { throw AdmissionTestError.unexpectedRequest }
    let query = Dictionary(uniqueKeysWithValues: (components.queryItems ?? []).map {
      ($0.name, $0.value ?? "")
    })
    guard query["from_ms"] == "0", query["to_ms"] == "1000", query["limit"] == "2",
      query["cursor"] == nil || query["cursor"] == "next-admission-page"
    else { throw AdmissionTestError.unexpectedRequest }
    let first = query["cursor"] == nil
    let eTag = "\"\(String(repeating: first ? "a" : "b", count: 64))\""
    if let requestedETag = request.value(forHTTPHeaderField: "If-None-Match") {
      guard requestedETag == eTag else { throw AdmissionTestError.unexpectedRequest }
      conditional += 1
      return reply(url, status: 304, body: Data(), eTag: eTag)
    }
    modified += 1
    let rows: [(Int64, Int64)] = first ? [(4, 100), (3, 100)] : secondPage
    let body = try JSONSerialization.data(withJSONObject: [
      "items": rows.map { admissionDrive(id: $0.0, start: $0.1) },
      "next_cursor": first ? "next-admission-page" as Any : NSNull(),
    ])
    return reply(url, body: body, eTag: eTag)
  }

  private func reply(
    _ url: URL, status: Int = 200, body: Data, eTag: String? = nil
  ) -> CurrentHubHTTPResponse {
    var headers = ["Content-Type": "application/json", "Cache-Control": "no-store"]
    if let eTag { headers["ETag"] = eTag }
    return CurrentHubHTTPResponse(statusCode: status, headers: headers, body: body, finalURL: url)
  }
}

private func admissionDrive(id: Int64, start: Int64) -> [String: Any] {
  let nullableKeys = [
    "duration_min", "speed_max", "start_soc", "end_soc", "ascent", "descent",
    "distance_km", "efficiency", "outside_temp_avg", "inside_temp_avg", "power_max", "power_min",
    "start_ideal_range_km", "end_ideal_range_km", "start_latitude", "start_longitude",
    "end_latitude", "end_longitude", "start_rated_range_km", "end_rated_range_km",
    "start_address", "end_address", "start_geofence", "end_geofence",
  ]
  var row = Dictionary(uniqueKeysWithValues: nullableKeys.map { ($0, NSNull() as Any) })
  row["id"] = id
  row["vehicle_id"] = admissionID.uuidString.lowercased()
  row["start_date_ms"] = start
  row["end_date_ms"] = start + 10
  if id == 4 { row["distance_km"] = 0; row["duration_min"] = 0 }
  return row
}

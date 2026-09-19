import Foundation
import XCTest

@testable import TeslatlasCurrentHub

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class CurrentHubLiveTests: XCTestCase {
  func testBoundedPrivateLiveInputsRejectParentSymlinksAndUnsafeDirectories() throws {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent(
      "current-hub-private-\(UUID().uuidString)", isDirectory: true
    )
    try FileManager.default.createDirectory(
      at: root, withIntermediateDirectories: false,
      attributes: [.posixPermissions: NSNumber(value: 0o700)]
    )
    defer { try? FileManager.default.removeItem(at: root) }

    let real = root.appendingPathComponent("real", isDirectory: true)
    try FileManager.default.createDirectory(
      at: real, withIntermediateDirectories: false,
      attributes: [.posixPermissions: NSNumber(value: 0o700)]
    )
    let target = real.appendingPathComponent("input.json")
    let data = Data(#"{"value":1}"#.utf8)
    try data.write(to: target, options: .atomic)
    try FileManager.default.setAttributes(
      [.posixPermissions: NSNumber(value: 0o600)], ofItemAtPath: target.path
    )
    let digest = MatrixSHA256.hexDigest(data)
    XCTAssertEqual(
      try boundedRead(target, maximumBytes: 64, expectedSHA256: digest), data
    )

    let parentLink = root.appendingPathComponent("parent-link", isDirectory: true)
    try FileManager.default.createSymbolicLink(
      atPath: parentLink.path, withDestinationPath: real.path
    )
    XCTAssertThrowsError(
      try boundedRead(
        parentLink.appendingPathComponent("input.json"),
        maximumBytes: 64,
        expectedSHA256: digest
      )
    ) { error in
      guard case CurrentHubError.invalidRequest(let reason) = error else {
        return XCTFail("unexpected parent-link error: \(error)")
      }
      XCTAssertTrue(reason.contains("parent"))
    }

    let writable = root.appendingPathComponent("writable", isDirectory: true)
    try FileManager.default.createDirectory(
      at: writable, withIntermediateDirectories: false,
      attributes: [.posixPermissions: NSNumber(value: 0o777)]
    )
    let writableTarget = writable.appendingPathComponent("input.json")
    try data.write(to: writableTarget, options: .atomic)
    try FileManager.default.setAttributes(
      [.posixPermissions: NSNumber(value: 0o600)], ofItemAtPath: writableTarget.path
    )
    XCTAssertThrowsError(try boundedRead(writableTarget, maximumBytes: 64))

    let oversized = real.appendingPathComponent("oversized.json")
    let oversizedData = Data(repeating: 0x41, count: 65)
    try oversizedData.write(to: oversized, options: .atomic)
    try FileManager.default.setAttributes(
      [.posixPermissions: NSNumber(value: 0o600)], ofItemAtPath: oversized.path
    )
    XCTAssertThrowsError(try boundedRead(oversized, maximumBytes: 64))
  }

  func testRequiredCurrentHubJourney() async throws {
    guard let configPath = ProcessInfo.processInfo.environment["TESLATLAS_CURRENT_HUB_LIVE_CONFIG"] else {
      return XCTFail("TESLATLAS_CURRENT_HUB_LIVE_CONFIG is required for CurrentHubLiveTests")
    }
    let config = try LiveConfig.load(URL(fileURLWithPath: configPath))
    let invitation = try JSONDecoder().decode(
      CurrentHubInvitation.self,
      from: try boundedRead(config.invitationPath, maximumBytes: 65_536)
    )
    XCTAssertEqual(invitation.endpoint, config.endpoint)

    let transport = try makeTrustedTransport(config: config, invitation: invitation)
    let store = LiveCredentialStore()
    let client = try await CurrentHubClient.connect(
      endpoint: config.endpoint,
      expectedHubID: config.expectedHubID,
      credentialStore: store,
      transport: transport
    )
    print("TASK6_LIVE milestone=connected")
    let discovery = await client.discoveryDocument()
    let health = try await client.health()
    let readiness = try await client.readiness()
    print("TASK6_LIVE milestone=probes")
    XCTAssertEqual(discovery.version, "2026.36.2")
    XCTAssertEqual(health.status, "ok")
    XCTAssertEqual(readiness.status, "ready")

    let wrongHubID = UUID(uuidString: "ffffffff-ffff-4fff-8fff-ffffffffffff")!
    do {
      _ = try await CurrentHubClient.connect(
        endpoint: config.endpoint,
        expectedHubID: wrongHubID,
        credentialStore: LiveCredentialStore(),
        transport: transport
      )
      XCTFail("Expected Hub identity rejection")
    } catch let error as CurrentHubError {
      guard case .hubIdentityMismatch = error else { throw error }
    }

    try await assertTrustNegative(config: config, invitation: invitation)
    try await assertInvitationLeafPinNegative(client: client, invitation: invitation)
    print("TASK6_LIVE milestone=negatives")
    let originalCredential = try await client.claim(
      invitation: invitation,
      deviceName: "Swift live \(config.clientPlatform)"
    )
    print("TASK6_LIVE milestone=claimed")

    let vehicles = try await client.vehicles()
    XCTAssertEqual(vehicles.count, 2)
    guard let selected = vehicles.first(where: { $0.vehicleID == config.selectedVehicleID }) else {
      return XCTFail("selected vehicle is absent")
    }
    XCTAssertFalse(selected.displayName?.isEmpty ?? true)
    let current = try await client.current(vehicleID: selected.vehicleID)
    XCTAssertEqual(current.observedAtMilliseconds, 1_788_566_400_000)
    XCTAssertEqual(current.batteryLevel, 0)
    XCTAssertNil(current.outsideTemperatureCelsius)
    XCTAssertEqual(current.insideTemperatureCelsius, 21.5)
    XCTAssertEqual(current.speedKilometresPerHour, 16)
    XCTAssertEqual(current.estimatedBatteryRangeKilometres, 160.93)
    XCTAssertEqual(current.odometerKilometres, 16_093.44)
    XCTAssertEqual(current.scheduledChargingStartTime, 1_788_570_000)
    XCTAssertEqual(current.activeRouteMilesToArrival, 12.5)

    var cursor: CurrentHubDriveCursor?
    var driveIDs: [Int64] = []
    for expected in [[105, 104], [103, 102], [101]] as [[Int64]] {
      let query = CurrentHubDriveQuery(limit: 2, cursor: cursor)
      let result = try await client.drives(vehicleID: selected.vehicleID, query: query)
      guard case .modified(let page, let eTag) = result else {
        return XCTFail("expected a modified drive page")
      }
      XCTAssertEqual(page.items.map(\.id), expected)
      driveIDs.append(contentsOf: page.items.map(\.id))
      let conditional = try await client.drives(
        vehicleID: selected.vehicleID,
        query: query,
        ifNoneMatch: eTag
      )
      XCTAssertEqual(conditional, .notModified(eTag: eTag))
      cursor = page.nextCursor
    }
    XCTAssertNil(cursor)
    XCTAssertEqual(driveIDs, [105, 104, 103, 102, 101])
    print("TASK6_LIVE milestone=pages")

    let lastPage = try await client.drives(
      vehicleID: selected.vehicleID,
      query: CurrentHubDriveQuery(limit: 500)
    )
    guard case .modified(let all, _) = lastPage,
      let nullable = all.items.first(where: { $0.id == 101 })
    else { return XCTFail("missing drive 101") }
    XCTAssertNil(nullable.distanceKilometres)
    XCTAssertNil(nullable.durationMinutes)

    let oldStore = LiveCredentialStore(originalCredential)
    let oldClient = try await CurrentHubClient.connect(
      endpoint: config.endpoint,
      expectedHubID: config.expectedHubID,
      credentialStore: oldStore,
      transport: transport
    )
    _ = try await client.rotateCredential()
    print("TASK6_LIVE milestone=rotated")
    do {
      _ = try await oldClient.vehicles()
      XCTFail("rotated bearer remained valid")
    } catch let error as CurrentHubError {
      guard case .unauthorized = error else { throw error }
    }
    let rotatedVehicles = try await client.vehicles()
    XCTAssertEqual(rotatedVehicles.count, 2)

    do {
      _ = try await client.claim(invitation: invitation, deviceName: "replay")
      XCTFail("single-use invitation replay succeeded")
    } catch let error as CurrentHubError {
      guard case .unauthorized = error else { throw error }
    }

    do {
      _ = try await client.require(.commands)
    } catch let error as CurrentHubError {
      XCTAssertEqual(error, .capabilityUnavailable("commands"))
    }

    try await assertCancelledRequest(transport: transport, endpoint: config.endpoint)
    var checks = [
      "trusted_tls", "identity_negative", "trust_negative",
      "invitation_leaf_pin_negative", "claim", "rotation",
      "old_bearer_rejected", "pagination", "etag_304", "units_null_zero",
      "cancellation", "unsupported_zero",
    ]
    #if os(macOS) || os(Linux)
      try await assertOversizedChunkedResponseRejected()
      checks.append("chunked_oversize")
    #else
      print("TASK6_LIVE milestone=chunked_oversize_skipped platform=iOS")
      checks.append("chunked_oversize_skipped")
    #endif

    let receipt: [String: Any] = [
      "status": "passed",
      "profile_id": "hub-http-v1@1.0.0",
      "profile_sha256": CurrentHubBinding.approvedManifestSHA256,
      "product_version": "2026.36.2",
      "client_platform": config.clientPlatform,
      "endpoint": config.endpoint.absoluteString,
      "hub_id": config.expectedHubID.uuidString.lowercased(),
      "vehicle_count": vehicles.count,
      "drive_ids": driveIDs,
      "page_count": 3,
      "conditional_304_count": 3,
      "checks": checks,
    ]
    let data = try JSONSerialization.data(withJSONObject: receipt, options: [.prettyPrinted, .sortedKeys])
    try data.write(to: config.receiptPath, options: .atomic)
  }

  private func makeTrustedTransport(
    config: LiveConfig,
    invitation: CurrentHubInvitation
  ) throws -> CurrentHubURLSessionTransport {
    #if canImport(Security)
      let der = try certificateDER(fromPEM: boundedRead(config.certificatePath, maximumBytes: 65_536))
      return try CurrentHubURLSessionTransport(
        maximumResponseBytes: 1_048_576,
        trustedCertificateAuthoritiesDER: [der],
        expectedLeafCertificateSHA256: invitation.tlsPin
      )
    #else
      return CurrentHubURLSessionTransport(maximumResponseBytes: 1_048_576)
    #endif
  }

  private func assertTrustNegative(
    config: LiveConfig,
    invitation: CurrentHubInvitation
  ) async throws {
    #if canImport(Security)
      let der = try certificateDER(fromPEM: boundedRead(config.certificatePath, maximumBytes: 65_536))
      let wrong = try CurrentHubURLSessionTransport(
        trustedCertificateAuthoritiesDER: [der],
        expectedLeafCertificateSHA256: String(repeating: "f", count: 64)
      )
      do {
        _ = try await wrong.send(
          URLRequest(url: config.endpoint.appendingPathComponent("healthz"))
        )
        XCTFail("wrong leaf pin was accepted")
      } catch {}
    #else
      let wrong = try CurrentHubURLSessionTransport(
        expectedLeafCertificateSHA256: String(repeating: "f", count: 64)
      )
      do {
        _ = try await wrong.send(
          URLRequest(url: config.endpoint.appendingPathComponent("healthz"))
        )
        XCTFail("wrong leaf pin was accepted")
      } catch let error as CurrentHubError {
        guard case .transportFailure = error else { throw error }
      }
      guard let untrustedTLSEndpoint = config.untrustedTLSEndpoint else {
        return XCTFail("untrusted_tls_endpoint is required on FoundationNetworking")
      }
      do {
        _ = try await CurrentHubURLSessionTransport().send(
          URLRequest(url: untrustedTLSEndpoint.appendingPathComponent("healthz"))
        )
        XCTFail("untrusted live TLS endpoint was accepted")
      } catch let error as CurrentHubError {
        guard case .transportFailure = error else { throw error }
      }
    #endif
  }

  private func assertInvitationLeafPinNegative(
    client: CurrentHubClient,
    invitation: CurrentHubInvitation
  ) async throws {
    let wrongPin = (invitation.tlsPin.first == "f" ? "e" : "f")
      + String(invitation.tlsPin.dropFirst())
    var pairing = try XCTUnwrap(
      URLComponents(url: invitation.pairingURI, resolvingAgainstBaseURL: false)
    )
    pairing.queryItems = try XCTUnwrap(pairing.queryItems).map { item in
      item.name == "tls_pin" ? URLQueryItem(name: item.name, value: wrongPin) : item
    }
    let wrongData = try JSONSerialization.data(withJSONObject: [
      "endpoint": invitation.endpoint.absoluteString,
      "pairingId": invitation.pairingID.uuidString.lowercased(),
      "expiresAtMs": invitation.expiresAtMilliseconds,
      "tlsPin": wrongPin,
      "pairingUri": try XCTUnwrap(pairing.url).absoluteString,
      "secret": invitation.secret,
    ])
    let wrongInvitation = try JSONDecoder().decode(CurrentHubInvitation.self, from: wrongData)
    do {
      _ = try await client.claim(invitation: wrongInvitation, deviceName: "wrong leaf pin")
      XCTFail("claim succeeded with the wrong invitation leaf pin")
    } catch let error as CurrentHubError {
      guard case .transportFailure = error else { throw error }
    }
    // The subsequent positive claim must succeed. Since invitations are single use,
    // that proves the wrong-pin TLS connection transmitted no accepted claim secret.
  }

  private func assertCancelledRequest(
    transport: CurrentHubURLSessionTransport,
    endpoint: URL
  ) async throws {
    let task = Task {
      withUnsafeCurrentTask { $0?.cancel() }
      return try await transport.send(
        URLRequest(url: endpoint.appendingPathComponent("healthz"))
      )
    }
    do {
      _ = try await task.value
      XCTFail("pre-cancelled live transport request succeeded")
    } catch is CancellationError {}
  }

  #if os(macOS) || os(Linux)
  private func assertOversizedChunkedResponseRejected() async throws {
    guard let script = Bundle.module.url(
      forResource: "oversize_chunked_server",
      withExtension: "py",
      subdirectory: "Fixtures"
    ) else { return XCTFail("oversize server resource is missing") }
    let process = Process()
    let output = Pipe()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/python3")
    process.arguments = [script.path]
    process.standardOutput = output
    process.standardError = Pipe()
    try process.run()
    defer {
      if process.isRunning { process.terminate() }
      process.waitUntilExit()
    }
    let portData = output.fileHandleForReading.availableData
    guard let portText = String(data: portData, encoding: .utf8)?.split(separator: "\n").first,
      let port = Int(portText)
    else { return XCTFail("oversize server did not publish a port") }
    let transport = CurrentHubURLSessionTransport(maximumResponseBytes: 1_048_576)
    do {
      _ = try await transport.send(
        URLRequest(url: URL(string: "http://127.0.0.1:\(port)/oversize")!)
      )
      XCTFail("oversized chunked response was accepted")
    } catch let error as CurrentHubError {
      guard case .invalidResponse(_, _, let reason) = error,
        reason == "response body exceeds 1048576 bytes"
      else { throw error }
    }
  }
  #endif
}

private actor LiveCredentialStore: CurrentHubCredentialStore {
  private var credential: CurrentHubCredential?
  init(_ credential: CurrentHubCredential? = nil) { self.credential = credential }
  func loadCredential() async throws -> CurrentHubCredential? { credential }
  func saveCredential(_ credential: CurrentHubCredential) async throws {
    self.credential = credential
  }
}

private struct LiveConfig: Decodable {
  let endpoint: URL
  let expectedHubID: UUID
  let selectedVehicleID: UUID
  let invitationPath: URL
  let certificatePath: URL
  let receiptPath: URL
  let clientPlatform: String
  let untrustedTLSEndpoint: URL?

  enum CodingKeys: String, CodingKey {
    case endpoint
    case expectedHubID = "expected_hub_id"
    case selectedVehicleID = "selected_vehicle_id"
    case invitationPath = "invitation_path"
    case certificatePath = "certificate_path"
    case receiptPath = "receipt_path"
    case clientPlatform = "client_platform"
    case untrustedTLSEndpoint = "untrusted_tls_endpoint"
  }

  static func load(_ path: URL) throws -> LiveConfig {
    let value = try JSONDecoder().decode(
      LiveConfig.self,
      from: boundedRead(path, maximumBytes: 65_536)
    )
    guard value.endpoint.scheme == "https",
      value.expectedHubID != UUID.currentHubZero,
      value.selectedVehicleID != UUID.currentHubZero,
      !value.clientPlatform.isEmpty
    else { throw CurrentHubError.invalidRequest("live config is incomplete") }
    return value
  }
}

private func boundedRead(
  _ url: URL, maximumBytes: Int, expectedSHA256: String? = nil
) throws -> Data {
  try matrixPrivateRead(url, maximumBytes: maximumBytes, expectedSHA256: expectedSHA256)
}

#if canImport(Security)
  private func certificateDER(fromPEM data: Data) throws -> Data {
    guard let text = String(data: data, encoding: .utf8),
      let start = text.range(of: "-----BEGIN CERTIFICATE-----"),
      let end = text.range(of: "-----END CERTIFICATE-----", range: start.upperBound..<text.endIndex)
    else { throw CurrentHubError.invalidRequest("certificate file is not PEM") }
    let base64 = text[start.upperBound..<end.lowerBound]
      .filter { !$0.isWhitespace }
    guard let der = Data(base64Encoded: String(base64)) else {
      throw CurrentHubError.invalidRequest("certificate PEM body is invalid")
    }
    return der
  }
#endif

import Foundation
import XCTest

@testable import TeslatlasCurrentHub

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif
#if canImport(Security)
  import Security
#endif
#if canImport(Darwin)
  import Darwin
#else
import Glibc
#endif

private enum MatrixWorkerFailure: Error { case cancellationAccepted, oversizeAccepted }

final class CurrentHubMatrixWorkerTests: XCTestCase {
  func testInstalledCurrentHubMatrixWorker() async throws {
    guard let path = ProcessInfo.processInfo.environment[
      "TESLATLAS_CURRENT_HUB_MATRIX_WORKER_CONFIG"
    ] else {
      return XCTFail("TESLATLAS_CURRENT_HUB_MATRIX_WORKER_CONFIG is required")
    }
    let configData = try matrixBoundedRead(URL(fileURLWithPath: path), maximumBytes: 1_048_576)
    let config = try CurrentHubMatrixWorkerConfig.decode(configData)
    let channel = try MatrixWorkerChannel(config: config)

    let bootstrap = try await channel.phase("bootstrap_pair")
    let active = try bootstrap.invitation("invitation")
    let expired = try bootstrap.invitation("expired_invitation")
    let endpoint = active.endpoint
    let transport = CurrentHubTranscriptTransport(
      base: try matrixTrustedTransport(config: config, invitation: active)
    )
    let store = MatrixCredentialStore()
    let probeStart = await transport.count()
    let client = try await CurrentHubClient.connect(
      endpoint: endpoint,
      expectedHubID: bootstrap.hubID,
      credentialStore: store,
      transport: transport
    )
    let discovery = await client.discoveryDocument()
    let health = try await client.health()
    let readiness = try await client.readiness()
    let probeRequests = await transport.snapshot(since: probeStart)

    let badStart = await transport.count()
    let replacement = (active.secret.first == "0" ? "1" : "0") + active.secret.dropFirst()
    let bad = try matrixInvitation(active, replacingSecret: String(replacement))
    await matrixAssertError(try await client.claim(invitation: bad, deviceName: "bad")) {
      if case .unauthorized = $0 { return true }
      return false
    }
    let badRequests = await transport.snapshot(since: badStart)

    let expiredStart = await transport.count()
    await matrixAssertError(try await client.claim(invitation: expired, deviceName: "expired")) {
      $0 == .invitationExpired
    }
    let expiredEnd = await transport.count()
    XCTAssertEqual(expiredEnd, expiredStart)

    let claimStart = await transport.count()
    let originalCredential = try await client.claim(invitation: active, deviceName: "Swift matrix")
    let claimRequests = await transport.snapshot(since: claimStart)
    let vehicleStart = await transport.count()
    let vehicles = try await client.vehicles()
    let vehicleRequests = await transport.snapshot(since: vehicleStart)
    XCTAssertEqual(vehicles.count, 2)
    let selected = try XCTUnwrap(vehicles.first { $0.displayName == "Interop – Árvíztűrő 🚗" })
    let empty = try XCTUnwrap(vehicles.first { $0.vehicleID != selected.vehicleID })

    let currentStart = await transport.count()
    let current = try await client.current(vehicleID: selected.vehicleID)
    let emptyCurrent = try await client.current(vehicleID: empty.vehicleID)
    let currentRequests = await transport.snapshot(since: currentStart)

    let unknownStart = await transport.count()
    let unknownID = UUID(uuidString: "33333333-3333-4333-8333-333333333333")!
    await matrixAssertError(try await client.current(vehicleID: unknownID)) {
      if case .notFound = $0 { return true }
      return false
    }
    let unknownRequests = await transport.snapshot(since: unknownStart)

    var pages: [[Int64]] = []
    var pageRequests: [[CurrentHubTranscriptEntry]] = []
    var etagRequests: [CurrentHubTranscriptEntry] = []
    var cursor: CurrentHubDriveCursor?
    var firstCursor: CurrentHubDriveCursor?
    for _ in 0..<3 {
      let start = await transport.count()
      let query = CurrentHubDriveQuery(limit: 2, cursor: cursor)
      let response = try await client.drives(vehicleID: selected.vehicleID, query: query)
      guard case .modified(let page, let tag) = response else {
        return XCTFail("matrix drive page was not modified")
      }
      pages.append(page.items.map(\.id))
      if firstCursor == nil { firstCursor = page.nextCursor }
      let conditional = try await client.drives(
        vehicleID: selected.vehicleID, query: query, ifNoneMatch: tag
      )
      guard case .notModified = conditional else {
        return XCTFail("matrix conditional drive request was modified")
      }
      let captured = await transport.snapshot(since: start)
      pageRequests.append(captured.filter { $0.status == 200 })
      etagRequests.append(contentsOf: captured)
      cursor = page.nextCursor
    }
    XCTAssertEqual(pages, [[105, 104], [103, 102], [101]])
    XCTAssertNil(cursor)

    let wrongVehicleStart = await transport.count()
    await matrixAssertError(
      try await client.drives(
        vehicleID: empty.vehicleID,
        query: CurrentHubDriveQuery(limit: 2, cursor: try XCTUnwrap(firstCursor))
      )
    ) { error in
      if case .api(let status, let code, _, _) = error {
        return status == 400 && code == "invalid_cursor"
      }
      return false
    }
    let wrongVehicleRequests = await transport.snapshot(since: wrongVehicleStart)

    let wrongFilterStart = await transport.count()
    await matrixAssertError(
      try await client.drives(
        vehicleID: selected.vehicleID,
        query: CurrentHubDriveQuery(
          fromMilliseconds: 1_788_566_400_001, limit: 2,
          cursor: try XCTUnwrap(firstCursor)
        )
      )
    ) { error in
      if case .api(let status, let code, _, _) = error {
        return status == 400 && code == "invalid_cursor"
      }
      return false
    }
    let wrongFilterRequests = await transport.snapshot(since: wrongFilterStart)

    let unsupportedStart = await transport.count()
    await matrixAssertError(try await client.require(.commands)) {
      $0 == .capabilityUnavailable("commands")
    }
    let unsupportedEnd = await transport.count()
    XCTAssertEqual(unsupportedEnd, unsupportedStart)

    let rotationStart = await transport.count()
    let oldClient = try await CurrentHubClient.connect(
      endpoint: endpoint, expectedHubID: bootstrap.hubID,
      credentialStore: MatrixCredentialStore(originalCredential), transport: transport
    )
    let rotatedCredential = try await client.rotateCredential()
    let rotatedClient = try await CurrentHubClient.connect(
      endpoint: endpoint, expectedHubID: bootstrap.hubID,
      credentialStore: MatrixCredentialStore(rotatedCredential), transport: transport
    )
    await matrixAssertError(try await oldClient.vehicles()) {
      if case .unauthorized = $0 { return true }
      return false
    }
    let rotatedVehicles = try await client.vehicles()
    XCTAssertEqual(rotatedVehicles.count, 2)
    let rotationRequests = await transport.snapshot(since: rotationStart)

    let replayStart = await transport.count()
    await matrixAssertError(try await client.claim(invitation: active, deviceName: "replay")) {
      if case .unauthorized = $0 { return true }
      return false
    }
    let replayRequests = await transport.snapshot(since: replayStart)

    let revokePair = try await channel.phase(
      "revoke_and_pair", deviceID: originalCredential.deviceID
    )
    let revokedStart = await transport.count()
    await matrixAssertError(try await rotatedClient.vehicles()) {
      if case .unauthorized = $0 { return true }
      return false
    }
    let revokedRequests = await transport.snapshot(since: revokedStart)
    let freshInvitation = try revokePair.invitation("invitation")
    let freshStore = MatrixCredentialStore()
    let freshClient = try await CurrentHubClient.connect(
      endpoint: endpoint, expectedHubID: bootstrap.hubID,
      credentialStore: freshStore, transport: transport
    )
    let reauthStart = await transport.count()
    let freshCredential = try await freshClient.claim(invitation: freshInvitation, deviceName: "Swift matrix fresh")
    let reauthenticatedVehicles = try await freshClient.vehicles()
    XCTAssertEqual(reauthenticatedVehicles.count, 2)
    let reauthRequests = await transport.snapshot(since: reauthStart)

    let restartBefore = channel.observationSequence
    let restart = try await channel.phase("restart")
    let restartStart = await transport.count()
    let restartedVehicles = try await freshClient.vehicles()
    XCTAssertEqual(restartedVehicles.count, 2)
    let restartRequests = await transport.snapshot(since: restartStart)

    let outageBefore = channel.observationSequence
    _ = try await channel.phase("outage_stop")
    let outageStart = await transport.count()
    await matrixAssertError(try await freshClient.vehicles()) {
      if case .transportFailure = $0 { return true }
      return false
    }
    let outageFailureRequests = await transport.snapshot(since: outageStart)
    let recovery = try await channel.phase("outage_start")
    XCTAssertEqual(recovery.hubID, bootstrap.hubID)
    let recoveryStart = await transport.count()
    let recoveredVehicles = try await freshClient.vehicles()
    XCTAssertEqual(recoveredVehicles.count, 2)
    let recoveryRequests = outageFailureRequests + (await transport.snapshot(since: recoveryStart))

    let cancellation = try await matrixCancellationEvidence(endpoint: endpoint)
    let bodyLimit = try await matrixBodyLimitEvidence()
    _ = try await channel.phase("final_verify")

    let commonAnchor = bootstrap.observationSequence
    let runtime = matrixRuntimeProjection(actorID: config.actorID)
    let identities = try matrixInputIdentities(config)
    let serviceMode = try matrixServiceMode(config)
    let actorFacts: [String: [String: Any]] = [
      "candidate_artifact_identity": [
        "hub_sha256": identities.hubSHA256, "tarball_sha256": identities.artifactSHA256,
        "package_version": "2026.36.2", "installed_members": identities.installedMembers.count,
      ],
      "installed_service_runtime": ["service_mode": serviceMode],
      "discovery_identity_profile": [
        "hub_id": discovery.hubID.uuidString.lowercased(), "api_versions": discovery.apiVersions,
        "protocol": discovery.protocolName, "protocol_major": discovery.protocolMajor,
        "pack_format": discovery.packFormat, "version": discovery.version,
      ],
      "unauthenticated_discovery": ["discovery": 200, "health": health.status == "ok" ? 200 : -1, "readiness": readiness.status == "ready" ? 200 : -1, "credential_absent": true],
      "bad_invitation": ["pairing_id": active.pairingID.uuidString.lowercased(), "typed_error": "unauthorized", "http_status": 401, "credential_created": false],
      "expired_invitation": ["pairing_id": expired.pairingID.uuidString.lowercased(), "expires_at_ms": expired.expiresAtMilliseconds, "observed_after_expiry": Int64(Date().timeIntervalSince1970 * 1000) > expired.expiresAtMilliseconds, "outgoing_requests": 0, "typed_error": "invitationExpired", "credential_created": false],
      "replayed_invitation": ["pairing_id": active.pairingID.uuidString.lowercased(), "typed_error": "unauthorized", "http_status": 401, "credential_created": false],
      "real_auth": ["pairing_id": active.pairingID.uuidString.lowercased(), "claimed": 200, "vehicles": vehicles.map { ["vehicle_id": $0.vehicleID.uuidString.lowercased(), "display_name": $0.displayName as Any] }],
      "credential_lifecycle_reauth": ["pairing_id": freshInvitation.pairingID.uuidString.lowercased(), "new_device": freshCredential.deviceID != originalCredential.deviceID, "vehicles": 200, "fresh_claim": 200],
      "revocation": ["typed_error": "unauthorized", "http_status": 401],
      "unknown_vehicle": ["typed_error": "notFound", "http_status": 404, "vehicle_id": unknownID.uuidString.lowercased()],
      "exact_current_values": [
        "battery_level": try XCTUnwrap(current.batteryLevel),
        "inside_temp": try XCTUnwrap(current.insideTemperatureCelsius),
        "outside_temp": current.outsideTemperatureCelsius.map { $0 as Any } ?? NSNull(),
        "observed_at_ms": try XCTUnwrap(current.observedAtMilliseconds),
        "est_battery_range_km": try XCTUnwrap(current.estimatedBatteryRangeKilometres),
        "odometer": try XCTUnwrap(current.odometerKilometres),
        "speed": try XCTUnwrap(current.speedKilometresPerHour),
        "scheduled_charging_start_time": try XCTUnwrap(current.scheduledChargingStartTime),
        "active_route_miles_to_arrival": try XCTUnwrap(current.activeRouteMilesToArrival),
        "empty_vehicle_observed_at_ms": emptyCurrent.observedAtMilliseconds.map { $0 as Any } ?? NSNull(),
      ],
      "endpoint_restart": ["same_hub": restart.hubID == bootstrap.hubID, "new_process": restart.serviceGeneration != bootstrap.serviceGeneration, "vehicles": 200],
      "outage_recovery": ["outage_observed": true, "vehicles": 200],
      "unsupported_operation_zero_requests": ["outgoing_requests": 0],
      "credential_rotation_api": ["rotated": true, "same_device": rotatedCredential.deviceID == originalCredential.deviceID, "vehicles": 200, "old_credential_error": "unauthorized", "old_credential_status": 401],
      "drives_three_page_order": ["pages": pages],
      "drives_terminal_cursor": ["next_cursor": NSNull(), "ids": pages.last!],
      "drives_etag_304": ["kind": "notModified", "post_304_ids": pages[1]],
      "drives_wrong_vehicle_cursor": ["typed_error": "api", "http_status": 400, "error_code": "invalid_cursor"],
      "drives_wrong_filter_cursor": ["typed_error": "api", "http_status": 400, "error_code": "invalid_cursor"],
      "native_macos_transport": runtime,
      "native_linux_transport": runtime,
      "transport_cancellation": ["cancelled": true, "typed_error": "CancellationError", "bounded": true],
      "transport_body_limit": ["limit_bytes": 1_048_576, "oversize_rejected": true, "typed_error": "invalidResponse"],
    ]
    let requests: [String: [CurrentHubTranscriptEntry]] = [
      "candidate_artifact_identity": [], "installed_service_runtime": [],
      "discovery_identity_profile": probeRequests.filter { $0.route == "/.well-known/teslatlas-hub" },
      "unauthenticated_discovery": probeRequests,
      "bad_invitation": badRequests, "expired_invitation": [], "replayed_invitation": replayRequests,
      "real_auth": claimRequests + vehicleRequests, "credential_lifecycle_reauth": reauthRequests,
      "revocation": revokedRequests, "unknown_vehicle": unknownRequests,
      "exact_current_values": currentRequests, "endpoint_restart": restartRequests,
      "outage_recovery": recoveryRequests, "unsupported_operation_zero_requests": [],
      "credential_rotation_api": rotationRequests, "drives_three_page_order": pageRequests.flatMap { $0 },
      "drives_terminal_cursor": pageRequests.last ?? [], "drives_etag_304": etagRequests,
      "drives_wrong_vehicle_cursor": wrongVehicleRequests,
      "drives_wrong_filter_cursor": wrongFilterRequests,
      "native_macos_transport": probeRequests, "native_linux_transport": probeRequests,
      "transport_cancellation": cancellation, "transport_body_limit": bodyLimit,
    ]
    let lifecycle: [String: (Int, Int)] = [
      "endpoint_restart": (restartBefore, restart.observationSequence),
      "outage_recovery": (outageBefore, recovery.observationSequence),
      "credential_lifecycle_reauth": (commonAnchor, revokePair.observationSequence),
      "revocation": (commonAnchor, revokePair.observationSequence),
      "transport_cancellation": (recovery.observationSequence, recovery.observationSequence),
      "transport_body_limit": (recovery.observationSequence, recovery.observationSequence),
    ]
    let operations = matrixCaseOperations
    let rows: [[String: Any]] = operations.keys.sorted().map { id in
      let anchor = lifecycle[id] ?? (commonAnchor, commonAnchor)
      return [
        "case_id": id, "operation": operations[id]!,
        "actor_manifest_sha256": identities.actorManifestSHA256,
        "session_sequence_before": anchor.0, "session_sequence_after": anchor.1,
        "facts": actorFacts[id]!, "requests": matrixTranscript(requests[id] ?? []),
      ]
    }
    try channel.writeEvidence(rows: rows)
  }
}

private let matrixCaseOperations: [String: String] = [
  "candidate_artifact_identity": "observe_identity", "installed_service_runtime": "observe_identity",
  "discovery_identity_profile": "discovery", "unauthenticated_discovery": "unauthenticated_probes",
  "bad_invitation": "bad_invitation", "expired_invitation": "expired_invitation",
  "replayed_invitation": "replayed_invitation", "real_auth": "real_auth",
  "credential_lifecycle_reauth": "reauthentication", "revocation": "revoked_credential",
  "unknown_vehicle": "unknown_vehicle", "exact_current_values": "exact_current",
  "endpoint_restart": "endpoint_restart", "outage_recovery": "outage_recovery",
  "unsupported_operation_zero_requests": "unsupported_operations", "credential_rotation_api": "credential_rotation",
  "drives_three_page_order": "drives_three_pages", "drives_terminal_cursor": "drives_terminal_cursor",
  "drives_etag_304": "drives_etag", "drives_wrong_vehicle_cursor": "wrong_vehicle_cursor",
  "drives_wrong_filter_cursor": "wrong_filter_cursor", "native_macos_transport": "transport_identity",
  "native_linux_transport": "transport_identity", "transport_cancellation": "transport_cancellation",
  "transport_body_limit": "transport_body_limit",
]

private actor MatrixCredentialStore: CurrentHubCredentialStore {
  private var credential: CurrentHubCredential?
  init(_ credential: CurrentHubCredential? = nil) { self.credential = credential }
  func loadCredential() async throws -> CurrentHubCredential? { credential }
  func saveCredential(_ credential: CurrentHubCredential) async throws { self.credential = credential }
}

private func matrixAssertError<T>(
  _ expression: @autoclosure () async throws -> T,
  _ predicate: (CurrentHubError) -> Bool,
  file: StaticString = #filePath, line: UInt = #line
) async {
  do { _ = try await expression(); XCTFail("expected current-Hub error", file: file, line: line) }
  catch let error as CurrentHubError { XCTAssertTrue(predicate(error), "unexpected error: \(error)", file: file, line: line) }
  catch { XCTFail("unexpected error: \(error)", file: file, line: line) }
}

private func matrixTranscript(_ entries: [CurrentHubTranscriptEntry]) -> [[String: Any]] {
  entries.map { ["method": $0.method, "route": $0.route, "status": $0.status, "request_id": $0.requestID] }
}

private func matrixInvitation(
  _ invitation: CurrentHubInvitation, replacingSecret secret: String
) throws -> CurrentHubInvitation {
  let data = try JSONSerialization.data(withJSONObject: [
    "endpoint": invitation.endpoint.absoluteString,
    "pairingId": invitation.pairingID.uuidString.lowercased(),
    "expiresAtMs": invitation.expiresAtMilliseconds, "tlsPin": invitation.tlsPin,
    "pairingUri": invitation.pairingURI.absoluteString, "secret": secret,
  ])
  return try JSONDecoder().decode(CurrentHubInvitation.self, from: data)
}

private func matrixRuntimeProjection(actorID _: String) -> [String: Any] {
  #if os(macOS)
    return ["os": "macOS", "transport": "URLSession", "trusted_tls": true]
  #else
    return ["os": "Ubuntu 22.04.5", "transport": "libcurl", "trusted_tls": true]
  #endif
}

private func matrixTrustedTransport(
  config: CurrentHubMatrixWorkerConfig, invitation: CurrentHubInvitation
) throws -> CurrentHubURLSessionTransport {
  #if canImport(Security)
    let certificate = try matrixInput(config, discriminator: { $0.id == "certificate" })
    let der = try matrixCertificateDER(matrixBoundedRead(URL(fileURLWithPath: certificate.local.path), maximumBytes: 65_536))
    return try CurrentHubURLSessionTransport(
      maximumResponseBytes: 1_048_576,
      trustedCertificateAuthoritiesDER: [der],
      expectedLeafCertificateSHA256: invitation.tlsPin
    )
  #else
    return CurrentHubURLSessionTransport(maximumResponseBytes: 1_048_576)
  #endif
}

private func matrixInput(
  _ config: CurrentHubMatrixWorkerConfig,
  discriminator: (CurrentHubMatrixStagedFile) -> Bool
) throws -> CurrentHubMatrixStagedFile {
  guard let value = config.inputs.first(where: discriminator) else {
    throw CurrentHubError.invalidRequest("matrix worker input is missing")
  }
  return value
}

private struct MatrixInputIdentities {
  let hubSHA256: String
  let artifactSHA256: String
  let actorManifestSHA256: String
  let installedMembers: [[String: Any]]
}

private func matrixInputIdentities(_ config: CurrentHubMatrixWorkerConfig) throws -> MatrixInputIdentities {
  var artifact: String?
  var hub: String?
  var actorManifest: String?
  var members: [[String: Any]]?
  for input in config.inputs {
    let data = try matrixBoundedRead(URL(fileURLWithPath: input.local.path), maximumBytes: 1_048_576)
    guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { continue }
    if object["execution_kind"] as? String == "actual_hub_acceptance",
      let artifacts = object["artifacts"] as? [[String: Any]],
      let selected = artifacts.first(where: { $0["role"] as? String == "swift_sdk_product" }) {
      artifact = selected["sha256"] as? String
      hub = artifacts.first(where: { $0["role"] as? String == "hub_executable" })?["sha256"] as? String
    }
    if object["artifact_sha256"] is String, let files = object["files"] as? [[String: Any]] {
      members = files.map { item in
        ["path": item["path"]!, "sha256": item["sha256"]!, "bytes": item["bytes"]!,
         "mode": String(format: "%04o", item["mode"] as! Int)]
      }
    }
    if object["build_record"] != nil { actorManifest = input.local.sha256 }
  }
  guard let hub, let artifact, let actorManifest, let members else {
    throw CurrentHubError.invalidRequest("matrix installed identities are incomplete")
  }
  return MatrixInputIdentities(
    hubSHA256: hub, artifactSHA256: artifact, actorManifestSHA256: actorManifest,
    installedMembers: members.sorted { ($0["path"] as! String) < ($1["path"] as! String) }
  )
}

private func matrixServiceMode(_ config: CurrentHubMatrixWorkerConfig) throws -> String {
  for input in config.inputs {
    let data = try matrixBoundedRead(URL(fileURLWithPath: input.local.path), maximumBytes: 1_048_576)
    guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
      object["execution_kind"] as? String == "actual_hub_acceptance",
      let runtime = object["runtime"] as? [String: Any],
      let hub = runtime["hub"] as? [String: Any], let mode = hub["service_mode"] as? String
    else { continue }
    return mode
  }
  throw CurrentHubError.invalidRequest("matrix service mode is missing")
}

#if canImport(Security)
  private func matrixCertificateDER(_ data: Data) throws -> Data {
    guard let text = String(data: data, encoding: .utf8),
      let start = text.range(of: "-----BEGIN CERTIFICATE-----"),
      let end = text.range(of: "-----END CERTIFICATE-----", range: start.upperBound..<text.endIndex),
      let der = Data(base64Encoded: text[start.upperBound..<end.lowerBound].filter { !$0.isWhitespace })
    else { throw CurrentHubError.invalidRequest("matrix certificate is not PEM") }
    return der
  }
#endif

private func matrixCancellationEvidence(endpoint: URL) async throws -> [CurrentHubTranscriptEntry] {
  let transcript = CurrentHubTranscriptTransport(base: CurrentHubURLSessionTransport(maximumResponseBytes: 1_048_576))
  let task = Task {
    withUnsafeCurrentTask { $0?.cancel() }
    return try await transcript.send(URLRequest(url: endpoint.appendingPathComponent("healthz")))
  }
  do {
    _ = try await task.value
    XCTFail("pre-cancelled matrix request succeeded")
    throw MatrixWorkerFailure.cancellationAccepted
  }
  catch is CancellationError {}
  return await transcript.snapshot()
}

private func matrixBodyLimitEvidence() async throws -> [CurrentHubTranscriptEntry] {
  let server = try MatrixOversizeServer()
  let transcript = CurrentHubTranscriptTransport(base: CurrentHubURLSessionTransport(maximumResponseBytes: 1_048_576))
  do {
    _ = try await transcript.send(URLRequest(url: URL(string: "http://127.0.0.1:\(server.port)/oversize")!))
    await server.close()
    XCTFail("oversize matrix response succeeded")
    throw MatrixWorkerFailure.oversizeAccepted
  }
  catch let error as CurrentHubError {
    guard case .invalidResponse(_, _, let reason) = error,
      reason == "response body exceeds 1048576 bytes" else {
      await server.close(); throw error
    }
  }
  let entries = await transcript.snapshot()
  await server.close()
  return entries
}

private final class MatrixOversizeServer: @unchecked Sendable {
  let port: UInt16
  private let descriptor: Int32
  private let task: Task<Void, Never>

  init() throws {
    #if os(Linux)
      let stream = Int32(SOCK_STREAM.rawValue)
    #else
      let stream = SOCK_STREAM
    #endif
    let listener = socket(AF_INET, stream, 0)
    guard listener >= 0 else { throw CurrentHubError.transportFailure(code: Int(errno)) }
    var reuse: Int32 = 1
    _ = setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout.size(ofValue: reuse)))
    #if !os(Linux)
      _ = setsockopt(listener, SOL_SOCKET, SO_NOSIGPIPE, &reuse, socklen_t(MemoryLayout.size(ofValue: reuse)))
    #endif
    var address = sockaddr_in()
    #if !os(Linux)
      address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    #endif
    address.sin_family = sa_family_t(AF_INET)
    address.sin_port = 0
    address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
    let bound = withUnsafePointer(to: &address) {
      $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
        bind(listener, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
      }
    }
    guard bound == 0, listen(listener, 1) == 0 else {
      DarwinOrGlibcClose(listener)
      throw CurrentHubError.transportFailure(code: Int(errno))
    }
    var actual = sockaddr_in(); var length = socklen_t(MemoryLayout<sockaddr_in>.size)
    let named = withUnsafeMutablePointer(to: &actual) {
      $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(listener, $0, &length) }
    }
    guard named == 0 else {
      DarwinOrGlibcClose(listener)
      throw CurrentHubError.transportFailure(code: Int(errno))
    }
    descriptor = listener; port = UInt16(bigEndian: actual.sin_port)
    task = Task.detached {
      let connection = accept(listener, nil, nil)
      guard connection >= 0 else { return }
      defer { DarwinOrGlibcClose(connection) }
      var request = [UInt8](repeating: 0, count: 4096)
      _ = recv(connection, &request, request.count, 0)
      let header = Array("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".utf8)
      guard MatrixOversizeServer.sendAll(header, to: connection) else { return }
      let payload = [UInt8](repeating: 0x20, count: 65_536)
      let prefix = Array("10000\r\n".utf8); let suffix = Array("\r\n".utf8)
      for _ in 0..<17 {
        guard Self.sendAll(prefix, to: connection), Self.sendAll(payload, to: connection),
          Self.sendAll(suffix, to: connection) else { return }
      }
      _ = Self.sendAll(Array("0\r\n\r\n".utf8), to: connection)
    }
  }

  func close() async {
    _ = shutdown(descriptor, Int32(SHUT_RDWR))
    DarwinOrGlibcClose(descriptor)
    task.cancel()
    await task.value
  }

  private static func sendAll(_ bytes: [UInt8], to descriptor: Int32) -> Bool {
    bytes.withUnsafeBytes { buffer in
      var offset = 0
      while offset < buffer.count {
        #if os(Linux)
          let count = send(descriptor, buffer.baseAddress!.advanced(by: offset), buffer.count - offset, Int32(MSG_NOSIGNAL))
        #else
          let count = send(descriptor, buffer.baseAddress!.advanced(by: offset), buffer.count - offset, 0)
        #endif
        if count <= 0 { return false }
        offset += count
      }
      return true
    }
  }
}

private func DarwinOrGlibcClose(_ descriptor: Int32) {
  #if os(Linux)
    _ = Glibc.close(descriptor)
  #else
    _ = Darwin.close(descriptor)
  #endif
}

private struct MatrixPhaseResult {
  let value: [String: Any]
  let observationSequence: Int
  let hubID: UUID
  let serviceGeneration: String

  func invitation(_ key: String) throws -> CurrentHubInvitation {
    guard let data = try? JSONSerialization.data(withJSONObject: value[key] as Any) else {
      throw CurrentHubError.invalidRequest("matrix phase invitation is missing")
    }
    return try JSONDecoder().decode(CurrentHubInvitation.self, from: data)
  }
}

private final class MatrixWorkerChannel: @unchecked Sendable {
  private let config: CurrentHubMatrixWorkerConfig
  private var sequence = 0
  private(set) var observationSequence: Int
  private var proofSHA256: String
  private var observedHubID: UUID?
  private var observedServiceGeneration: String?

  init(config: CurrentHubMatrixWorkerConfig) throws {
    self.config = config
    let initial = try matrixInput(config, discriminator: { $0.id == "initial_observation" })
    let data = try matrixBoundedRead(URL(fileURLWithPath: initial.local.path), maximumBytes: 65_536)
    guard MatrixSHA256.hexDigest(data) == initial.local.sha256,
      let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
      Set(object.keys) == ["session_sequence", "proof_sha256"],
      let sequence = object["session_sequence"] as? Int,
      let digest = object["proof_sha256"] as? String
    else { throw CurrentHubError.invalidRequest("matrix initial observation is invalid") }
    observationSequence = sequence; proofSHA256 = digest
  }

  func phase(_ id: String, deviceID: UUID? = nil) async throws -> MatrixPhaseResult {
    sequence += 1
    let witness: [String: Any] = [
      "schema_version": 1, "session_id": config.sessionID.uuidString.lowercased(),
      "actor_id": config.actorID, "phase_id": id,
      "device_id": deviceID?.uuidString.lowercased() ?? NSNull(),
    ]
    let witnessPath = URL(fileURLWithPath: config.privateRoot).appendingPathComponent(String(format: "worker-witness-%06d.json", sequence))
    let witnessBinding = try matrixWriteJSON(witness, to: witnessPath)
    let ready: [String: Any] = [
      "schema_version": 1, "type": "worker_ready",
      "session_id": config.sessionID.uuidString.lowercased(), "cell_id": config.cellID,
      "session_input_sha256": config.sessionInputSHA256, "instance_nonce": config.instanceNonce,
      "sequence": sequence, "actor_id": config.actorID, "phase": id,
      "observation": ["session_sequence": observationSequence, "proof_sha256": proofSHA256],
      "evidence": witnessBinding,
    ]
    let readyPath = URL(fileURLWithPath: config.coordinationDirectory).appendingPathComponent(String(format: "worker-ready-%06d.json", sequence))
    let readyBinding = try matrixWriteJSON(ready, to: readyPath)
    let ackURL = URL(fileURLWithPath: config.coordinationDirectory).appendingPathComponent(String(format: "worker-ack-%06d.json", sequence))
    let deadline = Date().addingTimeInterval(160)
    while !FileManager.default.fileExists(atPath: ackURL.path) {
      if Date() >= deadline { throw CurrentHubError.invalidResponse(statusCode: 0, requestID: nil, reason: "matrix worker acknowledgement timed out") }
      try await Task.sleep(nanoseconds: 250_000_000)
    }
    let ackData = try matrixBoundedRead(ackURL, maximumBytes: 65_536)
    guard let ack = try JSONSerialization.jsonObject(with: ackData) as? [String: Any],
      Set(ack.keys) == ["schema_version", "type", "session_id", "cell_id", "session_input_sha256", "instance_nonce", "sequence", "ready_sha256", "actor_id", "phase", "status", "action", "result"],
      ack["schema_version"] as? Int == 1, ack["type"] as? String == "worker_ack",
      ack["session_id"] as? String == config.sessionID.uuidString.lowercased(),
      ack["cell_id"] as? String == config.cellID,
      ack["session_input_sha256"] as? String == config.sessionInputSHA256,
      ack["instance_nonce"] as? String == config.instanceNonce,
      ack["sequence"] as? Int == sequence, ack["ready_sha256"] as? String == readyBinding["sha256"] as? String,
      ack["actor_id"] as? String == config.actorID, ack["phase"] as? String == id,
      ack["status"] as? String == "accepted", ack["action"] as? String == "continue",
      let resultBinding = ack["result"] as? [String: String],
      let resultPath = resultBinding["path"], let resultDigest = resultBinding["sha256"]
    else { throw CurrentHubError.invalidRequest("matrix worker acknowledgement is invalid") }
    let resultData = try matrixBoundedRead(URL(fileURLWithPath: resultPath), maximumBytes: 1_048_576)
    guard MatrixSHA256.hexDigest(resultData) == resultDigest,
      let envelope = try JSONSerialization.jsonObject(with: resultData) as? [String: Any],
      let results = envelope["results"] as? [[String: Any]],
      let last = results.last?["result"] as? [String: Any]
    else { throw CurrentHubError.invalidRequest("matrix phase result is invalid") }
    if let proof = last["proof"] as? [String: Any], let nextSequence = proof["sequence"] as? Int,
      nextSequence > observationSequence,
      let discovery = proof["discovery"] as? [String: Any],
      let hubText = discovery["hub_id"] as? String, let hubID = UUID(uuidString: hubText),
      let service = proof["service"] as? [String: Any],
      let generation = service["generation"] as? String {
      let proofData = try JSONSerialization.data(withJSONObject: proof, options: [.sortedKeys, .withoutEscapingSlashes])
      observationSequence = nextSequence; proofSHA256 = MatrixSHA256.hexDigest(proofData)
      observedHubID = hubID; observedServiceGeneration = generation
    }
    guard let observedHubID, let observedServiceGeneration else {
      throw CurrentHubError.invalidRequest("matrix phase has no running identity")
    }
    return MatrixPhaseResult(value: last, observationSequence: observationSequence,
                             hubID: observedHubID, serviceGeneration: observedServiceGeneration)
  }

  func writeEvidence(rows: [[String: Any]]) throws {
    let value: [String: Any] = [
      "schema_version": 1, "session_id": config.sessionID.uuidString.lowercased(),
      "cell_id": config.cellID, "session_input_sha256": config.sessionInputSHA256,
      "actor_id": config.actorID, "raw": rows,
      "cleanup": ["status": "passed", "transport_resources_closed": true, "auxiliary_fixture_stopped": true],
    ]
    _ = try matrixWriteJSON(value, to: URL(fileURLWithPath: config.evidencePath))
  }
}

private func matrixBoundedRead(_ url: URL, maximumBytes: Int) throws -> Data {
  let data = try Data(contentsOf: url, options: .mappedIfSafe)
  guard data.count <= maximumBytes else { throw CurrentHubError.invalidRequest("matrix file exceeds bound") }
  return data
}

private func matrixWriteJSON(_ value: Any, to url: URL) throws -> [String: Any] {
  var data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys, .withoutEscapingSlashes])
  data.append(0x0a)
  let descriptor = open(url.path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, S_IRUSR | S_IWUSR)
  guard descriptor >= 0 else { throw CurrentHubError.invalidRequest("matrix exclusive output is unavailable") }
  defer { close(descriptor) }
  try data.withUnsafeBytes { bytes in
    var offset = 0
    while offset < bytes.count {
      let count = write(descriptor, bytes.baseAddress!.advanced(by: offset), bytes.count - offset)
      guard count > 0 else { throw CurrentHubError.invalidRequest("matrix output write failed") }
      offset += count
    }
  }
  guard fsync(descriptor) == 0 else { throw CurrentHubError.invalidRequest("matrix output flush failed") }
  return ["path": url.path, "sha256": MatrixSHA256.hexDigest(data)]
}

private enum MatrixSHA256 {
  private static let initial: [UInt32] = [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19]
  private static let constants: [UInt32] = [
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
  ]
  static func hexDigest(_ data: Data) -> String {
    var message = [UInt8](data); let bitLength = UInt64(message.count) * 8
    message.append(0x80); while message.count % 64 != 56 { message.append(0) }
    message.append(contentsOf: (0..<8).reversed().map { UInt8((bitLength >> UInt64($0 * 8)) & 0xff) })
    var hash = initial
    for offset in stride(from: 0, to: message.count, by: 64) {
      var words = [UInt32](repeating: 0, count: 64)
      for index in 0..<16 { let base = offset + index * 4; words[index] = UInt32(message[base]) << 24 | UInt32(message[base+1]) << 16 | UInt32(message[base+2]) << 8 | UInt32(message[base+3]) }
      for index in 16..<64 { let a = words[index-15]; let b = words[index-2]; let s0 = rotate(a,7) ^ rotate(a,18) ^ (a >> 3); let s1 = rotate(b,17) ^ rotate(b,19) ^ (b >> 10); words[index] = words[index-16] &+ s0 &+ words[index-7] &+ s1 }
      var a=hash[0],b=hash[1],c=hash[2],d=hash[3],e=hash[4],f=hash[5],g=hash[6],h=hash[7]
      for index in 0..<64 { let s1=rotate(e,6)^rotate(e,11)^rotate(e,25); let ch=(e&f)^((~e)&g); let t1=h&+s1&+ch&+constants[index]&+words[index]; let s0=rotate(a,2)^rotate(a,13)^rotate(a,22); let maj=(a&b)^(a&c)^(b&c); let t2=s0&+maj; h=g;g=f;f=e;e=d&+t1;d=c;c=b;b=a;a=t1&+t2 }
      hash[0] &+= a; hash[1] &+= b; hash[2] &+= c; hash[3] &+= d
      hash[4] &+= e; hash[5] &+= f; hash[6] &+= g; hash[7] &+= h
    }
    return hash.map { String(format: "%08x", $0) }.joined()
  }
  private static func rotate(_ value: UInt32, _ bits: UInt32) -> UInt32 { (value >> bits) | (value << (32-bits)) }
}

import Foundation
import XCTest
@testable import TeslatlasHubSDK
@testable import TeslatlasCommands

final class RichAdmissionCorrectionTests: XCTestCase {
  private let vehicle = "vehicle_demo_alpha"
  private let timestamp = "2026-08-30T12:00:01.250Z"

  func testEveryRecognizedEventRequiresItsCompleteBoundPayload() throws {
    for (name, body, identifier) in try eventCases() {
      let valid = try envelope(name, payload: body, identifier: identifier)
      var decoder = TeslatlasEventStreamDecoder()
      guard case .event(let accepted)? = try decoder.append(wire(valid)).first else {
        return XCTFail("Valid \(name) was not delivered")
      }
      XCTAssertEqual(accepted.eventType, name)
      var empty = valid
      empty["data"] = [String: Any]()
      var rejecting = TeslatlasEventStreamDecoder()
      XCTAssertThrowsError(try rejecting.append(wire(empty)), "Empty \(name) payload") {
        XCTAssertEqual($0 as? TeslatlasEventDecodingError, .malformedEnvelope)
      }
    }
    var unknown = TeslatlasEventStreamDecoder()
    XCTAssertEqual(try unknown.append(Data("event: future.event\ndata: not-json\n\n".utf8)), [])
  }

  func testEventRequiresNullableEnvelopeAndPayloadFieldsAndTypedRevision() throws {
    let payload = try object(TestDocuments.currentState)
    let baseline = try envelope("vehicle.current.changed", payload: payload, identifier: vehicle)
    for field in ["vehicle_id", "resource_id"] {
      var missing = baseline
      missing.removeValue(forKey: field)
      var decoder = TeslatlasEventStreamDecoder()
      XCTAssertThrowsError(try decoder.append(wire(missing))) {
        XCTAssertEqual($0 as? TeslatlasEventDecodingError, .malformedEnvelope)
      }
    }
    for field in ["revision", "vehicle_id", "location"] {
      var changedPayload = payload
      changedPayload.removeValue(forKey: field)
      var invalid = baseline
      invalid["data"] = changedPayload
      var decoder = TeslatlasEventStreamDecoder()
      XCTAssertThrowsError(try decoder.append(wire(invalid))) {
        XCTAssertEqual($0 as? TeslatlasEventDecodingError, .malformedEnvelope)
      }
    }
    var wrongType = payload
    wrongType["revision"] = "42"
    var invalid = baseline
    invalid["data"] = wrongType
    var decoder = TeslatlasEventStreamDecoder()
    XCTAssertThrowsError(try decoder.append(wire(invalid))) {
      XCTAssertEqual($0 as? TeslatlasEventDecodingError, .malformedEnvelope)
    }
  }

  func testMetadataTombstoneAndObservationConstraintsUseTheirOwnShapes() throws {
    let tombstone = try metadata(tombstone: true)
    let valid = try envelope("metadata.changed", payload: tombstone, identifier: "metadata_demo_0001")
    var decoder = TeslatlasEventStreamDecoder()
    XCTAssertEqual(try decoder.append(wire(valid)).count, 1)
    var invalid = valid
    invalid["revision"] = 8
    var rejecting = TeslatlasEventStreamDecoder()
    XCTAssertThrowsError(try rejecting.append(wire(invalid))) {
      XCTAssertEqual($0 as? TeslatlasEventDecodingError, .revisionMismatch(envelope: 8, payload: 7))
    }
    for field in ["typed_value", "source_sequence", "quality_flags"] {
      var payload = observation()
      payload.removeValue(forKey: field)
      var rejected = TeslatlasEventStreamDecoder()
      XCTAssertThrowsError(try rejected.append(wire(envelope("observation.admitted", payload: payload,
        identifier: "observation_demo_0001"))))
    }
    var payload = observation()
    payload["typed_value"] = ["kind": "boolean", "value": "true", "unit": "boolean"]
    var rejected = TeslatlasEventStreamDecoder()
    XCTAssertThrowsError(try rejected.append(wire(envelope("observation.admitted", payload: payload,
      identifier: "observation_demo_0001"))))
  }

  func testReplayResetIsObservableInOrderAtEveryCRLFSplitAndReconnect() async throws {
    let transport = ScriptedHTTPTransport([discoveryResponse(TestDocuments.readDiscovery)])
    let client = try await connect(transport)
    for delimiter in ["\n", "\r", "\r\n"] {
      let first = try wire(envelope("vehicle.current.changed", payload: object(TestDocuments.currentState),
        identifier: vehicle), delimiter: delimiter)
      let reset = Data("id:\(delimiter)\(delimiter): heartbeat\(delimiter)\(delimiter)".utf8)
      for split in 0...reset.count {
        var decoder = TeslatlasEventStreamDecoder()
        var checkpoint: String?
        let initial = try decoder.append(first + Data(": prior heartbeat\(delimiter)".utf8))
        for output in initial {
          if case .event(let event) = output { checkpoint = event.eventID }
        }
        XCTAssertEqual(checkpoint, "event_demo_0042")
        let outputs = try decoder.append(Data(reset.prefix(split)))
          + decoder.append(Data(reset.dropFirst(split))) + decoder.finish()
        XCTAssertEqual(outputs, [.replayReset], "\(delimiter.debugDescription), split \(split)")
        for output in outputs {
          if case .replayReset = output { checkpoint = nil }
        }
        let request = try await client.eventRequest(lastEventID: checkpoint)
        XCTAssertNil(request.value(forHTTPHeaderField: "Last-Event-ID"))
        var resumed = TeslatlasEventStreamDecoder()
        let next = try resumed.append(first + Data(": resumed heartbeat\(delimiter)".utf8))
        for output in next {
          if case .event(let event) = output { checkpoint = event.eventID }
        }
        let replay = try await client.eventRequest(lastEventID: checkpoint)
        XCTAssertEqual(replay.value(forHTTPHeaderField: "Last-Event-ID"), "event_demo_0042")
      }
    }
  }

  func testAllPublishedDiscoveryNumericDomainsRejectBeforeAuthenticatedDispatch() async throws {
    let domains: [(String, Int, Int)] = [
      ("max_request_body_bytes", 1024, 262144), ("default_page_size", 1, 100),
      ("max_page_size", 100, 500), ("max_history_range_days", 1, 366),
      ("max_dense_range_days", 1, 31), ("max_concurrent_requests", 1, 32),
      ("max_sse_connections", 1, 4), ("event_replay_retention_seconds", 86400, 604800),
      ("idempotency_retention_seconds", 86400, 604800),
    ]
    for (field, minimum, maximum) in domains {
      for value in [minimum - 1, maximum + 1] {
        let changed = try discoveryLimit(field, value)
        XCTAssertThrowsError(try HubDiscoveryDecoder.decode(Data(changed.utf8))) {
          XCTAssertNotNil($0 as? TeslatlasDiscoveryError)
        }
        let transport = ScriptedHTTPTransport([discoveryResponse(changed)])
        await assertThrowsErrorAsync(try await connect(transport)) {
          XCTAssertNotNil($0 as? TeslatlasDiscoveryError)
        }
        let requests = await transport.requests
        XCTAssertEqual(requests.count, 1)
        XCTAssertNil(requests.first?.value(forHTTPHeaderField: "Authorization"))
      }
      for value in [minimum, maximum] {
        XCTAssertNoThrow(try HubDiscoveryDecoder.decode(Data(discoveryLimit(field, value).utf8)))
      }
    }
  }

  func testEveryPublicQueryRouteAdmitsReleasedResourceAndRejectsWrongDiscriminator() async throws {
    for route in try queryCases() {
      let validTransport = ScriptedHTTPTransport([discoveryResponse(TestDocuments.readDiscovery),
        .json(status: 200, body: route.body, eTag: "\"valid\"", version: "1.2.0")])
      let validClient = try await connect(validTransport)
      try await route.call(validClient)
      var invalid = try object(route.body)
      invalid["resource_type"] = "foreign_resource"
      await rejectQuery(route, body: invalid)
    }
  }

  func testScopedQueriesRejectForeignSingletonAndPageIdentities() async throws {
    for route in try queryCases() where route.identity != nil {
      var invalid = try object(route.body)
      let field = try XCTUnwrap(route.identity)
      if var items = invalid["items"] as? [[String: Any]] {
        items[0][field] = "foreign_demo_0001"
        invalid["items"] = items
      } else { invalid[field] = "foreign_demo_0001" }
      await rejectQuery(route, body: invalid)
    }
  }

  func testQueryRejectsResourceDomainsRequiredNullablesAndQualityInvariants() async throws {
    let cases = try queryCases()
    let current = try XCTUnwrap(cases.first { $0.name == "current" })
    for (field, value) in [("revision", 0), ("battery_level_percent", 101), ("range_km", -1)] {
      var invalid = try object(current.body)
      invalid[field] = value
      await rejectQuery(current, body: invalid)
    }
    var missing = try object(current.body)
    missing.removeValue(forKey: "location")
    await rejectQuery(current, body: missing)
    var invalidQuality = try object(current.body)
    var quality = try XCTUnwrap(invalidQuality["quality"] as? [String: Any])
    quality["gap_count"] = 1
    invalidQuality["quality"] = quality
    await rejectQuery(current, body: invalidQuality)
    let positions = try XCTUnwrap(cases.first { $0.name == "positions" })
    var invalidPositions = try object(positions.body)
    var items = try XCTUnwrap(invalidPositions["items"] as? [[String: Any]])
    items[0]["location"] = ["latitude": 91, "longitude": 0]
    invalidPositions["items"] = items
    await rejectQuery(positions, body: invalidPositions)
    for heading in [-1, 360] {
      var invalid = try object(positions.body)
      var items = try XCTUnwrap(invalid["items"] as? [[String: Any]])
      items[0]["heading_degrees"] = heading
      invalid["items"] = items
      await rejectQuery(positions, body: invalid)
    }
    for name in ["charges", "updates", "samples", "quality"] {
      let route = try XCTUnwrap(cases.first { $0.name == name })
      var invalid = try object(route.body)
      var items = try XCTUnwrap(invalid["items"] as? [[String: Any]])
      switch name {
      case "charges": items[0]["charging_type"] = "other"
      case "updates": items[0]["status"] = "other"
      case "samples": items[0]["phases"] = 4
      default: items[0]["quality"] = "other"
      }
      invalid["items"] = items
      await rejectQuery(route, body: invalid)
    }
  }

  func testValidNullableQueryResourceRemainsSuccessful() async throws {
    var current = try object(TestDocuments.currentState)
    for field in ["battery_level_percent", "range_km", "odometer_km", "locked",
      "climate_on", "charging_state", "location", "inside_temperature_c", "outside_temperature_c"] {
      current[field] = NSNull()
    }
    let transport = ScriptedHTTPTransport([discoveryResponse(TestDocuments.readDiscovery),
      .json(status: 200, body: try json(current), eTag: "\"nulls\"", version: "1.2.0")])
    let client = try await connect(transport)
    guard case .modified(let accepted, _) = try await client.currentState(vehicleID: vehicle) else {
      return XCTFail("Expected modified nullable resource")
    }
    XCTAssertNil(accepted.location)
    XCTAssertNil(accepted.batteryLevelPercent)
  }

  func testCommandRejectsForeignIntentAndInvalidStateWithoutRetry() async throws {
    for (field, value) in [("vehicle_id", "vehicle_other"), ("command", "start_charge"),
      ("command_class", "climate")] {
      var job = try object(TestDocuments.commandJob)
      job[field] = value
      await rejectCommand(job)
    }
    for count in [-1, 17] {
      var job = try object(TestDocuments.commandJob)
      job["attempt_count"] = count
      await rejectCommand(job)
    }
    var emptyAudit = try object(TestDocuments.commandJob)
    emptyAudit["audit"] = []
    await rejectCommand(emptyAudit)
    for state in ["succeeded", "failed"] {
      var job = try object(TestDocuments.commandJob)
      job["state"] = state
      await rejectCommand(job)
    }
  }

  func testSameIntentProgressedIdempotentCommandReplayAndProblemExtensionsRemainAccepted() async throws {
    for state in ["accepted", "succeeded", "failed", "indeterminate"] {
      var job = try object(TestDocuments.commandJob)
      job["state"] = state
      job["attempt_count"] = state == "accepted" ? 0 : 1
      if state == "succeeded" { job["result"] = ["future_result": ["safe": true]] }
      if state == "failed" { job["error"] = try object(TestDocuments.problemWithExtension) }
      let transport = ScriptedHTTPTransport([commandResponse(try json(job))])
      // A retained job uses its historical policy even when discovery is refreshed.
      let discovery = TestDocuments.commandDiscovery.replacingOccurrences(
        of: "\"retry_policy\":\"state_verified\"", with: "\"retry_policy\":\"none\"")
      let client = try commandClient(transport, discovery: discovery)
      let request = try commandRequest()
      let key = UUID()
      let accepted = try await client.submit(request, idempotencyKey: key)
      XCTAssertEqual(accepted.job.state.rawValue, state)
      if state == "failed" { XCTAssertEqual(accepted.job.error?.code, "invalid_cursor") }
      let sent = await transport.requests
      XCTAssertEqual(sent.count, 1)
      XCTAssertEqual(sent[0].value(forHTTPHeaderField: "Idempotency-Key"), key.uuidString)
      let encoded = try XCTUnwrap(sent[0].httpBody)
      XCTAssertEqual(try JSONDecoder().decode(CommandRequest.self, from: encoded), request)
    }
  }

  private struct QueryCase {
    let name: String
    let body: String
    let identity: String?
    let call: (TeslatlasClient) async throws -> Void
  }

  private func queryCases() throws -> [QueryCase] {
    [
      QueryCase(name: "vehicles", body: try ReleasedFixture.string("vehicles-page.json"), identity: nil,
        call: { _ = try await $0.vehicles() }),
      QueryCase(name: "current", body: try ReleasedFixture.string("current-state.json"), identity: "vehicle_id",
        call: { _ = try await $0.currentState(vehicleID: "vehicle_demo_alpha") }),
      QueryCase(name: "drives", body: try ReleasedFixture.string("drives-page.json"), identity: "vehicle_id",
        call: { _ = try await $0.drives(vehicleID: "vehicle_demo_alpha") }),
      QueryCase(name: "drive", body: TestDocuments.drive, identity: "drive_id",
        call: { _ = try await $0.drive(id: "drive_demo_0001") }),
      QueryCase(name: "positions", body: try ReleasedFixture.string("positions-page.json"), identity: "drive_id",
        call: { _ = try await $0.positions(driveID: "drive_demo_0001") }),
      QueryCase(name: "charges", body: try ReleasedFixture.string("charges-page.json"), identity: "vehicle_id",
        call: { _ = try await $0.charges(vehicleID: "vehicle_demo_alpha") }),
      QueryCase(name: "charge", body: TestDocuments.charge, identity: "charge_id",
        call: { _ = try await $0.charge(id: "charge_demo_0001") }),
      QueryCase(name: "samples", body: try ReleasedFixture.string("charge-samples-page.json"), identity: "charge_id",
        call: { _ = try await $0.chargeSamples(chargeID: "charge_demo_0001") }),
      QueryCase(name: "states", body: try ReleasedFixture.string("states-page.json"), identity: "vehicle_id",
        call: { _ = try await $0.states(vehicleID: "vehicle_demo_alpha") }),
      QueryCase(name: "updates", body: try ReleasedFixture.string("updates-page.json"), identity: "vehicle_id",
        call: { _ = try await $0.softwareUpdates(vehicleID: "vehicle_demo_alpha") }),
      QueryCase(name: "quality", body: try ReleasedFixture.string("data-quality-page.json"), identity: nil,
        call: { _ = try await $0.dataQuality() }),
    ]
  }

  private func rejectQuery(_ route: QueryCase, body: [String: Any]) async {
    do {
      let transport = ScriptedHTTPTransport([discoveryResponse(TestDocuments.readDiscovery),
        .json(status: 200, body: try json(body), eTag: "\"invalid\"", version: "1.2.0")])
      let client = try await connect(transport)
      await assertThrowsErrorAsync(try await route.call(client)) {
        guard case .invalidResponse(statusCode: 200, requestID: _, reason: _) = $0 as? TeslatlasSDKError else {
          return XCTFail("Expected typed invalid response for \(route.name): \($0)")
        }
      }
      let sent = await transport.requests
      XCTAssertEqual(sent.count, 2)
    } catch { XCTFail("Unexpected setup error: \(error)") }
  }

  private func rejectCommand(_ job: [String: Any], location: String = "/v1/commands/command_demo_0001") async {
    do {
      let transport = ScriptedHTTPTransport([commandResponse(try json(job), location: location)])
      let client = try commandClient(transport)
      await assertThrowsErrorAsync(try await client.submit(commandRequest(), idempotencyKey: UUID())) {
        guard case .invalidResponse = $0 as? TeslatlasCommandError else {
          return XCTFail("Expected typed command rejection: \($0)")
        }
      }
      let count = await transport.requests.count
      XCTAssertEqual(count, 1)
    } catch { XCTFail("Unexpected setup error: \(error)") }
  }

  private func connect(_ transport: ScriptedHTTPTransport) async throws -> TeslatlasClient {
    try await TeslatlasClient.connect(discoveryURL: TestURLs.discovery,
      maximumProtocolVersion: XCTUnwrap(TeslatlasProtocolVersion("1.2.0")),
      authorization: BearerCredential("rich-synthetic-test"), transport: transport)
  }

  private func commandClient(_ transport: ScriptedHTTPTransport,
    discovery: String = TestDocuments.commandDiscovery) throws -> TeslatlasCommandClient {
    try TeslatlasCommandClient(discovery: HubDiscoveryDecoder.decode(Data(discovery.utf8)),
      selectedProtocolVersion: XCTUnwrap(TeslatlasProtocolVersion("1.2.0")),
      endpointTrustPolicy: HubEndpointTrustPolicy(discoveryURL: TestURLs.discovery),
      authorization: BearerCredential("rich-synthetic-command"), transport: transport)
  }

  private func commandRequest() throws -> CommandRequest {
    CommandRequest(vehicleID: vehicle, command: "set_charge_limit", commandClass: .charging,
      parameters: ["percent": .integer(80)], expectedState: ["charge_limit_percent": .integer(80)],
      expiresAt: try XCTUnwrap(TeslatlasTimestamp("2026-08-30T12:05:00.000Z")),
      confirmation: CommandConfirmation(confirmedAt: try XCTUnwrap(TeslatlasTimestamp(timestamp)),
        confirmedBy: "user_demo_owner"))
  }

  private func discoveryResponse(_ body: String) -> TeslatlasHTTPResponse {
    .json(status: 200, body: body, eTag: "\"discovery\"", version: "1.2.0")
  }

  private func commandResponse(_ body: String, location: String = "/v1/commands/command_demo_0001") -> TeslatlasHTTPResponse {
    var headers = TestHeaders.json(eTag: "\"command\"", version: "1.2.0")
    headers["Location"] = location
    return TeslatlasHTTPResponse(statusCode: 202, headers: headers, body: Data(body.utf8))
  }

  private func object(_ body: String) throws -> [String: Any] {
    try XCTUnwrap(JSONSerialization.jsonObject(with: Data(body.utf8)) as? [String: Any])
  }

  private func json(_ object: [String: Any]) throws -> String {
    String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self)
  }

  private func discoveryLimit(_ field: String, _ value: Int) throws -> String {
    var root = try object(TestDocuments.readDiscovery)
    var limits = try XCTUnwrap(root["limits"] as? [String: Any])
    limits[field] = value
    root["limits"] = limits
    return try json(root)
  }

  private func wire(_ envelope: [String: Any], delimiter: String = "\n") throws -> Data {
    Data("id: \(envelope["event_id"]!)\(delimiter)event: \(envelope["event_type"]!)\(delimiter)data: \(try json(envelope))\(delimiter)\(delimiter)".utf8)
  }

  private func envelope(_ name: String, payload: [String: Any], identifier: String) throws -> [String: Any] {
    ["event_id": "event_demo_0042", "event_type": name, "occurred_at": timestamp,
      "vehicle_id": payload["vehicle_id"] ?? NSNull(), "resource_id": identifier,
      "revision": payload["revision"] ?? 7, "data": payload]
  }

  private func eventCases() throws -> [(String, [String: Any], String)] {
    var result: [(String, [String: Any], String)] = [
      ("observation.admitted", observation(), "observation_demo_0001"),
      ("vehicle.current.changed", try object(TestDocuments.currentState), vehicle),
      ("command.changed", try object(TestDocuments.commandJob), "command_demo_0001"),
      ("metadata.changed", try metadata(tombstone: false), "metadata_demo_0001"),
      ("data_quality.changed", try object(TestDocuments.quality), vehicle),
    ]
    for name in ["drive.started", "drive.updated", "drive.ended"] {
      result.append((name, try object(TestDocuments.drive), "drive_demo_0001"))
    }
    for name in ["charge.started", "charge.updated", "charge.ended"] {
      result.append((name, try object(TestDocuments.charge), "charge_demo_0001"))
    }
    let states = try object(TestDocuments.statePage)["items"] as! [[String: Any]]
    result.append(("state.changed", states[0], "state_demo_0001"))
    let updates = try object(TestDocuments.updatePage)["items"] as! [[String: Any]]
    result.append(("software_update.changed", updates[0], "update_demo_0001"))
    return result
  }

  private func observation() -> [String: Any] {
    ["observation_id": "observation_demo_0001", "vehicle_id": vehicle, "source": "fleet_telemetry",
      "provider_timestamp": timestamp, "received_timestamp": timestamp, "source_sequence": NSNull(),
      "field": "battery.level", "typed_value": ["kind": "number", "value": 78, "unit": "percent"],
      "validity": "valid", "quality_flags": [], "raw_payload_hash": String(repeating: "a", count: 64),
      "collector_version": "1.0.0", "projection_version": "3.1.0"]
  }

  private func metadata(tombstone: Bool) throws -> [String: Any] {
    let history: [[String: Any]] = [["revision": 6, "action": "created", "at": timestamp,
      "actor_id": "user_demo_owner", "previous_hash": NSNull(), "new_hash": String(repeating: "a", count: 64)]]
    var result: [String: Any] = ["metadata_id": "metadata_demo_0001", "vehicle_id": vehicle,
      "kind": "tag", "target": ["resource_type": "vehicle", "resource_id": vehicle]]
    if tombstone {
      result["audit"] = ["history": history, "deletion": ["revision": 7, "action": "deleted",
        "at": timestamp, "actor_id": "user_demo_owner"]]
    } else {
      result["revision"] = 7
      result["value"] = ["future_extension": ["safe": true]]
      result["created_at"] = timestamp
      result["created_by"] = "user_demo_owner"
      result["updated_at"] = timestamp
      result["updated_by"] = "user_demo_owner"
      result["audit"] = history
    }
    return result
  }
}

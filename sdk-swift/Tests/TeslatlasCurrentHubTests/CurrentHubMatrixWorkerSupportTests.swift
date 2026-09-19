import Foundation
import XCTest

@testable import TeslatlasCurrentHub

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class CurrentHubMatrixWorkerSupportTests: XCTestCase {
  func testWorkerCellDeadlineIsSingleMonotonicBudget() throws {
    let deadline = try MatrixWorkerCellDeadline(
      startNanoseconds: 1_000_000_000,
      remainingMilliseconds: 100
    )

    XCTAssertEqual(
      try deadline.phaseDeadline(
        startingAt: 1_080_000_000,
        phaseMilliseconds: 80
      ),
      1_100_000_000
    )
    XCTAssertTrue(deadline.isExpired(at: 1_100_000_000))
  }

  func testWorkerCellDeadlineCapsLongPhaseAndRejectsExpiredStart() throws {
    let deadline = try MatrixWorkerCellDeadline(
      startNanoseconds: 5_000_000_000,
      remainingMilliseconds: 100
    )

    XCTAssertEqual(
      try deadline.phaseDeadline(startingAt: 5_050_000_000, phaseMilliseconds: 500),
      5_100_000_000
    )
    XCTAssertTrue(deadline.isExpired(at: 5_100_000_000))
    XCTAssertFalse(deadline.isExpired(at: 5_099_999_999))
    XCTAssertThrowsError(
      try MatrixWorkerCellDeadline(startNanoseconds: .max, remainingMilliseconds: 1)
    )
    XCTAssertThrowsError(try deadline.phaseDeadline(startingAt: 5_000_000_000, phaseMilliseconds: 0))
  }

  func testTranscriptUsesRedactedRouteAndActualResponseIdentity() async throws {
    let base = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(
        statusCode: 404,
        headers: ["X-Request-ID": "request-7"],
        body: Data(),
        finalURL: URL(string: "https://hub.example.invalid/v1/vehicles/33333333-3333-4333-8333-333333333333/current?secret=absent")!
      )
    ])
    let transport = CurrentHubTranscriptTransport(base: base)
    _ = try await transport.send(URLRequest(
      url: URL(string: "https://hub.example.invalid/v1/vehicles/33333333-3333-4333-8333-333333333333/current?cursor=sensitive")!
    ))

    let transcript = await transport.snapshot()
    XCTAssertEqual(transcript, [
      CurrentHubTranscriptEntry(
        method: "GET", route: "/v1/vehicles/{vehicle_id}/current",
        status: 404, requestID: "request-7",
        scope: "/v1/vehicles/33333333-3333-4333-8333-333333333333/current?cursor=sensitive"
      )
    ])
  }

  func testTranscriptRecordsOnlyConditionalETagWitnessHeaders() async throws {
    let eTag = CurrentHubTestData.eTag
    let base = ScriptedCurrentHubTransport([
      CurrentHubStubResponse(
        statusCode: 304,
        headers: [
          "X-Request-ID": "request-304",
          "ETag": eTag,
          "Cache-Control": "no-store",
          "Authorization": "must-not-be-recorded",
        ],
        body: Data(),
        finalURL: URL(string: "https://hub.example.invalid/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?limit=2&cursor=opaque")!
      )
    ])
    let transport = CurrentHubTranscriptTransport(base: base)
    var request = URLRequest(
      url: URL(string: "https://hub.example.invalid/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?limit=2&cursor=opaque")!
    )
    request.setValue(eTag, forHTTPHeaderField: "If-None-Match")

    _ = try await transport.send(request)

    let transcript = await transport.snapshot()
    XCTAssertEqual(transcript, [
      CurrentHubTranscriptEntry(
        method: "GET", route: "/v1/vehicles/{vehicle_id}/drives",
        status: 304, requestID: "request-304",
        scope: "/v1/vehicles/11111111-1111-4111-8111-111111111111/drives?limit=2&cursor=opaque",
        requestIfNoneMatch: eTag,
        responseETag: eTag,
        responseCacheControl: "no-store"
      )
    ])
  }

  func testWorkerConfigRejectsAdditionalCommandAuthority() throws {
    let data = try JSONSerialization.data(withJSONObject: [
      "schema_version": 1,
      "kind": "matrix-actor-worker",
      "actor_id": "swift_macos",
      "session_id": "12345678-1234-4234-8234-123456789abc",
      "cell_id": "swift__macos_arm64",
      "instance_nonce": String(repeating: "a", count: 64),
      "session_input_sha256": String(repeating: "b", count: 64),
      "phase_contract": ["id": "phase", "root": ["path": "/root", "sha256": String(repeating: "c", count: 64)], "local": ["path": "/local", "sha256": String(repeating: "c", count: 64)]],
      "inputs": [], "private_root": "/private", "coordination_dir": "/coord",
      "evidence_path": "/evidence", "log_path": "/log",
      "executable": "/untrusted",
    ])

    XCTAssertThrowsError(try CurrentHubMatrixWorkerConfig.decode(data))
  }

  func testWorkerConfigRejectsUnknownPhaseContractFields() throws {
    var config = validWorkerConfigObject()
    var phaseContract = try XCTUnwrap(config["phase_contract"] as? [String: Any])
    phaseContract["unexpected"] = true
    config["phase_contract"] = phaseContract

    XCTAssertThrowsError(try CurrentHubMatrixWorkerConfig.decode(try workerConfigData(config)))
  }

  func testWorkerConfigRejectsUnknownInputFields() throws {
    var config = validWorkerConfigObject()
    var inputs = try XCTUnwrap(config["inputs"] as? [[String: Any]])
    inputs[0]["unexpected"] = true
    config["inputs"] = inputs

    XCTAssertThrowsError(try CurrentHubMatrixWorkerConfig.decode(try workerConfigData(config)))
  }

  func testWorkerConfigRejectsUnknownRootBindingFields() throws {
    var config = validWorkerConfigObject()
    var phaseContract = try XCTUnwrap(config["phase_contract"] as? [String: Any])
    var root = try XCTUnwrap(phaseContract["root"] as? [String: Any])
    root["unexpected"] = true
    phaseContract["root"] = root
    config["phase_contract"] = phaseContract

    XCTAssertThrowsError(try CurrentHubMatrixWorkerConfig.decode(try workerConfigData(config)))
  }

  func testWorkerConfigRejectsUnknownLocalBindingFields() throws {
    var config = validWorkerConfigObject()
    var input = try XCTUnwrap((config["inputs"] as? [[String: Any]])?.first)
    var local = try XCTUnwrap(input["local"] as? [String: Any])
    local["unexpected"] = true
    input["local"] = local
    config["inputs"] = [input]

    XCTAssertThrowsError(try CurrentHubMatrixWorkerConfig.decode(try workerConfigData(config)))
  }

  func testWorkerConfigRejectsNonIntegerFieldTypes() throws {
    let valid = validWorkerConfigObject()
    let cases = [
      ("schema_version", "1.0"),
      ("schema_version", "true"),
      ("remaining_cell_ms", "1000.0"),
      ("remaining_cell_ms", "true"),
      ("remaining_cell_ms", "1.5"),
    ]

    for (field, replacement) in cases {
      var config = valid
      config[field] = replacement == "true" ? true : Double(replacement)!
      let data: Data
      if replacement.hasSuffix(".0") {
        let encoded = try workerConfigData(config)
        let source = try XCTUnwrap(String(data: encoded, encoding: .utf8))
        let integer = field == "schema_version" ? "1" : "1000"
        data = Data(source.replacingOccurrences(
          of: "\"\(field)\":\(integer)",
          with: "\"\(field)\":\(replacement)"
        ).utf8)
      } else {
        data = try workerConfigData(config)
      }
      XCTAssertThrowsError(
        try CurrentHubMatrixWorkerConfig.decode(data),
        "accepted non-integer \(field)=\(replacement)"
      )
    }
  }

  func testWorkerPhaseResultEnvelopeRequiresExactIdentityRecipeAndItems() throws {
    let config = try CurrentHubMatrixWorkerConfig.decode(
      try workerConfigData(validWorkerConfigObject())
    )
    let phase: [String: Any] = [
      "phase_id": "bootstrap_pair",
      "ordinal": 1,
      "recipe_id": "swift-bootstrap_pair",
      "operations": ["verify", "pair"],
      "timeout_ms": 200_000,
    ]
    let validItems: [[String: Any]] = [
      ["operation": "verify", "result": ["value": "verify-result"]],
      ["operation": "pair", "result": ["value": "pair-result"]],
    ]
    let valid: [String: Any] = [
      "schema_version": 1,
      "session_id": config.sessionID.uuidString.lowercased(),
      "actor_id": config.actorID,
      "phase_id": "bootstrap_pair",
      "results": validItems,
    ]

    let accepted = try matrixValidatePhaseResultEnvelope(
      valid, config: config, phase: phase, phaseID: "bootstrap_pair"
    )
    XCTAssertEqual(accepted["value"] as? String, "pair-result")

    var unknownEnvelopeKey = valid
    unknownEnvelopeKey["unexpected"] = true
    var wrongIdentity = valid
    wrongIdentity["session_id"] = "87654321-4321-4234-8234-123456789abc"
    var wrongSchemaType = valid
    wrongSchemaType["schema_version"] = 1.0
    for (label, envelope) in [
      ("unknown envelope key", unknownEnvelopeKey),
      ("wrong session identity", wrongIdentity),
      ("non-integer schema version", wrongSchemaType),
    ] {
      XCTAssertThrowsError(
        try matrixValidatePhaseResultEnvelope(
          envelope, config: config, phase: phase, phaseID: "bootstrap_pair"
        ),
        label
      )
    }

    var countMismatch = valid
    countMismatch["results"] = [validItems[0]]
    var orderMismatch = valid
    orderMismatch["results"] = [validItems[1], validItems[0]]

    var itemWithExtraKey = valid
    var extraItems = validItems
    extraItems[0]["unexpected"] = true
    itemWithExtraKey["results"] = extraItems

    var itemWithMissingKey = valid
    var missingItems = validItems
    missingItems[0].removeValue(forKey: "result")
    itemWithMissingKey["results"] = missingItems

    var itemWithWrongOperationType = valid
    var wrongOperationItems = validItems
    wrongOperationItems[0]["operation"] = 1
    itemWithWrongOperationType["results"] = wrongOperationItems

    var itemWithWrongResultType = valid
    var wrongResultItems = validItems
    wrongResultItems[0]["result"] = "result"
    itemWithWrongResultType["results"] = wrongResultItems

    for (label, envelope) in [
      ("result count mismatch", countMismatch),
      ("result operation order mismatch", orderMismatch),
      ("result item extra key", itemWithExtraKey),
      ("result item missing key", itemWithMissingKey),
      ("result item wrong operation type", itemWithWrongOperationType),
      ("result item wrong result type", itemWithWrongResultType),
    ] {
      XCTAssertThrowsError(
        try matrixValidatePhaseResultEnvelope(
          envelope, config: config, phase: phase, phaseID: "bootstrap_pair"
        ),
        label
      )
    }
  }
}

private func validWorkerConfigObject() -> [String: Any] {
  let digest = String(repeating: "c", count: 64)
  let root: [String: Any] = ["path": "/root", "sha256": digest]
  let local: [String: Any] = ["path": "/local", "sha256": digest]
  let phaseContract: [String: Any] = [
    "id": "phase",
    "root": root,
    "local": local,
  ]
  let input: [String: Any] = [
    "id": "initial_observation",
    "root": root,
    "local": local,
  ]
  return [
    "schema_version": 1,
    "kind": "matrix-actor-worker",
    "actor_id": "swift_macos",
    "session_id": "12345678-1234-4234-8234-123456789abc",
    "cell_id": "swift__macos_arm64",
    "instance_nonce": String(repeating: "a", count: 64),
    "session_input_sha256": String(repeating: "b", count: 64),
    "remaining_cell_ms": 1000,
    "phase_contract": phaseContract,
    "inputs": [input],
    "private_root": "/private",
    "coordination_dir": "/coord",
    "evidence_path": "/evidence",
    "log_path": "/log",
  ]
}

private func workerConfigData(_ object: [String: Any]) throws -> Data {
  try JSONSerialization.data(withJSONObject: object)
}

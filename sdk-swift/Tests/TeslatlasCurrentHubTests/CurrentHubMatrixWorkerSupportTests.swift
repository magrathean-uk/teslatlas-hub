import Foundation
import XCTest

@testable import TeslatlasCurrentHub

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class CurrentHubMatrixWorkerSupportTests: XCTestCase {
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
        status: 404, requestID: "request-7"
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
}

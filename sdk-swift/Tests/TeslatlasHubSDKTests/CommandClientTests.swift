import Foundation
import XCTest

@testable import TeslatlasCommands
@testable import TeslatlasHubSDK

final class CommandClientTests: XCTestCase {
  func testSubmissionUsesAdvertisedCatalogueIdempotencyAndOneNetworkAttempt()
    async throws
  {
    let transport = ScriptedHTTPTransport([
      TeslatlasHTTPResponse(
        statusCode: 202,
        headers: [
          "Cache-Control": "private, max-age=0, must-revalidate",
          "Content-Type": "application/json",
          "ETag": "\"command-accepted\"",
          "Location": "/v1/commands/command_demo_0001",
          "Teslatlas-Protocol-Version": "1.2.0",
          "Vary": "Authorization, Teslatlas-Protocol-Version",
        ],
        body: Data(TestDocuments.commandJob.utf8)
      )
    ])
    let client = try makeCommandClient(transport)
    let key = UUID(uuidString: "018f18d2-6f45-7b3c-8a91-3c7286a10d42")!

    let acceptance = try await client.submit(
      CommandRequest(
        vehicleID: "vehicle_demo_alpha",
        command: "set_charge_limit",
        commandClass: .charging,
        parameters: ["percent": .integer(80)],
        expectedState: ["charge_limit_percent": .integer(80)],
        expiresAt: try XCTUnwrap(
          TeslatlasTimestamp("2026-08-30T12:05:00.000Z")
        ),
        confirmation: CommandConfirmation(
          confirmedAt: try XCTUnwrap(
            TeslatlasTimestamp("2026-08-30T12:00:00.000Z")
          ),
          confirmedBy: "user_demo_owner"
        )
      ),
      idempotencyKey: key
    )

    XCTAssertEqual(acceptance.job.commandID, "command_demo_0001")
    XCTAssertEqual(acceptance.job.state, .accepted)
    XCTAssertEqual(acceptance.entityTag, EntityTag("\"command-accepted\""))
    XCTAssertEqual(acceptance.location, "/v1/commands/command_demo_0001")
    let requests = await transport.requests
    XCTAssertEqual(requests.count, 1)
    XCTAssertEqual(requests[0].httpMethod, "POST")
    XCTAssertEqual(requests[0].url?.path, "/v1/commands")
    XCTAssertEqual(
      requests[0].value(forHTTPHeaderField: "Idempotency-Key"),
      key.uuidString
    )
    XCTAssertEqual(
      requests[0].value(forHTTPHeaderField: "Teslatlas-Protocol-Version"),
      "1.2.0"
    )
    XCTAssertEqual(
      requests[0].value(forHTTPHeaderField: "Authorization"),
      "Bearer command-secret"
    )
  }

  func testConfirmationRequiredCommandFailsBeforeNetwork() async throws {
    let transport = ScriptedHTTPTransport([])
    let client = try makeCommandClient(transport)
    let request = CommandRequest(
      vehicleID: "vehicle_demo_alpha",
      command: "set_charge_limit",
      commandClass: .charging,
      parameters: ["percent": .integer(80)],
      expectedState: ["charge_limit_percent": .integer(80)],
      expiresAt: try XCTUnwrap(
        TeslatlasTimestamp("2026-08-30T12:05:00.000Z")
      )
    )

    await assertThrowsErrorAsync(
      try await client.submit(request, idempotencyKey: UUID())
    ) { error in
      XCTAssertEqual(
        error as? TeslatlasCommandError,
        .confirmationRequired("set_charge_limit")
      )
    }
    let requestCount = await transport.requests.count
    XCTAssertEqual(requestCount, 0)
  }

  func testRetryableServerProblemIsReturnedAfterExactlyOneAttempt() async throws {
    let transport = ScriptedHTTPTransport([
      TeslatlasHTTPResponse(
        statusCode: 503,
        headers: [
          "Content-Type": "application/problem+json",
          "Teslatlas-Protocol-Version": "1.2.0",
          "X-Request-ID": "request_unavailable_1",
        ],
        body: Data(TestDocuments.retryableProblem.utf8)
      )
    ])
    let client = try makeCommandClient(transport)
    let request = CommandRequest(
      vehicleID: "vehicle_demo_alpha",
      command: "set_charge_limit",
      commandClass: .charging,
      parameters: ["percent": .integer(80)],
      expectedState: ["charge_limit_percent": .integer(80)],
      expiresAt: try XCTUnwrap(
        TeslatlasTimestamp("2026-08-30T12:05:00.000Z")
      ),
      confirmation: CommandConfirmation(
        confirmedAt: try XCTUnwrap(
          TeslatlasTimestamp("2026-08-30T12:00:00.000Z")
        ),
        confirmedBy: "user_demo_owner"
      )
    )

    await assertThrowsErrorAsync(
      try await client.submit(request, idempotencyKey: UUID())
    ) { error in
      guard case .problem(let problem) = error as? TeslatlasSDKError else {
        return XCTFail("Expected typed problem")
      }
      XCTAssertTrue(problem.retryable)
      XCTAssertEqual(problem.code, "unavailable")
    }
    let requestCount = await transport.requests.count
    XCTAssertEqual(requestCount, 1)
  }

  func testCommandClientRejectsUntrustedAdvertisedCommandOrigin() throws {
    let malicious = TestDocuments.commandDiscovery.replacingOccurrences(
      of: "https://hub.example.invalid/v1",
      with: "https://attacker.example.invalid/v1"
    )
    let discovery = try HubDiscoveryDecoder.decode(Data(malicious.utf8))
    let policy = try HubEndpointTrustPolicy(discoveryURL: TestURLs.discovery)

    XCTAssertThrowsError(
      try TeslatlasCommandClient(
        discovery: discovery,
        selectedProtocolVersion: try XCTUnwrap(
          TeslatlasProtocolVersion("1.2.0")
        ),
        endpointTrustPolicy: policy,
        authorization: try BearerCredential("must-not-leak"),
        transport: ScriptedHTTPTransport([])
      )
    ) { error in
      XCTAssertEqual(
        error as? TeslatlasDiscoveryError,
        .untrustedEndpointOrigin("https://attacker.example.invalid")
      )
    }
  }

  private func makeCommandClient(_ transport: ScriptedHTTPTransport) throws
    -> TeslatlasCommandClient
  {
    let discovery = try HubDiscoveryDecoder.decode(
      Data(TestDocuments.commandDiscovery.utf8)
    )
    return try TeslatlasCommandClient(
      discovery: discovery,
      selectedProtocolVersion: try XCTUnwrap(
        TeslatlasProtocolVersion("1.2.0")
      ),
      endpointTrustPolicy: try HubEndpointTrustPolicy(
        discoveryURL: TestURLs.discovery
      ),
      authorization: try BearerCredential("command-secret"),
      transport: transport
    )
  }
}

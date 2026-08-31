import Foundation
import XCTest

@testable import TeslatlasHubSDK

final class QueryRoutesTests: XCTestCase {
  func testAllReleasedReadRoutesUseTypedModelsAndAuthorityPaths() async throws {
    let transport = ScriptedHTTPTransport([
      .json(
        status: 200, body: TestDocuments.readDiscovery, eTag: "\"discovery\"", version: "1.0.0"),
      .json(status: 200, body: TestDocuments.drivePage, eTag: "\"drives\"", version: "1.2.0"),
      .json(status: 200, body: TestDocuments.drive, eTag: "\"drive\"", version: "1.2.0"),
      .json(status: 200, body: TestDocuments.positionPage, eTag: "\"positions\"", version: "1.2.0"),
      .json(status: 200, body: TestDocuments.chargePage, eTag: "\"charges\"", version: "1.2.0"),
      .json(status: 200, body: TestDocuments.charge, eTag: "\"charge\"", version: "1.2.0"),
      .json(
        status: 200, body: TestDocuments.chargeSamplePage, eTag: "\"samples\"", version: "1.2.0"),
      .json(status: 200, body: TestDocuments.statePage, eTag: "\"states\"", version: "1.2.0"),
      .json(status: 200, body: TestDocuments.updatePage, eTag: "\"updates\"", version: "1.2.0"),
      .json(
        status: 200, body: TestDocuments.dataQualityPage, eTag: "\"quality\"", version: "1.2.0"),
    ])
    let client = try await makeReadClient(transport)
    let from = try XCTUnwrap(TeslatlasTimestamp("2026-08-01T00:00:00.000Z"))
    let to = try XCTUnwrap(TeslatlasTimestamp("2026-08-30T00:00:00.000Z"))
    let history = HistoryRequest(from: from, to: to, limit: 10)

    let drives = try await client.drives(
      vehicleID: "vehicle_demo_alpha",
      request: history
    )
    let drive = try await client.drive(id: "drive_demo_0001")
    let positions = try await client.positions(
      driveID: "drive_demo_0001",
      request: history
    )
    let charges = try await client.charges(
      vehicleID: "vehicle_demo_alpha",
      request: history
    )
    let charge = try await client.charge(id: "charge_demo_0001")
    let samples = try await client.chargeSamples(
      chargeID: "charge_demo_0001",
      request: history
    )
    let states = try await client.states(
      vehicleID: "vehicle_demo_alpha",
      request: history
    )
    let updates = try await client.softwareUpdates(
      vehicleID: "vehicle_demo_alpha",
      request: history
    )
    let quality = try await client.dataQuality(
      request: DataQualityRequest(
        vehicleID: "vehicle_demo_alpha",
        from: from,
        to: to,
        limit: 10
      )
    )

    XCTAssertEqual(try modified(drives).items.first?.driveID, "drive_demo_0001")
    XCTAssertEqual(try modified(drive).driveID, "drive_demo_0001")
    XCTAssertEqual(try modified(positions).items.first?.positionID, "position_demo_0001")
    XCTAssertEqual(try modified(charges).items.first?.chargeID, "charge_demo_0001")
    XCTAssertEqual(try modified(charge).chargeID, "charge_demo_0001")
    XCTAssertEqual(try modified(samples).items.first?.chargeSampleID, "sample_demo_0001")
    XCTAssertEqual(try modified(states).items.first?.stateID, "state_demo_0001")
    XCTAssertEqual(try modified(updates).items.first?.updateID, "update_demo_0001")
    XCTAssertEqual(try modified(quality).items.first?.subjectID, "vehicle_demo_alpha")

    let requests = await transport.requests
    XCTAssertEqual(
      requests.dropFirst().compactMap { $0.url?.path },
      [
        "/v1/vehicles/vehicle_demo_alpha/drives",
        "/v1/drives/drive_demo_0001",
        "/v1/drives/drive_demo_0001/positions",
        "/v1/vehicles/vehicle_demo_alpha/charges",
        "/v1/charges/charge_demo_0001",
        "/v1/charges/charge_demo_0001/samples",
        "/v1/vehicles/vehicle_demo_alpha/states",
        "/v1/vehicles/vehicle_demo_alpha/updates",
        "/v1/data-quality",
      ]
    )
    XCTAssertEqual(
      URLComponents(
        url: try XCTUnwrap(requests.last?.url),
        resolvingAgainstBaseURL: false
      )?.queryItems,
      [
        URLQueryItem(name: "vehicle_id", value: "vehicle_demo_alpha"),
        URLQueryItem(name: "from", value: from.rawValue),
        URLQueryItem(name: "to", value: to.rawValue),
        URLQueryItem(name: "limit", value: "10"),
      ]
    )
  }

  func testDenseHistoryRangeAboveDiscoveredLimitFailsBeforeNetwork() async throws {
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: TestDocuments.readDiscovery, eTag: "\"discovery\"", version: "1.0.0")
    ])
    let client = try await makeReadClient(transport)
    let request = HistoryRequest(
      from: try XCTUnwrap(TeslatlasTimestamp("2026-01-01T00:00:00.000Z")),
      to: try XCTUnwrap(TeslatlasTimestamp("2026-02-02T00:00:00.000Z"))
    )

    await assertThrowsErrorAsync(
      try await client.positions(driveID: "drive_demo_0001", request: request)
    ) { error in
      XCTAssertEqual(
        error as? TeslatlasSDKError,
        .limitExceeded(name: "history_range_days", maximum: 31, actual: 32)
      )
    }
    let requestCount = await transport.requests.count
    XCTAssertEqual(requestCount, 1)
  }

  private func makeReadClient(_ transport: ScriptedHTTPTransport) async throws
    -> TeslatlasClient
  {
    try await TeslatlasClient.connect(
      discoveryURL: TestURLs.discovery,
      maximumProtocolVersion: try XCTUnwrap(TeslatlasProtocolVersion("1.2.0")),
      authorization: try BearerCredential("test-token"),
      transport: transport
    )
  }

  private func modified<Value>(_ response: TeslatlasConditionalResponse<Value>)
    throws -> Value
  {
    guard case .modified(let value, _) = response else {
      throw TestRouteError.notModified
    }
    return value
  }
}

private enum TestRouteError: Error {
  case notModified
}

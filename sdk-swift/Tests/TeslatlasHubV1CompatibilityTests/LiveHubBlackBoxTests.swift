import Foundation
import TeslatlasHubV1Compatibility
import XCTest

final class LiveHubBlackBoxTests: XCTestCase {
  func testUnchangedHubV100ReadJourney() async throws {
    let environment = ProcessInfo.processInfo.environment
    guard let discoveryText = environment["TESLATLAS_HUB_V1_DISCOVERY_URL"],
      let discoveryURL = URL(string: discoveryText),
      let hubIDText = environment["TESLATLAS_HUB_V1_EXPECTED_HUB_ID"],
      let hubID = UUID(uuidString: hubIDText),
      let bearer = environment["TESLATLAS_HUB_V1_BEARER"]
    else {
      throw XCTSkip(
        "Set TESLATLAS_HUB_V1_DISCOVERY_URL, "
          + "TESLATLAS_HUB_V1_EXPECTED_HUB_ID and TESLATLAS_HUB_V1_BEARER"
      )
    }

    let client = try await HubV1Client.connect(
      discoveryURL: discoveryURL,
      expectedHubID: hubID,
      credential: try HubV1BearerCredential(bearer)
    )
    let discovery = await client.discoveryDocument()
    XCTAssertEqual(discovery.version, "1.0.0")
    XCTAssertEqual(
      discovery.sourceURL.absoluteString,
      "https://github.com/magrathean-uk/teslatlas-hub/tree/v1.0.0"
    )

    let vehicles = try await client.vehicles()
    guard let vehicle = vehicles.first else {
      throw XCTSkip("Live Hub exposes no vehicle to the paired device")
    }
    let state = try await client.currentState(vehicleID: vehicle.vehicleID)
    XCTAssertEqual(state.vehicleID, vehicle.vehicleID)

    guard discovery.capabilities.contains("query.drives") else {
      throw XCTSkip("Live Hub does not advertise query.drives")
    }
    let first = try await client.drives(
      vehicleID: vehicle.vehicleID,
      query: HubV1DriveQuery(limit: 1)
    )
    guard case .modified(let page, _) = first,
      let cursor = page.nextCursor
    else { return }
    _ = try await client.drives(
      vehicleID: vehicle.vehicleID,
      query: HubV1DriveQuery(limit: 1, cursor: cursor)
    )
  }
}

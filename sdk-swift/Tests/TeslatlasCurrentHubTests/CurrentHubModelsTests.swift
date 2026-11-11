import Foundation
import XCTest

@testable import TeslatlasCurrentHub

final class CurrentHubModelsTests: XCTestCase {
  func testDiscoveryDecodesCurrentProfileWithoutInventedHeaders() throws {
    let discovery = try JSONDecoder().decode(
      CurrentHubDiscovery.self,
      from: Data(
        #"{"hub_id":"11111111-1111-4111-8111-111111111111","protocol":"teslatlas-sync","protocol_major":1,"api_versions":["1.0"],"capabilities":["query.vehicles","query.current"],"version":"2026.36.2","sourceUrl":"https://example.invalid/source","pack_format":"sqlite-zstd"}"#.utf8
      )
    )

    XCTAssertEqual(discovery.hubID.uuidString.lowercased(), "11111111-1111-4111-8111-111111111111")
    XCTAssertEqual(discovery.apiVersions, ["1.0"])
    XCTAssertNil(discovery.manifestPublicKey)
  }

  func testCurrentStatePreservesNullAndZero() throws {
    let state = try JSONDecoder().decode(
      CurrentHubCurrentState.self,
      from: Data(
        #"{"vehicle_id":"11111111-1111-4111-8111-111111111111","battery_level":0,"speed":null,"observed_at_ms":0}"#.utf8
      )
    )

    XCTAssertEqual(state.batteryLevel, 0)
    XCTAssertNil(state.speedKilometresPerHour)
    XCTAssertEqual(state.observedAtMilliseconds, 0)
  }

  func testDriveIDDecodesLosslesslyAtSigned64Maximum() throws {
    let drive = try JSONDecoder().decode(
      CurrentHubDrive.self,
      from: Data(
        #"{"id":9223372036854775807,"vehicle_id":"11111111-1111-4111-8111-111111111111","start_date_ms":105,"end_date_ms":106}"#.utf8
      )
    )

    XCTAssertEqual(drive.id, Int64.max)
  }

  func testCursorIsBoundedOpaqueAndRedacted() throws {
    let cursor = try CurrentHubDriveCursor(rawValue: String(repeating: "x", count: 4_096))
    XCTAssertEqual(cursor.rawValue.count, 4_096)
    XCTAssertFalse(String(describing: cursor).contains("xxxx"))
    XCTAssertThrowsError(
      try CurrentHubDriveCursor(rawValue: String(repeating: "x", count: 4_097))
    )
    XCTAssertThrowsError(try CurrentHubDriveCursor(rawValue: "bad\nvalue"))
  }

  func testCredentialRequiresHubWireTokenAndRedactsIt() throws {
    let token = String(repeating: "a", count: 64)
    let credential = try CurrentHubCredential(
      deviceID: UUID(uuidString: "11111111-1111-4111-8111-111111111111")!,
      accessToken: token,
      expiresAtMilliseconds: 9_223_372_036_854_775_000
    )

    XCTAssertFalse(String(describing: credential).contains(token))
    XCTAssertThrowsError(
      try CurrentHubCredential(
        deviceID: UUID(),
        accessToken: "short",
        expiresAtMilliseconds: 1
      )
    )
  }

  func testHealthAndReadinessHaveDistinctShapes() throws {
    let health = try JSONDecoder().decode(
      CurrentHubHealth.self,
      from: Data(#"{"status":"ok","version":"2026.36.2"}"#.utf8)
    )
    let readiness = try JSONDecoder().decode(
      CurrentHubReadiness.self,
      from: Data(#"{"status":"ready"}"#.utf8)
    )

    XCTAssertEqual(health.status, "ok")
    XCTAssertEqual(health.version, "2026.36.2")
    XCTAssertEqual(readiness.status, "ready")
  }
}

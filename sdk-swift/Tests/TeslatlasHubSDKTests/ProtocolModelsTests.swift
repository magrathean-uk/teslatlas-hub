import Foundation
import XCTest

@testable import TeslatlasHubSDK

final class ProtocolModelsTests: XCTestCase {
  func testSemanticVersionRejectsNonCanonicalValuesAndOrdersNumerically() throws {
    XCTAssertNil(TeslatlasProtocolVersion("1.02.0"))
    XCTAssertNil(TeslatlasProtocolVersion("1.2"))
    XCTAssertNil(TeslatlasProtocolVersion("v1.2.0"))

    let older = try XCTUnwrap(TeslatlasProtocolVersion("1.9.0"))
    let newer = try XCTUnwrap(TeslatlasProtocolVersion("1.10.0"))
    XCTAssertLessThan(older, newer)
  }

  func testDiscoveryDecodesStableIdentityVersionsCapabilitiesAndLimits() throws {
    let document = try HubDiscoveryDecoder.decode(Data(TestDocuments.discovery.utf8))

    XCTAssertEqual(
      document.hubID,
      "urn:uuid:018f18d2-6f45-7b3c-8a91-3c7286a10d42"
    )
    XCTAssertEqual(document.protocolInfo.currentVersion.description, "1.2.0")
    XCTAssertEqual(
      document.protocolInfo.supportedVersions.map(\.description),
      ["1.0.0", "1.1.0", "1.2.0"]
    )
    XCTAssertEqual(document.capabilities.map(\.id), ["query.vehicles"])
    XCTAssertEqual(document.limits.maximumPageSize, 500)
    XCTAssertEqual(
      document.endpoints.api.absoluteString,
      "https://hub.example.invalid/v1"
    )
  }

  func testDiscoveryNegotiatesHighestCompatibleVersionNotNewerThanClient() throws {
    let document = try HubDiscoveryDecoder.decode(Data(TestDocuments.discovery.utf8))

    XCTAssertEqual(
      try document.protocolInfo.negotiate(
        maximum: XCTUnwrap(TeslatlasProtocolVersion("1.1.8"))
      ).description,
      "1.1.0"
    )
  }

  func testDiscoveryRejectsUnsupportedMajor() throws {
    let document = try HubDiscoveryDecoder.decode(Data(TestDocuments.discovery.utf8))

    XCTAssertThrowsError(
      try document.protocolInfo.negotiate(
        maximum: XCTUnwrap(TeslatlasProtocolVersion("2.0.0"))
      )
    ) { error in
      XCTAssertEqual(
        error as? TeslatlasDiscoveryError,
        .noCompatibleProtocolVersion(
          maximum: "2.0.0",
          supported: ["1.0.0", "1.1.0", "1.2.0"]
        )
      )
    }
  }

  func testDiscoveryRejectsCredentialsVINVehicleAndUserData() {
    let forbiddenFields = [
      "credentials", "provider_token", "vin", "vehicle_id", "user_data",
    ]

    for field in forbiddenFields {
      let injected = TestDocuments.discovery.replacingOccurrences(
        of: "\"hub_id\":",
        with: "\"\(field)\": \"private\", \"hub_id\":"
      )

      XCTAssertThrowsError(
        try HubDiscoveryDecoder.decode(Data(injected.utf8)),
        "Expected discovery to reject \(field)"
      ) { error in
        XCTAssertEqual(
          error as? TeslatlasDiscoveryError,
          .forbiddenDiscoveryField(field)
        )
      }
    }
  }

  func testDiscoveryRejectsDuplicateCapabilityIDs() {
    let duplicate = TestDocuments.discovery.replacingOccurrences(
      of: "] , \"endpoints\"",
      with:
        ", {\"id\":\"query.vehicles\",\"version\":\"1.0.0\",\"introduced_in\":\"1.0.0\",\"status\":\"stable\",\"href\":\"/v1/vehicles\"}] , \"endpoints\""
    )

    XCTAssertThrowsError(try HubDiscoveryDecoder.decode(Data(duplicate.utf8))) {
      error in
      XCTAssertEqual(
        error as? TeslatlasDiscoveryError,
        .invalidDocument("capability IDs must be unique")
      )
    }
  }

  func testTimestampPreservesExactMillisecondUTCWireValue() throws {
    let timestamp = try XCTUnwrap(
      TeslatlasTimestamp("2026-08-30T12:00:01.250Z")
    )

    XCTAssertEqual(timestamp.description, "2026-08-30T12:00:01.250Z")
    XCTAssertNil(TeslatlasTimestamp("2026-08-30T12:00:01Z"))
    XCTAssertNil(TeslatlasTimestamp("2026-08-30T12:00:01.250+00:00"))
  }

  func testTimestampRejectsImpossibleCalendarValues() {
    XCTAssertNil(TeslatlasTimestamp("2026-13-30T12:00:01.250Z"))
    XCTAssertNil(TeslatlasTimestamp("2026-02-30T12:00:01.250Z"))
    XCTAssertNil(TeslatlasTimestamp("2026-08-30T25:00:01.250Z"))
  }

  func testBearerCredentialDescriptionNeverContainsSecret() throws {
    let credential = try BearerCredential("secret-token-value")

    XCTAssertEqual(String(describing: credential), "BearerCredential(<redacted>)")
    XCTAssertFalse(String(reflecting: credential).contains("secret-token-value"))
  }

  func testBearerCredentialRejectsHeaderControlCharacters() {
    XCTAssertThrowsError(try BearerCredential("secret\nInjected: value"))
  }
}

import Foundation
import XCTest

@testable import TeslatlasHubV1Compatibility

final class BindingFixtureTests: XCTestCase {
  func testBundledBindingDescriptorIsPinnedToReleasedHub() throws {
    let descriptor = try HubV1BindingDescriptor.bundled()

    XCTAssertEqual(descriptor.bindingVersion, "1.0.0")
    XCTAssertEqual(descriptor.hubVersion, "1.0.0")
    XCTAssertEqual(descriptor.hubTag, "v1.0.0")
    XCTAssertEqual(
      descriptor.hubCommit,
      "a5e6c5c4f86776da96c9946f7e45b2080c571f86"
    )
    XCTAssertEqual(
      descriptor.bindingSHA256,
      "78aca4b6014625420efb66fbe35f6ea10d72a7f596210dac80964c33e0f04f65"
    )
    XCTAssertEqual(
      descriptor.supportedOperations,
      ["discovery", "vehicles", "current_state", "drives"]
    )
    XCTAssertEqual(
      descriptor.unavailableFeatures,
      ["events", "commands", "metadata", "charges"]
    )
  }

  func testSHA256ImplementationMatchesPublishedVector() {
    XCTAssertEqual(
      HubV1SHA256.hexDigest(of: Data("abc".utf8)),
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    )
  }

  func testBundledBindingPinsHTTPStatusesHeadersAndErrorMapping() throws {
    let binding = try HubV1BindingLoader.load()

    XCTAssertEqual(binding.authentication.header, "Authorization")
    XCTAssertEqual(binding.responses.successStatus, 200)
    XCTAssertEqual(binding.responses.notModifiedStatus, 304)
    XCTAssertEqual(binding.responses.unauthorizedStatus, 401)
    XCTAssertEqual(binding.responses.requestIDHeader, "X-Request-ID")
    XCTAssertEqual(binding.responses.entityTagHeader, "ETag")
    XCTAssertEqual(
      binding.responses.driveErrorStatusByCode["invalid_cursor"],
      400
    )
    XCTAssertEqual(
      binding.responses.driveErrorStatusByCode["vehicle_not_found"],
      404
    )
    XCTAssertEqual(
      binding.responses.driveErrorStatusByCode["service_unavailable"],
      503
    )
  }

  func testFixtureBytesMatchDeterministicManifest() throws {
    let manifest = try JSONDecoder().decode(
      [String: String].self,
      from: HubV1TestData.fixture("FIXTURE-SHA256")
    )
    let expectedNames = Set([
      "AUTHORITY",
      "current-state.json",
      "discovery-base.json",
      "discovery-full.json",
      "drives-page-1.json",
      "drives-page-2.json",
      "error-invalid-cursor.json",
      "vehicles.json",
    ])
    XCTAssertEqual(Set(manifest.keys), expectedNames)

    for name in expectedNames.sorted() {
      let data: Data
      if name == "AUTHORITY" {
        data = try HubV1TestData.fixture("AUTHORITY", extension: "")
      } else {
        data = try HubV1TestData.fixture(
          String(name.dropLast(".json".count))
        )
      }
      XCTAssertEqual(
        HubV1SHA256.hexDigest(of: data),
        manifest[name],
        "Fixture drift: \(name)"
      )
    }
  }

  func testFixtureAuthoritySeparatesProtocolAndHubProof() throws {
    let authority = String(
      decoding: try HubV1TestData.fixture("AUTHORITY", extension: ""),
      as: UTF8.self
    )
    XCTAssertTrue(authority.contains("protocol_release_binding_present=false"))
    XCTAssertTrue(authority.contains("hub_tag=v1.0.0"))
    XCTAssertTrue(
      authority.contains(
        "hub_commit=a5e6c5c4f86776da96c9946f7e45b2080c571f86"
      )
    )
  }
}

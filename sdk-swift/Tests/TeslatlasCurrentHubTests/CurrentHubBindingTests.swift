import XCTest

@testable import TeslatlasCurrentHub

final class CurrentHubBindingTests: XCTestCase {
  func testBindingLoadsApprovedProfileManifest() throws {
    let binding = try CurrentHubBinding.load()

    XCTAssertEqual(binding.profileID, "hub-http-v1@1.0.0")
    XCTAssertEqual(
      binding.manifestSHA256,
      "b80d940e8edd15896c797f659dd76e08c8b2cf2229e8386d96342b1fa4c7d926"
    )
    XCTAssertEqual(binding.testedHubVersions, ["2026.36.2"])
    XCTAssertEqual(binding.maximumResponseBytes, 1_048_576)
  }
}

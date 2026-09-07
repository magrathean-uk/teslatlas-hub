import XCTest

@testable import TeslatlasCurrentHub

final class CurrentHubBindingTests: XCTestCase {
  func testBindingLoadsApprovedProfileManifest() throws {
    let binding = try CurrentHubBinding.load()

    XCTAssertEqual(binding.profileID, "hub-http-v1@1.0.0")
    XCTAssertEqual(
      binding.manifestSHA256,
      "b3914d35d28374f6423af789e9ed6a4a4c82196a068c041946e24d609db0b05b"
    )
    XCTAssertEqual(binding.testedHubVersions, ["2026.36.2"])
    XCTAssertEqual(binding.maximumResponseBytes, 1_048_576)
  }
}

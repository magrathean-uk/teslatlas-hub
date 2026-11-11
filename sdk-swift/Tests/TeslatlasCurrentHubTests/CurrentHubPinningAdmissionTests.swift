import Foundation
import XCTest

@testable import TeslatlasCurrentHub

/// Pure admission checks only. No native transport, URLSession, curl or server is exercised.
final class CurrentHubPinningAdmissionTests: XCTestCase {
  func testPinningAdmissionRejectsMissingAndNonHTTPSURLs() {
    let pin = String(repeating: "a", count: 64)
    let urls: [URL?] = [
      nil,
      URL(string: "http://hub.example.invalid/claim"),
      URL(string: "ftp://hub.example.invalid/claim"),
      URL(string: "file:///synthetic/claim"),
      URL(string: "relative-claim"),
    ]
    for url in urls {
      XCTAssertThrowsError(
        try CurrentHubPinningAdmission.requireHTTPS(url: url, expectedLeafCertificateSHA256: pin)
      ) { error in
        XCTAssertEqual(
          error as? CurrentHubError,
          .invalidRequest("leaf certificate pinning requires HTTPS before transmission")
        )
      }
    }
  }

  func testPinningAdmissionAllowsHTTPS() throws {
    let pin = String(repeating: "a", count: 64)
    for url in ["https://hub.example.invalid/claim", "HTTPS://hub.example.invalid/claim"] {
      try CurrentHubPinningAdmission.requireHTTPS(
        url: URL(string: url),
        expectedLeafCertificateSHA256: pin
      )
    }
  }

  func testUnpinnedAdmissionPreservesHTTP() throws {
    try CurrentHubPinningAdmission.requireHTTPS(
      url: URL(string: "http://hub.example.invalid/fixture"),
      expectedLeafCertificateSHA256: nil
    )
  }
}

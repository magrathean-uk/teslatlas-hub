import Foundation
import XCTest

@testable import TeslatlasHubSDK

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class BearerDiagnosticsCorrectionTests: XCTestCase {
  func testDirectAndNestedDiagnosticsRedactWhileAuthorizationStillWorks() throws {
    let token = "synthetic-rich-diagnostic-token"
    let credential = try BearerCredential(token)
    struct Container { let credential: BearerCredential }
    let nested = Container(credential: credential)
    var directDump = ""
    var nestedDump = ""
    dump(credential, to: &directDump)
    dump(nested, to: &nestedDump)
    XCTAssertTrue(Mirror(reflecting: credential).children.isEmpty)
    let child = try XCTUnwrap(Mirror(reflecting: nested).children.first)
    XCTAssertTrue(Mirror(reflecting: child.value).children.isEmpty)
    for diagnostic in [directDump, nestedDump, String(describing: credential),
      String(reflecting: credential), String(reflecting: nested)] {
      XCTAssertFalse(diagnostic.contains(token))
    }
    var request = URLRequest(url: URL(string: "https://synthetic.invalid")!)
    credential.apply(to: &request)
    XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer \(token)")
  }
}

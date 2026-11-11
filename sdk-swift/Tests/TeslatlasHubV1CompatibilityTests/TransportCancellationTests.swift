import Foundation
import XCTest

@testable import TeslatlasHubV1Compatibility

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class TransportCancellationTests: XCTestCase {
  func testExplicitCancellationErrorPreservesCancellationClassification() {
    XCTAssertTrue(
      HubV1URLSessionTransport.classifiedTransportError(CancellationError()) is CancellationError
    )
  }

  func testCancelledTaskURLErrorPreservesCancellationClassification() async {
    let isCancellation = await Task {
      withUnsafeCurrentTask { $0?.cancel() }
      let error = HubV1URLSessionTransport.classifiedTransportError(URLError(.cancelled))
      return error is CancellationError
    }.value

    XCTAssertTrue(isCancellation)
  }

  func testURLErrorCancelledWithoutTaskCancellationRemainsTransportFailure() {
    XCTAssertFalse(Task.isCancelled)
    XCTAssertEqual(
      HubV1URLSessionTransport.classifiedTransportError(URLError(.cancelled)) as? HubV1Error,
      .transportFailure(code: URLError.Code.cancelled.rawValue)
    )
  }

  func testOtherTransportErrorsKeepExistingClassification() {
    let domainError = HubV1Error.unauthorized(requestID: "synthetic-request")
    XCTAssertEqual(
      HubV1URLSessionTransport.classifiedTransportError(domainError) as? HubV1Error,
      domainError
    )
    XCTAssertEqual(
      HubV1URLSessionTransport.classifiedTransportError(URLError(.timedOut)) as? HubV1Error,
      .transportFailure(code: URLError.Code.timedOut.rawValue)
    )
    XCTAssertEqual(
      HubV1URLSessionTransport.classifiedTransportError(TestSupportError.noResponse) as? HubV1Error,
      .transportFailure(code: -1)
    )
  }
}

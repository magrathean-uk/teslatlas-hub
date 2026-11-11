import Foundation
import XCTest

@testable import TeslatlasHubSDK

final class EventStreamContractTests: XCTestCase {
  func testKnownEventDecodesIdentityAndCapsServerRetry() throws {
    var decoder = TeslatlasEventStreamDecoder()
    let wire =
      "retry: 99999\nid: event_demo_0042\nevent: vehicle.current.changed\ndata: \(TestDocuments.currentStateEventLine)\n\n"

    let output = try decoder.append(Data(wire.utf8))

    XCTAssertEqual(output.count, 2)
    XCTAssertEqual(output.first, .retry(milliseconds: 30_000))
    guard case .event(let event) = output.last else {
      return XCTFail("Expected event")
    }
    XCTAssertEqual(event.eventID, "event_demo_0042")
    XCTAssertEqual(event.eventType, "vehicle.current.changed")
    XCTAssertEqual(event.vehicleID, "vehicle_demo_alpha")
    XCTAssertEqual(event.resourceID, "vehicle_demo_alpha")
    XCTAssertEqual(event.revision, 42)
  }

  func testUnknownEventIsIgnoredBeforeMalformedJSONIsDecoded() throws {
    var decoder = TeslatlasEventStreamDecoder()

    let output = try decoder.append(
      Data("id: future-1\nevent: future.event\ndata: not-json\n\n".utf8)
    )

    XCTAssertEqual(output, [])
  }

  func testRecognizedEventFailsClosedOnSSEAndEnvelopeIdentityMismatch() {
    var decoder = TeslatlasEventStreamDecoder()
    let wire =
      "id: wrong-id\nevent: vehicle.current.changed\ndata: \(TestDocuments.currentStateEventLine)\n\n"

    XCTAssertThrowsError(try decoder.append(Data(wire.utf8))) { error in
      XCTAssertEqual(
        error as? TeslatlasEventDecodingError,
        .eventIDMismatch(sse: "wrong-id", envelope: "event_demo_0042")
      )
    }
  }

  func testRecognizedEventFailsClosedOnPayloadRevisionMismatch() {
    var decoder = TeslatlasEventStreamDecoder()
    let changed = TestDocuments.currentStateEventLine.replacingOccurrences(
      of: "\"revision\":42,\"data\"",
      with: "\"revision\":41,\"data\""
    )
    let wire = "id: event_demo_0042\nevent: vehicle.current.changed\ndata: \(changed)\n\n"

    XCTAssertThrowsError(try decoder.append(Data(wire.utf8))) { error in
      XCTAssertEqual(
        error as? TeslatlasEventDecodingError,
        .revisionMismatch(envelope: 41, payload: 42)
      )
    }
  }

  func testEventRequestCarriesOpaqueReplayIDFiltersVersionAndBearer() async throws {
    let transport = ScriptedHTTPTransport([
      .json(status: 200, body: TestDocuments.readDiscovery, eTag: "\"discovery\"", version: "1.0.0")
    ])
    let client = try await TeslatlasClient.connect(
      discoveryURL: TestURLs.discovery,
      maximumProtocolVersion: try XCTUnwrap(TeslatlasProtocolVersion("1.2.0")),
      authorization: try BearerCredential("event-secret"),
      transport: transport
    )

    let request = try await client.eventRequest(
      lastEventID: "opaque.event_0042~-",
      vehicleID: "vehicle_demo_alpha",
      eventTypes: ["drive.updated", "charge.updated"]
    )

    XCTAssertEqual(request.url?.path, "/v1/events")
    XCTAssertEqual(
      URLComponents(url: try XCTUnwrap(request.url), resolvingAgainstBaseURL: false)?.queryItems,
      [
        URLQueryItem(name: "vehicle_id", value: "vehicle_demo_alpha"),
        URLQueryItem(name: "event_type", value: "drive.updated"),
        URLQueryItem(name: "event_type", value: "charge.updated"),
      ]
    )
    XCTAssertEqual(
      request.value(forHTTPHeaderField: "Last-Event-ID"),
      "opaque.event_0042~-"
    )
    XCTAssertEqual(
      request.value(forHTTPHeaderField: "Teslatlas-Protocol-Version"),
      "1.2.0"
    )
    XCTAssertEqual(
      request.value(forHTTPHeaderField: "Authorization"),
      "Bearer event-secret"
    )
    XCTAssertEqual(
      request.value(forHTTPHeaderField: "Accept"),
      "text/event-stream"
    )
  }
}

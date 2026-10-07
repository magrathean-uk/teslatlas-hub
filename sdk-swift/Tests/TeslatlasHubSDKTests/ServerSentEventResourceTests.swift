import Foundation
import XCTest

@testable import TeslatlasHubSDK

final class ServerSentEventResourceTests: XCTestCase {
  func testEveryUTF8AndCRLFSplitPreservesEventsRetryAndInheritedID() throws {
    let wire = Data("id: opaque-é\r\ndata: first 🌍\r\n\r\n: comment\rretry: 17\rdata: second\r\r".utf8)
    let expected: [ServerSentEventDecoderOutput] = [
      .event(ServerSentEvent(id: "opaque-é", name: nil, data: "first 🌍")),
      .retry(milliseconds: 17),
      .event(ServerSentEvent(id: "opaque-é", name: nil, data: "second")),
    ]
    for split in 0...wire.count {
      var decoder = ServerSentEventDecoder()
      var output = try decoder.append(Data(wire.prefix(split)))
      output.append(contentsOf: try decoder.append(Data(wire.dropFirst(split))))
      output.append(contentsOf: try decoder.finish())
      XCTAssertEqual(output, expected, "Split at byte \(split)")
    }
  }

  func testOneByteFragmentsAcceptUTF8LineBeyondFormerDefaultCeiling() throws {
    let payload = String(repeating: "é", count: 35_000)
    let wire = Data("id: long\ndata: \(payload)\n\n".utf8)
    var decoder = ServerSentEventDecoder()
    var output: [ServerSentEventDecoderOutput] = []
    for byte in wire {
      output.append(contentsOf: try decoder.append(Data([byte])))
    }
    XCTAssertEqual(output, [.event(ServerSentEvent(id: "long", name: nil, data: payload))])
    XCTAssertEqual(try decoder.finish(), [])
  }

  func testLargeBatchOfCommentsDoesNotConsumeTheLineOrEventBudget() throws {
    var decoder = ServerSentEventDecoder(
      limits: ServerSentEventDecoderLimits(maximumLineBytes: 32, maximumEventDataBytes: 16)
    )
    let comments = String(repeating: ":keep-alive\r\n", count: 50_000)
    let output = try decoder.append(Data("id: retained\n\(comments)data: first\n\ndata: second\n\n".utf8))
    XCTAssertEqual(
      output,
      [
        .event(ServerSentEvent(id: "retained", name: nil, data: "first")),
        .event(ServerSentEvent(id: "retained", name: nil, data: "second")),
      ]
    )
    XCTAssertEqual(try decoder.finish(), [])
  }

  func testDefaultSingleDataLineAdmitsFullAggregateBudgetAndRejectsOverflow() throws {
    let budget = 8 * 1_024 * 1_024
    let payload = String(repeating: "x", count: budget)
    var decoder = ServerSentEventDecoder()
    let output = try decoder.append(Data("data: \(payload)\n\n".utf8))
    XCTAssertEqual(output.count, 1)
    guard case .event(let event)? = output.first else {
      return XCTFail("Expected a full-budget event")
    }
    XCTAssertNil(event.id)
    XCTAssertNil(event.name)
    XCTAssertTrue(event.data == payload, "Full-budget event data must be preserved")

    XCTAssertThrowsError(
      try decoder.append(Data("id: stale\ndata: a\ndata: \(payload)\n".utf8))
    ) {
      XCTAssertEqual($0 as? ServerSentEventDecodingError, .eventDataTooLarge(limit: budget))
    }
    XCTAssertEqual(
      try decoder.append(Data("data: recovered\n\n".utf8)),
      [.event(ServerSentEvent(id: nil, name: nil, data: "recovered"))]
    )
  }

  func testExplicitLineLimitIsEnforcedAcrossFragmentsAndClearsPendingCRAndID() throws {
    var decoder = ServerSentEventDecoder(
      limits: ServerSentEventDecoderLimits(maximumLineBytes: 8, maximumEventDataBytes: 32)
    )
    XCTAssertEqual(try decoder.append(Data("id: x\r".utf8)), [])
    XCTAssertEqual(try decoder.append(Data("\ndata: ".utf8)), [])
    XCTAssertEqual(try decoder.append(Data("xx".utf8)), [])
    XCTAssertThrowsError(try decoder.append(Data("x".utf8))) {
      XCTAssertEqual($0 as? ServerSentEventDecodingError, .lineTooLong(limit: 8))
    }
    XCTAssertEqual(
      try decoder.append(Data("data: ok\r\n\r\n".utf8)),
      [.event(ServerSentEvent(id: nil, name: nil, data: "ok"))]
    )
  }

  func testFinishDispatchesTrailingCRBoundaryThenDiscardsUnterminatedEvent() throws {
    var decoder = ServerSentEventDecoder()
    XCTAssertEqual(try decoder.append(Data("id: first\rdata: delivered\r\r".utf8)), [])
    XCTAssertEqual(
      try decoder.finish(),
      [.event(ServerSentEvent(id: "first", name: nil, data: "delivered"))]
    )
    XCTAssertEqual(try decoder.append(Data("id: partial\rdata: incomplete\r".utf8)), [])
    XCTAssertEqual(try decoder.finish(), [])
    XCTAssertEqual(
      try decoder.append(Data("data: fresh\n\n".utf8)),
      [.event(ServerSentEvent(id: nil, name: nil, data: "fresh"))]
    )
  }

  func testInvalidUTF8AfterSplitCRResetsThePendingBoundaryAndIdentity() throws {
    var decoder = ServerSentEventDecoder()
    XCTAssertEqual(try decoder.append(Data("id: stale\r".utf8)), [])
    var invalidLine = Data("\ndata: ".utf8)
    invalidLine.append(contentsOf: [0xFF, 0x0D])
    XCTAssertEqual(try decoder.append(invalidLine), [])
    XCTAssertThrowsError(try decoder.append(Data([0x0A]))) {
      XCTAssertEqual($0 as? ServerSentEventDecodingError, .invalidUTF8)
    }
    XCTAssertEqual(
      try decoder.append(Data("data: fresh\n\n".utf8)),
      [.event(ServerSentEvent(id: nil, name: nil, data: "fresh"))]
    )
  }

  func testPublicDecoderAcceptsLargeSingleLineMetadataWithinAggregateBudget() throws {
    let value = String(repeating: "é", count: 35_000)
    let object: [String: Any] = [
      "event_id": "event_metadata_large", "event_type": "metadata.changed",
      "occurred_at": "2026-08-30T12:00:01.250Z", "vehicle_id": "vehicle_demo_alpha",
      "resource_id": "metadata_demo_0001", "revision": 7,
      "data": [
        "metadata_id": "metadata_demo_0001", "vehicle_id": "vehicle_demo_alpha",
        "kind": "tag", "target": ["resource_type": "vehicle", "resource_id": "vehicle_demo_alpha"],
        "revision": 7, "value": ["large_extension": value],
        "created_at": "2026-08-30T12:00:01.250Z", "created_by": "user_demo_owner",
        "updated_at": "2026-08-30T12:00:01.250Z", "updated_by": "user_demo_owner",
        "audit": [["revision": 7, "action": "created", "at": "2026-08-30T12:00:01.250Z",
          "actor_id": "user_demo_owner", "previous_hash": NSNull(),
          "new_hash": String(repeating: "a", count: 64)]],
      ],
    ]
    let json = String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self)
    var decoder = TeslatlasEventStreamDecoder()
    let output = try decoder.append(Data("id: event_metadata_large\nevent: metadata.changed\ndata: \(json)\n\n".utf8))
    XCTAssertEqual(output.count, 1)
    guard case .event(let event)? = output.first,
      case .object(let payload) = event.data
    else {
      return XCTFail("Expected complete metadata event")
    }
    XCTAssertEqual(event.eventID, "event_metadata_large")
    XCTAssertEqual(event.resourceID, "metadata_demo_0001")
    XCTAssertEqual(event.revision, 7)
    XCTAssertEqual(payload["value"], .object(["large_extension": .string(value)]))
  }
}

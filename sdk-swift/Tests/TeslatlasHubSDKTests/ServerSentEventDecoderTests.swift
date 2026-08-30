import Foundation
import XCTest

@testable import TeslatlasHubSDK

final class ServerSentEventDecoderTests: XCTestCase {
  func testDecodesFragmentedCRLFEventAndRetryInstruction() throws {
    var decoder = ServerSentEventDecoder()

    let first = try decoder.append(Data(":keep-alive\r".utf8))
    let second = try decoder.append(
      Data(
        "\nid: event-9\r\nevent: vehicle.current\r\ndata: {\"soc\":80}\r\ndata: ready\r\nretry: 1500\r\n\r"
          .utf8)
    )
    let third = try decoder.append(Data("\n".utf8))

    XCTAssertEqual(first, [])
    XCTAssertEqual(second, [.retry(milliseconds: 1_500)])
    XCTAssertEqual(
      third,
      [
        .event(
          ServerSentEvent(
            id: "event-9",
            name: "vehicle.current",
            data: "{\"soc\":80}\nready"
          )
        )
      ]
    )
  }

  func testLastEventIDPersistsUntilAnEmptyIDClearsIt() throws {
    var decoder = ServerSentEventDecoder()

    let output = try decoder.append(
      Data("id: first\ndata: one\n\ndata: two\n\nid:\ndata: three\n\n".utf8)
    )

    XCTAssertEqual(
      output,
      [
        .event(ServerSentEvent(id: "first", name: nil, data: "one")),
        .event(ServerSentEvent(id: "first", name: nil, data: "two")),
        .event(ServerSentEvent(id: nil, name: nil, data: "three")),
      ]
    )
  }

  func testIgnoresUnknownFieldsInvalidRetryAndEventsWithoutData() throws {
    var decoder = ServerSentEventDecoder()

    let output = try decoder.append(
      Data("id: retained\nunknown: value\nretry: -1\nretry: later\n\n\ndata: delivered\n\n".utf8)
    )

    XCTAssertEqual(
      output,
      [.event(ServerSentEvent(id: "retained", name: nil, data: "delivered"))]
    )
  }

  func testFinishDiscardsAnUnterminatedEvent() throws {
    var decoder = ServerSentEventDecoder()

    XCTAssertEqual(try decoder.append(Data("data: partial".utf8)), [])
    XCTAssertEqual(try decoder.finish(), [])
  }

  func testFinishRejectsInvalidUTF8() throws {
    var decoder = ServerSentEventDecoder()
    _ = try decoder.append(Data([0x64, 0x61, 0x74, 0x61, 0x3A, 0x20, 0xFF]))

    XCTAssertThrowsError(try decoder.finish()) { error in
      XCTAssertEqual(error as? ServerSentEventDecodingError, .invalidUTF8)
    }
  }

  func testRejectsOversizedLineAndResetsBufferedState() throws {
    var decoder = ServerSentEventDecoder(
      limits: ServerSentEventDecoderLimits(
        maximumLineBytes: 8,
        maximumEventDataBytes: 32
      )
    )

    XCTAssertThrowsError(
      try decoder.append(Data("123456789".utf8))
    ) { error in
      XCTAssertEqual(
        error as? ServerSentEventDecodingError,
        .lineTooLong(limit: 8)
      )
    }

    XCTAssertEqual(
      try decoder.append(Data("data: x\n\n".utf8)),
      [.event(ServerSentEvent(id: nil, name: nil, data: "x"))]
    )
  }

  func testRejectsOversizedEventDataAndResetsLastEventID() throws {
    var decoder = ServerSentEventDecoder(
      limits: ServerSentEventDecoderLimits(
        maximumLineBytes: 32,
        maximumEventDataBytes: 5
      )
    )

    XCTAssertThrowsError(
      try decoder.append(
        Data("id: unsafe\ndata: 1234\ndata: 56\n".utf8)
      )
    ) { error in
      XCTAssertEqual(
        error as? ServerSentEventDecodingError,
        .eventDataTooLarge(limit: 5)
      )
    }

    XCTAssertEqual(
      try decoder.append(Data("data: ok\n\n".utf8)),
      [.event(ServerSentEvent(id: nil, name: nil, data: "ok"))]
    )
  }
}

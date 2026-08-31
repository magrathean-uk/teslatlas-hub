import CryptoKit
import Foundation
import XCTest

@testable import TeslatlasCommands
@testable import TeslatlasHubSDK

final class ReleasedProtocolFixtureTests: XCTestCase {
  func testFixturesArePinnedToExactReleasedAuthority() throws {
    let authority = try ReleasedFixture.string("AUTHORITY")
      .trimmingCharacters(in: .whitespacesAndNewlines)

    XCTAssertEqual(
      authority,
      "teslatlas-protocol 79ced4c7fdc79520ad31d72a0280bf5f3f19f407 profile 1.2.0"
    )
  }

  func testFixtureBytesMatchReleasedAuthorityHashes() throws {
    let expected = try JSONDecoder().decode(
      [String: String].self,
      from: ReleasedFixture.data("FIXTURE-SHA256.json")
    )

    XCTAssertEqual(Set(expected.keys), Set(ReleasedFixture.protocolFixtureNames))
    for name in ReleasedFixture.protocolFixtureNames {
      let digest = SHA256.hash(data: try ReleasedFixture.data(name))
        .map { String(format: "%02x", $0) }
        .joined()
      XCTAssertEqual(digest, expected[name], "Fixture drift: \(name)")
    }
  }

  func testReleasedDiscoveryAndQueryExamplesDecodeThroughPublicModels() throws {
    let discovery = try HubDiscoveryDecoder.decode(
      ReleasedFixture.data("discovery.json")
    )
    XCTAssertEqual(
      discovery.capabilities.map(\.id),
      [
        "query.vehicles",
        "query.history",
        "events.sse",
        "data-quality",
        "commands.async",
        "metadata.mutable",
      ]
    )

    let decoder = JSONDecoder()
    XCTAssertEqual(
      try decoder.decode(
        VehicleCurrentState.self,
        from: ReleasedFixture.data("current-state.json")
      ).revision,
      42
    )
    XCTAssertEqual(
      try decoder.decode(
        VehiclePage.self,
        from: ReleasedFixture.data("vehicles-page.json")
      ).items.first?.vehicleID,
      "vehicle_demo_alpha"
    )
    XCTAssertEqual(
      try decoder.decode(
        DrivePage.self,
        from: ReleasedFixture.data("drives-page.json")
      ).items.first?.driveID,
      "drive_demo_0001"
    )
    XCTAssertEqual(
      try decoder.decode(
        PositionPage.self,
        from: ReleasedFixture.data("positions-page.json")
      ).items.first?.positionID,
      "position_demo_0001"
    )
    XCTAssertEqual(
      try decoder.decode(
        ChargePage.self,
        from: ReleasedFixture.data("charges-page.json")
      ).items.first?.chargeID,
      "charge_demo_0001"
    )
    XCTAssertEqual(
      try decoder.decode(
        ChargeSamplePage.self,
        from: ReleasedFixture.data("charge-samples-page.json")
      ).items.first?.chargeSampleID,
      "charge_sample_demo_0001"
    )
    XCTAssertEqual(
      try decoder.decode(
        StatePage.self,
        from: ReleasedFixture.data("states-page.json")
      ).items.first?.stateID,
      "state_demo_0001"
    )
    XCTAssertEqual(
      try decoder.decode(
        SoftwareUpdatePage.self,
        from: ReleasedFixture.data("updates-page.json")
      ).items.first?.updateID,
      "update_demo_0001"
    )
    XCTAssertEqual(
      try decoder.decode(
        DataQualityPage.self,
        from: ReleasedFixture.data("data-quality-page.json")
      ).items.first?.subjectID,
      "drive_demo_0001"
    )
  }

  func testReleasedProblemCommandAndEventExamplesDecode() throws {
    let decoder = JSONDecoder()
    XCTAssertEqual(
      try decoder.decode(
        TeslatlasProblemDetails.self,
        from: ReleasedFixture.data("error.json")
      ).code,
      "invalid_cursor"
    )
    XCTAssertEqual(
      try decoder.decode(
        CommandRequest.self,
        from: ReleasedFixture.data("command-request.json")
      ).command,
      "set_charge_limit"
    )
    XCTAssertEqual(
      try decoder.decode(
        CommandJob.self,
        from: ReleasedFixture.data("command-job.json")
      ).commandID,
      "command_demo_0001"
    )

    let eventData = try ReleasedFixture.data("event-envelope.json")
    let event = try decoder.decode(TeslatlasEventEnvelope.self, from: eventData)
    var streamDecoder = TeslatlasEventStreamDecoder()
    let compact = try JSONSerialization.data(
      withJSONObject: JSONSerialization.jsonObject(with: eventData)
    )
    let wire =
      "id: \(event.eventID)\nevent: \(event.eventType)\ndata: \(String(decoding: compact, as: UTF8.self))\n\n"

    XCTAssertEqual(
      try streamDecoder.append(Data(wire.utf8)),
      [.event(event)]
    )
  }

  func testDecoderRecognizesEveryEventInReleasedCatalogue() throws {
    struct Contract: Decodable {
      struct Event: Decodable { let name: String }
      let events: [Event]
    }
    let contract = try JSONDecoder().decode(
      Contract.self,
      from: ReleasedFixture.data("sse-contract.json")
    )

    for event in contract.events {
      var decoder = TeslatlasEventStreamDecoder()
      let wire = "id: event-test\nevent: \(event.name)\ndata: {}\n\n"
      XCTAssertThrowsError(
        try decoder.append(Data(wire.utf8)),
        "Expected \(event.name) to be recognized before envelope validation"
      ) { error in
        XCTAssertEqual(
          error as? TeslatlasEventDecodingError,
          .malformedEnvelope
        )
      }
    }
  }
}

enum ReleasedFixture {
  static let protocolFixtureNames = [
    "charge-samples-page.json",
    "charges-page.json",
    "command-job.json",
    "command-request.json",
    "current-state.json",
    "data-quality-page.json",
    "data-quality.json",
    "discovery.json",
    "drives-page.json",
    "error.json",
    "event-envelope.json",
    "positions-page.json",
    "sse-contract.json",
    "states-page.json",
    "updates-page.json",
    "vehicles-page.json",
  ]

  static func data(_ name: String) throws -> Data {
    guard
      let url = Bundle.module.url(
        forResource: name,
        withExtension: nil,
        subdirectory: "Fixtures"
      )
    else {
      throw ReleasedFixtureError.missing(name)
    }
    return try Data(contentsOf: url)
  }

  static func string(_ name: String) throws -> String {
    String(decoding: try data(name), as: UTF8.self)
  }
}

private enum ReleasedFixtureError: Error {
  case missing(String)
}

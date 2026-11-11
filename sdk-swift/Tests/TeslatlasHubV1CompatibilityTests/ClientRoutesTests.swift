import Foundation
import XCTest

@testable import TeslatlasHubV1Compatibility

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

final class ClientRoutesTests: XCTestCase {
  func testVehiclesUsesOnlyBoundRouteAndPairedBearer() async throws {
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(body: try HubV1TestData.fixture("vehicles"))
      ]
    )

    let vehicles = try await client.vehicles()

    XCTAssertEqual(vehicles.count, 1)
    XCTAssertEqual(vehicles[0].vehicleID, HubV1TestData.vehicleID)
    XCTAssertEqual(vehicles[0].displayName, "Roadrunner")
    let requests = await transport.requests()
    XCTAssertEqual(requests.count, 2)
    XCTAssertEqual(requests[1].url?.path, "/v1/vehicles")
    XCTAssertNil(requests[1].url?.query)
    XCTAssertEqual(requests[1].httpMethod, "GET")
    XCTAssertEqual(
      requests[1].value(forHTTPHeaderField: "Authorization"),
      "Bearer \(HubV1TestData.bearer)"
    )
  }

  func testCurrentStateUsesBoundVehicleRouteAndDecodesExactShape() async throws {
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(body: try HubV1TestData.fixture("current-state"))
      ]
    )

    let state = try await client.currentState(vehicleID: HubV1TestData.vehicleID)

    XCTAssertEqual(state.vehicleID, HubV1TestData.vehicleID)
    XCTAssertEqual(state.displayName, "Roadrunner")
    XCTAssertEqual(state.batteryLevel, 78)
    XCTAssertEqual(state.car?.model, "3")
    XCTAssertEqual(state.car?.settings.suspendMinutes, 12)
    XCTAssertEqual(state.odometerKilometres, 12_000.5)
    let captured = await transport.requests()
    let request = try XCTUnwrap(captured.last)
    XCTAssertEqual(
      request.url?.path,
      "/v1/vehicles/11111111-2222-4333-8444-555555555555/current"
    )
    XCTAssertEqual(
      request.value(forHTTPHeaderField: "Authorization"),
      "Bearer \(HubV1TestData.bearer)"
    )
  }

  func testDrivePageUsesBindingBoundsOpaqueCursorAndStrongETag() async throws {
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          headers: [
            "Content-Type": "application/json",
            "ETag": HubV1TestData.strongETag,
          ],
          body: try HubV1TestData.fixture("drives-page-1")
        )
      ]
    )

    let result = try await client.drives(
      vehicleID: HubV1TestData.vehicleID,
      query: HubV1DriveQuery(
        fromMilliseconds: 1_788_000_000_000,
        toMilliseconds: 1_789_000_000_000,
        limit: 25
      )
    )

    guard case .modified(let page, let eTag) = result else {
      return XCTFail("Expected modified drive page")
    }
    XCTAssertEqual(eTag.rawValue, HubV1TestData.strongETag)
    XCTAssertEqual(page.items.count, 1)
    XCTAssertEqual(page.items[0].id, 42)
    XCTAssertEqual(page.items[0].vehicleID, HubV1TestData.vehicleID)
    XCTAssertEqual(page.items[0].distanceKilometres, 20.4)
    XCTAssertNotNil(page.nextCursor)
    XCTAssertEqual(page.nextCursor?.rawValue.count, 168)
    XCTAssertEqual(page.nextCursor?.description, "HubV1DriveCursor(<redacted>)")

    let captured = await transport.requests()
    let request = try XCTUnwrap(captured.last)
    let components = try XCTUnwrap(
      URLComponents(url: try XCTUnwrap(request.url), resolvingAgainstBaseURL: false)
    )
    XCTAssertEqual(
      components.path,
      "/v1/vehicles/11111111-2222-4333-8444-555555555555/drives"
    )
    XCTAssertEqual(
      components.queryItems,
      [
        URLQueryItem(name: "from_ms", value: "1788000000000"),
        URLQueryItem(name: "to_ms", value: "1789000000000"),
        URLQueryItem(name: "limit", value: "25"),
      ]
    )
  }

  func testSecondDrivePageReusesOpaqueCursorWithoutParsingIt() async throws {
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          headers: [
            "Content-Type": "application/json",
            "ETag": HubV1TestData.strongETag,
          ],
          body: try HubV1TestData.fixture("drives-page-1")
        ),
        HubV1StubResponse(
          headers: [
            "Content-Type": "application/json",
            "ETag": "\"abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd\"",
          ],
          body: try HubV1TestData.fixture("drives-page-2")
        ),
      ]
    )

    let first = try await client.drives(vehicleID: HubV1TestData.vehicleID)
    guard case .modified(let firstPage, _) = first,
      let cursor = firstPage.nextCursor
    else {
      return XCTFail("Expected first-page cursor")
    }
    let second = try await client.drives(
      vehicleID: HubV1TestData.vehicleID,
      query: HubV1DriveQuery(cursor: cursor)
    )

    guard case .modified(let page, _) = second else {
      return XCTFail("Expected second drive page")
    }
    XCTAssertEqual(page.items.first?.id, 41)
    XCTAssertNil(page.nextCursor)
    let captured = await transport.requests()
    let request = try XCTUnwrap(captured.last)
    let components = try XCTUnwrap(
      URLComponents(url: try XCTUnwrap(request.url), resolvingAgainstBaseURL: false)
    )
    XCTAssertEqual(
      components.queryItems?.last,
      URLQueryItem(name: "cursor", value: cursor.rawValue)
    )
  }

  func testDrivePageRejectsMoreItemsThanRequested() async throws {
    var object = try HubV1TestData.jsonObject("drives-page-1")
    let item = try XCTUnwrap((object["items"] as? [[String: Any]])?.first)
    object["items"] = [item, item]
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          headers: [
            "Content-Type": "application/json",
            "ETag": HubV1TestData.strongETag,
          ],
          body: try HubV1TestData.encoded(object)
        )
      ]
    )

    await assertThrowsHubV1Error(
      try await client.drives(
        vehicleID: HubV1TestData.vehicleID,
        query: HubV1DriveQuery(limit: 1)
      )
    ) { error in
      guard case .invalidResponse(_, _, let reason) = error else {
        return XCTFail("Expected invalidResponse, got \(error)")
      }
      XCTAssertTrue(reason.contains("requested limit"))
    }
  }

  func testDrivePageRejectsItemOutsideRequestedTimeRange() async throws {
    var object = try HubV1TestData.jsonObject("drives-page-1")
    var items = try XCTUnwrap(object["items"] as? [[String: Any]])
    items[0]["start_date_ms"] = 1_700_000_000_000
    object["items"] = items
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          headers: [
            "Content-Type": "application/json",
            "ETag": HubV1TestData.strongETag,
          ],
          body: try HubV1TestData.encoded(object)
        )
      ]
    )

    await assertThrowsHubV1Error(
      try await client.drives(
        vehicleID: HubV1TestData.vehicleID,
        query: HubV1DriveQuery(
          fromMilliseconds: 1_788_000_000_000,
          toMilliseconds: 1_789_000_000_000
        )
      )
    ) { error in
      guard case .invalidResponse(_, _, let reason) = error else {
        return XCTFail("Expected invalidResponse, got \(error)")
      }
      XCTAssertTrue(reason.contains("time range"))
    }
  }

  func testDrivePageRejectsOrderOutsideBinding() async throws {
    var object = try HubV1TestData.jsonObject("drives-page-1")
    let newer = try XCTUnwrap((object["items"] as? [[String: Any]])?.first)
    var olderIDFirst = newer
    olderIDFirst["id"] = 41
    object["items"] = [olderIDFirst, newer]
    object["next_cursor"] = NSNull()
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          headers: [
            "Content-Type": "application/json",
            "ETag": HubV1TestData.strongETag,
          ],
          body: try HubV1TestData.encoded(object)
        )
      ]
    )

    await assertThrowsHubV1Error(
      try await client.drives(vehicleID: HubV1TestData.vehicleID)
    ) { error in
      guard case .invalidResponse(_, _, let reason) = error else {
        return XCTFail("Expected invalidResponse, got \(error)")
      }
      XCTAssertTrue(reason.contains("ordered"))
    }
  }

  func testDriveConditionalRequestReturnsNotModified() async throws {
    let (client, transport) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          statusCode: 304,
          headers: ["ETag": HubV1TestData.strongETag],
          body: Data()
        )
      ]
    )
    let tag = try HubV1EntityTag(rawValue: HubV1TestData.strongETag)

    let result = try await client.drives(
      vehicleID: HubV1TestData.vehicleID,
      ifNoneMatch: tag
    )

    XCTAssertEqual(result, .notModified(eTag: tag))
    let captured = await transport.requests()
    let request = try XCTUnwrap(captured.last)
    XCTAssertEqual(
      request.value(forHTTPHeaderField: "If-None-Match"),
      HubV1TestData.strongETag
    )
  }

  func testDriveErrorUsesOnlyStableBindingCodes() async throws {
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          statusCode: 400,
          headers: [
            "Content-Type": "application/json",
            "X-Request-ID": "request-demo-1",
          ],
          body: try HubV1TestData.fixture("error-invalid-cursor")
        )
      ]
    )

    await assertThrowsHubV1Error(
      try await client.drives(vehicleID: HubV1TestData.vehicleID)
    ) { error in
      XCTAssertEqual(
        error,
        .api(
          statusCode: 400,
          code: "invalid_cursor",
          message: "cursor is invalid for this vehicle and time range",
          requestID: "request-demo-1"
        )
      )
    }
  }

  func testDriveErrorRejectsStatusThatDoesNotMatchBoundCode() async throws {
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          statusCode: 503,
          headers: ["Content-Type": "application/json"],
          body: try HubV1TestData.fixture("error-invalid-cursor")
        )
      ]
    )

    await assertThrowsHubV1Error(
      try await client.drives(vehicleID: HubV1TestData.vehicleID)
    ) { error in
      guard case .invalidResponse(_, _, let reason) = error else {
        return XCTFail("Expected invalidResponse, got \(error)")
      }
      XCTAssertTrue(reason.contains("status"))
    }
  }

  func testUnauthorizedRequiresBoundBearerChallenge() async throws {
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(
          statusCode: 401,
          headers: [
            "WWW-Authenticate": "Bearer",
            "X-Request-ID": "request-auth-1",
          ],
          body: Data()
        )
      ]
    )

    await assertThrowsHubV1Error(try await client.vehicles()) { error in
      XCTAssertEqual(error, .unauthorized(requestID: "request-auth-1"))
    }
  }

  func testUnauthorizedWithoutBoundChallengeFailsClosed() async throws {
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(statusCode: 401, headers: [:], body: Data())
      ]
    )

    await assertThrowsHubV1Error(try await client.vehicles()) { error in
      guard case .invalidResponse(_, _, let reason) = error else {
        return XCTFail("Expected invalidResponse, got \(error)")
      }
      XCTAssertTrue(reason.contains("bearer challenge"))
    }
  }

  func testCurrentStateRejectsDifferentVehicleIdentity() async throws {
    var object = try HubV1TestData.jsonObject("current-state")
    object["vehicle_id"] = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(body: try HubV1TestData.encoded(object))
      ]
    )

    await assertThrowsHubV1Error(
      try await client.currentState(vehicleID: HubV1TestData.vehicleID)
    ) { error in
      guard case .invalidResponse(_, _, let reason) = error else {
        return XCTFail("Expected invalidResponse, got \(error)")
      }
      XCTAssertTrue(reason.contains("vehicle identity"))
    }
  }

  func testVehicleListRejectsFieldsOutsideBinding() async throws {
    var object = try HubV1TestData.jsonObject("vehicles")
    object["next_cursor"] = "invented"
    let (client, _) = try await makeHubV1Client(
      additionalResponses: [
        HubV1StubResponse(body: try HubV1TestData.encoded(object))
      ]
    )

    await assertThrowsHubV1Error(try await client.vehicles()) { error in
      guard case .invalidResponse = error else {
        return XCTFail("Expected invalidResponse, got \(error)")
      }
    }
  }
}

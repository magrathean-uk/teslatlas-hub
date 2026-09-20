import XCTest
import Foundation
#if canImport(CryptoKit)
import CryptoKit
#endif
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
import TeslatlasCurrentHub

@testable import CurrentHubConsumer

#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

final class CurrentHubConsumerTests: XCTestCase {
  func testSemanticSnapshotMatchesCanonicalThreeVehicleReceipt() throws {
    let vehicleIDs = [
      "11111111-1111-4111-8111-111111111111",
      "22222222-2222-4222-8222-222222222222",
      "33333333-3333-4333-8333-333333333333",
    ]
    let currents = try [
      semanticCurrentJSON(
        vehicleID: vehicleIDs[0], observedAt: "1788566400000", state: "online",
        batteryLevel: "0", estimatedRange: "160.93", odometer: "16093.44",
        speed: "16", insideTemperature: "21.5", outsideTemperature: "null",
        chargerPower: "null", locked: "true"
      ),
      semanticCurrentJSON(vehicleID: vehicleIDs[1]),
      semanticCurrentJSON(vehicleID: vehicleIDs[2]),
    ].map { try JSONDecoder().decode(CurrentHubCurrentState.self, from: Data($0.utf8)) }
    let drives = try [
      semanticDriveJSON(id: 5, vehicleID: vehicleIDs[0], start: 1_788_566_200_000),
      semanticDriveJSON(id: 4, vehicleID: vehicleIDs[0], start: 1_788_566_200_000),
      semanticDriveJSON(id: 3, vehicleID: vehicleIDs[0], start: 1_788_566_100_000),
      semanticDriveJSON(id: 2, vehicleID: vehicleIDs[0], start: 1_788_566_000_000),
      semanticDriveJSON(
        id: 1, vehicleID: vehicleIDs[0], start: 1_788_565_900_000,
        distance: "null", duration: "null"
      ),
    ].map { try JSONDecoder().decode(CurrentHubDrive.self, from: Data($0.utf8)) }

    let data = try ConsumerSemanticSnapshot.encode(
      vehicles: [
        ConsumerSemanticVehicle(current: currents[0], drives: drives),
        ConsumerSemanticVehicle(current: currents[1], drives: []),
        ConsumerSemanticVehicle(current: currents[2], drives: []),
      ],
      fromMilliseconds: 1_788_565_900_000,
      toMilliseconds: 1_788_566_200_001,
      boundaryChecks: [
        ConsumerHistoryBoundaryCheck(
          window: "before-equal-start", fromMilliseconds: 1_788_565_900_000,
          toMilliseconds: 1_788_566_200_000, count: 3
        ),
        ConsumerHistoryBoundaryCheck(
          window: "equal-start-only", fromMilliseconds: 1_788_566_200_000,
          toMilliseconds: 1_788_566_200_001, count: 2
        ),
      ]
    )

    XCTAssertEqual(data.count, 2_959)
    #if canImport(CryptoKit)
      XCTAssertEqual(
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined(),
        "20b164b499673ba207136dcb0107d8c8d74170029721125442914b99f3ae173a"
      )
    #endif
    let root = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    XCTAssertEqual(Set(root.keys), [
      "schema_version", "profile", "vehicle_order", "vehicles",
      "history_boundary_checks", "null_rule", "stale_rule",
    ])
    let vehicles = try XCTUnwrap(root["vehicles"] as? [[String: Any]])
    XCTAssertEqual(vehicles.count, 3)
    let boundaries = try XCTUnwrap(root["history_boundary_checks"] as? [[String: Any]])
    XCTAssertEqual(boundaries.map { ($0["count"] as? NSNumber)?.intValue }, [3, 2])
    XCTAssertEqual(boundaries.map { ($0["from_ms"] as? NSNumber)?.int64Value }, [
      1_788_565_900_000, 1_788_566_200_000,
    ])
    XCTAssertEqual(boundaries.map { ($0["to_ms"] as? NSNumber)?.int64Value }, [
      1_788_566_200_000, 1_788_566_200_001,
    ])
    let firstCurrent = try XCTUnwrap(vehicles[0]["current"] as? [String: Any])
    XCTAssertEqual((firstCurrent["battery_level"] as? [String: Any])?["value"] as? Int, 0)
    XCTAssertTrue((firstCurrent["outside_temperature"] as? [String: Any])?["value"] is NSNull)
    let firstHistory = try XCTUnwrap(vehicles[0]["history"] as? [String: Any])
    let order = try XCTUnwrap(firstHistory["order"] as? [[String: Any]])
    XCTAssertEqual(order.prefix(2).compactMap { ($0["start_date_ms"] as? NSNumber)?.int64Value }, [
      1_788_566_200_000, 1_788_566_200_000,
    ])
    XCTAssertTrue((order.last?["distance"] as? [String: Any])?["value"] is NSNull)
    XCTAssertTrue((order.last?["duration"] as? [String: Any])?["value"] is NSNull)
    XCTAssertFalse(containsPrivateSemanticMaterial(root, vehicleIDs: vehicleIDs))
  }

  func testRestartContinuityRejectsChangedOrLostDataAtTheSameVehicleCount() throws {
    let firstVehicleID = "11111111-1111-4111-8111-111111111111"
    let secondVehicleID = "22222222-2222-4222-8222-222222222222"
    let initialCurrents = try [
      semanticCurrentJSON(
        vehicleID: firstVehicleID, observedAt: "1788566400000", state: "online",
        batteryLevel: "80"
      ),
      semanticCurrentJSON(vehicleID: secondVehicleID),
    ].map { try JSONDecoder().decode(CurrentHubCurrentState.self, from: Data($0.utf8)) }
    let changedCurrents = try [
      semanticCurrentJSON(
        vehicleID: firstVehicleID, observedAt: "1788566400000", state: "online",
        batteryLevel: "79"
      ),
      semanticCurrentJSON(vehicleID: secondVehicleID),
    ].map { try JSONDecoder().decode(CurrentHubCurrentState.self, from: Data($0.utf8)) }
    let drives = try [
      semanticDriveJSON(id: 2, vehicleID: firstVehicleID, start: 8_000),
      semanticDriveJSON(id: 1, vehicleID: firstVehicleID, start: 5_000),
    ].map { try JSONDecoder().decode(CurrentHubDrive.self, from: Data($0.utf8)) }
    let boundaries = [
      ConsumerHistoryBoundaryCheck(
        window: "before-equal-start", fromMilliseconds: 1_000,
        toMilliseconds: 8_000, count: 1
      ),
      ConsumerHistoryBoundaryCheck(
        window: "equal-start-only", fromMilliseconds: 8_000,
        toMilliseconds: 8_001, count: 1
      ),
    ]
    func snapshot(currents: [CurrentHubCurrentState], drives: [CurrentHubDrive]) throws -> Data {
      XCTAssertEqual(currents.count, 2)
      return try ConsumerSemanticSnapshot.encode(
        vehicles: [
          ConsumerSemanticVehicle(current: currents[0], drives: drives),
          ConsumerSemanticVehicle(current: currents[1], drives: []),
        ],
        fromMilliseconds: 1_000,
        toMilliseconds: 9_001,
        boundaryChecks: boundaries
      )
    }

    let beforeRestart = try snapshot(currents: initialCurrents, drives: drives)
    let changedAfterRestart = try snapshot(currents: changedCurrents, drives: drives)
    let lostAfterRestart = try snapshot(currents: initialCurrents, drives: [drives[0]])

    for afterRestart in [changedAfterRestart, lostAfterRestart] {
      XCTAssertThrowsError(
        try ConsumerSemanticSnapshot.requireUnchanged(
          before: beforeRestart,
          after: afterRestart
        )
      ) { error in
        XCTAssertEqual(error as? ConsumerError, .restartSemanticMismatch)
      }
    }
  }

  func testExclusiveWriterCreatesOwnerOnlyRegularFileAndRejectsExistingOrSymlink() throws {
    let directory = FileManager.default.temporaryDirectory
      .appendingPathComponent("current-hub-output-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
    defer { try? FileManager.default.removeItem(at: directory) }
    let output = directory.appendingPathComponent("snapshot.json")
    try OwnerOnlyExclusiveFileWriter.write(Data("private".utf8), path: output.path)

    var metadata = stat()
    XCTAssertEqual(lstat(output.path, &metadata), 0)
    XCTAssertEqual(metadata.st_mode & S_IFMT, S_IFREG)
    XCTAssertEqual(metadata.st_uid, getuid())
    XCTAssertEqual(metadata.st_mode & 0o777, 0o600)
    XCTAssertThrowsError(
      try OwnerOnlyExclusiveFileWriter.write(Data("replacement".utf8), path: output.path)
    )
    XCTAssertEqual(try Data(contentsOf: output), Data("private".utf8))

    let target = directory.appendingPathComponent("target")
    let link = directory.appendingPathComponent("link")
    try Data("untouched".utf8).write(to: target)
    XCTAssertEqual(symlink(target.path, link.path), 0)
    XCTAssertThrowsError(
      try OwnerOnlyExclusiveFileWriter.write(Data("replacement".utf8), path: link.path)
    )
    XCTAssertEqual(try Data(contentsOf: target), Data("untouched".utf8))
  }

  func testConfigurationRequiresInvitationForInMemorySession() throws {
    let data = Data("""
      {"endpoint":"https://hub.example","expectedHubID":"11111111-1111-4111-8111-111111111111","deviceName":"test"}
      """.utf8)

    XCTAssertThrowsError(try ConsumerConfig.decode(from: data))
  }

  func testConfigurationCarriesCanonicalThreeVehicleExpectationsAndPrivateOutputs() throws {
    let data = Data("""
      {"endpoint":"https://hub.example","expectedHubID":"11111111-1111-4111-8111-111111111111","invitationPath":"/private/invitation.json","deviceName":"test","expectedVehicleCount":3,"expectedObservedCount":1,"expectedAbsentCount":2,"semanticSnapshotPath":"/private/semantic.json","cleanupDeviceIDPath":"/private/device-id","driveFromMs":1788565900000,"driveToMs":1788566200001}
      """.utf8)

    let config = try ConsumerConfig.decode(from: data)
    XCTAssertEqual(config.expectedVehicleCount, 3)
    XCTAssertEqual(config.expectedObservedCount, 1)
    XCTAssertEqual(config.expectedAbsentCount, 2)
    XCTAssertNil(config.expectedDriveCounts)
    XCTAssertFalse(config.deriveLatestDriveBoundaryChecks)
    XCTAssertEqual(config.semanticSnapshotPath, "/private/semantic.json")
    XCTAssertEqual(config.cleanupDeviceIDPath, "/private/device-id")

    let invalid = Data("""
      {"endpoint":"https://hub.example","expectedHubID":"11111111-1111-4111-8111-111111111111","invitationPath":"/private/invitation.json","deviceName":"test","expectedVehicleCount":3,"expectedObservedCount":1,"expectedAbsentCount":1,"semanticSnapshotPath":"/private/semantic.json","cleanupDeviceIDPath":"/private/device-id","driveFromMs":1788565900000,"driveToMs":1788566200001}
      """.utf8)
    XCTAssertThrowsError(try ConsumerConfig.decode(from: invalid))
  }

  func testConfigurationAcceptsArbitraryWindowDriveCountsAndDerivedBoundaries() throws {
    let data = Data("""
      {"endpoint":"https://hub.example","expectedHubID":"11111111-1111-4111-8111-111111111111","invitationPath":"/private/invitation.json","deviceName":"test","expectedVehicleCount":2,"expectedObservedCount":2,"expectedAbsentCount":0,"expectedDriveCounts":[3,1],"deriveLatestDriveBoundaryChecks":true,"semanticSnapshotPath":"/private/semantic.json","cleanupDeviceIDPath":"/private/device-id","driveFromMs":1000,"driveToMs":9001}
      """.utf8)

    let config = try ConsumerConfig.decode(from: data)
    XCTAssertEqual(config.driveFromMilliseconds, 1_000)
    XCTAssertEqual(config.driveToMilliseconds, 9_001)
    XCTAssertEqual(config.expectedDriveCounts, [3, 1])
    XCTAssertTrue(config.deriveLatestDriveBoundaryChecks)

    let wrongCount = Data("""
      {"endpoint":"https://hub.example","expectedHubID":"11111111-1111-4111-8111-111111111111","invitationPath":"/private/invitation.json","deviceName":"test","expectedVehicleCount":2,"expectedObservedCount":2,"expectedAbsentCount":0,"expectedDriveCounts":[3],"deriveLatestDriveBoundaryChecks":true,"semanticSnapshotPath":"/private/semantic.json","cleanupDeviceIDPath":"/private/device-id","driveFromMs":1000,"driveToMs":9001}
      """.utf8)
    XCTAssertThrowsError(try ConsumerConfig.decode(from: wrongCount))
  }

  func testConfigurationRequiresPairedRestartMarkers() throws {
    let data = Data("""
      {"endpoint":"https://hub.example","expectedHubID":"11111111-1111-4111-8111-111111111111","invitationPath":"/private/invitation.json","deviceName":"test","restartReadyPath":"/private/ready"}
      """.utf8)

    XCTAssertThrowsError(try ConsumerConfig.decode(from: data))
  }

  func testPrivateFileReaderRejectsGroupReadableAndOversizedFiles() throws {
    let fileURL = FileManager.default.temporaryDirectory
      .appendingPathComponent("current-hub-consumer-\(UUID().uuidString)")
    defer { try? FileManager.default.removeItem(at: fileURL) }
    try Data("private".utf8).write(to: fileURL)

    XCTAssertEqual(chmod(fileURL.path, 0o640), 0)
    XCTAssertThrowsError(
      try OwnerOnlyFileReader.read(path: fileURL.path, maximumBytes: 64)
    ) { error in
      XCTAssertEqual(error as? ConsumerPrivateInputError, .insecurePermissions)
    }

    XCTAssertEqual(chmod(fileURL.path, 0o600), 0)
    XCTAssertThrowsError(
      try OwnerOnlyFileReader.read(path: fileURL.path, maximumBytes: 4)
    ) { error in
      XCTAssertEqual(error as? ConsumerPrivateInputError, .tooLarge)
    }
  }

  func testDrivePagerReusesWindowAndRejectsCursorCycle() async throws {
    let vehicleID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    let firstCursor = try CurrentHubDriveCursor(rawValue: "cursor-a")
    let pages = [
      ConsumerDrivePage(itemCount: 2, nextCursor: firstCursor),
      ConsumerDrivePage(itemCount: 1, nextCursor: firstCursor),
    ]
    let recorder = QueryRecorder()

    do {
      _ = try await ConsumerDrivePager.fetch(
        vehicleID: vehicleID,
        fromMilliseconds: 100,
        toMilliseconds: 200,
        limit: 2,
        maximumPages: 3
      ) { query in
        let index = await recorder.append(query)
        return pages[index]
      }
      XCTFail("Expected a cursor cycle")
    } catch let error as ConsumerError {
      XCTAssertEqual(error, .paginationCursorCycle)
    }

    let queries = await recorder.values()
    XCTAssertEqual(queries.count, 2)
    XCTAssertEqual(queries[0].fromMilliseconds, 100)
    XCTAssertEqual(queries[0].toMilliseconds, 200)
    XCTAssertEqual(queries[0].limit, 2)
    XCTAssertNil(queries[0].cursor)
    XCTAssertEqual(queries[1].fromMilliseconds, 100)
    XCTAssertEqual(queries[1].toMilliseconds, 200)
    XCTAssertEqual(queries[1].limit, 2)
    XCTAssertEqual(queries[1].cursor, firstCursor)
  }

  func testDrivePagerStopsAtMaximumPages() async throws {
    let vehicleID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    let firstCursor = try CurrentHubDriveCursor(rawValue: "cursor-a")
    let secondCursor = try CurrentHubDriveCursor(rawValue: "cursor-b")
    let pages = [
      ConsumerDrivePage(itemCount: 2, nextCursor: firstCursor),
      ConsumerDrivePage(itemCount: 2, nextCursor: secondCursor),
    ]
    let pageProvider = PageProvider(pages: pages)

    do {
      _ = try await ConsumerDrivePager.fetch(
        vehicleID: vehicleID,
        fromMilliseconds: nil,
        toMilliseconds: nil,
        limit: 2,
        maximumPages: 2
      ) { _ in
        try await pageProvider.next()
      }
      XCTFail("Expected the page bound to be enforced")
    } catch let error as ConsumerError {
      XCTAssertEqual(error, .paginationLimitExceeded)
    }
  }

  func testDrivePagerReportsPageAndConditionalCounts() async throws {
    let vehicleID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    let firstCursor = try CurrentHubDriveCursor(rawValue: "cursor-a")
    let pages = [
      ConsumerDrivePage(
        itemCount: 2,
        nextCursor: firstCursor,
        conditionalNotModified: true
      ),
      ConsumerDrivePage(
        itemCount: 1,
        nextCursor: nil,
        conditionalNotModified: true
      ),
    ]
    let provider = PageProvider(pages: pages)

    let result = try await ConsumerDrivePager.fetch(
      vehicleID: vehicleID,
      fromMilliseconds: nil,
      toMilliseconds: nil,
      limit: 2,
      maximumPages: 3
    ) { _ in
      try await provider.next()
    }

    XCTAssertEqual(result.pageCount, 2)
    XCTAssertEqual(result.driveCount, 3)
    XCTAssertEqual(result.pageItemCounts, [2, 1])
    XCTAssertEqual(result.conditionalNotModifiedCount, 2)
  }

  func testDrivePagerRetainsPageItemsInServerOrder() async throws {
    let vehicleID = "11111111-1111-4111-8111-111111111111"
    let decoded = try [
      semanticDriveJSON(id: 9, vehicleID: vehicleID, start: 1_788_566_200_000),
      semanticDriveJSON(id: 8, vehicleID: vehicleID, start: 1_788_566_200_000),
      semanticDriveJSON(
        id: 7, vehicleID: vehicleID, start: 1_788_565_900_000,
        distance: "null", duration: "null"
      ),
    ].map { try JSONDecoder().decode(CurrentHubDrive.self, from: Data($0.utf8)) }
    let cursor = try CurrentHubDriveCursor(rawValue: "next")
    let provider = PageProvider(pages: [
      ConsumerDrivePage(items: Array(decoded.prefix(2)), nextCursor: cursor),
      ConsumerDrivePage(items: [decoded[2]], nextCursor: nil),
    ])

    let result = try await ConsumerDrivePager.fetch(
      vehicleID: try XCTUnwrap(UUID(uuidString: vehicleID)),
      fromMilliseconds: 1_788_565_900_000,
      toMilliseconds: 1_788_566_200_001,
      limit: 2,
      maximumPages: 3
    ) { _ in
      try await provider.next()
    }

    XCTAssertEqual(result.items.map(\.id), [9, 8, 7])
    XCTAssertEqual(result.items.prefix(2).map(\.startDateMilliseconds), [
      1_788_566_200_000, 1_788_566_200_000,
    ])
    XCTAssertNil(result.items.last?.distanceKilometres)
    XCTAssertNil(result.items.last?.durationMinutes)
  }

  func testDerivedLatestDriveBoundaryChecksUseArbitraryWindow() throws {
    let vehicleID = "11111111-1111-4111-8111-111111111111"
    let drives = try [
      semanticDriveJSON(id: 3, vehicleID: vehicleID, start: 8_000),
      semanticDriveJSON(id: 2, vehicleID: vehicleID, start: 5_000),
      semanticDriveJSON(id: 1, vehicleID: vehicleID, start: 2_000),
    ].map { try JSONDecoder().decode(CurrentHubDrive.self, from: Data($0.utf8)) }

    let expectations = try ConsumerHistoryBoundaryPlanner.expectations(
      fromMilliseconds: 1_000,
      toMilliseconds: 9_001,
      primaryItems: drives,
      deriveLatestDriveBoundaryChecks: true
    )

    XCTAssertEqual(expectations, [
      ConsumerHistoryBoundaryExpectation(
        window: "before-equal-start", fromMilliseconds: 1_000,
        toMilliseconds: 8_000, expectedCount: 2
      ),
      ConsumerHistoryBoundaryExpectation(
        window: "equal-start-only", fromMilliseconds: 8_000,
        toMilliseconds: 8_001, expectedCount: 1
      ),
    ])
  }

  func testDerivedLatestDriveBoundaryChecksCountTiedLatestStarts() throws {
    let vehicleID = "11111111-1111-4111-8111-111111111111"
    let drives = try [
      semanticDriveJSON(id: 4, vehicleID: vehicleID, start: 8_000),
      semanticDriveJSON(id: 3, vehicleID: vehicleID, start: 8_000),
      semanticDriveJSON(id: 2, vehicleID: vehicleID, start: 5_000),
      semanticDriveJSON(id: 1, vehicleID: vehicleID, start: 2_000),
    ].map { try JSONDecoder().decode(CurrentHubDrive.self, from: Data($0.utf8)) }

    let expectations = try ConsumerHistoryBoundaryPlanner.expectations(
      fromMilliseconds: 1_000,
      toMilliseconds: 9_001,
      primaryItems: drives,
      deriveLatestDriveBoundaryChecks: true
    )

    XCTAssertEqual(expectations.map(\.expectedCount), [2, 2])
  }

  func testDefaultBoundaryChecksPreserveCanonicalSyntheticWindows() throws {
    let expectations = try ConsumerHistoryBoundaryPlanner.expectations(
      fromMilliseconds: 10,
      toMilliseconds: 20,
      primaryItems: [],
      deriveLatestDriveBoundaryChecks: false
    )

    XCTAssertEqual(expectations.map(\.expectedCount), [3, 2])
    XCTAssertEqual(expectations.map(\.fromMilliseconds), [
      1_788_565_900_000, 1_788_566_200_000,
    ])
    XCTAssertEqual(expectations.map(\.toMilliseconds), [
      1_788_566_200_000, 1_788_566_200_001,
    ])
  }

  func testZeroDriveLiveJourneySkipsHistoryBoundaryChecks() throws {
    let skipped = try ConsumerHistoryBoundaryPlanner.liveJourneyExpectations(
      fromMilliseconds: 1_000,
      toMilliseconds: 9_001,
      primaryItems: [],
      expectedDriveCounts: [0],
      deriveLatestDriveBoundaryChecks: false
    )
    XCTAssertEqual(skipped, [])

    let vehicleID = "11111111-1111-4111-8111-111111111111"
    let drives = try [
      semanticDriveJSON(id: 2, vehicleID: vehicleID, start: 8_000),
      semanticDriveJSON(id: 1, vehicleID: vehicleID, start: 5_000),
    ].map { try JSONDecoder().decode(CurrentHubDrive.self, from: Data($0.utf8)) }
    let derived = try ConsumerHistoryBoundaryPlanner.liveJourneyExpectations(
      fromMilliseconds: 1_000,
      toMilliseconds: 9_001,
      primaryItems: drives,
      expectedDriveCounts: [2],
      deriveLatestDriveBoundaryChecks: true
    )
    XCTAssertEqual(derived.map(\.expectedCount), [1, 1])
  }

  func testClearingCredentialStoreRejectsLateSave() async throws {
    let store = InMemoryCredentialStore()
    await store.clear()

    do {
      try await store.saveCredential(try credential(token: String(repeating: "a", count: 64)))
      XCTFail("Expected a closed store to reject a late save")
    } catch let error as ConsumerError {
      XCTAssertEqual(error, .sessionClosed)
    }
    let storedCredential = try await store.loadCredential()
    XCTAssertNil(storedCredential)
  }

  func testSafeErrorStatusOmitsSensitiveDetails() {
    let status = ConsumerStatus.forError(
      CurrentHubError.api(
        statusCode: 503,
        code: "service_unavailable",
        message: "secret bearer and invitation details",
        requestID: "private-request-id"
      )
    )
    XCTAssertEqual(status, "api_error_503")
    XCTAssertFalse(status.contains("secret"))
    XCTAssertFalse(status.contains("private-request-id"))
  }

  func testCancellationClearsAndClosesTheSession() async throws {
    let transport = RecordingConsumerTransport(
      responses: [Response(body: discoveryJSON())],
      delayNanoseconds: 0
    )
    let session = try await CurrentHubConsumerSession.connect(
      endpoint: TestData.endpoint,
      expectedHubID: TestData.hubID,
      transport: transport
    )
    let operation = Task {
      try await ConsumerLifecycle.execute(session: session) {
        try await Task.sleep(nanoseconds: 1_000_000_000)
        return true
      }
    }
    try await Task.sleep(nanoseconds: 20_000_000)
    operation.cancel()

    let result = await operation.result
    guard case .failure = result else {
      XCTFail("Expected cancellation")
      return
    }
    do {
      _ = try await session.health()
      XCTFail("Expected the cleaned-up session to reject new work")
    } catch let error as ConsumerError {
      XCTAssertEqual(error, .sessionClosed)
    }
  }

  func testCancelledQueuedOperationDoesNotReachTheTransport() async throws {
    let transport = RecordingConsumerTransport(
      responses: [
        Response(body: discoveryJSON()),
        Response(body: healthJSON()),
        Response(body: healthJSON()),
      ],
      delayNanoseconds: 100_000_000
    )
    let session = try await CurrentHubConsumerSession.connect(
      endpoint: TestData.endpoint,
      expectedHubID: TestData.hubID,
      transport: transport
    )
    let first = Task { try await session.health() }
    for _ in 0..<100 where await transport.activeRequestCount() == 0 {
      try await Task.sleep(nanoseconds: 1_000_000)
    }
    let second = Task { try await session.health() }
    second.cancel()

    _ = try await first.value
    let secondResult = await second.result
    guard case .failure = secondResult else {
      XCTFail("Expected the queued operation to be cancelled")
      return
    }
    let paths = await transport.paths()
    XCTAssertEqual(paths.count, 2)
    await session.closeAndClear()
  }

  func testSessionSerializesClaimAndRotation() async throws {
    let transport = RecordingConsumerTransport(
      responses: [
        Response(body: discoveryJSON()),
        Response(body: claimJSON(token: "a")),
        Response(body: claimJSON(token: "b")),
      ],
      delayNanoseconds: 20_000_000
    )
    let session = try await CurrentHubConsumerSession.connect(
      endpoint: TestData.endpoint,
      expectedHubID: TestData.hubID,
      transport: transport
    )
    let invitation = try invitation()

    let claim = Task {
      try await session.claim(invitation: invitation, deviceName: "consumer-test")
    }
    var claimRequestObserved = false
    for _ in 0..<100 {
      let paths = await transport.paths()
      if paths.contains("/v1/pairings/11111111-1111-4111-8111-111111111111/claim") {
        claimRequestObserved = true
        break
      }
      try await Task.sleep(nanoseconds: 1_000_000)
    }
    XCTAssertTrue(claimRequestObserved)
    let rotation = Task {
      try await session.rotateCredential()
    }
    _ = try await claim.value
    _ = try await rotation.value

    let maximumConcurrentRequests = await transport.maximumConcurrentRequests()
    let paths = await transport.paths()
    XCTAssertEqual(maximumConcurrentRequests, 1)
    XCTAssertEqual(
      paths,
      [
        "/.well-known/teslatlas-hub",
        "/v1/pairings/11111111-1111-4111-8111-111111111111/claim",
        "/v1/device/rotate",
      ]
    )
    await session.closeAndClear()
  }
}

private actor QueryRecorder {
  private var queries: [CurrentHubDriveQuery] = []

  func append(_ query: CurrentHubDriveQuery) -> Int {
    queries.append(query)
    return queries.count - 1
  }
  func count() -> Int { queries.count }
  func values() -> [CurrentHubDriveQuery] { queries }
}

private actor PageProvider {
  private let pages: [ConsumerDrivePage]
  private var index = 0

  init(pages: [ConsumerDrivePage]) { self.pages = pages }

  func next() throws -> ConsumerDrivePage {
    defer { index += 1 }
    guard index < pages.count else { throw ConsumerTestError.noResponse }
    return pages[index]
  }
}

private enum TestData {
  static let endpoint = URL(string: "https://hub.example.invalid")!
  static let hubID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
}

private struct Response: Sendable {
  let statusCode: Int = 200
  let headers: [String: String] = [
    "Content-Type": "application/json",
    "Cache-Control": "no-store",
    "ETag": "\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"",
  ]
  let body: Data
}

private actor RecordingConsumerTransport: CurrentHubInvitationPinningTransport {
  private var responses: [Response]
  private let delayNanoseconds: UInt64
  private var requestPaths: [String] = []
  private var activeRequests = 0
  private var maximumActiveRequests = 0

  init(responses: [Response], delayNanoseconds: UInt64) {
    self.responses = responses
    self.delayNanoseconds = delayNanoseconds
  }

  func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse {
    try await sendResponse(for: request)
  }

  func send(
    _ request: URLRequest,
    validatingLeafCertificateSHA256 _: String
  ) async throws -> CurrentHubHTTPResponse {
    try await sendResponse(for: request)
  }

  func paths() -> [String] { requestPaths }
  func maximumConcurrentRequests() -> Int { maximumActiveRequests }
  func activeRequestCount() -> Int { activeRequests }

  private func sendResponse(for request: URLRequest) async throws -> CurrentHubHTTPResponse {
    guard let url = request.url, !responses.isEmpty else {
      throw ConsumerTestError.noResponse
    }
    requestPaths.append(url.path)
    activeRequests += 1
    maximumActiveRequests = max(maximumActiveRequests, activeRequests)
    defer { activeRequests -= 1 }
    try await Task.sleep(nanoseconds: delayNanoseconds)
    let response = responses.removeFirst()
    return CurrentHubHTTPResponse(
      statusCode: response.statusCode,
      headers: response.headers,
      body: response.body,
      finalURL: url
    )
  }
}

private enum ConsumerTestError: Error { case noResponse }

private func credential(token: String) throws -> CurrentHubCredential {
  try CurrentHubCredential(
    deviceID: TestData.hubID,
    accessToken: token,
    expiresAtMilliseconds: 4_102_444_800_000
  )
}

private func discoveryJSON() -> Data {
  Data("""
    {"api_versions":["1.0"],"capabilities":["query.vehicles","query.current","query.drives","sync.packs"],"hub_id":"11111111-1111-4111-8111-111111111111","pack_format":"sqlite-zstd","protocol":"teslatlas-sync","protocol_major":1,"sourceUrl":"https://example.invalid/source","version":"2026.36.2"}
    """.utf8)
}

private func healthJSON() -> Data {
  Data("""
    {"status":"ok","version":"2026.36.2"}
    """.utf8)
}

private func claimJSON(token: String) -> Data {
  Data("""
    {"access_token":"\(String(repeating: token, count: 64))","device_id":"11111111-1111-4111-8111-111111111111","expires_at_ms":4102444800000}
    """.utf8)
}

private func invitation() throws -> CurrentHubInvitation {
  let secret = String(repeating: "0", count: 64)
  let pin = String(repeating: "1", count: 64)
  let data = Data("""
    {"endpoint":"https://hub.example.invalid","expiresAtMs":4102444800000,"pairingId":"11111111-1111-4111-8111-111111111111","pairingUri":"teslatlas-hub://pair?endpoint=https%3A%2F%2Fhub.example.invalid&pairing_id=11111111-1111-4111-8111-111111111111&secret=\(secret)&tls_pin=\(pin)","secret":"\(secret)","tlsPin":"\(pin)"}
    """.utf8)
  return try JSONDecoder().decode(CurrentHubInvitation.self, from: data)
}

private func semanticCurrentJSON(
  vehicleID: String,
  observedAt: String = "null",
  state: String = "null",
  batteryLevel: String = "null",
  estimatedRange: String = "null",
  odometer: String = "null",
  speed: String = "null",
  insideTemperature: String = "null",
  outsideTemperature: String = "null",
  chargerPower: String = "null",
  locked: String = "null"
) -> String {
  let encodedState = state == "null" ? "null" : "\"\(state)\""
  return """
    {"vehicle_id":"\(vehicleID)","observed_at_ms":\(observedAt),"state":\(encodedState),"battery_level":\(batteryLevel),"est_battery_range_km":\(estimatedRange),"odometer":\(odometer),"speed":\(speed),"inside_temp":\(insideTemperature),"outside_temp":\(outsideTemperature),"charger_power":\(chargerPower),"locked":\(locked)}
    """
}

private func semanticDriveJSON(
  id: Int64,
  vehicleID: String,
  start: Int64,
  distance: String = "4.2",
  duration: String = "1"
) -> String {
  """
  {"id":\(id),"vehicle_id":"\(vehicleID)","start_date_ms":\(start),"end_date_ms":\(start + 60_000),"distance_km":\(distance),"duration_min":\(duration)}
  """
}

private func containsPrivateSemanticMaterial(_ value: Any, vehicleIDs: [String]) -> Bool {
  if let object = value as? [String: Any] {
    let forbiddenKeyFragments = [
      "vehicle_id", "display_name", "address", "latitude", "longitude", "geofence",
      "destination", "token", "cursor", "endpoint",
    ]
    if object.keys.contains(where: { key in
      forbiddenKeyFragments.contains(where: { key.lowercased().contains($0) })
    }) { return true }
    return object.values.contains { containsPrivateSemanticMaterial($0, vehicleIDs: vehicleIDs) }
  }
  if let array = value as? [Any] {
    return array.contains { containsPrivateSemanticMaterial($0, vehicleIDs: vehicleIDs) }
  }
  if let string = value as? String {
    return UUID(uuidString: string) != nil
      || vehicleIDs.contains(string)
      || string.contains("display-name-secret")
      || string.contains("address-secret")
      || string.contains("location-secret")
      || string.contains("token-secret")
      || string.contains("cursor-secret")
      || string.contains("hub.example")
  }
  return false
}

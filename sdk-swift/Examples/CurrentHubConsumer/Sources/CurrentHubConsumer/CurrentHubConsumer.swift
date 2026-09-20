import Foundation
import TeslatlasCurrentHub

#if canImport(CryptoKit)
import CryptoKit
#endif

#if canImport(Security)
import Security
#endif
#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

@main
struct CurrentHubConsumer {
  private static let semanticFromMilliseconds: Int64 = 1_788_565_900_000
  private static let semanticToMilliseconds: Int64 = 1_788_566_200_001

  static func main() async {
    do {
      try await run()
    } catch {
      let message = "status=\(ConsumerStatus.forError(error))\n"
      FileHandle.standardError.write(Data(message.utf8))
      exit(1)
    }
  }

  private static func run() async throws {
    guard CommandLine.arguments.count == 2 else { throw ConsumerError.usage }
    let configPath = CommandLine.arguments[1]
    let configData = try OwnerOnlyFileReader.read(
      path: configPath,
      maximumBytes: ConsumerConfig.maximumEncodedBytes
    )
    let config = try ConsumerConfig.decode(from: configData)
    let invitationData = try OwnerOnlyFileReader.read(
      path: config.invitationPath,
      maximumBytes: 64 * 1024
    )
    let invitation = try JSONDecoder().decode(CurrentHubInvitation.self, from: invitationData)
    let transport = try makeTransport(caPath: config.caPath)
    let session = try await CurrentHubConsumerSession.connect(
      endpoint: config.endpoint,
      expectedHubID: config.expectedHubID,
      transport: transport
    )

    let report = try await ConsumerLifecycle.execute(session: session) {
      let discovery = await session.discoveryDocument()
      guard discovery.hubID == config.expectedHubID,
        discovery.protocolName == "teslatlas-sync",
        discovery.protocolMajor == 1,
        discovery.apiVersions == ["1.0"],
        discovery.version == "2026.36.2",
        Set(discovery.capabilities) == [
          "query.vehicles", "query.current", "query.drives", "sync.packs",
        ]
      else { throw ConsumerError.unexpectedDiscovery }
      let originalCredential = try await session.claim(
        invitation: invitation,
        deviceName: config.deviceName
      )
      try OwnerOnlyExclusiveFileWriter.write(
        Data(originalCredential.deviceID.uuidString.lowercased().utf8),
        path: config.cleanupDeviceIDPath
      )
      do {
        _ = try await session.claim(invitation: invitation, deviceName: "replay-check")
        throw ConsumerError.invitationReplayAccepted
      } catch CurrentHubError.unauthorized {
        // The invitation is one-use and its deliberate replay was rejected.
      }
      let health = try await session.health()
      let readiness = try await session.readiness()
      guard health.status == "ok", health.version == "2026.36.2",
        readiness.status == "ready", readiness.reason == nil
      else { throw ConsumerError.unexpectedFixture }
      let vehicles = try await session.vehicles()
      guard vehicles.count == config.expectedVehicleCount else {
        throw ConsumerError.unexpectedFixture
      }
      let vehicleID = config.vehicleID ?? vehicles[0].vehicleID
      guard let selectedVehicle = vehicles.first(where: { $0.vehicleID == vehicleID }) else {
        throw ConsumerError.noVehicles
      }
      let orderedVehicles = [selectedVehicle] + vehicles.filter { $0.vehicleID != vehicleID }
      var observedCurrentCount = 0
      var absentCurrentCount = 0
      var currentStates: [CurrentHubCurrentState] = []
      for vehicle in orderedVehicles {
        let current = try await session.current(vehicleID: vehicle.vehicleID)
        currentStates.append(current)
        if current.observedAtMilliseconds == nil {
          absentCurrentCount += 1
        } else {
          observedCurrentCount += 1
        }
      }
      guard observedCurrentCount == config.expectedObservedCount,
        absentCurrentCount == config.expectedAbsentCount
      else {
        throw ConsumerError.unexpectedFixture
      }
      let driveFromMilliseconds = config.driveFromMilliseconds ?? semanticFromMilliseconds
      let driveToMilliseconds = config.driveToMilliseconds ?? semanticToMilliseconds
      let expectedDriveCounts = config.expectedDriveCounts ?? [5, 0, 0]
      var histories: [ConsumerDrivePageSummary] = []
      for vehicle in orderedVehicles {
        let history = try await ConsumerDrivePager.fetch(
          vehicleID: vehicle.vehicleID,
          fromMilliseconds: driveFromMilliseconds,
          toMilliseconds: driveToMilliseconds,
          limit: 2,
          maximumPages: 128
        ) { query in
          try await session.drivePage(query: query, vehicleID: vehicle.vehicleID)
        }
        histories.append(history)
      }
      guard histories.map(\.driveCount) == expectedDriveCounts,
        histories.allSatisfy({ $0.conditionalNotModifiedCount == $0.pageCount })
      else {
        throw ConsumerError.unexpectedFixture
      }
      let boundaryInputs = try ConsumerHistoryBoundaryPlanner.liveJourneyExpectations(
        fromMilliseconds: driveFromMilliseconds,
        toMilliseconds: driveToMilliseconds,
        primaryItems: histories[0].items,
        expectedDriveCounts: expectedDriveCounts,
        deriveLatestDriveBoundaryChecks: config.deriveLatestDriveBoundaryChecks
      )
      var boundaryChecks: [ConsumerHistoryBoundaryCheck] = []
      for input in boundaryInputs {
        let result = try await ConsumerDrivePager.fetch(
          vehicleID: vehicleID,
          fromMilliseconds: input.fromMilliseconds,
          toMilliseconds: input.toMilliseconds,
          limit: 2,
          maximumPages: 128
        ) { query in
          try await session.drivePage(query: query, vehicleID: vehicleID)
        }
        guard result.driveCount == input.expectedCount else {
          throw ConsumerError.unexpectedFixture
        }
        boundaryChecks.append(
          ConsumerHistoryBoundaryCheck(
            window: input.window,
            fromMilliseconds: input.fromMilliseconds,
            toMilliseconds: input.toMilliseconds,
            count: result.driveCount
          )
        )
      }
      let oldClient = try await session.client(using: originalCredential)
      _ = try await session.rotateCredential()
      do {
        _ = try await oldClient.vehicles()
        throw ConsumerError.oldCredentialAccepted
      } catch CurrentHubError.unauthorized {
        // Rotation invalidated the old bearer.
      }
      guard try await session.vehicles().count == config.expectedVehicleCount else {
        throw ConsumerError.unexpectedFixture
      }
      let preRestartSnapshotData = try ConsumerSemanticSnapshot.encode(
        vehicles: zip(currentStates, histories).map {
          ConsumerSemanticVehicle(current: $0.0, drives: $0.1.items)
        },
        fromMilliseconds: driveFromMilliseconds,
        toMilliseconds: driveToMilliseconds,
        boundaryChecks: boundaryChecks
      )
      var snapshotData = preRestartSnapshotData
      var restartVerified = false
      if let readyPath = config.restartReadyPath,
        let continuePath = config.restartContinuePath
      {
        try await ConsumerRestartGate.wait(
          readyPath: readyPath,
          continuePath: continuePath
        )
        let restartedDiscovery = try await session.refreshDiscovery()
        let restartedHealth = try await session.health()
        let restartedReadiness = try await session.readiness()
        let restartedVehicles = try await session.vehicles()
        guard restartedDiscovery.hubID == config.expectedHubID,
          restartedDiscovery.version == "2026.36.2",
          restartedHealth.status == "ok",
          restartedReadiness.status == "ready",
          restartedVehicles.count == config.expectedVehicleCount,
          Set(restartedVehicles.map(\.vehicleID)) == Set(orderedVehicles.map(\.vehicleID))
        else { throw ConsumerError.unexpectedFixture }

        var restartedCurrentStates: [CurrentHubCurrentState] = []
        var restartedHistories: [ConsumerDrivePageSummary] = []
        for vehicle in orderedVehicles {
          restartedCurrentStates.append(
            try await session.current(vehicleID: vehicle.vehicleID)
          )
          restartedHistories.append(
            try await ConsumerDrivePager.fetch(
              vehicleID: vehicle.vehicleID,
              fromMilliseconds: driveFromMilliseconds,
              toMilliseconds: driveToMilliseconds,
              limit: 2,
              maximumPages: 128
            ) { query in
              try await session.drivePage(query: query, vehicleID: vehicle.vehicleID)
            }
          )
        }
        guard restartedHistories.map(\.driveCount) == expectedDriveCounts,
          restartedHistories.allSatisfy({
            $0.conditionalNotModifiedCount == $0.pageCount
          })
        else { throw ConsumerError.restartSemanticMismatch }

        var restartedBoundaryChecks: [ConsumerHistoryBoundaryCheck] = []
        for input in boundaryInputs {
          let result = try await ConsumerDrivePager.fetch(
            vehicleID: vehicleID,
            fromMilliseconds: input.fromMilliseconds,
            toMilliseconds: input.toMilliseconds,
            limit: 2,
            maximumPages: 128
          ) { query in
            try await session.drivePage(query: query, vehicleID: vehicleID)
          }
          guard result.driveCount == input.expectedCount else {
            throw ConsumerError.restartSemanticMismatch
          }
          restartedBoundaryChecks.append(
            ConsumerHistoryBoundaryCheck(
              window: input.window,
              fromMilliseconds: input.fromMilliseconds,
              toMilliseconds: input.toMilliseconds,
              count: result.driveCount
            )
          )
        }
        let postRestartSnapshotData = try ConsumerSemanticSnapshot.encode(
          vehicles: zip(restartedCurrentStates, restartedHistories).map {
            ConsumerSemanticVehicle(current: $0.0, drives: $0.1.items)
          },
          fromMilliseconds: driveFromMilliseconds,
          toMilliseconds: driveToMilliseconds,
          boundaryChecks: restartedBoundaryChecks
        )
        try ConsumerSemanticSnapshot.requireUnchanged(
          before: preRestartSnapshotData,
          after: postRestartSnapshotData
        )
        snapshotData = postRestartSnapshotData
        restartVerified = true
      }
      try OwnerOnlyExclusiveFileWriter.write(snapshotData, path: config.semanticSnapshotPath)
      return ConsumerJourneyReport(
        vehicleCount: vehicles.count,
        currentObservedCount: observedCurrentCount,
        currentAbsentCount: absentCurrentCount,
        drivePageCount: histories[0].pageCount,
        driveCount: histories[0].driveCount,
        conditionalNotModifiedCount: histories[0].conditionalNotModifiedCount,
        restartVerified: restartVerified,
        semanticSnapshotSHA256: semanticSHA256(snapshotData)
      )
    }

    print("discovery=status=ok")
    print("claim=status=ok")
    print("health=status=ok")
    print("readiness=status=ready")
    print("vehicles=count=\(report.vehicleCount)")
    print("current=observed=\(report.currentObservedCount) absent=\(report.currentAbsentCount)")
    print("drives=pages=\(report.drivePageCount) count=\(report.driveCount) not_modified=\(report.conditionalNotModifiedCount)")
    print("rotation=status=ok old_credential=rejected post_rotation=ok")
    print("replay=status=rejected")
    print("restart=status=\(report.restartVerified ? "ok" : "not_requested")")
    print(
      "semantic_snapshot=sha256=\(report.semanticSnapshotSHA256 ?? "unavailable") source=\(report.restartVerified ? "fresh_post_restart" : "initial_read")"
    )
  }

  private static func makeTransport(caPath: String?) throws -> CurrentHubURLSessionTransport {
    #if canImport(Security)
    guard let caPath else { return CurrentHubURLSessionTransport() }
    let caData = try OwnerOnlyFileReader.read(path: caPath, maximumBytes: 512 * 1024)
    return try CurrentHubURLSessionTransport(trustedCertificateAuthoritiesDER: [caData])
    #else
    guard caPath == nil else { throw ConsumerError.unsupportedLinuxCertificateAnchors }
    return CurrentHubURLSessionTransport()
    #endif
  }

  private static func semanticSHA256(_ data: Data) -> String? {
    #if canImport(CryptoKit)
    SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    #else
    nil
    #endif
  }
}

private struct ConsumerJourneyReport: Sendable {
  let vehicleCount: Int
  let currentObservedCount: Int
  let currentAbsentCount: Int
  let drivePageCount: Int
  let driveCount: Int
  let conditionalNotModifiedCount: Int
  let restartVerified: Bool
  let semanticSnapshotSHA256: String?
}

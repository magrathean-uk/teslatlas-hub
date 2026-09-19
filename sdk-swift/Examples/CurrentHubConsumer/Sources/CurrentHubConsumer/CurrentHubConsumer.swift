import Foundation
import TeslatlasCurrentHub

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
      guard vehicles.count == 2 else { throw ConsumerError.unexpectedFixture }
      let vehicleID = config.vehicleID ?? vehicles[0].vehicleID
      guard vehicles.contains(where: { $0.vehicleID == vehicleID }) else {
        throw ConsumerError.noVehicles
      }
      var observedCurrentCount = 0
      var absentCurrentCount = 0
      for vehicle in vehicles {
        let current = try await session.current(vehicleID: vehicle.vehicleID)
        if current.observedAtMilliseconds == nil {
          absentCurrentCount += 1
        } else {
          observedCurrentCount += 1
        }
      }
      guard observedCurrentCount == 1, absentCurrentCount == 1 else {
        throw ConsumerError.unexpectedFixture
      }
      let drives = try await ConsumerDrivePager.fetch(
        vehicleID: vehicleID,
        fromMilliseconds: config.driveFromMilliseconds,
        toMilliseconds: config.driveToMilliseconds,
        limit: 2,
        maximumPages: 3
      ) { query in
        try await session.drivePage(query: query, vehicleID: vehicleID)
      }
      guard drives.pageItemCounts == [2, 2, 1], drives.conditionalNotModifiedCount == 3 else {
        throw ConsumerError.unexpectedFixture
      }
      let oldClient = try await session.client(using: originalCredential)
      _ = try await session.rotateCredential()
      do {
        _ = try await oldClient.vehicles()
        throw ConsumerError.oldCredentialAccepted
      } catch CurrentHubError.unauthorized {
        // Rotation invalidated the old bearer.
      }
      guard try await session.vehicles().count == 2 else {
        throw ConsumerError.unexpectedFixture
      }
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
        guard restartedDiscovery.hubID == config.expectedHubID,
          restartedDiscovery.version == "2026.36.2",
          restartedHealth.status == "ok",
          restartedReadiness.status == "ready",
          try await session.vehicles().count == 2
        else { throw ConsumerError.unexpectedFixture }
        restartVerified = true
      }
      return ConsumerJourneyReport(
        vehicleCount: vehicles.count,
        currentObservedCount: observedCurrentCount,
        currentAbsentCount: absentCurrentCount,
        drivePageCount: drives.pageCount,
        driveCount: drives.driveCount,
        conditionalNotModifiedCount: drives.conditionalNotModifiedCount,
        restartVerified: restartVerified
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
}

private struct ConsumerJourneyReport: Sendable {
  let vehicleCount: Int
  let currentObservedCount: Int
  let currentAbsentCount: Int
  let drivePageCount: Int
  let driveCount: Int
  let conditionalNotModifiedCount: Int
  let restartVerified: Bool
}

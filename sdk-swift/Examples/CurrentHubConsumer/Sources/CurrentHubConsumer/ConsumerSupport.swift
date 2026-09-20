import Foundation
import TeslatlasCurrentHub

#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

enum ConsumerError: Error, Equatable, Sendable {
  case usage
  case invalidConfiguration
  case unsupportedLinuxCertificateAnchors
  case noVehicles
  case sessionClosed
  case invalidDriveQuery
  case invalidDrivePage
  case paginationCursorCycle
  case paginationLimitExceeded
  case paginationUnexpectedNotModified
  case unexpectedDiscovery
  case unexpectedFixture
  case invitationReplayAccepted
  case oldCredentialAccepted
  case invalidRestartGate
  case restartTimeout
  case restartSemanticMismatch
  case privateOutputFailure
}

struct ConsumerConfig: Decodable, Sendable {
  static let maximumEncodedBytes = 64 * 1024

  let endpoint: URL
  let expectedHubID: UUID
  let invitationPath: String
  let caPath: String?
  let deviceName: String
  let expectedVehicleCount: Int
  let expectedObservedCount: Int
  let expectedAbsentCount: Int
  let expectedDriveCounts: [Int]?
  let deriveLatestDriveBoundaryChecks: Bool
  let semanticSnapshotPath: String
  let cleanupDeviceIDPath: String
  let vehicleID: UUID?
  let driveFromMilliseconds: Int64?
  let driveToMilliseconds: Int64?
  let restartReadyPath: String?
  let restartContinuePath: String?

  enum CodingKeys: String, CodingKey, CaseIterable {
    case endpoint
    case expectedHubID
    case invitationPath
    case caPath
    case deviceName
    case expectedVehicleCount, expectedObservedCount, expectedAbsentCount
    case expectedDriveCounts, deriveLatestDriveBoundaryChecks
    case semanticSnapshotPath, cleanupDeviceIDPath
    case vehicleID
    case driveFromMilliseconds = "driveFromMs"
    case driveToMilliseconds = "driveToMs"
    case restartReadyPath, restartContinuePath
  }

  static func decode(from data: Data) throws -> ConsumerConfig {
    guard data.count <= maximumEncodedBytes else {
      throw ConsumerError.invalidConfiguration
    }
    return try JSONDecoder().decode(ConsumerConfig.self, from: data)
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    endpoint = try container.decode(URL.self, forKey: .endpoint)
    expectedHubID = try container.decode(UUID.self, forKey: .expectedHubID)
    invitationPath = try container.decode(String.self, forKey: .invitationPath)
    caPath = try container.decodeIfPresent(String.self, forKey: .caPath)
    deviceName = try container.decode(String.self, forKey: .deviceName)
    expectedVehicleCount = try container.decode(Int.self, forKey: .expectedVehicleCount)
    expectedObservedCount = try container.decode(Int.self, forKey: .expectedObservedCount)
    expectedAbsentCount = try container.decode(Int.self, forKey: .expectedAbsentCount)
    expectedDriveCounts = try container.decodeIfPresent([Int].self, forKey: .expectedDriveCounts)
    deriveLatestDriveBoundaryChecks = try container.decodeIfPresent(
      Bool.self,
      forKey: .deriveLatestDriveBoundaryChecks
    ) ?? false
    semanticSnapshotPath = try container.decode(String.self, forKey: .semanticSnapshotPath)
    cleanupDeviceIDPath = try container.decode(String.self, forKey: .cleanupDeviceIDPath)
    vehicleID = try container.decodeIfPresent(UUID.self, forKey: .vehicleID)
    driveFromMilliseconds = try container.decodeIfPresent(
      Int64.self,
      forKey: .driveFromMilliseconds
    )
    driveToMilliseconds = try container.decodeIfPresent(
      Int64.self,
      forKey: .driveToMilliseconds
    )
    restartReadyPath = try container.decodeIfPresent(String.self, forKey: .restartReadyPath)
    restartContinuePath = try container.decodeIfPresent(String.self, forKey: .restartContinuePath)
    let (expectedCurrentCount, countOverflow) = expectedObservedCount.addingReportingOverflow(
      expectedAbsentCount
    )

    guard endpoint.scheme?.lowercased() == "https",
      endpoint.host != nil,
      endpoint.user == nil,
      endpoint.password == nil,
      endpoint.query == nil,
      endpoint.fragment == nil,
      endpoint.path.isEmpty || endpoint.path == "/",
      !isZeroUUID(expectedHubID),
      !invitationPath.isEmpty,
      invitationPath.utf8.count <= 4_096,
      caPath.map({ !$0.isEmpty && $0.utf8.count <= 4_096 }) ?? true,
      !deviceName.isEmpty,
      deviceName.utf8.count <= 65_536,
      deviceName.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }),
      expectedVehicleCount > 0,
      expectedObservedCount >= 0,
      expectedAbsentCount >= 0,
      !countOverflow,
      expectedCurrentCount == expectedVehicleCount,
      (expectedDriveCounts.map { counts in
        counts.count == expectedVehicleCount && counts.allSatisfy({ $0 >= 0 })
      } ?? (expectedVehicleCount == 3)),
      !semanticSnapshotPath.isEmpty,
      semanticSnapshotPath.utf8.count <= 4_096,
      !cleanupDeviceIDPath.isEmpty,
      cleanupDeviceIDPath.utf8.count <= 4_096,
      semanticSnapshotPath != cleanupDeviceIDPath,
      vehicleID.map({ !isZeroUUID($0) }) ?? true,
      restartReadyPath.map({ !$0.isEmpty && $0.utf8.count <= 4_096 }) ?? true,
      restartContinuePath.map({ !$0.isEmpty && $0.utf8.count <= 4_096 }) ?? true
    else {
      throw ConsumerError.invalidConfiguration
    }

    switch (driveFromMilliseconds, driveToMilliseconds) {
    case (nil, nil):
      break
    case let (.some(from), .some(to)) where from >= 0 && from < to:
      break
    default:
      throw ConsumerError.invalidConfiguration
    }
    guard (restartReadyPath == nil) == (restartContinuePath == nil) else {
      throw ConsumerError.invalidConfiguration
    }
  }
}

enum OwnerOnlyExclusiveFileWriter {
  static func write(_ data: Data, path: String) throws {
    guard !path.isEmpty, path.utf8.count <= 4_096 else {
      throw ConsumerError.privateOutputFailure
    }
    let descriptor = path.withCString { pathPointer in
      #if canImport(Darwin)
      Darwin.open(pathPointer, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0o600)
      #else
      Glibc.open(pathPointer, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0o600)
      #endif
    }
    guard descriptor >= 0 else { throw ConsumerError.privateOutputFailure }
    var succeeded = false
    defer {
      #if canImport(Darwin)
      _ = Darwin.close(descriptor)
      #else
      _ = Glibc.close(descriptor)
      #endif
      if !succeeded {
        path.withCString { pointer in
          #if canImport(Darwin)
          _ = Darwin.unlink(pointer)
          #else
          _ = Glibc.unlink(pointer)
          #endif
        }
      }
    }

    var metadata = stat()
    #if canImport(Darwin)
    let statResult = Darwin.fstat(descriptor, &metadata)
    #else
    let statResult = Glibc.fstat(descriptor, &metadata)
    #endif
    guard statResult == 0,
      (metadata.st_mode & S_IFMT) == S_IFREG,
      metadata.st_uid == currentUserID(),
      (metadata.st_mode & 0o077) == 0
    else { throw ConsumerError.privateOutputFailure }

    var written = 0
    while written < data.count {
      let count = data.withUnsafeBytes { rawBuffer -> Int in
        guard let baseAddress = rawBuffer.baseAddress else { return 0 }
        let address = baseAddress.advanced(by: written)
        #if canImport(Darwin)
        return Darwin.write(descriptor, address, data.count - written)
        #else
        return Glibc.write(descriptor, address, data.count - written)
        #endif
      }
      guard count > 0 else { throw ConsumerError.privateOutputFailure }
      written += count
    }
    succeeded = true
  }
}

enum ConsumerPrivateInputError: Error, Equatable, Sendable {
  case invalidPath
  case notRegular
  case wrongOwner
  case insecurePermissions
  case tooLarge
  case unreadable
}

enum OwnerOnlyFileReader {
  static func read(path: String, maximumBytes: Int) throws -> Data {
    guard !path.isEmpty, maximumBytes > 0 else {
      throw ConsumerPrivateInputError.invalidPath
    }

    let descriptor = path.withCString { pathPointer in
      #if canImport(Darwin)
      Darwin.open(pathPointer, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
      #else
      Glibc.open(pathPointer, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
      #endif
    }
    guard descriptor >= 0 else {
      throw ConsumerPrivateInputError.unreadable
    }
    defer {
      #if canImport(Darwin)
      _ = Darwin.close(descriptor)
      #else
      _ = Glibc.close(descriptor)
      #endif
    }

    var metadata = stat()
    let statResult: Int32
    #if canImport(Darwin)
    statResult = Darwin.fstat(descriptor, &metadata)
    #else
    statResult = Glibc.fstat(descriptor, &metadata)
    #endif
    guard statResult == 0 else {
      throw ConsumerPrivateInputError.unreadable
    }
    guard (metadata.st_mode & S_IFMT) == S_IFREG else {
      throw ConsumerPrivateInputError.notRegular
    }
    guard metadata.st_uid == currentUserID() else {
      throw ConsumerPrivateInputError.wrongOwner
    }
    guard (metadata.st_mode & 0o077) == 0 else {
      throw ConsumerPrivateInputError.insecurePermissions
    }
    guard metadata.st_size >= 0, metadata.st_size <= Int64(maximumBytes) else {
      throw ConsumerPrivateInputError.tooLarge
    }

    var data = Data()
    data.reserveCapacity(Int(metadata.st_size))
    while true {
      let remaining = maximumBytes - data.count
      var buffer = [UInt8](repeating: 0, count: max(1, min(4_096, remaining)))
      let count = buffer.withUnsafeMutableBytes { rawBuffer -> Int in
        guard let baseAddress = rawBuffer.baseAddress else { return 0 }
        #if canImport(Darwin)
        return Darwin.read(descriptor, baseAddress, rawBuffer.count)
        #else
        return Glibc.read(descriptor, baseAddress, rawBuffer.count)
        #endif
      }
      guard count >= 0 else {
        throw ConsumerPrivateInputError.unreadable
      }
      if count == 0 { break }
      guard data.count + count <= maximumBytes else {
        throw ConsumerPrivateInputError.tooLarge
      }
      data.append(contentsOf: buffer.prefix(count))
    }
    return data
  }
}

private func currentUserID() -> uid_t {
  #if canImport(Darwin)
  Darwin.getuid()
  #else
  Glibc.getuid()
  #endif
}

private func isZeroUUID(_ value: UUID) -> Bool {
  value == UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
}

actor InMemoryCredentialStore: CurrentHubCredentialStore {
  private var credential: CurrentHubCredential?
  private var acceptsSaves = true

  init(_ credential: CurrentHubCredential? = nil) {
    self.credential = credential
  }

  func loadCredential() async throws -> CurrentHubCredential? { credential }

  func saveCredential(_ credential: CurrentHubCredential) async throws {
    guard acceptsSaves else { throw ConsumerError.sessionClosed }
    self.credential = credential
  }

  func clear() {
    credential = nil
    acceptsSaves = false
  }
}

private actor ConsumerOperationGate {
  private var occupied = false
  private var waiters: [CheckedContinuation<Void, Never>] = []

  func acquire() async {
    if !occupied {
      occupied = true
      return
    }
    await withCheckedContinuation { continuation in
      waiters.append(continuation)
    }
  }

  func release() {
    if let continuation = waiters.first {
      waiters.removeFirst()
      continuation.resume()
    } else {
      occupied = false
    }
  }
}

actor CurrentHubConsumerSession {
  private let client: CurrentHubClient
  private let credentialStore: InMemoryCredentialStore
  private let endpoint: URL
  private let expectedHubID: UUID
  private let transport: any CurrentHubInvitationPinningTransport
  private let operationGate = ConsumerOperationGate()
  private var isOpen = true

  private init(
    client: CurrentHubClient,
    credentialStore: InMemoryCredentialStore,
    endpoint: URL,
    expectedHubID: UUID,
    transport: any CurrentHubInvitationPinningTransport
  ) {
    self.client = client
    self.credentialStore = credentialStore
    self.endpoint = endpoint
    self.expectedHubID = expectedHubID
    self.transport = transport
  }

  static func connect(
    endpoint: URL,
    expectedHubID: UUID,
    transport: any CurrentHubInvitationPinningTransport
  ) async throws -> CurrentHubConsumerSession {
    let credentialStore = InMemoryCredentialStore()
    let client = try await CurrentHubClient.connect(
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      credentialStore: credentialStore,
      transport: transport
    )
    return CurrentHubConsumerSession(
      client: client,
      credentialStore: credentialStore,
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      transport: transport
    )
  }

  func discoveryDocument() async -> CurrentHubDiscovery {
    await client.discoveryDocument()
  }

  func health() async throws -> CurrentHubHealth {
    let client = self.client
    return try await serialized { try await client.health() }
  }

  func readiness() async throws -> CurrentHubReadiness {
    let client = self.client
    return try await serialized { try await client.readiness() }
  }

  func claim(
    invitation: CurrentHubInvitation,
    deviceName: String
  ) async throws -> CurrentHubCredential {
    let client = self.client
    return try await serialized {
      try await client.claim(invitation: invitation, deviceName: deviceName)
    }
  }

  func rotateCredential() async throws -> CurrentHubCredential {
    let client = self.client
    return try await serialized { try await client.rotateCredential() }
  }

  func vehicles() async throws -> [CurrentHubVehicle] {
    let client = self.client
    return try await serialized { try await client.vehicles() }
  }

  func current(vehicleID: UUID) async throws -> CurrentHubCurrentState {
    let client = self.client
    return try await serialized { try await client.current(vehicleID: vehicleID) }
  }

  func client(using credential: CurrentHubCredential) async throws -> CurrentHubClient {
    try await CurrentHubClient.connect(
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      credentialStore: InMemoryCredentialStore(credential),
      transport: transport
    )
  }

  func refreshDiscovery() async throws -> CurrentHubDiscovery {
    let client = self.client
    return try await serialized { try await client.refreshDiscovery() }
  }

  func drivePage(query: CurrentHubDriveQuery, vehicleID: UUID) async throws -> ConsumerDrivePage {
    let client = self.client
    let result = try await serialized {
      try await client.drives(vehicleID: vehicleID, query: query)
    }
    switch result {
    case let .modified(page, eTag):
      let conditional = try await serialized {
        try await client.drives(vehicleID: vehicleID, query: query, ifNoneMatch: eTag)
      }
      guard conditional == .notModified(eTag: eTag) else {
        throw ConsumerError.paginationUnexpectedNotModified
      }
      return ConsumerDrivePage(
        items: page.items,
        nextCursor: page.nextCursor,
        conditionalNotModified: true
      )
    case .notModified:
      throw ConsumerError.paginationUnexpectedNotModified
    }
  }

  func closeAndClear() async {
    isOpen = false
    await credentialStore.clear()
  }

  private func serialized<T: Sendable>(
    _ operation: @escaping @Sendable () async throws -> T
  ) async throws -> T {
    await operationGate.acquire()
    if Task.isCancelled {
      await operationGate.release()
      throw CancellationError()
    }
    guard isOpen else {
      await operationGate.release()
      throw ConsumerError.sessionClosed
    }
    do {
      let result = try await operation()
      await operationGate.release()
      return result
    } catch {
      await operationGate.release()
      throw error
    }
  }
}

struct ConsumerDrivePage: Equatable, Sendable {
  let items: [CurrentHubDrive]
  let itemCount: Int
  let nextCursor: CurrentHubDriveCursor?
  let conditionalNotModified: Bool

  init(
    items: [CurrentHubDrive],
    nextCursor: CurrentHubDriveCursor?,
    conditionalNotModified: Bool = false
  ) {
    self.items = items
    itemCount = items.count
    self.nextCursor = nextCursor
    self.conditionalNotModified = conditionalNotModified
  }

  init(
    itemCount: Int,
    nextCursor: CurrentHubDriveCursor?,
    conditionalNotModified: Bool = false
  ) {
    items = []
    self.itemCount = itemCount
    self.nextCursor = nextCursor
    self.conditionalNotModified = conditionalNotModified
  }
}

struct ConsumerDrivePageSummary: Equatable, Sendable {
  let pageCount: Int
  let driveCount: Int
  let pageItemCounts: [Int]
  let conditionalNotModifiedCount: Int
  let items: [CurrentHubDrive]
}

struct ConsumerHistoryBoundaryExpectation: Equatable, Sendable {
  let window: String
  let fromMilliseconds: Int64
  let toMilliseconds: Int64
  let expectedCount: Int
}

enum ConsumerHistoryBoundaryPlanner {
  private static let syntheticFromMilliseconds: Int64 = 1_788_565_900_000
  private static let syntheticEqualStartMilliseconds: Int64 = 1_788_566_200_000
  private static let syntheticToMilliseconds: Int64 = 1_788_566_200_001

  static func liveJourneyExpectations(
    fromMilliseconds: Int64,
    toMilliseconds: Int64,
    primaryItems: [CurrentHubDrive],
    expectedDriveCounts: [Int],
    deriveLatestDriveBoundaryChecks: Bool
  ) throws -> [ConsumerHistoryBoundaryExpectation] {
    if !expectedDriveCounts.isEmpty, expectedDriveCounts.allSatisfy({ $0 == 0 }) {
      return []
    }
    return try expectations(
      fromMilliseconds: fromMilliseconds,
      toMilliseconds: toMilliseconds,
      primaryItems: primaryItems,
      deriveLatestDriveBoundaryChecks: deriveLatestDriveBoundaryChecks
    )
  }

  static func expectations(
    fromMilliseconds: Int64,
    toMilliseconds: Int64,
    primaryItems: [CurrentHubDrive],
    deriveLatestDriveBoundaryChecks: Bool
  ) throws -> [ConsumerHistoryBoundaryExpectation] {
    if !deriveLatestDriveBoundaryChecks {
      return [
        ConsumerHistoryBoundaryExpectation(
          window: "before-equal-start",
          fromMilliseconds: syntheticFromMilliseconds,
          toMilliseconds: syntheticEqualStartMilliseconds,
          expectedCount: 3
        ),
        ConsumerHistoryBoundaryExpectation(
          window: "equal-start-only",
          fromMilliseconds: syntheticEqualStartMilliseconds,
          toMilliseconds: syntheticToMilliseconds,
          expectedCount: 2
        ),
      ]
    }

    guard let latestStart = primaryItems.first?.startDateMilliseconds,
      latestStart > fromMilliseconds,
      latestStart < toMilliseconds,
      latestStart < Int64.max
    else { throw ConsumerError.unexpectedFixture }
    let beforeLatestCount = primaryItems.count(where: {
      $0.startDateMilliseconds < latestStart
    })
    let equalLatestCount = primaryItems.count(where: {
      $0.startDateMilliseconds == latestStart
    })
    guard equalLatestCount > 0 else { throw ConsumerError.unexpectedFixture }
    return [
      ConsumerHistoryBoundaryExpectation(
        window: "before-equal-start",
        fromMilliseconds: fromMilliseconds,
        toMilliseconds: latestStart,
        expectedCount: beforeLatestCount
      ),
      ConsumerHistoryBoundaryExpectation(
        window: "equal-start-only",
        fromMilliseconds: latestStart,
        toMilliseconds: latestStart + 1,
        expectedCount: equalLatestCount
      ),
    ]
  }
}

enum ConsumerDrivePager {
  static func fetch(
    vehicleID: UUID,
    fromMilliseconds: Int64?,
    toMilliseconds: Int64?,
    limit: Int,
    maximumPages: Int,
    page: @escaping @Sendable (CurrentHubDriveQuery) async throws -> ConsumerDrivePage
  ) async throws -> ConsumerDrivePageSummary {
    guard !isZeroUUID(vehicleID), (1...500).contains(limit), (1...128).contains(maximumPages) else {
      throw ConsumerError.invalidDriveQuery
    }
    switch (fromMilliseconds, toMilliseconds) {
    case (nil, nil):
      break
    case let (.some(from), .some(to)) where from >= 0 && from < to:
      break
    default:
      throw ConsumerError.invalidDriveQuery
    }

    var cursor: CurrentHubDriveCursor?
    var seenCursors = Set<CurrentHubDriveCursor>()
    var pageCount = 0
    var driveCount = 0
    var pageItemCounts: [Int] = []
    var conditionalNotModifiedCount = 0
    var items: [CurrentHubDrive] = []

    while true {
      try Task.checkCancellation()
      let query = CurrentHubDriveQuery(
        fromMilliseconds: fromMilliseconds,
        toMilliseconds: toMilliseconds,
        limit: limit,
        cursor: cursor
      )
      let result = try await page(query)
      pageCount += 1
      guard result.itemCount >= 0, result.itemCount <= limit else {
        throw ConsumerError.invalidDrivePage
      }
      driveCount += result.itemCount
      items.append(contentsOf: result.items)
      pageItemCounts.append(result.itemCount)
      if result.conditionalNotModified { conditionalNotModifiedCount += 1 }
      guard let nextCursor = result.nextCursor else {
        return ConsumerDrivePageSummary(
          pageCount: pageCount,
          driveCount: driveCount,
          pageItemCounts: pageItemCounts,
          conditionalNotModifiedCount: conditionalNotModifiedCount,
          items: items
        )
      }
      guard !seenCursors.contains(nextCursor) else {
        throw ConsumerError.paginationCursorCycle
      }
      guard pageCount < maximumPages else {
        throw ConsumerError.paginationLimitExceeded
      }
      seenCursors.insert(nextCursor)
      cursor = nextCursor
    }
  }
}

enum ConsumerRestartGate {
  private static let timeoutNanoseconds: UInt64 = 30_000_000_000
  private static let pollNanoseconds: UInt64 = 100_000_000

  static func wait(readyPath: String, continuePath: String) async throws {
    guard readyPath != continuePath else { throw ConsumerError.invalidRestartGate }
    try createReadyMarker(path: readyPath)
    let clock = ContinuousClock()
    let deadline = clock.now.advanced(by: .nanoseconds(Int64(timeoutNanoseconds)))
    while clock.now < deadline {
      if FileManager.default.fileExists(atPath: continuePath) {
        let value = try OwnerOnlyFileReader.read(path: continuePath, maximumBytes: 32)
        guard value == Data("continue\n".utf8) else {
          throw ConsumerError.invalidRestartGate
        }
        return
      }
      try await Task.sleep(nanoseconds: pollNanoseconds)
    }
    throw ConsumerError.restartTimeout
  }

  private static func createReadyMarker(path: String) throws {
    let descriptor = path.withCString { pointer in
      #if canImport(Darwin)
      Darwin.open(pointer, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0o600)
      #else
      Glibc.open(pointer, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0o600)
      #endif
    }
    guard descriptor >= 0 else { throw ConsumerError.invalidRestartGate }
    defer {
      #if canImport(Darwin)
      _ = Darwin.close(descriptor)
      #else
      _ = Glibc.close(descriptor)
      #endif
    }
    let marker = Array("ready\n".utf8)
    let count = marker.withUnsafeBytes { buffer -> Int in
      guard let address = buffer.baseAddress else { return 0 }
      #if canImport(Darwin)
      return Darwin.write(descriptor, address, buffer.count)
      #else
      return Glibc.write(descriptor, address, buffer.count)
      #endif
    }
    guard count == marker.count else { throw ConsumerError.invalidRestartGate }
  }
}

enum ConsumerLifecycle {
  static func execute<T: Sendable>(
    session: CurrentHubConsumerSession,
    operation: @escaping @Sendable () async throws -> T
  ) async throws -> T {
    let task = Task { try await operation() }
    return try await withTaskCancellationHandler {
      do {
        let value = try await task.value
        await session.closeAndClear()
        return value
      } catch {
        task.cancel()
        _ = await task.result
        await session.closeAndClear()
        throw error
      }
    } onCancel: {
      task.cancel()
    }
  }
}

enum ConsumerStatus {
  static func forError(_ error: Error) -> String {
    if error is CancellationError { return "cancelled" }
    switch error {
    case ConsumerError.usage: return "usage"
    case ConsumerError.invalidConfiguration: return "invalid_configuration"
    case ConsumerError.unsupportedLinuxCertificateAnchors: return "unsupported_ca_configuration"
    case ConsumerError.noVehicles: return "no_vehicles"
    case ConsumerError.sessionClosed: return "session_closed"
    case ConsumerError.invalidDriveQuery: return "invalid_drive_query"
    case ConsumerError.invalidDrivePage: return "invalid_drive_page"
    case ConsumerError.paginationCursorCycle: return "pagination_cursor_cycle"
    case ConsumerError.paginationLimitExceeded: return "pagination_limit"
    case ConsumerError.paginationUnexpectedNotModified: return "unexpected_not_modified"
    case ConsumerError.unexpectedDiscovery: return "unexpected_discovery"
    case ConsumerError.unexpectedFixture: return "unexpected_fixture"
    case ConsumerError.invitationReplayAccepted: return "invitation_replay_accepted"
    case ConsumerError.oldCredentialAccepted: return "old_credential_accepted"
    case ConsumerError.invalidRestartGate: return "invalid_restart_gate"
    case ConsumerError.restartTimeout: return "restart_timeout"
    case ConsumerError.restartSemanticMismatch: return "restart_semantic_mismatch"
    case ConsumerError.privateOutputFailure: return "private_output_failure"
    case ConsumerPrivateInputError.invalidPath,
      ConsumerPrivateInputError.notRegular,
      ConsumerPrivateInputError.wrongOwner,
      ConsumerPrivateInputError.insecurePermissions,
      ConsumerPrivateInputError.tooLarge,
      ConsumerPrivateInputError.unreadable:
      return "invalid_private_input"
    case CurrentHubError.unauthorized: return "unauthorized"
    case CurrentHubError.notFound: return "not_found"
    case CurrentHubError.serviceUnavailable: return "service_unavailable"
    case CurrentHubError.api(let statusCode, _, _, _): return "api_error_\(statusCode)"
    case CurrentHubError.transportFailure: return "transport_failure"
    case CurrentHubError.credentialExpired, CurrentHubError.invitationExpired: return "expired"
    case CurrentHubError.capabilityUnavailable: return "unsupported"
    case CurrentHubError.invalidBinding,
      CurrentHubError.invalidDiscoveryURL,
      CurrentHubError.invalidDiscovery,
      CurrentHubError.hubIdentityMismatch,
      CurrentHubError.untrustedOrigin:
      return "invalid_profile"
    case CurrentHubError.invalidRequest: return "invalid_request"
    case CurrentHubError.invalidResponse: return "invalid_response"
    default: return "failure"
    }
  }
}

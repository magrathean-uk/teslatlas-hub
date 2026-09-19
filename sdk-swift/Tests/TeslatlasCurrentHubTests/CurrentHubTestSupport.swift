import Foundation
import XCTest

@testable import TeslatlasCurrentHub

#if !SWIFT_PACKAGE
private final class CurrentHubRuntimeTestBundleMarker {}
#endif

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif
#if canImport(Darwin)
  import Darwin
#else
  import Glibc
#endif

enum CurrentHubTestData {
  static let endpoint = URL(string: "https://hub.example.invalid")!
  static let hubID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
  static let vehicleID = hubID
  static let tokenA = String(repeating: "a", count: 64)
  static let tokenB = String(repeating: "b", count: 64)
  static let eTag = "\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\""

  static func fixture(_ name: String) throws -> Data {
    #if SWIFT_PACKAGE
    let bundle = Bundle.module
    #else
    let bundle = Bundle(for: CurrentHubRuntimeTestBundleMarker.self)
    #endif
    let url = bundle.url(
      forResource: name,
      withExtension: "json",
      subdirectory: "Fixtures"
    ) ?? bundle.url(forResource: name, withExtension: "json")
    guard let url else { throw CurrentHubTestError.missingFixture(name) }
    return try Data(contentsOf: url)
  }
}

enum CurrentHubTestError: Error { case missingFixture(String), noResponse }

struct CurrentHubStubResponse: Sendable {
  let statusCode: Int
  let headers: [String: String]
  let body: Data
  let finalURL: URL?

  init(
    statusCode: Int = 200,
    headers: [String: String] = ["Content-Type": "application/json"],
    body: Data,
    finalURL: URL? = nil
  ) {
    self.statusCode = statusCode
    self.headers = headers
    self.body = body
    self.finalURL = finalURL
  }
}

actor ScriptedCurrentHubTransport: CurrentHubInvitationPinningTransport {
  private var responses: [CurrentHubStubResponse]
  private var captured: [URLRequest] = []
  private var validatedLeafPins: [String] = []

  init(_ responses: [CurrentHubStubResponse]) { self.responses = responses }

  func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse {
    captured.append(request)
    guard !responses.isEmpty, let url = request.url else {
      throw CurrentHubTestError.noResponse
    }
    let response = responses.removeFirst()
    return CurrentHubHTTPResponse(
      statusCode: response.statusCode,
      headers: response.headers,
      body: response.body,
      finalURL: response.finalURL ?? url
    )
  }

  func send(
    _ request: URLRequest,
    validatingLeafCertificateSHA256 expectedLeafCertificateSHA256: String
  ) async throws -> CurrentHubHTTPResponse {
    validatedLeafPins.append(expectedLeafCertificateSHA256)
    return try await send(request)
  }

  func requests() -> [URLRequest] { captured }
  func leafPins() -> [String] { validatedLeafPins }
}

actor MemoryCurrentHubCredentialStore: CurrentHubCredentialStore {
  private var value: CurrentHubCredential?
  private var saves = 0
  init(_ value: CurrentHubCredential? = nil) { self.value = value }
  func loadCredential() async throws -> CurrentHubCredential? { value }
  func saveCredential(_ credential: CurrentHubCredential) async throws {
    saves += 1
    value = credential
  }
  func saveCount() -> Int { saves }
}

func currentHubCredential(_ token: String = CurrentHubTestData.tokenA) throws
  -> CurrentHubCredential
{
  try CurrentHubCredential(
    deviceID: CurrentHubTestData.hubID,
    accessToken: token,
    expiresAtMilliseconds: Int64.max
  )
}

func makeCurrentHubClient(
  credential: CurrentHubCredential? = try? currentHubCredential(),
  additionalResponses: [CurrentHubStubResponse] = []
) async throws -> (CurrentHubClient, ScriptedCurrentHubTransport, MemoryCurrentHubCredentialStore) {
  let transport = ScriptedCurrentHubTransport(
    [CurrentHubStubResponse(body: try CurrentHubTestData.fixture("discovery"))]
      + additionalResponses
  )
  let store = MemoryCurrentHubCredentialStore(credential)
  let client = try await CurrentHubClient.connectForTesting(
    endpoint: CurrentHubTestData.endpoint,
    expectedHubID: CurrentHubTestData.hubID,
    credentialStore: store,
    transport: transport
  )
  return (client, transport, store)
}

func assertCurrentHubError<T>(
  _ expression: @autoclosure () async throws -> T,
  file: StaticString = #filePath,
  line: UInt = #line,
  _ verify: (CurrentHubError) -> Void
) async {
  do {
    _ = try await expression()
    XCTFail("Expected CurrentHubError", file: file, line: line)
  } catch let error as CurrentHubError {
    verify(error)
  } catch {
    XCTFail("Unexpected error: \(error)", file: file, line: line)
  }
}

struct CurrentHubTranscriptEntry: Codable, Equatable, Sendable {
  let method: String
  let route: String
  let status: Int
  let requestID: String
  let scope: String
  let requestIfNoneMatch: String?
  let responseETag: String?
  let responseCacheControl: String?

  init(
    method: String,
    route: String,
    status: Int,
    requestID: String,
    scope: String,
    requestIfNoneMatch: String? = nil,
    responseETag: String? = nil,
    responseCacheControl: String? = nil
  ) {
    self.method = method
    self.route = route
    self.status = status
    self.requestID = requestID
    self.scope = scope
    self.requestIfNoneMatch = requestIfNoneMatch
    self.responseETag = responseETag
    self.responseCacheControl = responseCacheControl
  }

  enum CodingKeys: String, CodingKey {
    case method, route, status, scope
    case requestID = "request_id"
    case requestIfNoneMatch = "request_if_none_match"
    case responseETag = "response_etag"
    case responseCacheControl = "response_cache_control"
  }
}

actor CurrentHubTranscriptTransport: CurrentHubInvitationPinningTransport {
  private let base: any CurrentHubInvitationPinningTransport
  private var entries: [CurrentHubTranscriptEntry] = []

  init(base: any CurrentHubInvitationPinningTransport) { self.base = base }

  func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse {
    do {
      let response = try await base.send(request)
      entries.append(Self.entry(request, response))
      return response
    } catch {
      entries.append(Self.failureEntry(request, error))
      throw error
    }
  }

  func send(
    _ request: URLRequest,
    validatingLeafCertificateSHA256 expectedLeafCertificateSHA256: String
  ) async throws -> CurrentHubHTTPResponse {
    do {
      let response = try await base.send(
        request,
        validatingLeafCertificateSHA256: expectedLeafCertificateSHA256
      )
      entries.append(Self.entry(request, response))
      return response
    } catch {
      entries.append(Self.failureEntry(request, error))
      throw error
    }
  }

  func count() -> Int { entries.count }
  func snapshot(since index: Int = 0) -> [CurrentHubTranscriptEntry] {
    guard index >= 0, index <= entries.count else { return [] }
    return Array(entries[index..<entries.endIndex])
  }

  private static func failureEntry(
    _ request: URLRequest, _ error: Error
  ) -> CurrentHubTranscriptEntry {
    let status: Int
    let requestID: String
    switch error {
    case CurrentHubError.unauthorized(let value): status = 401; requestID = value ?? "missing-request-id"
    case CurrentHubError.notFound(let value): status = 404; requestID = value ?? "missing-request-id"
    case CurrentHubError.serviceUnavailable(let value): status = 503; requestID = value ?? "missing-request-id"
    case CurrentHubError.api(let value, _, _, let identifier): status = value; requestID = identifier ?? "missing-request-id"
    default: status = 0; requestID = "transport-no-response"
    }
    return CurrentHubTranscriptEntry(
      method: request.httpMethod ?? "GET",
      route: request.url.map(redactedRoute) ?? "/invalid",
      status: status,
      requestID: requestID,
      scope: request.url.map(requestScope) ?? "/invalid",
      requestIfNoneMatch: request.value(forHTTPHeaderField: "If-None-Match")
    )
  }

  private static func entry(
    _ request: URLRequest,
    _ response: CurrentHubHTTPResponse
  ) -> CurrentHubTranscriptEntry {
    CurrentHubTranscriptEntry(
      method: request.httpMethod ?? "GET",
      route: redactedRoute(response.finalURL),
      status: response.statusCode,
      requestID: response.headers.first {
        $0.key.caseInsensitiveCompare("X-Request-ID") == .orderedSame
      }?.value ?? "missing-request-id",
      scope: request.url.map(requestScope) ?? "/invalid",
      requestIfNoneMatch: request.value(forHTTPHeaderField: "If-None-Match"),
      responseETag: response.headers.first {
        $0.key.caseInsensitiveCompare("ETag") == .orderedSame
      }?.value,
      responseCacheControl: response.headers.first {
        $0.key.caseInsensitiveCompare("Cache-Control") == .orderedSame
      }?.value
    )
  }

  private static func redactedRoute(_ url: URL) -> String {
    let pieces = url.path.split(separator: "/", omittingEmptySubsequences: false)
    var result = pieces.map(String.init)
    if result.count >= 4, result[1] == "v1", result[2] == "pairings" {
      result[3] = "{pairing_id}"
    }
    if result.count >= 4, result[1] == "v1", result[2] == "vehicles" {
      result[3] = "{vehicle_id}"
    }
    return result.joined(separator: "/")
  }

  private static func requestScope(_ url: URL) -> String {
    guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
      return url.path
    }
    return components.percentEncodedQuery.map { components.path + "?" + $0 } ?? components.path
  }
}

struct CurrentHubMatrixFileBinding: Codable, Sendable {
  let path: String
  let sha256: String
}

struct CurrentHubMatrixStagedFile: Codable, Sendable {
  let id: String
  let root: CurrentHubMatrixFileBinding
  let local: CurrentHubMatrixFileBinding
}

struct MatrixWorkerCellDeadline: Sendable {
  let deadlineNanoseconds: UInt64

  init(startNanoseconds: UInt64, remainingMilliseconds: Int) throws {
    guard (1...3_600_000).contains(remainingMilliseconds) else {
      throw CurrentHubError.invalidRequest("matrix worker deadline is invalid")
    }
    let duration = UInt64(remainingMilliseconds).multipliedReportingOverflow(by: 1_000_000)
    let deadline = startNanoseconds.addingReportingOverflow(duration.partialValue)
    guard !duration.overflow, !deadline.overflow else {
      throw CurrentHubError.invalidRequest("matrix worker deadline is invalid")
    }
    deadlineNanoseconds = deadline.partialValue
  }

  func phaseDeadline(startingAt: UInt64, phaseMilliseconds: Int) throws -> UInt64 {
    guard phaseMilliseconds > 0 else {
      throw CurrentHubError.invalidRequest("matrix phase timeout is invalid")
    }
    let duration = UInt64(phaseMilliseconds).multipliedReportingOverflow(by: 1_000_000)
    let phase = startingAt.addingReportingOverflow(duration.partialValue)
    guard !duration.overflow, !phase.overflow else {
      throw CurrentHubError.invalidRequest("matrix phase timeout is invalid")
    }
    return min(deadlineNanoseconds, phase.partialValue)
  }

  func isExpired(at nowNanoseconds: UInt64) -> Bool {
    nowNanoseconds >= deadlineNanoseconds
  }
}

struct CurrentHubMatrixWorkerConfig: Codable, Sendable {
  let schemaVersion: Int
  let kind: String
  let actorID: String
  let sessionID: UUID
  let cellID: String
  let instanceNonce: String
  let sessionInputSHA256: String
  let remainingCellMilliseconds: Int
  let phaseContract: CurrentHubMatrixStagedFile
  let inputs: [CurrentHubMatrixStagedFile]
  let privateRoot: String
  let coordinationDirectory: String
  let evidencePath: String
  let logPath: String

  enum CodingKeys: String, CodingKey {
    case kind, inputs
    case schemaVersion = "schema_version"
    case actorID = "actor_id"
    case sessionID = "session_id"
    case cellID = "cell_id"
    case instanceNonce = "instance_nonce"
    case sessionInputSHA256 = "session_input_sha256"
    case remainingCellMilliseconds = "remaining_cell_ms"
    case phaseContract = "phase_contract"
    case privateRoot = "private_root"
    case coordinationDirectory = "coordination_dir"
    case evidencePath = "evidence_path"
    case logPath = "log_path"
  }

  static func decode(_ data: Data) throws -> CurrentHubMatrixWorkerConfig {
    let object = try matrixStrictJSONObject(data)
    guard let dictionary = object as? [String: Any], Set(dictionary.keys) == Set([
      "schema_version", "kind", "actor_id", "session_id", "cell_id",
      "instance_nonce", "session_input_sha256", "phase_contract", "inputs",
      "remaining_cell_ms", "private_root", "coordination_dir", "evidence_path", "log_path",
    ]) else { throw CurrentHubError.invalidRequest("matrix worker config shape is invalid") }
    guard strictInteger(dictionary["schema_version"]),
      strictInteger(dictionary["remaining_cell_ms"]),
      strictStagedFileObject(dictionary["phase_contract"]),
      let inputs = dictionary["inputs"] as? [Any],
      inputs.allSatisfy(strictStagedFileObject)
    else { throw CurrentHubError.invalidRequest("matrix worker config shape is invalid") }
    let value = try JSONDecoder().decode(Self.self, from: data)
    let hex = CharacterSet(charactersIn: "0123456789abcdef")
    guard value.schemaVersion == 1, value.kind == "matrix-actor-worker",
      ["swift_macos", "swift_linux"].contains(value.actorID),
      ["swift__macos_arm64", "swift__debian13_amd64", "swift__debian13_arm64"]
        .contains(value.cellID),
      value.instanceNonce.count == 64,
      value.instanceNonce.unicodeScalars.allSatisfy(hex.contains),
      value.sessionInputSHA256.count == 64,
      value.sessionInputSHA256.unicodeScalars.allSatisfy(hex.contains),
      (1...3_600_000).contains(value.remainingCellMilliseconds),
      [value.privateRoot, value.coordinationDirectory, value.evidencePath, value.logPath]
        .allSatisfy({ $0.hasPrefix("/") && !$0.contains("..") }),
      strictStagedFile(value.phaseContract),
      value.inputs.count <= 64,
      Set(value.inputs.map(\.id)).count == value.inputs.count,
      value.inputs.filter({ $0.id == "initial_observation" }).count == 1,
      value.inputs.allSatisfy(strictStagedFile)
    else { throw CurrentHubError.invalidRequest("matrix worker config identity is invalid") }
    return value
  }

  private static func strictStagedFile(_ value: CurrentHubMatrixStagedFile) -> Bool {
    let token = value.id.range(
      of: #"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"#,
      options: .regularExpression
    ) != nil
    return token && strictBinding(value.root) && strictBinding(value.local)
      && value.root.sha256 == value.local.sha256
  }

  private static func strictBinding(_ value: CurrentHubMatrixFileBinding) -> Bool {
    let hex = CharacterSet(charactersIn: "0123456789abcdef")
    return value.path.hasPrefix("/") && !value.path.contains("..")
      && !value.path.contains("\n") && !value.path.contains("\0")
      && value.sha256.count == 64
      && value.sha256.unicodeScalars.allSatisfy(hex.contains)
  }

  private static func strictInteger(_ value: Any?) -> Bool {
    guard let number = value as? NSNumber else { return false }
    let type = String(cString: number.objCType)
    return !["c", "C", "B", "d", "D", "f", "F"].contains(type)
  }

  private static func strictStagedFileObject(_ value: Any?) -> Bool {
    guard let dictionary = value as? [String: Any], Set(dictionary.keys) == Set([
      "id", "root", "local",
    ]), dictionary["id"] is String,
      strictBindingObject(dictionary["root"]), strictBindingObject(dictionary["local"])
    else { return false }
    return true
  }

  private static func strictBindingObject(_ value: Any?) -> Bool {
    guard let dictionary = value as? [String: Any], Set(dictionary.keys) == Set([
      "path", "sha256",
    ]), dictionary["path"] is String, dictionary["sha256"] is String
    else { return false }
    return true
  }
}

private func matrixPrivateAdmissionPath(_ url: URL) throws -> [String] {
  var path = url.path
  guard path.hasPrefix("/"), !path.isEmpty,
    !path.contains("\0"), !path.contains("\n"), !path.contains("\r")
  else { throw CurrentHubError.invalidRequest("matrix private file path is invalid") }

  // macOS exposes these fixed system directories through aliases. Resolve
  // only a canonical system alias; every caller-provided parent is then
  // opened with O_NOFOLLOW through directory descriptors below.
  for (alias, target) in [("/etc", "/private/etc"), ("/tmp", "/private/tmp"),
                          ("/var", "/private/var")] {
    guard path == alias || path.hasPrefix(alias + "/") else { continue }
    let destination = try? FileManager.default.destinationOfSymbolicLink(atPath: alias)
    if destination == String(target.dropFirst()) || destination == target {
      path = target + String(path.dropFirst(alias.count))
      break
    }
  }

  let components = path.split(separator: "/", omittingEmptySubsequences: true)
    .map(String.init)
  guard !components.isEmpty, !components.contains(where: { $0 == "." || $0 == ".." }) else {
    throw CurrentHubError.invalidRequest("matrix private file path is invalid")
  }
  return components
}

private func matrixAdmitPrivateDirectory(_ descriptor: Int32) throws {
  var metadata = stat()
  guard fstat(descriptor, &metadata) == 0,
    (metadata.st_mode & mode_t(S_IFMT)) == mode_t(S_IFDIR)
  else { throw CurrentHubError.invalidRequest("matrix private parent directory is invalid") }
  guard metadata.st_uid == getuid() || metadata.st_uid == 0 else {
    throw CurrentHubError.invalidRequest("matrix private parent directory ownership is invalid")
  }
  // Ancestors may be searchable by the system, but a writable one must be
  // private or sticky (the latter is required for /tmp).
  guard metadata.st_mode & 0o022 == 0 || metadata.st_mode & 0o1000 != 0 else {
    throw CurrentHubError.invalidRequest("matrix private parent directory mode is invalid")
  }
}

private func matrixOpenPrivateParent(_ url: URL) throws -> (descriptor: Int32, leaf: String) {
  let components = try matrixPrivateAdmissionPath(url)
  let directoryFlags = O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC
  let root = "/".withCString { open($0, directoryFlags) }
  guard root >= 0 else {
    throw CurrentHubError.invalidRequest("matrix private parent directory is unavailable")
  }
  var parent = root
  do {
    try matrixAdmitPrivateDirectory(parent)
    for component in components.dropLast() {
      let child = component.withCString { openat(parent, $0, directoryFlags) }
      guard child >= 0 else {
        throw CurrentHubError.invalidRequest("matrix private parent directory is unavailable")
      }
      do {
        try matrixAdmitPrivateDirectory(child)
      } catch {
        _ = close(child)
        throw error
      }
      _ = close(parent)
      parent = child
    }
    return (parent, components[components.count - 1])
  } catch {
    _ = close(parent)
    throw error
  }
}

private func matrixOpenPrivateFile(_ url: URL) throws -> Int32 {
  let (parent, leaf) = try matrixOpenPrivateParent(url)
  defer { _ = close(parent) }
  let descriptor = leaf.withCString {
    openat(parent, $0, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)
  }
  guard descriptor >= 0 else {
    throw CurrentHubError.invalidRequest("matrix private file is unavailable")
  }
  return descriptor
}

func matrixPrivateRead(
  _ url: URL, maximumBytes: Int, expectedSHA256: String? = nil
) throws -> Data {
  guard maximumBytes >= 0 else {
    throw CurrentHubError.invalidRequest("matrix private file bound is invalid")
  }
  let descriptor = try matrixOpenPrivateFile(url)
  defer { _ = close(descriptor) }
  var metadata = stat()
  guard fstat(descriptor, &metadata) == 0,
    metadata.st_uid == getuid(),
    (metadata.st_mode & mode_t(S_IFMT)) == mode_t(S_IFREG),
    metadata.st_mode & 0o077 == 0,
    metadata.st_size >= 0,
    metadata.st_size <= maximumBytes
  else { throw CurrentHubError.invalidRequest("matrix private file admission failed") }
  var data = Data()
  data.reserveCapacity(Int(metadata.st_size))
  var buffer = [UInt8](repeating: 0, count: min(65_536, maximumBytes + 1))
  while data.count <= maximumBytes {
    let count = read(descriptor, &buffer, min(buffer.count, maximumBytes + 1 - data.count))
    guard count >= 0 else {
      throw CurrentHubError.invalidRequest("matrix private file read failed")
    }
    if count == 0 { break }
    data.append(buffer, count: count)
  }
  guard data.count <= maximumBytes else {
    throw CurrentHubError.invalidRequest("matrix private file exceeds bound")
  }
  if let expectedSHA256 {
    guard CurrentHubSHA256.hexDigest(of: data) == expectedSHA256 else {
      throw CurrentHubError.invalidRequest("matrix private file binding changed")
    }
  }
  return data
}

func matrixStrictJSONObject(_ data: Data) throws -> Any {
  guard String(data: data, encoding: .utf8) != nil else {
    throw CurrentHubError.invalidRequest("matrix JSON is invalid")
  }
  var scanner = MatrixStrictJSONScanner(bytes: [UInt8](data))
  try scanner.validate()
  return try JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
}

private struct MatrixStrictJSONScanner {
  let bytes: [UInt8]
  var index = 0

  mutating func validate() throws {
    skipWhitespace()
    try value()
    skipWhitespace()
    guard index == bytes.count else { throw invalid() }
  }

  private func invalid() -> CurrentHubError {
    .invalidRequest("matrix JSON is invalid")
  }

  mutating private func skipWhitespace() {
    while index < bytes.count, [0x20, 0x09, 0x0a, 0x0d].contains(bytes[index]) {
      index += 1
    }
  }

  mutating private func value() throws {
    guard index < bytes.count else { throw invalid() }
    switch bytes[index] {
    case 0x7b: try object()
    case 0x5b: try array()
    case 0x22: _ = try string()
    case 0x74: try literal("true")
    case 0x66: try literal("false")
    case 0x6e: try literal("null")
    case 0x2d, 0x30...0x39: try number()
    default: throw invalid()
    }
  }

  mutating private func object() throws {
    index += 1; skipWhitespace()
    var keys = Set<String>()
    if take(0x7d) { return }
    while true {
      guard index < bytes.count, bytes[index] == 0x22 else { throw invalid() }
      let key = try string()
      guard keys.insert(key).inserted else { throw invalid() }
      skipWhitespace(); guard take(0x3a) else { throw invalid() }
      skipWhitespace(); try value(); skipWhitespace()
      if take(0x7d) { return }
      guard take(0x2c) else { throw invalid() }
      skipWhitespace()
    }
  }

  mutating private func array() throws {
    index += 1; skipWhitespace()
    if take(0x5d) { return }
    while true {
      try value(); skipWhitespace()
      if take(0x5d) { return }
      guard take(0x2c) else { throw invalid() }
      skipWhitespace()
    }
  }

  mutating private func string() throws -> String {
    let start = index
    index += 1
    while index < bytes.count {
      let byte = bytes[index]
      if byte == 0x22 {
        index += 1
        let literal = Data(bytes[start..<index])
        return try JSONDecoder().decode(String.self, from: literal)
      }
      if byte < 0x20 { throw invalid() }
      if byte == 0x5c {
        index += 1
        guard index < bytes.count else { throw invalid() }
        if bytes[index] == 0x75 {
          guard index + 4 < bytes.count,
            bytes[(index + 1)...(index + 4)].allSatisfy({
              (0x30...0x39).contains($0) || (0x41...0x46).contains($0) || (0x61...0x66).contains($0)
            })
          else { throw invalid() }
          index += 4
        } else if ![0x22, 0x5c, 0x2f, 0x62, 0x66, 0x6e, 0x72, 0x74].contains(bytes[index]) {
          throw invalid()
        }
      }
      index += 1
    }
    throw invalid()
  }

  mutating private func literal(_ text: String) throws {
    let literal = Array(text.utf8)
    guard index + literal.count <= bytes.count,
      Array(bytes[index..<(index + literal.count)]) == literal
    else { throw invalid() }
    index += literal.count
  }

  mutating private func number() throws {
    let start = index
    while index < bytes.count,
      ![0x20, 0x09, 0x0a, 0x0d, 0x2c, 0x5d, 0x7d].contains(bytes[index]) {
      index += 1
    }
    guard let text = String(bytes: bytes[start..<index], encoding: .utf8),
      text.range(
        of: #"^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?$"#,
        options: .regularExpression
      ) != nil
    else { throw invalid() }
    if text.contains(".") || text.contains("e") || text.contains("E") {
      guard let number = Double(text), number.isFinite else { throw invalid() }
    }
  }

  mutating private func take(_ byte: UInt8) -> Bool {
    guard index < bytes.count, bytes[index] == byte else { return false }
    index += 1
    return true
  }
}

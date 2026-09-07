import Foundation
import XCTest

@testable import TeslatlasCurrentHub

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

enum CurrentHubTestData {
  static let endpoint = URL(string: "https://hub.example.invalid")!
  static let hubID = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
  static let vehicleID = hubID
  static let tokenA = String(repeating: "a", count: 64)
  static let tokenB = String(repeating: "b", count: 64)
  static let eTag = "\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\""

  static func fixture(_ name: String) throws -> Data {
    guard let url = Bundle.module.url(
      forResource: name,
      withExtension: "json",
      subdirectory: "Fixtures"
    ) else { throw CurrentHubTestError.missingFixture(name) }
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

  enum CodingKeys: String, CodingKey {
    case method, route, status
    case requestID = "request_id"
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
      requestID: requestID
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
      }?.value ?? "missing-request-id"
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

struct CurrentHubMatrixWorkerConfig: Codable, Sendable {
  let schemaVersion: Int
  let kind: String
  let actorID: String
  let sessionID: UUID
  let cellID: String
  let instanceNonce: String
  let sessionInputSHA256: String
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
    case phaseContract = "phase_contract"
    case privateRoot = "private_root"
    case coordinationDirectory = "coordination_dir"
    case evidencePath = "evidence_path"
    case logPath = "log_path"
  }

  static func decode(_ data: Data) throws -> CurrentHubMatrixWorkerConfig {
    let object = try JSONSerialization.jsonObject(with: data)
    guard let dictionary = object as? [String: Any], Set(dictionary.keys) == Set([
      "schema_version", "kind", "actor_id", "session_id", "cell_id",
      "instance_nonce", "session_input_sha256", "phase_contract", "inputs",
      "private_root", "coordination_dir", "evidence_path", "log_path",
    ]) else { throw CurrentHubError.invalidRequest("matrix worker config shape is invalid") }
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
}

import Foundation

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

public enum CurrentHubUnavailableOperation: String, Sendable {
  case charges, commands, events, metadata, pairedDevices, dataQuality
}

public actor CurrentHubClient {
  private let binding: CurrentHubBinding
  private let endpoint: CurrentHubEndpoint
  private let expectedHubID: UUID
  private let credentialStore: any CurrentHubCredentialStore
  private let transport: any CurrentHubHTTPTransport
  private var discovery: CurrentHubDiscovery

  private init(
    binding: CurrentHubBinding,
    endpoint: CurrentHubEndpoint,
    expectedHubID: UUID,
    credentialStore: any CurrentHubCredentialStore,
    transport: any CurrentHubHTTPTransport,
    discovery: CurrentHubDiscovery
  ) {
    self.binding = binding
    self.endpoint = endpoint
    self.expectedHubID = expectedHubID
    self.credentialStore = credentialStore
    self.transport = transport
    self.discovery = discovery
  }

  public static func connect(
    endpoint: URL,
    expectedHubID: UUID,
    credentialStore: any CurrentHubCredentialStore
  ) async throws -> CurrentHubClient {
    let binding = try CurrentHubBinding.load()
    return try await connect(
      binding: binding,
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      credentialStore: credentialStore,
      transport: CurrentHubURLSessionTransport(
        maximumResponseBytes: binding.maximumResponseBytes
      )
    )
  }

  public static func connect(
    endpoint: URL,
    expectedHubID: UUID,
    credentialStore: any CurrentHubCredentialStore,
    transport: any CurrentHubHTTPTransport
  ) async throws -> CurrentHubClient {
    try await connect(
      binding: CurrentHubBinding.load(),
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      credentialStore: credentialStore,
      transport: transport
    )
  }

  static func connectForTesting(
    endpoint: URL,
    expectedHubID: UUID,
    credentialStore: any CurrentHubCredentialStore,
    transport: any CurrentHubHTTPTransport
  ) async throws -> CurrentHubClient {
    try await connect(
      binding: CurrentHubBinding.load(),
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      credentialStore: credentialStore,
      transport: transport
    )
  }

  private static func connect(
    binding: CurrentHubBinding,
    endpoint endpointURL: URL,
    expectedHubID: UUID,
    credentialStore: any CurrentHubCredentialStore,
    transport: any CurrentHubHTTPTransport
  ) async throws -> CurrentHubClient {
    guard expectedHubID != UUID.currentHubZero else {
      throw CurrentHubError.invalidRequest("expected Hub identity must not be nil UUID")
    }
    let endpoint = try CurrentHubEndpoint(endpointURL)
    let discovery = try await fetchDiscovery(
      binding: binding,
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      transport: transport
    )
    return CurrentHubClient(
      binding: binding,
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      credentialStore: credentialStore,
      transport: transport,
      discovery: discovery
    )
  }

  public func discoveryDocument() -> CurrentHubDiscovery { discovery }

  @discardableResult
  public func refreshDiscovery() async throws -> CurrentHubDiscovery {
    let value = try await Self.fetchDiscovery(
      binding: binding,
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      transport: transport
    )
    discovery = value
    return value
  }

  public func health() async throws -> CurrentHubHealth {
    let response = try await send(request(path: "/healthz"))
    let health = try decodeSuccess(CurrentHubHealth.self, response: response, operation: "health")
    guard health.status == "ok", binding.testedHubVersions.contains(health.version) else {
      throw invalid(response, reason: "health response has an unexpected status or product version")
    }
    return health
  }

  public func readiness() async throws -> CurrentHubReadiness {
    let response = try await send(request(path: "/readyz"))
    if response.statusCode == 503 {
      if !response.body.isEmpty,
        let readiness = try? JSONDecoder().decode(CurrentHubReadiness.self, from: response.body),
        readiness.status == "not_ready",
        let reason = readiness.reason,
        Self.readinessReasons.contains(reason)
      {
        return readiness
      }
      throw CurrentHubError.serviceUnavailable(requestID: response.header("X-Request-ID"))
    }
    let readiness = try decodeSuccess(CurrentHubReadiness.self, response: response, operation: "readiness")
    guard readiness.status == "ready", readiness.reason == nil else {
      throw invalid(response, reason: "ready response does not match hub-http-v1@1.0.0")
    }
    return readiness
  }

  public func claim(
    invitation: CurrentHubInvitation,
    deviceName: String
  ) async throws -> CurrentHubCredential {
    try validate(invitation: invitation)
    guard !deviceName.isEmpty, deviceName.utf8.count <= 65_536,
      deviceName.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) })
    else {
      throw CurrentHubError.invalidRequest("device name is empty, too long, or contains control characters")
    }
    var request = request(path: "/v1/pairings/\(invitation.pairingID.uuidString.lowercased())/claim")
    request.httpMethod = "POST"
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.httpBody = try JSONSerialization.data(
      withJSONObject: ["secret": invitation.secret, "device_name": deviceName],
      options: [.sortedKeys]
    )
    guard let pinningTransport = transport as? any CurrentHubInvitationPinningTransport else {
      throw CurrentHubError.invalidRequest(
        "current-Hub claim transport must own invitation leaf pin validation"
      )
    }
    let response = try await send(
      request,
      using: pinningTransport,
      validatingLeafCertificateSHA256: invitation.tlsPin
    )
    let envelope = try decodeSuccess(
      CurrentHubClaimEnvelope.self,
      response: response,
      operation: "pairing claim"
    )
    let credential = try envelope.credential()
    try requireFreshlyIssued(credential, response: response, operation: "pairing claim")
    try await credentialStore.saveCredential(credential)
    return credential
  }

  public func rotateCredential() async throws -> CurrentHubCredential {
    var request = request(path: "/v1/device/rotate")
    request.httpMethod = "POST"
    try await authorize(&request)
    let response = try await send(request)
    let envelope = try decodeSuccess(
      CurrentHubClaimEnvelope.self,
      response: response,
      operation: "credential rotation"
    )
    let credential = try envelope.credential()
    try requireFreshlyIssued(credential, response: response, operation: "credential rotation")
    try await credentialStore.saveCredential(credential)
    return credential
  }

  public func vehicles() async throws -> [CurrentHubVehicle] {
    var request = request(path: "/v1/vehicles")
    try await authorize(&request)
    let response = try await send(request)
    if response.statusCode == 200 {
      try requireObjectKeys(
        response,
        required: CurrentHubVehicle.requiredWireKeys,
        arrayKey: "vehicles",
        context: "vehicle"
      )
    }
    let envelope = try decodeSuccess(
      CurrentHubVehicleListEnvelope.self,
      response: response,
      operation: "vehicles"
    )
    guard envelope.vehicles.count <= 10_000 else {
      throw invalid(response, reason: "vehicle list exceeds profile bound")
    }
    return envelope.vehicles
  }

  public func current(vehicleID: UUID) async throws -> CurrentHubCurrentState {
    try validateVehicleID(vehicleID)
    var request = request(
      path: "/v1/vehicles/\(vehicleID.uuidString.lowercased())/current"
    )
    try await authorize(&request)
    let response = try await send(request)
    if response.statusCode == 200 {
      try requireObjectKeys(
        response,
        required: CurrentHubCurrentState.requiredWireKeys,
        context: "current state"
      )
    }
    let value = try decodeSuccess(
      CurrentHubCurrentState.self,
      response: response,
      operation: "current state"
    )
    guard value.vehicleID == vehicleID else {
      throw invalid(response, reason: "current-state vehicle identity does not match the request")
    }
    return value
  }

  public func drives(
    vehicleID: UUID,
    query: CurrentHubDriveQuery = CurrentHubDriveQuery(),
    ifNoneMatch: CurrentHubEntityTag? = nil
  ) async throws -> CurrentHubDrivePageResult {
    try validateVehicleID(vehicleID)
    guard discovery.capabilities.contains("query.drives") else {
      throw CurrentHubError.capabilityUnavailable("query.drives")
    }
    let resolved = try resolve(query)
    var request = request(
      path: "/v1/vehicles/\(vehicleID.uuidString.lowercased())/drives",
      queryItems: resolved.items
    )
    try await authorize(&request)
    if let ifNoneMatch {
      request.setValue(ifNoneMatch.rawValue, forHTTPHeaderField: "If-None-Match")
    }
    let response = try await send(request)
    if response.statusCode == 304 {
      guard response.body.isEmpty else {
        throw invalid(response, reason: "304 drive response must have an empty body")
      }
      let eTag = try driveHeaders(response)
      return .notModified(eTag: eTag)
    }
    try requireSuccess(response, operation: "drives")
    let eTag = try driveHeaders(response)
    try requireObjectKeys(
      response,
      required: CurrentHubDrive.requiredWireKeys,
      arrayKey: "items",
      rootRequired: ["items", "next_cursor"],
      context: "drive"
    )
    let envelope: CurrentHubDrivePageEnvelope
    do {
      envelope = try JSONDecoder().decode(CurrentHubDrivePageEnvelope.self, from: response.body)
    } catch {
      throw invalid(response, reason: "drive page does not match hub-http-v1@1.0.0")
    }
    guard envelope.items.count <= resolved.limit else {
      throw invalid(response, reason: "drive page contains more items than the requested limit")
    }
    guard envelope.items.allSatisfy({ $0.vehicleID == vehicleID }) else {
      throw invalid(response, reason: "drive vehicle identity does not match the request")
    }
    guard envelope.items.allSatisfy({ drive in
      drive.startDateMilliseconds >= resolved.from
        && drive.startDateMilliseconds < resolved.to
    }) else {
      throw invalid(response, reason: "drive is outside the requested time range")
    }
    guard zip(envelope.items, envelope.items.dropFirst()).allSatisfy({ earlier, later in
      earlier.startDateMilliseconds > later.startDateMilliseconds
        || (earlier.startDateMilliseconds == later.startDateMilliseconds && earlier.id > later.id)
    }) else {
      throw invalid(response, reason: "drive page is not ordered by descending start date and ID")
    }
    let cursor: CurrentHubDriveCursor?
    do {
      cursor = try envelope.nextCursor.map(CurrentHubDriveCursor.init(rawValue:))
    } catch {
      throw invalid(response, reason: "drive page contains an invalid opaque cursor")
    }
    return .modified(CurrentHubDrivePage(items: envelope.items, nextCursor: cursor), eTag: eTag)
  }

  public func require(_ operation: CurrentHubUnavailableOperation) async throws -> Never {
    throw CurrentHubError.capabilityUnavailable(operation.rawValue)
  }

  private static func fetchDiscovery(
    binding: CurrentHubBinding,
    endpoint: CurrentHubEndpoint,
    expectedHubID: UUID,
    transport: any CurrentHubHTTPTransport
  ) async throws -> CurrentHubDiscovery {
    var request = URLRequest(url: endpoint.url(path: "/.well-known/teslatlas-hub"))
    request.httpMethod = "GET"
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    let response: CurrentHubHTTPResponse
    do { response = try await transport.send(request) }
    catch let error as CurrentHubError { throw error }
    catch is CancellationError { throw CancellationError() }
    catch let error as URLError { throw CurrentHubError.transportFailure(code: error.errorCode) }
    catch { throw CurrentHubError.transportFailure(code: -1) }
    try endpoint.requireSameOrigin(response.finalURL)
    guard response.statusCode == 200 else {
      if response.statusCode == 503 {
        throw CurrentHubError.serviceUnavailable(requestID: response.header("X-Request-ID"))
      }
      throw CurrentHubError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "unexpected discovery status"
      )
    }
    try requireJSON(response)
    let document: CurrentHubDiscovery
    do { document = try JSONDecoder().decode(CurrentHubDiscovery.self, from: response.body) }
    catch {
      throw CurrentHubError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "discovery does not match hub-http-v1@1.0.0"
      )
    }
    guard document.hubID == expectedHubID else {
      throw CurrentHubError.hubIdentityMismatch(expected: expectedHubID, actual: document.hubID)
    }
    let advertised = Set(document.capabilities)
    let known = binding.requiredCapabilities.union(binding.optionalCapabilities)
    guard document.protocolName == "teslatlas-sync",
      document.protocolMajor == 1,
      document.apiVersions == ["1.0"],
      Set(document.capabilities).count == document.capabilities.count,
      binding.testedHubVersions.contains(document.version),
      binding.requiredCapabilities.isSubset(of: advertised),
      advertised.isSubset(of: known),
      document.packFormat == "sqlite-zstd"
    else {
      throw CurrentHubError.invalidDiscovery("Hub does not match the approved current profile and tested product versions")
    }
    return document
  }

  private func request(path: String, queryItems: [URLQueryItem] = []) -> URLRequest {
    var request = URLRequest(url: endpoint.url(path: path, queryItems: queryItems))
    request.httpMethod = "GET"
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    return request
  }

  private func authorize(_ request: inout URLRequest) async throws {
    guard let credential = try await credentialStore.loadCredential() else {
      throw CurrentHubError.unauthorized(requestID: nil)
    }
    guard credential.expiresAtMilliseconds > Self.currentTimeMilliseconds else {
      throw CurrentHubError.credentialExpired
    }
    credential.apply(to: &request)
  }

  private func send(_ request: URLRequest) async throws -> CurrentHubHTTPResponse {
    do {
      let response = try await transport.send(request)
      try endpoint.requireSameOrigin(response.finalURL)
      return response
    } catch let error as CurrentHubError { throw error }
    catch is CancellationError { throw CancellationError() }
    catch let error as URLError { throw CurrentHubError.transportFailure(code: error.errorCode) }
    catch { throw CurrentHubError.transportFailure(code: -1) }
  }

  private func send(
    _ request: URLRequest,
    using transport: any CurrentHubInvitationPinningTransport,
    validatingLeafCertificateSHA256 expectedLeafCertificateSHA256: String
  ) async throws -> CurrentHubHTTPResponse {
    do {
      let response = try await transport.send(
        request,
        validatingLeafCertificateSHA256: expectedLeafCertificateSHA256
      )
      try endpoint.requireSameOrigin(response.finalURL)
      return response
    } catch let error as CurrentHubError { throw error }
    catch is CancellationError { throw CancellationError() }
    catch let error as URLError { throw CurrentHubError.transportFailure(code: error.errorCode) }
    catch { throw CurrentHubError.transportFailure(code: -1) }
  }

  private func requireFreshlyIssued(
    _ credential: CurrentHubCredential,
    response: CurrentHubHTTPResponse,
    operation: String
  ) throws {
    guard credential.expiresAtMilliseconds > Self.currentTimeMilliseconds else {
      throw invalid(response, reason: "\(operation) returned an expired credential")
    }
  }

  private func decodeSuccess<T: Decodable>(
    _ type: T.Type,
    response: CurrentHubHTTPResponse,
    operation: String
  ) throws -> T {
    try requireSuccess(response, operation: operation)
    do { return try JSONDecoder().decode(type, from: response.body) }
    catch { throw invalid(response, reason: "\(operation) response does not match hub-http-v1@1.0.0") }
  }

  private func requireSuccess(_ response: CurrentHubHTTPResponse, operation: String) throws {
    guard response.statusCode == 200 else {
      throw try mappedError(response, operation: operation)
    }
    try Self.requireJSON(response)
  }

  private static func requireJSON(_ response: CurrentHubHTTPResponse) throws {
    let media = response.header("Content-Type")?.split(separator: ";", maxSplits: 1).first?
      .trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    guard media == "application/json" else {
      throw CurrentHubError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "response is not application/json"
      )
    }
  }

  private func mappedError(_ response: CurrentHubHTTPResponse, operation: String) throws -> CurrentHubError {
    let requestID = response.header("X-Request-ID")
    switch response.statusCode {
    case 401: return .unauthorized(requestID: requestID)
    case 404: return .notFound(requestID: requestID)
    case 503 where response.body.isEmpty: return .serviceUnavailable(requestID: requestID)
    default: break
    }
    guard !response.body.isEmpty else {
      return .invalidResponse(statusCode: response.statusCode, requestID: requestID, reason: "unexpected empty \(operation) error")
    }
    try Self.requireJSON(response)
    guard let envelope = try? JSONDecoder().decode(CurrentHubAPIErrorEnvelope.self, from: response.body) else {
      return .invalidResponse(statusCode: response.statusCode, requestID: requestID, reason: "invalid \(operation) error envelope")
    }
    if response.statusCode == 503, envelope.error.code != "service_unavailable" {
      return .invalidResponse(statusCode: 503, requestID: requestID, reason: "503 error code does not match status")
    }
    return .api(statusCode: response.statusCode, code: envelope.error.code, message: envelope.error.message, requestID: requestID)
  }

  private func driveHeaders(_ response: CurrentHubHTTPResponse) throws -> CurrentHubEntityTag {
    guard response.header("Cache-Control") == "no-store" else {
      throw invalid(response, reason: "drive response requires Cache-Control: no-store")
    }
    guard let raw = response.header("ETag") else {
      throw invalid(response, reason: "drive response requires a strong ETag")
    }
    do { return try CurrentHubEntityTag(rawValue: raw) }
    catch { throw invalid(response, reason: "drive response has an invalid strong ETag") }
  }

  private func resolve(_ query: CurrentHubDriveQuery) throws
    -> (from: Int64, to: Int64, limit: Int, items: [URLQueryItem])
  {
    let from = query.fromMilliseconds ?? 0
    let to = query.toMilliseconds ?? Int64.max
    let limit = query.limit ?? 100
    guard from >= 0, to >= 0, from < to else {
      throw CurrentHubError.invalidRequest("drive range must be nonnegative, inclusive/exclusive, and increasing")
    }
    guard (1...500).contains(limit) else {
      throw CurrentHubError.invalidRequest("drive limit must be between 1 and 500")
    }
    var items: [URLQueryItem] = []
    if query.fromMilliseconds != nil { items.append(URLQueryItem(name: "from_ms", value: String(from))) }
    if query.toMilliseconds != nil { items.append(URLQueryItem(name: "to_ms", value: String(to))) }
    if query.limit != nil { items.append(URLQueryItem(name: "limit", value: String(limit))) }
    if let cursor = query.cursor { items.append(URLQueryItem(name: "cursor", value: cursor.rawValue)) }
    return (from, to, limit, items)
  }

  private func validateVehicleID(_ value: UUID) throws {
    guard value != UUID.currentHubZero else {
      throw CurrentHubError.invalidRequest("vehicle identity must not be nil UUID")
    }
  }

  private func validate(invitation: CurrentHubInvitation) throws {
    guard invitation.pairingID != UUID.currentHubZero,
      invitation.endpoint == endpoint.originURL,
      invitation.secret.utf8.count == 64,
      invitation.secret.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
      invitation.tlsPin.utf8.count == 64,
      invitation.tlsPin.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) })
    else {
      throw CurrentHubError.invalidRequest("pairing invitation identity, endpoint, secret, or TLS pin is invalid")
    }
    guard invitation.expiresAtMilliseconds > Self.currentTimeMilliseconds else {
      throw CurrentHubError.invitationExpired
    }
    guard let components = URLComponents(url: invitation.pairingURI, resolvingAgainstBaseURL: false),
      components.scheme == "teslatlas-hub", components.host == "pair",
      components.queryItems?.first(where: { $0.name == "endpoint" })?.value == endpoint.originURL.absoluteString,
      components.queryItems?.first(where: { $0.name == "pairing_id" })?.value == invitation.pairingID.uuidString.lowercased(),
      components.queryItems?.first(where: { $0.name == "secret" })?.value == invitation.secret,
      components.queryItems?.first(where: { $0.name == "tls_pin" })?.value == invitation.tlsPin
    else {
      throw CurrentHubError.invalidRequest("pairing URI does not match invitation fields")
    }
  }

  private func invalid(_ response: CurrentHubHTTPResponse, reason: String) -> CurrentHubError {
    .invalidResponse(statusCode: response.statusCode, requestID: response.header("X-Request-ID"), reason: reason)
  }

  private func requireObjectKeys(
    _ response: CurrentHubHTTPResponse,
    required: Set<String>,
    arrayKey: String? = nil,
    rootRequired: Set<String> = [],
    context: String
  ) throws {
    guard let root = try? JSONSerialization.jsonObject(with: response.body) as? [String: Any],
      rootRequired.isSubset(of: Set(root.keys))
    else {
      throw invalid(response, reason: "\(context) response is missing required fields")
    }
    let objects: [[String: Any]]
    if let arrayKey {
      guard let values = root[arrayKey] as? [[String: Any]] else {
        throw invalid(response, reason: "\(context) response is missing required fields")
      }
      objects = values
    } else {
      objects = [root]
    }
    guard objects.allSatisfy({ required.isSubset(of: Set($0.keys)) }) else {
      throw invalid(response, reason: "\(context) response is missing required fields")
    }
  }

  private static let readinessReasons: Set<String> = [
    "catalogue_unavailable", "lifecycle_quarantined", "published_content_unservable",
    "collector_absent", "collector_stale", "collector_auth_terminal",
  ]

  private static var currentTimeMilliseconds: Int64 {
    Int64(Date().timeIntervalSince1970 * 1_000)
  }
}

struct CurrentHubEndpoint: Sendable {
  let originURL: URL

  init(_ url: URL) throws {
    guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
      components.scheme?.lowercased() == "https",
      components.host != nil,
      components.user == nil, components.password == nil,
      components.query == nil, components.fragment == nil,
      components.path.isEmpty || components.path == "/"
    else {
      throw CurrentHubError.invalidDiscoveryURL("endpoint must be an HTTPS origin without path, credentials, query, or fragment")
    }
    var origin = components
    origin.path = ""
    guard let originURL = origin.url else {
      throw CurrentHubError.invalidDiscoveryURL("endpoint origin is invalid")
    }
    self.originURL = originURL
  }

  func url(path: String, queryItems: [URLQueryItem] = []) -> URL {
    var components = URLComponents(url: originURL, resolvingAgainstBaseURL: false)!
    components.path = path
    components.queryItems = queryItems.isEmpty ? nil : queryItems
    return components.url!
  }

  func requireSameOrigin(_ url: URL) throws {
    guard let actual = try? CurrentHubEndpoint(url.deletingPathComponents()).originURL,
      actual == originURL
    else {
      throw CurrentHubError.untrustedOrigin(url.absoluteString)
    }
  }
}

private extension URL {
  func deletingPathComponents() -> URL {
    guard var components = URLComponents(url: self, resolvingAgainstBaseURL: false) else { return self }
    components.path = ""
    components.query = nil
    components.fragment = nil
    return components.url ?? self
  }
}

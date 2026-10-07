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
  private let nowMilliseconds: @Sendable () -> Int64
  private var discovery: CurrentHubDiscovery
  private var identityFailure: CurrentHubError?
  private var identityGeneration: UInt64 = 0
  private var credentialGeneration: UInt64 = 0
  private var pendingIssuedCredential: CurrentHubCredential?
  private var credentialMutationInProgress = false

  private struct RequestContext {
    let discovery: CurrentHubDiscovery
    let identityGeneration: UInt64
    let credentialGeneration: UInt64
  }

  private init(
    binding: CurrentHubBinding,
    endpoint: CurrentHubEndpoint,
    expectedHubID: UUID,
    credentialStore: any CurrentHubCredentialStore,
    transport: any CurrentHubHTTPTransport,
    discovery: CurrentHubDiscovery,
    nowMilliseconds: @escaping @Sendable () -> Int64
  ) {
    self.binding = binding
    self.endpoint = endpoint
    self.expectedHubID = expectedHubID
    self.credentialStore = credentialStore
    self.transport = transport
    self.discovery = discovery
    self.nowMilliseconds = nowMilliseconds
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
    transport: any CurrentHubHTTPTransport,
    nowMilliseconds: @escaping @Sendable () -> Int64 = {
      Int64(Date().timeIntervalSince1970 * 1_000)
    }
  ) async throws -> CurrentHubClient {
    try await connect(
      binding: CurrentHubBinding.load(),
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      credentialStore: credentialStore,
      transport: transport,
      nowMilliseconds: nowMilliseconds
    )
  }

  private static func connect(
    binding: CurrentHubBinding,
    endpoint endpointURL: URL,
    expectedHubID: UUID,
    credentialStore: any CurrentHubCredentialStore,
    transport: any CurrentHubHTTPTransport,
    nowMilliseconds: @escaping @Sendable () -> Int64 = {
      Int64(Date().timeIntervalSince1970 * 1_000)
    }
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
      discovery: discovery,
      nowMilliseconds: nowMilliseconds
    )
  }

  public func discoveryDocument() -> CurrentHubDiscovery { discovery }

  @discardableResult
  public func refreshDiscovery() async throws -> CurrentHubDiscovery {
    let generation = identityGeneration
    let value: CurrentHubDiscovery
    do {
      value = try await Self.fetchDiscovery(
        binding: binding,
        endpoint: endpoint,
        expectedHubID: expectedHubID,
        transport: transport
      )
    } catch let error as CurrentHubError {
      if case .hubIdentityMismatch = error {
        identityFailure = error
        identityGeneration &+= 1
      }
      throw error
    }
    guard generation == identityGeneration else {
      throw identityFailure ?? CurrentHubError.invalidRequest(
        "discovery identity context changed; retry revalidation"
      )
    }
    discovery = value
    identityFailure = nil
    return value
  }

  /// An issued credential retained until its store save succeeds. This value and its
  /// explicit secret archive can be used to recover a failed save without issuing again.
  public func pendingCredentialForPersistence() -> CurrentHubCredential? {
    pendingIssuedCredential
  }

  /// Retries only local persistence; it never repeats the remote claim or rotation.
  @discardableResult
  public func retryCredentialPersistence() async throws -> CurrentHubCredential {
    guard !credentialMutationInProgress else {
      throw CurrentHubError.invalidRequest("credential issuance or persistence is already in progress")
    }
    guard let credential = pendingIssuedCredential else {
      throw CurrentHubError.invalidRequest("there is no issued credential awaiting persistence")
    }
    guard credential.expiresAtMilliseconds > nowMilliseconds() else {
      throw CurrentHubError.credentialExpired
    }
    credentialMutationInProgress = true
    defer { credentialMutationInProgress = false }
    try await credentialStore.saveCredential(credential)
    pendingIssuedCredential = nil
    return credential
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
    let context = requestContext()
    try requireTrustedIdentity(context)
    try validate(invitation: invitation)
    guard !deviceName.isEmpty, deviceName.utf8.count <= 65_536,
      deviceName.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) })
    else {
      throw CurrentHubError.invalidRequest("device name is empty, too long, or contains control characters")
    }
    let body = try JSONSerialization.data(
      withJSONObject: ["secret": invitation.secret, "device_name": deviceName],
      options: [.sortedKeys]
    )
    guard body.count <= 4_096 else {
      throw CurrentHubError.invalidRequest("pairing claim request exceeds 4096-byte profile bound")
    }
    var request = request(path: "/v1/pairings/\(invitation.pairingID.uuidString.lowercased())/claim")
    request.httpMethod = "POST"
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.httpBody = body
    guard let pinningTransport = transport as? any CurrentHubInvitationPinningTransport else {
      throw CurrentHubError.invalidRequest(
        "current-Hub claim transport must own invitation leaf pin validation"
      )
    }
    try beginCredentialMutation()
    defer { credentialMutationInProgress = false }
    let response = try await send(
      request,
      using: pinningTransport,
      validatingLeafCertificateSHA256: invitation.tlsPin,
      context: context
    )
    let envelope = try decodeSuccess(
      CurrentHubClaimEnvelope.self,
      response: response,
      operation: "pairing claim"
    )
    let credential = try envelope.credential()
    try requireFreshlyIssued(credential, response: response, operation: "pairing claim")
    try await persistIssuedCredential(credential)
    return credential
  }

  public func rotateCredential() async throws -> CurrentHubCredential {
    try requireTrustedIdentity(requestContext())
    try beginCredentialMutation()
    defer { credentialMutationInProgress = false }
    let context = requestContext()
    var request = request(path: "/v1/device/rotate")
    request.httpMethod = "POST"
    try await authorize(&request, context: context, allowingCredentialMutation: true)
    let response = try await send(request, context: context, allowingCredentialMutation: true)
    let envelope = try decodeSuccess(
      CurrentHubClaimEnvelope.self,
      response: response,
      operation: "credential rotation"
    )
    let credential = try envelope.credential()
    try requireFreshlyIssued(credential, response: response, operation: "credential rotation")
    try await persistIssuedCredential(credential)
    return credential
  }

  public func vehicles() async throws -> [CurrentHubVehicle] {
    let context = requestContext()
    var request = request(path: "/v1/vehicles")
    try await authorize(&request, context: context)
    let response = try await send(request, context: context)
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
    let context = requestContext()
    try validateVehicleID(vehicleID)
    var request = request(
      path: "/v1/vehicles/\(vehicleID.uuidString.lowercased())/current"
    )
    try await authorize(&request, context: context)
    let response = try await send(request, context: context)
    if response.statusCode == 200 {
      try requireObjectKeys(
        response,
        required: CurrentHubCurrentState.requiredWireKeys,
        nullableObjectKeys: ["car": CurrentHubProjectionCar.requiredWireKeys],
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
    let context = requestContext()
    try validateVehicleID(vehicleID)
    guard context.discovery.capabilities.contains("query.drives") else {
      throw CurrentHubError.capabilityUnavailable("query.drives")
    }
    let resolved = try resolve(query)
    var request = request(
      path: "/v1/vehicles/\(vehicleID.uuidString.lowercased())/drives",
      queryItems: resolved.items
    )
    try await authorize(&request, context: context)
    if let ifNoneMatch {
      request.setValue(ifNoneMatch.rawValue, forHTTPHeaderField: "If-None-Match")
    }
    let response = try await send(request, context: context)
    if response.statusCode == 304 {
      guard response.body.isEmpty else {
        throw invalid(response, reason: "304 drive response must have an empty body")
      }
      let eTag = try driveHeaders(response)
      guard let ifNoneMatch, eTag == ifNoneMatch else {
        throw invalid(response, reason: "304 drive response must match the sent strong ETag")
      }
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

  private func requestContext() -> RequestContext {
    RequestContext(
      discovery: discovery,
      identityGeneration: identityGeneration,
      credentialGeneration: credentialGeneration
    )
  }

  private func requireTrustedIdentity(_ context: RequestContext) throws {
    if let identityFailure { throw identityFailure }
    guard context.identityGeneration == identityGeneration else {
      throw CurrentHubError.invalidRequest("request identity context changed; retry after discovery revalidation")
    }
  }

  private func requireCredentialDispatch(
    _ context: RequestContext,
    allowingCredentialMutation: Bool
  ) throws {
    try requireTrustedIdentity(context)
    guard context.credentialGeneration == credentialGeneration else {
      throw CurrentHubError.invalidRequest("request credential context changed; retry with the stored credential")
    }
    guard pendingIssuedCredential == nil else {
      throw CurrentHubError.invalidRequest("issued credential requires retryCredentialPersistence before authenticated requests")
    }
    guard allowingCredentialMutation || !credentialMutationInProgress else {
      throw CurrentHubError.invalidRequest("credential issuance or persistence is already in progress")
    }
  }

  private func beginCredentialMutation() throws {
    guard !credentialMutationInProgress else {
      throw CurrentHubError.invalidRequest("credential issuance or persistence is already in progress")
    }
    if let credential = pendingIssuedCredential,
      credential.expiresAtMilliseconds <= nowMilliseconds()
    {
      pendingIssuedCredential = nil
    }
    guard pendingIssuedCredential == nil else {
      throw CurrentHubError.invalidRequest("issued credential requires retryCredentialPersistence before another claim or rotation")
    }
    credentialMutationInProgress = true
    credentialGeneration &+= 1
  }

  private func persistIssuedCredential(_ credential: CurrentHubCredential) async throws {
    pendingIssuedCredential = credential
    try await credentialStore.saveCredential(credential)
    pendingIssuedCredential = nil
  }

  private func authorize(
    _ request: inout URLRequest,
    context: RequestContext,
    allowingCredentialMutation: Bool = false
  ) async throws {
    try requireCredentialDispatch(context, allowingCredentialMutation: allowingCredentialMutation)
    guard let credential = try await credentialStore.loadCredential() else {
      throw CurrentHubError.unauthorized(requestID: nil)
    }
    try requireCredentialDispatch(context, allowingCredentialMutation: allowingCredentialMutation)
    guard credential.expiresAtMilliseconds > nowMilliseconds() else {
      throw CurrentHubError.credentialExpired
    }
    credential.apply(to: &request)
  }

  private func send(
    _ request: URLRequest,
    context: RequestContext? = nil,
    allowingCredentialMutation: Bool = false
  ) async throws -> CurrentHubHTTPResponse {
    if let context {
      try requireCredentialDispatch(context, allowingCredentialMutation: allowingCredentialMutation)
    }
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
    validatingLeafCertificateSHA256 expectedLeafCertificateSHA256: String,
    context: RequestContext
  ) async throws -> CurrentHubHTTPResponse {
    try requireTrustedIdentity(context)
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
    guard credential.expiresAtMilliseconds > nowMilliseconds() else {
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
    let invitationOrigin = try? CurrentHubEndpoint(invitation.endpoint).originURL
    guard invitation.pairingID != UUID.currentHubZero,
      invitationOrigin == endpoint.originURL,
      invitation.secret.utf8.count == 64,
      invitation.secret.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
      invitation.tlsPin.utf8.count == 64,
      invitation.tlsPin.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) })
    else {
      throw CurrentHubError.invalidRequest("pairing invitation identity, endpoint, secret, or TLS pin is invalid")
    }
    guard invitation.expiresAtMilliseconds > nowMilliseconds() else {
      throw CurrentHubError.invitationExpired
    }
    let pairingEndpoint = URLComponents(url: invitation.pairingURI, resolvingAgainstBaseURL: false)
      .flatMap { components in
        components.queryItems?.first(where: { $0.name == "endpoint" })?.value
      }
      .flatMap(URL.init(string:))
      .flatMap { try? CurrentHubEndpoint($0).originURL }
    guard let components = URLComponents(url: invitation.pairingURI, resolvingAgainstBaseURL: false),
      components.scheme == "teslatlas-hub", components.host == "pair",
      pairingEndpoint == endpoint.originURL,
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
    nullableObjectKeys: [String: Set<String>] = [:],
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
    for object in objects {
      for (key, nestedRequired) in nullableObjectKeys {
        if object[key] is NSNull { continue }
        guard let nested = object[key] as? [String: Any],
          nestedRequired.isSubset(of: Set(nested.keys))
        else {
          throw invalid(response, reason: "\(context) \(key) is missing required fields")
        }
      }
    }
  }

  private static let readinessReasons: Set<String> = [
    "catalogue_unavailable", "lifecycle_quarantined", "published_content_unservable",
    "collector_absent", "collector_stale", "collector_auth_terminal",
  ]

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

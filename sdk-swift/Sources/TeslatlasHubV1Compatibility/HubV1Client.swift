import Foundation

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

public actor HubV1Client {
  private let binding: HubV1WireBinding
  private let endpoint: HubV1Endpoint
  private let expectedHubID: UUID
  private let credential: HubV1BearerCredential
  private let transport: any HubV1HTTPTransport
  private var discovery: HubV1Discovery

  private init(
    binding: HubV1WireBinding,
    endpoint: HubV1Endpoint,
    expectedHubID: UUID,
    credential: HubV1BearerCredential,
    transport: any HubV1HTTPTransport,
    discovery: HubV1Discovery
  ) {
    self.binding = binding
    self.endpoint = endpoint
    self.expectedHubID = expectedHubID
    self.credential = credential
    self.transport = transport
    self.discovery = discovery
  }

  public static func connect(
    discoveryURL: URL,
    expectedHubID: UUID,
    credential: HubV1BearerCredential
  ) async throws -> HubV1Client {
    try await connectForTesting(
      discoveryURL: discoveryURL,
      expectedHubID: expectedHubID,
      credential: credential,
      transport: HubV1URLSessionTransport()
    )
  }

  static func connectForTesting(
    discoveryURL: URL,
    expectedHubID: UUID,
    credential: HubV1BearerCredential,
    transport: any HubV1HTTPTransport
  ) async throws -> HubV1Client {
    guard expectedHubID != UUID.zero else {
      throw HubV1Error.invalidRequest("expected Hub identity must not be nil UUID")
    }
    let binding = try HubV1BindingLoader.load()
    let endpoint = try HubV1Endpoint(
      discoveryURL: discoveryURL,
      discoveryPath: binding.discovery.path
    )
    let discovery = try await fetchDiscovery(
      binding: binding,
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      transport: transport
    )
    return HubV1Client(
      binding: binding,
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      credential: credential,
      transport: transport,
      discovery: discovery
    )
  }

  public func discoveryDocument() -> HubV1Discovery {
    discovery
  }

  @discardableResult
  public func refreshDiscovery() async throws -> HubV1Discovery {
    let refreshed = try await Self.fetchDiscovery(
      binding: binding,
      endpoint: endpoint,
      expectedHubID: expectedHubID,
      transport: transport
    )
    discovery = refreshed
    return refreshed
  }

  public func vehicles() async throws -> [HubV1Vehicle] {
    let route = try binding.route(for: .vehicles)
    let request = try authenticatedRequest(route: route)
    let response = try await send(request)
    try requireJSONSuccess(response, operation: .vehicles)
    try HubV1JSONShape.validateVehicleList(response.body, binding: binding)

    do {
      return try JSONDecoder().decode(
        HubV1VehicleListEnvelope.self,
        from: response.body
      ).vehicles
    } catch {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header(binding.responses.requestIDHeader),
        reason: "vehicle list does not match deployed-Hub v1.0.0"
      )
    }
  }

  public func currentState(vehicleID: UUID) async throws -> HubV1CurrentState {
    guard vehicleID != UUID.zero else {
      throw HubV1Error.invalidRequest("vehicle identity must not be nil UUID")
    }
    let route = try binding.route(for: .currentState)
    let request = try authenticatedRequest(route: route, vehicleID: vehicleID)
    let response = try await send(request)
    try requireJSONSuccess(response, operation: .currentState)
    try HubV1JSONShape.validateCurrentState(response.body, binding: binding)

    let state: HubV1CurrentState
    do {
      state = try JSONDecoder().decode(HubV1CurrentState.self, from: response.body)
    } catch {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header(binding.responses.requestIDHeader),
        reason: "current state does not match deployed-Hub v1.0.0"
      )
    }
    guard state.vehicleID == vehicleID else {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header(binding.responses.requestIDHeader),
        reason: "current-state vehicle identity does not match the request"
      )
    }
    return state
  }

  public func drives(
    vehicleID: UUID,
    query: HubV1DriveQuery = HubV1DriveQuery(),
    ifNoneMatch: HubV1EntityTag? = nil
  ) async throws -> HubV1DrivePageResult {
    guard vehicleID != UUID.zero else {
      throw HubV1Error.invalidRequest("vehicle identity must not be nil UUID")
    }
    let route = try binding.route(for: .drives)
    let resolved = try resolve(query)
    let request = try authenticatedRequest(
      route: route,
      vehicleID: vehicleID,
      queryItems: resolved.queryItems,
      ifNoneMatch: ifNoneMatch
    )
    let response = try await send(request)

    if response.statusCode == binding.responses.notModifiedStatus {
      guard response.body.isEmpty else {
        throw HubV1Error.invalidResponse(
          statusCode: binding.responses.notModifiedStatus,
          requestID: response.header(binding.responses.requestIDHeader),
          reason: "304 drive response must not contain a body"
        )
      }
      return .notModified(eTag: try requireStrongETag(response))
    }

    try requireJSONSuccess(response, operation: .drives)
    let eTag = try requireStrongETag(response)
    try HubV1JSONShape.validateDrivePage(response.body, binding: binding)

    let envelope: HubV1DrivePageEnvelope
    do {
      envelope = try JSONDecoder().decode(
        HubV1DrivePageEnvelope.self,
        from: response.body
      )
    } catch {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header(binding.responses.requestIDHeader),
        reason: "drive page does not match deployed-Hub v1.0.0"
      )
    }
    try validateDrivePage(
      envelope,
      vehicleID: vehicleID,
      query: resolved,
      response: response
    )

    let nextCursor: HubV1DriveCursor?
    if let rawCursor = envelope.nextCursor {
      nextCursor = try HubV1DriveCursor(rawValue: rawCursor)
    } else {
      nextCursor = nil
    }
    return .modified(
      HubV1DrivePage(items: envelope.items, nextCursor: nextCursor),
      eTag: eTag
    )
  }

  private static func fetchDiscovery(
    binding: HubV1WireBinding,
    endpoint: HubV1Endpoint,
    expectedHubID: UUID,
    transport: any HubV1HTTPTransport
  ) async throws -> HubV1Discovery {
    var request = URLRequest(url: endpoint.discoveryURL)
    request.httpMethod = binding.discovery.method
    request.setValue(binding.responses.jsonMediaType, forHTTPHeaderField: "Accept")
    request.setValue("no-store", forHTTPHeaderField: "Cache-Control")

    guard request.value(forHTTPHeaderField: binding.authentication.header) == nil else {
      throw HubV1Error.invalidBinding("discovery request unexpectedly contains credentials")
    }

    let response = try await transport.send(request)
    try endpoint.validate(response.finalURL, expected: endpoint.discoveryURL)
    let requestID = response.header(binding.responses.requestIDHeader)
    guard response.statusCode == binding.responses.successStatus else {
      if response.statusCode == binding.responses.serviceUnavailableStatus {
        throw HubV1Error.serviceUnavailable(requestID: requestID)
      }
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: requestID,
        reason: "discovery returned an unexpected status"
      )
    }
    try requireJSONContentType(response, binding: binding)
    try HubV1JSONShape.validateDiscovery(response.body, binding: binding)

    let document: HubV1Discovery
    do {
      document = try JSONDecoder().decode(HubV1Discovery.self, from: response.body)
    } catch {
      throw HubV1Error.invalidDiscovery(
        "document does not match deployed-Hub v1.0.0 discovery"
      )
    }
    try validateDiscovery(
      document,
      rawBody: response.body,
      expectedHubID: expectedHubID,
      binding: binding
    )
    return document
  }

  private static func validateDiscovery(
    _ document: HubV1Discovery,
    rawBody: Data,
    expectedHubID: UUID,
    binding: HubV1WireBinding
  ) throws {
    guard document.hubID == expectedHubID else {
      throw HubV1Error.hubIdentityMismatch(
        expected: expectedHubID,
        actual: document.hubID
      )
    }
    guard document.protocolName == binding.discovery.protocolName,
      document.protocolMajor == binding.discovery.protocolMajor,
      document.apiVersions == binding.discovery.apiVersions,
      document.version == binding.discovery.hubVersion,
      document.sourceURL.absoluteString == binding.discovery.sourceURL,
      document.packFormat == binding.discovery.packFormat
    else {
      throw HubV1Error.invalidDiscovery(
        "protocol, API version, Hub version, source, or pack format changed"
      )
    }

    guard Set(document.capabilities).count == document.capabilities.count,
      binding.discovery.allowedCapabilitySets.contains(document.capabilities)
    else {
      throw HubV1Error.invalidDiscovery(
        "capability set does not match deployed-Hub v1.0.0"
      )
    }

    let fullSet = binding.discovery.allowedCapabilitySets.last
    if document.capabilities == fullSet {
      guard let key = document.manifestPublicKey,
        key.count == 64,
        key.utf8.allSatisfy({ byte in
          (byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102)
        })
      else {
        throw HubV1Error.invalidDiscovery(
          "full capability set requires a lowercase Ed25519 public key"
        )
      }
    } else if document.manifestPublicKey != nil {
      throw HubV1Error.invalidDiscovery(
        "base capability set must not advertise a manifest public key"
      )
    }

    guard let root = try? JSONSerialization.jsonObject(with: rawBody) as? [String: Any],
      let rawHubID = root["hub_id"] as? String,
      rawHubID == document.hubID.uuidString.lowercased()
    else {
      throw HubV1Error.invalidDiscovery("Hub identity is not canonical lowercase UUID")
    }
  }

  private func authenticatedRequest(
    route: HubV1WireBinding.Route,
    vehicleID: UUID? = nil,
    queryItems: [URLQueryItem] = [],
    ifNoneMatch: HubV1EntityTag? = nil
  ) throws -> URLRequest {
    guard discovery.capabilities.contains(route.requiredCapability) else {
      throw HubV1Error.capabilityUnavailable(route.requiredCapability)
    }

    var path = route.pathTemplate
    if path.contains("{vehicle_id}") {
      guard let vehicleID else {
        throw HubV1Error.invalidBinding("route requires vehicle identity")
      }
      path = path.replacingOccurrences(
        of: "{vehicle_id}",
        with: vehicleID.uuidString.lowercased()
      )
    }
    guard !path.contains("{") && !path.contains("}") else {
      throw HubV1Error.invalidBinding("route contains unresolved parameters")
    }

    let url = try endpoint.makeURL(path: path, queryItems: queryItems)
    try endpoint.validate(url, expectedPath: path)

    var request = URLRequest(url: url)
    request.httpMethod = route.method
    request.setValue(binding.responses.jsonMediaType, forHTTPHeaderField: "Accept")
    request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
    if let ifNoneMatch {
      request.setValue(
        ifNoneMatch.rawValue,
        forHTTPHeaderField: binding.drivePagination.conditionalRequestHeader
      )
    }

    // Deliberately last: every binding, capability, identity, path and origin
    // check above must finish before the bearer can enter an HTTP request.
    credential.apply(
      to: &request,
      header: binding.authentication.header,
      scheme: binding.authentication.scheme
    )
    return request
  }

  private func send(_ request: URLRequest) async throws -> HubV1HTTPResponse {
    guard let requestURL = request.url else {
      throw HubV1Error.invalidRequest("request URL is unavailable")
    }
    let response = try await transport.send(request)
    try endpoint.validate(response.finalURL, expected: requestURL)
    return response
  }

  private func resolve(_ query: HubV1DriveQuery) throws -> HubV1ResolvedDriveQuery {
    let rules = binding.drivePagination
    let from = query.fromMilliseconds ?? rules.fromDefault
    let to = query.toMilliseconds ?? rules.toDefault
    let limit = query.limit ?? rules.limitDefault

    guard from >= rules.fromMinimum, to >= rules.toMinimum, from < to else {
      throw HubV1Error.invalidRequest(
        "drive time range must be nonnegative with from_ms earlier than to_ms"
      )
    }
    guard limit >= rules.limitMinimum, limit <= rules.limitMaximum else {
      throw HubV1Error.invalidRequest(
        "drive limit must be between \(rules.limitMinimum) and \(rules.limitMaximum)"
      )
    }

    var items = [
      URLQueryItem(name: rules.fromParameter, value: String(from)),
      URLQueryItem(name: rules.toParameter, value: String(to)),
      URLQueryItem(name: rules.limitParameter, value: String(limit)),
    ]
    if let cursor = query.cursor {
      items.append(
        URLQueryItem(name: rules.cursorParameter, value: cursor.rawValue)
      )
    }
    return HubV1ResolvedDriveQuery(
      fromMilliseconds: from,
      toMilliseconds: to,
      limit: limit,
      queryItems: items
    )
  }

  private func validateDrivePage(
    _ envelope: HubV1DrivePageEnvelope,
    vehicleID: UUID,
    query: HubV1ResolvedDriveQuery,
    response: HubV1HTTPResponse
  ) throws {
    let requestID = response.header(binding.responses.requestIDHeader)
    guard envelope.items.count <= query.limit else {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: requestID,
        reason: "drive page exceeds the requested limit"
      )
    }
    guard envelope.items.allSatisfy({ $0.vehicleID == vehicleID }) else {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: requestID,
        reason: "drive page contains a different vehicle identity"
      )
    }
    guard
      envelope.items.allSatisfy({ drive in
        drive.startDateMilliseconds >= query.fromMilliseconds
          && drive.startDateMilliseconds < query.toMilliseconds
      })
    else {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: requestID,
        reason: "drive page contains an item outside the requested time range"
      )
    }
    guard
      zip(envelope.items, envelope.items.dropFirst()).allSatisfy({ newer, older in
        newer.startDateMilliseconds > older.startDateMilliseconds
          || (newer.startDateMilliseconds == older.startDateMilliseconds
            && newer.id > older.id)
      })
    else {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: requestID,
        reason: "drive page is not ordered by the deployed pagination contract"
      )
    }
  }

  private func requireJSONSuccess(
    _ response: HubV1HTTPResponse,
    operation: HubV1Operation
  ) throws {
    guard response.statusCode == binding.responses.successStatus else {
      throw try responseError(response, operation: operation)
    }
    try Self.requireJSONContentType(response, binding: binding)
  }

  private func responseError(
    _ response: HubV1HTTPResponse,
    operation: HubV1Operation
  ) throws -> HubV1Error {
    let requestID = response.header(binding.responses.requestIDHeader)
    if response.statusCode == binding.responses.unauthorizedStatus {
      guard
        response.header(binding.responses.unauthorizedChallengeHeader)
          == binding.responses.unauthorizedChallengeValue
      else {
        return .invalidResponse(
          statusCode: response.statusCode,
          requestID: requestID,
          reason: "unauthorized response is missing the deployed bearer challenge"
        )
      }
      return .unauthorized(requestID: requestID)
    }

    if operation == .drives, !response.body.isEmpty,
      Self.isJSONContentType(
        response.header(binding.responses.contentTypeHeader),
        expected: binding.responses.jsonMediaType
      )
    {
      do {
        try HubV1JSONShape.validateAPIError(response.body, binding: binding)
        let envelope = try JSONDecoder().decode(
          HubV1APIErrorEnvelope.self,
          from: response.body
        )
        guard binding.stableDriveErrorCodes.contains(envelope.error.code) else {
          return .invalidResponse(
            statusCode: response.statusCode,
            requestID: requestID,
            reason: "drive error code is outside the deployed binding"
          )
        }
        guard
          binding.responses.driveErrorStatusByCode[envelope.error.code]
            == response.statusCode
        else {
          return .invalidResponse(
            statusCode: response.statusCode,
            requestID: requestID,
            reason: "drive error status does not match its deployed error code"
          )
        }
        return .api(
          statusCode: response.statusCode,
          code: envelope.error.code,
          message: envelope.error.message,
          requestID: requestID
        )
      } catch let error as HubV1Error {
        return error
      } catch {
        return .invalidResponse(
          statusCode: response.statusCode,
          requestID: requestID,
          reason: "drive error body does not match the deployed binding"
        )
      }
    }

    switch response.statusCode {
    case binding.responses.notFoundStatus:
      return .notFound(requestID: requestID)
    case binding.responses.serviceUnavailableStatus:
      return .serviceUnavailable(requestID: requestID)
    default:
      return .invalidResponse(
        statusCode: response.statusCode,
        requestID: requestID,
        reason: "unexpected HTTP status"
      )
    }
  }

  private func requireStrongETag(_ response: HubV1HTTPResponse) throws
    -> HubV1EntityTag
  {
    guard let raw = response.header(binding.responses.entityTagHeader) else {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header(binding.responses.requestIDHeader),
        reason: "drive response is missing its strong ETag"
      )
    }
    do {
      return try HubV1EntityTag(rawValue: raw)
    } catch {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header(binding.responses.requestIDHeader),
        reason: "drive response ETag is invalid or weak"
      )
    }
  }

  private static func requireJSONContentType(
    _ response: HubV1HTTPResponse,
    binding: HubV1WireBinding
  ) throws {
    guard
      isJSONContentType(
        response.header(binding.responses.contentTypeHeader),
        expected: binding.responses.jsonMediaType
      )
    else {
      throw HubV1Error.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header(binding.responses.requestIDHeader),
        reason: "response Content-Type is not application/json"
      )
    }
  }

  private static func isJSONContentType(_ value: String?, expected: String) -> Bool {
    value?
      .split(separator: ";", maxSplits: 1)
      .first?
      .trimmingCharacters(in: .whitespacesAndNewlines)
      .lowercased() == expected.lowercased()
  }
}

private struct HubV1ResolvedDriveQuery: Sendable {
  let fromMilliseconds: Int64
  let toMilliseconds: Int64
  let limit: Int
  let queryItems: [URLQueryItem]
}

private struct HubV1Endpoint: Sendable {
  struct Origin: Equatable, Sendable, CustomStringConvertible {
    let scheme: String
    let host: String
    let port: Int?

    init(url: URL) throws {
      guard
        let components = URLComponents(
          url: url,
          resolvingAgainstBaseURL: false
        ), let rawScheme = components.scheme, let rawHost = components.host,
        !rawHost.isEmpty
      else {
        throw HubV1Error.invalidDiscoveryURL("URL must contain an absolute origin")
      }
      let scheme = rawScheme.lowercased()
      let host = rawHost.lowercased()
      guard scheme == "https" || (scheme == "http" && Self.isLoopback(host)) else {
        throw HubV1Error.invalidDiscoveryURL(
          "non-loopback Hub discovery must use HTTPS"
        )
      }
      self.scheme = scheme
      self.host = host
      let defaultPort = scheme == "https" ? 443 : 80
      port = components.port == defaultPort ? nil : components.port
    }

    var description: String {
      if let port { return "\(scheme)://\(host):\(port)" }
      return "\(scheme)://\(host)"
    }

    private static func isLoopback(_ host: String) -> Bool {
      if host == "localhost" || host == "::1" || host == "[::1]" {
        return true
      }
      let octets = host.split(separator: ".", omittingEmptySubsequences: false)
      guard octets.count == 4,
        let first = UInt8(octets[0]), first == 127,
        octets.dropFirst().allSatisfy({ UInt8($0) != nil })
      else {
        return false
      }
      return true
    }
  }

  let discoveryURL: URL
  let origin: Origin

  init(discoveryURL: URL, discoveryPath: String) throws {
    guard
      let components = URLComponents(
        url: discoveryURL,
        resolvingAgainstBaseURL: false
      ), components.user == nil, components.password == nil,
      components.query == nil, components.fragment == nil,
      components.percentEncodedPath == discoveryPath
    else {
      throw HubV1Error.invalidDiscoveryURL(
        "discovery URL must be the exact binding path without credentials, query or fragment"
      )
    }
    origin = try Origin(url: discoveryURL)
    self.discoveryURL = try Self.buildURL(
      origin: origin,
      path: discoveryPath,
      queryItems: []
    )
  }

  func makeURL(path: String, queryItems: [URLQueryItem]) throws -> URL {
    try Self.buildURL(origin: origin, path: path, queryItems: queryItems)
  }

  func validate(_ url: URL, expected: URL) throws {
    let actualOrigin = try Origin(url: url)
    guard actualOrigin == origin else {
      throw HubV1Error.untrustedOrigin(actualOrigin.description)
    }
    guard Self.normalisedComponents(url) == Self.normalisedComponents(expected) else {
      throw HubV1Error.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "transport changed the validated request URL"
      )
    }
  }

  func validate(_ url: URL, expectedPath: String) throws {
    let actualOrigin = try Origin(url: url)
    guard actualOrigin == origin else {
      throw HubV1Error.untrustedOrigin(actualOrigin.description)
    }
    guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
      components.user == nil, components.password == nil,
      components.fragment == nil, components.percentEncodedPath == expectedPath
    else {
      throw HubV1Error.invalidRequest("route URL no longer matches the binding")
    }
  }

  private static func buildURL(
    origin: Origin,
    path: String,
    queryItems: [URLQueryItem]
  ) throws -> URL {
    guard path.hasPrefix("/"), !path.contains("?"), !path.contains("#"),
      !path.contains("..")
    else {
      throw HubV1Error.invalidBinding("route path is not an absolute safe path")
    }
    var components = URLComponents()
    components.scheme = origin.scheme
    components.host = origin.host
    components.port = origin.port
    components.percentEncodedPath = path
    components.queryItems = queryItems.isEmpty ? nil : queryItems
    guard let url = components.url else {
      throw HubV1Error.invalidBinding("route cannot be represented as a URL")
    }
    return url
  }

  private static func normalisedComponents(_ url: URL) -> String? {
    guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false),
      let scheme = components.scheme?.lowercased(),
      let host = components.host?.lowercased()
    else { return nil }
    components.scheme = scheme
    components.host = host
    if (scheme == "https" && components.port == 443)
      || (scheme == "http" && components.port == 80)
    {
      components.port = nil
    }
    return components.string
  }
}

private enum HubV1JSONShape {
  static func validateDiscovery(
    _ data: Data,
    binding: HubV1WireBinding
  ) throws {
    let root = try rootObject(data, context: "discovery")
    try validate(
      root,
      model: HubV1WireBinding.Model(
        requiredFields: binding.discovery.requiredFields,
        optionalFields: binding.discovery.optionalFields
      ), context: "discovery")
  }

  static func validateVehicleList(
    _ data: Data,
    binding: HubV1WireBinding
  ) throws {
    let root = try rootObject(data, context: "vehicle list")
    try validate(root, model: try binding.model(named: "vehicle_list"), context: "vehicle list")
    guard let vehicles = root["vehicles"] as? [Any] else {
      throw HubV1Error.invalidResponse(
        statusCode: 200,
        requestID: nil,
        reason: "vehicle list vehicles field is not an array"
      )
    }
    for (index, value) in vehicles.enumerated() {
      guard let object = value as? [String: Any] else {
        throw HubV1Error.invalidResponse(
          statusCode: 200,
          requestID: nil,
          reason: "vehicle list item \(index) is not an object"
        )
      }
      try validate(
        object,
        model: try binding.model(named: "vehicle"),
        context: "vehicle list item"
      )
    }
  }

  static func validateCurrentState(
    _ data: Data,
    binding: HubV1WireBinding
  ) throws {
    let root = try rootObject(data, context: "current state")
    try validate(root, model: try binding.model(named: "current_state"), context: "current state")
    if let car = root["car"], !(car is NSNull) {
      guard let carObject = car as? [String: Any] else {
        throw HubV1Error.invalidResponse(
          statusCode: 200,
          requestID: nil,
          reason: "current-state car field is not an object"
        )
      }
      try validate(
        carObject,
        model: try binding.model(named: "projection_car"),
        context: "current-state car"
      )
      guard let settings = carObject["settings"] as? [String: Any] else {
        throw HubV1Error.invalidResponse(
          statusCode: 200,
          requestID: nil,
          reason: "current-state car settings are not an object"
        )
      }
      try validate(
        settings,
        model: try binding.model(named: "projection_car_settings"),
        context: "current-state car settings"
      )
    }
  }

  static func validateDrivePage(
    _ data: Data,
    binding: HubV1WireBinding
  ) throws {
    let root = try rootObject(data, context: "drive page")
    try validate(root, model: try binding.model(named: "drive_page"), context: "drive page")
    guard let items = root["items"] as? [Any] else {
      throw HubV1Error.invalidResponse(
        statusCode: 200,
        requestID: nil,
        reason: "drive page items field is not an array"
      )
    }
    for (index, value) in items.enumerated() {
      guard let object = value as? [String: Any] else {
        throw HubV1Error.invalidResponse(
          statusCode: 200,
          requestID: nil,
          reason: "drive page item \(index) is not an object"
        )
      }
      try validate(
        object,
        model: try binding.model(named: "drive"),
        context: "drive page item"
      )
    }
  }

  static func validateAPIError(
    _ data: Data,
    binding: HubV1WireBinding
  ) throws {
    let root = try rootObject(data, context: "API error")
    try validate(root, model: try binding.model(named: "error_envelope"), context: "API error")
    guard let error = root["error"] as? [String: Any] else {
      throw HubV1Error.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "API error field is not an object"
      )
    }
    try validate(error, model: try binding.model(named: "error"), context: "API error body")
  }

  private static func rootObject(_ data: Data, context: String) throws
    -> [String: Any]
  {
    let value: Any
    do {
      value = try JSONSerialization.jsonObject(with: data)
    } catch {
      throw HubV1Error.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "\(context) is not valid JSON"
      )
    }
    guard let object = value as? [String: Any] else {
      throw HubV1Error.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "\(context) root is not an object"
      )
    }
    return object
  }

  private static func validate(
    _ object: [String: Any],
    model: HubV1WireBinding.Model,
    context: String
  ) throws {
    let actual = Set(object.keys)
    let required = Set(model.requiredFields)
    let allowed = required.union(model.optionalFields)
    guard required.isSubset(of: actual), actual.isSubset(of: allowed) else {
      throw HubV1Error.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "\(context) fields do not match the deployed binding"
      )
    }
  }
}

extension UUID {
  fileprivate static let zero = UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
}

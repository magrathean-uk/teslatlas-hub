import Foundation

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

public actor TeslatlasClient {
  public private(set) var discovery: HubDiscoveryDocument
  public private(set) var selectedProtocolVersion: TeslatlasProtocolVersion
  public let endpointTrustPolicy: HubEndpointTrustPolicy

  private let maximumProtocolVersion: TeslatlasProtocolVersion
  private let authorization: any TeslatlasAuthorization
  private let transport: any TeslatlasHTTPTransport
  private let maximumResponseBytes =
    URLSessionTeslatlasTransport.defaultMaximumResponseBytes

  private init(
    discovery: HubDiscoveryDocument,
    selectedProtocolVersion: TeslatlasProtocolVersion,
    maximumProtocolVersion: TeslatlasProtocolVersion,
    endpointTrustPolicy: HubEndpointTrustPolicy,
    authorization: any TeslatlasAuthorization,
    transport: any TeslatlasHTTPTransport
  ) {
    self.discovery = discovery
    self.selectedProtocolVersion = selectedProtocolVersion
    self.maximumProtocolVersion = maximumProtocolVersion
    self.endpointTrustPolicy = endpointTrustPolicy
    self.authorization = authorization
    self.transport = transport
  }

  public static func connect(
    discoveryURL: URL,
    maximumProtocolVersion: TeslatlasProtocolVersion,
    additionalTrustedEndpointOrigins: [URL] = [],
    authorization: any TeslatlasAuthorization,
    transport: any TeslatlasHTTPTransport = URLSessionTeslatlasTransport()
  ) async throws -> TeslatlasClient {
    let endpointTrustPolicy = try HubEndpointTrustPolicy(
      discoveryURL: discoveryURL,
      additionalTrustedOrigins: additionalTrustedEndpointOrigins
    )
    let discovery = try await fetchDiscovery(
      from: discoveryURL,
      transport: transport
    )
    try endpointTrustPolicy.validate(discovery)
    let selected = try discovery.protocolInfo.negotiate(
      maximum: maximumProtocolVersion
    )
    return TeslatlasClient(
      discovery: discovery,
      selectedProtocolVersion: selected,
      maximumProtocolVersion: maximumProtocolVersion,
      endpointTrustPolicy: endpointTrustPolicy,
      authorization: authorization,
      transport: transport
    )
  }

  public func refreshDiscovery(from url: URL) async throws {
    try endpointTrustPolicy.validate(url)
    let refreshed = try await Self.fetchDiscovery(from: url, transport: transport)
    guard refreshed.hubID == discovery.hubID else {
      throw TeslatlasDiscoveryError.hubIdentityChanged(
        expected: discovery.hubID,
        actual: refreshed.hubID
      )
    }
    try endpointTrustPolicy.validate(refreshed)
    let selected = try refreshed.protocolInfo.negotiate(
      maximum: maximumProtocolVersion
    )
    discovery = refreshed
    selectedProtocolVersion = selected
  }

  public func vehicles(
    page: PageRequest = PageRequest(),
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<VehiclePage> {
    guard discovery.capability("query.vehicles") != nil else {
      throw TeslatlasSDKError.capabilityUnavailable("query.vehicles")
    }
    if let limit = page.limit, limit > discovery.limits.maximumPageSize {
      throw TeslatlasSDKError.limitExceeded(
        name: "limit",
        maximum: discovery.limits.maximumPageSize,
        actual: limit
      )
    }
    if let limit = page.limit, limit < 1 {
      throw TeslatlasSDKError.limitExceeded(
        name: "limit",
        maximum: discovery.limits.maximumPageSize,
        actual: limit
      )
    }

    var components = URLComponents(
      url: discovery.endpoints.api.appendingPathComponent("vehicles"),
      resolvingAgainstBaseURL: false
    )
    var queryItems: [URLQueryItem] = []
    if let cursor = page.cursor {
      queryItems.append(URLQueryItem(name: "cursor", value: cursor.rawValue))
    }
    if let limit = page.limit {
      queryItems.append(URLQueryItem(name: "limit", value: String(limit)))
    }
    components?.queryItems = queryItems.isEmpty ? nil : queryItems
    guard let url = components?.url else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "could not build vehicles URL"
      )
    }
    return try await get(url, ifNoneMatch: ifNoneMatch, as: VehiclePage.self)
  }

  public func currentState(
    vehicleID: String,
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<VehicleCurrentState> {
    guard discovery.capability("query.vehicles") != nil else {
      throw TeslatlasSDKError.capabilityUnavailable("query.vehicles")
    }
    let url = discovery.endpoints.api
      .appendingPathComponent("vehicles")
      .appendingPathComponent(vehicleID)
      .appendingPathComponent("current")
    return try await get(
      url,
      ifNoneMatch: ifNoneMatch,
      as: VehicleCurrentState.self
    )
  }

  public func drives(
    vehicleID: String,
    request: HistoryRequest = HistoryRequest(),
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<DrivePage> {
    try requireCapability("query.history")
    let url = try historyURL(
      discovery.endpoints.api
        .appendingPathComponent("vehicles")
        .appendingPathComponent(vehicleID)
        .appendingPathComponent("drives"),
      request: request,
      maximumDays: discovery.limits.maximumHistoryRangeDays
    )
    return try await get(url, ifNoneMatch: ifNoneMatch, as: DrivePage.self)
  }

  public func drive(
    id: String,
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<Drive> {
    try requireCapability("query.history")
    let url = discovery.endpoints.api
      .appendingPathComponent("drives")
      .appendingPathComponent(id)
    return try await get(url, ifNoneMatch: ifNoneMatch, as: Drive.self)
  }

  public func positions(
    driveID: String,
    request: HistoryRequest = HistoryRequest(),
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<PositionPage> {
    try requireCapability("query.history")
    let url = try historyURL(
      discovery.endpoints.api
        .appendingPathComponent("drives")
        .appendingPathComponent(driveID)
        .appendingPathComponent("positions"),
      request: request,
      maximumDays: discovery.limits.maximumDenseRangeDays
    )
    return try await get(url, ifNoneMatch: ifNoneMatch, as: PositionPage.self)
  }

  public func charges(
    vehicleID: String,
    request: HistoryRequest = HistoryRequest(),
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<ChargePage> {
    try requireCapability("query.history")
    let url = try historyURL(
      discovery.endpoints.api
        .appendingPathComponent("vehicles")
        .appendingPathComponent(vehicleID)
        .appendingPathComponent("charges"),
      request: request,
      maximumDays: discovery.limits.maximumHistoryRangeDays
    )
    return try await get(url, ifNoneMatch: ifNoneMatch, as: ChargePage.self)
  }

  public func charge(
    id: String,
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<Charge> {
    try requireCapability("query.history")
    let url = discovery.endpoints.api
      .appendingPathComponent("charges")
      .appendingPathComponent(id)
    return try await get(url, ifNoneMatch: ifNoneMatch, as: Charge.self)
  }

  public func chargeSamples(
    chargeID: String,
    request: HistoryRequest = HistoryRequest(),
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<ChargeSamplePage> {
    try requireCapability("query.history")
    let url = try historyURL(
      discovery.endpoints.api
        .appendingPathComponent("charges")
        .appendingPathComponent(chargeID)
        .appendingPathComponent("samples"),
      request: request,
      maximumDays: discovery.limits.maximumDenseRangeDays
    )
    return try await get(
      url,
      ifNoneMatch: ifNoneMatch,
      as: ChargeSamplePage.self
    )
  }

  public func states(
    vehicleID: String,
    request: HistoryRequest = HistoryRequest(),
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<StatePage> {
    try requireCapability("query.history")
    let url = try historyURL(
      discovery.endpoints.api
        .appendingPathComponent("vehicles")
        .appendingPathComponent(vehicleID)
        .appendingPathComponent("states"),
      request: request,
      maximumDays: discovery.limits.maximumHistoryRangeDays
    )
    return try await get(url, ifNoneMatch: ifNoneMatch, as: StatePage.self)
  }

  public func softwareUpdates(
    vehicleID: String,
    request: HistoryRequest = HistoryRequest(),
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<SoftwareUpdatePage> {
    try requireCapability("query.history")
    let url = try historyURL(
      discovery.endpoints.api
        .appendingPathComponent("vehicles")
        .appendingPathComponent(vehicleID)
        .appendingPathComponent("updates"),
      request: request,
      maximumDays: discovery.limits.maximumHistoryRangeDays
    )
    return try await get(
      url,
      ifNoneMatch: ifNoneMatch,
      as: SoftwareUpdatePage.self
    )
  }

  public func dataQuality(
    request: DataQualityRequest = DataQualityRequest(),
    ifNoneMatch: EntityTag? = nil
  ) async throws -> TeslatlasConditionalResponse<DataQualityPage> {
    try requireCapability("data-quality")
    let history = HistoryRequest(
      from: request.from,
      to: request.to,
      cursor: request.cursor,
      limit: request.limit
    )
    let baseURL = discovery.endpoints.api.appendingPathComponent("data-quality")
    let historyItems = try historyQueryItems(
      history,
      maximumDays: discovery.limits.maximumHistoryRangeDays
    )
    var queryItems: [URLQueryItem] = []
    if let vehicleID = request.vehicleID {
      queryItems.append(URLQueryItem(name: "vehicle_id", value: vehicleID))
    }
    queryItems.append(contentsOf: historyItems)
    let url = try url(baseURL, queryItems: queryItems)
    return try await get(url, ifNoneMatch: ifNoneMatch, as: DataQualityPage.self)
  }

  public func eventRequest(
    lastEventID: String? = nil,
    vehicleID: String? = nil,
    eventTypes: [String] = []
  ) throws -> URLRequest {
    try requireCapability("events.sse")
    guard eventTypes.count <= 32 else {
      throw TeslatlasSDKError.limitExceeded(
        name: "event_type",
        maximum: 32,
        actual: eventTypes.count
      )
    }
    guard Set(eventTypes).count == eventTypes.count else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "event types must be unique"
      )
    }

    var queryItems: [URLQueryItem] = []
    if let vehicleID {
      queryItems.append(URLQueryItem(name: "vehicle_id", value: vehicleID))
    }
    queryItems.append(
      contentsOf: eventTypes.map {
        URLQueryItem(name: "event_type", value: $0)
      }
    )
    let streamURL = try url(discovery.endpoints.events, queryItems: queryItems)
    var request = URLRequest(url: streamURL)
    request.httpMethod = "GET"
    request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
    request.setValue(
      selectedProtocolVersion.description,
      forHTTPHeaderField: "Teslatlas-Protocol-Version"
    )
    if let lastEventID, !lastEventID.isEmpty {
      request.setValue(lastEventID, forHTTPHeaderField: "Last-Event-ID")
    }
    authorization.apply(to: &request)
    return request
  }

  private func requireCapability(_ id: String) throws {
    guard discovery.capability(id) != nil else {
      throw TeslatlasSDKError.capabilityUnavailable(id)
    }
  }

  private func historyURL(
    _ baseURL: URL,
    request: HistoryRequest,
    maximumDays: Int
  ) throws -> URL {
    try url(
      baseURL,
      queryItems: historyQueryItems(request, maximumDays: maximumDays)
    )
  }

  private func historyQueryItems(
    _ request: HistoryRequest,
    maximumDays: Int
  ) throws -> [URLQueryItem] {
    if let limit = request.limit {
      guard (1...discovery.limits.maximumPageSize).contains(limit) else {
        throw TeslatlasSDKError.limitExceeded(
          name: "limit",
          maximum: discovery.limits.maximumPageSize,
          actual: limit
        )
      }
    }
    if let from = request.from, let to = request.to {
      let rangeDays = try Self.rangeDays(from: from, to: to)
      guard rangeDays <= maximumDays else {
        throw TeslatlasSDKError.limitExceeded(
          name: "history_range_days",
          maximum: maximumDays,
          actual: rangeDays
        )
      }
    }

    var items: [URLQueryItem] = []
    if let from = request.from {
      items.append(URLQueryItem(name: "from", value: from.rawValue))
    }
    if let to = request.to {
      items.append(URLQueryItem(name: "to", value: to.rawValue))
    }
    if let cursor = request.cursor {
      items.append(URLQueryItem(name: "cursor", value: cursor.rawValue))
    }
    if let limit = request.limit {
      items.append(URLQueryItem(name: "limit", value: String(limit)))
    }
    return items
  }

  private func url(_ baseURL: URL, queryItems: [URLQueryItem]) throws -> URL {
    guard !queryItems.isEmpty else { return baseURL }
    var components = URLComponents(
      url: baseURL,
      resolvingAgainstBaseURL: false
    )
    components?.queryItems = queryItems
    guard let result = components?.url else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "could not build query URL"
      )
    }
    return result
  }

  private static func rangeDays(
    from: TeslatlasTimestamp,
    to: TeslatlasTimestamp
  ) throws -> Int {
    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    guard let start = formatter.date(from: from.rawValue),
      let end = formatter.date(from: to.rawValue),
      end > start
    else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: 0,
        requestID: nil,
        reason: "from must be earlier than to"
      )
    }
    return Int(ceil(end.timeIntervalSince(start) / 86_400))
  }

  private func get<Value: Decodable & Sendable>(
    _ url: URL,
    ifNoneMatch: EntityTag?,
    as type: Value.Type
  ) async throws -> TeslatlasConditionalResponse<Value> {
    var request = URLRequest(url: url)
    request.httpMethod = "GET"
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    request.setValue(
      selectedProtocolVersion.description,
      forHTTPHeaderField: "Teslatlas-Protocol-Version"
    )
    if let ifNoneMatch {
      request.setValue(ifNoneMatch.rawValue, forHTTPHeaderField: "If-None-Match")
    }
    authorization.apply(to: &request)

    let response = try await transport.send(request)
    return try Self.decodeConditional(
      response,
      selectedProtocolVersion: selectedProtocolVersion,
      maximumResponseBytes: maximumResponseBytes,
      as: type
    )
  }

  private static func fetchDiscovery(
    from url: URL,
    transport: any TeslatlasHTTPTransport
  ) async throws -> HubDiscoveryDocument {
    var request = URLRequest(url: url)
    request.httpMethod = "GET"
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    let response = try await transport.send(request)

    guard response.statusCode == 200 else {
      throw try decodeFailure(response)
    }
    try validateJSONResponseHeaders(response, requiresVersion: false)
    return try HubDiscoveryDecoder.decode(response.body)
  }

  private static func decodeConditional<Value: Decodable & Sendable>(
    _ response: TeslatlasHTTPResponse,
    selectedProtocolVersion: TeslatlasProtocolVersion,
    maximumResponseBytes: Int,
    as type: Value.Type
  ) throws -> TeslatlasConditionalResponse<Value> {
    guard response.body.count <= maximumResponseBytes else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "response body exceeds \(maximumResponseBytes) bytes"
      )
    }

    switch response.statusCode {
    case 200:
      try validateJSONResponseHeaders(response, requiresVersion: true)
      try validateSelectedVersion(response, expected: selectedProtocolVersion)
      let eTag = try requiredEntityTag(response)
      do {
        let value = try JSONDecoder().decode(type, from: response.body)
        return .modified(value, eTag)
      } catch {
        throw TeslatlasSDKError.invalidResponse(
          statusCode: response.statusCode,
          requestID: response.header("X-Request-ID"),
          reason: "response body does not match the selected protocol schema"
        )
      }
    case 304:
      guard response.body.isEmpty else {
        throw TeslatlasSDKError.invalidResponse(
          statusCode: 304,
          requestID: response.header("X-Request-ID"),
          reason: "304 response must not contain a body"
        )
      }
      try validateCacheHeaders(response)
      try validateSelectedVersion(response, expected: selectedProtocolVersion)
      return .notModified(try requiredEntityTag(response))
    default:
      throw try decodeFailure(response)
    }
  }

  private static func validateJSONResponseHeaders(
    _ response: TeslatlasHTTPResponse,
    requiresVersion: Bool
  ) throws {
    guard
      response.header("Content-Type")?.lowercased()
        .hasPrefix("application/json") == true
    else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "expected application/json"
      )
    }
    try validateCacheHeaders(response)
    _ = try requiredEntityTag(response)
    if requiresVersion, response.header("Teslatlas-Protocol-Version") == nil {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "missing selected protocol version"
      )
    }
  }

  private static func validateCacheHeaders(_ response: TeslatlasHTTPResponse)
    throws
  {
    guard response.header("Cache-Control") != nil,
      response.header("Vary") != nil
    else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "missing required cache metadata"
      )
    }
  }

  private static func requiredEntityTag(_ response: TeslatlasHTTPResponse) throws
    -> EntityTag
  {
    guard let rawValue = response.header("ETag") else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "missing ETag"
      )
    }
    let eTag = EntityTag(rawValue)
    guard eTag.isValid else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "malformed ETag"
      )
    }
    return eTag
  }

  private static func validateSelectedVersion(
    _ response: TeslatlasHTTPResponse,
    expected: TeslatlasProtocolVersion
  ) throws {
    guard response.header("Teslatlas-Protocol-Version") == expected.description
    else {
      throw TeslatlasSDKError.invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "server selected an unexpected protocol version"
      )
    }
  }

  private static func decodeFailure(_ response: TeslatlasHTTPResponse) throws
    -> TeslatlasSDKError
  {
    guard response.statusCode >= 400,
      response.header("Content-Type")?.lowercased()
        .hasPrefix("application/problem+json") == true
    else {
      return .invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "unexpected HTTP response"
      )
    }
    do {
      let problem = try JSONDecoder().decode(
        TeslatlasProblemDetails.self,
        from: response.body
      )
      guard problem.status == response.statusCode else {
        return .invalidResponse(
          statusCode: response.statusCode,
          requestID: response.header("X-Request-ID"),
          reason: "problem status does not match HTTP status"
        )
      }
      return .problem(problem)
    } catch let error as TeslatlasSDKError {
      return error
    } catch {
      return .invalidResponse(
        statusCode: response.statusCode,
        requestID: response.header("X-Request-ID"),
        reason: "invalid problem details"
      )
    }
  }
}

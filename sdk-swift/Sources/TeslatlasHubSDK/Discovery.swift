import Foundation

public enum TeslatlasDiscoveryError: Error, Equatable, Sendable {
  case invalidDocument(String)
  case forbiddenDiscoveryField(String)
  case noCompatibleProtocolVersion(maximum: String, supported: [String])
  case hubIdentityChanged(expected: String, actual: String)
  case untrustedEndpointOrigin(String)
}

public struct HubEndpointTrustPolicy: Equatable, Sendable {
  private struct Origin: Hashable, Sendable, CustomStringConvertible {
    let scheme: String
    let host: String
    let port: Int?

    init(_ url: URL) throws {
      guard let scheme = url.scheme?.lowercased(),
        let host = url.host?.lowercased(),
        scheme == "https" || (scheme == "http" && Self.isLoopback(host))
      else {
        throw TeslatlasDiscoveryError.invalidDocument(
          "trusted endpoint origins must use HTTPS except on loopback"
        )
      }
      self.scheme = scheme
      self.host = host
      if url.port == Self.defaultPort(for: scheme) {
        port = nil
      } else {
        port = url.port
      }
    }

    var description: String {
      if let port { return "\(scheme)://\(host):\(port)" }
      return "\(scheme)://\(host)"
    }

    private static func defaultPort(for scheme: String) -> Int? {
      switch scheme {
      case "https": 443
      case "http": 80
      default: nil
      }
    }

    private static func isLoopback(_ host: String) -> Bool {
      host == "localhost" || host == "127.0.0.1" || host == "::1"
    }
  }

  private let origins: Set<Origin>

  public init(
    discoveryURL: URL,
    additionalTrustedOrigins: [URL] = []
  ) throws {
    var origins = Set<Origin>()
    origins.insert(try Origin(discoveryURL))
    for url in additionalTrustedOrigins {
      origins.insert(try Origin(url))
    }
    self.origins = origins
  }

  public func validate(_ document: HubDiscoveryDocument) throws {
    for endpoint in [
      document.endpoints.wellKnown,
      document.endpoints.api,
      document.endpoints.events,
      document.endpoints.openAPI,
    ] {
      try validate(endpoint)
    }
  }

  public func validate(_ url: URL) throws {
    let origin = try Origin(url)
    guard origins.contains(origin) else {
      throw TeslatlasDiscoveryError.untrustedEndpointOrigin(origin.description)
    }
  }
}

public struct HubProtocolInfo: Codable, Equatable, Sendable {
  public let currentVersion: TeslatlasProtocolVersion
  public let supportedVersions: [TeslatlasProtocolVersion]
  public let minimumClientVersion: TeslatlasProtocolVersion
  public let versionHeader: String
  public let selection: String

  enum CodingKeys: String, CodingKey {
    case currentVersion = "current_version"
    case supportedVersions = "supported_versions"
    case minimumClientVersion = "minimum_client_version"
    case versionHeader = "version_header"
    case selection
  }

  public func negotiate(maximum: TeslatlasProtocolVersion) throws
    -> TeslatlasProtocolVersion
  {
    guard
      let selected = supportedVersions.last(where: {
        $0.major == maximum.major && $0 <= maximum
      })
    else {
      throw TeslatlasDiscoveryError.noCompatibleProtocolVersion(
        maximum: maximum.description,
        supported: supportedVersions.map(\.description)
      )
    }
    return selected
  }
}

public enum HubCapabilityStatus: String, Codable, Equatable, Sendable {
  case stable
  case deprecated
  case experimental
}

public struct HubCapabilityDeprecation: Codable, Equatable, Sendable {
  public let deprecatedAt: TeslatlasTimestamp
  public let sunsetAt: TeslatlasTimestamp?
  public let successor: String
  public let documentation: URL

  enum CodingKeys: String, CodingKey {
    case deprecatedAt = "deprecated_at"
    case sunsetAt = "sunset_at"
    case successor, documentation
  }
}

public struct HubCommandDescriptor: Codable, Equatable, Sendable {
  public let name: String
  public let commandClass: String
  public let requiredScope: String
  public let retryPolicy: String
  public let confirmationRequired: Bool
  public let parametersSchema: [String: TeslatlasJSONValue]
  public let expectedStateSchema: [String: TeslatlasJSONValue]

  enum CodingKeys: String, CodingKey {
    case name
    case commandClass = "command_class"
    case requiredScope = "required_scope"
    case retryPolicy = "retry_policy"
    case confirmationRequired = "confirmation_required"
    case parametersSchema = "parameters_schema"
    case expectedStateSchema = "expected_state_schema"
  }
}

public struct HubCapability: Codable, Equatable, Sendable {
  public let id: String
  public let version: TeslatlasProtocolVersion
  public let introducedIn: TeslatlasProtocolVersion
  public let status: HubCapabilityStatus
  public let href: String
  public let deprecation: HubCapabilityDeprecation?
  public let commands: [HubCommandDescriptor]?

  enum CodingKeys: String, CodingKey {
    case id, version, status, href, deprecation, commands
    case introducedIn = "introduced_in"
  }
}

public struct HubEndpointSet: Codable, Equatable, Sendable {
  public let wellKnown: URL
  public let api: URL
  public let events: URL
  public let openAPI: URL

  enum CodingKeys: String, CodingKey {
    case wellKnown = "well_known"
    case api, events
    case openAPI = "openapi"
  }
}

public struct HubLimits: Codable, Equatable, Sendable {
  public let maximumRequestBodyBytes: Int
  public let defaultPageSize: Int
  public let maximumPageSize: Int
  public let maximumHistoryRangeDays: Int
  public let maximumDenseRangeDays: Int
  public let maximumConcurrentRequests: Int
  public let maximumSSEConnections: Int
  public let eventReplayRetentionSeconds: Int
  public let idempotencyRetentionSeconds: Int

  enum CodingKeys: String, CodingKey {
    case maximumRequestBodyBytes = "max_request_body_bytes"
    case defaultPageSize = "default_page_size"
    case maximumPageSize = "max_page_size"
    case maximumHistoryRangeDays = "max_history_range_days"
    case maximumDenseRangeDays = "max_dense_range_days"
    case maximumConcurrentRequests = "max_concurrent_requests"
    case maximumSSEConnections = "max_sse_connections"
    case eventReplayRetentionSeconds = "event_replay_retention_seconds"
    case idempotencyRetentionSeconds = "idempotency_retention_seconds"
  }
}

public struct HubDiscoveryDocument: Codable, Equatable, Sendable {
  public let hubID: String
  public let protocolInfo: HubProtocolInfo
  public let capabilities: [HubCapability]
  public let endpoints: HubEndpointSet
  public let limits: HubLimits

  enum CodingKeys: String, CodingKey {
    case hubID = "hub_id"
    case protocolInfo = "protocol"
    case capabilities, endpoints, limits
  }

  public func capability(_ id: String) -> HubCapability? {
    capabilities.first { $0.id == id }
  }
}

public enum HubDiscoveryDecoder {
  public static func decode(_ data: Data) throws -> HubDiscoveryDocument {
    let object: Any
    do {
      object = try JSONSerialization.jsonObject(with: data)
    } catch {
      throw TeslatlasDiscoveryError.invalidDocument("invalid JSON")
    }

    guard let root = object as? [String: Any] else {
      throw TeslatlasDiscoveryError.invalidDocument("root must be an object")
    }
    if let forbidden = findForbiddenField(in: root) {
      throw TeslatlasDiscoveryError.forbiddenDiscoveryField(forbidden)
    }

    let document: HubDiscoveryDocument
    do {
      document = try JSONDecoder().decode(HubDiscoveryDocument.self, from: data)
    } catch {
      throw TeslatlasDiscoveryError.invalidDocument(
        "document does not match protocol schema"
      )
    }

    try validate(document)
    return document
  }

  private static func validate(_ document: HubDiscoveryDocument) throws {
    let hubPattern =
      #"^urn:uuid:[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"#
    guard document.hubID.range(of: hubPattern, options: .regularExpression) != nil
    else {
      throw TeslatlasDiscoveryError.invalidDocument("invalid Hub identity")
    }

    let versions = document.protocolInfo.supportedVersions
    guard !versions.isEmpty,
      versions == versions.sorted(),
      Set(versions).count == versions.count,
      versions.last == document.protocolInfo.currentVersion,
      versions.first == document.protocolInfo.minimumClientVersion,
      versions.allSatisfy({ $0.major == document.protocolInfo.currentVersion.major })
    else {
      throw TeslatlasDiscoveryError.invalidDocument(
        "invalid protocol version ordering"
      )
    }
    guard document.protocolInfo.versionHeader == "Teslatlas-Protocol-Version",
      document.protocolInfo.selection
        == "highest-compatible-not-newer-than-client"
    else {
      throw TeslatlasDiscoveryError.invalidDocument(
        "invalid protocol negotiation constants"
      )
    }

    let capabilityIDs = document.capabilities.map(\.id)
    guard Set(capabilityIDs).count == capabilityIDs.count else {
      throw TeslatlasDiscoveryError.invalidDocument(
        "capability IDs must be unique"
      )
    }

    for endpoint in [
      document.endpoints.wellKnown,
      document.endpoints.api,
      document.endpoints.events,
      document.endpoints.openAPI,
    ] {
      guard endpoint.host != nil, isAllowedTransport(endpoint) else {
        throw TeslatlasDiscoveryError.invalidDocument(
          "non-loopback endpoints must use HTTPS"
        )
      }
    }
  }

  private static func isAllowedTransport(_ url: URL) -> Bool {
    if url.scheme?.lowercased() == "https" { return true }
    guard url.scheme?.lowercased() == "http" else { return false }
    let host = url.host?.lowercased()
    return host == "localhost" || host == "127.0.0.1" || host == "::1"
  }

  private static func findForbiddenField(in value: Any) -> String? {
    let forbidden = Set([
      "access_token", "bearer_token", "credential", "credentials", "email",
      "provider_token", "user", "user_data", "user_id", "vehicle_id",
      "vehicle_ids", "vin",
    ])

    if let object = value as? [String: Any] {
      for key in object.keys.sorted() where forbidden.contains(key.lowercased()) {
        return key
      }
      for (key, child) in object {
        if key == "parameters_schema" || key == "expected_state_schema" {
          continue
        }
        if let found = findForbiddenField(in: child) { return found }
      }
    } else if let array = value as? [Any] {
      for child in array {
        if let found = findForbiddenField(in: child) { return found }
      }
    }
    return nil
  }
}

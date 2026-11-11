import Foundation

public struct HubV1BindingDescriptor: Equatable, Sendable {
  public let bindingVersion: String
  public let hubVersion: String
  public let hubTag: String
  public let hubCommit: String
  public let bindingSHA256: String
  public let supportedOperations: [String]
  public let unavailableFeatures: [String]

  public static func bundled() throws -> Self {
    let binding = try HubV1BindingLoader.load()
    return Self(
      bindingVersion: binding.bindingVersion,
      hubVersion: binding.discovery.hubVersion,
      hubTag: binding.authority.hubTag,
      hubCommit: binding.authority.hubCommit,
      bindingSHA256: HubV1BindingLoader.expectedSHA256,
      supportedOperations: ["discovery"] + binding.routes.map(\.operation),
      unavailableFeatures: binding.unavailableFeatures
    )
  }
}

enum HubV1Operation: String, CaseIterable, Sendable {
  case vehicles
  case currentState = "current_state"
  case drives
}

struct HubV1WireBinding: Decodable, Equatable, Sendable {
  struct Authority: Decodable, Equatable, Sendable {
    struct SourceFile: Decodable, Equatable, Sendable {
      let path: String
      let gitBlobSHA: String

      enum CodingKeys: String, CodingKey {
        case path
        case gitBlobSHA = "git_blob_sha"
      }
    }

    let status: String
    let protocolRepository: String
    let protocolCommitChecked: String
    let protocolReleaseBindingPresent: Bool
    let hubRepository: String
    let hubTag: String
    let hubTagObject: String
    let hubCommit: String
    let correspondingSourceURL: String
    let sourceFiles: [SourceFile]

    enum CodingKeys: String, CodingKey {
      case status
      case protocolRepository = "protocol_repository"
      case protocolCommitChecked = "protocol_commit_checked"
      case protocolReleaseBindingPresent = "protocol_release_binding_present"
      case hubRepository = "hub_repository"
      case hubTag = "hub_tag"
      case hubTagObject = "hub_tag_object"
      case hubCommit = "hub_commit"
      case correspondingSourceURL = "corresponding_source_url"
      case sourceFiles = "source_files"
    }
  }

  struct Discovery: Decodable, Equatable, Sendable {
    let method: String
    let path: String
    let requiredFields: [String]
    let optionalFields: [String]
    let protocolName: String
    let protocolMajor: Int
    let apiVersions: [String]
    let hubVersion: String
    let sourceURL: String
    let packFormat: String
    let allowedCapabilitySets: [[String]]

    enum CodingKeys: String, CodingKey {
      case method, path
      case requiredFields = "required_fields"
      case optionalFields = "optional_fields"
      case protocolName = "protocol"
      case protocolMajor = "protocol_major"
      case apiVersions = "api_versions"
      case hubVersion = "hub_version"
      case sourceURL = "source_url"
      case packFormat = "pack_format"
      case allowedCapabilitySets = "allowed_capability_sets"
    }
  }

  struct Authentication: Decodable, Equatable, Sendable {
    let header: String
    let scheme: String
    let bearerWireEncoding: String
    let bearerWireLength: Int
    let discoveryIsUnauthenticated: Bool
    let credentialedOperations: [String]
    let allowRedirects: Bool
    let nonLoopbackRequiresHTTPS: Bool

    enum CodingKeys: String, CodingKey {
      case header, scheme
      case bearerWireEncoding = "bearer_wire_encoding"
      case bearerWireLength = "bearer_wire_length"
      case discoveryIsUnauthenticated = "discovery_is_unauthenticated"
      case credentialedOperations = "credentialed_operations"
      case allowRedirects = "allow_redirects"
      case nonLoopbackRequiresHTTPS = "non_loopback_requires_https"
    }
  }

  struct Responses: Decodable, Equatable, Sendable {
    let successStatus: Int
    let notModifiedStatus: Int
    let badRequestStatus: Int
    let unauthorizedStatus: Int
    let notFoundStatus: Int
    let serviceUnavailableStatus: Int
    let jsonMediaType: String
    let contentTypeHeader: String
    let requestIDHeader: String
    let entityTagHeader: String
    let unauthorizedChallengeHeader: String
    let unauthorizedChallengeValue: String
    let driveErrorStatusByCode: [String: Int]

    enum CodingKeys: String, CodingKey {
      case successStatus = "success_status"
      case notModifiedStatus = "not_modified_status"
      case badRequestStatus = "bad_request_status"
      case unauthorizedStatus = "unauthorized_status"
      case notFoundStatus = "not_found_status"
      case serviceUnavailableStatus = "service_unavailable_status"
      case jsonMediaType = "json_media_type"
      case contentTypeHeader = "content_type_header"
      case requestIDHeader = "request_id_header"
      case entityTagHeader = "entity_tag_header"
      case unauthorizedChallengeHeader = "unauthorized_challenge_header"
      case unauthorizedChallengeValue = "unauthorized_challenge_value"
      case driveErrorStatusByCode = "drive_error_status_by_code"
    }
  }

  struct Route: Decodable, Equatable, Sendable {
    let operation: String
    let method: String
    let pathTemplate: String
    let requiredCapability: String
    let responseModel: String

    enum CodingKeys: String, CodingKey {
      case operation, method
      case pathTemplate = "path_template"
      case requiredCapability = "required_capability"
      case responseModel = "response_model"
    }
  }

  struct DrivePagination: Decodable, Equatable, Sendable {
    let fromParameter: String
    let fromDefault: Int64
    let fromMinimum: Int64
    let fromInclusive: Bool
    let toParameter: String
    let toDefault: Int64
    let toMinimum: Int64
    let toExclusive: Bool
    let limitParameter: String
    let limitDefault: Int
    let limitMinimum: Int
    let limitMaximum: Int
    let cursorParameter: String
    let cursorIsOpaque: Bool
    let sortFields: [String]
    let sortDirection: String
    let etagIsStrong: Bool
    let conditionalRequestHeader: String
    let eTagFormat: String

    enum CodingKeys: String, CodingKey {
      case fromParameter = "from_parameter"
      case fromDefault = "from_default"
      case fromMinimum = "from_minimum"
      case fromInclusive = "from_inclusive"
      case toParameter = "to_parameter"
      case toDefault = "to_default"
      case toMinimum = "to_minimum"
      case toExclusive = "to_exclusive"
      case limitParameter = "limit_parameter"
      case limitDefault = "limit_default"
      case limitMinimum = "limit_minimum"
      case limitMaximum = "limit_maximum"
      case cursorParameter = "cursor_parameter"
      case cursorIsOpaque = "cursor_is_opaque"
      case sortFields = "sort_fields"
      case sortDirection = "sort_direction"
      case etagIsStrong = "etag_is_strong"
      case conditionalRequestHeader = "conditional_request_header"
      case eTagFormat = "etag_format"
    }
  }

  struct Model: Decodable, Equatable, Sendable {
    let requiredFields: [String]
    let optionalFields: [String]

    enum CodingKeys: String, CodingKey {
      case requiredFields = "required_fields"
      case optionalFields = "optional_fields"
    }
  }

  let kind: String
  let bindingVersion: String
  let authority: Authority
  let discovery: Discovery
  let authentication: Authentication
  let responses: Responses
  let routes: [Route]
  let drivePagination: DrivePagination
  let models: [String: Model]
  let stableDriveErrorCodes: [String]
  let unavailableFeatures: [String]

  enum CodingKeys: String, CodingKey {
    case kind
    case bindingVersion = "binding_version"
    case authority, discovery, authentication, responses, routes
    case drivePagination = "drive_pagination"
    case models
    case stableDriveErrorCodes = "stable_drive_error_codes"
    case unavailableFeatures = "unavailable_features"
  }

  func route(for operation: HubV1Operation) throws -> Route {
    guard let route = routes.first(where: { $0.operation == operation.rawValue }) else {
      throw HubV1Error.invalidBinding("missing route for \(operation.rawValue)")
    }
    return route
  }

  func model(named name: String) throws -> Model {
    guard let model = models[name] else {
      throw HubV1Error.invalidBinding("missing model \(name)")
    }
    return model
  }
}

enum HubV1BindingLoader {
  static let expectedSHA256 =
    "78aca4b6014625420efb66fbe35f6ea10d72a7f596210dac80964c33e0f04f65"

  static func load() throws -> HubV1WireBinding {
    guard
      let url = Bundle.module.url(
        forResource: "deployed-hub-v1.0.0",
        withExtension: "json",
        subdirectory: "Binding"
      )
    else {
      throw HubV1Error.invalidBinding("bundled binding resource is missing")
    }

    let data: Data
    do {
      data = try Data(contentsOf: url, options: [.mappedIfSafe])
    } catch {
      throw HubV1Error.invalidBinding("bundled binding resource is unreadable")
    }

    guard HubV1SHA256.hexDigest(of: data) == expectedSHA256 else {
      throw HubV1Error.invalidBinding("bundled binding digest does not match")
    }

    let binding: HubV1WireBinding
    do {
      binding = try JSONDecoder().decode(HubV1WireBinding.self, from: data)
    } catch {
      throw HubV1Error.invalidBinding("bundled binding is not decodable")
    }
    try validate(binding)
    return binding
  }

  private static func validate(_ binding: HubV1WireBinding) throws {
    guard binding.kind == "teslatlas-deployed-hub-binding",
      binding.bindingVersion == "1.0.0",
      binding.authority.status == "vendored-from-immutable-hub-release-source",
      binding.authority.protocolRepository
        == "https://github.com/magrathean-uk/teslatlas-protocol",
      binding.authority.protocolCommitChecked
        == "79ced4c7fdc79520ad31d72a0280bf5f3f19f407",
      binding.authority.protocolReleaseBindingPresent == false,
      binding.authority.hubRepository
        == "https://github.com/magrathean-uk/teslatlas-hub",
      binding.authority.hubTag == "v1.0.0",
      binding.authority.hubTagObject
        == "4b45708a00f14f76306f6cb37375eb0c538643d7",
      binding.authority.hubCommit
        == "a5e6c5c4f86776da96c9946f7e45b2080c571f86",
      binding.authority.correspondingSourceURL
        == "https://github.com/magrathean-uk/teslatlas-hub/tree/v1.0.0",
      binding.authority.sourceFiles == [
        .init(
          path: "src/lib.rs",
          gitBlobSHA: "ba2119ad7e68397986ce1b21803208106062ad1e"
        ),
        .init(
          path: "docs/guides/api.md",
          gitBlobSHA: "013b12bdeb9852c276a290a6cbcd16ece4d84ad4"
        ),
        .init(
          path: "src/api/server.rs",
          gitBlobSHA: "bc58b487ad5c01bb4889ba12be1cd37435c7e039"
        ),
        .init(
          path: "src/api/public_query.rs",
          gitBlobSHA: "94960af88473306bd3cb0b1f3684f348ec00b7fb"
        ),
        .init(
          path: "src/collection/current_state.rs",
          gitBlobSHA: "9d93500a9ad5a93fa7651cb1f5131e78f28e1e37"
        ),
        .init(
          path: "src/storage/db/access_models.rs",
          gitBlobSHA: "9f33f8c6d2576e056e7d283842c20a4cbfdafb64"
        ),
        .init(
          path: "src/storage/db.rs",
          gitBlobSHA: "b7b4c0f8cb6eeb6d50b1f36a30327032a7379e27"
        ),
        .init(
          path: "src/storage/db/catalogue_helpers.rs",
          gitBlobSHA: "03c0d70ececc15949a3441a846895c58b4ba7a02"
        ),
        .init(
          path: "src/sync/hub_pack/model.rs",
          gitBlobSHA: "6fccc042c7d91cc794d3ec711ec72bd9b8d7689c"
        ),
      ]
    else {
      throw HubV1Error.invalidBinding("release authority constants do not match")
    }

    let discovery = binding.discovery
    guard discovery.method == "GET",
      discovery.path == "/.well-known/teslatlas-hub",
      discovery.protocolName == "teslatlas-sync",
      discovery.protocolMajor == 1,
      discovery.apiVersions == ["1.0"],
      discovery.hubVersion == "1.0.0",
      discovery.sourceURL == binding.authority.correspondingSourceURL,
      discovery.packFormat == "sqlite-zstd",
      discovery.optionalFields == ["manifestPublicKey"],
      Set(discovery.requiredFields).count == discovery.requiredFields.count,
      Set(discovery.requiredFields).isDisjoint(with: discovery.optionalFields)
    else {
      throw HubV1Error.invalidBinding("discovery contract constants do not match")
    }

    let baseCapabilities = ["query.vehicles", "query.current"]
    let fullCapabilities = baseCapabilities + ["query.drives", "sync.packs"]
    guard discovery.allowedCapabilitySets == [baseCapabilities, fullCapabilities] else {
      throw HubV1Error.invalidBinding("capability sets do not match Hub v1.0.0")
    }

    guard binding.authentication.header == "Authorization",
      binding.authentication.scheme == "Bearer",
      binding.authentication.bearerWireEncoding == "ascii-hex",
      binding.authentication.bearerWireLength == 64,
      binding.authentication.discoveryIsUnauthenticated,
      binding.authentication.credentialedOperations
        == HubV1Operation.allCases.map(\.rawValue),
      binding.authentication.allowRedirects == false,
      binding.authentication.nonLoopbackRequiresHTTPS
    else {
      throw HubV1Error.invalidBinding("authentication contract does not match")
    }

    guard binding.responses.successStatus == 200,
      binding.responses.notModifiedStatus == 304,
      binding.responses.badRequestStatus == 400,
      binding.responses.unauthorizedStatus == 401,
      binding.responses.notFoundStatus == 404,
      binding.responses.serviceUnavailableStatus == 503,
      binding.responses.jsonMediaType == "application/json",
      binding.responses.contentTypeHeader == "Content-Type",
      binding.responses.requestIDHeader == "X-Request-ID",
      binding.responses.entityTagHeader == "ETag",
      binding.responses.unauthorizedChallengeHeader == "WWW-Authenticate",
      binding.responses.unauthorizedChallengeValue == "Bearer",
      binding.responses.driveErrorStatusByCode == [
        "invalid_query": 400,
        "invalid_time_range": 400,
        "invalid_limit": 400,
        "invalid_cursor": 400,
        "vehicle_not_found": 404,
        "service_unavailable": 503,
      ]
    else {
      throw HubV1Error.invalidBinding("HTTP response contract does not match")
    }

    guard binding.routes.count == HubV1Operation.allCases.count,
      Set(binding.routes.map(\.operation)).count == binding.routes.count
    else {
      throw HubV1Error.invalidBinding("route operations are missing or duplicated")
    }

    let expectedRoutes: [(HubV1Operation, String, String, String)] = [
      (.vehicles, "/v1/vehicles", "query.vehicles", "vehicle_list"),
      (
        .currentState,
        "/v1/vehicles/{vehicle_id}/current",
        "query.current",
        "current_state"
      ),
      (
        .drives,
        "/v1/vehicles/{vehicle_id}/drives",
        "query.drives",
        "drive_page"
      ),
    ]
    for expected in expectedRoutes {
      let route = try binding.route(for: expected.0)
      guard route.method == "GET",
        route.pathTemplate == expected.1,
        route.requiredCapability == expected.2,
        route.responseModel == expected.3,
        route.pathTemplate.first == "/",
        !route.pathTemplate.contains("://"),
        !route.pathTemplate.contains("?"),
        !route.pathTemplate.contains("#"),
        !route.pathTemplate.contains("..")
      else {
        throw HubV1Error.invalidBinding(
          "route does not match \(expected.0.rawValue)"
        )
      }
    }

    let page = binding.drivePagination
    guard page.fromParameter == "from_ms",
      page.fromDefault == 0,
      page.fromMinimum == 0,
      page.fromInclusive,
      page.toParameter == "to_ms",
      page.toDefault == Int64.max,
      page.toMinimum == 0,
      page.toExclusive,
      page.limitParameter == "limit",
      page.limitDefault == 100,
      page.limitMinimum == 1,
      page.limitMaximum == 500,
      page.cursorParameter == "cursor",
      page.cursorIsOpaque,
      page.sortFields == ["start_date_ms", "id"],
      page.sortDirection == "descending",
      page.etagIsStrong,
      page.conditionalRequestHeader == "If-None-Match",
      page.eTagFormat == "quoted-lowercase-sha256"
    else {
      throw HubV1Error.invalidBinding("drive pagination constants do not match")
    }

    let expectedModels = Set([
      "vehicle_list", "vehicle", "current_state", "projection_car",
      "projection_car_settings", "drive_page", "drive", "error_envelope",
      "error",
    ])
    guard Set(binding.models.keys) == expectedModels else {
      throw HubV1Error.invalidBinding("model catalogue does not match")
    }
    for (name, model) in binding.models {
      guard !model.requiredFields.isEmpty,
        Set(model.requiredFields).count == model.requiredFields.count,
        Set(model.optionalFields).count == model.optionalFields.count,
        Set(model.requiredFields).isDisjoint(with: model.optionalFields)
      else {
        throw HubV1Error.invalidBinding("invalid field catalogue for \(name)")
      }
    }

    guard
      binding.stableDriveErrorCodes == [
        "invalid_query", "invalid_time_range", "invalid_limit", "invalid_cursor",
        "vehicle_not_found", "service_unavailable",
      ], binding.unavailableFeatures == ["events", "commands", "metadata", "charges"]
    else {
      throw HubV1Error.invalidBinding("error or unavailable-feature catalogue changed")
    }
  }
}

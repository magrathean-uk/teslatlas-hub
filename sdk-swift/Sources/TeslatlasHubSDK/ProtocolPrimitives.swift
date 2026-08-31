import Foundation

public struct TeslatlasProtocolVersion: RawRepresentable, Codable, Hashable,
  Comparable, Sendable, CustomStringConvertible
{
  public let rawValue: String
  public let major: Int
  public let minor: Int
  public let patch: Int

  public init?(_ rawValue: String) {
    let parts = rawValue.split(separator: ".", omittingEmptySubsequences: false)
    guard parts.count == 3 else { return nil }

    var numbers: [Int] = []
    for part in parts {
      guard !part.isEmpty,
        part.allSatisfy(\.isNumber),
        part == "0" || part.first != "0",
        let number = Int(part)
      else {
        return nil
      }
      numbers.append(number)
    }

    self.rawValue = rawValue
    major = numbers[0]
    minor = numbers[1]
    patch = numbers[2]
  }

  public init?(rawValue: String) {
    self.init(rawValue)
  }

  public var description: String { rawValue }

  public static func < (lhs: Self, rhs: Self) -> Bool {
    (lhs.major, lhs.minor, lhs.patch) < (rhs.major, rhs.minor, rhs.patch)
  }

  public init(from decoder: Decoder) throws {
    let container = try decoder.singleValueContainer()
    let rawValue = try container.decode(String.self)
    guard let value = Self(rawValue) else {
      throw DecodingError.dataCorruptedError(
        in: container,
        debugDescription: "Invalid semantic version"
      )
    }
    self = value
  }

  public func encode(to encoder: Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(rawValue)
  }
}

public struct TeslatlasTimestamp: RawRepresentable, Codable, Hashable, Sendable,
  CustomStringConvertible
{
  public let rawValue: String

  public init?(_ rawValue: String) {
    let pattern = #"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}Z$"#
    guard rawValue.range(of: pattern, options: .regularExpression) != nil else {
      return nil
    }

    let formatter = ISO8601DateFormatter()
    formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    formatter.timeZone = TimeZone(secondsFromGMT: 0)
    guard let date = formatter.date(from: rawValue),
      formatter.string(from: date) == rawValue
    else {
      return nil
    }
    self.rawValue = rawValue
  }

  public init?(rawValue: String) {
    self.init(rawValue)
  }

  public var description: String { rawValue }

  public init(from decoder: Decoder) throws {
    let container = try decoder.singleValueContainer()
    let rawValue = try container.decode(String.self)
    guard let value = Self(rawValue) else {
      throw DecodingError.dataCorruptedError(
        in: container,
        debugDescription: "Expected RFC 3339 UTC timestamp with milliseconds"
      )
    }
    self = value
  }

  public func encode(to encoder: Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(rawValue)
  }
}

public struct OpaqueCursor: RawRepresentable, Codable, Hashable, Sendable,
  CustomStringConvertible, CustomDebugStringConvertible
{
  public let rawValue: String

  public init(_ rawValue: String) {
    self.rawValue = rawValue
  }

  public init(rawValue: String) {
    self.init(rawValue)
  }

  public var description: String { "OpaqueCursor(<redacted>)" }
  public var debugDescription: String { description }
}

public struct EntityTag: RawRepresentable, Codable, Hashable, Sendable,
  CustomStringConvertible
{
  public let rawValue: String

  public init(_ rawValue: String) {
    self.rawValue = rawValue
  }

  public init(rawValue: String) {
    self.init(rawValue)
  }

  public var description: String { rawValue }

  var isValid: Bool {
    rawValue.range(
      of: #"^(?:W/)?\"[^\"]+\"$"#,
      options: .regularExpression
    ) != nil
  }
}

public enum TeslatlasJSONValue: Codable, Equatable, Sendable {
  case null
  case bool(Bool)
  case integer(Int64)
  case number(Double)
  case string(String)
  case array([TeslatlasJSONValue])
  case object([String: TeslatlasJSONValue])

  public init(from decoder: Decoder) throws {
    let container = try decoder.singleValueContainer()
    if container.decodeNil() {
      self = .null
    } else if let value = try? container.decode(Bool.self) {
      self = .bool(value)
    } else if let value = try? container.decode(Int64.self) {
      self = .integer(value)
    } else if let value = try? container.decode(Double.self) {
      guard value.isFinite else {
        throw DecodingError.dataCorruptedError(
          in: container,
          debugDescription: "JSON number must be finite"
        )
      }
      self = .number(value)
    } else if let value = try? container.decode(String.self) {
      self = .string(value)
    } else if let value = try? container.decode([TeslatlasJSONValue].self) {
      self = .array(value)
    } else {
      self = .object(try container.decode([String: TeslatlasJSONValue].self))
    }
  }

  public func encode(to encoder: Encoder) throws {
    var container = encoder.singleValueContainer()
    switch self {
    case .null:
      try container.encodeNil()
    case .bool(let value):
      try container.encode(value)
    case .integer(let value):
      try container.encode(value)
    case .number(let value):
      guard value.isFinite else {
        throw EncodingError.invalidValue(
          value,
          EncodingError.Context(
            codingPath: encoder.codingPath,
            debugDescription: "JSON number must be finite"
          )
        )
      }
      try container.encode(value)
    case .string(let value):
      try container.encode(value)
    case .array(let value):
      try container.encode(value)
    case .object(let value):
      try container.encode(value)
    }
  }
}

public struct TeslatlasProblemDetails: Codable, Equatable, Sendable {
  public struct FieldError: Codable, Equatable, Sendable {
    public let field: String
    public let code: String
    public let message: String?

    public init(field: String, code: String, message: String? = nil) {
      self.field = field
      self.code = code
      self.message = message
    }
  }

  public let type: String
  public let title: String
  public let status: Int
  public let detail: String?
  public let instance: String
  public let code: String
  public let requestID: String
  public let retryable: Bool
  public let fieldErrors: [FieldError]?
  public let retryAfterSeconds: Int?

  enum CodingKeys: String, CodingKey {
    case type, title, status, detail, instance, code, retryable
    case requestID = "request_id"
    case fieldErrors = "field_errors"
    case retryAfterSeconds = "retry_after_seconds"
  }
}

public enum TeslatlasSDKError: Error, Equatable, Sendable {
  case problem(TeslatlasProblemDetails)
  case invalidResponse(statusCode: Int, requestID: String?, reason: String)
  case limitExceeded(name: String, maximum: Int, actual: Int)
  case capabilityUnavailable(String)
}

public enum TeslatlasConditionalResponse<Value: Sendable>: Sendable {
  case modified(Value, EntityTag)
  case notModified(EntityTag)
}

extension TeslatlasConditionalResponse: Equatable where Value: Equatable {}

public struct PageRequest: Equatable, Sendable {
  public let cursor: OpaqueCursor?
  public let limit: Int?

  public init(cursor: OpaqueCursor? = nil, limit: Int? = nil) {
    self.cursor = cursor
    self.limit = limit
  }
}

public struct HistoryRequest: Equatable, Sendable {
  public let from: TeslatlasTimestamp?
  public let to: TeslatlasTimestamp?
  public let cursor: OpaqueCursor?
  public let limit: Int?

  public init(
    from: TeslatlasTimestamp? = nil,
    to: TeslatlasTimestamp? = nil,
    cursor: OpaqueCursor? = nil,
    limit: Int? = nil
  ) {
    self.from = from
    self.to = to
    self.cursor = cursor
    self.limit = limit
  }
}

public struct DataQualityRequest: Equatable, Sendable {
  public let vehicleID: String?
  public let from: TeslatlasTimestamp?
  public let to: TeslatlasTimestamp?
  public let cursor: OpaqueCursor?
  public let limit: Int?

  public init(
    vehicleID: String? = nil,
    from: TeslatlasTimestamp? = nil,
    to: TeslatlasTimestamp? = nil,
    cursor: OpaqueCursor? = nil,
    limit: Int? = nil
  ) {
    self.vehicleID = vehicleID
    self.from = from
    self.to = to
    self.cursor = cursor
    self.limit = limit
  }
}

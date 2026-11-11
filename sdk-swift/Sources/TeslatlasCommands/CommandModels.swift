import Foundation
import TeslatlasHubSDK

public enum CommandClass: String, Codable, Equatable, Sendable {
  case climate
  case charging
  case access
  case nuisance
  case vehicleState = "vehicle_state"
}

public enum CommandState: String, Codable, Equatable, Sendable {
  case accepted
  case authorising
  case sent
  case providerAcknowledged = "provider_acknowledged"
  case verifying
  case succeeded
  case failed
  case indeterminate
  case expired
  case cancelled
}

public enum CommandRetryPolicy: String, Codable, Equatable, Sendable {
  case none
  case stateVerified = "state_verified"
}

public struct CommandConfirmation: Codable, Equatable, Sendable {
  public let confirmedAt: TeslatlasTimestamp
  public let confirmedBy: String

  public init(confirmedAt: TeslatlasTimestamp, confirmedBy: String) {
    self.confirmedAt = confirmedAt
    self.confirmedBy = confirmedBy
  }

  enum CodingKeys: String, CodingKey {
    case confirmedAt = "confirmed_at"
    case confirmedBy = "confirmed_by"
  }
}

public struct CommandRequest: Codable, Equatable, Sendable {
  public let vehicleID: String
  public let command: String
  public let commandClass: CommandClass
  public let parameters: [String: TeslatlasJSONValue]
  public let expectedState: [String: TeslatlasJSONValue]
  public let expiresAt: TeslatlasTimestamp
  public let confirmation: CommandConfirmation?

  public init(
    vehicleID: String,
    command: String,
    commandClass: CommandClass,
    parameters: [String: TeslatlasJSONValue],
    expectedState: [String: TeslatlasJSONValue],
    expiresAt: TeslatlasTimestamp,
    confirmation: CommandConfirmation? = nil
  ) {
    self.vehicleID = vehicleID
    self.command = command
    self.commandClass = commandClass
    self.parameters = parameters
    self.expectedState = expectedState
    self.expiresAt = expiresAt
    self.confirmation = confirmation
  }

  enum CodingKeys: String, CodingKey {
    case vehicleID = "vehicle_id"
    case command
    case commandClass = "command_class"
    case parameters
    case expectedState = "expected_state"
    case expiresAt = "expires_at"
    case confirmation
  }
}

public struct CommandAuditEvent: Codable, Equatable, Sendable {
  public let state: CommandState
  public let at: TeslatlasTimestamp
  public let actorID: String
  public let note: String?

  enum CodingKeys: String, CodingKey {
    case state, at
    case actorID = "actor_id"
    case note
  }
}

public struct CommandLinks: Codable, Equatable, Sendable {
  public let `self`: String
}

public struct CommandJob: Codable, Equatable, Sendable {
  public let commandID: String
  public let vehicleID: String
  public let command: String
  public let commandClass: CommandClass
  public let state: CommandState
  public let createdAt: TeslatlasTimestamp
  public let updatedAt: TeslatlasTimestamp
  public let expiresAt: TeslatlasTimestamp
  public let attemptCount: Int
  public let retryPolicy: CommandRetryPolicy
  public let result: [String: TeslatlasJSONValue]?
  public let error: TeslatlasProblemDetails?
  public let audit: [CommandAuditEvent]
  public let links: CommandLinks

  enum CodingKeys: String, CodingKey {
    case commandID = "command_id"
    case vehicleID = "vehicle_id"
    case command
    case commandClass = "command_class"
    case state
    case createdAt = "created_at"
    case updatedAt = "updated_at"
    case expiresAt = "expires_at"
    case attemptCount = "attempt_count"
    case retryPolicy = "retry_policy"
    case result, error, audit, links
  }
}

public struct CommandAcceptance: Equatable, Sendable {
  public let job: CommandJob
  public let entityTag: EntityTag
  public let location: String

  public init(job: CommandJob, entityTag: EntityTag, location: String) {
    self.job = job
    self.entityTag = entityTag
    self.location = location
  }
}

public enum TeslatlasCommandError: Error, Equatable, Sendable {
  case capabilityUnavailable
  case commandNotAdvertised(String)
  case commandClassMismatch(command: String, expected: String, actual: String)
  case confirmationRequired(String)
  case invalidResponse(String)
}

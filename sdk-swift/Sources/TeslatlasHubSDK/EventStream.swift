import Foundation

public struct TeslatlasEventEnvelope: Codable, Equatable, Sendable {
  public let eventID: String
  public let eventType: String
  public let occurredAt: TeslatlasTimestamp
  public let vehicleID: String?
  public let resourceID: String?
  public let revision: Int
  public let data: TeslatlasJSONValue

  enum CodingKeys: String, CodingKey {
    case eventID = "event_id"
    case eventType = "event_type"
    case occurredAt = "occurred_at"
    case vehicleID = "vehicle_id"
    case resourceID = "resource_id"
    case revision, data
  }
}

public enum TeslatlasEventStreamOutput: Equatable, Sendable {
  case event(TeslatlasEventEnvelope)
  case retry(milliseconds: UInt64)
}

public enum TeslatlasEventDecodingError: Error, Equatable, Sendable {
  case invalidUTF8
  case lineTooLong(limit: Int)
  case eventDataTooLarge(limit: Int)
  case missingEventName
  case missingEventID
  case malformedEnvelope
  case eventNameMismatch(sse: String, envelope: String)
  case eventIDMismatch(sse: String, envelope: String)
  case revisionMismatch(envelope: Int, payload: Int)
  case vehicleIdentityMismatch(envelope: String?, payload: String?)
  case resourceIdentityMismatch(envelope: String?, payload: String?)
}

public struct TeslatlasEventStreamDecoder: Sendable {
  private static let recognizedEventNames = Set([
    "observation.admitted",
    "vehicle.current.changed",
    "drive.started",
    "drive.updated",
    "drive.ended",
    "charge.started",
    "charge.updated",
    "charge.ended",
    "state.changed",
    "software_update.changed",
    "data_quality.changed",
    "command.changed",
    "metadata.changed",
  ])

  private static let resourceIDFields = [
    "observation.admitted": "observation_id",
    "vehicle.current.changed": "vehicle_id",
    "drive.started": "drive_id",
    "drive.updated": "drive_id",
    "drive.ended": "drive_id",
    "charge.started": "charge_id",
    "charge.updated": "charge_id",
    "charge.ended": "charge_id",
    "state.changed": "state_id",
    "software_update.changed": "update_id",
    "data_quality.changed": "subject_id",
    "command.changed": "command_id",
    "metadata.changed": "metadata_id",
  ]

  private var framingDecoder: ServerSentEventDecoder

  public init(
    maximumLineBytes: Int = 64 * 1_024,
    maximumEventDataBytes: Int = 8 * 1_024 * 1_024
  ) {
    framingDecoder = ServerSentEventDecoder(
      limits: ServerSentEventDecoderLimits(
        maximumLineBytes: maximumLineBytes,
        maximumEventDataBytes: maximumEventDataBytes
      )
    )
  }

  public mutating func append(_ data: Data) throws
    -> [TeslatlasEventStreamOutput]
  {
    do {
      return try transform(framingDecoder.append(data))
    } catch let error as ServerSentEventDecodingError {
      throw Self.map(error)
    }
  }

  public mutating func finish() throws -> [TeslatlasEventStreamOutput] {
    do {
      return try transform(framingDecoder.finish())
    } catch let error as ServerSentEventDecodingError {
      throw Self.map(error)
    }
  }

  private mutating func transform(
    _ outputs: [ServerSentEventDecoderOutput]
  ) throws -> [TeslatlasEventStreamOutput] {
    var result: [TeslatlasEventStreamOutput] = []
    for output in outputs {
      switch output {
      case .retry(let milliseconds):
        result.append(.retry(milliseconds: min(milliseconds, 30_000)))
      case .event(let event):
        guard let name = event.name else {
          throw TeslatlasEventDecodingError.missingEventName
        }
        guard Self.recognizedEventNames.contains(name) else {
          continue
        }
        guard let id = event.id else {
          throw TeslatlasEventDecodingError.missingEventID
        }
        let envelope: TeslatlasEventEnvelope
        do {
          envelope = try JSONDecoder().decode(
            TeslatlasEventEnvelope.self,
            from: Data(event.data.utf8)
          )
        } catch {
          throw TeslatlasEventDecodingError.malformedEnvelope
        }
        guard name == envelope.eventType else {
          throw TeslatlasEventDecodingError.eventNameMismatch(
            sse: name,
            envelope: envelope.eventType
          )
        }
        guard id == envelope.eventID else {
          throw TeslatlasEventDecodingError.eventIDMismatch(
            sse: id,
            envelope: envelope.eventID
          )
        }
        try Self.validatePayloadIdentity(envelope)
        result.append(.event(envelope))
      }
    }
    return result
  }

  private static func validatePayloadIdentity(
    _ envelope: TeslatlasEventEnvelope
  ) throws {
    guard case .object(let object) = envelope.data else {
      throw TeslatlasEventDecodingError.malformedEnvelope
    }

    if let payloadRevision = integer(object["revision"]),
      payloadRevision != envelope.revision
    {
      throw TeslatlasEventDecodingError.revisionMismatch(
        envelope: envelope.revision,
        payload: payloadRevision
      )
    }
    if let audit = object["audit"],
      case .object(let auditObject) = audit,
      let deletion = auditObject["deletion"],
      case .object(let deletionObject) = deletion,
      let deletionRevision = integer(deletionObject["revision"]),
      deletionRevision != envelope.revision
    {
      throw TeslatlasEventDecodingError.revisionMismatch(
        envelope: envelope.revision,
        payload: deletionRevision
      )
    }

    if let payloadVehicleValue = object["vehicle_id"] {
      let payloadVehicleID = nullableString(payloadVehicleValue)
      guard payloadVehicleID == envelope.vehicleID else {
        throw TeslatlasEventDecodingError.vehicleIdentityMismatch(
          envelope: envelope.vehicleID,
          payload: payloadVehicleID
        )
      }
    }

    if let field = resourceIDFields[envelope.eventType],
      let payloadResourceValue = object[field]
    {
      let payloadResourceID = nullableString(payloadResourceValue)
      guard payloadResourceID == envelope.resourceID else {
        throw TeslatlasEventDecodingError.resourceIdentityMismatch(
          envelope: envelope.resourceID,
          payload: payloadResourceID
        )
      }
    }
  }

  private static func integer(_ value: TeslatlasJSONValue?) -> Int? {
    guard case .integer(let integer) = value,
      let result = Int(exactly: integer)
    else {
      return nil
    }
    return result
  }

  private static func nullableString(_ value: TeslatlasJSONValue) -> String? {
    switch value {
    case .string(let string): string
    case .null: nil
    default: nil
    }
  }

  private static func map(_ error: ServerSentEventDecodingError)
    -> TeslatlasEventDecodingError
  {
    switch error {
    case .invalidUTF8: .invalidUTF8
    case .lineTooLong(let limit): .lineTooLong(limit: limit)
    case .eventDataTooLarge(let limit): .eventDataTooLarge(limit: limit)
    }
  }
}

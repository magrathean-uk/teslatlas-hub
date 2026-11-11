import Foundation
import TeslatlasCurrentHub

struct ConsumerSemanticVehicle: Sendable {
  let current: CurrentHubCurrentState
  let drives: [CurrentHubDrive]
}

struct ConsumerHistoryBoundaryCheck: Sendable {
  let window: String
  let fromMilliseconds: Int64
  let toMilliseconds: Int64
  let count: Int
}

enum ConsumerSemanticSnapshot {
  static let profile = "hub-http-v1@1.0.0"
  static let bounds = "from inclusive, to exclusive"
  static let nullRule = "null means unavailable, unknown, or not derivable; numeric zero remains zero"
  static let staleRule = "no freshness guarantee beyond observed_at_ms; vehicle-1 timestamp is retained exactly and HA displays telemetry age"

  static func encode(
    vehicles: [ConsumerSemanticVehicle],
    fromMilliseconds: Int64,
    toMilliseconds: Int64,
    boundaryChecks: [ConsumerHistoryBoundaryCheck]
  ) throws -> Data {
    let snapshot = Snapshot(
      schemaVersion: 1,
      profile: profile,
      vehicleOrder: vehicles.indices.map { "vehicle-\($0 + 1)" },
      vehicles: vehicles.enumerated().map { index, vehicle in
        Vehicle(
          label: "vehicle-\(index + 1)",
          current: Current(vehicle.current),
          history: History(
            query: Query(
              fromMilliseconds: fromMilliseconds,
              toMilliseconds: toMilliseconds,
              bounds: bounds
            ),
            count: vehicle.drives.count,
            order: vehicle.drives.enumerated().map { driveIndex, drive in
              HistoryItem(ordinal: driveIndex + 1, drive: drive)
            }
          )
        )
      },
      historyBoundaryChecks: boundaryChecks.map(BoundaryCheck.init),
      nullRule: nullRule,
      staleRule: staleRule
    )
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
    return try encoder.encode(snapshot)
  }

  static func requireUnchanged(before: Data, after: Data) throws {
    guard before == after else { throw ConsumerError.restartSemanticMismatch }
  }
}

private struct Snapshot: Encodable {
  let schemaVersion: Int
  let profile: String
  let vehicleOrder: [String]
  let vehicles: [Vehicle]
  let historyBoundaryChecks: [BoundaryCheck]
  let nullRule: String
  let staleRule: String

  enum CodingKeys: String, CodingKey {
    case schemaVersion = "schema_version"
    case profile
    case vehicleOrder = "vehicle_order"
    case vehicles
    case historyBoundaryChecks = "history_boundary_checks"
    case nullRule = "null_rule"
    case staleRule = "stale_rule"
  }
}

private struct Vehicle: Encodable {
  let label: String
  let current: Current
  let history: History
}

private struct Current: Encodable {
  let observedAtMilliseconds: Int64?
  let state: String
  let batteryLevel: Measurement<Int64>
  let estimatedRange: Measurement<Double>
  let odometer: Measurement<Double>
  let speed: Measurement<Int64>
  let insideTemperature: Measurement<Double>
  let outsideTemperature: Measurement<Double>
  let chargerPower: Measurement<Double>
  let locked: Bool?

  init(_ current: CurrentHubCurrentState) {
    observedAtMilliseconds = current.observedAtMilliseconds
    state = current.state ?? "unavailable"
    batteryLevel = Measurement(value: current.batteryLevel, unit: "percent")
    estimatedRange = Measurement(
      value: current.estimatedBatteryRangeKilometres,
      unit: "km"
    )
    odometer = Measurement(value: current.odometerKilometres, unit: "km")
    speed = Measurement(value: current.speedKilometresPerHour, unit: "km/h")
    insideTemperature = Measurement(
      value: current.insideTemperatureCelsius,
      unit: "degrees Celsius"
    )
    outsideTemperature = Measurement(
      value: current.outsideTemperatureCelsius,
      unit: "degrees Celsius"
    )
    chargerPower = Measurement(value: current.chargerPowerKilowatts, unit: "kW")
    locked = current.locked
  }

  enum CodingKeys: String, CodingKey {
    case observedAtMilliseconds = "observed_at_ms"
    case state
    case batteryLevel = "battery_level"
    case estimatedRange = "estimated_range"
    case odometer, speed
    case insideTemperature = "inside_temperature"
    case outsideTemperature = "outside_temperature"
    case chargerPower = "charger_power"
    case locked
  }

  func encode(to encoder: Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    if let observedAtMilliseconds {
      try container.encode(observedAtMilliseconds, forKey: .observedAtMilliseconds)
    } else {
      try container.encodeNil(forKey: .observedAtMilliseconds)
    }
    try container.encode(state, forKey: .state)
    try container.encode(batteryLevel, forKey: .batteryLevel)
    try container.encode(estimatedRange, forKey: .estimatedRange)
    try container.encode(odometer, forKey: .odometer)
    try container.encode(speed, forKey: .speed)
    try container.encode(insideTemperature, forKey: .insideTemperature)
    try container.encode(outsideTemperature, forKey: .outsideTemperature)
    try container.encode(chargerPower, forKey: .chargerPower)
    if let locked {
      try container.encode(locked, forKey: .locked)
    } else {
      try container.encodeNil(forKey: .locked)
    }
  }
}

private struct Measurement<Value: Encodable>: Encodable {
  let value: Value?
  let unit: String

  func encode(to encoder: Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    if let value {
      try container.encode(value, forKey: .value)
    } else {
      try container.encodeNil(forKey: .value)
    }
    try container.encode(unit, forKey: .unit)
  }

  private enum CodingKeys: String, CodingKey { case value, unit }
}

private struct Query: Encodable {
  let fromMilliseconds: Int64
  let toMilliseconds: Int64
  let bounds: String

  enum CodingKeys: String, CodingKey {
    case fromMilliseconds = "from_ms"
    case toMilliseconds = "to_ms"
    case bounds
  }
}

private struct History: Encodable {
  let query: Query
  let count: Int
  let order: [HistoryItem]
}

private struct HistoryItem: Encodable {
  let ordinal: Int
  let startDateMilliseconds: Int64
  let endDateMilliseconds: Int64
  let distance: Measurement<Double>
  let duration: Measurement<Int64>

  init(ordinal: Int, drive: CurrentHubDrive) {
    self.ordinal = ordinal
    startDateMilliseconds = drive.startDateMilliseconds
    endDateMilliseconds = drive.endDateMilliseconds
    distance = Measurement(value: drive.distanceKilometres, unit: "km")
    duration = Measurement(value: drive.durationMinutes, unit: "minutes")
  }

  enum CodingKeys: String, CodingKey {
    case ordinal
    case startDateMilliseconds = "start_date_ms"
    case endDateMilliseconds = "end_date_ms"
    case distance, duration
  }
}

private struct BoundaryCheck: Encodable {
  let window: String
  let fromMilliseconds: Int64
  let toMilliseconds: Int64
  let count: Int

  init(_ check: ConsumerHistoryBoundaryCheck) {
    window = check.window
    fromMilliseconds = check.fromMilliseconds
    toMilliseconds = check.toMilliseconds
    count = check.count
  }

  enum CodingKeys: String, CodingKey {
    case window
    case fromMilliseconds = "from_ms"
    case toMilliseconds = "to_ms"
    case count
  }
}

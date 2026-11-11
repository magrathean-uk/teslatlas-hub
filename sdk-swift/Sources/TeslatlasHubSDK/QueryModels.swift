import Foundation

public enum VehicleState: String, Codable, Equatable, Sendable {
  case online, asleep, offline, driving, charging, updating, unknown
}

public enum VehicleChargingState: String, Codable, Equatable, Sendable {
  case charging, stopped, complete, disconnected, unknown
}

public struct TeslatlasLocation: Codable, Equatable, Sendable {
  public let latitude: Double
  public let longitude: Double
  public let altitudeMetres: Double?

  enum CodingKeys: String, CodingKey {
    case latitude, longitude
    case altitudeMetres = "altitude_m"
  }
}

public struct DataQualityIssue: Codable, Equatable, Sendable {
  public let code: String
  public let severity: String
  public let message: String
  public let from: TeslatlasTimestamp?
  public let to: TeslatlasTimestamp?
  public let affectedFields: [String]?

  enum CodingKeys: String, CodingKey {
    case code, severity, message, from, to
    case affectedFields = "affected_fields"
  }
}

public struct DataQualityAssessment: Codable, Equatable, Sendable {
  public let subjectType: String
  public let subjectID: String
  public let quality: String
  public let sources: [String]
  public let gapCount: Int
  public let largestGapSeconds: Int
  public let derivedFields: [String]
  public let projectionVersion: TeslatlasProtocolVersion
  public let assessedAt: TeslatlasTimestamp
  public let issues: [DataQualityIssue]

  enum CodingKeys: String, CodingKey {
    case subjectType = "subject_type"
    case subjectID = "subject_id"
    case quality, sources
    case gapCount = "gap_count"
    case largestGapSeconds = "largest_gap_seconds"
    case derivedFields = "derived_fields"
    case projectionVersion = "projection_version"
    case assessedAt = "assessed_at"
    case issues
  }
}

public struct VehicleSummary: Codable, Equatable, Sendable {
  public let resourceType: String
  public let vehicleID: String
  public let displayName: String
  public let state: VehicleState
  public let lastObservedAt: TeslatlasTimestamp
  public let revision: Int

  enum CodingKeys: String, CodingKey {
    case resourceType = "resource_type"
    case vehicleID = "vehicle_id"
    case displayName = "display_name"
    case state
    case lastObservedAt = "last_observed_at"
    case revision
  }
}

public struct VehicleCurrentState: Codable, Equatable, Sendable {
  public let resourceType: String
  public let vehicleID: String
  public let observedAt: TeslatlasTimestamp
  public let revision: Int
  public let state: VehicleState
  public let batteryLevelPercent: Double?
  public let rangeKilometres: Double?
  public let odometerKilometres: Double?
  public let insideTemperatureCelsius: Double?
  public let outsideTemperatureCelsius: Double?
  public let locked: Bool?
  public let climateOn: Bool?
  public let chargingState: VehicleChargingState?
  public let location: TeslatlasLocation?
  public let quality: DataQualityAssessment

  enum CodingKeys: String, CodingKey {
    case resourceType = "resource_type"
    case vehicleID = "vehicle_id"
    case observedAt = "observed_at"
    case revision, state
    case batteryLevelPercent = "battery_level_percent"
    case rangeKilometres = "range_km"
    case odometerKilometres = "odometer_km"
    case insideTemperatureCelsius = "inside_temperature_c"
    case outsideTemperatureCelsius = "outside_temperature_c"
    case locked
    case climateOn = "climate_on"
    case chargingState = "charging_state"
    case location, quality
  }
}

public struct Drive: Codable, Equatable, Sendable {
  public let resourceType: String
  public let driveID: String
  public let vehicleID: String
  public let startAt: TeslatlasTimestamp
  public let endAt: TeslatlasTimestamp?
  public let startOdometerKilometres: Double?
  public let endOdometerKilometres: Double?
  public let distanceKilometres: Double?
  public let durationSeconds: Int?
  public let energyUsedKilowattHours: Double?
  public let efficiencyWattHoursPerKilometre: Double?
  public let quality: DataQualityAssessment

  enum CodingKeys: String, CodingKey {
    case resourceType = "resource_type"
    case driveID = "drive_id"
    case vehicleID = "vehicle_id"
    case startAt = "start_at"
    case endAt = "end_at"
    case startOdometerKilometres = "start_odometer_km"
    case endOdometerKilometres = "end_odometer_km"
    case distanceKilometres = "distance_km"
    case durationSeconds = "duration_seconds"
    case energyUsedKilowattHours = "energy_used_kwh"
    case efficiencyWattHoursPerKilometre = "efficiency_wh_per_km"
    case quality
  }
}

public struct Position: Codable, Equatable, Sendable {
  public let resourceType: String
  public let positionID: String
  public let driveID: String
  public let vehicleID: String
  public let observedAt: TeslatlasTimestamp
  public let location: TeslatlasLocation
  public let speedKilometresPerHour: Double?
  public let headingDegrees: Double?
  public let qualityFlags: [String]

  enum CodingKeys: String, CodingKey {
    case resourceType = "resource_type"
    case positionID = "position_id"
    case driveID = "drive_id"
    case vehicleID = "vehicle_id"
    case observedAt = "observed_at"
    case location
    case speedKilometresPerHour = "speed_km_h"
    case headingDegrees = "heading_degrees"
    case qualityFlags = "quality_flags"
  }
}

public struct Charge: Codable, Equatable, Sendable {
  public let resourceType: String
  public let chargeID: String
  public let vehicleID: String
  public let startAt: TeslatlasTimestamp
  public let endAt: TeslatlasTimestamp?
  public let chargingType: String
  public let energyAddedKilowattHours: Double?
  public let startBatteryPercent: Double?
  public let endBatteryPercent: Double?
  public let location: TeslatlasLocation?
  public let quality: DataQualityAssessment

  enum CodingKeys: String, CodingKey {
    case resourceType = "resource_type"
    case chargeID = "charge_id"
    case vehicleID = "vehicle_id"
    case startAt = "start_at"
    case endAt = "end_at"
    case chargingType = "charging_type"
    case energyAddedKilowattHours = "energy_added_kwh"
    case startBatteryPercent = "start_battery_percent"
    case endBatteryPercent = "end_battery_percent"
    case location, quality
  }
}

public struct ChargeSample: Codable, Equatable, Sendable {
  public let resourceType: String
  public let chargeSampleID: String
  public let chargeID: String
  public let vehicleID: String
  public let observedAt: TeslatlasTimestamp
  public let batteryLevelPercent: Double?
  public let powerKilowatts: Double?
  public let energyAddedKilowattHours: Double?
  public let voltageVolts: Double?
  public let currentAmperes: Double?
  public let phases: Int?
  public let qualityFlags: [String]

  enum CodingKeys: String, CodingKey {
    case resourceType = "resource_type"
    case chargeSampleID = "charge_sample_id"
    case chargeID = "charge_id"
    case vehicleID = "vehicle_id"
    case observedAt = "observed_at"
    case batteryLevelPercent = "battery_level_percent"
    case powerKilowatts = "power_kw"
    case energyAddedKilowattHours = "energy_added_kwh"
    case voltageVolts = "voltage_v"
    case currentAmperes = "current_a"
    case phases
    case qualityFlags = "quality_flags"
  }
}

public struct StateInterval: Codable, Equatable, Sendable {
  public let resourceType: String
  public let stateID: String
  public let vehicleID: String
  public let state: VehicleState
  public let startAt: TeslatlasTimestamp
  public let endAt: TeslatlasTimestamp?
  public let quality: DataQualityAssessment

  enum CodingKeys: String, CodingKey {
    case resourceType = "resource_type"
    case stateID = "state_id"
    case vehicleID = "vehicle_id"
    case state
    case startAt = "start_at"
    case endAt = "end_at"
    case quality
  }
}

public struct SoftwareUpdate: Codable, Equatable, Sendable {
  public let resourceType: String
  public let updateID: String
  public let vehicleID: String
  public let status: String
  public let version: String
  public let firstSeenAt: TeslatlasTimestamp
  public let lastSeenAt: TeslatlasTimestamp
  public let installedAt: TeslatlasTimestamp?
  public let quality: DataQualityAssessment

  enum CodingKeys: String, CodingKey {
    case resourceType = "resource_type"
    case updateID = "update_id"
    case vehicleID = "vehicle_id"
    case status, version
    case firstSeenAt = "first_seen_at"
    case lastSeenAt = "last_seen_at"
    case installedAt = "installed_at"
    case quality
  }
}

public struct TeslatlasPage<Item: Codable & Sendable>: Codable, Sendable {
  public let resourceType: String
  public let items: [Item]
  public let nextCursor: OpaqueCursor?
  public let snapshotRevision: String
  public let generatedAt: TeslatlasTimestamp

  enum CodingKeys: String, CodingKey {
    case resourceType = "resource_type"
    case items
    case nextCursor = "next_cursor"
    case snapshotRevision = "snapshot_revision"
    case generatedAt = "generated_at"
  }
}

extension TeslatlasPage: Equatable where Item: Equatable {}

public typealias VehiclePage = TeslatlasPage<VehicleSummary>
public typealias DrivePage = TeslatlasPage<Drive>
public typealias PositionPage = TeslatlasPage<Position>
public typealias ChargePage = TeslatlasPage<Charge>
public typealias ChargeSamplePage = TeslatlasPage<ChargeSample>
public typealias StatePage = TeslatlasPage<StateInterval>
public typealias SoftwareUpdatePage = TeslatlasPage<SoftwareUpdate>
public typealias DataQualityPage = TeslatlasPage<DataQualityAssessment>

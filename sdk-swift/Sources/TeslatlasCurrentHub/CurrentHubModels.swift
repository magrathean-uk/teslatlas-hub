import Foundation

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

extension UUID {
  static let currentHubZero = UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
}

public enum CurrentHubError: Error, Equatable, Sendable {
  case invalidBinding(String)
  case invalidDiscoveryURL(String)
  case invalidDiscovery(String)
  case hubIdentityMismatch(expected: UUID, actual: UUID)
  case untrustedOrigin(String)
  case capabilityUnavailable(String)
  case credentialExpired
  case invitationExpired
  case invalidRequest(String)
  case transportFailure(code: Int)
  case unauthorized(requestID: String?)
  case notFound(requestID: String?)
  case serviceUnavailable(requestID: String?)
  case api(statusCode: Int, code: String, message: String, requestID: String?)
  case invalidResponse(statusCode: Int, requestID: String?, reason: String)
}

public struct CurrentHubCredential: Sendable, CustomStringConvertible,
  CustomDebugStringConvertible, CustomReflectable
{
  public let deviceID: UUID
  public let expiresAtMilliseconds: Int64
  private let accessToken: String

  public init(
    deviceID: UUID,
    accessToken: String,
    expiresAtMilliseconds: Int64
  ) throws {
    guard deviceID != UUID.currentHubZero,
      accessToken.utf8.count == 64,
      accessToken.utf8.allSatisfy({ byte in
        (byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102)
      })
    else {
      throw CurrentHubError.invalidRequest(
        "paired-device credential must contain a non-nil device identity and 64-character lowercase hexadecimal access token"
      )
    }
    self.deviceID = deviceID
    self.accessToken = accessToken
    self.expiresAtMilliseconds = expiresAtMilliseconds
  }

  public var description: String { "CurrentHubCredential(<redacted>)" }
  public var debugDescription: String { description }
  public var customMirror: Mirror {
    Mirror(self, children: ["token": "<redacted>"])
  }

  func apply(to request: inout URLRequest) {
    request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
  }
}

public protocol CurrentHubCredentialStore: Sendable {
  func loadCredential() async throws -> CurrentHubCredential?
  func saveCredential(_ credential: CurrentHubCredential) async throws
}

public struct CurrentHubInvitation: Decodable, Sendable,
  CustomStringConvertible, CustomDebugStringConvertible
{
  public let endpoint: URL
  public let pairingID: UUID
  public let expiresAtMilliseconds: Int64
  public let tlsPin: String
  public let pairingURI: URL
  let secret: String

  enum CodingKeys: String, CodingKey {
    case endpoint
    case pairingID = "pairingId"
    case expiresAtMilliseconds = "expiresAtMs"
    case tlsPin
    case pairingURI = "pairingUri"
    case secret
  }

  public var description: String { "CurrentHubInvitation(<redacted>)" }
  public var debugDescription: String { description }
}

struct CurrentHubClaimEnvelope: Decodable, Sendable {
  let deviceID: UUID
  let accessToken: String
  let expiresAtMilliseconds: Int64

  enum CodingKeys: String, CodingKey {
    case deviceID = "device_id"
    case accessToken = "access_token"
    case expiresAtMilliseconds = "expires_at_ms"
  }

  func credential() throws -> CurrentHubCredential {
    try CurrentHubCredential(
      deviceID: deviceID,
      accessToken: accessToken,
      expiresAtMilliseconds: expiresAtMilliseconds
    )
  }
}

public struct CurrentHubHealth: Decodable, Equatable, Sendable {
  public let status: String
  public let version: String
}

public struct CurrentHubReadiness: Decodable, Equatable, Sendable {
  public let status: String
  public let reason: String?
}

public struct CurrentHubDiscovery: Decodable, Equatable, Sendable {
  public let hubID: UUID
  public let protocolName: String
  public let protocolMajor: Int
  public let apiVersions: [String]
  public let capabilities: [String]
  public let version: String
  public let sourceURL: URL
  public let packFormat: String
  public let manifestPublicKey: String?

  enum CodingKeys: String, CodingKey {
    case hubID = "hub_id"
    case protocolName = "protocol"
    case protocolMajor = "protocol_major"
    case apiVersions = "api_versions"
    case capabilities, version
    case sourceURL = "sourceUrl"
    case packFormat = "pack_format"
    case manifestPublicKey
  }
}

public struct CurrentHubVehicle: Decodable, Equatable, Sendable {
  public let vehicleID: UUID
  public let displayName: String?

  enum CodingKeys: String, CodingKey, CaseIterable {
    case vehicleID = "vehicle_id"
    case displayName = "display_name"
  }

  static let requiredWireKeys = Set(CodingKeys.allCases.map(\.rawValue))
}

struct CurrentHubVehicleListEnvelope: Decodable, Equatable, Sendable {
  let vehicles: [CurrentHubVehicle]
}

public struct CurrentHubProjectionCarSettings: Decodable, Equatable, Sendable {
  public let enabled: Bool
  public let useStreamingAPI: Bool
  public let suspendAfterIdleMinutes: Int64
  public let suspendMinutes: Int64
  public let suspendMinutesResolved: Bool
  public let requireNotUnlocked: Bool
  public let freeSupercharging: Bool
  public let lfpBattery: Bool

  enum CodingKeys: String, CodingKey, CaseIterable {
    case enabled
    case useStreamingAPI = "use_streaming_api"
    case suspendAfterIdleMinutes = "suspend_after_idle_min"
    case suspendMinutes = "suspend_min"
    case suspendMinutesResolved = "suspend_min_resolved"
    case requireNotUnlocked = "req_not_unlocked"
    case freeSupercharging = "free_supercharging"
    case lfpBattery = "lfp_battery"
  }
}

public struct CurrentHubProjectionCar: Decodable, Equatable, Sendable {
  public let id: Int64
  public let name: String
  public let model: String
  public let vin: String?
  public let sourceEID: Int64?
  public let sourceVID: Int64?
  public let trimBadging: String?
  public let marketingName: String?
  public let exteriorColour: String?
  public let wheelType: String?
  public let spoilerType: String?
  public let firmwareVersion: String?
  public let efficiencyWhPerKilometre: Double?
  public let settings: CurrentHubProjectionCarSettings

  enum CodingKeys: String, CodingKey {
    case id, name, model, vin
    case sourceEID = "source_eid"
    case sourceVID = "source_vid"
    case trimBadging = "trim_badging"
    case marketingName = "marketing_name"
    case exteriorColour = "exterior_color"
    case wheelType = "wheel_type"
    case spoilerType = "spoiler_type"
    case firmwareVersion = "firmware_version"
    case efficiencyWhPerKilometre = "efficiency_wh_per_km"
    case settings
  }
}

public struct CurrentHubCurrentState: Decodable, Equatable, Sendable {
  public let vehicleID: UUID
  public let observedAtMilliseconds: Int64?
  public let car: CurrentHubProjectionCar?
  public let displayName: String?
  public let state: String?
  public let sinceMilliseconds: Int64?
  public let healthy: Bool?
  public let latitude: Double?
  public let longitude: Double?
  public let heading: Double?
  public let batteryLevel: Int64?
  public let chargingState: String?
  public let usableBatteryLevel: Int64?
  public let idealBatteryRangeKilometres: Double?
  public let estimatedBatteryRangeKilometres: Double?
  public let ratedBatteryRangeKilometres: Double?
  public let chargeEnergyAdded: Double?
  public let speedKilometresPerHour: Int64?
  public let outsideTemperatureCelsius: Double?
  public let insideTemperatureCelsius: Double?
  public let isClimateOn: Bool?
  public let isPreconditioning: Bool?
  public let locked: Bool?
  public let sentryMode: Bool?
  public let pluggedIn: Bool?
  public let scheduledChargingStartTime: Int64?
  public let chargeLimitSOC: Int64?
  public let chargerPowerKilowatts: Double?
  public let windowsOpen: Bool?
  public let driverFrontWindowOpen: Bool?
  public let driverRearWindowOpen: Bool?
  public let passengerFrontWindowOpen: Bool?
  public let passengerRearWindowOpen: Bool?
  public let doorsOpen: Bool?
  public let driverFrontDoorOpen: Bool?
  public let driverRearDoorOpen: Bool?
  public let passengerFrontDoorOpen: Bool?
  public let passengerRearDoorOpen: Bool?
  public let odometerKilometres: Double?
  public let shiftState: String?
  public let chargePortDoorOpen: Bool?
  public let timeToFullChargeHours: Double?
  public let chargerPhases: Int64?
  public let chargerActualCurrentAmperes: Double?
  public let chargerVoltageVolts: Double?
  public let firmwareVersion: String?
  public let updateAvailable: Bool?
  public let updateVersion: String?
  public let updateStatus: String?
  public let isUserPresent: Bool?
  public let geofence: String?
  public let model: String?
  public let trimBadging: String?
  public let exteriorColour: String?
  public let wheelType: String?
  public let spoilerType: String?
  public let trunkOpen: Bool?
  public let frunkOpen: Bool?
  public let elevationMetres: Double?
  public let powerKilowatts: Double?
  public let chargeCurrentRequestAmperes: Int64?
  public let chargeCurrentRequestMaximumAmperes: Int64?
  public let tpmsPressureFrontLeftBar: Double?
  public let tpmsPressureFrontRightBar: Double?
  public let tpmsPressureRearLeftBar: Double?
  public let tpmsPressureRearRightBar: Double?
  public let tpmsSoftWarningFrontLeft: Bool?
  public let tpmsSoftWarningFrontRight: Bool?
  public let tpmsSoftWarningRearLeft: Bool?
  public let tpmsSoftWarningRearRight: Bool?
  public let climateKeeperMode: String?
  public let activeRouteDestination: String?
  public let activeRouteLatitude: Double?
  public let activeRouteLongitude: Double?
  public let activeRouteEnergyAtArrivalPercent: Double?
  public let activeRouteMilesToArrival: Double?
  public let activeRouteMinutesToArrival: Double?
  public let activeRouteTrafficMinutesDelay: Double?
  public let centreDisplayState: Int64?
  public let serviceMode: Bool?
  public let sunRoofState: String?
  public let sunRoofInstalled: Bool?
  public let sunRoofPercentOpen: Int64?
  public let downloadPercent: Int64?
  public let installPercent: Int64?

  enum CodingKeys: String, CodingKey, CaseIterable {
    case vehicleID = "vehicle_id"
    case observedAtMilliseconds = "observed_at_ms"
    case car
    case displayName = "display_name"
    case state
    case sinceMilliseconds = "since"
    case healthy
    case latitude
    case longitude
    case heading
    case batteryLevel = "battery_level"
    case chargingState = "charging_state"
    case usableBatteryLevel = "usable_battery_level"
    case idealBatteryRangeKilometres = "ideal_battery_range_km"
    case estimatedBatteryRangeKilometres = "est_battery_range_km"
    case ratedBatteryRangeKilometres = "rated_battery_range_km"
    case chargeEnergyAdded = "charge_energy_added"
    case speedKilometresPerHour = "speed"
    case outsideTemperatureCelsius = "outside_temp"
    case insideTemperatureCelsius = "inside_temp"
    case isClimateOn = "is_climate_on"
    case isPreconditioning = "is_preconditioning"
    case locked
    case sentryMode = "sentry_mode"
    case pluggedIn = "plugged_in"
    case scheduledChargingStartTime = "scheduled_charging_start_time"
    case chargeLimitSOC = "charge_limit_soc"
    case chargerPowerKilowatts = "charger_power"
    case windowsOpen = "windows_open"
    case driverFrontWindowOpen = "driver_front_window_open"
    case driverRearWindowOpen = "driver_rear_window_open"
    case passengerFrontWindowOpen = "passenger_front_window_open"
    case passengerRearWindowOpen = "passenger_rear_window_open"
    case doorsOpen = "doors_open"
    case driverFrontDoorOpen = "driver_front_door_open"
    case driverRearDoorOpen = "driver_rear_door_open"
    case passengerFrontDoorOpen = "passenger_front_door_open"
    case passengerRearDoorOpen = "passenger_rear_door_open"
    case odometerKilometres = "odometer"
    case shiftState = "shift_state"
    case chargePortDoorOpen = "charge_port_door_open"
    case timeToFullChargeHours = "time_to_full_charge"
    case chargerPhases = "charger_phases"
    case chargerActualCurrentAmperes = "charger_actual_current"
    case chargerVoltageVolts = "charger_voltage"
    case firmwareVersion = "version"
    case updateAvailable = "update_available"
    case updateVersion = "update_version"
    case updateStatus = "update_status"
    case isUserPresent = "is_user_present"
    case geofence
    case model
    case trimBadging = "trim_badging"
    case exteriorColour = "exterior_color"
    case wheelType = "wheel_type"
    case spoilerType = "spoiler_type"
    case trunkOpen = "trunk_open"
    case frunkOpen = "frunk_open"
    case elevationMetres = "elevation"
    case powerKilowatts = "power"
    case chargeCurrentRequestAmperes = "charge_current_request"
    case chargeCurrentRequestMaximumAmperes = "charge_current_request_max"
    case tpmsPressureFrontLeftBar = "tpms_pressure_fl"
    case tpmsPressureFrontRightBar = "tpms_pressure_fr"
    case tpmsPressureRearLeftBar = "tpms_pressure_rl"
    case tpmsPressureRearRightBar = "tpms_pressure_rr"
    case tpmsSoftWarningFrontLeft = "tpms_soft_warning_fl"
    case tpmsSoftWarningFrontRight = "tpms_soft_warning_fr"
    case tpmsSoftWarningRearLeft = "tpms_soft_warning_rl"
    case tpmsSoftWarningRearRight = "tpms_soft_warning_rr"
    case climateKeeperMode = "climate_keeper_mode"
    case activeRouteDestination = "active_route_destination"
    case activeRouteLatitude = "active_route_latitude"
    case activeRouteLongitude = "active_route_longitude"
    case activeRouteEnergyAtArrivalPercent = "active_route_energy_at_arrival"
    case activeRouteMilesToArrival = "active_route_miles_to_arrival"
    case activeRouteMinutesToArrival = "active_route_minutes_to_arrival"
    case activeRouteTrafficMinutesDelay = "active_route_traffic_minutes_delay"
    case centreDisplayState = "center_display_state"
    case serviceMode = "service_mode"
    case sunRoofState = "sun_roof_state"
    case sunRoofInstalled = "sun_roof_installed"
    case sunRoofPercentOpen = "sun_roof_percent_open"
    case downloadPercent = "download_perc"
    case installPercent = "install_perc"
  }

  static let requiredWireKeys = Set(CodingKeys.allCases.map(\.rawValue))
}

public struct CurrentHubDriveCursor: Codable, Equatable, Hashable,
  Sendable, CustomStringConvertible, CustomDebugStringConvertible
{
  public let rawValue: String

  public init(rawValue: String) throws {
    guard !rawValue.isEmpty, rawValue.utf8.count <= 4_096,
      rawValue.unicodeScalars.allSatisfy({
        !CharacterSet.controlCharacters.contains($0)
      })
    else {
      throw CurrentHubError.invalidRequest("drive cursor is empty or contains control characters")
    }
    self.rawValue = rawValue
  }

  public init(from decoder: Decoder) throws {
    let container = try decoder.singleValueContainer()
    let raw = try container.decode(String.self)
    do {
      try self.init(rawValue: raw)
    } catch {
      throw DecodingError.dataCorruptedError(
        in: container,
        debugDescription: "invalid opaque current-Hub drive cursor"
      )
    }
  }

  public func encode(to encoder: Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(rawValue)
  }

  public var description: String { "CurrentHubDriveCursor(<redacted>)" }
  public var debugDescription: String { description }
}

public struct CurrentHubEntityTag: Codable, Equatable, Hashable,
  Sendable, CustomStringConvertible
{
  public let rawValue: String

  public init(rawValue: String) throws {
    let bytes = Array(rawValue.utf8)
    guard bytes.count == 66,
      bytes.first == 0x22, bytes.last == 0x22,
      bytes.dropFirst().dropLast().allSatisfy({ byte in
        (0x30...0x39).contains(byte) || (0x61...0x66).contains(byte)
      })
    else {
      throw CurrentHubError.invalidRequest(
        "drive ETag must be the quoted lowercase SHA-256 emitted by current Hub hub-http-v1@1.0.0"
      )
    }
    self.rawValue = rawValue
  }

  public init(from decoder: Decoder) throws {
    let container = try decoder.singleValueContainer()
    let raw = try container.decode(String.self)
    do {
      try self.init(rawValue: raw)
    } catch {
      throw DecodingError.dataCorruptedError(
        in: container,
        debugDescription: "invalid strong current-Hub entity tag"
      )
    }
  }

  public func encode(to encoder: Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(rawValue)
  }

  public var description: String { rawValue }
}

public struct CurrentHubDrive: Decodable, Equatable, Sendable {
  public let id: Int64
  public let vehicleID: UUID
  public let startDateMilliseconds: Int64
  public let endDateMilliseconds: Int64
  public let distanceKilometres: Double?
  public let durationMinutes: Int64?
  public let efficiencyWhPerKilometre: Double?
  public let outsideTemperatureAverageCelsius: Double?
  public let insideTemperatureAverageCelsius: Double?
  public let maximumSpeedKilometresPerHour: Int64?
  public let maximumPowerKilowatts: Double?
  public let minimumPowerKilowatts: Double?
  public let startIdealRangeKilometres: Double?
  public let endIdealRangeKilometres: Double?
  public let startAddress: String?
  public let endAddress: String?
  public let startGeofence: String?
  public let endGeofence: String?
  public let startLatitude: Double?
  public let startLongitude: Double?
  public let endLatitude: Double?
  public let endLongitude: Double?
  public let startSOC: Int64?
  public let endSOC: Int64?
  public let startRatedRangeKilometres: Double?
  public let endRatedRangeKilometres: Double?
  public let ascentMetres: Int64?
  public let descentMetres: Int64?

  enum CodingKeys: String, CodingKey, CaseIterable {
    case id
    case vehicleID = "vehicle_id"
    case startDateMilliseconds = "start_date_ms"
    case endDateMilliseconds = "end_date_ms"
    case distanceKilometres = "distance_km"
    case durationMinutes = "duration_min"
    case efficiencyWhPerKilometre = "efficiency"
    case outsideTemperatureAverageCelsius = "outside_temp_avg"
    case insideTemperatureAverageCelsius = "inside_temp_avg"
    case maximumSpeedKilometresPerHour = "speed_max"
    case maximumPowerKilowatts = "power_max"
    case minimumPowerKilowatts = "power_min"
    case startIdealRangeKilometres = "start_ideal_range_km"
    case endIdealRangeKilometres = "end_ideal_range_km"
    case startAddress = "start_address"
    case endAddress = "end_address"
    case startGeofence = "start_geofence"
    case endGeofence = "end_geofence"
    case startLatitude = "start_latitude"
    case startLongitude = "start_longitude"
    case endLatitude = "end_latitude"
    case endLongitude = "end_longitude"
    case startSOC = "start_soc"
    case endSOC = "end_soc"
    case startRatedRangeKilometres = "start_rated_range_km"
    case endRatedRangeKilometres = "end_rated_range_km"
    case ascentMetres = "ascent"
    case descentMetres = "descent"
  }

  static let requiredWireKeys = Set(CodingKeys.allCases.map(\.rawValue))
}

struct CurrentHubDrivePageEnvelope: Decodable, Equatable, Sendable {
  let items: [CurrentHubDrive]
  let nextCursor: String?

  enum CodingKeys: String, CodingKey {
    case items
    case nextCursor = "next_cursor"
  }
}

public struct CurrentHubDrivePage: Equatable, Sendable {
  public let items: [CurrentHubDrive]
  public let nextCursor: CurrentHubDriveCursor?

}

public enum CurrentHubDrivePageResult: Equatable, Sendable {
  case modified(CurrentHubDrivePage, eTag: CurrentHubEntityTag)
  case notModified(eTag: CurrentHubEntityTag)
}

public struct CurrentHubDriveQuery: Equatable, Sendable {
  public let fromMilliseconds: Int64?
  public let toMilliseconds: Int64?
  public let limit: Int?
  public let cursor: CurrentHubDriveCursor?

  public init(
    fromMilliseconds: Int64? = nil,
    toMilliseconds: Int64? = nil,
    limit: Int? = nil,
    cursor: CurrentHubDriveCursor? = nil
  ) {
    self.fromMilliseconds = fromMilliseconds
    self.toMilliseconds = toMilliseconds
    self.limit = limit
    self.cursor = cursor
  }
}

struct CurrentHubAPIErrorEnvelope: Decodable, Equatable, Sendable {
  struct APIError: Decodable, Equatable, Sendable {
    let code: String
    let message: String
  }

  let error: APIError
}

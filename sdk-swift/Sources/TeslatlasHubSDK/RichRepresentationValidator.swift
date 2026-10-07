import Foundation

/// Admission for the selected public rich 1.2 profile. This is deliberately
/// package scoped: the current-Hub and v1 compatibility products have separate contracts.
package enum RichRepresentationValidator {
  enum InvalidRepresentation: Error { case invalid }

  indirect enum Rule: Sendable {
    case any
    case type(String)
    case string(Int?, Int?, String?, String?)
    case number(Bool, Double?, Double?, Double?)
    case object([String], [String: Rule], Bool, Int?, Int?)
    case array(Rule, Int?, Int?, Bool)
    case values([TeslatlasJSONValue])
    case oneOf([Rule])
    case allOf([Rule])
    case conditional(Rule, Rule)
    case reference(String)
  }

  package static func validate(_ data: Data, representation: String) throws {
    let value = try JSONDecoder().decode(TeslatlasJSONValue.self, from: data)
    try validate(value, representation: representation)
  }

  static func validate(_ value: TeslatlasJSONValue, representation: String) throws {
    guard let rule = rules[representation], accepts(value, rule) else {
      throw InvalidRepresentation.invalid
    }
  }

  private static func accepts(_ value: TeslatlasJSONValue, _ rule: Rule) -> Bool {
    switch rule {
    case .any: return true
    case .reference(let name):
      guard let referenced = rules[name] else { return false }
      return accepts(value, referenced)
    case .type(let name):
      switch (name, value) {
      case ("null", .null), ("boolean", .bool), ("object", .object),
        ("array", .array), ("string", .string): return true
      default: return false
      }
    case .values(let values): return values.contains(value)
    case .string(let minimum, let maximum, let pattern, let format):
      guard case .string(let text) = value else { return false }
      // JSON Schema length counts Unicode scalar values, not grapheme clusters.
      let length = text.unicodeScalars.count
      if let minimum, length < minimum { return false }
      if let maximum, length > maximum { return false }
      if let pattern, text.range(of: pattern, options: .regularExpression) == nil {
        return false
      }
      if format == "date-time" { return TeslatlasTimestamp(text) != nil }
      if format == "uri" {
        guard let components = URLComponents(string: text), components.scheme != nil else {
          return false
        }
      }
      if format == "uri" || format == "uri-reference" {
        return !text.unicodeScalars.contains { $0.value <= 0x20 || $0.value == 0x7f }
          && URLComponents(string: text) != nil
      }
      return true
    case .number(let integer, let minimum, let maximum, let exclusiveMaximum):
      let number: Double
      switch value {
      case .integer(let raw): number = Double(raw)
      case .number(let raw):
        guard !integer || raw.rounded(.towardZero) == raw else { return false }
        number = raw
      default: return false
      }
      guard number.isFinite else { return false }
      if let minimum, number < minimum { return false }
      if let maximum, number > maximum { return false }
      if let exclusiveMaximum, number >= exclusiveMaximum { return false }
      return true
    case .object(let required, let properties, let allowExtra, let minimum, let maximum):
      guard case .object(let object) = value,
        required.allSatisfy({ object[$0] != nil }),
        allowExtra || object.keys.allSatisfy({ properties[$0] != nil })
      else { return false }
      if let minimum, object.count < minimum { return false }
      if let maximum, object.count > maximum { return false }
      return properties.allSatisfy { key, rule in
        guard let property = object[key] else { return true }
        return accepts(property, rule)
      }
    case .array(let itemRule, let minimum, let maximum, let unique):
      guard case .array(let items) = value else { return false }
      if let minimum, items.count < minimum { return false }
      if let maximum, items.count > maximum { return false }
      if unique {
        for index in items.indices where items[..<index].contains(items[index]) { return false }
      }
      return items.allSatisfy { accepts($0, itemRule) }
    case .oneOf(let alternatives):
      return alternatives.filter { accepts(value, $0) }.count == 1
    case .allOf(let constraints): return constraints.allSatisfy { accepts(value, $0) }
    case .conditional(let condition, let consequence):
      return !accepts(value, condition) || accepts(value, consequence)
    }
  }

  // Bound to urn:teslatlas:protocol:schema:*:1.2.0. Rules preserve each schema
  // required field, nullable union, numeric domain and permitted extension object.
  private static let rules: [String: Rule] = [
    "event.envelope": .object(
      ["event_id", "event_type", "occurred_at", "vehicle_id", "resource_id", "revision", "data"],
      ["event_id": .reference("common.opaque_id"), "event_type": .type("string"),
       "occurred_at": .reference("common.utc_timestamp"),
       "vehicle_id": .oneOf([.reference("common.opaque_id"), .type("null")]),
       "resource_id": .oneOf([.reference("common.opaque_id"), .type("null")]),
       "revision": .reference("common.revision"), "data": .type("object")],
      false, nil, nil),
    "common.semver": .string(nil, nil, "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$", nil),
    "common.utc_timestamp": .string(nil, nil, "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\\.[0-9]{3}Z$", "date-time"),
    "common.opaque_id": .string(3, 128, "^[a-z][a-z0-9_~-]+$", nil),
    "common.actor_id": .string(3, 128, "^(device|service|user)_[a-z0-9_~-]+$", nil),
    "common.sha256": .string(nil, nil, "^[0-9a-f]{64}$", nil),
    "common.cursor": .string(16, 2048, "^[A-Za-z0-9._~-]+$", nil),
    "common.revision": .number(true, 1, nil, nil),
    "common.source": .values([.string("owner_api"), .string("legacy_stream"), .string("fleet_api"), .string("fleet_telemetry"), .string("teslamate_mirror"), .string("teslamate_import"), .string("manual_import"), .string("ble_gap_fill")]),
    "common.quality": .values([.string("complete"), .string("partial"), .string("degraded")]),
    "common.quality_flag": .values([.string("duplicate"), .string("delayed"), .string("reordered"), .string("gap_before"), .string("gap_after"), .string("clock_skew"), .string("provider_conflict"), .string("inferred"), .string("stale"), .string("invalid_for_context")]),
    "common.location": .object(["latitude", "longitude"], ["latitude": .number(false, -90, 90, nil), "longitude": .number(false, -180, 180, nil), "altitude_m": .oneOf([.number(false, nil, nil, nil), .type("null")])], false, nil, nil),
    "resources.vehicle_state": .values([.string("online"), .string("asleep"), .string("offline"), .string("driving"), .string("charging"), .string("updating"), .string("unknown")]),
    "resources.vehicle": .object(["resource_type", "vehicle_id", "display_name", "state", "last_observed_at", "revision"], ["resource_type": .values([.string("vehicle")]), "vehicle_id": .reference("common.opaque_id"), "display_name": .string(1, 128, nil, nil), "state": .reference("resources.vehicle_state"), "last_observed_at": .reference("common.utc_timestamp"), "revision": .reference("common.revision")], false, nil, nil),
    "resources.current_state": .object(["resource_type", "vehicle_id", "observed_at", "revision", "state", "battery_level_percent", "range_km", "odometer_km", "locked", "climate_on", "charging_state", "location", "quality"], ["resource_type": .values([.string("current_state")]), "vehicle_id": .reference("common.opaque_id"), "observed_at": .reference("common.utc_timestamp"), "revision": .reference("common.revision"), "state": .reference("resources.vehicle_state"), "battery_level_percent": .oneOf([.number(false, 0, 100, nil), .type("null")]), "range_km": .oneOf([.number(false, 0, nil, nil), .type("null")]), "odometer_km": .oneOf([.number(false, 0, nil, nil), .type("null")]), "inside_temperature_c": .oneOf([.number(false, nil, nil, nil), .type("null")]), "outside_temperature_c": .oneOf([.number(false, nil, nil, nil), .type("null")]), "locked": .oneOf([.type("boolean"), .type("null")]), "climate_on": .oneOf([.type("boolean"), .type("null")]), "charging_state": .values([.string("charging"), .string("stopped"), .string("complete"), .string("disconnected"), .string("unknown"), .null]), "location": .oneOf([.reference("common.location"), .type("null")]), "quality": .reference("data-quality")], false, nil, nil),
    "resources.drive": .object(["resource_type", "drive_id", "vehicle_id", "start_at", "end_at", "start_odometer_km", "end_odometer_km", "distance_km", "duration_seconds", "energy_used_kwh", "quality"], ["resource_type": .values([.string("drive")]), "drive_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "start_at": .reference("common.utc_timestamp"), "end_at": .oneOf([.reference("common.utc_timestamp"), .type("null")]), "start_odometer_km": .oneOf([.number(false, 0, nil, nil), .type("null")]), "end_odometer_km": .oneOf([.number(false, 0, nil, nil), .type("null")]), "distance_km": .oneOf([.number(false, 0, nil, nil), .type("null")]), "duration_seconds": .oneOf([.number(true, 0, nil, nil), .type("null")]), "energy_used_kwh": .oneOf([.number(false, 0, nil, nil), .type("null")]), "efficiency_wh_per_km": .oneOf([.number(false, 0, nil, nil), .type("null")]), "quality": .reference("data-quality")], false, nil, nil),
    "resources.position": .object(["resource_type", "position_id", "drive_id", "vehicle_id", "observed_at", "location", "speed_km_h", "heading_degrees", "quality_flags"], ["resource_type": .values([.string("position")]), "position_id": .reference("common.opaque_id"), "drive_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "observed_at": .reference("common.utc_timestamp"), "location": .reference("common.location"), "speed_km_h": .oneOf([.number(false, 0, nil, nil), .type("null")]), "heading_degrees": .oneOf([.number(false, 0, nil, 360), .type("null")]), "quality_flags": .array(.reference("common.quality_flag"), nil, nil, true)], false, nil, nil),
    "resources.charge": .object(["resource_type", "charge_id", "vehicle_id", "start_at", "end_at", "charging_type", "energy_added_kwh", "start_battery_percent", "end_battery_percent", "location", "quality"], ["resource_type": .values([.string("charge")]), "charge_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "start_at": .reference("common.utc_timestamp"), "end_at": .oneOf([.reference("common.utc_timestamp"), .type("null")]), "charging_type": .values([.string("ac"), .string("dc"), .string("unknown")]), "energy_added_kwh": .oneOf([.number(false, 0, nil, nil), .type("null")]), "start_battery_percent": .oneOf([.number(false, 0, 100, nil), .type("null")]), "end_battery_percent": .oneOf([.number(false, 0, 100, nil), .type("null")]), "location": .oneOf([.reference("common.location"), .type("null")]), "quality": .reference("data-quality")], false, nil, nil),
    "resources.charge_sample": .object(["resource_type", "charge_sample_id", "charge_id", "vehicle_id", "observed_at", "battery_level_percent", "power_kw", "energy_added_kwh", "voltage_v", "current_a", "phases", "quality_flags"], ["resource_type": .values([.string("charge_sample")]), "charge_sample_id": .reference("common.opaque_id"), "charge_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "observed_at": .reference("common.utc_timestamp"), "battery_level_percent": .oneOf([.number(false, 0, 100, nil), .type("null")]), "power_kw": .oneOf([.number(false, 0, nil, nil), .type("null")]), "energy_added_kwh": .oneOf([.number(false, 0, nil, nil), .type("null")]), "voltage_v": .oneOf([.number(false, 0, nil, nil), .type("null")]), "current_a": .oneOf([.number(false, 0, nil, nil), .type("null")]), "phases": .oneOf([.number(true, 1, 3, nil), .type("null")]), "quality_flags": .array(.reference("common.quality_flag"), nil, nil, true)], false, nil, nil),
    "resources.state_interval": .object(["resource_type", "state_id", "vehicle_id", "state", "start_at", "end_at", "quality"], ["resource_type": .values([.string("state_interval")]), "state_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "state": .reference("resources.vehicle_state"), "start_at": .reference("common.utc_timestamp"), "end_at": .oneOf([.reference("common.utc_timestamp"), .type("null")]), "quality": .reference("data-quality")], false, nil, nil),
    "resources.software_update": .object(["resource_type", "update_id", "vehicle_id", "status", "version", "first_seen_at", "last_seen_at", "installed_at", "quality"], ["resource_type": .values([.string("software_update")]), "update_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "status": .values([.string("available"), .string("downloading"), .string("installing"), .string("succeeded"), .string("failed")]), "version": .string(1, 64, nil, nil), "first_seen_at": .reference("common.utc_timestamp"), "last_seen_at": .reference("common.utc_timestamp"), "installed_at": .oneOf([.reference("common.utc_timestamp"), .type("null")]), "quality": .reference("data-quality")], false, nil, nil),
    "resources.vehicle_page": .object(["resource_type", "items", "next_cursor", "snapshot_revision", "generated_at"], ["resource_type": .values([.string("vehicle_page")]), "items": .array(.reference("resources.vehicle"), nil, nil, false), "next_cursor": .oneOf([.reference("common.cursor"), .type("null")]), "snapshot_revision": .reference("common.opaque_id"), "generated_at": .reference("common.utc_timestamp")], false, nil, nil),
    "resources.drive_page": .object(["resource_type", "items", "next_cursor", "snapshot_revision", "generated_at"], ["resource_type": .values([.string("drive_page")]), "items": .array(.reference("resources.drive"), nil, nil, false), "next_cursor": .oneOf([.reference("common.cursor"), .type("null")]), "snapshot_revision": .reference("common.opaque_id"), "generated_at": .reference("common.utc_timestamp")], false, nil, nil),
    "resources.position_page": .object(["resource_type", "items", "next_cursor", "snapshot_revision", "generated_at"], ["resource_type": .values([.string("position_page")]), "items": .array(.reference("resources.position"), nil, nil, false), "next_cursor": .oneOf([.reference("common.cursor"), .type("null")]), "snapshot_revision": .reference("common.opaque_id"), "generated_at": .reference("common.utc_timestamp")], false, nil, nil),
    "resources.charge_page": .object(["resource_type", "items", "next_cursor", "snapshot_revision", "generated_at"], ["resource_type": .values([.string("charge_page")]), "items": .array(.reference("resources.charge"), nil, nil, false), "next_cursor": .oneOf([.reference("common.cursor"), .type("null")]), "snapshot_revision": .reference("common.opaque_id"), "generated_at": .reference("common.utc_timestamp")], false, nil, nil),
    "resources.charge_sample_page": .object(["resource_type", "items", "next_cursor", "snapshot_revision", "generated_at"], ["resource_type": .values([.string("charge_sample_page")]), "items": .array(.reference("resources.charge_sample"), nil, nil, false), "next_cursor": .oneOf([.reference("common.cursor"), .type("null")]), "snapshot_revision": .reference("common.opaque_id"), "generated_at": .reference("common.utc_timestamp")], false, nil, nil),
    "resources.state_page": .object(["resource_type", "items", "next_cursor", "snapshot_revision", "generated_at"], ["resource_type": .values([.string("state_page")]), "items": .array(.reference("resources.state_interval"), nil, nil, false), "next_cursor": .oneOf([.reference("common.cursor"), .type("null")]), "snapshot_revision": .reference("common.opaque_id"), "generated_at": .reference("common.utc_timestamp")], false, nil, nil),
    "resources.update_page": .object(["resource_type", "items", "next_cursor", "snapshot_revision", "generated_at"], ["resource_type": .values([.string("update_page")]), "items": .array(.reference("resources.software_update"), nil, nil, false), "next_cursor": .oneOf([.reference("common.cursor"), .type("null")]), "snapshot_revision": .reference("common.opaque_id"), "generated_at": .reference("common.utc_timestamp")], false, nil, nil),
    "resources.data_quality_page": .object(["resource_type", "items", "next_cursor", "snapshot_revision", "generated_at"], ["resource_type": .values([.string("data_quality_page")]), "items": .array(.reference("data-quality"), nil, nil, false), "next_cursor": .oneOf([.reference("common.cursor"), .type("null")]), "snapshot_revision": .reference("common.opaque_id"), "generated_at": .reference("common.utc_timestamp")], false, nil, nil),
    "data-quality": .allOf([.object(["subject_type", "subject_id", "quality", "sources", "gap_count", "largest_gap_seconds", "derived_fields", "projection_version", "assessed_at", "issues"], ["subject_type": .values([.string("vehicle"), .string("projection"), .string("drive"), .string("charge"), .string("state"), .string("update")]), "subject_id": .reference("common.opaque_id"), "quality": .reference("common.quality"), "sources": .array(.reference("common.source"), nil, nil, true), "gap_count": .number(true, 0, nil, nil), "largest_gap_seconds": .number(true, 0, nil, nil), "derived_fields": .array(.string(nil, nil, "^[a-z][a-z0-9_]*(?:\\.[a-z][a-z0-9_]*)*$", nil), nil, nil, true), "projection_version": .reference("common.semver"), "assessed_at": .reference("common.utc_timestamp"), "issues": .array(.reference("data-quality.issue"), nil, nil, false)], false, nil, nil), .allOf([.conditional(.object(["quality"], ["quality": .values([.string("complete")])], true, nil, nil), .object([], ["gap_count": .values([.integer(0)]), "largest_gap_seconds": .values([.integer(0)]), "issues": .array(.any, nil, 0, false)], true, nil, nil)), .conditional(.object(["gap_count"], ["gap_count": .values([.integer(0)])], true, nil, nil), .object([], ["largest_gap_seconds": .values([.integer(0)])], true, nil, nil))])]),
    "data-quality.issue": .object(["code", "severity", "message"], ["code": .values([.string("missing_interval"), .string("late_observation"), .string("reordered_observation"), .string("conflicting_duplicate"), .string("clock_skew"), .string("source_unavailable"), .string("invalid_for_context")]), "severity": .values([.string("info"), .string("warning"), .string("error")]), "message": .string(1, 512, nil, nil), "from": .reference("common.utc_timestamp"), "to": .reference("common.utc_timestamp"), "affected_fields": .array(.string(nil, nil, "^[a-z][a-z0-9_]*(?:\\.[a-z][a-z0-9_]*)*$", nil), nil, nil, true)], false, nil, nil),
    "observation": .object(["observation_id", "vehicle_id", "source", "provider_timestamp", "received_timestamp", "source_sequence", "field", "typed_value", "validity", "quality_flags", "raw_payload_hash", "collector_version", "projection_version"], ["observation_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "source": .reference("common.source"), "provider_timestamp": .reference("common.utc_timestamp"), "received_timestamp": .reference("common.utc_timestamp"), "source_sequence": .oneOf([.string(1, 256, nil, nil), .number(true, 0, nil, nil), .type("null")]), "field": .string(nil, 256, "^[a-z][a-z0-9_]*(?:\\.[a-z][a-z0-9_]*)*$", nil), "typed_value": .reference("observation.typed_value"), "validity": .values([.string("valid"), .string("suspect"), .string("invalid")]), "quality_flags": .array(.reference("common.quality_flag"), nil, nil, true), "raw_payload_hash": .reference("common.sha256"), "collector_version": .reference("common.semver"), "projection_version": .reference("common.semver")], false, nil, nil),
    "observation.typed_value": .oneOf([.reference("observation.null_value"), .reference("observation.boolean_value"), .reference("observation.integer_value"), .reference("observation.number_value"), .reference("observation.string_value"), .reference("observation.object_value"), .reference("observation.array_value")]),
    "observation.unit": .values([.string("percent"), .string("km"), .string("km_h"), .string("celsius"), .string("kwh"), .string("kw"), .string("volts"), .string("amps"), .string("degrees"), .string("seconds"), .string("boolean"), .string("text"), .string("none")]),
    "observation.null_value": .object(["kind", "value"], ["kind": .values([.string("null")]), "value": .type("null")], false, nil, nil),
    "observation.boolean_value": .object(["kind", "value", "unit"], ["kind": .values([.string("boolean")]), "value": .type("boolean"), "unit": .values([.string("boolean")])], false, nil, nil),
    "observation.integer_value": .object(["kind", "value", "unit"], ["kind": .values([.string("integer")]), "value": .number(true, nil, nil, nil), "unit": .reference("observation.unit")], false, nil, nil),
    "observation.number_value": .object(["kind", "value", "unit"], ["kind": .values([.string("number")]), "value": .number(false, nil, nil, nil), "unit": .reference("observation.unit")], false, nil, nil),
    "observation.string_value": .object(["kind", "value", "unit"], ["kind": .values([.string("string")]), "value": .string(nil, 4096, nil, nil), "unit": .reference("observation.unit")], false, nil, nil),
    "observation.object_value": .object(["kind", "value"], ["kind": .values([.string("object")]), "value": .object([], [:], true, nil, nil)], false, nil, nil),
    "observation.array_value": .object(["kind", "value"], ["kind": .values([.string("array")]), "value": .array(.any, nil, nil, false)], false, nil, nil),
    "metadata.metadata_kind": .values([.string("geofence"), .string("location_name"), .string("tag"), .string("note"), .string("trip_group"), .string("charge_cost_correction"), .string("electricity_tariff"), .string("favourite_location"), .string("session_override")]),
    "metadata.metadata_target": .object(["resource_type", "resource_id"], ["resource_type": .values([.string("vehicle"), .string("drive"), .string("charge"), .string("location"), .string("installation")]), "resource_id": .reference("common.opaque_id")], false, nil, nil),
    "metadata.metadata_record": .object(["metadata_id", "vehicle_id", "kind", "target", "value", "revision", "created_at", "created_by", "updated_at", "updated_by", "audit"], ["metadata_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "kind": .reference("metadata.metadata_kind"), "target": .reference("metadata.metadata_target"), "value": .object([], [:], true, nil, 64), "revision": .reference("common.revision"), "created_at": .reference("common.utc_timestamp"), "created_by": .reference("common.actor_id"), "updated_at": .reference("common.utc_timestamp"), "updated_by": .reference("common.actor_id"), "audit": .array(.reference("metadata.metadata_audit_event"), 1, nil, false)], false, nil, nil),
    "metadata.metadata_tombstone": .object(["metadata_id", "vehicle_id", "kind", "target", "audit"], ["metadata_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "kind": .reference("metadata.metadata_kind"), "target": .reference("metadata.metadata_target"), "audit": .object(["history", "deletion"], ["history": .array(.reference("metadata.metadata_audit_event"), 1, nil, false), "deletion": .reference("metadata.metadata_deletion_event")], false, nil, nil)], false, nil, nil),
    "metadata.metadata_audit_event": .allOf([.object(["revision", "action", "at", "actor_id", "previous_hash", "new_hash"], ["revision": .reference("common.revision"), "action": .values([.string("created"), .string("updated")]), "at": .reference("common.utc_timestamp"), "actor_id": .reference("common.actor_id"), "previous_hash": .oneOf([.reference("common.sha256"), .type("null")]), "new_hash": .oneOf([.reference("common.sha256"), .type("null")])], false, nil, nil), .allOf([.conditional(.object(["action"], ["action": .values([.string("created")])], true, nil, nil), .object([], ["previous_hash": .type("null"), "new_hash": .reference("common.sha256")], true, nil, nil)), .conditional(.object(["action"], ["action": .values([.string("updated")])], true, nil, nil), .object([], ["previous_hash": .reference("common.sha256"), "new_hash": .reference("common.sha256")], true, nil, nil))])]),
    "metadata.metadata_deletion_event": .object(["revision", "action", "at", "actor_id"], ["revision": .reference("common.revision"), "action": .values([.string("deleted")]), "at": .reference("common.utc_timestamp"), "actor_id": .reference("common.actor_id")], false, nil, nil),
    "command.command_name": .string(nil, nil, "^[a-z][a-z0-9_]{1,63}$", nil),
    "command.command_class": .values([.string("climate"), .string("charging"), .string("access"), .string("nuisance"), .string("vehicle_state")]),
    "command.command_state": .values([.string("accepted"), .string("authorising"), .string("sent"), .string("provider_acknowledged"), .string("verifying"), .string("succeeded"), .string("failed"), .string("indeterminate"), .string("expired"), .string("cancelled")]),
    "command.command_job": .allOf([.object(["command_id", "vehicle_id", "command", "command_class", "state", "created_at", "updated_at", "expires_at", "attempt_count", "retry_policy", "audit", "links"], ["command_id": .reference("common.opaque_id"), "vehicle_id": .reference("common.opaque_id"), "command": .reference("command.command_name"), "command_class": .reference("command.command_class"), "state": .reference("command.command_state"), "created_at": .reference("common.utc_timestamp"), "updated_at": .reference("common.utc_timestamp"), "expires_at": .reference("common.utc_timestamp"), "attempt_count": .number(true, 0, 16, nil), "retry_policy": .values([.string("none"), .string("state_verified")]), "result": .object([], [:], true, nil, nil), "error": .reference("error"), "audit": .array(.reference("command.command_audit_event"), 1, nil, false), "links": .object(["self"], ["self": .string(nil, nil, nil, "uri-reference")], false, nil, nil)], false, nil, nil), .allOf([.conditional(.object(["state"], ["state": .values([.string("succeeded")])], true, nil, nil), .object(["result"], [:], true, nil, nil)), .conditional(.object(["state"], ["state": .values([.string("failed")])], true, nil, nil), .object(["error"], [:], true, nil, nil))])]),
    "command.command_audit_event": .object(["state", "at", "actor_id"], ["state": .reference("command.command_state"), "at": .reference("common.utc_timestamp"), "actor_id": .reference("common.actor_id"), "note": .string(nil, 512, nil, nil)], false, nil, nil),
    "error": .object(["type", "title", "status", "code", "request_id", "instance", "retryable"], ["type": .string(nil, nil, nil, "uri"), "title": .string(1, 160, nil, nil), "status": .number(true, 400, 599, nil), "detail": .string(nil, 2048, nil, nil), "instance": .string(nil, nil, nil, "uri-reference"), "code": .values([.string("invalid_request"), .string("invalid_time_range"), .string("unsupported_protocol_version"), .string("invalid_cursor"), .string("cursor_expired"), .string("cursor_query_mismatch"), .string("cursor_scope_changed"), .string("unauthorized"), .string("forbidden"), .string("not_found"), .string("precondition_required"), .string("metadata_revision_conflict"), .string("idempotency_conflict"), .string("command_not_supported"), .string("command_not_permitted"), .string("command_expired"), .string("event_replay_expired"), .string("event_id_invalid"), .string("rate_limited"), .string("request_too_large"), .string("range_too_large"), .string("concurrency_limit"), .string("internal_error"), .string("unavailable")]), "request_id": .reference("common.opaque_id"), "retryable": .type("boolean"), "field_errors": .array(.reference("error.field_error"), nil, nil, false), "retry_after_seconds": .number(true, 0, 86400, nil)], true, nil, nil),
    "error.field_error": .object(["field", "code"], ["field": .string(1, 256, nil, nil), "code": .string(nil, nil, "^[a-z][a-z0-9_]+$", nil), "message": .string(nil, 512, nil, nil)], false, nil, nil),
  ]
}

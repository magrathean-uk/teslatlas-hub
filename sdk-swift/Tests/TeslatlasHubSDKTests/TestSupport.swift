import Foundation
import XCTest

@testable import TeslatlasHubSDK

enum TestURLs {
  static let discovery = URL(
    string: "https://hub.example.invalid/.well-known/teslatlas-hub"
  )!
}

enum TestHeaders {
  static func json(eTag: String, version: String) -> [String: String] {
    [
      "Cache-Control": "private, max-age=0, must-revalidate",
      "Content-Type": "application/json",
      "ETag": eTag,
      "Teslatlas-Protocol-Version": version,
      "Vary": "Authorization, Teslatlas-Protocol-Version",
    ]
  }
}

extension TeslatlasHTTPResponse {
  static func json(
    status: Int,
    body: String,
    eTag: String,
    version: String
  ) -> Self {
    Self(
      statusCode: status,
      headers: TestHeaders.json(eTag: eTag, version: version),
      body: Data(body.utf8)
    )
  }
}

actor ScriptedHTTPTransport: TeslatlasHTTPTransport {
  private var queuedResponses: [TeslatlasHTTPResponse]
  private(set) var requests: [URLRequest] = []

  init(_ queuedResponses: [TeslatlasHTTPResponse]) {
    self.queuedResponses = queuedResponses
  }

  func send(_ request: URLRequest) async throws -> TeslatlasHTTPResponse {
    requests.append(request)
    guard !queuedResponses.isEmpty else {
      throw TestTransportError.noResponse
    }
    return queuedResponses.removeFirst()
  }
}

enum TestTransportError: Error {
  case noResponse
}

func assertThrowsErrorAsync<T>(
  _ expression: @autoclosure () async throws -> T,
  _ verify: (Error) -> Void = { _ in }
) async {
  do {
    _ = try await expression()
    XCTFail("Expected expression to throw")
  } catch {
    verify(error)
  }
}

enum TestDocuments {
  static let discovery = #"""
    {
      "hub_id": "urn:uuid:018f18d2-6f45-7b3c-8a91-3c7286a10d42",
      "protocol": {
        "current_version": "1.2.0",
        "supported_versions": ["1.0.0", "1.1.0", "1.2.0"],
        "minimum_client_version": "1.0.0",
        "version_header": "Teslatlas-Protocol-Version",
        "selection": "highest-compatible-not-newer-than-client"
      },
      "capabilities": [
        {
          "id": "query.vehicles",
          "version": "1.0.0",
          "introduced_in": "1.0.0",
          "status": "stable",
          "href": "/v1/vehicles"
        }
      ] , "endpoints": {
        "well_known": "https://hub.example.invalid/.well-known/teslatlas-hub",
        "api": "https://hub.example.invalid/v1",
        "events": "https://hub.example.invalid/v1/events",
        "openapi": "https://hub.example.invalid/openapi/teslatlas-v1.openapi.json"
      },
      "limits": {
        "max_request_body_bytes": 262144,
        "default_page_size": 100,
        "max_page_size": 500,
        "max_history_range_days": 366,
        "max_dense_range_days": 31,
        "max_concurrent_requests": 8,
        "max_sse_connections": 2,
        "event_replay_retention_seconds": 86400,
        "idempotency_retention_seconds": 86400
      }
    }
    """#

  static let currentState = #"""
    {
      "resource_type": "current_state",
      "vehicle_id": "vehicle_demo_alpha",
      "observed_at": "2026-08-30T12:00:00.000Z",
      "revision": 42,
      "state": "online",
      "battery_level_percent": 78,
      "range_km": 338.4,
      "odometer_km": 12000.5,
      "inside_temperature_c": 21.5,
      "outside_temperature_c": 17,
      "locked": true,
      "climate_on": false,
      "charging_state": "disconnected",
      "location": null,
      "quality": {
        "subject_type": "vehicle",
        "subject_id": "vehicle_demo_alpha",
        "quality": "complete",
        "sources": ["fleet_telemetry"],
        "gap_count": 0,
        "largest_gap_seconds": 0,
        "derived_fields": [],
        "projection_version": "3.1.0",
        "assessed_at": "2026-08-30T12:00:01.250Z",
        "issues": []
      }
    }
    """#

  static let vehiclePage = #"""
    {
      "resource_type": "vehicle_page",
      "items": [
        {
          "resource_type": "vehicle",
          "vehicle_id": "vehicle_demo_alpha",
          "display_name": "Roadrunner",
          "state": "online",
          "last_observed_at": "2026-08-30T12:00:00.000Z",
          "revision": 42
        }
      ],
      "next_cursor": "abcDEF0123._~-xyz",
      "snapshot_revision": "snapshot_demo_0001",
      "generated_at": "2026-08-30T12:00:01.000Z"
    }
    """#

  static let problemWithExtension = #"""
    {
      "type": "urn:teslatlas:problem:invalid-cursor",
      "title": "Invalid cursor",
      "status": 400,
      "detail": "The cursor is invalid.",
      "instance": "/requests/request_demo_0001",
      "code": "invalid_cursor",
      "request_id": "request_demo_0001",
      "retryable": false,
      "future_extension": {"safe_to_ignore": true}
    }
    """#

  static let readDiscovery = discovery.replacingOccurrences(
    of: "] , \"endpoints\"",
    with: #"""
      ,
        {"id":"query.history","version":"1.0.0","introduced_in":"1.0.0","status":"stable","href":"/v1/vehicles/{vehicle_id}/drives"},
        {"id":"events.sse","version":"1.0.0","introduced_in":"1.0.0","status":"stable","href":"/v1/events"},
        {"id":"data-quality","version":"1.0.0","introduced_in":"1.0.0","status":"stable","href":"/v1/data-quality"}
      ] , "endpoints"
      """#
  )

  static let quality = #"""
    {
      "subject_type":"vehicle","subject_id":"vehicle_demo_alpha",
      "quality":"complete","sources":["fleet_telemetry"],"gap_count":0,
      "largest_gap_seconds":0,"derived_fields":[],"projection_version":"3.1.0",
      "assessed_at":"2026-08-30T12:00:01.250Z","issues":[]
    }
    """#

  static let drive = #"""
    {
      "resource_type":"drive","drive_id":"drive_demo_0001",
      "vehicle_id":"vehicle_demo_alpha","start_at":"2026-08-30T10:00:00.000Z",
      "end_at":"2026-08-30T10:30:00.000Z","start_odometer_km":12000,
      "end_odometer_km":12020,"distance_km":20,"duration_seconds":1800,
      "energy_used_kwh":3.5,"efficiency_wh_per_km":175,"quality":\#(quality)
    }
    """#

  static let drivePage = page(
    type: "drive_page",
    item: drive,
    snapshot: "snapshot_drive_0001"
  )

  static let positionPage = page(
    type: "position_page",
    item: #"""
      {
        "resource_type":"position","position_id":"position_demo_0001",
        "drive_id":"drive_demo_0001","vehicle_id":"vehicle_demo_alpha",
        "observed_at":"2026-08-30T10:01:00.000Z",
        "location":{"latitude":51.5,"longitude":-0.1,"altitude_m":12},
        "speed_km_h":42,"heading_degrees":180,"quality_flags":[]
      }
      """#,
    snapshot: "snapshot_position_0001"
  )

  static let charge = #"""
    {
      "resource_type":"charge","charge_id":"charge_demo_0001",
      "vehicle_id":"vehicle_demo_alpha","start_at":"2026-08-29T20:00:00.000Z",
      "end_at":"2026-08-29T22:00:00.000Z","charging_type":"ac",
      "energy_added_kwh":22,"start_battery_percent":30,"end_battery_percent":80,
      "location":null,"quality":\#(quality)
    }
    """#

  static let chargePage = page(
    type: "charge_page",
    item: charge,
    snapshot: "snapshot_charge_0001"
  )

  static let chargeSamplePage = page(
    type: "charge_sample_page",
    item: #"""
      {
        "resource_type":"charge_sample","charge_sample_id":"sample_demo_0001",
        "charge_id":"charge_demo_0001","vehicle_id":"vehicle_demo_alpha",
        "observed_at":"2026-08-29T20:01:00.000Z","battery_level_percent":31,
        "power_kw":11,"energy_added_kwh":0.18,"voltage_v":230,"current_a":16,
        "phases":3,"quality_flags":[]
      }
      """#,
    snapshot: "snapshot_sample_0001"
  )

  static let statePage = page(
    type: "state_page",
    item: #"""
      {
        "resource_type":"state_interval","state_id":"state_demo_0001",
        "vehicle_id":"vehicle_demo_alpha","state":"online",
        "start_at":"2026-08-30T09:00:00.000Z","end_at":null,
        "quality":\#(quality)
      }
      """#,
    snapshot: "snapshot_state_0001"
  )

  static let updatePage = page(
    type: "update_page",
    item: #"""
      {
        "resource_type":"software_update","update_id":"update_demo_0001",
        "vehicle_id":"vehicle_demo_alpha","status":"available","version":"2026.26.7",
        "first_seen_at":"2026-08-30T08:00:00.000Z",
        "last_seen_at":"2026-08-30T12:00:00.000Z","installed_at":null,
        "quality":\#(quality)
      }
      """#,
    snapshot: "snapshot_update_0001"
  )

  static let dataQualityPage = page(
    type: "data_quality_page",
    item: quality,
    snapshot: "snapshot_quality_0001"
  )

  static let currentStateEvent = #"""
    {
      "event_id":"event_demo_0042","event_type":"vehicle.current.changed",
      "occurred_at":"2026-08-30T12:00:01.250Z",
      "vehicle_id":"vehicle_demo_alpha","resource_id":"vehicle_demo_alpha",
      "revision":42,"data":\#(currentState)
    }
    """#

  static let currentStateEventLine = currentStateEvent.replacingOccurrences(
    of: "\n",
    with: ""
  )

  static let commandDiscovery = readDiscovery.replacingOccurrences(
    of: "] , \"endpoints\"",
    with: #"""
      ,{
        "id":"commands.async","version":"1.0.0","introduced_in":"1.1.0",
        "status":"stable","href":"/v1/commands","commands":[{
          "name":"set_charge_limit","command_class":"charging",
          "required_scope":"vehicle.commands.charging","retry_policy":"state_verified",
          "confirmation_required":true,
          "parameters_schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"},
          "expected_state_schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}
        }]
      }] , "endpoints"
      """#
  )

  static let commandJob = #"""
    {
      "command_id":"command_demo_0001","vehicle_id":"vehicle_demo_alpha",
      "command":"set_charge_limit","command_class":"charging","state":"accepted",
      "created_at":"2026-08-30T12:00:00.100Z",
      "updated_at":"2026-08-30T12:00:00.100Z",
      "expires_at":"2026-08-30T12:05:00.000Z","attempt_count":0,
      "retry_policy":"state_verified","audit":[{
        "state":"accepted","at":"2026-08-30T12:00:00.100Z","actor_id":"service_hub"
      }],"links":{"self":"/v1/commands/command_demo_0001"}
    }
    """#

  static let retryableProblem = #"""
    {
      "type":"urn:teslatlas:problem:unavailable","title":"Unavailable",
      "status":503,"instance":"/requests/request_unavailable_1",
      "code":"unavailable","request_id":"request_unavailable_1",
      "retryable":true,"retry_after_seconds":3
    }
    """#

  private static func page(type: String, item: String, snapshot: String) -> String {
    #"""
    {
      "resource_type":"\#(type)","items":[\#(item)],"next_cursor":null,
      "snapshot_revision":"\#(snapshot)","generated_at":"2026-08-30T12:00:01.000Z"
    }
    """#
  }
}

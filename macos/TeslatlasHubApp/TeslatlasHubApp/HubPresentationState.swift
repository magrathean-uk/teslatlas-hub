// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

enum HubMainSection: Equatable {
    case dashboard
    case vehicles
}

enum HubModalKind: Equatable {
    case onboarding
    case diagnostics
    case logs
    case serviceDetails
}

enum HubModalTransition: Equatable {
    case present(HubModalKind)
    case reuse(HubModalKind)
    case replace(old: HubModalKind, new: HubModalKind)
}

struct HubModalState {
    private(set) var active: HubModalKind?

    mutating func request(_ kind: HubModalKind) -> HubModalTransition {
        if active == kind { return .reuse(kind) }
        if let active {
            let old = active
            self.active = kind
            return .replace(old: old, new: kind)
        }
        active = kind
        return .present(kind)
    }

    mutating func dismiss(_ kind: HubModalKind) {
        if active == kind { active = nil }
    }
}

enum HubSessionEvent: Equatable {
    case hubSetUp
    case teslaMateImported
    case hubStarted
    case hubStopped
    case hubRestarted
    case accountChanged(HubAccountProvider)
    case accountDisconnected
    case vehicleCommandAccepted(HubVehicleControl, vehicle: String)
}

struct HubSessionActivityStore {
    let limit: Int
    let now: () -> Date
    private(set) var activities: [HubActivity] = []

    mutating func record(_ event: HubSessionEvent) {
        activities.insert(HubActivity(message: event.message, age: "just now", color: event.color), at: 0)
        activities = Array(activities.prefix(limit))
    }
}

private extension HubSessionEvent {
    var message: String {
        switch self {
        case .hubSetUp: return "Hub set up and started"
        case .teslaMateImported: return "Imported TeslaMate history"
        case .hubStarted: return "Hub service started"
        case .hubStopped: return "Hub service stopped"
        case .hubRestarted: return "Hub service restarted"
        case let .accountChanged(provider): return "Now using \(provider.displayName)"
        case .accountDisconnected: return "Tesla account disconnected"
        case let .vehicleCommandAccepted(command, vehicle):
            return "\(command.title) accepted for \(vehicle)"
        }
    }

    var color: NSColor {
        switch self {
        case .hubStopped, .accountDisconnected: return .systemOrange
        case .teslaMateImported, .hubSetUp, .hubStarted, .hubRestarted,
             .accountChanged, .vehicleCommandAccepted: return .systemGreen
        }
    }
}

// SPDX-License-Identifier: AGPL-3.0-only

import AppKit

enum HubMainSection: Equatable {
    case dashboard
    case vehicles
    case activity
    case settings
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
        activities.insert(HubActivity(message: event.message, age: HubL10n.text("hub.HubController.3905.1332", fallback: "just now"), color: event.color), at: 0)
        activities = Array(activities.prefix(limit))
    }
}

private extension HubSessionEvent {
    var message: String {
        switch self {
        case .hubSetUp: return HubL10n.text("hub.HubPresentationState.69.1522", fallback: "Hub set up and started")
        case .teslaMateImported: return HubL10n.text("hub.HubPresentationState.70.1523", fallback: "Imported TeslaMate history")
        case .hubStarted: return HubL10n.text("hub.HubPresentationState.71.1524", fallback: "Hub service started")
        case .hubStopped: return HubL10n.text("hub.HubPresentationState.72.1525", fallback: "Hub service stopped")
        case .hubRestarted: return HubL10n.text("hub.HubPresentationState.73.1526", fallback: "Hub service restarted")
        case let .accountChanged(provider): return HubL10n.format("hub.HubPresentationState.74.1527", fallback: "Now using %1$@", arguments: [String(describing: provider.displayName)])
        case .accountDisconnected: return HubL10n.text("hub.HubPresentationState.75.1528", fallback: "Tesla account disconnected")
        case let .vehicleCommandAccepted(command, vehicle):
            return HubL10n.format("hub.HubPresentationState.77.1529", fallback: "%1$@ accepted for %2$@", arguments: [String(describing: command.title), String(describing: vehicle)])
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

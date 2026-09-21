// SPDX-License-Identifier: AGPL-3.0-only

import Foundation

/// Safe, deterministic UI states used for design review and visual regression captures.
/// Preview scenes never execute service, Tesla, migration, or filesystem mutations.
enum HubPreviewScene: String, CaseIterable {
    case welcome = "r01-welcome"
    case choose = "r02-choose"
    case migration = "r03-migration"
    case migrationConnected = "r04-migration-connected"
    case verify = "r05-verify"
    case finishMigration = "r06-finish-migration"
    case dashboard = "r07-dashboard"
    case vehicles = "r08-vehicles"
    case diagnostics = "r09-diagnostics"
    case logs = "r10-logs"
    case serviceDetails = "r11-service-details"
    case manageMenu = "r12-manage-menu"
    case provider = "r13-provider"
    case fleet = "r14-fleet"
    case legacy = "r15-legacy"
    case activity = "r16-activity"
    case settings = "r17-settings"
    case finish = "r18-finish"
    case importing = "r19-importing"

    init?(environmentValue: String?) {
        guard let value = environmentValue?.lowercased() else { return nil }
        let aliases: [String: HubPreviewScene] = [
            "r01": .welcome, "welcome": .welcome,
            "r02": .choose, "choose": .choose,
            "r03": .migration, "migration": .migration,
            "r04": .migrationConnected, "migration-connected": .migrationConnected,
            "r05": .verify, "verify": .verify,
            "r06": .finishMigration, "finish": .finishMigration,
            "finish-migration": .finishMigration,
            "r07": .dashboard, "dashboard": .dashboard,
            "r08": .vehicles, "vehicles": .vehicles,
            "r09": .diagnostics, "diagnostics": .diagnostics,
            "r10": .logs, "logs": .logs,
            "r11": .serviceDetails, "service-details": .serviceDetails,
            "r12": .manageMenu, "manage-menu": .manageMenu,
            "r13": .provider, "provider": .provider,
            "r14": .fleet, "fleet": .fleet,
            "r15": .legacy, "legacy": .legacy,
            "r16": .activity, "activity": .activity,
            "r17": .settings, "settings": .settings,
            "r18": .finish, "finish-new": .finish,
            "r19": .importing, "importing": .importing
        ]
        if let scene = aliases[value] {
            self = scene
        } else {
            self.init(rawValue: value)
        }
    }

    var onboardingRoute: String? {
        switch self {
        case .welcome: return "welcome"
        case .choose: return "choose"
        case .migration: return "migration"
        case .migrationConnected: return "migration-connected"
        case .verify: return "verify"
        case .finishMigration: return "finish-migration"
        case .provider: return "provider"
        case .fleet: return "fleet"
        case .legacy: return "legacy"
        case .finish: return "finish"
        case .importing: return "importing"
        case .dashboard, .vehicles, .activity, .settings,
             .diagnostics, .logs, .serviceDetails, .manageMenu:
            return nil
        }
    }

    var isOnboarding: Bool { onboardingRoute != nil }
}

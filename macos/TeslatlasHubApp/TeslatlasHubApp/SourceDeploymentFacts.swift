// SPDX-License-Identifier: AGPL-3.0-only

import Foundation

enum SourceDeploymentFacts {
    static let databaseUser = "DATABASE_USER"
    static let databasePassword = "DATABASE_PASS"
    static let databaseName = "DATABASE_NAME"
    static let databaseHost = "DATABASE_HOST"
    /// Names the source application container by the presence of this variable name.
    /// Only the name is compared; its value is never selected, read or carried.
    static let sourceShapeMarker = "ENCRYPTION_KEY"
    static let databaseService = "database"
    static let ociVersionLabel = "org.opencontainers.image.version"
    static let postgresPort = "5432"
}

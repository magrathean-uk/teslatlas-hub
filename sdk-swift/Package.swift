// swift-tools-version: 6.0

import PackageDescription

let package = Package(
  name: "teslatlas-sdk-swift",
  platforms: [
    .iOS(.v17),
    .macOS(.v14),
  ],
  products: [
    .library(name: "TeslatlasHubSDK", targets: ["TeslatlasHubSDK"]),
    .library(name: "TeslatlasCommands", targets: ["TeslatlasCommands"]),
    .library(
      name: "TeslatlasHubV1Compatibility",
      targets: ["TeslatlasHubV1Compatibility"]
    ),
    .executable(
      name: "TeslatlasHubSDKExample",
      targets: ["TeslatlasHubSDKExample"]
    ),
  ],
  targets: [
    .target(name: "TeslatlasHubSDK"),
    .target(
      name: "TeslatlasHubV1Compatibility",
      resources: [.copy("Binding")]
    ),
    .target(
      name: "TeslatlasCommands",
      dependencies: ["TeslatlasHubSDK"]
    ),
    .executableTarget(
      name: "TeslatlasHubSDKExample",
      dependencies: ["TeslatlasHubSDK"],
      path: "Examples/TeslatlasHubSDKExample"
    ),
    .testTarget(
      name: "TeslatlasHubSDKTests",
      dependencies: ["TeslatlasHubSDK", "TeslatlasCommands"],
      resources: [.copy("Fixtures")]
    ),
    .testTarget(
      name: "TeslatlasHubV1CompatibilityTests",
      dependencies: ["TeslatlasHubV1Compatibility"],
      resources: [.copy("Fixtures")]
    ),
  ]
)

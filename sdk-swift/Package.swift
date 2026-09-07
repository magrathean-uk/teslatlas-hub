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
    .library(
      name: "TeslatlasCurrentHub",
      targets: ["TeslatlasCurrentHub"]
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
      name: "TeslatlasCurrentHub",
      dependencies: [
        .target(name: "CurrentHubCurlShim", condition: .when(platforms: [.linux]))
      ],
      resources: [.copy("Binding")]
    ),
    .target(
      name: "CurrentHubCurlShim",
      path: "Sources/CurrentHubCurlShim",
      publicHeadersPath: "include",
      linkerSettings: [
        .linkedLibrary("curl", .when(platforms: [.linux])),
        .linkedLibrary("ssl", .when(platforms: [.linux])),
        .linkedLibrary("crypto", .when(platforms: [.linux])),
      ]
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
    .testTarget(
      name: "TeslatlasCurrentHubTests",
      dependencies: ["TeslatlasCurrentHub"],
      resources: [.copy("Fixtures")]
    ),
  ]
)

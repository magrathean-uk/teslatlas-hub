// swift-tools-version: 6.0

import PackageDescription

let package = Package(
  name: "tesatlas-sdk-swift",
  platforms: [
    .iOS(.v17),
    .macOS(.v14),
  ],
  products: [
    .library(name: "TesatlasHubSDK", targets: ["TesatlasHubSDK"]),
    .library(name: "TesatlasCommands", targets: ["TesatlasCommands"]),
    .library(
      name: "TesatlasHubV1Compatibility",
      targets: ["TesatlasHubV1Compatibility"]
    ),
    .library(
      name: "TesatlasCurrentHub",
      targets: ["TesatlasCurrentHub"]
    ),
    .executable(
      name: "TesatlasHubSDKExample",
      targets: ["TesatlasHubSDKExample"]
    ),
  ],
  targets: [
    .target(name: "TesatlasHubSDK"),
    .target(
      name: "TesatlasHubV1Compatibility",
      resources: [.copy("Binding")]
    ),
    .target(
      name: "TesatlasCurrentHub",
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
      name: "TesatlasCommands",
      dependencies: ["TesatlasHubSDK"]
    ),
    .executableTarget(
      name: "TesatlasHubSDKExample",
      dependencies: ["TesatlasHubSDK"],
      path: "Examples/TesatlasHubSDKExample"
    ),
    .testTarget(
      name: "TesatlasHubSDKTests",
      dependencies: ["TesatlasHubSDK", "TesatlasCommands"],
      resources: [.copy("Fixtures")]
    ),
    .testTarget(
      name: "TesatlasHubV1CompatibilityTests",
      dependencies: ["TesatlasHubV1Compatibility"],
      resources: [.copy("Fixtures")]
    ),
    .testTarget(
      name: "TesatlasCurrentHubTests",
      dependencies: ["TesatlasCurrentHub"],
      resources: [.copy("Fixtures")]
    ),
  ]
)

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
    .executable(
      name: "TeslatlasHubSDKExample",
      targets: ["TeslatlasHubSDKExample"]
    ),
  ],
  targets: [
    .target(name: "TeslatlasHubSDK"),
    .executableTarget(
      name: "TeslatlasHubSDKExample",
      dependencies: ["TeslatlasHubSDK"],
      path: "Examples/TeslatlasHubSDKExample"
    ),
    .testTarget(
      name: "TeslatlasHubSDKTests",
      dependencies: ["TeslatlasHubSDK"]
    ),
  ]
)
